// SPDX-License-Identifier: GPL-3.0-only
//! OpenAI STT backend (whisper-1 / gpt-4o-transcribe). Compatible JSON shape
//! with Groq for the text field.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::multipart;
use serde::Deserialize;

use crate::lang::LanguageSelection;
use crate::lang_cache::LanguageCache;
use crate::traits::{SpeechToText, Transcription};

const OPENAI_ENDPOINT: &str = "https://api.openai.com/v1/audio/transcriptions";
const OPENAI_MODELS_ENDPOINT: &str = "https://api.openai.com/v1/models";
const DEFAULT_MODEL: &str = "whisper-1";
pub(crate) const BACKEND_KEY: &str = "openai";

pub struct OpenAiStt {
    api_key: String,
    model: String,
    client: reqwest::Client,
    languages: Vec<String>,
    cloud_force_primary: bool,
    cloud_rerun_on_mismatch: bool,
    lang_cache: Arc<LanguageCache>,
}

impl OpenAiStt {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_model(api_key, DEFAULT_MODEL)
    }
    pub fn with_model(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            client: crate::groq::warm_client(),
            languages: Vec::new(),
            cloud_force_primary: false,
            cloud_rerun_on_mismatch: false,
            lang_cache: LanguageCache::global(),
        }
    }

    #[must_use]
    pub fn with_languages(mut self, codes: Vec<String>) -> Self {
        self.languages = codes;
        self
    }

    #[must_use]
    pub fn with_cloud_force_primary(mut self, on: bool) -> Self {
        self.cloud_force_primary = on;
        self
    }

    #[must_use]
    pub fn with_cloud_rerun_on_mismatch(mut self, on: bool) -> Self {
        self.cloud_rerun_on_mismatch = on;
        self
    }

    #[must_use]
    pub fn with_lang_cache(mut self, cache: Arc<LanguageCache>) -> Self {
        self.lang_cache = cache;
        self
    }

    fn effective_selection(&self, lang_override: Option<&str>) -> LanguageSelection {
        LanguageSelection::from_config(&self.languages).with_override(lang_override)
    }

    async fn do_request(&self, wav: &[u8], lang: Option<&str>) -> Result<Resp> {
        let part = multipart::Part::bytes(wav.to_vec())
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
            .post(OPENAI_ENDPOINT)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .context("openai POST failed")?;
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("openai STT {status}: {body}");
        }
        serde_json::from_str(&body).with_context(|| format!("parse openai response: {body}"))
    }
}

#[derive(Deserialize)]
struct Resp {
    text: String,
    /// Some OpenAI transcription endpoints (verbose_json,
    /// gpt-4o-transcribe) echo the detected language; whisper-1's
    /// default JSON shape does not. Keep it optional so both work.
    #[serde(default)]
    language: Option<String>,
}

#[async_trait]
impl SpeechToText for OpenAiStt {
    async fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
        lang: Option<&str>,
    ) -> Result<Transcription> {
        let wav = crate::groq::encode_wav(pcm, sample_rate);
        let selection = self.effective_selection(lang);

        let first_pass_lang: Option<String> = match &selection {
            LanguageSelection::Auto => None,
            LanguageSelection::Forced(c) => Some(c.clone()),
            LanguageSelection::AllowList(_) => {
                if self.cloud_force_primary {
                    selection.fallback_hint().map(str::to_string)
                } else {
                    None
                }
            }
        };

        let parsed = self.do_request(&wav, first_pass_lang.as_deref()).await?;

        if let LanguageSelection::AllowList(_) = &selection {
            if let Some(detected) = parsed.language.as_deref() {
                if selection.contains(detected) {
                    self.lang_cache.record(BACKEND_KEY, detected);
                } else if self.cloud_rerun_on_mismatch {
                    if let Some(cached) = self.lang_cache.get(BACKEND_KEY) {
                        tracing::warn!(
                            "openai returned banned language {detected:?} (allow-list \
                             {:?}); re-issuing with cached language={cached}",
                            self.languages
                        );
                        let retried = self.do_request(&wav, Some(&cached)).await?;
                        return Ok(Transcription {
                            text: retried.text,
                            language: retried.language.or(Some(cached)),
                            duration_ms: None,
                        });
                    }
                    tracing::debug!(
                        "openai detected banned language {detected:?}; cache empty, \
                         accepting unforced response"
                    );
                } else {
                    tracing::debug!("openai detected banned language {detected:?}; rerun disabled");
                }
            }
        }

        // Fall back to first_pass_lang for the language field when the
        // provider doesn't echo one (whisper-1 default JSON).
        let language = parsed
            .language
            .or_else(|| first_pass_lang.clone())
            .or_else(|| selection.fallback_hint().map(str::to_string));

        Ok(Transcription {
            text: parsed.text,
            language,
            duration_ms: None,
        })
    }

    fn name(&self) -> &'static str {
        "openai"
    }

    async fn prewarm(&self) -> Result<()> {
        let res = self
            .client
            .get(OPENAI_MODELS_ENDPOINT)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .context("openai prewarm")?;
        let _ = res.bytes().await;
        Ok(())
    }
}
