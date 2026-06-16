// SPDX-License-Identifier: GPL-3.0-only
//! ElevenLabs Scribe STT backend — batch `POST /v1/speech-to-text`
//! against the `scribe_v1` model.
//!
//! Differences from [`crate::groq`] that callers should know about,
//! mirroring [`crate::cartesia`]:
//!
//! * **No verbose / segment-level scores.** Scribe returns
//!   `{ language_code, language_probability, text, words? }`. There is
//!   no per-segment `avg_logprob` / `no_speech_prob`, so we cannot run
//!   the Whisper-style logprob rerun ([`crate::groq::pick_best_peer`])
//!   nor the silence-hallucination filter. When the user sets
//!   `general.cloud_rerun_on_language_mismatch = true` we log one
//!   warning per process to flag the degradation and otherwise ignore
//!   the knob.
//! * **Multipart form fields.** `model_id` + `file`; the forced
//!   language goes in the optional `language_code` form field (ISO
//!   639-1/3), not a query parameter.
//! * **Auth header.** `xi-api-key: <key>`.
//!
//! The model id default lives in the catalogue
//! (`crates/fono-core/src/provider_catalog.rs`); pass any Scribe
//! variant via `[stt.cloud].model` to override.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::multipart;
use serde::Deserialize;

use crate::lang::LanguageSelection;
use crate::traits::{SpeechToText, Transcription};

const ENDPOINT: &str = "https://api.elevenlabs.io/v1/speech-to-text";
const MODELS_ENDPOINT: &str = "https://api.elevenlabs.io/v1/models";
/// Default ElevenLabs Scribe model.
const DEFAULT_MODEL: &str = "scribe_v1";
/// Stable key the language-cache layer uses for this backend.
pub(crate) const BACKEND_KEY: &str = "elevenlabs";

/// Process-wide flag for the "rerun unavailable" warning. We log once
/// per binary instead of every transcription so a configured allow-list
/// with `cloud_rerun_on_language_mismatch = true` doesn't spam the log.
static RERUN_WARN_LOGGED: AtomicBool = AtomicBool::new(false);

pub struct ElevenLabsStt {
    api_key: String,
    model: String,
    client: reqwest::Client,
    languages: Vec<String>,
    /// Captured from `general.cloud_rerun_on_language_mismatch`. Scribe
    /// can't honour it (no logprobs), so the only effect is the
    /// one-shot warning in [`ElevenLabsStt::transcribe`].
    cloud_rerun_on_mismatch: bool,
    prompts: std::collections::HashMap<String, String>,
}

impl ElevenLabsStt {
    #[must_use]
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
            prompts: std::collections::HashMap::new(),
        }
    }

    /// Builder: configured language allow-list. See
    /// [`crate::lang::LanguageSelection`] for semantics.
    #[must_use]
    pub fn with_languages(mut self, codes: Vec<String>) -> Self {
        self.languages = codes;
        self
    }

    /// Builder: capture `general.cloud_rerun_on_language_mismatch`.
    /// Scribe has no per-segment confidence scores so the rerun itself
    /// is a no-op; we keep the setter for symmetry with
    /// [`crate::groq::GroqStt`] and to drive the one-shot warning.
    #[must_use]
    pub fn with_cloud_rerun_on_mismatch(mut self, on: bool) -> Self {
        self.cloud_rerun_on_mismatch = on;
        self
    }

    /// Builder: per-language initial-prompt map. Scribe accepts no
    /// equivalent of Whisper's `prompt` field; the map is captured for
    /// forward compatibility (so `[stt.prompts]` doesn't error) but
    /// currently unused on the wire. Documented in `docs/providers.md`.
    #[must_use]
    pub fn with_prompts(mut self, prompts: std::collections::HashMap<String, String>) -> Self {
        self.prompts = prompts;
        self
    }

    fn effective_selection(&self, lang_override: Option<&str>) -> LanguageSelection {
        LanguageSelection::from_config(&self.languages).with_override(lang_override)
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct ScribeResponse {
    pub text: String,
    #[serde(default)]
    pub language_code: Option<String>,
    #[serde(default)]
    pub language_probability: Option<f64>,
}

/// Single batch POST to `https://api.elevenlabs.io/v1/speech-to-text`.
/// Multipart body carries `model_id` + `file`; a forced `language_code`
/// (ISO 639-1/3) is sent as an extra form field when present.
pub(crate) async fn elevenlabs_post_wav(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    wav: &[u8],
    lang: Option<&str>,
) -> Result<ScribeResponse> {
    let part = multipart::Part::bytes(wav.to_vec()).file_name("audio.wav").mime_str("audio/wav")?;
    let mut form = multipart::Form::new().text("model_id", model.to_string()).part("file", part);
    if let Some(l) = lang {
        form = form.text("language_code", l.to_string());
    }
    let res = client
        .post(ENDPOINT)
        .header("xi-api-key", api_key)
        .multipart(form)
        .send()
        .await
        .context("elevenlabs POST failed")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("elevenlabs STT returned {status}: {body}");
    }
    serde_json::from_str(&body).with_context(|| format!("parse elevenlabs response: {body}"))
}

#[async_trait]
impl SpeechToText for ElevenLabsStt {
    async fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
        lang: Option<&str>,
    ) -> Result<Transcription> {
        let wav = crate::groq::encode_wav(pcm, sample_rate);
        let selection = self.effective_selection(lang);

        // First-pass language: forced → the code; auto → none;
        // allow-list → none (Scribe auto-detects and we post-validate /
        // warn below).
        let first_pass_lang: Option<String> = match &selection {
            LanguageSelection::Auto => None,
            LanguageSelection::Forced(c) => Some(c.clone()),
            LanguageSelection::AllowList(_) => None,
        };

        let parsed = elevenlabs_post_wav(
            &self.client,
            &self.api_key,
            &self.model,
            &wav,
            first_pass_lang.as_deref(),
        )
        .await?;

        // Scribe has no per-segment confidence, so the
        // `cloud_rerun_on_language_mismatch` knob is informational only.
        // Warn once per process when the user opted in so the
        // degradation is visible without flooding the log.
        if let LanguageSelection::AllowList(_) = &selection {
            if let Some(detected_raw) = parsed.language_code.as_deref() {
                let detected = crate::lang::whisper_lang_to_code(detected_raw);
                if !selection.contains(&detected) {
                    if self.cloud_rerun_on_mismatch
                        && !RERUN_WARN_LOGGED.swap(true, Ordering::Relaxed)
                    {
                        tracing::warn!(
                            "elevenlabs STT detected banned language {detected_raw:?} \
                             (normalised {detected:?}, allow-list {:?}); rerun-on-mismatch \
                             is unavailable on Scribe (no per-segment logprobs) — \
                             accepting the detected response. This warning is logged once.",
                            self.languages
                        );
                    } else {
                        tracing::info!(
                            "elevenlabs STT detected banned language {detected_raw:?} \
                             (normalised {detected:?}); accepting unforced response"
                        );
                    }
                }
            }
        }

        // Scribe returns alpha-3 ISO 639-2/3 codes (`eng`, `ron`).
        // Normalise to alpha-2 so downstream consumers (TTS language
        // hint, assistant summary, history) see `en`/`ro`, matching the
        // user's configured allow-list and every other backend.
        let normalised_lang =
            parsed.language_code.as_deref().map(crate::lang::whisper_lang_to_code);

        Ok(Transcription { text: parsed.text, language: normalised_lang, duration_ms: None })
    }

    fn name(&self) -> &'static str {
        "elevenlabs"
    }

    async fn prewarm(&self) -> Result<()> {
        // Warm the TLS pool with a cheap authed GET against the model
        // listing (the same endpoint the wizard probes for key
        // validation). The small 200 body is drained immediately.
        let res = self
            .client
            .get(MODELS_ENDPOINT)
            .header("xi-api-key", &self.api_key)
            .send()
            .await
            .context("elevenlabs prewarm")?;
        let _ = res.bytes().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_matches_catalogue() {
        assert_eq!(DEFAULT_MODEL, crate::defaults::default_cloud_model("elevenlabs"));
    }

    #[test]
    fn parses_minimal_response() {
        let body = r#"{"text":"hello world"}"#;
        let parsed: ScribeResponse = serde_json::from_str(body).expect("parse");
        assert_eq!(parsed.text, "hello world");
        assert_eq!(parsed.language_code, None);
        assert_eq!(parsed.language_probability, None);
    }

    #[test]
    fn parses_full_response_with_extra_fields() {
        // Scribe also returns `words`; unknown fields must not break
        // the forward-compatible parser.
        let body = r#"{
            "language_code": "eng",
            "language_probability": 0.99,
            "text": "salut",
            "words": [{"text": "salut", "start": 0.1, "end": 0.7, "type": "word"}],
            "request_id": "req_123"
        }"#;
        let parsed: ScribeResponse = serde_json::from_str(body).expect("parse");
        assert_eq!(parsed.text, "salut");
        assert_eq!(parsed.language_code.as_deref(), Some("eng"));
        assert_eq!(parsed.language_probability, Some(0.99));
    }

    #[test]
    fn builder_captures_languages_and_prompts() {
        let mut prompts = std::collections::HashMap::new();
        prompts.insert("en".to_string(), "Professional dictation.".to_string());
        let stt = ElevenLabsStt::new("sk_test")
            .with_languages(vec!["en".into(), "ro".into()])
            .with_prompts(prompts)
            .with_cloud_rerun_on_mismatch(true);
        assert_eq!(stt.languages, vec!["en", "ro"]);
        assert_eq!(stt.prompts.get("en").map(String::as_str), Some("Professional dictation."));
        assert!(stt.cloud_rerun_on_mismatch);
    }

    #[test]
    fn name_is_stable_label() {
        let stt = ElevenLabsStt::new("sk_test");
        assert_eq!(stt.name(), "elevenlabs");
    }
}
