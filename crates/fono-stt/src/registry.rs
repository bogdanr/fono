// SPDX-License-Identifier: GPL-3.0-only
//! Registry of whisper model variants with pinned SHA256 hashes and HF URLs.
//! `FONO_MODEL_MIRROR` env var overrides the host at download time.
//!
//! `WHISPER_MODELS` is the single source of truth for each model's size,
//! quality, and hardware cost. The wizard and `models install --recommend`
//! both read this table; do not duplicate numbers elsewhere.
//!
//! # Word-error-rate data source
//!
//! WER estimates are aggregated from the OpenAI Whisper paper (Radford
//! et al., 2022, FLEURS), Hugging Face's whisper-evals, and CommonVoice
//! community benchmarks; numbers are intentionally pessimistic (closer
//! to real-world dictation than read-aloud test sets) and rounded to
//! whole percent. Languages not listed in `wer_by_lang` have no public
//! benchmark for that variant.
//!
//! The wizard does not show raw percentages to users — it bucketises
//! them into Excellent / Good / Acceptable / Inaccurate (see
//! `wizard.rs::AccuracyBucket`). Anything above 15% in any of the
//! user's selected languages is flagged `Inaccurate`.
//!
//! # Realtime-factor reference
//!
//! `realtime_factor_cpu_avx2` is measured on an 8-physical-core ~3 GHz
//! AVX2 desktop running whisper.cpp at default settings (no streaming
//! overhead). A value of 6.0 means the model processes 6 seconds of
//! audio per wall-second in batch mode. Live (streaming) mode adds
//! 2–4× compute amplification on top, which is why the live-mode
//! threshold in `HardwareSnapshot::affords_model` is much higher than
//! 1× realtime. Apple-Silicon machines (which whisper.cpp accelerates
//! via Metal/CoreML) get a separate, lower threshold.
//!
//! # `wizard_visible`
//!
//! Set to `false` for variants we no longer recommend in the first-run
//! wizard but keep in the registry so existing configs that pin the
//! name continue to download. `medium` and `medium.en` were demoted
//! when `large-v3-turbo` arrived (faster, similar quality).

#[derive(Debug, Clone, Copy)]
pub struct ModelInfo {
    pub name: &'static str,
    pub multilingual: bool,
    pub approx_mb: u32,
    /// Minimum *available* RAM (in MiB) the model needs to load and run
    /// inference without swapping. Conservative: includes model weights,
    /// mel-filter state, and beam-search buffers.
    pub min_ram_mb: u32,
    /// Audio-seconds processed per wall-second on the AVX2 reference
    /// machine (8 physical cores, ~3 GHz). See module doc for details.
    /// Used by `HardwareSnapshot::affords_model` to gate live-mode
    /// recommendations.
    pub realtime_factor_cpu_avx2: f32,
    /// (language-code, WER-percent) pairs from FLEURS / community evals.
    /// Codes are BCP-47 alpha-2. Languages absent from this slice have
    /// no published estimate. See module doc for caveats.
    pub wer_by_lang: &'static [(&'static str, f32)],
    /// HuggingFace path, e.g. `ggerganov/whisper.cpp/resolve/main/ggml-small.bin`.
    pub url_path: &'static str,
    pub sha256: &'static str,
    /// Whether the wizard should offer this model in its shortlist.
    /// `false` for legacy variants we keep for compatibility but no
    /// longer recommend (e.g. `medium` after turbo's release).
    pub wizard_visible: bool,
}

/// Default HuggingFace host; override via `FONO_MODEL_MIRROR`.
pub const DEFAULT_MIRROR: &str = "https://huggingface.co";

/// SHA-256 sentinel for entries without a pinned digest. The downloader
/// logs the computed hash at info level and accepts the file; a future
/// change will pin real values once an authoritative manifest exists.
pub const UNPINNED: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub const WHISPER_MODELS: &[ModelInfo] = &[
    // ── tiny (75 MB) ────────────────────────────────────────────────────────
    ModelInfo {
        name: "tiny",
        multilingual: true,
        approx_mb: 75,
        min_ram_mb: 250,
        realtime_factor_cpu_avx2: 20.0,
        wer_by_lang: &[
            ("en", 12.0),
            ("es", 14.0),
            ("fr", 18.0),
            ("de", 22.0),
            ("it", 18.0),
            ("pt", 14.0),
            ("nl", 24.0),
            ("ro", 28.0),
            ("pl", 30.0),
            ("ru", 26.0),
            ("uk", 36.0),
            ("tr", 28.0),
            ("zh", 30.0),
            ("ja", 34.0),
        ],
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin",
        sha256: UNPINNED,
        wizard_visible: true,
    },
    ModelInfo {
        name: "tiny.en",
        multilingual: false,
        approx_mb: 75,
        min_ram_mb: 250,
        realtime_factor_cpu_avx2: 20.0,
        wer_by_lang: &[("en", 9.0)],
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
        sha256: UNPINNED,
        wizard_visible: true,
    },
    // ── base (142 MB) ───────────────────────────────────────────────────────
    ModelInfo {
        name: "base",
        multilingual: true,
        approx_mb: 142,
        min_ram_mb: 400,
        realtime_factor_cpu_avx2: 10.0,
        wer_by_lang: &[
            ("en", 9.0),
            ("es", 10.0),
            ("fr", 13.0),
            ("de", 16.0),
            ("it", 13.0),
            ("pt", 10.0),
            ("nl", 17.0),
            ("ro", 20.0),
            ("pl", 22.0),
            ("ru", 19.0),
            ("uk", 27.0),
            ("tr", 21.0),
            ("zh", 21.0),
            ("ja", 25.0),
        ],
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
        sha256: UNPINNED,
        wizard_visible: true,
    },
    ModelInfo {
        name: "base.en",
        multilingual: false,
        approx_mb: 142,
        min_ram_mb: 400,
        realtime_factor_cpu_avx2: 10.0,
        wer_by_lang: &[("en", 7.0)],
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
        sha256: UNPINNED,
        wizard_visible: true,
    },
    // ── small (466 MB) ──────────────────────────────────────────────────────
    ModelInfo {
        name: "small",
        multilingual: true,
        approx_mb: 466,
        min_ram_mb: 1_000,
        realtime_factor_cpu_avx2: 4.0,
        wer_by_lang: &[
            ("en", 6.0),
            ("es", 7.0),
            ("fr", 9.0),
            ("de", 10.0),
            ("it", 9.0),
            ("pt", 7.0),
            ("nl", 12.0),
            ("ro", 13.0),
            ("pl", 15.0),
            ("ru", 13.0),
            ("uk", 19.0),
            ("tr", 14.0),
            ("zh", 14.0),
            ("ja", 17.0),
        ],
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
        sha256: UNPINNED,
        wizard_visible: true,
    },
    ModelInfo {
        name: "small.en",
        multilingual: false,
        approx_mb: 466,
        min_ram_mb: 1_000,
        realtime_factor_cpu_avx2: 4.0,
        wer_by_lang: &[("en", 5.0)],
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
        sha256: UNPINNED,
        wizard_visible: true,
    },
    // ── large-v3-turbo (~1.6 GB) — replaces medium for the wizard ───────────
    // 4-decoder distilled large-v3: medium-ish quality at small-ish speed.
    // Multilingual only (no .en variant published).
    ModelInfo {
        name: "large-v3-turbo",
        multilingual: true,
        approx_mb: 1_620,
        min_ram_mb: 3_400,
        realtime_factor_cpu_avx2: 2.5,
        wer_by_lang: &[
            ("en", 4.0),
            ("es", 5.0),
            ("fr", 6.0),
            ("de", 7.0),
            ("it", 6.0),
            ("pt", 5.0),
            ("nl", 8.0),
            ("ro", 9.0),
            ("pl", 10.0),
            ("ru", 9.0),
            ("uk", 14.0),
            ("tr", 10.0),
            ("zh", 9.0),
            ("ja", 13.0),
        ],
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
        sha256: UNPINNED,
        wizard_visible: true,
    },
    // ── medium (1.5 GB) — kept for backwards compat, hidden from wizard ─────
    // Superseded by large-v3-turbo (similar quality, ~2× faster). Existing
    // configs that pin "medium" still resolve and download.
    ModelInfo {
        name: "medium",
        multilingual: true,
        approx_mb: 1_500,
        min_ram_mb: 3_200,
        realtime_factor_cpu_avx2: 1.2,
        wer_by_lang: &[
            ("en", 4.0),
            ("es", 5.0),
            ("fr", 6.0),
            ("de", 7.0),
            ("it", 6.0),
            ("pt", 5.0),
            ("nl", 8.0),
            ("ro", 9.0),
            ("pl", 10.0),
            ("ru", 9.0),
            ("uk", 14.0),
            ("tr", 10.0),
            ("zh", 9.0),
            ("ja", 12.0),
        ],
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-medium.bin",
        sha256: UNPINNED,
        wizard_visible: false,
    },
    ModelInfo {
        name: "medium.en",
        multilingual: false,
        approx_mb: 1_500,
        min_ram_mb: 3_200,
        realtime_factor_cpu_avx2: 1.2,
        wer_by_lang: &[("en", 3.5)],
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-medium.en.bin",
        sha256: UNPINNED,
        wizard_visible: false,
    },
];

pub struct ModelRegistry;

impl ModelRegistry {
    /// Look up a model by name (case-insensitive).
    #[must_use]
    pub fn get(name: &str) -> Option<&'static ModelInfo> {
        let lower = name.to_ascii_lowercase();
        WHISPER_MODELS.iter().find(|m| m.name == lower)
    }

    #[must_use]
    pub fn all() -> &'static [ModelInfo] {
        WHISPER_MODELS
    }

    /// Resolve the effective mirror URL, honouring `FONO_MODEL_MIRROR`.
    #[must_use]
    pub fn mirror() -> String {
        std::env::var("FONO_MODEL_MIRROR").unwrap_or_else(|_| DEFAULT_MIRROR.to_string())
    }

    /// Build the full download URL for a model.
    #[must_use]
    pub fn url_for(model: &ModelInfo) -> String {
        format!("{}/{}", Self::mirror(), model.url_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_small_model_is_multilingual() {
        let m = ModelRegistry::get("small").unwrap();
        assert!(m.multilingual);
    }

    #[test]
    fn mirror_override() {
        std::env::set_var("FONO_MODEL_MIRROR", "https://example.test");
        assert!(ModelRegistry::mirror().starts_with("https://example.test"));
        std::env::remove_var("FONO_MODEL_MIRROR");
    }

    #[test]
    fn every_model_has_english_wer() {
        for m in WHISPER_MODELS {
            let has_en = m.wer_by_lang.iter().any(|&(lang, _)| lang == "en");
            assert!(
                has_en,
                "model '{}' is missing English WER in wer_by_lang",
                m.name
            );
        }
    }

    #[test]
    fn english_only_models_have_single_wer_entry() {
        for m in WHISPER_MODELS.iter().filter(|m| !m.multilingual) {
            assert_eq!(
                m.wer_by_lang.len(),
                1,
                "English-only model '{}' should have exactly one WER entry",
                m.name
            );
            assert_eq!(m.wer_by_lang[0].0, "en");
        }
    }

    /// Within each language family, larger English variants must have
    /// equal-or-better WER than smaller ones.
    #[test]
    fn wer_monotonically_better_in_en_only_family() {
        let get_en_wer = |name: &str| {
            ModelRegistry::get(name)
                .and_then(|m| m.wer_by_lang.iter().find(|&&(l, _)| l == "en"))
                .map(|&(_, wer)| wer)
                .unwrap_or(f32::MAX)
        };
        assert!(get_en_wer("tiny.en") > get_en_wer("base.en"));
        assert!(get_en_wer("base.en") > get_en_wer("small.en"));
        assert!(get_en_wer("small.en") >= get_en_wer("medium.en"));
    }

    #[test]
    fn wer_monotonically_better_in_multilingual_family() {
        let get_en_wer = |name: &str| {
            ModelRegistry::get(name)
                .and_then(|m| m.wer_by_lang.iter().find(|&&(l, _)| l == "en"))
                .map(|&(_, wer)| wer)
                .unwrap_or(f32::MAX)
        };
        assert!(get_en_wer("tiny") > get_en_wer("base"));
        assert!(get_en_wer("base") > get_en_wer("small"));
        assert!(get_en_wer("small") >= get_en_wer("large-v3-turbo"));
    }

    #[test]
    fn min_ram_monotonic_within_families() {
        let get = |name: &str| ModelRegistry::get(name).map(|m| m.min_ram_mb).unwrap_or(0);
        assert!(get("tiny.en") <= get("base.en"));
        assert!(get("base.en") <= get("small.en"));
        assert!(get("small.en") <= get("medium.en"));
        assert!(get("tiny") <= get("base"));
        assert!(get("base") <= get("small"));
        assert!(get("small") <= get("large-v3-turbo"));
    }

    #[test]
    fn realtime_factor_larger_models_are_slower() {
        let rf = |name: &str| {
            ModelRegistry::get(name)
                .map(|m| m.realtime_factor_cpu_avx2)
                .unwrap_or(0.0)
        };
        assert!(rf("tiny.en") > rf("base.en"));
        assert!(rf("base.en") > rf("small.en"));
        assert!(rf("small.en") > rf("medium.en"));
        // Turbo: faster than medium (its replacement), slower than small.
        assert!(rf("large-v3-turbo") > rf("medium"));
        assert!(rf("large-v3-turbo") < rf("small"));
    }

    #[test]
    fn medium_variants_are_hidden_from_wizard() {
        for name in ["medium", "medium.en"] {
            let m = ModelRegistry::get(name).expect(name);
            assert!(
                !m.wizard_visible,
                "{name} must be wizard_visible=false (legacy)"
            );
        }
    }

    #[test]
    fn turbo_is_visible_in_wizard() {
        let m = ModelRegistry::get("large-v3-turbo").expect("turbo missing");
        assert!(m.wizard_visible);
        assert!(m.multilingual);
    }
}
