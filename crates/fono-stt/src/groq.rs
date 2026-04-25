// SPDX-License-Identifier: GPL-3.0-only
//! Groq STT backend — fastest hosted whisper. HTTPS via reqwest+rustls.

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::multipart;
use serde::Deserialize;

use crate::traits::{SpeechToText, Transcription};

const GROQ_ENDPOINT: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const GROQ_MODELS_ENDPOINT: &str = "https://api.groq.com/openai/v1/models";
/// Default Groq STT model. Latency plan L15 picks `whisper-large-v3-turbo`
/// (≈5× faster than `whisper-1`) — overridable via `stt.cloud.model`.
const DEFAULT_MODEL: &str = "whisper-large-v3-turbo";

pub struct GroqStt {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl GroqStt {
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_model(api_key, DEFAULT_MODEL)
    }

    pub fn with_model(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            client: warm_client(),
        }
    }
}

/// Build a `reqwest::Client` tuned for low-latency reuse across many
/// short requests (latency plan L3): HTTP/2 keep-alive, idle pool kept
/// hot for a minute, generous-but-bounded request timeout.
pub(crate) fn warm_client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_idle_timeout(std::time::Duration::from_secs(60))
        .pool_max_idle_per_host(4)
        .tcp_keepalive(Some(std::time::Duration::from_secs(30)))
        .http2_keep_alive_interval(Some(std::time::Duration::from_secs(20)))
        .http2_keep_alive_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(45))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct GroqResponse {
    text: String,
    #[serde(default)]
    language: Option<String>,
}

#[async_trait]
impl SpeechToText for GroqStt {
    async fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
        lang: Option<&str>,
    ) -> Result<Transcription> {
        let wav = encode_wav(pcm, sample_rate);
        let part = multipart::Part::bytes(wav)
            .file_name("audio.wav")
            .mime_str("audio/wav")?;
        let mut form = multipart::Form::new()
            .text("model", self.model.clone())
            .part("file", part);
        if let Some(l) = lang {
            form = form.text("language", l.to_string());
        }

        let res = self
            .client
            .post(GROQ_ENDPOINT)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .context("groq POST failed")?;
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("groq STT {status}: {body}");
        }
        let parsed: GroqResponse =
            serde_json::from_str(&body).with_context(|| format!("parse groq response: {body}"))?;
        Ok(Transcription {
            text: parsed.text,
            language: parsed.language,
            duration_ms: None,
        })
    }

    fn name(&self) -> &'static str {
        "groq"
    }

    async fn prewarm(&self) -> Result<()> {
        // GET /v1/models is a cheap authed call; warms TCP+TLS so the
        // first real request doesn't pay handshake latency.
        let res = self
            .client
            .get(GROQ_MODELS_ENDPOINT)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .context("groq prewarm")?;
        let _ = res.bytes().await; // drain so the connection returns to the pool
        Ok(())
    }
}

/// Encode mono f32 samples as a 16-bit PCM WAV blob.
pub fn encode_wav(pcm: &[f32], sample_rate: u32) -> Vec<u8> {
    let num_samples = pcm.len() as u32;
    let byte_rate = sample_rate * 2;
    let data_size = num_samples * 2;
    let mut out = Vec::with_capacity(44 + data_size as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_size).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_size.to_le_bytes());
    for s in pcm {
        let clamped = s.clamp(-1.0, 1.0);
        let i = (clamped * i16::MAX as f32) as i16;
        out.extend_from_slice(&i.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_is_44_bytes() {
        let blob = encode_wav(&[0.0; 16], 16_000);
        assert_eq!(&blob[..4], b"RIFF");
        assert_eq!(&blob[8..12], b"WAVE");
        assert_eq!(blob.len(), 44 + 32);
    }
}
