// SPDX-License-Identifier: GPL-3.0-only
//! Registry of whisper model variants with pinned SHA256 hashes and HF URLs.
//! `FONO_MODEL_MIRROR` env var overrides the host at download time.
//!
//! `WHISPER_MODELS` is the single source of truth for each model's size,
//! quality, and hardware cost. The wizard and `models install --recommend`
//! both read this table; do not duplicate numbers elsewhere.
//!
//! # Quantization ladder
//!
//! Each user-facing model name (`tiny`, `tiny.en`, `small`, `small.en`,
//! `large-v3-turbo`) carries one **default** quantization plus zero or
//! more **opt-in** alternatives reachable via
//! `[stt.local].quantization = "fp16" | "q8_0" | "q5_1"`. The default
//! is the smallest quantization that passes both gates of the
//! acceptance rule documented in
//! `docs/decisions/0026-stt-quantization-ladder.md`:
//!
//! - mean per-fixture Levenshtein-accuracy Δ ≤ +0.05 vs fp16
//! - max per-fixture Δ ≤ +0.20 vs fp16
//!
//! Models that fail the rule at every quantization keep fp16 as their
//! default. Models where no quantization improves on fp16 (notably
//! `base` and `base.en`) have been dropped from the registry entirely —
//! they are dominated on the speed/quality frontier by `small-q5_1` and
//! `small.en-q8_0` respectively.
//!
//! # Word-error-rate data source
//!
//! WER estimates are aggregated from the OpenAI Whisper paper (Radford
//! et al., 2022, FLEURS), Hugging Face's whisper-evals, and CommonVoice
//! community benchmarks; numbers are intentionally pessimistic (closer
//! to real-world dictation than read-aloud test sets) and rounded to
//! whole percent. Languages not listed in `wer_by_lang` have no public
//! benchmark for that variant. Values reflect the default quantization
//! shipped for each name — the perf-pass equivalence runs (2026-05-19)
//! confirmed that quantization-induced WER drift stays inside ±2 pp on
//! the variants we ship, so a single per-name WER table is sufficient.
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
//! As of 2026-05-19 the numbers in this table are anchored to the
//! empirical batch RTF measured on `ultra7-258v` (Intel Core Ultra 7
//! 258V, 8p/8l Lunar Lake, AVX2 + FMA, no SMT) at
//! `docs/bench/2026-05-19-perf-pass/summary/matrix.json`. That host is
//! the closest match in the calibration matrix to the "generic 8-core
//! AVX2" reference. Numbers are rounded down (conservative) so users
//! with weaker AVX2 hosts still see correct verdicts.

use std::fmt;

/// Quantization level of a GGML whisper weight file.
///
/// Maps to the upstream `ggerganov/whisper.cpp` GGML file-name suffixes:
/// `Fp16` → no suffix (`ggml-small.bin`), `Q5_1` → `-q5_1`, `Q8_0` → `-q8_0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Quantization {
    /// Half-precision weights, no quantization. Largest, slowest, max quality.
    Fp16,
    /// 5-bit asymmetric block quantization. Smallest, fastest, slight quality
    /// drop on smaller models — safe on `tiny`, `tiny.en`, `small`.
    Q5_1,
    /// 8-bit block quantization. Near-lossless quality, ~50% smaller than fp16.
    /// Default for `small.en` and `large-v3-turbo`.
    Q8_0,
}

impl Quantization {
    /// Short token used in config files, CLI flags, and bench rows.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fp16 => "fp16",
            Self::Q5_1 => "q5_1",
            Self::Q8_0 => "q8_0",
        }
    }

    /// GGML file-name suffix. `Fp16` has no suffix; `Q5_1` → `-q5_1`,
    /// `Q8_0` → `-q8_0`.
    #[must_use]
    pub fn file_suffix(self) -> &'static str {
        match self {
            Self::Fp16 => "",
            Self::Q5_1 => "-q5_1",
            Self::Q8_0 => "-q8_0",
        }
    }

    /// Parse from a config token. Accepts `fp16`, `q5_1`, `q8_0`
    /// (case-insensitive). Returns `None` for unknown values; the
    /// caller is responsible for surfacing a friendly error.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "fp16" | "f16" => Some(Self::Fp16),
            "q5_1" | "q5-1" => Some(Self::Q5_1),
            "q8_0" | "q8-0" => Some(Self::Q8_0),
            _ => None,
        }
    }
}

impl fmt::Display for Quantization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// User-level preference for `[stt.local].quantization`. Resolves to a
/// concrete [`Quantization`] via [`ModelRegistry::resolve_quantization`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantizationPref {
    /// Use the model's registry default. The wizard and 99% of users
    /// stay here.
    Auto,
    /// Pin to a specific quantization. Fails fast at factory build
    /// time if the chosen `(name, quantization)` is not in the registry.
    Pinned(Quantization),
}

impl QuantizationPref {
    /// Parse from a config token. `auto` (or empty) → `Auto`; otherwise
    /// defer to [`Quantization::parse`]. Unknown tokens return `None`
    /// so the caller can produce a contextual error.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
            return Some(Self::Auto);
        }
        Quantization::parse(trimmed).map(Self::Pinned)
    }
}

/// One concrete `(model name, quantization)` pair available in the registry.
#[derive(Debug, Clone, Copy)]
pub struct QuantVariant {
    pub quantization: Quantization,
    /// File size in MiB; used for the wizard's `~466 MB` labels and as
    /// an input to `HardwareSnapshot::affords_model` so the verdict
    /// follows whichever quantization the user picks.
    pub approx_mb: u32,
    /// SHA-256 of the GGML file. [`UNPINNED`] means "log the computed
    /// hash and accept anything"; tighten once a manifest exists.
    pub sha256: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct ModelInfo {
    pub name: &'static str,
    pub multilingual: bool,
    /// File size of the **default** quantization (matches
    /// `quantizations[0].approx_mb`). Kept as a top-level field because
    /// the wizard's affordability + display labels operate on the
    /// default; opt-in alternatives are looked up through
    /// `quantizations`.
    pub approx_mb: u32,
    /// Minimum *available* RAM (in MiB) the model needs to load and
    /// run inference without swapping. Sized for the default
    /// quantization plus mel-filter state and beam-search buffers.
    pub min_ram_mb: u32,
    /// Audio-seconds processed per wall-second on the AVX2 reference
    /// machine (8 physical cores, ~3 GHz) at the default quantization.
    /// See module doc for details. Used by
    /// `HardwareSnapshot::affords_model` to gate live-mode
    /// recommendations.
    pub realtime_factor_cpu_avx2: f32,
    /// (language-code, WER-percent) pairs from FLEURS / community
    /// evals + this repo's perf-pass equivalence runs. Codes are
    /// BCP-47 alpha-2. Languages absent from this slice have no
    /// published estimate for this name.
    pub wer_by_lang: &'static [(&'static str, f32)],
    /// HuggingFace mirror directory containing the GGML files (without
    /// the file name). The full URL is constructed by
    /// [`ModelRegistry::url_for`].
    pub url_dir: &'static str,
    /// The quantization shipped when the user has
    /// `[stt.local].quantization = "auto"`. Must appear in `quantizations`.
    pub default_quantization: Quantization,
    /// All quantizations the registry knows how to download for this
    /// name. The default is one of these.
    pub quantizations: &'static [QuantVariant],
}

/// Default HuggingFace host; override via `FONO_MODEL_MIRROR`.
pub const DEFAULT_MIRROR: &str = "https://huggingface.co";

/// Canonical upstream directory for whisper.cpp GGML files.
const GGERGANOV_DIR: &str = "ggerganov/whisper.cpp/resolve/main";

/// SHA-256 sentinel for entries without a pinned digest. The downloader
/// logs the computed hash at info level and accepts the file; a future
/// change will pin real values once an authoritative manifest exists.
pub const UNPINNED: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub const WHISPER_MODELS: &[ModelInfo] = &[
    // ── tiny — multilingual ─────────────────────────────────────────────
    // Default: q5_1 (31 MB). The 2026-05-19 perf-pass run on
    // `ultra7-258v` Vulkan confirmed mean Δacc +0.025 and max +0.088 vs
    // fp16 on English fixtures — well inside the acceptance gate. Note
    // that `tiny` multilingual has a hard quality floor on non-Latin
    // languages (Romanian Levenshtein ~0.25, Chinese ~0.50): the
    // `wer_by_lang` table reflects this and the wizard's
    // `AccuracyBucket::Inaccurate` filter routes affected users to a
    // larger model or cloud STT.
    ModelInfo {
        name: "tiny",
        multilingual: true,
        approx_mb: 42,
        // Peak RSS 311 MiB (q5_1) on ultra7-258v Vulkan; +50% headroom
        // for KV-cache growth on long segments and CPU-lane padding.
        min_ram_mb: 1_024,
        // Empirical batch RTF on ultra7-258v CPU q8_0: 40.x.
        realtime_factor_cpu_avx2: 40.0,
        wer_by_lang: &[
            ("en", 16.0),
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
        url_dir: GGERGANOV_DIR,
        // q8_0 default per ADR 0027 acceptance rule (2026-05-25 amendment):
        // universal q8_0 selected for consistency across the registry;
        // q5_1 reachable via `[stt.local].quantization = "q5_1"`.
        default_quantization: Quantization::Q8_0,
        quantizations: &[
            QuantVariant { quantization: Quantization::Q5_1, approx_mb: 31, sha256: UNPINNED },
            QuantVariant { quantization: Quantization::Q8_0, approx_mb: 42, sha256: UNPINNED },
        ],
    },
    // ── tiny.en — English-only ──────────────────────────────────────────
    // Default: q8_0 (42 MB) per ADR 0027 acceptance rule — universal q8_0
    // selected for consistency; q5_1 reachable via override.
    ModelInfo {
        name: "tiny.en",
        multilingual: false,
        approx_mb: 42,
        // Peak RSS 309 MiB (q5_1) on ultra7-258v Vulkan.
        min_ram_mb: 1_024,
        // Empirical batch RTF on ultra7-258v CPU q8_0: 52.x (matrix.md:39,
        // rounded down).
        realtime_factor_cpu_avx2: 52.0,
        wer_by_lang: &[("en", 13.0)],
        url_dir: GGERGANOV_DIR,
        default_quantization: Quantization::Q8_0,
        quantizations: &[
            QuantVariant { quantization: Quantization::Q5_1, approx_mb: 31, sha256: UNPINNED },
            QuantVariant { quantization: Quantization::Q8_0, approx_mb: 42, sha256: UNPINNED },
        ],
    },
    // ── small — multilingual ────────────────────────────────────────────
    // Default: q8_0 (253 MB) per ADR 0027 acceptance rule — universal
    // q8_0 selected for consistency; q5_1 and fp16 reachable via
    // `[stt.local].quantization`. q5_1 matched or beat fp16 on every
    // English fixture but q8_0 is the safer middle ground (Lunar Lake
    // CPU keeps RTF ≥ 2.0 at q8_0, and accuracy stays inside the
    // ADR 0027 +0.05 / +0.20 gate at every fixture).
    ModelInfo {
        name: "small",
        multilingual: true,
        approx_mb: 253,
        // Peak RSS 940 MiB (q8_0) on ultra7-258v Vulkan.
        min_ram_mb: 1_536,
        // Empirical batch RTF on ultra7-258v CPU q8_0: 8.68 (matrix.md:233).
        realtime_factor_cpu_avx2: 8.7,
        wer_by_lang: &[
            ("en", 10.0),
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
        url_dir: GGERGANOV_DIR,
        default_quantization: Quantization::Q8_0,
        quantizations: &[
            QuantVariant { quantization: Quantization::Q5_1, approx_mb: 182, sha256: UNPINNED },
            QuantVariant { quantization: Quantization::Q8_0, approx_mb: 253, sha256: UNPINNED },
            QuantVariant { quantization: Quantization::Fp16, approx_mb: 466, sha256: UNPINNED },
        ],
    },
    // ── small.en — English-only ─────────────────────────────────────────
    // Default: q8_0 (253 MB). q5_1 has max Δacc +0.219 on one fixture
    // (just outside the +0.20 gate); q8_0 stays inside the gate at
    // every fixture. Both q5_1 and fp16 are reachable as overrides.
    ModelInfo {
        name: "small.en",
        multilingual: false,
        approx_mb: 253,
        // Peak RSS ~875 MiB (q8_0) on ultra7-258v Vulkan.
        min_ram_mb: 1_536,
        // Empirical batch RTF on ultra7-258v CPU q8_0: 7.15
        // (`docs/bench/calibration/matrix.md:235`).
        realtime_factor_cpu_avx2: 7.15,
        wer_by_lang: &[("en", 9.0)],
        url_dir: GGERGANOV_DIR,
        default_quantization: Quantization::Q8_0,
        quantizations: &[
            QuantVariant { quantization: Quantization::Q8_0, approx_mb: 253, sha256: UNPINNED },
            QuantVariant { quantization: Quantization::Q5_1, approx_mb: 182, sha256: UNPINNED },
            QuantVariant { quantization: Quantization::Fp16, approx_mb: 466, sha256: UNPINNED },
        ],
    },
    // ── large-v3-turbo — multilingual, both modes ───────────────────────
    // Default: q8_0 (834 MB). q8_0 is acc-neutral vs fp16
    // (Δ +0.008 mean, max +0.001) and unlocks Lunar Lake CPU as a
    // viable host (RTF 2.31 vs 0.62 fp16). q5_0 was measured to break
    // `en-conversational` (acc 0.354 vs 0.046 fp16) and is excluded.
    // q5_1 is not currently published upstream — tracked on the
    // roadmap as a self-quantization research item.
    ModelInfo {
        name: "large-v3-turbo",
        multilingual: true,
        approx_mb: 834,
        // Peak RSS 2.24 GiB (q8_0) on CPU, 267 MiB on Vulkan. Conservative
        // for CPU lane; Vulkan headroom is irrelevant here.
        min_ram_mb: 3_072,
        // Empirical batch RTF on ultra7-258v CPU q8_0: 2.31.
        realtime_factor_cpu_avx2: 2.3,
        wer_by_lang: &[
            ("en", 8.0),
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
        url_dir: GGERGANOV_DIR,
        default_quantization: Quantization::Q8_0,
        quantizations: &[
            QuantVariant { quantization: Quantization::Q8_0, approx_mb: 834, sha256: UNPINNED },
            QuantVariant { quantization: Quantization::Fp16, approx_mb: 1_620, sha256: UNPINNED },
        ],
    },
];

pub struct ModelRegistry;

impl ModelRegistry {
    /// Look up a model by name (case-insensitive). Returns `None` for
    /// unknown names — callers should produce a friendly error pointing
    /// users at `fono models list`.
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

    /// Build the full download URL for a specific `(model, quantization)`.
    /// Returns `None` if the registry doesn't carry that variant.
    #[must_use]
    pub fn url_for(model: &ModelInfo, quant: Quantization) -> Option<String> {
        let _ = model.quantizations.iter().find(|v| v.quantization == quant)?;
        Some(format!("{}/{}/{}", Self::mirror(), model.url_dir, Self::filename(model.name, quant)))
    }

    /// GGML file basename for `(name, quantization)`. Does not check the
    /// registry — this is a pure naming function.
    #[must_use]
    pub fn filename(name: &str, quant: Quantization) -> String {
        format!("ggml-{name}{}.bin", quant.file_suffix())
    }

    /// Look up the [`QuantVariant`] row for a `(model, quantization)`
    /// pair. Returns `None` if the variant is not in the registry — e.g.
    /// `tiny` + `Fp16` (we ship only `tiny-q5_1`).
    #[must_use]
    pub fn variant_for(model: &ModelInfo, quant: Quantization) -> Option<&'static QuantVariant> {
        model.quantizations.iter().find(|v| v.quantization == quant)
    }

    /// Resolve a user `QuantizationPref` against a model's available
    /// quantizations. `Auto` returns the model's default; `Pinned(q)`
    /// returns `q` only if it's in `model.quantizations`, otherwise
    /// errors with a list of supported alternatives.
    pub fn resolve_quantization(
        model: &ModelInfo,
        pref: QuantizationPref,
    ) -> Result<Quantization, String> {
        match pref {
            QuantizationPref::Auto => Ok(model.default_quantization),
            QuantizationPref::Pinned(q) => {
                if model.quantizations.iter().any(|v| v.quantization == q) {
                    Ok(q)
                } else {
                    let supported: Vec<&'static str> =
                        model.quantizations.iter().map(|v| v.quantization.as_str()).collect();
                    Err(format!(
                        "model {:?} does not ship the {:?} quantization; supported: {}",
                        model.name,
                        q.as_str(),
                        supported.join(", ")
                    ))
                }
            }
        }
    }

    /// Pick the best local STT model for this hardware without any user
    /// input. Used on first-run startup so the daemon has a working
    /// config even when the user hasn't gone through the wizard.
    ///
    /// Selection rule: walk the multilingual models from largest to
    /// smallest and return the first that `affords_model` accepts.
    /// Falls back to `tiny` if even that is impossible.
    ///
    /// Multilingual is the safe default because the OOTB config has an
    /// empty languages list (auto-detect). An English-only model would
    /// silently mistranscribe non-English audio.
    #[must_use]
    pub fn pick_default_local(snap: &fono_core::HardwareSnapshot) -> &'static str {
        for m in WHISPER_MODELS.iter().filter(|m| m.multilingual).rev() {
            if snap.affords_model(m.min_ram_mb, m.approx_mb, m.realtime_factor_cpu_avx2) {
                return m.name;
            }
        }
        "tiny"
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
    fn english_only_variants_beat_multilingual_on_english() {
        let wer_of = |name: &str| {
            ModelRegistry::get(name)
                .and_then(|m| m.wer_by_lang.iter().find(|&&(l, _)| l == "en"))
                .map(|&(_, w)| w)
                .expect("missing English WER")
        };
        for size in ["tiny", "small"] {
            let multi = wer_of(size);
            let en = wer_of(&format!("{size}.en"));
            assert!(en <= multi, "{size}.en={en} should be <= {size}={multi} on English");
        }
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
            assert!(has_en, "model '{}' is missing English WER in wer_by_lang", m.name);
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
        assert!(get_en_wer("tiny.en") > get_en_wer("small.en"));
    }

    #[test]
    fn wer_monotonically_better_in_multilingual_family() {
        let get_en_wer = |name: &str| {
            ModelRegistry::get(name)
                .and_then(|m| m.wer_by_lang.iter().find(|&&(l, _)| l == "en"))
                .map(|&(_, wer)| wer)
                .unwrap_or(f32::MAX)
        };
        assert!(get_en_wer("tiny") > get_en_wer("small"));
        assert!(get_en_wer("small") >= get_en_wer("large-v3-turbo"));
    }

    #[test]
    fn min_ram_monotonic_within_families() {
        let get = |name: &str| ModelRegistry::get(name).map(|m| m.min_ram_mb).unwrap_or(0);
        assert!(get("tiny.en") <= get("small.en"));
        assert!(get("tiny") <= get("small"));
        assert!(get("small") <= get("large-v3-turbo"));
    }

    #[test]
    fn realtime_factor_larger_models_are_slower() {
        let rf = |name: &str| {
            ModelRegistry::get(name).map(|m| m.realtime_factor_cpu_avx2).unwrap_or(0.0)
        };
        // Within multilingual family: tiny > small > turbo.
        assert!(rf("tiny") > rf("small"));
        assert!(rf("small") > rf("large-v3-turbo"));
        // Within English-only family.
        assert!(rf("tiny.en") > rf("small.en"));
        // Turbo at q8_0 clears 2.0 on the 8-core AVX2 reference — a
        // jump up from the fp16 turbo's 0.6 RTF.
        assert!(rf("large-v3-turbo") >= 2.0);
    }

    #[test]
    fn turbo_is_multilingual() {
        let m = ModelRegistry::get("large-v3-turbo").expect("turbo missing");
        assert!(m.multilingual);
    }

    #[test]
    fn base_family_dropped_from_registry() {
        // `base` and `base.en` were measured to be dominated by
        // `small-q5_1` / `small.en-q8_0` and removed from the registry
        // on 2026-05-19. If you add them back, refresh the perf-pass
        // bench results and document the rationale in an ADR.
        assert!(ModelRegistry::get("base").is_none());
        assert!(ModelRegistry::get("base.en").is_none());
    }

    #[test]
    fn registry_carries_exactly_five_entries() {
        // Locks in the post-perf-pass shape. If you grow this list,
        // refresh the wizard ladder + ADR 0026.
        assert_eq!(WHISPER_MODELS.len(), 5);
    }

    #[test]
    fn default_quantization_present_in_quantizations() {
        for m in WHISPER_MODELS {
            assert!(
                m.quantizations.iter().any(|v| v.quantization == m.default_quantization),
                "model '{}' default_quantization {:?} not listed in quantizations",
                m.name,
                m.default_quantization
            );
        }
    }

    #[test]
    fn approx_mb_matches_default_quant_variant() {
        for m in WHISPER_MODELS {
            let default = m
                .quantizations
                .iter()
                .find(|v| v.quantization == m.default_quantization)
                .expect("default variant missing");
            assert_eq!(
                m.approx_mb, default.approx_mb,
                "model '{}' approx_mb {} disagrees with default-variant size {}",
                m.name, m.approx_mb, default.approx_mb
            );
        }
    }

    #[test]
    fn defaults_match_acceptance_rule() {
        // Locks in the per-model quantization defaults from the
        // 2026-05-19 perf pass (see plans/2026-05-19-stt-perf-pass-v1.md).
        let cases = [
            ("tiny", Quantization::Q8_0),
            ("tiny.en", Quantization::Q8_0),
            ("small", Quantization::Q8_0),
            ("small.en", Quantization::Q8_0),
            ("large-v3-turbo", Quantization::Q8_0),
        ];
        for (name, expected) in cases {
            let m = ModelRegistry::get(name).expect("model missing");
            assert_eq!(
                m.default_quantization, expected,
                "model '{name}' default quantization shifted; refresh the bench data \
                 before updating the registry"
            );
        }
    }

    #[test]
    fn pinned_quantization_must_be_in_registry() {
        // `tiny` does not ship Fp16 — pinning should fail.
        let m = ModelRegistry::get("tiny").unwrap();
        assert!(ModelRegistry::resolve_quantization(
            m,
            QuantizationPref::Pinned(Quantization::Fp16)
        )
        .is_err());
        // `small` ships all three quantizations — every pin resolves.
        let m = ModelRegistry::get("small").unwrap();
        for q in [Quantization::Q5_1, Quantization::Q8_0, Quantization::Fp16] {
            assert_eq!(
                ModelRegistry::resolve_quantization(m, QuantizationPref::Pinned(q)).unwrap(),
                q
            );
        }
    }

    #[test]
    fn auto_resolves_to_default() {
        for m in WHISPER_MODELS {
            assert_eq!(
                ModelRegistry::resolve_quantization(m, QuantizationPref::Auto).unwrap(),
                m.default_quantization
            );
        }
    }

    #[test]
    fn filename_naming_scheme() {
        assert_eq!(ModelRegistry::filename("small", Quantization::Fp16), "ggml-small.bin");
        assert_eq!(ModelRegistry::filename("small", Quantization::Q5_1), "ggml-small-q5_1.bin");
        assert_eq!(
            ModelRegistry::filename("large-v3-turbo", Quantization::Q8_0),
            "ggml-large-v3-turbo-q8_0.bin"
        );
        assert_eq!(ModelRegistry::filename("tiny.en", Quantization::Q5_1), "ggml-tiny.en-q5_1.bin");
    }

    #[test]
    fn url_for_returns_none_for_unsupported_variant() {
        let m = ModelRegistry::get("tiny").unwrap();
        // tiny ships q5_1 and q8_0 (post-2026-05-25); fp16 is not in the
        // registry.
        assert!(ModelRegistry::url_for(m, Quantization::Fp16).is_none());
        assert!(ModelRegistry::url_for(m, Quantization::Q5_1).is_some());
        assert!(ModelRegistry::url_for(m, Quantization::Q8_0).is_some());
    }

    #[test]
    fn quantization_pref_parse() {
        assert_eq!(QuantizationPref::parse("auto"), Some(QuantizationPref::Auto));
        assert_eq!(QuantizationPref::parse(""), Some(QuantizationPref::Auto));
        assert_eq!(QuantizationPref::parse("  "), Some(QuantizationPref::Auto));
        assert_eq!(
            QuantizationPref::parse("q5_1"),
            Some(QuantizationPref::Pinned(Quantization::Q5_1))
        );
        assert_eq!(
            QuantizationPref::parse("Q8_0"),
            Some(QuantizationPref::Pinned(Quantization::Q8_0))
        );
        assert_eq!(
            QuantizationPref::parse("fp16"),
            Some(QuantizationPref::Pinned(Quantization::Fp16))
        );
        assert!(QuantizationPref::parse("q4_k_m").is_none());
    }

    fn fake_snap(cores: u32, ram_gb: u64, avx2: bool) -> fono_core::HardwareSnapshot {
        fono_core::HardwareSnapshot {
            physical_cores: cores,
            logical_cores: cores * 2,
            total_ram_bytes: ram_gb * 1024 * 1024 * 1024,
            available_ram_bytes: ram_gb * 1024 * 1024 * 1024,
            free_disk_bytes: 200 * 1024 * 1024 * 1024,
            cpu_features: fono_core::hwcheck::CpuFeatures { avx2, ..Default::default() },
            os: "linux".into(),
            arch: "x86_64".into(),
            host_gpu: fono_core::hwcheck::HostGpu::None,
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
        // Weak laptop: turbo unaffordable, small at q8_0 still unaffordable → small at default.
        let weak = fake_snap(2, 4, true);
        assert_eq!(ModelRegistry::pick_default_local(&weak), "small");

        // 16-core desktop: turbo at q8_0 (RTF 2.3) is Comfortable on a
        // 16-core box with plenty of RAM — it's the new top of the
        // ladder.
        let strong = fake_snap(16, 32, true);
        assert_eq!(ModelRegistry::pick_default_local(&strong), "large-v3-turbo");
    }

    /// For every (host, model-family) pair in the calibration matrix, the
    /// registry's chosen default quantization should not be more than 1.5×
    /// slower than the fastest variant for that family on that host.
    ///
    /// If a future calibration sweep trips this invariant, that's the
    /// signal to revisit the registry defaults (or to add an
    /// exception-with-rationale below).
    #[test]
    fn matrix_winners_within_1_5x() {
        use std::collections::BTreeMap;

        const MATRIX_JSON: &str = include_str!("../../../docs/bench/calibration/matrix.json");
        // Known-accepted exceptions: cells where the universal-q8_0
        // default is >1.5× slower than the matrix winner but the
        // wizard's host-pick policy never selects this model on this
        // host, so the slower RTF is unreachable in practice.
        const EXCEPTIONS: &[(&str, &str, &str)] = &[
            // i7-7500u CPU: wizard picks small.en, never tiny. Per plan
            // 2026-05-25-wizard-selection-heuristics-refresh-v5.md.
            ("i7-7500u", "cpu", "tiny"),
            // small.en on Vulkan iGPUs: every host strong enough to
            // run Vulkan lands large-v3-turbo Comfortable, which
            // outranks small.en on accuracy → small.en/Vulkan is
            // unreachable. See plan v5 §"Alternative Approaches" #1.
            ("i7-1255u", "vulkan", "small.en"),
            ("i7-7500u", "vulkan", "small.en"),
            ("i7-8550u", "vulkan", "small.en"),
            ("ultra7-258v", "vulkan", "small.en"),
        ];

        let v: serde_json::Value = serde_json::from_str(MATRIX_JSON).expect("matrix.json parses");
        // Flatten — matrix.json is `{ "cells": [...] }` or a top-level
        // array depending on the generator; walk both shapes.
        let cells: Vec<&serde_json::Value> = match &v {
            serde_json::Value::Array(a) => a.iter().collect(),
            serde_json::Value::Object(o) => o
                .get("cells")
                .and_then(|c| c.as_array())
                .map(|a| a.iter().collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        assert!(!cells.is_empty(), "matrix.json contained no cells");

        // Bucket cells by (host, family, backend). `model` looks like
        // `small`, `small-q5_1`, `small.en-q8_0`, etc. — the family is
        // the substring before the first `-`.
        let mut buckets: BTreeMap<(String, String, String), Vec<(String, f64)>> = BTreeMap::new();

        for cell in &cells {
            let host = cell.get("host").and_then(|h| h.as_str()).unwrap_or("");
            let backend = cell.get("build").and_then(|b| b.as_str()).unwrap_or("");
            let model = cell.get("model").and_then(|m| m.as_str()).unwrap_or("");
            let rtf = cell.get("batch_rtf_median").and_then(serde_json::Value::as_f64);
            if host.is_empty() || backend.is_empty() || model.is_empty() {
                continue;
            }
            let Some(rtf) = rtf else { continue };
            let family = model.split('-').next().unwrap_or(model).to_string();
            let quant = if let Some((_, q)) = model.split_once('-') {
                q.to_string()
            } else {
                "fp16".to_string()
            };
            buckets
                .entry((host.to_string(), family, backend.to_string()))
                .or_default()
                .push((quant, rtf));
        }

        // For each registry model, check every (host, backend) bucket
        // that contains the family.
        let mut violations: Vec<String> = Vec::new();
        for m in WHISPER_MODELS {
            // The registry's family is the model name verbatim (we
            // don't ship multiple families per ModelInfo).
            let want_family = m.name.to_string();
            let want_quant = match m.default_quantization {
                Quantization::Fp16 => "fp16",
                Quantization::Q5_1 => "q5_1",
                Quantization::Q8_0 => "q8_0",
            };
            for ((host, family, backend), variants) in &buckets {
                if family != &want_family {
                    continue;
                }
                if EXCEPTIONS
                    .iter()
                    .any(|(h, b, f)| h == host && b == backend && *f == want_family.as_str())
                {
                    continue;
                }
                // The fastest variant in this bucket is the matrix winner.
                let winner = variants.iter().map(|(_, rtf)| *rtf).fold(f64::MIN, f64::max);
                // Find the default's RTF; if absent, the registry has
                // not been calibrated against this cell — skip (the
                // affordability check elsewhere keeps us honest).
                let Some(default_rtf) = variants
                    .iter()
                    .find(|(q, _)| q == want_quant || (want_quant == "fp16" && q == "fp16"))
                    .map(|(_, r)| *r)
                else {
                    continue;
                };
                if winner > 0.0 && default_rtf * 1.5 < winner {
                    violations.push(format!(
                        "{host}/{backend}/{want_family}: default={want_quant} \
                         rtf={default_rtf:.2} < winner rtf={winner:.2}/1.5"
                    ));
                }
            }
        }

        assert!(
            violations.is_empty(),
            "registry default within 1.5× matrix winner failed:\n  {}",
            violations.join("\n  ")
        );
    }
}
