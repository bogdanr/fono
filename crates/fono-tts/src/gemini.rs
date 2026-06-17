// SPDX-License-Identifier: GPL-3.0-only
//! Gemini native text-to-speech via the Gemini API's audio-generation path —
//! `POST /v1beta/models/<model>:generateContent` with
//! `responseModalities: ["AUDIO"]` on a single `GEMINI_API_KEY` (free tier).
//! See ADR 0034.
//!
//! Wire shape:
//!   POST `https://generativelanguage.googleapis.com/v1beta/models/<model>:generateContent`
//!   header: `x-goog-api-key: <key>`
//!   body:
//!     ```json
//!     { "contents": [{ "parts": [{ "text": "<text>" }] }],
//!       "generationConfig": {
//!         "responseModalities": ["AUDIO"],
//!         "speechConfig": { "voiceConfig": {
//!           "prebuiltVoiceConfig": { "voiceName": "Kore" } } } } }
//!     ```
//!   response: `candidates[0].content.parts[0].inlineData.data` is base64
//!     raw int16 LE mono PCM. The companion `mimeType` is
//!     `audio/L16;codec=pcm;rate=24000`; we parse the `rate=` field and fall
//!     back to 24 kHz (the documented default for the prebuilt voices).
//!
//! The voice is selected by `voiceName` (one of the 30 prebuilt voices —
//! `Kore`, `Puck`, `Charon`, …); the catalogue exposes a curated, gendered
//! subset as the palette. A per-call `voice` hint overrides the configured
//! voice. Gemini TTS is multilingual (40+ languages incl. Romanian) and
//! auto-detects the spoken language from the text, so no `lang` is sent.
//!
//! The model id default lives in the catalogue
//! (`crates/fono-core/src/provider_catalog.rs`).

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use fono_core::provider_catalog;
use futures::stream::{BoxStream, StreamExt};
use serde::{Deserialize, Serialize};

use crate::traits::{TextToSpeech, TtsAudio, TtsChunk};

const BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
/// Documented native rate for the prebuilt Gemini TTS voices.
const NATIVE_RATE: u32 = 24_000;

pub struct GeminiTts {
    api_key: String,
    model: String,
    voice: String,
    client: reqwest::Client,
}

impl GeminiTts {
    /// Construct from the catalogue defaults, with optional model and voice
    /// overrides (empty / `None` fall back to the catalogue entry).
    #[must_use]
    pub fn new(
        api_key: impl Into<String>,
        model_override: Option<String>,
        voice_override: Option<String>,
    ) -> Self {
        let entry = provider_catalog::find("gemini")
            .and_then(|p| p.tts.as_ref())
            .expect("gemini catalogue entry must exist with a TTS capability");
        let model =
            model_override.filter(|m| !m.is_empty()).unwrap_or_else(|| entry.model.to_string());
        let voice = voice_override
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| entry.default_voice.to_string());
        Self { api_key: api_key.into(), model, voice, client: crate::openai_compat::warm_client() }
    }

    /// Configured model id. Exposed for tests.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Configured default voice. Exposed for tests.
    #[must_use]
    pub fn voice(&self) -> &str {
        &self.voice
    }

    fn endpoint(&self) -> String {
        format!("{BASE_URL}/models/{}:generateContent", self.model)
    }

    /// Streaming endpoint — `:streamGenerateContent?alt=sse` returns the audio
    /// incrementally as a Server-Sent Events stream of `GenerateResponse`
    /// objects, each carrying a slice of the utterance's PCM in an
    /// `inlineData` part. Cuts time-to-first-audio (ADR 0034 / cloud-streaming
    /// plan v2).
    fn stream_endpoint(&self) -> String {
        format!("{BASE_URL}/models/{}:streamGenerateContent?alt=sse", self.model)
    }

    /// Build the `generateContent` request body for `text` with `voice`.
    /// Exposed for tests.
    #[must_use]
    pub fn build_request_body(&self, text: &str, voice: &str) -> serde_json::Value {
        let req = GenerateRequest {
            contents: vec![Content { parts: vec![Part { text }] }],
            generation_config: GenerationConfig {
                response_modalities: ["AUDIO"],
                speech_config: SpeechConfig {
                    voice_config: VoiceConfig {
                        prebuilt_voice_config: PrebuiltVoiceConfig { voice_name: voice },
                    },
                },
            },
        };
        serde_json::to_value(req)
            .expect("serialising static-shape Gemini TTS request must not fail")
    }
}

// ----- request wire types ------------------------------------------------

#[derive(Serialize)]
struct GenerateRequest<'a> {
    contents: Vec<Content<'a>>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig<'a>,
}

#[derive(Serialize)]
struct Content<'a> {
    parts: Vec<Part<'a>>,
}

#[derive(Serialize)]
struct Part<'a> {
    text: &'a str,
}

#[derive(Serialize)]
struct GenerationConfig<'a> {
    #[serde(rename = "responseModalities")]
    response_modalities: [&'static str; 1],
    #[serde(rename = "speechConfig")]
    speech_config: SpeechConfig<'a>,
}

#[derive(Serialize)]
struct SpeechConfig<'a> {
    #[serde(rename = "voiceConfig")]
    voice_config: VoiceConfig<'a>,
}

#[derive(Serialize)]
struct VoiceConfig<'a> {
    #[serde(rename = "prebuiltVoiceConfig")]
    prebuilt_voice_config: PrebuiltVoiceConfig<'a>,
}

#[derive(Serialize)]
struct PrebuiltVoiceConfig<'a> {
    #[serde(rename = "voiceName")]
    voice_name: &'a str,
}

// ----- response wire types -----------------------------------------------

#[derive(Deserialize, Debug, Clone, Default)]
struct GenerateResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
}

#[derive(Deserialize, Debug, Clone, Default)]
struct Candidate {
    #[serde(default)]
    content: Option<RespContent>,
}

#[derive(Deserialize, Debug, Clone, Default)]
struct RespContent {
    #[serde(default)]
    parts: Vec<RespPart>,
}

#[derive(Deserialize, Debug, Clone, Default)]
struct RespPart {
    #[serde(default, rename = "inlineData")]
    inline_data: Option<InlineData>,
}

#[derive(Deserialize, Debug, Clone, Default)]
struct InlineData {
    #[serde(default, rename = "mimeType")]
    mime_type: String,
    #[serde(default)]
    data: String,
}

/// Pull the first audio part out of a parsed response, returning the
/// base64 payload and the sample rate parsed from its `mimeType`
/// (`audio/L16;codec=pcm;rate=24000` → 24000, falling back to the native
/// rate). Exposed for tests.
fn first_audio_part(resp: &GenerateResponse) -> Option<(&str, u32)> {
    resp.candidates
        .first()
        .and_then(|c| c.content.as_ref())
        .and_then(|c| c.parts.iter().find_map(|p| p.inline_data.as_ref()))
        .map(|d| (d.data.as_str(), parse_rate(&d.mime_type)))
}

/// Parse `rate=<n>` from an `audio/L16;codec=pcm;rate=24000` mime type,
/// defaulting to [`NATIVE_RATE`] when absent or malformed.
fn parse_rate(mime_type: &str) -> u32 {
    mime_type
        .split(';')
        .filter_map(|p| p.trim().strip_prefix("rate="))
        .find_map(|v| v.parse::<u32>().ok())
        .unwrap_or(NATIVE_RATE)
}

fn pcm_i16_le_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32767.0)
        .collect()
}

/// Incremental Server-Sent Events accumulator for the streaming TTS response.
///
/// Feeds raw response bytes in (`push_and_drain`) and yields the JSON payload
/// of each complete SSE event — the concatenated value(s) of its `data:`
/// field(s). Robust to `\n` / `\r\n` line endings and to events split across
/// network chunk boundaries: only whole lines (terminated by `\n`) are
/// processed, and a blank line closes the current event. Byte-level scanning
/// is safe because `\n` (0x0A) / `\r` (0x0D) never appear inside a multi-byte
/// UTF-8 sequence.
#[derive(Default)]
struct SseAudioDecoder {
    buf: Vec<u8>,
    data: String,
}

impl SseAudioDecoder {
    fn new() -> Self {
        Self::default()
    }

    /// Append `bytes` and return the JSON payload of every event that became
    /// complete. Incomplete trailing data stays buffered for the next call.
    fn push_and_drain(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = self.buf.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    events.push(std::mem::take(&mut self.data));
                }
            } else if let Some(rest) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(rest.trim_start());
            }
            // Other SSE fields (`event:`, `id:`, `:` comments) are ignored.
        }
        events
    }

    /// At end-of-stream, return any final event payload that wasn't terminated
    /// by a trailing blank line.
    fn flush(&mut self) -> Option<String> {
        // Process any complete line still buffered without a trailing newline.
        if !self.buf.is_empty() {
            let remaining = std::mem::take(&mut self.buf);
            let line = String::from_utf8_lossy(&remaining);
            let line = line.trim_end_matches(['\n', '\r']);
            if let Some(rest) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(rest.trim_start());
            }
        }
        (!self.data.is_empty()).then(|| std::mem::take(&mut self.data))
    }
}

/// Decode one SSE event JSON payload into a PCM chunk + sample rate, or `None`
/// if the event carries no audio part (keepalive / metadata-only events).
fn decode_sse_audio_event(json: &str) -> Result<Option<(Vec<f32>, u32)>> {
    let parsed: GenerateResponse = serde_json::from_str(json)
        .with_context(|| format!("parse gemini TTS stream event: {}", truncate(json, 200)))?;
    let Some((b64, rate)) = first_audio_part(&parsed) else {
        return Ok(None);
    };
    let bytes = BASE64_STANDARD.decode(b64).context("decoding gemini TTS stream base64 audio")?;
    Ok(Some((pcm_i16_le_to_f32(&bytes), rate)))
}

#[async_trait]
impl TextToSpeech for GeminiTts {
    fn name(&self) -> &'static str {
        "gemini"
    }

    fn native_sample_rate(&self) -> u32 {
        NATIVE_RATE
    }

    async fn synthesize(
        &self,
        text: &str,
        voice: Option<&str>,
        _lang: Option<&str>,
    ) -> Result<TtsAudio> {
        if text.is_empty() {
            return Ok(TtsAudio { pcm: Vec::new(), sample_rate: NATIVE_RATE });
        }
        let voice = voice.map(str::trim).filter(|v| !v.is_empty()).unwrap_or(self.voice.as_str());
        let body = self.build_request_body(text, voice);
        let resp = self
            .client
            .post(self.endpoint())
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("posting to gemini :generateContent (TTS)")?;
        let status = resp.status();
        let text_body = resp.text().await.context("reading gemini TTS response body")?;
        if !status.is_success() {
            return Err(anyhow!("gemini TTS returned {status}: {}", truncate(&text_body, 400)));
        }
        let parsed: GenerateResponse = serde_json::from_str(&text_body)
            .with_context(|| format!("parse gemini TTS response: {}", truncate(&text_body, 400)))?;
        let (b64, rate) = first_audio_part(&parsed)
            .ok_or_else(|| anyhow!("gemini TTS response contained no audio part"))?;
        let bytes = BASE64_STANDARD.decode(b64).context("decoding gemini TTS base64 audio")?;
        let pcm = pcm_i16_le_to_f32(&bytes);
        Ok(TtsAudio { pcm, sample_rate: rate })
    }

    async fn prewarm(&self) -> Result<()> {
        // Warm the TLS pool with a cheap authed GET against the model
        // listing (the same endpoint the wizard probes for key validation).
        let res = self
            .client
            .get(format!("{BASE_URL}/models"))
            .header("x-goog-api-key", &self.api_key)
            .send()
            .await
            .context("gemini TTS prewarm")?;
        let _ = res.bytes().await;
        Ok(())
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    async fn synthesize_stream(
        &self,
        text: &str,
        voice: Option<&str>,
        _lang: Option<&str>,
    ) -> Result<BoxStream<'static, Result<TtsChunk>>> {
        if text.is_empty() {
            let chunk = TtsChunk { pcm: Vec::new(), sample_rate: NATIVE_RATE, is_final: true };
            return Ok(Box::pin(futures::stream::once(async move { Ok(chunk) })));
        }
        let voice = voice.map(str::trim).filter(|v| !v.is_empty()).unwrap_or(self.voice.as_str());
        let body = self.build_request_body(text, voice);
        let resp = self
            .client
            .post(self.stream_endpoint())
            .header("x-goog-api-key", &self.api_key)
            .header("accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .context("posting to gemini :streamGenerateContent (TTS)")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "gemini streaming TTS returned {status}: {}",
                truncate(&body, 400)
            ));
        }

        // Pump the SSE byte stream on a background task; emit a PCM chunk per
        // audio event and a terminal empty `is_final` chunk when the stream
        // ends. A small channel applies natural backpressure to the network
        // read if the consumer (playback) lags.
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<TtsChunk>>(16);
        tokio::spawn(async move {
            let mut bytes_stream = resp.bytes_stream();
            let mut sse = SseAudioDecoder::new();
            while let Some(item) = bytes_stream.next().await {
                let bytes = match item {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = tx.send(Err(anyhow!("gemini TTS stream chunk error: {e}"))).await;
                        return;
                    }
                };
                for payload in sse.push_and_drain(&bytes) {
                    match decode_sse_audio_event(&payload) {
                        Ok(Some((pcm, rate))) => {
                            let chunk = TtsChunk { pcm, sample_rate: rate, is_final: false };
                            if tx.send(Ok(chunk)).await.is_err() {
                                return; // consumer dropped
                            }
                        }
                        Ok(None) => {} // non-audio event (metadata / keepalive)
                        Err(e) => {
                            let _ = tx.send(Err(e)).await;
                            return;
                        }
                    }
                }
            }
            // Drain any final event not closed by a trailing blank line.
            if let Some(payload) = sse.flush() {
                if let Ok(Some((pcm, rate))) = decode_sse_audio_event(&payload) {
                    let chunk = TtsChunk { pcm, sample_rate: rate, is_final: false };
                    let _ = tx.send(Ok(chunk)).await;
                }
            }
            let _ = tx
                .send(Ok(TtsChunk { pcm: Vec::new(), sample_rate: NATIVE_RATE, is_final: true }))
                .await;
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out = s[..max].to_string();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_uses_catalogue_defaults() {
        let c = GeminiTts::new("k", None, None);
        let entry = provider_catalog::find("gemini").and_then(|p| p.tts.as_ref()).unwrap();
        assert_eq!(c.model(), entry.model);
        assert_eq!(c.voice(), entry.default_voice);
        assert_eq!(c.native_sample_rate(), NATIVE_RATE);
        assert_eq!(c.name(), "gemini");
    }

    #[test]
    fn overrides_take_precedence_over_catalogue() {
        let c = GeminiTts::new("k", Some("gemini-x-tts".into()), Some("Puck".into()));
        assert_eq!(c.model(), "gemini-x-tts");
        assert_eq!(c.voice(), "Puck");
    }

    #[test]
    fn empty_overrides_fall_back_to_catalogue() {
        let c = GeminiTts::new("k", Some(String::new()), Some(String::new()));
        let entry = provider_catalog::find("gemini").and_then(|p| p.tts.as_ref()).unwrap();
        assert_eq!(c.model(), entry.model);
        assert_eq!(c.voice(), entry.default_voice);
    }

    #[test]
    fn request_body_shape_matches_spec() {
        let c = GeminiTts::new("k", None, None);
        let body = c.build_request_body("hello", "Kore");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "hello");
        assert_eq!(body["generationConfig"]["responseModalities"][0], "AUDIO");
        assert_eq!(
            body["generationConfig"]["speechConfig"]["voiceConfig"]["prebuiltVoiceConfig"]
                ["voiceName"],
            "Kore"
        );
    }

    #[test]
    fn endpoint_targets_generate_content() {
        let c = GeminiTts::new("k", Some("gemini-3.1-flash-tts-preview".into()), None);
        assert_eq!(
            c.endpoint(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-3.1-flash-tts-preview:generateContent"
        );
    }

    #[test]
    fn parse_rate_reads_mime_type() {
        assert_eq!(parse_rate("audio/L16;codec=pcm;rate=24000"), 24_000);
        assert_eq!(parse_rate("audio/L16; codec=pcm; rate=16000"), 16_000);
        // Missing / malformed rate falls back to the native rate.
        assert_eq!(parse_rate("audio/L16;codec=pcm"), NATIVE_RATE);
        assert_eq!(parse_rate(""), NATIVE_RATE);
        assert_eq!(parse_rate("audio/L16;rate=notanumber"), NATIVE_RATE);
    }

    #[test]
    fn first_audio_part_extracts_payload_and_rate() {
        let body = r#"{
            "candidates": [{
                "content": {"parts": [
                    {"inlineData": {"mimeType": "audio/L16;codec=pcm;rate=24000", "data": "AAECAw=="}}
                ]}
            }]
        }"#;
        let parsed: GenerateResponse = serde_json::from_str(body).expect("parse");
        let (data, rate) = first_audio_part(&parsed).expect("audio part");
        assert_eq!(data, "AAECAw==");
        assert_eq!(rate, 24_000);
    }

    #[test]
    fn first_audio_part_none_when_absent() {
        let parsed: GenerateResponse = serde_json::from_str(r#"{"candidates":[]}"#).expect("parse");
        assert!(first_audio_part(&parsed).is_none());
    }

    #[test]
    fn pcm_decode_roundtrips_le_int16() {
        // 0x0000 -> 0.0, 0x7FFF -> ~1.0, 0x8000 (-32768) -> ~-1.0
        let bytes = [0x00, 0x00, 0xFF, 0x7F, 0x00, 0x80];
        let pcm = pcm_i16_le_to_f32(&bytes);
        assert_eq!(pcm.len(), 3);
        assert!((pcm[0]).abs() < 1e-6);
        assert!((pcm[1] - 1.0).abs() < 1e-3);
        assert!((pcm[2] + 1.0).abs() < 1e-3);
    }

    #[tokio::test]
    async fn empty_text_returns_empty_audio_without_request() {
        let c = GeminiTts::new("k", None, None);
        let audio = c.synthesize("", None, None).await.unwrap();
        assert!(audio.pcm.is_empty());
        assert_eq!(audio.sample_rate, NATIVE_RATE);
    }

    #[test]
    fn client_supports_streaming() {
        assert!(GeminiTts::new("k", None, None).supports_streaming());
    }

    #[test]
    fn stream_endpoint_targets_stream_generate_content_sse() {
        let c = GeminiTts::new("k", Some("gemini-3.1-flash-tts-preview".into()), None);
        assert_eq!(
            c.stream_endpoint(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-3.1-flash-tts-preview:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn sse_decoder_extracts_events_across_chunk_boundaries() {
        let mut dec = SseAudioDecoder::new();
        // One event split across two network reads, CRLF line endings.
        let mut got = dec.push_and_drain(b"data: {\"candidates\":[{\"content\":{\"par");
        assert!(got.is_empty(), "incomplete event must not yield");
        got = dec.push_and_drain(
            b"ts\":[{\"inlineData\":{\"mimeType\":\"audio/L16;rate=24000\",\"data\":\"AAECAw==\"}}]}}]}\r\n\r\n",
        );
        assert_eq!(got.len(), 1);
        let (pcm, rate) = decode_sse_audio_event(&got[0]).unwrap().unwrap();
        assert_eq!(rate, 24_000);
        assert_eq!(pcm.len(), 2); // 4 bytes int16 LE -> 2 samples
    }

    #[test]
    fn sse_decoder_handles_two_events_and_ignores_comments() {
        let mut dec = SseAudioDecoder::new();
        let body = b": keepalive comment\n\
                     data: {\"candidates\":[{\"content\":{\"parts\":[{\"inlineData\":{\"mimeType\":\"audio/L16;rate=16000\",\"data\":\"AAEC\"}}]}}]}\n\n\
                     data: {\"candidates\":[{\"content\":{\"parts\":[{\"inlineData\":{\"mimeType\":\"audio/L16;rate=16000\",\"data\":\"BAUG\"}}]}}]}\n\n";
        let events = dec.push_and_drain(body);
        assert_eq!(events.len(), 2);
        for ev in &events {
            assert!(decode_sse_audio_event(ev).unwrap().is_some());
        }
    }

    #[test]
    fn sse_decoder_flush_emits_unterminated_final_event() {
        let mut dec = SseAudioDecoder::new();
        // No trailing blank line.
        let got = dec.push_and_drain(
            b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"inlineData\":{\"mimeType\":\"audio/L16;rate=24000\",\"data\":\"AAEC\"}}]}}]}",
        );
        assert!(got.is_empty(), "no blank line yet, nothing drained");
        let tail = dec.flush().expect("flush yields the buffered event");
        assert!(decode_sse_audio_event(&tail).unwrap().is_some());
        assert!(dec.flush().is_none(), "second flush is empty");
    }

    #[test]
    fn decode_sse_audio_event_returns_none_for_metadata_only() {
        // A usage/metadata event with no inlineData part.
        let json = r#"{"candidates":[{"content":{"parts":[]}}]}"#;
        assert!(decode_sse_audio_event(json).unwrap().is_none());
    }

    #[tokio::test]
    async fn synthesize_stream_empty_text_yields_one_final_chunk() {
        use futures::StreamExt;
        let c = GeminiTts::new("k", None, None);
        let chunks: Vec<_> = c.synthesize_stream("", None, None).await.unwrap().collect().await;
        assert_eq!(chunks.len(), 1);
        let only = chunks.into_iter().next().unwrap().unwrap();
        assert!(only.is_final);
        assert!(only.pcm.is_empty());
    }
}
