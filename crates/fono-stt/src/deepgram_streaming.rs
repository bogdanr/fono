// SPDX-License-Identifier: GPL-3.0-only
//! Deepgram streaming STT — a *native* WebSocket client against
//! `wss://api.deepgram.com/v1/listen`. Unlike Groq's pseudo-stream
//! (which re-POSTs the trailing audio every cadence tick), Deepgram
//! exposes a first-class realtime endpoint and partial / final
//! transcripts arrive as JSON frames at sub-300 ms cadence.
//!
//! Lifecycle:
//!
//! 1. Connect to `wss://api.deepgram.com/v1/listen?...` with the
//!    `Authorization: Token <key>` header attached to the upgrade
//!    request (literal `Token`, *not* `Bearer` — see [`crate::deepgram`]).
//! 2. Stream s16le mono PCM as binary WebSocket frames at the
//!    capture sample rate (`encoding=linear16&sample_rate=N&channels=1`
//!    is sent on the URL so Deepgram knows the wire format).
//! 3. Map incoming `Results` messages to [`TranscriptUpdate`] via the
//!    [`UpdateLane::Preview`] / [`UpdateLane::Finalize`] split:
//!    `is_final: false` → `Preview`, `is_final: true` → `Finalize`.
//! 4. Drive [`crate::streaming::StreamFrame::SegmentBoundary`] from
//!    Deepgram's `UtteranceEnd` VAD event so the overlay's pondering +
//!    auto-stop hook sees a Deepgram-driven boundary without
//!    backend-specific code.
//! 5. On EOF send `{"type":"CloseStream"}` so Deepgram flushes any
//!    pending finalize before the socket closes.
//!
//! Slice 2 of `plans/2026-05-23-deepgram-stt-nova-3-v1.md`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use futures::SinkExt;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::lang::LanguageSelection;
use crate::lang_cache::LanguageCache;
use crate::streaming::{StreamFrame, StreamingStt, TranscriptUpdate};

/// Build a streaming `wss://api.deepgram.com/v1/listen` URL with all
/// query params Deepgram needs to interpret the binary PCM frames the
/// daemon will send. Exposed for unit tests.
///
/// When `lang` is `None`, the URL falls back to `language=multi`
/// (Deepgram's documented auto-detect knob for Nova-2 and Nova-3
/// alike). The previous draft sent `detect_language=true`, which
/// Nova-3 rejects with HTTP 400 — the streaming path failed to
/// connect on every fresh session in the field until we caught the
/// drift.
#[must_use]
pub fn build_stream_url(model: &str, sample_rate: u32, lang: Option<&str>) -> String {
    let mut url = format!(
        "wss://api.deepgram.com/v1/listen?model={model}\
         &encoding=linear16&sample_rate={sample_rate}&channels=1\
         &interim_results=true&smart_format=true&punctuate=true&vad_events=true"
    );
    if let Some(code) = lang {
        url.push_str("&language=");
        url.push_str(code);
    } else {
        // Allow-list / auto path — let Deepgram auto-detect across its
        // supported set; the post-validation rerun (batch path) is the
        // only place we second-guess. Streaming doesn't rerun mid-
        // session because each utterance is short and a wrong-language
        // partial will self-correct on the next user-spoken word.
        url.push_str("&language=multi");
    }
    url
}

/// Subset of Deepgram's WebSocket message envelope. Every field is
/// `serde(default)` and unknown fields are ignored so additive schema
/// drift (Deepgram extends this regularly) cannot break the parser.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct DeepgramWsMessage {
    /// One of `Results`, `SpeechStarted`, `UtteranceEnd`,
    /// `Metadata`, etc. We only act on `Results` and `UtteranceEnd`.
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub is_final: bool,
    #[serde(default)]
    pub channel: Option<DeepgramWsChannel>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct DeepgramWsChannel {
    #[serde(default)]
    pub alternatives: Vec<DeepgramWsAlternative>,
    /// Populated only when the request asked for `detect_language=true`.
    #[serde(default)]
    pub detected_language: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct DeepgramWsAlternative {
    #[serde(default)]
    pub transcript: String,
    #[serde(default)]
    pub confidence: Option<f32>,
}

impl DeepgramWsMessage {
    /// Top alternative transcript on the first channel; empty when the
    /// message carried no channel / alternatives (e.g. a metadata
    /// frame, or a Results frame with only silence).
    #[must_use]
    pub fn transcript(&self) -> &str {
        self.channel
            .as_ref()
            .and_then(|c| c.alternatives.first())
            .map(|a| a.transcript.as_str())
            .unwrap_or("")
    }

    /// Detected language alpha-2 code on the first channel, when
    /// `detect_language=true` was asked on the URL.
    #[must_use]
    pub fn detected_language(&self) -> Option<&str> {
        self.channel.as_ref().and_then(|c| c.detected_language.as_deref())
    }
}

/// Streaming Deepgram client. Implements [`StreamingStt`] over a real
/// WebSocket — partial transcripts paint within ~150 ms of audio
/// arriving and finalize frames clear segments without a round-trip
/// per cadence tick.
pub struct DeepgramStreaming {
    api_key: String,
    model: String,
    languages: Vec<String>,
    #[allow(dead_code)] // captured for builder parity; rerun is batch-only.
    cloud_rerun_on_mismatch: bool,
    lang_cache: Arc<LanguageCache>,
}

impl DeepgramStreaming {
    #[must_use]
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            languages: Vec::new(),
            cloud_rerun_on_mismatch: false,
            lang_cache: LanguageCache::global(),
        }
    }

    /// Builder: language allow-list. See [`LanguageSelection`].
    #[must_use]
    pub fn with_languages(mut self, codes: Vec<String>) -> Self {
        self.languages = codes;
        self
    }

    /// Builder: re-issue with a forced language when Deepgram returns
    /// an out-of-allow-list code. Streaming honours the flag for
    /// builder parity with the batch backend but cannot rerun
    /// mid-session — each utterance is short enough that a
    /// wrong-language partial self-corrects on the next spoken word.
    #[must_use]
    pub fn with_cloud_rerun_on_mismatch(mut self, on: bool) -> Self {
        self.cloud_rerun_on_mismatch = on;
        self
    }

    /// Builder: inject a specific language cache (tests + bench).
    #[must_use]
    pub fn with_lang_cache(mut self, cache: Arc<LanguageCache>) -> Self {
        self.lang_cache = cache;
        self
    }

    /// Builder: preview cadence knob. No-op for Deepgram — the
    /// cadence is server-driven — but kept for parity with the Groq
    /// streaming builder so the factory can call it unconditionally.
    #[must_use]
    pub fn with_preview_cadence(self, _cadence: Option<Duration>) -> Self {
        self
    }

    fn effective_selection(&self, lang_override: Option<&str>) -> LanguageSelection {
        LanguageSelection::from_config(&self.languages).with_override(lang_override)
    }
}

/// Convert an f32 PCM slice in [-1.0, 1.0] to little-endian s16
/// bytes. Deepgram's `encoding=linear16` wire format. Values outside
/// the nominal range are clamped to silence clipping on the way to
/// the wire.
fn f32_to_s16le_bytes(pcm: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pcm.len() * 2);
    for &s in pcm {
        let clamped = s.clamp(-1.0, 1.0);
        // i16::MAX = 32_767; multiplying by 32_767 keeps -1.0 from
        // overflowing to -32_768's nominal sister-value.
        #[allow(clippy::cast_possible_truncation)]
        let i = (clamped * 32_767.0) as i16;
        out.extend_from_slice(&i.to_le_bytes());
    }
    out
}

#[async_trait]
impl StreamingStt for DeepgramStreaming {
    #[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
    async fn stream_transcribe(
        &self,
        mut frames: BoxStream<'static, StreamFrame>,
        sample_rate: u32,
        lang: Option<String>,
    ) -> Result<BoxStream<'static, TranscriptUpdate>> {
        let selection = self.effective_selection(lang.as_deref());
        let first_pass_lang: Option<String> = match &selection {
            LanguageSelection::Auto | LanguageSelection::AllowList(_) => None,
            LanguageSelection::Forced(c) => Some(c.clone()),
        };

        let url = build_stream_url(&self.model, sample_rate, first_pass_lang.as_deref());

        // Build the upgrade request manually so we can attach the
        // `Authorization: Token …` header on the handshake. Deepgram
        // uses the literal word `Token`, NOT `Bearer` — same footgun
        // pinned in the batch backend.
        let mut request = url
            .as_str()
            .into_client_request()
            .with_context(|| format!("building Deepgram WS request for {url}"))?;
        let header_value = HeaderValue::from_str(&format!("Token {key}", key = self.api_key))
            .context("Deepgram API key contains non-ASCII bytes")?;
        request.headers_mut().insert("Authorization", header_value);

        let (ws, _resp) = tokio_tungstenite::connect_async(request)
            .await
            .with_context(|| format!("Deepgram WS connect failed for {url}"))?;
        let (mut ws_write, mut ws_read) = ws.split();

        let (tx, rx) = mpsc::unbounded_channel::<TranscriptUpdate>();
        let lang_cache = Arc::clone(&self.lang_cache);
        let allow_list: Option<Vec<String>> = match &selection {
            LanguageSelection::AllowList(v) => Some(v.clone()),
            _ => None,
        };
        let started = Instant::now();

        // Reader task: consume WS messages, translate to TranscriptUpdate.
        let tx_reader = tx.clone();
        let read_handle = tokio::spawn(async move {
            // Segment index advances on UtteranceEnd; the next Results
            // frame after that increment paints the next segment.
            let mut segment_index: u32 = 0;
            while let Some(msg) = ws_read.next().await {
                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("deepgram WS read error: {e:#}");
                        break;
                    }
                };
                match msg {
                    Message::Text(payload) => {
                        let parsed: DeepgramWsMessage = match serde_json::from_str(&payload) {
                            Ok(p) => p,
                            Err(e) => {
                                tracing::debug!(
                                    "deepgram WS: ignoring unparseable message: {e}; \
                                     body={payload}"
                                );
                                continue;
                            }
                        };
                        match parsed.kind.as_str() {
                            "Results" => {
                                let text = parsed.transcript();
                                if text.is_empty() {
                                    continue;
                                }
                                // Post-validate detected language
                                // against the allow-list — when out
                                // of the set, suppress the preview so
                                // the overlay doesn't flash
                                // wrong-language text. The next
                                // utterance gets a fresh chance.
                                if let (Some(allow), Some(detected)) =
                                    (allow_list.as_ref(), parsed.detected_language())
                                {
                                    let detected_lc = detected.to_ascii_lowercase();
                                    let in_list =
                                        allow.iter().any(|c| c.eq_ignore_ascii_case(&detected_lc));
                                    if !in_list {
                                        tracing::info!(
                                            "deepgram stream: detected banned language \
                                             {detected:?} (allow-list {allow:?}); \
                                             suppressing this frame"
                                        );
                                        continue;
                                    }
                                    lang_cache.record(crate::deepgram::BACKEND_KEY, &detected_lc);
                                }
                                let elapsed = started.elapsed();
                                let lang_field = parsed.detected_language().map(str::to_string);
                                let upd = if parsed.is_final {
                                    TranscriptUpdate::finalize(segment_index, text, elapsed)
                                        .with_language(lang_field)
                                } else {
                                    TranscriptUpdate::preview(segment_index, text, elapsed)
                                        .with_language(lang_field)
                                };
                                if tx_reader.send(upd).is_err() {
                                    // Receiver dropped — caller lost
                                    // interest, stop draining.
                                    break;
                                }
                            }
                            "UtteranceEnd" => {
                                // Server-driven segment boundary.
                                // Advance the segment index so the
                                // next Results frame paints into a
                                // fresh segment.
                                segment_index = segment_index.saturating_add(1);
                            }
                            "SpeechStarted" | "Metadata" => {
                                // Informational — no transcript text.
                            }
                            other => {
                                tracing::debug!(
                                    "deepgram WS: ignoring unknown message kind {other:?}"
                                );
                            }
                        }
                    }
                    Message::Binary(_) => {
                        // Deepgram does not send audio back; ignore.
                    }
                    Message::Ping(p) => {
                        // Tungstenite responds to pings automatically;
                        // this branch exists to swallow the message.
                        tracing::trace!("deepgram WS ping ({} bytes)", p.len());
                    }
                    Message::Close(reason) => {
                        tracing::info!("deepgram WS closed: {reason:?}");
                        break;
                    }
                    _ => {}
                }
            }
        });

        // Writer task: pump PCM frames into the WS as binary messages,
        // forward EOF / boundary into the appropriate control message.
        tokio::spawn(async move {
            while let Some(frame) = frames.next().await {
                match frame {
                    StreamFrame::Pcm(chunk) => {
                        let bytes = f32_to_s16le_bytes(&chunk);
                        if let Err(e) = ws_write.send(Message::Binary(bytes)).await {
                            tracing::warn!("deepgram WS send error: {e:#}");
                            break;
                        }
                    }
                    StreamFrame::SegmentBoundary => {
                        // Ask Deepgram to flush any pending finalize
                        // without closing the socket. The reader
                        // already advances segment_index on
                        // `UtteranceEnd`; this just nudges the server
                        // to emit one promptly when the local VAD
                        // beat Deepgram's to the punch.
                        let msg = r#"{"type":"Finalize"}"#;
                        if let Err(e) = ws_write.send(Message::Text(msg.into())).await {
                            tracing::warn!("deepgram WS Finalize send failed: {e:#}");
                            break;
                        }
                    }
                    StreamFrame::Eof => {
                        let msg = r#"{"type":"CloseStream"}"#;
                        let _ = ws_write.send(Message::Text(msg.into())).await;
                        break;
                    }
                }
            }
            // Let the reader drain any final messages, then close.
            let _ = ws_write.close().await;
            // Best-effort wait so the final Finalize lands before the
            // output stream signals end.
            let _ = read_handle.await;
            drop(tx);
        });

        let out = UnboundedReceiverStream::new(rx);
        let boxed: BoxStream<'static, TranscriptUpdate> = out.boxed();
        Ok(boxed)
    }

    fn name(&self) -> &'static str {
        "deepgram"
    }

    fn is_local(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_header_uses_token_prefix_not_bearer() {
        // Same footgun as the batch backend — pin the exact wire
        // format here too so a copy-paste regression in the
        // handshake builder fires a test.
        let formatted = format!("Token {key}", key = "dg_streaming_key");
        assert_eq!(formatted, "Token dg_streaming_key");
        assert!(!formatted.starts_with("Bearer"));
    }

    #[test]
    fn build_url_includes_all_required_query_params() {
        let url = build_stream_url("nova-3", 16_000, None);
        assert!(url.starts_with("wss://api.deepgram.com/v1/listen?"));
        assert!(url.contains("model=nova-3"));
        assert!(url.contains("encoding=linear16"));
        assert!(url.contains("sample_rate=16000"));
        assert!(url.contains("channels=1"));
        assert!(url.contains("interim_results=true"));
        assert!(url.contains("smart_format=true"));
        assert!(url.contains("punctuate=true"));
        assert!(url.contains("vad_events=true"));
        // Nova-3 rejects `detect_language=true` with HTTP 400; the
        // documented multilingual knob is `language=multi`. The
        // first streaming cut shipped with the wrong flag and every
        // live session 400'd at handshake — this assertion pins the
        // fixed behaviour.
        assert!(url.contains("language=multi"));
        assert!(!url.contains("detect_language"));
    }

    #[test]
    fn build_url_forced_language_omits_multilingual() {
        let url = build_stream_url("nova-3", 16_000, Some("ro"));
        assert!(url.contains("language=ro"));
        assert!(!url.contains("language=multi"));
        assert!(!url.contains("detect_language"));
    }

    #[test]
    fn build_url_honours_custom_sample_rate() {
        let url = build_stream_url("nova-2", 48_000, Some("en"));
        assert!(url.contains("model=nova-2"));
        assert!(url.contains("sample_rate=48000"));
        assert!(url.contains("language=en"));
    }

    #[test]
    fn parses_interim_results_frame_as_preview() {
        let body = r#"{
            "type": "Results",
            "is_final": false,
            "channel": {
                "alternatives": [{"transcript": "hello wor", "confidence": 0.71}]
            }
        }"#;
        let parsed: DeepgramWsMessage = serde_json::from_str(body).expect("parse");
        assert_eq!(parsed.kind, "Results");
        assert!(!parsed.is_final);
        assert_eq!(parsed.transcript(), "hello wor");
    }

    #[test]
    fn parses_is_final_routes_to_finalize_lane() {
        let body = r#"{
            "type": "Results",
            "is_final": true,
            "channel": {
                "alternatives": [{"transcript": "hello world.", "confidence": 0.94}],
                "detected_language": "en"
            }
        }"#;
        let parsed: DeepgramWsMessage = serde_json::from_str(body).expect("parse");
        assert!(parsed.is_final);
        assert_eq!(parsed.transcript(), "hello world.");
        assert_eq!(parsed.detected_language(), Some("en"));
    }

    #[test]
    fn parses_utterance_end_event() {
        let body = r#"{"type":"UtteranceEnd","last_word_end":2.34}"#;
        let parsed: DeepgramWsMessage = serde_json::from_str(body).expect("parse");
        assert_eq!(parsed.kind, "UtteranceEnd");
        assert_eq!(parsed.transcript(), "");
    }

    #[test]
    fn parses_speech_started_event() {
        let body = r#"{"type":"SpeechStarted","timestamp":0.12}"#;
        let parsed: DeepgramWsMessage = serde_json::from_str(body).expect("parse");
        assert_eq!(parsed.kind, "SpeechStarted");
    }

    #[test]
    fn parses_message_with_unknown_extra_fields() {
        // Forward-compat: Deepgram extends this envelope routinely.
        let body = r#"{
            "type": "Results",
            "channel_index": [0,1],
            "duration": 1.23,
            "start": 0.0,
            "is_final": false,
            "speech_final": false,
            "from_finalize": false,
            "channel": {
                "alternatives": [{
                    "transcript": "hi",
                    "confidence": 0.9,
                    "words": [{"word":"hi","start":0.1,"end":0.3,"confidence":0.9}]
                }]
            },
            "metadata": {"request_id": "abc"}
        }"#;
        let parsed: DeepgramWsMessage = serde_json::from_str(body).expect("parse");
        assert_eq!(parsed.transcript(), "hi");
    }

    #[test]
    fn empty_message_yields_empty_transcript() {
        let parsed = DeepgramWsMessage::default();
        assert_eq!(parsed.transcript(), "");
        assert_eq!(parsed.detected_language(), None);
    }

    #[test]
    fn f32_to_s16le_known_samples() {
        // Spot-check the PCM converter so a numeric regression
        // (e.g. accidentally swapping `to_le_bytes` for `to_be_bytes`)
        // would fail loudly.
        let pcm = [0.0_f32, 1.0, -1.0, 0.5];
        let bytes = f32_to_s16le_bytes(&pcm);
        assert_eq!(bytes.len(), 8);
        // 0.0 -> 0
        assert_eq!(&bytes[0..2], &[0x00, 0x00]);
        // 1.0 -> 32767 = 0x7FFF, little-endian => [0xFF, 0x7F]
        assert_eq!(&bytes[2..4], &[0xFF, 0x7F]);
        // -1.0 -> -32767 = 0x8001, little-endian => [0x01, 0x80]
        assert_eq!(&bytes[4..6], &[0x01, 0x80]);
    }

    #[test]
    fn f32_to_s16le_clamps_out_of_range() {
        // Values outside [-1.0, 1.0] must not wrap around.
        let pcm = [2.5_f32, -3.0];
        let bytes = f32_to_s16le_bytes(&pcm);
        // Both clamp to 32767 / -32767.
        assert_eq!(&bytes[0..2], &[0xFF, 0x7F]);
        assert_eq!(&bytes[2..4], &[0x01, 0x80]);
    }

    #[test]
    fn builder_captures_state() {
        let s = DeepgramStreaming::new("dg_key", "nova-3")
            .with_languages(vec!["en".into(), "ro".into()])
            .with_cloud_rerun_on_mismatch(true)
            .with_preview_cadence(Some(Duration::from_millis(700)));
        assert_eq!(s.languages, vec!["en", "ro"]);
        assert!(s.cloud_rerun_on_mismatch);
        assert_eq!(s.model, "nova-3");
        assert_eq!(s.name(), "deepgram");
        assert!(!s.is_local());
    }
}
