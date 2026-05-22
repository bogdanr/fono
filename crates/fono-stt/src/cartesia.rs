// SPDX-License-Identifier: GPL-3.0-only
//! Cartesia STT backend — batch `POST /stt` against the `ink-whisper`
//! family. Realtime `ink-2` over the turn-based WebSocket endpoint
//! (`wss://api.cartesia.ai/stt/turns/websocket`) is deferred to a
//! Phase 2 streaming slice; see
//! `plans/2026-05-23-cartesia-stt-support-v2.md`.
//!
//! Differences from [`crate::groq`] that callers should know about:
//!
//! * **No verbose / segment-level scores.** Cartesia's batch endpoint
//!   returns `{ text, language?, duration?, words? }`. There is no
//!   per-segment `avg_logprob` / `no_speech_prob`, so we cannot run
//!   the Whisper-style logprob rerun ([`crate::groq::pick_best_peer`])
//!   nor the silence-hallucination filter
//!   ([`crate::groq::filter_hallucinated_segments`]). When the user
//!   sets `general.cloud_rerun_on_language_mismatch = true` we log
//!   one warning per process to flag the degradation and otherwise
//!   ignore the knob.
//! * **Language goes on the query string.** Per the batch endpoint
//!   docs, `language` is a query parameter (ISO-639-1) — not a
//!   multipart form field like Groq / OpenAI.
//! * **Auth header.** `X-Api-Key` plus a `Cartesia-Version` pin,
//!   matching `crates/fono-tts/src/cartesia.rs` (header names are
//!   case-insensitive per RFC 7230 §3.2 so the docs' `X-API-Key`
//!   spelling also works; we keep the TTS spelling to stay
//!   consistent with the TTS client and the wizard validator).
//!
//! The model id default lives in the catalogue
//! (`crates/fono-core/src/provider_catalog.rs`); pass anything from
//! the `ink-whisper` family (`ink-whisper`, future variants) via
//! `[stt.cloud].model` to override.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::multipart;
use serde::Deserialize;

use crate::lang::LanguageSelection;
use crate::traits::{SpeechToText, Transcription};

const CARTESIA_ENDPOINT: &str = "https://api.cartesia.ai/stt";
/// Pinned API version sent in the `Cartesia-Version` header. Mirrors
/// `crates/fono-tts/src/cartesia.rs::API_VERSION` so the two clients
/// stay in lockstep with the same documented contract.
const API_VERSION: &str = "2026-03-01";
/// Default Cartesia batch STT model. Must be in the `ink-whisper`
/// family per the batch-endpoint contract — `ink-2` is realtime-only.
const DEFAULT_MODEL: &str = "ink-whisper";
/// Stable key the language-cache layer uses for this backend.
pub(crate) const BACKEND_KEY: &str = "cartesia";

/// Process-wide flag for the "rerun unavailable" warning. We log once
/// per binary instead of every transcription so a configured allow-list
/// with `cloud_rerun_on_language_mismatch = true` doesn't spam the log.
static RERUN_WARN_LOGGED: AtomicBool = AtomicBool::new(false);

pub struct CartesiaStt {
    api_key: String,
    model: String,
    client: reqwest::Client,
    languages: Vec<String>,
    /// Captured from `general.cloud_rerun_on_language_mismatch`. Cartesia
    /// can't honour it (no logprobs), so the only effect is the
    /// one-shot warning in [`CartesiaStt::transcribe`].
    cloud_rerun_on_mismatch: bool,
    prompts: std::collections::HashMap<String, String>,
}

impl CartesiaStt {
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
    /// Cartesia has no per-segment confidence scores so the rerun
    /// itself is a no-op; we keep the setter for symmetry with
    /// [`crate::groq::GroqStt`] and to drive the one-shot warning.
    #[must_use]
    pub fn with_cloud_rerun_on_mismatch(mut self, on: bool) -> Self {
        self.cloud_rerun_on_mismatch = on;
        self
    }

    /// Builder: per-language initial-prompt map. Cartesia accepts no
    /// equivalent of Whisper's `prompt` field on the batch endpoint
    /// today; the map is captured for forward compatibility (so
    /// `[stt.prompts]` doesn't error) but currently unused on the
    /// wire. Documented in `docs/providers.md`.
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
pub struct CartesiaResponse {
    pub text: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
}

/// Single batch POST to `https://api.cartesia.ai/stt`. Multipart body
/// carries `file` + `model`; `language` is a **query parameter** (per
/// the documented endpoint shape) rather than a form field.
pub(crate) async fn cartesia_post_wav(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    wav: &[u8],
    lang: Option<&str>,
) -> Result<CartesiaResponse> {
    let part = multipart::Part::bytes(wav.to_vec()).file_name("audio.wav").mime_str("audio/wav")?;
    let form = multipart::Form::new().text("model", model.to_string()).part("file", part);
    let mut req = client
        .post(CARTESIA_ENDPOINT)
        .header("X-Api-Key", api_key)
        .header("Cartesia-Version", API_VERSION);
    if let Some(l) = lang {
        req = req.query(&[("language", l)]);
    }
    let res = req.multipart(form).send().await.context("cartesia POST failed")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("cartesia STT returned {status}: {body}");
    }
    serde_json::from_str(&body).with_context(|| format!("parse cartesia response: {body}"))
}

#[async_trait]
impl SpeechToText for CartesiaStt {
    async fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
        lang: Option<&str>,
    ) -> Result<Transcription> {
        let wav = crate::groq::encode_wav(pcm, sample_rate);
        let selection = self.effective_selection(lang);

        // First-pass language: forced → the code; auto → none;
        // allow-list → none (Cartesia auto-detects within its
        // supported set, and we post-validate / warn below).
        let first_pass_lang: Option<String> = match &selection {
            LanguageSelection::Auto => None,
            LanguageSelection::Forced(c) => Some(c.clone()),
            LanguageSelection::AllowList(_) => None,
        };

        let parsed = cartesia_post_wav(
            &self.client,
            &self.api_key,
            &self.model,
            &wav,
            first_pass_lang.as_deref(),
        )
        .await?;

        // Cartesia has no per-segment confidence, so the
        // `cloud_rerun_on_language_mismatch` knob is informational
        // only. Warn once per process when the user opted in so the
        // degradation is visible without flooding the log.
        if let LanguageSelection::AllowList(_) = &selection {
            if let Some(detected_raw) = parsed.language.as_deref() {
                let detected = crate::lang::whisper_lang_to_code(detected_raw);
                if !selection.contains(&detected) {
                    if self.cloud_rerun_on_mismatch
                        && !RERUN_WARN_LOGGED.swap(true, Ordering::Relaxed)
                    {
                        tracing::warn!(
                            "cartesia STT detected banned language {detected_raw:?} \
                             (normalised {detected:?}, allow-list {:?}); rerun-on-mismatch \
                             is unavailable on Cartesia (no per-segment logprobs) — \
                             accepting the detected response. This warning is logged once.",
                            self.languages
                        );
                    } else {
                        tracing::info!(
                            "cartesia STT detected banned language {detected_raw:?} \
                             (normalised {detected:?}); accepting unforced response"
                        );
                    }
                }
            }
        }

        Ok(Transcription { text: parsed.text, language: parsed.language, duration_ms: None })
    }

    fn name(&self) -> &'static str {
        "cartesia"
    }

    async fn prewarm(&self) -> Result<()> {
        // No cheap authed GET equivalent of Groq's `/models` is
        // documented for STT specifically; reuse the `/voices` listing
        // (used by the TTS client) so we warm the same TLS pool. The
        // 200 body is small and gets drained immediately.
        let res = self
            .client
            .get("https://api.cartesia.ai/voices")
            .header("X-Api-Key", &self.api_key)
            .header("Cartesia-Version", API_VERSION)
            .query(&[("limit", "1")])
            .send()
            .await
            .context("cartesia prewarm")?;
        let _ = res.bytes().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_matches_catalogue() {
        // Catch the catalogue drift bug that bit `sonic-transcribe`.
        // If someone edits one without the other this test fires.
        assert_eq!(DEFAULT_MODEL, crate::defaults::default_cloud_model("cartesia"));
    }

    #[test]
    fn parses_minimal_response() {
        let body = r#"{"text":"hello world"}"#;
        let parsed: CartesiaResponse = serde_json::from_str(body).expect("parse");
        assert_eq!(parsed.text, "hello world");
        assert_eq!(parsed.language, None);
        assert_eq!(parsed.duration, None);
    }

    #[test]
    fn parses_full_response_with_extra_fields() {
        // Docs say the response may also carry `words` — Cartesia is
        // free to add more fields without warning. `serde(default)` +
        // forward-compatible shape means unknown fields must not
        // break the parser.
        let body = r#"{
            "text": "salut",
            "language": "ro",
            "duration": 1.42,
            "words": [{"word": "salut", "start": 0.1, "end": 0.7}],
            "request_id": "req_123"
        }"#;
        let parsed: CartesiaResponse = serde_json::from_str(body).expect("parse");
        assert_eq!(parsed.text, "salut");
        assert_eq!(parsed.language.as_deref(), Some("ro"));
        assert_eq!(parsed.duration, Some(1.42));
    }

    #[test]
    fn builder_captures_languages_and_prompts() {
        let mut prompts = std::collections::HashMap::new();
        prompts.insert("en".to_string(), "Professional dictation.".to_string());
        let stt = CartesiaStt::new("sk_test")
            .with_languages(vec!["en".into(), "ro".into()])
            .with_prompts(prompts)
            .with_cloud_rerun_on_mismatch(true);
        assert_eq!(stt.languages, vec!["en", "ro"]);
        assert_eq!(stt.prompts.get("en").map(String::as_str), Some("Professional dictation."));
        assert!(stt.cloud_rerun_on_mismatch);
    }

    #[test]
    fn name_is_stable_label() {
        // Tray / doctor / critical_notify all key off the backend
        // name; if it ever changes the catalogue and provider tables
        // must change in lockstep.
        let stt = CartesiaStt::new("sk_test");
        assert_eq!(stt.name(), "cartesia");
    }
}
