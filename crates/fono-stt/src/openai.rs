// SPDX-License-Identifier: GPL-3.0-only
//! OpenAI STT backend (whisper-1 / gpt-4o-transcribe). Compatible JSON shape
//! with Groq for the text field.

use std::collections::HashMap;
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
    cloud_rerun_on_mismatch: bool,
    lang_cache: Arc<LanguageCache>,
    prompts: HashMap<String, String>,
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
            cloud_rerun_on_mismatch: false,
            lang_cache: LanguageCache::global(),
            prompts: HashMap::new(),
        }
    }

    #[must_use]
    pub fn with_languages(mut self, codes: Vec<String>) -> Self {
        self.languages = codes;
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

    /// Builder: per-language initial-prompt map. The prompt for the
    /// resolved language (if any) is included as the `prompt` form
    /// field on every request.
    #[must_use]
    pub fn with_prompts(mut self, prompts: HashMap<String, String>) -> Self {
        self.prompts = prompts;
        self
    }

    fn effective_selection(&self, lang_override: Option<&str>) -> LanguageSelection {
        LanguageSelection::from_config(&self.languages).with_override(lang_override)
    }

    fn prompt_for(&self, lang: Option<&str>) -> Option<&str> {
        lang.and_then(|l| self.prompts.get(l)).map(String::as_str)
    }

    async fn do_request(&self, wav: &[u8], lang: Option<&str>) -> Result<Resp> {
        let part = multipart::Part::bytes(wav.to_vec())
            .file_name("audio.wav")
            .mime_str("audio/wav")?;
        // Always request `verbose_json` so the response includes the
        // detected `language` field. whisper-1's plain `json` shape
        // does not, which means the post-validation gate would never
        // fire. See the matching comment in `groq::groq_post_wav`.
        let mut form = multipart::Form::new()
            .text("model", self.model.clone())
            .text("response_format", "verbose_json")
            .part("file", part);
        if let Some(l) = lang {
            form = form.text("language", l.to_string());
        }
        if let Some(p) = self.prompt_for(lang) {
            form = form.text("prompt", p.to_string());
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

    /// `verbose_json` variant for the per-peer rerun lane. Same shape
    /// as Groq's `verbose_json` (whisper-1 compatible). Some
    /// gpt-4o-transcribe deployments may return an empty `segments`
    /// array; in that case `mean_logprob()` returns `f32::NEG_INFINITY`
    /// and the first peer in iteration order wins by default.
    async fn do_request_verbose(&self, wav: &[u8], lang: Option<&str>) -> Result<VerboseResp> {
        let part = multipart::Part::bytes(wav.to_vec())
            .file_name("audio.wav")
            .mime_str("audio/wav")?;
        let mut form = multipart::Form::new()
            .text("model", self.model.clone())
            .text("response_format", "verbose_json")
            .part("file", part);
        if let Some(l) = lang {
            form = form.text("language", l.to_string());
        }
        if let Some(p) = self.prompt_for(lang) {
            form = form.text("prompt", p.to_string());
        }
        let res = self
            .client
            .post(OPENAI_ENDPOINT)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .context("openai verbose POST failed")?;
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("openai STT verbose {status}: {body}");
        }
        serde_json::from_str(&body)
            .with_context(|| format!("parse openai verbose response: {body}"))
    }

    /// Per-peer rerun: identical strategy to
    /// [`crate::groq::GroqStt::pick_best_peer`].
    async fn pick_best_peer(&self, wav: &[u8], peers: &[String]) -> Option<(String, VerboseResp)> {
        let mut best: Option<(f32, String, VerboseResp)> = None;
        for peer in peers {
            match self.do_request_verbose(wav, Some(peer)).await {
                Ok(r) => {
                    let score = r.mean_logprob();
                    tracing::info!(
                        "openai rerun candidate language={peer}: avg_logprob={score:.3}"
                    );
                    if best.as_ref().is_none_or(|(s, _, _)| score > *s) {
                        best = Some((score, peer.clone(), r));
                    }
                }
                Err(e) => {
                    tracing::warn!("openai rerun candidate language={peer} failed: {e:#}");
                }
            }
        }
        best.map(|(_, code, resp)| (code, resp))
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

#[derive(Deserialize)]
struct VerboseResp {
    text: String,
    #[serde(default)]
    segments: Vec<VerboseSeg>,
}

#[derive(Deserialize)]
struct VerboseSeg {
    #[serde(default)]
    avg_logprob: Option<f32>,
}

impl VerboseResp {
    fn mean_logprob(&self) -> f32 {
        let scored: Vec<f32> = self.segments.iter().filter_map(|s| s.avg_logprob).collect();
        if scored.is_empty() {
            f32::NEG_INFINITY
        } else {
            scored.iter().sum::<f32>() / scored.len() as f32
        }
    }
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
            LanguageSelection::AllowList(_) => None,
        };

        let parsed = self.do_request(&wav, first_pass_lang.as_deref()).await?;

        if let LanguageSelection::AllowList(peers) = &selection {
            if let Some(detected_raw) = parsed.language.as_deref() {
                let detected = crate::lang::whisper_lang_to_code(detected_raw);
                if selection.contains(&detected) {
                    self.lang_cache.record(BACKEND_KEY, &detected);
                } else if self.cloud_rerun_on_mismatch {
                    tracing::info!(
                        "openai returned banned language {detected_raw:?} (normalised \
                         {detected:?}, allow-list {:?}); reranking by per-peer avg_logprob",
                        self.languages
                    );
                    if let Some((picked, resp)) = self.pick_best_peer(&wav, peers).await {
                        self.lang_cache.record(BACKEND_KEY, &picked);
                        return Ok(Transcription {
                            text: resp.text,
                            language: Some(picked),
                            duration_ms: None,
                        });
                    }
                    tracing::warn!(
                        "openai rerun: every peer attempt failed; \
                         falling back to unforced response"
                    );
                } else {
                    tracing::info!("openai detected banned language {detected_raw:?} (normalised {detected:?}); rerun disabled");
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
