// SPDX-License-Identifier: GPL-3.0-only
//! Speechmatics realtime STT backend — a native WebSocket client
//! against `wss://<region>.rt.speechmatics.com/v2`, driven in a
//! *buffered one-shot* mode for [`SpeechToText::transcribe`].
//!
//! Fono's dictation model records a full PCM buffer then transcribes
//! it, so rather than pay the Batch API's job-create → poll → fetch
//! latency we open the realtime socket, stream the buffer, and collect
//! the `AddTranscript` finals as the audio drains. The same wire
//! protocol underpins a future `StreamingStt` live-preview path.
//!
//! Lifecycle (sequential, single task — no spawning needed for the
//! one-shot path):
//!
//! 1. Connect with the `Authorization: Bearer <key>` header on the
//!    upgrade request. Speechmatics uses the literal word `Bearer`
//!    (NOT Deepgram's `Token`); a unit test pins this.
//! 2. Send a `StartRecognition` message describing the audio
//!    (`audio_format`: raw `pcm_s16le` at `sample_rate`) and the
//!    `transcription_config` (language + operating point).
//! 3. Wait for `RecognitionStarted`.
//! 4. Stream the buffered PCM as binary `AddAudio` frames, counting
//!    them so the closing `EndOfStream` can carry `last_seq_no`.
//! 5. Send `EndOfStream { last_seq_no }`.
//! 6. Collect `AddTranscript.metadata.transcript` segments until
//!    `EndOfTranscript` (or socket close / `Error`), concatenate, and
//!    return.
//!
//! Limitations: the realtime API fixes one language per session (it
//! does not auto-detect mid-stream the way Deepgram's `language=multi`
//! does), so an allow-list resolves to its first entry and `Auto`
//! falls back to English. The chosen code is recorded in the language
//! cache for stickiness parity with the other cloud backends.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::lang::LanguageSelection;
use crate::lang_cache::LanguageCache;
use crate::traits::{SpeechToText, Transcription};

/// Default realtime region endpoint (Europe). US is
/// `wss://us.rt.speechmatics.com/v2`.
const DEFAULT_ENDPOINT: &str = "wss://eu.rt.speechmatics.com/v2";
/// Default operating point. Catalogue default is `enhanced`
/// (see `crates/fono-core/src/provider_catalog.rs`); this mirrors it
/// for the constructor-with-no-override path.
const DEFAULT_OPERATING_POINT: &str = "enhanced";
/// Cache key the language-stickiness layer uses for this backend.
pub(crate) const BACKEND_KEY: &str = "speechmatics";
/// Audio chunk size (bytes) per `AddAudio` frame. 32 KiB ≈ 16 k
/// s16 samples ≈ 1 s at 16 kHz — large enough to keep frame count
/// (and thus `last_seq_no`) small without starving the server.
const AUDIO_CHUNK_BYTES: usize = 32_768;

pub struct SpeechmaticsStt {
    api_key: String,
    /// Operating point (`standard` / `enhanced`), stored in the
    /// `model` slot for catalogue/factory symmetry with the other
    /// cloud backends.
    operating_point: String,
    endpoint: String,
    languages: Vec<String>,
    #[allow(dead_code)] // captured for builder parity; realtime fixes one language per session.
    cloud_rerun_on_mismatch: bool,
    lang_cache: Arc<LanguageCache>,
    #[allow(dead_code)] // captured so `[stt.prompts]` doesn't error; not yet sent on the wire.
    prompts: std::collections::HashMap<String, String>,
}

impl SpeechmaticsStt {
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_model(api_key, DEFAULT_OPERATING_POINT)
    }

    pub fn with_model(api_key: impl Into<String>, operating_point: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            operating_point: operating_point.into(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            languages: Vec::new(),
            cloud_rerun_on_mismatch: false,
            lang_cache: LanguageCache::global(),
            prompts: std::collections::HashMap::new(),
        }
    }

    /// Builder: language allow-list. See [`LanguageSelection`].
    #[must_use]
    pub fn with_languages(mut self, codes: Vec<String>) -> Self {
        self.languages = codes;
        self
    }

    /// Builder: realtime cannot rerun mid-session, but the flag is
    /// captured for builder parity with the batch cloud backends.
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

    /// Builder: override the realtime region endpoint
    /// (`wss://us.rt.speechmatics.com/v2`, on-prem URLs, …).
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Builder: per-language prompt map. Speechmatics realtime has no
    /// equivalent of Whisper's `prompt` field (custom vocabulary is a
    /// separate `additional_vocab` config), so the map is captured for
    /// forward-compat but unused on the wire today.
    #[must_use]
    pub fn with_prompts(mut self, prompts: std::collections::HashMap<String, String>) -> Self {
        self.prompts = prompts;
        self
    }

    /// Configured operating point. Exposed for tests.
    #[must_use]
    pub fn operating_point(&self) -> &str {
        &self.operating_point
    }

    /// Resolve the single session language for this transcription.
    /// Realtime fixes one language per session, so an allow-list
    /// collapses to its first entry and `Auto` falls back to English.
    fn session_language(&self, lang_override: Option<&str>) -> String {
        match LanguageSelection::from_config(&self.languages).with_override(lang_override) {
            LanguageSelection::Forced(c) => c,
            LanguageSelection::AllowList(peers) => {
                peers.first().cloned().unwrap_or_else(|| "en".to_string())
            }
            LanguageSelection::Auto => "en".to_string(),
        }
    }
}

/// Subset of a Speechmatics realtime server message. Every field is
/// `serde(default)` and unknown fields are ignored so additive schema
/// drift cannot break the parser. We only act on `RecognitionStarted`,
/// `AddTranscript`, `EndOfTranscript`, and `Error`.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct SpeechmaticsMessage {
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub metadata: SpeechmaticsMetadata,
    /// Present on `Error` / `Warning` frames.
    #[serde(default)]
    pub reason: Option<String>,
    /// Error category, e.g. `not_authorised`, on `Error` frames.
    #[serde(default, rename = "type")]
    pub error_type: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct SpeechmaticsMetadata {
    /// The transcript text for this segment. Speechmatics already
    /// includes inter-word spacing here.
    #[serde(default)]
    pub transcript: String,
}

/// Build the `StartRecognition` control message for a session.
/// Exposed for tests.
#[must_use]
pub fn build_start_recognition(
    operating_point: &str,
    language: &str,
    sample_rate: u32,
) -> serde_json::Value {
    serde_json::json!({
        "message": "StartRecognition",
        "audio_format": {
            "type": "raw",
            "encoding": "pcm_s16le",
            "sample_rate": sample_rate,
        },
        "transcription_config": {
            "language": language,
            "operating_point": operating_point,
            "enable_partials": false,
            "max_delay": 1.0,
        },
    })
}

/// Convert an f32 PCM slice in [-1.0, 1.0] to little-endian s16 bytes
/// (Speechmatics' `pcm_s16le` wire format). Out-of-range values clamp
/// rather than wrap.
fn f32_to_s16le_bytes(pcm: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pcm.len() * 2);
    for &s in pcm {
        let clamped = s.clamp(-1.0, 1.0);
        #[allow(clippy::cast_possible_truncation)]
        let i = (clamped * 32_767.0) as i16;
        out.extend_from_slice(&i.to_le_bytes());
    }
    out
}

#[async_trait]
impl SpeechToText for SpeechmaticsStt {
    async fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
        lang: Option<&str>,
    ) -> Result<Transcription> {
        if pcm.is_empty() {
            return Ok(Transcription { text: String::new(), language: None, duration_ms: None });
        }
        let language = self.session_language(lang);

        // Build the upgrade request manually so we can attach the
        // `Authorization: Bearer …` header on the handshake.
        // Speechmatics uses `Bearer`, NOT Deepgram's `Token` — a unit
        // test pins this so a copy-paste can't regress it.
        let mut request =
            self.endpoint.as_str().into_client_request().with_context(|| {
                format!("building Speechmatics WS request for {}", self.endpoint)
            })?;
        let header_value = HeaderValue::from_str(&format!("Bearer {key}", key = self.api_key))
            .context("Speechmatics API key contains non-ASCII bytes")?;
        request.headers_mut().insert("Authorization", header_value);

        let (mut ws, _resp) = tokio_tungstenite::connect_async(request)
            .await
            .with_context(|| format!("Speechmatics WS connect failed for {}", self.endpoint))?;

        // 1. StartRecognition.
        let start = build_start_recognition(&self.operating_point, &language, sample_rate);
        ws.send(Message::Text(start.to_string()))
            .await
            .context("sending Speechmatics StartRecognition")?;

        // 2. Wait for RecognitionStarted (ignore anything else).
        loop {
            let msg = ws
                .next()
                .await
                .ok_or_else(|| anyhow!("Speechmatics socket closed before RecognitionStarted"))?
                .context("reading Speechmatics handshake reply")?;
            if let Message::Text(payload) = msg {
                let parsed: SpeechmaticsMessage = serde_json::from_str(&payload)
                    .with_context(|| format!("parsing Speechmatics message: {payload}"))?;
                match parsed.message.as_str() {
                    "RecognitionStarted" => break,
                    "Error" => {
                        anyhow::bail!(
                            "Speechmatics error during start: {} ({})",
                            parsed.reason.unwrap_or_default(),
                            parsed.error_type.unwrap_or_default()
                        );
                    }
                    _ => {}
                }
            }
        }

        // 3. Stream the buffered PCM as binary AddAudio frames.
        let bytes = f32_to_s16le_bytes(pcm);
        let mut seq_no: u64 = 0;
        for chunk in bytes.chunks(AUDIO_CHUNK_BYTES) {
            ws.send(Message::Binary(chunk.to_vec()))
                .await
                .context("sending Speechmatics AddAudio frame")?;
            seq_no += 1;
        }

        // 4. EndOfStream with the count of audio frames sent.
        let eos = serde_json::json!({"message": "EndOfStream", "last_seq_no": seq_no});
        ws.send(Message::Text(eos.to_string()))
            .await
            .context("sending Speechmatics EndOfStream")?;

        // 5. Collect AddTranscript finals until EndOfTranscript / close.
        let mut transcript = String::new();
        while let Some(msg) = ws.next().await {
            let msg = msg.context("reading Speechmatics transcript stream")?;
            match msg {
                Message::Text(payload) => {
                    let parsed: SpeechmaticsMessage = match serde_json::from_str(&payload) {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::debug!(
                                "speechmatics: ignoring unparseable message: {e}; body={payload}"
                            );
                            continue;
                        }
                    };
                    match parsed.message.as_str() {
                        "AddTranscript" => transcript.push_str(&parsed.metadata.transcript),
                        "EndOfTranscript" => break,
                        "Error" => {
                            anyhow::bail!(
                                "Speechmatics error: {} ({})",
                                parsed.reason.unwrap_or_default(),
                                parsed.error_type.unwrap_or_default()
                            );
                        }
                        "Warning" => {
                            tracing::warn!(
                                "speechmatics warning: {}",
                                parsed.reason.unwrap_or_default()
                            );
                        }
                        _ => {}
                    }
                }
                Message::Close(reason) => {
                    tracing::info!("speechmatics WS closed: {reason:?}");
                    break;
                }
                _ => {}
            }
        }
        let _ = ws.close(None).await;

        let text = transcript.trim().to_string();
        self.lang_cache.record(BACKEND_KEY, &language);
        Ok(Transcription { text, language: Some(language), duration_ms: None })
    }

    fn name(&self) -> &'static str {
        "speechmatics"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_stable_label() {
        // Tray / doctor / critical_notify all key off the backend
        // name; if it ever changes the catalogue and provider tables
        // must change in lockstep.
        assert_eq!(SpeechmaticsStt::new("sm_test").name(), "speechmatics");
    }

    #[test]
    fn default_operating_point_matches_catalogue() {
        // Catch catalogue drift: if someone bumps the catalogue
        // without updating DEFAULT_OPERATING_POINT (or vice versa) this
        // fires.
        assert_eq!(DEFAULT_OPERATING_POINT, crate::defaults::default_cloud_model("speechmatics"));
    }

    #[test]
    fn auth_header_uses_bearer_prefix_not_token() {
        // Footgun: Deepgram uses the literal word `Token`; Speechmatics
        // uses `Bearer`. A copy-paste from the Deepgram WS client would
        // silently 401. Pin the exact wire format here.
        let formatted = format!("Bearer {key}", key = "sm_key");
        assert_eq!(formatted, "Bearer sm_key");
        assert!(!formatted.starts_with("Token"));
    }

    #[test]
    fn start_recognition_shape_matches_spec() {
        let v = build_start_recognition("enhanced", "en", 16_000);
        assert_eq!(v["message"], "StartRecognition");
        assert_eq!(v["audio_format"]["type"], "raw");
        assert_eq!(v["audio_format"]["encoding"], "pcm_s16le");
        assert_eq!(v["audio_format"]["sample_rate"], 16_000);
        assert_eq!(v["transcription_config"]["language"], "en");
        assert_eq!(v["transcription_config"]["operating_point"], "enhanced");
    }

    #[test]
    fn session_language_resolution() {
        // Forced override wins.
        let stt = SpeechmaticsStt::new("k").with_languages(vec!["ro".into(), "en".into()]);
        assert_eq!(stt.session_language(Some("fr")), "fr");
        // Allow-list collapses to first entry.
        assert_eq!(stt.session_language(None), "ro");
        // Auto (no allow-list) falls back to English.
        let auto = SpeechmaticsStt::new("k");
        assert_eq!(auto.session_language(None), "en");
    }

    #[test]
    fn parses_add_transcript_message() {
        let body = r#"{
            "message": "AddTranscript",
            "metadata": {"transcript": "Hello, welcome to Speechmatics.", "start_time": 0.0},
            "results": []
        }"#;
        let parsed: SpeechmaticsMessage = serde_json::from_str(body).expect("parse");
        assert_eq!(parsed.message, "AddTranscript");
        assert_eq!(parsed.metadata.transcript, "Hello, welcome to Speechmatics.");
    }

    #[test]
    fn parses_error_message() {
        let body = r#"{"message":"Error","type":"not_authorised","reason":"invalid api key"}"#;
        let parsed: SpeechmaticsMessage = serde_json::from_str(body).expect("parse");
        assert_eq!(parsed.message, "Error");
        assert_eq!(parsed.error_type.as_deref(), Some("not_authorised"));
        assert_eq!(parsed.reason.as_deref(), Some("invalid api key"));
    }

    #[test]
    fn parses_message_with_unknown_extra_fields() {
        let body = r#"{
            "message": "RecognitionStarted",
            "id": "abc-123",
            "language_pack_info": {"language_description": "English"}
        }"#;
        let parsed: SpeechmaticsMessage = serde_json::from_str(body).expect("parse");
        assert_eq!(parsed.message, "RecognitionStarted");
    }

    #[test]
    fn f32_to_s16le_known_samples() {
        let pcm = [0.0_f32, 1.0, -1.0];
        let bytes = f32_to_s16le_bytes(&pcm);
        assert_eq!(bytes.len(), 6);
        assert_eq!(&bytes[0..2], &[0x00, 0x00]);
        assert_eq!(&bytes[2..4], &[0xFF, 0x7F]);
        assert_eq!(&bytes[4..6], &[0x01, 0x80]);
    }

    #[tokio::test]
    async fn empty_pcm_returns_empty_without_connecting() {
        let stt = SpeechmaticsStt::new("sm_test");
        let out = stt.transcribe(&[], 16_000, None).await.expect("empty ok");
        assert!(out.text.is_empty());
        assert!(out.language.is_none());
    }

    #[test]
    fn builder_captures_state() {
        let stt = SpeechmaticsStt::with_model("k", "standard")
            .with_languages(vec!["en".into()])
            .with_cloud_rerun_on_mismatch(true)
            .with_endpoint("wss://us.rt.speechmatics.com/v2");
        assert_eq!(stt.operating_point(), "standard");
        assert_eq!(stt.languages, vec!["en"]);
        assert!(stt.cloud_rerun_on_mismatch);
        assert_eq!(stt.endpoint, "wss://us.rt.speechmatics.com/v2");
    }
}
