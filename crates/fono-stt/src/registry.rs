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
//! `realtime_factor_cpu_avx2` is the audio-seconds processed per
//! wall-second on an 8-physical-core / 8-thread AVX2 CPU running
//! whisper.cpp in batch mode at default settings. A value of 6.0
//! means the model processes 6 seconds of audio per wall-second.
//! Live (streaming) mode adds 2–4× compute amplification on top,
//! which is why the live-mode threshold in
//! `HardwareSnapshot::affords_model` is much higher than 1×
//! realtime. Apple-Silicon machines (which whisper.cpp accelerates
//! via Metal/CoreML) get a separate, lower threshold.
//!
//! As of 2026-05-15 the numbers in this table are anchored to the
//! empirical batch RTF measured on `ultra7-258v` (Intel Core Ultra 7
//! 258V, 8p/8l Lunar Lake, AVX2 + FMA, no SMT) at
//! `docs/bench/calibration/summary/matrix.json`. That host is the
//! closest match in the calibration matrix to the "generic 8-core
//! AVX2" reference. Numbers are rounded down (conservative) so users
//! with weaker AVX2 hosts still see correct verdicts.

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
        // Empirical peak RSS on ultra7-258v was 420 MiB; +20% headroom = 500.
        min_ram_mb: 500,
        // Empirical batch RTF on ultra7-258v: 20.49; rounded down.
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
    },
    ModelInfo {
        name: "tiny.en",
        multilingual: false,
        approx_mb: 75,
        // Peak RSS 414 MiB on ultra7-258v (English-only is identical
        // architecture, slightly lower KV-cache from monolingual head).
        min_ram_mb: 500,
        // Empirical batch RTF on ultra7-258v: 26.81. Held to 20.0 to
        // match the multilingual sibling and stay conservative on
        // older hosts with smaller branch predictors.
        realtime_factor_cpu_avx2: 20.0,
        wer_by_lang: &[("en", 9.0)],
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
        sha256: UNPINNED,
    },
    // ── base (142 MB) ───────────────────────────────────────────────────────
    ModelInfo {
        name: "base",
        multilingual: true,
        approx_mb: 142,
        // Empirical peak RSS on ultra7-258v was 585 MiB; +20% headroom = 700.
        min_ram_mb: 700,
        // Empirical batch RTF on ultra7-258v: 11.39; rounded down.
        realtime_factor_cpu_avx2: 11.0,
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
    },
    ModelInfo {
        name: "base.en",
        multilingual: false,
        approx_mb: 142,
        // Peak RSS 591 MiB on ultra7-258v.
        min_ram_mb: 700,
        // Empirical batch RTF on ultra7-258v: 13.20. Held to 11.0 to
        // match the multilingual sibling and stay conservative.
        realtime_factor_cpu_avx2: 11.0,
        wer_by_lang: &[("en", 7.0)],
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
        sha256: UNPINNED,
    },
    // ── small (466 MB) ──────────────────────────────────────────────────────
    ModelInfo {
        name: "small",
        multilingual: true,
        approx_mb: 466,
        // Empirical peak RSS on ultra7-258v was 1360 MiB; +10% headroom = 1500.
        min_ram_mb: 1_500,
        // Empirical batch RTF on ultra7-258v: 3.13. The previous
        // value (4.0) was the 2024 estimate; measured numbers landed
        // ~25% lower.
        realtime_factor_cpu_avx2: 3.0,
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
    },
    ModelInfo {
        name: "small.en",
        multilingual: false,
        approx_mb: 466,
        // Peak RSS 1367 MiB on ultra7-258v.
        min_ram_mb: 1_500,
        // Empirical batch RTF on ultra7-258v: 3.90. Held to 3.0 to
        // match the multilingual sibling and stay conservative.
        realtime_factor_cpu_avx2: 3.0,
        wer_by_lang: &[("en", 5.0)],
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
        sha256: UNPINNED,
    },
    // ── large-v3-turbo (~1.6 GB) — replaces medium for the wizard ───────────
    // 4-decoder distilled large-v3: medium-ish quality at small-ish speed.
    // Multilingual only (no .en variant published).
    ModelInfo {
        name: "large-v3-turbo",
        multilingual: true,
        approx_mb: 1_620,
        // Empirical peak RSS across hosts: 3642 (ryzen-5950x), 3654
        // (ultra7-258v); +~10% headroom for KV-cache growth on long
        // segments = 4000.
        min_ram_mb: 4_000,
        // Empirical batch RTF on ultra7-258v (8-core AVX2 reference):
        // 0.61 — sub-realtime. The previous value (2.5) was off by 4×
        // and is the root cause of the wizard recommending turbo on
        // CPU-only laptops that cannot actually run it. Holding to
        // 0.6 (rounded down) puts turbo correctly in the Unsuitable
        // bucket on every CPU-only laptop class we measured.
        realtime_factor_cpu_avx2: 0.6,
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

    /// Pick the best local STT model for this hardware without any user
    /// input. Used on first-run startup so the daemon has a working
    /// config even when the user hasn't gone through the wizard.
    ///
    /// Selection rule: walk the wizard-visible multilingual models from
    /// largest to smallest and return the first that lands `Comfortable`.
    /// If none are Comfortable, return the largest `Borderline` instead.
    /// Falls back to `tiny` if even that is impossible.
    ///
    /// Multilingual is the safe default because the OOTB config has an
    /// empty languages list (auto-detect). An English-only model would
    /// silently mistranscribe non-English audio.
    #[must_use]
    pub fn pick_default_local(snap: &fono_core::HardwareSnapshot) -> &'static str {
        use fono_core::hwcheck::Affordability;
        // Largest → smallest so we prefer accuracy when the host allows.
        let candidates: Vec<&ModelInfo> = WHISPER_MODELS
            .iter()
            .filter(|m| m.multilingual)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let mut best_borderline: Option<&ModelInfo> = None;
        for m in &candidates {
            match snap.affords_model(m.min_ram_mb, m.approx_mb, m.realtime_factor_cpu_avx2) {
                Affordability::Comfortable => return m.name,
                Affordability::Borderline if best_borderline.is_none() => {
                    best_borderline = Some(m);
                }
                _ => {}
            }
        }
        best_borderline.map(|m| m.name).unwrap_or("tiny")
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
        // Within multilingual family: tiny > base > small > turbo.
        assert!(rf("tiny") > rf("base"));
        assert!(rf("base") > rf("small"));
        assert!(rf("small") > rf("large-v3-turbo"));
        // Within English-only family.
        assert!(rf("tiny.en") > rf("base.en"));
        assert!(rf("base.en") > rf("small.en"));
        // Turbo sub-realtime on the 8-core AVX2 reference is the
        // empirical truth as of 2026-05-15.
        assert!(rf("large-v3-turbo") < 1.0);
    }

    #[test]
    fn turbo_is_multilingual() {
        let m = ModelRegistry::get("large-v3-turbo").expect("turbo missing");
        assert!(m.multilingual);
    }

    fn fake_snap(cores: u32, ram_gb: u64, avx2: bool) -> fono_core::HardwareSnapshot {
        fono_core::HardwareSnapshot {
            physical_cores: cores,
            logical_cores: cores * 2,
            total_ram_bytes: ram_gb * 1024 * 1024 * 1024,
            available_ram_bytes: ram_gb * 1024 * 1024 * 1024,
            free_disk_bytes: 200 * 1024 * 1024 * 1024,
            cpu_features: fono_core::hwcheck::CpuFeatures {
                avx2,
                ..Default::default()
            },
            os: "linux".into(),
            arch: "x86_64".into(),
        }
    }

    #[test]
    fn pick_default_local_returns_multilingual() {
        // Any reasonable machine should resolve to a multilingual model.
        let snap = fake_snap(8, 16, true);
        let picked = ModelRegistry::pick_default_local(&snap);
        let info = ModelRegistry::get(picked).expect("picked model unknown");
        assert!(info.multilingual, "default must be multilingual; got {picked}");
    }

    #[test]
    fn pick_default_local_scales_to_hardware() {
        // 2-core ancient laptop: small Unsuitable (0.6 < 1.0 batch floor),
        // base rf=11 × 0.25 = 2.75 → Borderline. Picker returns the largest
        // Borderline since none are Comfortable: base.
        let weak = fake_snap(2, 8, true);
        assert_eq!(ModelRegistry::pick_default_local(&weak), "base");

        // 16-core desktop: small rf=3.0 × sqrt(2) ≈ 4.24 < 6.0 → still
        // Borderline. base rf=11 × sqrt(2) ≈ 15.6 → Comfortable. Picker
        // walks largest→smallest so returns base (first Comfortable).
        let strong = fake_snap(16, 32, true);
        assert_eq!(ModelRegistry::pick_default_local(&strong), "base");
    }
}
