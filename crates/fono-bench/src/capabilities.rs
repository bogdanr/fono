// SPDX-License-Identifier: GPL-3.0-only
//! Typed model-capability surface for the equivalence harness.
//!
//! Replaces the inline `args.stt == "local" && args.model.ends_with(".en")`
//! boolean previously embedded in `bin/fono-bench.rs` (Wave 2 Thread A,
//! `plans/2026-04-28-wave-2-close-out-v1.md`). Centralising the decision
//! lets `run_fixture` short-circuit before WAV reads on incompatible
//! fixtures, and gives the JSON report a stable place to record which
//! model produced each row.

use serde::{Deserialize, Serialize};

/// Static description of an STT backend's language coverage.
///
/// `english_only = true` means the model can only transcribe English; any
/// non-English fixture must be skipped before inference. `model_label`
/// is the human-readable identifier persisted into
/// `EquivalenceReport.model_capabilities`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub english_only: bool,
    pub model_label: String,
}

impl ModelCapabilities {
    /// Resolve capabilities for a local whisper.cpp model from its stem
    /// (the part after `ggml-` and before `.bin`).
    ///
    /// Whisper `*.en` models are English-only. Quantization suffixes
    /// like `-q5_1`, `-q4_0`, `-q8_0` and the `_n` variants are stripped
    /// before the `.en` check so that e.g. `tiny.en-q5_1` correctly
    /// classifies as English-only.
    #[must_use]
    pub fn for_local_whisper(model_stem: &str) -> Self {
        let normalised = strip_quantization_suffix(model_stem);
        Self {
            english_only: normalised.ends_with(".en"),
            model_label: format!("local:{model_stem}"),
        }
    }

    /// Resolve capabilities for a cloud STT provider/model pair.
    ///
    /// Every cloud SKU we wire up today is multilingual; the explicit
    /// match arms exist so a future English-only cloud model (or a
    /// previously-multilingual one with a regression) can be flipped
    /// without grepping for boolean literals.
    #[must_use]
    pub fn for_cloud(provider: &str, model: &str) -> Self {
        let english_only = match provider {
            "groq" | "openai" | "assemblyai" | "deepgram" | "azure" | "google"
            | "speechmatics" | "cartesia" | "nemotron" => false,
            other => {
                tracing::warn!(
                    provider = other,
                    model,
                    "unknown cloud STT provider; defaulting to multilingual"
                );
                false
            }
        };
        Self {
            english_only,
            model_label: format!("{provider}:{model}"),
        }
    }

    /// Decide whether a fixture demands a multilingual model.
    ///
    /// The default rule is "any non-English fixture requires multilingual";
    /// the `fixture_override` argument lets a manifest entry override
    /// that derivation explicitly (e.g. an English-language fixture that
    /// nevertheless contains code-switched non-English phrases).
    #[must_use]
    pub fn fixture_requires_multilingual(fx_lang: &str, fixture_override: Option<bool>) -> bool {
        if let Some(v) = fixture_override {
            return v;
        }
        fx_lang != "en"
    }
}

/// Strip a trailing `-q\d+(_\d+)?` quantization fragment from a whisper
/// model stem. Implemented with manual char scanning to avoid pulling
/// `regex` into `fono-bench` for one site.
fn strip_quantization_suffix(stem: &str) -> &str {
    // Look for the last `-q` substring; if everything after it parses
    // as `\d+(_\d+)?` we treat it as a quantization suffix.
    let Some(idx) = stem.rfind("-q") else {
        return stem;
    };
    let tail = &stem[idx + 2..];
    if tail.is_empty() {
        return stem;
    }
    let mut chars = tail.chars();
    // First run of digits.
    let mut saw_digit = false;
    let mut peek;
    loop {
        peek = chars.clone().next();
        match peek {
            Some(c) if c.is_ascii_digit() => {
                saw_digit = true;
                chars.next();
            }
            _ => break,
        }
    }
    if !saw_digit {
        return stem;
    }
    // Optional `_<digits>` group.
    if matches!(peek, Some('_')) {
        chars.next();
        let mut saw_inner_digit = false;
        loop {
            match chars.clone().next() {
                Some(c) if c.is_ascii_digit() => {
                    saw_inner_digit = true;
                    chars.next();
                }
                _ => break,
            }
        }
        if !saw_inner_digit {
            return stem;
        }
    }
    if chars.next().is_some() {
        // Trailing junk after the suffix → not a quantization tag.
        return stem;
    }
    &stem[..idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_only_models_classify_correctly() {
        for stem in ["tiny.en", "small.en", "medium.en", "base.en"] {
            let caps = ModelCapabilities::for_local_whisper(stem);
            assert!(caps.english_only, "{stem} should be english_only");
            assert_eq!(caps.model_label, format!("local:{stem}"));
        }
    }

    #[test]
    fn multilingual_models_classify_correctly() {
        for stem in [
            "tiny",
            "base",
            "small",
            "medium",
            "large-v3",
            "large-v3-turbo",
        ] {
            let caps = ModelCapabilities::for_local_whisper(stem);
            assert!(!caps.english_only, "{stem} should be multilingual");
        }
    }

    #[test]
    fn quantization_suffix_normalised_before_en_check() {
        assert!(ModelCapabilities::for_local_whisper("tiny.en-q5_1").english_only);
        assert!(ModelCapabilities::for_local_whisper("tiny.en-q4_0").english_only);
        assert!(ModelCapabilities::for_local_whisper("small.en-q8_0").english_only);
        assert!(!ModelCapabilities::for_local_whisper("tiny-q4_0").english_only);
        assert!(!ModelCapabilities::for_local_whisper("large-v3-q5_0").english_only);
        // Label preserves the original stem (suffix and all) so the
        // report can identify the precise file used.
        assert_eq!(
            ModelCapabilities::for_local_whisper("tiny.en-q5_1").model_label,
            "local:tiny.en-q5_1"
        );
    }

    #[test]
    fn quantization_strip_rejects_non_digit_tails() {
        // `-quack` is not a quantization tag.
        assert_eq!(strip_quantization_suffix("tiny-quack"), "tiny-quack");
        // Bare `-q` with no digits.
        assert_eq!(strip_quantization_suffix("tiny-q"), "tiny-q");
        // Trailing junk after the digit run.
        assert_eq!(strip_quantization_suffix("tiny-q5x"), "tiny-q5x");
    }

    #[test]
    fn cloud_providers_default_multilingual() {
        for p in [
            "groq",
            "openai",
            "assemblyai",
            "deepgram",
            "azure",
            "google",
            "speechmatics",
            "cartesia",
            "nemotron",
        ] {
            let caps = ModelCapabilities::for_cloud(p, "x");
            assert!(!caps.english_only, "{p} should default multilingual");
            assert_eq!(caps.model_label, format!("{p}:x"));
        }
    }

    #[test]
    fn unknown_cloud_provider_defaults_multilingual() {
        // Warns via tracing — we don't assert the warning here to keep
        // the test simple; the boolean is the contract.
        let caps = ModelCapabilities::for_cloud("future-en-only", "x");
        assert!(!caps.english_only);
    }

    #[test]
    fn fixture_requires_multilingual_default_and_override() {
        assert!(!ModelCapabilities::fixture_requires_multilingual("en", None));
        assert!(ModelCapabilities::fixture_requires_multilingual("ro", None));
        assert!(ModelCapabilities::fixture_requires_multilingual(
            "en",
            Some(true)
        ));
        assert!(!ModelCapabilities::fixture_requires_multilingual(
            "ro",
            Some(false)
        ));
    }
}
