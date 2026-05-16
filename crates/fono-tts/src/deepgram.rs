// SPDX-License-Identifier: GPL-3.0-only
//! Deepgram `/v1/speak` client.
//!
//! Wire shape:
//!   POST `https://api.deepgram.com/v1/speak
//!         ?model=aura-2-thalia-en&encoding=linear16&sample_rate=24000`
//!   header: `Authorization: Token <key>` (literal word `Token`, NOT
//!           `Bearer`).
//!   body: `{ "text": <text> }`
//!   response: raw int16 LE mono PCM at 24 kHz.
//!
//! Deepgram selects a voice via the `model` query parameter — e.g.
//! `aura-2-thalia-en` IS the voice. The catalogue stores the voice as
//! the empty string and we keep the model itself authoritative.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use fono_core::provider_catalog;
use serde::Serialize;

use crate::traits::{TextToSpeech, TtsAudio};

const NATIVE_RATE: u32 = 24_000;
const BASE_ENDPOINT: &str = "https://api.deepgram.com/v1/speak";

pub struct DeepgramTts {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl DeepgramTts {
    #[must_use]
    pub fn new(api_key: impl Into<String>, model_override: Option<String>) -> Self {
        let entry = provider_catalog::find("deepgram")
            .and_then(|p| p.tts.as_ref())
            .expect("deepgram catalogue entry must exist with a TTS capability");
        Self {
            api_key: api_key.into(),
            model: model_override.unwrap_or_else(|| entry.model.to_string()),
            client: crate::openai_compat::warm_client(),
        }
    }

    /// Configured model id (which doubles as the voice selector).
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Resolved POST URL with model + encoding + sample-rate query
    /// parameters baked in. Exposed for tests.
    #[must_use]
    pub fn speech_url(&self) -> String {
        format!(
            "{BASE_ENDPOINT}?model={model}&encoding=linear16&sample_rate={rate}",
            model = self.model,
            rate = NATIVE_RATE
        )
    }

    /// Build the JSON body for `synthesize`. Exposed for tests.
    #[must_use]
    pub fn build_request_body(&self, text: &str) -> serde_json::Value {
        serde_json::to_value(SpeakReq { text })
            .expect("serialising static-shape Deepgram request must not fail")
    }
}

#[derive(Serialize)]
struct SpeakReq<'a> {
    text: &'a str,
}

#[async_trait]
impl TextToSpeech for DeepgramTts {
    fn name(&self) -> &'static str {
        "deepgram"
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
            .post(self.speech_url())
            .header("Authorization", format!("Token {}", self.api_key))
            .json(&body)
            .send()
            .await
            .context("posting to deepgram /v1/speak")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(anyhow!("deepgram TTS returned {status}: {}", truncate(&body, 400)));
        }
        let bytes = resp.bytes().await.context("reading deepgram TTS response body")?;
        let pcm = pcm_i16_le_to_f32(&bytes);
        Ok(TtsAudio { pcm, sample_rate: NATIVE_RATE })
    }

    async fn prewarm(&self) -> Result<()> {
        // No documented cheap GET; defer the TLS handshake to the
        // first synthesis call.
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
    fn deepgram_client_uses_catalogue_defaults() {
        let c = DeepgramTts::new("dg-test", None);
        assert_eq!(c.model(), "aura-2-thalia-en");
        assert_eq!(
            c.speech_url(),
            "https://api.deepgram.com/v1/speak?model=aura-2-thalia-en&encoding=linear16&sample_rate=24000"
        );
        assert_eq!(c.native_sample_rate(), NATIVE_RATE);
    }

    #[test]
    fn request_body_shape_matches_spec() {
        let c = DeepgramTts::new("dg-test", None);
        let body = c.build_request_body("hi");
        // Body is the bare `{ "text": ... }` document — no extras.
        assert_eq!(body["text"], "hi");
        let obj = body.as_object().expect("body is a JSON object");
        assert_eq!(obj.len(), 1, "body has only the `text` field");
    }

    /// Phase F regression: Deepgram uses `Token …`, not `Bearer …`.
    /// Capture the header value via a request builder shim.
    #[test]
    fn auth_header_uses_token_prefix() {
        let c = DeepgramTts::new("dg-key-x", None);
        let value = format!("Token {}", c.api_key);
        assert!(value.starts_with("Token "), "Deepgram auth must use the literal `Token` prefix");
        assert!(!value.starts_with("Bearer "), "Deepgram auth must NOT use Bearer");
    }

    #[tokio::test]
    async fn empty_text_returns_empty_audio_without_request() {
        let c = DeepgramTts::new("dg-test", None);
        let audio = c.synthesize("", None, None).await.unwrap();
        assert!(audio.pcm.is_empty());
        assert_eq!(audio.sample_rate, NATIVE_RATE);
    }
}
