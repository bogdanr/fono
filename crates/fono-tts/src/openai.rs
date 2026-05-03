// SPDX-License-Identifier: GPL-3.0-only
//! OpenAI `/v1/audio/speech` client.
//!
//! POSTs `{ model, voice, input, response_format: "pcm" }` and reads
//! the response body as raw int16 LE mono PCM at 24 kHz (the documented
//! native rate for `pcm` output). Decoded to `f32` mono on the way
//! back. We deliberately avoid the `mp3` default to skip MP3 decoding.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::Serialize;

use crate::traits::{TextToSpeech, TtsAudio};

/// OpenAI's `pcm` response format is documented as 24 kHz mono int16 LE.
const NATIVE_RATE: u32 = 24_000;
const ENDPOINT: &str = "https://api.openai.com/v1/audio/speech";
const MODELS_ENDPOINT: &str = "https://api.openai.com/v1/models";

pub struct OpenAiTts {
    api_key: String,
    model: String,
    /// Default voice when the per-call `voice` argument is `None`.
    default_voice: String,
    client: reqwest::Client,
}

impl OpenAiTts {
    #[must_use]
    pub fn new(
        api_key: impl Into<String>,
        model: impl Into<String>,
        default_voice: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            default_voice: default_voice.into(),
            client: warm_client(),
        }
    }
}

/// Warm reqwest client tuned for short, latency-sensitive POSTs. Same
/// shape as `fono_llm::openai_compat::warm_client()`; kept local to
/// avoid pulling fono-llm into fono-tts.
fn warm_client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(4)
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .http2_keep_alive_interval(Some(Duration::from_secs(20)))
        .http2_keep_alive_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default()
}

#[derive(Serialize)]
struct SpeechReq<'a> {
    model: &'a str,
    voice: &'a str,
    input: &'a str,
    response_format: &'static str,
}

#[async_trait]
impl TextToSpeech for OpenAiTts {
    fn name(&self) -> &'static str {
        "openai"
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
            return Ok(TtsAudio {
                pcm: Vec::new(),
                sample_rate: NATIVE_RATE,
            });
        }
        let v = voice
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(self.default_voice.as_str());
        let req = SpeechReq {
            model: &self.model,
            voice: v,
            input: text,
            response_format: "pcm",
        };
        let resp = self
            .client
            .post(ENDPOINT)
            .bearer_auth(&self.api_key)
            .json(&req)
            .send()
            .await
            .context("posting to openai /v1/audio/speech")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(anyhow!(
                "openai TTS returned {status}: {}",
                truncate(&body, 400)
            ));
        }
        let bytes = resp
            .bytes()
            .await
            .context("reading openai TTS response body")?;
        let pcm = pcm_i16_le_to_f32(&bytes);
        Ok(TtsAudio {
            pcm,
            sample_rate: NATIVE_RATE,
        })
    }

    async fn prewarm(&self) -> Result<()> {
        // Cheap GET to `/v1/models` pays the TLS handshake before the
        // user's first F10 press. Auth header is required; we don't
        // care about the response.
        let _ = self
            .client
            .get(MODELS_ENDPOINT)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .context("openai TTS prewarm GET /v1/models")?;
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

    #[tokio::test]
    async fn empty_text_returns_empty_audio_without_request() {
        let tts = OpenAiTts::new("sk-test", "tts-1", "alloy");
        let audio = tts.synthesize("", None, None).await.unwrap();
        assert!(audio.pcm.is_empty());
        assert_eq!(audio.sample_rate, NATIVE_RATE);
    }

    #[test]
    fn pcm_decode_round_trip() {
        let bytes: Vec<u8> = [0_i16, 32767, -32767]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let f = pcm_i16_le_to_f32(&bytes);
        assert_eq!(f.len(), 3);
        assert!((f[1] - 1.0).abs() < 1e-3);
        assert!((f[2] - -1.0).abs() < 1e-3);
    }

    #[test]
    fn truncate_handles_short_strings() {
        assert_eq!(truncate("hello", 10), "hello");
    }
}
