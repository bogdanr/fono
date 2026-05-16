// SPDX-License-Identifier: GPL-3.0-only
//! Cartesia `/tts/bytes` client.
//!
//! Wire shape:
//!   POST `https://api.cartesia.ai/tts/bytes`
//!   header: `X-Api-Key: <key>`
//!   body: `{ "model_id": "sonic-2", "transcript": <text>,
//!           "voice": { "mode": "id", "id": <voice_id> },
//!           "output_format": { "container": "raw",
//!                              "encoding": "pcm_s16le",
//!                              "sample_rate": 24000 },
//!           "language": "en" }`
//!   response: raw int16 LE mono PCM at 24 kHz.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use fono_core::provider_catalog;
use serde::Serialize;

use crate::traits::{TextToSpeech, TtsAudio};

const NATIVE_RATE: u32 = 24_000;
const ENDPOINT: &str = "https://api.cartesia.ai/tts/bytes";

pub struct CartesiaTts {
    api_key: String,
    model: String,
    voice_id: String,
    /// Best-effort language hint. Cartesia uses a 2-letter code; we
    /// default to `"en"` which matches the catalogue's English voice.
    language: String,
    client: reqwest::Client,
}

impl CartesiaTts {
    /// Build a client using the catalogue defaults for model / voice.
    #[must_use]
    pub fn new(
        api_key: impl Into<String>,
        model_override: Option<String>,
        voice_override: Option<String>,
    ) -> Self {
        let entry = provider_catalog::find("cartesia")
            .and_then(|p| p.tts.as_ref())
            .expect("cartesia catalogue entry must exist with a TTS capability");
        Self {
            api_key: api_key.into(),
            model: model_override.unwrap_or_else(|| entry.model.to_string()),
            voice_id: voice_override.unwrap_or_else(|| entry.default_voice.to_string()),
            language: "en".to_string(),
            client: crate::openai_compat::warm_client(),
        }
    }

    /// Configured model id. Exposed for tests.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Configured voice id. Exposed for tests.
    #[must_use]
    pub fn voice_id(&self) -> &str {
        &self.voice_id
    }

    /// Endpoint URL. Exposed for tests.
    #[must_use]
    pub const fn endpoint(&self) -> &'static str {
        ENDPOINT
    }

    /// Build the JSON request body for `synthesize`. Exposed for tests
    /// so request-shape assertions stay in pure Rust.
    #[must_use]
    pub fn build_request_body(&self, text: &str) -> serde_json::Value {
        serde_json::to_value(SynthesizeReq {
            model_id: &self.model,
            transcript: text,
            voice: VoiceRef { mode: "id", id: &self.voice_id },
            output_format: OutputFormat {
                container: "raw",
                encoding: "pcm_s16le",
                sample_rate: NATIVE_RATE,
            },
            language: &self.language,
        })
        .expect("serialising static-shape Cartesia request must not fail")
    }
}

#[derive(Serialize)]
struct SynthesizeReq<'a> {
    model_id: &'a str,
    transcript: &'a str,
    voice: VoiceRef<'a>,
    output_format: OutputFormat,
    language: &'a str,
}

#[derive(Serialize)]
struct VoiceRef<'a> {
    mode: &'a str,
    id: &'a str,
}

#[derive(Serialize)]
struct OutputFormat {
    container: &'static str,
    encoding: &'static str,
    sample_rate: u32,
}

#[async_trait]
impl TextToSpeech for CartesiaTts {
    fn name(&self) -> &'static str {
        "cartesia"
    }

    fn native_sample_rate(&self) -> u32 {
        NATIVE_RATE
    }

    async fn synthesize(
        &self,
        text: &str,
        _voice: Option<&str>,
        _lang: Option<&str>,
    ) -> Result<TtsAudio> {
        if text.is_empty() {
            return Ok(TtsAudio { pcm: Vec::new(), sample_rate: NATIVE_RATE });
        }
        let body = self.build_request_body(text);
        let resp = self
            .client
            .post(ENDPOINT)
            .header("X-Api-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("posting to cartesia /tts/bytes")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(anyhow!("cartesia TTS returned {status}: {}", truncate(&body, 400)));
        }
        let bytes = resp.bytes().await.context("reading cartesia TTS response body")?;
        let pcm = pcm_i16_le_to_f32(&bytes);
        Ok(TtsAudio { pcm, sample_rate: NATIVE_RATE })
    }

    async fn prewarm(&self) -> Result<()> {
        // Cartesia has no documented cheap GET we can prewarm with;
        // the TCP/TLS handshake is paid lazily on the first POST.
        // Mirrors the wyoming backend's prewarm no-op.
        Ok(())
    }
}

fn pcm_i16_le_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32767.0)
        .collect()
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
    fn cartesia_client_uses_catalogue_defaults() {
        let c = CartesiaTts::new("ck-test", None, None);
        assert_eq!(c.model(), "sonic-2");
        assert_eq!(c.voice_id(), "a0e99841-438c-4a64-b679-ae501e7d6091");
        assert_eq!(c.endpoint(), "https://api.cartesia.ai/tts/bytes");
        assert_eq!(c.native_sample_rate(), NATIVE_RATE);
    }

    /// Inspect the JSON body without making a live request. Locks in
    /// the exact wire shape required by `/tts/bytes`.
    #[test]
    fn request_body_shape_matches_spec() {
        let c = CartesiaTts::new("ck-test", None, None);
        let body = c.build_request_body("hello world");
        assert_eq!(body["model_id"], "sonic-2");
        assert_eq!(body["transcript"], "hello world");
        assert_eq!(body["voice"]["mode"], "id");
        assert_eq!(body["voice"]["id"], "a0e99841-438c-4a64-b679-ae501e7d6091");
        assert_eq!(body["output_format"]["container"], "raw");
        assert_eq!(body["output_format"]["encoding"], "pcm_s16le");
        assert_eq!(body["output_format"]["sample_rate"], 24_000);
        assert_eq!(body["language"], "en");
    }

    #[tokio::test]
    async fn empty_text_returns_empty_audio_without_request() {
        let c = CartesiaTts::new("ck-test", None, None);
        let audio = c.synthesize("", None, None).await.unwrap();
        assert!(audio.pcm.is_empty());
        assert_eq!(audio.sample_rate, NATIVE_RATE);
    }
}
