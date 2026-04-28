// SPDX-License-Identifier: GPL-3.0-only
//! Groq STT backend — fastest hosted whisper. HTTPS via reqwest+rustls.

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::multipart;
use serde::Deserialize;

use crate::lang::LanguageSelection;
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
    /// Configured language allow-list (see `crate::lang`).
    languages: Vec<String>,
    /// When the allow-list has > 1 entry, send the primary code on
    /// the first request instead of letting Groq auto-detect.
    cloud_force_primary: bool,
    /// When the provider returns a language outside the allow-list,
    /// re-issue the request with the primary code forced. Costs an
    /// extra round-trip per mismatch; off by default.
    cloud_rerun_on_mismatch: bool,
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
            languages: Vec::new(),
            cloud_force_primary: false,
            cloud_rerun_on_mismatch: false,
        }
    }

    /// Builder: set the language allow-list. See
    /// [`crate::lang::LanguageSelection`] for semantics.
    #[must_use]
    pub fn with_languages(mut self, codes: Vec<String>) -> Self {
        self.languages = codes;
        self
    }

    /// Builder: when the allow-list has > 1 entry, force the primary
    /// code on the first request. Default `false`.
    #[must_use]
    pub fn with_cloud_force_primary(mut self, on: bool) -> Self {
        self.cloud_force_primary = on;
        self
    }

    /// Builder: re-issue the request with the primary code if the
    /// provider returned a banned language. Default `false`.
    #[must_use]
    pub fn with_cloud_rerun_on_mismatch(mut self, on: bool) -> Self {
        self.cloud_rerun_on_mismatch = on;
        self
    }

    fn effective_selection(&self, lang_override: Option<&str>) -> LanguageSelection {
        LanguageSelection::from_config(&self.languages).with_override(lang_override)
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

#[derive(Deserialize, Debug, Clone)]
pub struct GroqResponse {
    pub text: String,
    #[serde(default)]
    pub language: Option<String>,
}

/// Issue a single transcription request to Groq's batch endpoint.
/// Shared between [`GroqStt`] (batch) and the streaming pseudo-stream
/// backend in [`crate::groq_streaming`] (re-POSTs the trailing N
/// seconds every 700 ms). The caller resolves the model + language
/// allow-list semantics; this helper just does the multipart POST and
/// parses the JSON.
pub(crate) async fn groq_post_wav(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    wav: &[u8],
    lang: Option<&str>,
) -> Result<GroqResponse> {
    let part = multipart::Part::bytes(wav.to_vec())
        .file_name("audio.wav")
        .mime_str("audio/wav")?;
    let mut form = multipart::Form::new()
        .text("model", model.to_string())
        .part("file", part);
    if let Some(l) = lang {
        form = form.text("language", l.to_string());
    }
    let res = client
        .post(GROQ_ENDPOINT)
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .context("groq POST failed")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("groq STT {status}: {body}");
    }
    serde_json::from_str(&body).with_context(|| format!("parse groq response: {body}"))
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
        let selection = self.effective_selection(lang);

        // First-pass language to send: forced -> the code; auto -> none;
        // allow-list -> primary if `cloud_force_primary`, else none and
        // we post-validate the provider's detected language.
        let first_pass_lang: Option<String> = match &selection {
            LanguageSelection::Auto => None,
            LanguageSelection::Forced(c) => Some(c.clone()),
            LanguageSelection::AllowList(_) => {
                if self.cloud_force_primary {
                    selection.primary().map(str::to_string)
                } else {
                    None
                }
            }
        };

        let parsed = self.do_request(&wav, first_pass_lang.as_deref()).await?;

        // Post-validate against the allow-list. Whisper-flavoured cloud
        // STT providers (Groq, OpenAI) return the detected language in
        // the response; if it falls outside our allow-list, optionally
        // re-issue the same audio with the primary code forced.
        if let LanguageSelection::AllowList(_) = &selection {
            if let Some(detected) = parsed.language.as_deref() {
                if !selection.contains(detected) {
                    let primary = selection.primary().unwrap_or("");
                    if self.cloud_rerun_on_mismatch && !primary.is_empty() {
                        tracing::warn!(
                            "groq returned banned language {detected:?} (allow-list \
                             {:?}); re-issuing with language={primary}",
                            self.languages
                        );
                        let retried = self.do_request(&wav, Some(primary)).await?;
                        return Ok(Transcription {
                            text: retried.text,
                            language: retried.language.or_else(|| Some(primary.to_string())),
                            duration_ms: None,
                        });
                    }
                    tracing::warn!(
                        "groq detected language {detected:?} is outside the allow-list \
                         {:?}; accepting transcript as-is (set \
                         general.cloud_rerun_on_language_mismatch = true to re-issue)",
                        self.languages
                    );
                }
            }
        }

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

impl GroqStt {
    /// Issue a single transcription request to Groq with the given
    /// optional `language` form field. Factored out so the
    /// post-validation rerun path is one extra await away.
    async fn do_request(&self, wav: &[u8], lang: Option<&str>) -> Result<GroqResponse> {
        groq_post_wav(&self.client, &self.api_key, &self.model, wav, lang).await
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
