// SPDX-License-Identifier: GPL-3.0-only
//! Gemini speech-to-text via the Gemini API's audio-understanding path —
//! batch `POST /v1beta/models/<model>:generateContent` on a single
//! `GEMINI_API_KEY` (free tier). See ADR 0034.
//!
//! This is **prompt-driven transcription**, not a dedicated ASR. Audio is
//! attached inline (base64 WAV) to a normal multimodal model alongside a
//! strict transcribe-only instruction; the model returns the transcript as
//! plain text. Consequences callers must know about (mirroring
//! [`crate::elevenlabs`] / [`crate::cartesia`]):
//!
//! * **No per-segment confidence and no detected language code.** There is
//!   no `avg_logprob` / `no_speech_prob`, so we cannot run the Whisper-style
//!   logprob rerun ([`crate::groq::pick_best_peer`]) nor the
//!   silence-hallucination filter. When the user sets
//!   `general.cloud_rerun_on_language_mismatch = true` together with a
//!   language allow-list we log one warning per process to flag the
//!   degradation and otherwise ignore the knob.
//! * **Batch only.** There is no native streaming ASR here; F7 streaming
//!   dictation stays on the streaming backends (the realtime Live API
//!   provides transcription on the realtime path instead).
//! * **Auth header.** `x-goog-api-key: <key>`.
//!
//! The model id default lives in the catalogue
//! (`crates/fono-core/src/provider_catalog.rs`); pass any Gemini model via
//! `[stt.cloud].model` to override.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::lang::LanguageSelection;
use crate::traits::{SpeechToText, Transcription};

const BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
/// Default Gemini model for audio-understanding transcription.
const DEFAULT_MODEL: &str = "gemini-flash-lite-latest";
/// Stable key the language-cache layer uses for this backend.
pub(crate) const BACKEND_KEY: &str = "gemini";

/// Process-wide flag for the "rerun unavailable" warning. We log once per
/// binary instead of every transcription so a configured allow-list with
/// `cloud_rerun_on_language_mismatch = true` doesn't spam the log.
static RERUN_WARN_LOGGED: AtomicBool = AtomicBool::new(false);

pub struct GeminiStt {
    api_key: String,
    model: String,
    client: reqwest::Client,
    languages: Vec<String>,
    /// Captured from `general.cloud_rerun_on_language_mismatch`. Gemini
    /// can't honour it (no logprobs, no detected language), so the only
    /// effect is the one-shot warning in [`GeminiStt::transcribe`].
    cloud_rerun_on_mismatch: bool,
    prompts: std::collections::HashMap<String, String>,
}

impl GeminiStt {
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

    /// Builder: capture `general.cloud_rerun_on_language_mismatch`. Gemini
    /// has no per-segment confidence so the rerun itself is a no-op; we keep
    /// the setter for symmetry with [`crate::groq::GroqStt`] and to drive the
    /// one-shot warning.
    #[must_use]
    pub fn with_cloud_rerun_on_mismatch(mut self, on: bool) -> Self {
        self.cloud_rerun_on_mismatch = on;
        self
    }

    /// Builder: per-language prompt map. Used as a lightweight vocabulary
    /// hint appended to the transcribe instruction (Gemini has no dedicated
    /// `prompt` field). Keyed by language code; the forced/first language is
    /// consulted, falling back to an `*` wildcard entry.
    #[must_use]
    pub fn with_prompts(mut self, prompts: std::collections::HashMap<String, String>) -> Self {
        self.prompts = prompts;
        self
    }

    fn effective_selection(&self, lang_override: Option<&str>) -> LanguageSelection {
        LanguageSelection::from_config(&self.languages).with_override(lang_override)
    }

    fn endpoint(&self) -> String {
        format!("{BASE_URL}/models/{}:generateContent", self.model)
    }

    /// Build the transcribe-only instruction, optionally pinning the
    /// language and appending a vocabulary hint.
    fn instruction(forced_lang: Option<&str>, hint: Option<&str>) -> String {
        use std::fmt::Write as _;
        let mut s = String::from(
            "You are a transcription engine. Transcribe the attached audio verbatim. \
             Output only the exact words spoken, with no preamble, no commentary, no \
             quotation marks, and no explanation. If the audio is silent or unintelligible, \
             output nothing at all.",
        );
        if let Some(l) = forced_lang {
            let _ = write!(s, " The spoken language is {l}; transcribe in that language.");
        }
        if let Some(h) = hint.filter(|h| !h.is_empty()) {
            let _ = write!(s, " Context vocabulary that may appear: {h}");
        }
        s
    }
}

// ----- wire types --------------------------------------------------------

#[derive(Serialize)]
struct GenerateRequest<'a> {
    contents: Vec<Content<'a>>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig,
}

#[derive(Serialize)]
struct Content<'a> {
    role: &'a str,
    parts: Vec<Part<'a>>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Part<'a> {
    Text {
        text: &'a str,
    },
    Inline {
        #[serde(rename = "inlineData")]
        inline_data: InlineData,
    },
}

#[derive(Serialize)]
struct InlineData {
    #[serde(rename = "mimeType")]
    mime_type: &'static str,
    data: String,
}

#[derive(Serialize)]
struct GenerationConfig {
    temperature: f32,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct GenerateResponse {
    #[serde(default)]
    pub candidates: Vec<Candidate>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Candidate {
    #[serde(default)]
    pub content: Option<RespContent>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct RespContent {
    #[serde(default)]
    pub parts: Vec<RespPart>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct RespPart {
    #[serde(default)]
    pub text: Option<String>,
}

/// Concatenate the text of every part of the first candidate, trimmed.
/// Strips a single pair of wrapping quotes the model occasionally adds.
#[must_use]
pub fn extract_transcript(resp: &GenerateResponse) -> String {
    let raw: String = resp
        .candidates
        .first()
        .and_then(|c| c.content.as_ref())
        .map(|c| c.parts.iter().filter_map(|p| p.text.as_deref()).collect::<Vec<_>>().join(""))
        .unwrap_or_default();
    let trimmed = raw.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .filter(|inner| !inner.contains('"'))
        .unwrap_or(trimmed);
    unquoted.to_string()
}

#[async_trait]
impl SpeechToText for GeminiStt {
    async fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
        lang: Option<&str>,
    ) -> Result<Transcription> {
        let wav = crate::groq::encode_wav(pcm, sample_rate);
        let selection = self.effective_selection(lang);

        // Forced → pin the language in the prompt; auto / allow-list → leave
        // Gemini to detect (it returns no language code, so allow-list is
        // post-validated only by the one-shot warning below).
        let forced_lang: Option<String> = match &selection {
            LanguageSelection::Forced(c) => Some(c.clone()),
            LanguageSelection::Auto | LanguageSelection::AllowList(_) => None,
        };

        // Pick a vocabulary hint: the forced/first language's prompt, else a
        // `*` wildcard entry if present.
        let hint = forced_lang
            .as_deref()
            .and_then(|l| self.prompts.get(l))
            .or_else(|| self.prompts.get("*"))
            .map(String::as_str);

        let instruction = Self::instruction(forced_lang.as_deref(), hint);
        let req = GenerateRequest {
            contents: vec![Content {
                role: "user",
                parts: vec![
                    Part::Text { text: &instruction },
                    Part::Inline {
                        inline_data: InlineData {
                            mime_type: "audio/wav",
                            data: BASE64_STANDARD.encode(&wav),
                        },
                    },
                ],
            }],
            generation_config: GenerationConfig { temperature: 0.0 },
        };

        let res = self
            .client
            .post(self.endpoint())
            .header("x-goog-api-key", &self.api_key)
            .json(&req)
            .send()
            .await
            .context("gemini STT POST failed")?;
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("gemini STT returned {status}: {body}");
        }
        let parsed: GenerateResponse = serde_json::from_str(&body)
            .with_context(|| format!("parse gemini response: {body}"))?;

        // Gemini returns no detected language. When the user pinned an
        // allow-list and opted into rerun-on-mismatch, warn once per process
        // that the signal needed to honour it does not exist here.
        if let LanguageSelection::AllowList(_) = &selection {
            if self.cloud_rerun_on_mismatch && !RERUN_WARN_LOGGED.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    "gemini STT exposes no detected language or per-segment logprobs; \
                     rerun-on-mismatch (allow-list {:?}) is unavailable — accepting the \
                     transcript as returned. This warning is logged once.",
                    self.languages
                );
            }
        }

        let text = extract_transcript(&parsed);
        // We only know the language when the caller forced one.
        Ok(Transcription { text, language: forced_lang, duration_ms: None })
    }

    fn name(&self) -> &'static str {
        BACKEND_KEY
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
            .context("gemini prewarm")?;
        let _ = res.bytes().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_matches_catalogue() {
        assert_eq!(DEFAULT_MODEL, crate::defaults::default_cloud_model("gemini"));
    }

    #[test]
    fn extracts_transcript_from_candidate_parts() {
        let body = r#"{
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "hello "}, {"text": "world"}]},
                "finishReason": "STOP"
            }]
        }"#;
        let parsed: GenerateResponse = serde_json::from_str(body).expect("parse");
        assert_eq!(extract_transcript(&parsed), "hello world");
    }

    #[test]
    fn extract_strips_wrapping_quotes_and_whitespace() {
        let body = r#"{"candidates":[{"content":{"parts":[{"text":"  \"salut\"  "}]}}]}"#;
        let parsed: GenerateResponse = serde_json::from_str(body).expect("parse");
        assert_eq!(extract_transcript(&parsed), "salut");
    }

    #[test]
    fn extract_keeps_inner_quotes() {
        // A transcript that legitimately contains quotes must not be mangled.
        let body = r#"{"candidates":[{"content":{"parts":[{"text":"he said \"hi\" loudly"}]}}]}"#;
        let parsed: GenerateResponse = serde_json::from_str(body).expect("parse");
        assert_eq!(extract_transcript(&parsed), "he said \"hi\" loudly");
    }

    #[test]
    fn extract_empty_when_no_candidates() {
        let parsed: GenerateResponse = serde_json::from_str(r#"{"candidates":[]}"#).expect("parse");
        assert_eq!(extract_transcript(&parsed), "");
    }

    #[test]
    fn request_serializes_inline_audio_and_instruction() {
        let instruction = GeminiStt::instruction(Some("ro"), Some("Fono, NimbleX"));
        let req = GenerateRequest {
            contents: vec![Content {
                role: "user",
                parts: vec![
                    Part::Text { text: &instruction },
                    Part::Inline {
                        inline_data: InlineData { mime_type: "audio/wav", data: "AAA=".into() },
                    },
                ],
            }],
            generation_config: GenerationConfig { temperature: 0.0 },
        };
        let json = serde_json::to_string(&req).expect("serialize");
        assert!(json.contains("\"inlineData\""), "{json}");
        assert!(json.contains("\"mimeType\":\"audio/wav\""), "{json}");
        assert!(json.contains("ro"), "language hint missing: {json}");
        assert!(json.contains("NimbleX"), "vocab hint missing: {json}");
    }

    #[test]
    fn builder_captures_languages_and_prompts() {
        let mut prompts = std::collections::HashMap::new();
        prompts.insert("en".to_string(), "Professional dictation.".to_string());
        let stt = GeminiStt::new("k")
            .with_languages(vec!["en".into(), "ro".into()])
            .with_prompts(prompts)
            .with_cloud_rerun_on_mismatch(true);
        assert_eq!(stt.languages, vec!["en", "ro"]);
        assert_eq!(stt.prompts.get("en").map(String::as_str), Some("Professional dictation."));
        assert!(stt.cloud_rerun_on_mismatch);
    }

    #[test]
    fn name_is_stable_label() {
        assert_eq!(GeminiStt::new("k").name(), "gemini");
    }
}
