// SPDX-License-Identifier: GPL-3.0-only
//! Streaming↔batch equivalence harness. Plan v6 R18.
//!
//! Compares the text emitted by `SpeechToText::transcribe` (batch lane)
//! against the concatenated `Finalize` text emitted by
//! `StreamingStt::stream_transcribe` (streaming lane) for the same
//! input PCM, and reports a normalized Levenshtein distance plus
//! latency ratios (TTFF, TTC). The harness is the headline Slice A
//! acceptance gate; once all 12 curated fixtures are landed it gets
//! tightened to the strict v6 thresholds (R18.1).
//!
//! Slice A scope (this module):
//!
//! * Tier-1 (whisper-only) comparison — `--llm none`.
//! * Two curated fixtures (real audio when available, otherwise
//!   synthetic-tone placeholders flagged in the manifest).
//! * `--quick` flag for fast smoke runs.
//!
//! Tier-2 (with-LLM cleanup), the remaining 10 fixtures, and the
//! cloud-streaming rows of R18 land in Slice B.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::capabilities::ModelCapabilities;
use fono_stt::SpeechToText;

/// Slice-A normalized-Levenshtein PASS threshold. Looser than the v6
/// plan's R18.1 0.01 because Slice A may ship with synthetic-tone
/// placeholder fixtures that exercise harness shape rather than real
/// transcription accuracy. Tighten to 0.01 once the 12-fixture real-
/// audio set lands (tracked in the manifest's `synthetic_placeholder`
/// flag).
pub const TIER1_LEVENSHTEIN_THRESHOLD: f32 = 0.05;

/// One fixture entry in `tests/fixtures/equivalence/manifest.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct ManifestFixture {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub source_url: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub reference: String,
    #[serde(default = "default_lang")]
    pub language: String,
    /// True when the WAV is a synthesized tone/silence placeholder, not
    /// real speech. Harness loosens its accuracy expectations and the
    /// CI gate documents this in its summary.
    #[serde(default)]
    pub synthetic_placeholder: bool,
    /// Approx duration in seconds — informational, used by `--quick` to
    /// skip fixtures longer than the configured ceiling.
    #[serde(default)]
    pub duration_estimate_s: f32,
    /// Optional per-fixture override for the equivalence (stream↔batch)
    /// gate threshold. The legacy `levenshtein_threshold` TOML key
    /// continues to deserialize into this field via `serde(alias)`
    /// during the v0.2 → v0.3 transition; new fixtures should use
    /// `equivalence_threshold` directly.
    #[serde(default, alias = "levenshtein_threshold")]
    pub equivalence_threshold: Option<f32>,
    /// Optional per-fixture override for the accuracy (batch↔reference)
    /// gate threshold. When unset, `run_fixture` falls back to
    /// `equivalence_threshold` so existing manifests retain their
    /// pre-split behaviour.
    #[serde(default)]
    pub accuracy_threshold: Option<f32>,
    /// Explicit override for the "this fixture demands a multilingual
    /// model" decision. When `None`, the harness derives `language != "en"`.
    #[serde(default)]
    pub requires_multilingual: Option<bool>,
}

fn default_lang() -> String {
    "en".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub fixtures: Vec<ManifestFixture>,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read manifest {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("parse manifest {}", path.display()))
    }
}

/// Per-mode result for one fixture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModeResult {
    pub text: String,
    pub elapsed_ms: u128,
    /// Time-to-first-feedback. For batch this is identical to
    /// `elapsed_ms`; for streaming it's the timestamp of the first
    /// preview update.
    pub ttff_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Modes {
    pub batch: ModeResult,
    pub streaming: Option<ModeResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metrics {
    /// Stream↔batch consistency: normalized Levenshtein between the
    /// streaming-lane Finalize text and the batch-lane transcript.
    /// This is the original Slice A R18 equivalence number.
    pub stt_levenshtein_norm: f32,
    /// Accuracy: normalized Levenshtein between the batch transcript
    /// and the manifest's `reference` text. `None` when no reference
    /// is supplied for the fixture. `#[serde(default)]` so older JSON
    /// reports without this field continue to deserialize cleanly.
    #[serde(default)]
    pub stt_accuracy_levenshtein: Option<f32>,
    /// Streaming TTFF / batch TTC. `None` when streaming wasn't run.
    pub ttff_ratio: Option<f32>,
    /// Streaming TTC / batch TTC. `None` when streaming wasn't run.
    pub ttc_ratio: Option<f32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Fail,
    /// Run was skipped (e.g. `--quick` filter, missing model). Reported
    /// but never blocks the gate.
    Skipped,
}

/// Typed cause of a `Verdict::Skipped` row, persisted into the JSON
/// report so downstream consumers can distinguish capability-induced
/// skips (English-only model on a non-English fixture) from runtime
/// skips (`--quick` ceiling, missing streaming runtime, error before
/// inference completed).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Model lacks the language needed to transcribe the fixture.
    Capability,
    /// `--quick` filter elided the fixture as too long.
    Quick,
    /// Streaming runtime not wired up and no reference text supplied,
    /// so neither equivalence nor accuracy could be measured.
    NoStreaming,
    /// Inference raised an error before producing a verdict.
    RuntimeError,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquivalenceResult {
    pub fixture: String,
    pub language: String,
    pub synthetic_placeholder: bool,
    /// Audio duration in seconds, computed from the WAV sample count /
    /// sample rate. `0.0` for skipped or errored fixtures.
    pub duration_s: f64,
    pub modes: Modes,
    pub metrics: Metrics,
    pub verdict: Verdict,
    /// Typed skip reason for `Verdict::Skipped` rows. `None` for
    /// `Pass` / `Fail` rows or for legacy reports that predate this
    /// field.
    #[serde(default)]
    pub skip_reason: Option<SkipReason>,
    /// Free-text note (skip reason, threshold-override note, etc.).
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquivalenceReport {
    pub fono_version: String,
    pub stt_backend: String,
    pub tier: String,
    pub threshold_levenshtein: f32,
    pub results: Vec<EquivalenceResult>,
    /// R18.23: pinned `[interactive]` boundary knobs (and any other
    /// streaming-determinism inputs) so the JSON report is fully
    /// reproducible. `None` for legacy / older reports.
    #[serde(default)]
    pub pinned_params: Option<BoundaryKnobs>,
    /// Typed capability surface for the resolved STT backend (Wave 2
    /// Thread A). `None` for legacy reports written before the field
    /// landed.
    #[serde(default)]
    pub model_capabilities: Option<ModelCapabilities>,
}

impl EquivalenceReport {
    /// Roll up per-fixture verdicts into a single run-level verdict.
    ///
    /// Capability-induced skips (`SkipReason::Capability`) never make
    /// the run `Skipped` — they're an expected outcome on English-only
    /// models running over a multilingual manifest. A run is `Skipped`
    /// only when every row was skipped *for non-capability reasons*
    /// (no streaming runtime, `--quick` filter, etc.) — i.e. the
    /// developer ran the harness on hardware/config that simply has
    /// no executable rows.
    #[must_use]
    pub fn overall_verdict(&self) -> Verdict {
        if self.results.is_empty() {
            return Verdict::Skipped;
        }
        let mut saw_pass = false;
        let mut all_skipped_capability = true;
        let mut all_skipped = true;
        for r in &self.results {
            match r.verdict {
                Verdict::Fail => return Verdict::Fail,
                Verdict::Pass => {
                    saw_pass = true;
                    all_skipped = false;
                    all_skipped_capability = false;
                }
                Verdict::Skipped => {
                    if r.skip_reason != Some(SkipReason::Capability) {
                        all_skipped_capability = false;
                    }
                }
            }
        }
        if saw_pass {
            Verdict::Pass
        } else if all_skipped && all_skipped_capability {
            // Pure capability-skip run with zero executable rows still
            // counts as Pass (the manifest is just incompatible with
            // the chosen model — not a failure of the harness).
            Verdict::Pass
        } else {
            Verdict::Skipped
        }
    }
}

/// Strip absolute timing fields from an [`EquivalenceReport`] so it can
/// be committed as a deterministic per-PR comparison anchor.
///
/// Wave 2 Thread C: CI runs `fono-bench equivalence ... --baseline
/// --output ci-bench.json` on every PR and diffs `ci-bench.json`
/// against `docs/bench/baseline-comfortable-tiny-en.json`. The
/// committed baseline must therefore contain only fields that are
/// stable across machines / runs: per-fixture verdicts, structural
/// metadata (`model_capabilities`, `pinned_params`, `skip_reason`),
/// and **ratios** (which are relative). Absolute milliseconds and
/// audio durations would flap on shared runners and are not part of
/// the contract.
///
/// Returns a `serde_json::Value` so downstream tooling can pretty-print
/// or hash it without the JSON object being coupled to a Rust type.
#[must_use]
pub fn baseline_subset(report: &EquivalenceReport) -> serde_json::Value {
    let mut v = serde_json::to_value(report).expect("EquivalenceReport always serialises");
    if let Some(results) = v.get_mut("results").and_then(|r| r.as_array_mut()) {
        for row in results.iter_mut() {
            if let Some(obj) = row.as_object_mut() {
                obj.remove("duration_s");
                if let Some(modes) = obj.get_mut("modes").and_then(|m| m.as_object_mut()) {
                    for k in ["batch", "streaming"] {
                        if let Some(mode) = modes.get_mut(k) {
                            if let Some(mo) = mode.as_object_mut() {
                                mo.remove("elapsed_ms");
                                mo.remove("ttff_ms");
                            }
                        }
                    }
                }
            }
        }
    }
    v
}

// ---------------------------------------------------------------------
// v7 boundary-knob harness pinning (R18.10 amended + R18.23).
// ---------------------------------------------------------------------

/// The set of `[interactive]` boundary-heuristic knobs the harness
/// pins for a streaming run, copied verbatim into the JSON report's
/// `pinned_params` block (R18.23). Persisting the values makes
/// streaming-vs-batch comparisons reproducible across machines and
/// guards against the "we changed the default and the diff drifted
/// silently" failure mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundaryKnobs {
    pub commit_use_prosody: bool,
    pub commit_prosody_extend_ms: u32,
    pub commit_use_punctuation_hint: bool,
    pub commit_punct_extend_ms: u32,
    pub commit_hold_on_filler: bool,
    pub commit_filler_words: Vec<String>,
    pub commit_dangling_words: Vec<String>,
    pub eou_drain_extended_ms: u32,
    pub chunk_ms_initial: u32,
    pub chunk_ms_steady: u32,
}

impl BoundaryKnobs {
    /// Defaults match `fono_core::config::Interactive::default()`.
    /// Used by the `A2-default` row, which is also the v6 / Slice A
    /// equivalence-gate row.
    #[must_use]
    pub fn defaults() -> Self {
        Self {
            commit_use_prosody: false,
            commit_prosody_extend_ms: 250,
            commit_use_punctuation_hint: true,
            commit_punct_extend_ms: 150,
            commit_hold_on_filler: true,
            commit_filler_words: vec![
                "um".into(),
                "uh".into(),
                "er".into(),
                "ah".into(),
                "mm".into(),
                "like".into(),
                "you know".into(),
            ],
            commit_dangling_words: vec![
                "and".into(),
                "but".into(),
                "or".into(),
                "so".into(),
                "because".into(),
                "the".into(),
                "a".into(),
                "an".into(),
                "of".into(),
                "to".into(),
                "with".into(),
                "for".into(),
                "in".into(),
                "on".into(),
                "at".into(),
                "from".into(),
            ],
            eou_drain_extended_ms: 1500,
            chunk_ms_initial: 600,
            chunk_ms_steady: 1500,
        }
    }

    /// All-off variant: every heuristic disabled, every extension
    /// zeroed. The `A2-no-heur` row uses this to prove heuristics are
    /// additive — Tier-1 + Tier-2 must still pass under this config.
    #[must_use]
    pub fn all_off() -> Self {
        Self {
            commit_use_prosody: false,
            commit_prosody_extend_ms: 0,
            commit_use_punctuation_hint: false,
            commit_punct_extend_ms: 0,
            commit_hold_on_filler: false,
            commit_filler_words: Vec::new(),
            commit_dangling_words: Vec::new(),
            eou_drain_extended_ms: 0,
            chunk_ms_initial: 600,
            chunk_ms_steady: 1500,
        }
    }
}

/// Named A/B variant of the boundary knobs the harness can run a fixture
/// against. Tier-1 / Tier-2 *gate* on the `A2-default` row only; the
/// other three rows ship as informational diff reports for tuning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessVariant {
    pub name: String,
    /// `true` for the row whose verdict gates CI; `false` for
    /// informational rows.
    pub gating: bool,
    pub knobs: BoundaryKnobs,
}

/// Default top-level config the harness CLI hands to `run_fixture` for
/// each row. Currently a thin wrapper around the variant list; future
/// fields (per-tier overrides, cloud-row knobs, etc.) land here without
/// re-typing the row vec at every call site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessConfig {
    pub variants: Vec<HarnessVariant>,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            variants: a2_variants(),
        }
    }
}

/// The four R18.23 A2 rows. Order is stable so the JSON report renders
/// deterministically.
#[must_use]
pub fn a2_variants() -> Vec<HarnessVariant> {
    let prosody_only = BoundaryKnobs {
        commit_use_prosody: true,
        ..BoundaryKnobs::all_off()
    };
    let filler_only = {
        let d = BoundaryKnobs::defaults();
        BoundaryKnobs {
            commit_use_prosody: false,
            commit_use_punctuation_hint: false,
            commit_hold_on_filler: true,
            commit_filler_words: d.commit_filler_words,
            commit_dangling_words: d.commit_dangling_words,
            eou_drain_extended_ms: d.eou_drain_extended_ms,
            commit_prosody_extend_ms: 0,
            commit_punct_extend_ms: 0,
            chunk_ms_initial: d.chunk_ms_initial,
            chunk_ms_steady: d.chunk_ms_steady,
        }
    };
    vec![
        HarnessVariant {
            name: "A2-no-heur".into(),
            gating: false,
            knobs: BoundaryKnobs::all_off(),
        },
        HarnessVariant {
            name: "A2-default".into(),
            gating: true,
            knobs: BoundaryKnobs::defaults(),
        },
        HarnessVariant {
            name: "A2-prosody".into(),
            gating: false,
            knobs: prosody_only,
        },
        HarnessVariant {
            name: "A2-filler".into(),
            gating: false,
            knobs: filler_only,
        },
    ]
}

// ---------------------------------------------------------------------
// Levenshtein normalization helper.
// ---------------------------------------------------------------------

/// Case-fold and collapse internal whitespace for distance comparison.
#[must_use]
pub fn normalize_for_compare(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = true; // strip leading whitespace
    for ch in s.chars().flat_map(char::to_lowercase) {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Plain (non-normalized) Levenshtein on byte-stride iterators of
/// `char`s. O(n*m) time, O(min(n,m)) space.
fn levenshtein_chars(a: &[char], b: &[char]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let (a, b) = if a.len() < b.len() { (b, a) } else { (a, b) };
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Normalized Levenshtein distance in `[0.0, 1.0]`. Inputs are first
/// case-folded and whitespace-collapsed via [`normalize_for_compare`].
#[must_use]
pub fn levenshtein_norm(a: &str, b: &str) -> f32 {
    let na = normalize_for_compare(a);
    let nb = normalize_for_compare(b);
    let ac: Vec<char> = na.chars().collect();
    let bc: Vec<char> = nb.chars().collect();
    let denom = ac.len().max(bc.len()).max(1);
    let dist = levenshtein_chars(&ac, &bc);
    dist as f32 / denom as f32
}

// ---------------------------------------------------------------------
// Harness execution.
// ---------------------------------------------------------------------

/// Top-level driver that runs both passes for one fixture and produces
/// an [`EquivalenceResult`].
pub async fn run_fixture(
    fixture: &ManifestFixture,
    fixture_root: &Path,
    stt: Arc<dyn SpeechToText>,
    streaming_stt: Option<Arc<dyn StreamingSttHandle>>,
    caps: &ModelCapabilities,
    quick_max_seconds: Option<f32>,
) -> Result<EquivalenceResult> {
    // Capability gate first — short-circuits before any WAV read so
    // the test for English-only models on multilingual fixtures stays
    // free of disk I/O, and the mock-STT capability-skip integration
    // test can drive it without committing more audio.
    if caps.english_only
        && ModelCapabilities::fixture_requires_multilingual(
            &fixture.language,
            fixture.requires_multilingual,
        )
    {
        return Ok(skipped_with_reason(
            fixture,
            SkipReason::Capability,
            format!(
                "model {} is English-only; fixture language is {}",
                caps.model_label, fixture.language
            ),
        ));
    }

    let path = fixture_root.join(&fixture.path);
    if let Some(max) = quick_max_seconds {
        if fixture.duration_estimate_s > max {
            return Ok(skipped_with_reason(
                fixture,
                SkipReason::Quick,
                format!(
                    "fixture longer than --quick ceiling ({:.1}s > {:.1}s)",
                    fixture.duration_estimate_s, max
                ),
            ));
        }
    }

    let wav =
        crate::wav::read(&path).with_context(|| format!("read fixture {}", path.display()))?;
    let pcm = wav.samples;
    let sample_rate = wav.sample_rate;
    let duration_s = if sample_rate > 0 {
        pcm.len() as f64 / sample_rate as f64
    } else {
        0.0
    };

    // Batch pass.
    let lang_owned = if fixture.language.is_empty() {
        None
    } else {
        Some(fixture.language.clone())
    };
    let lang = lang_owned.as_deref();
    let t0 = Instant::now();
    let batch_tx = stt.transcribe(&pcm, sample_rate, lang).await?;
    let batch_elapsed = t0.elapsed();
    let batch = ModeResult {
        text: batch_tx.text.clone(),
        elapsed_ms: batch_elapsed.as_millis(),
        ttff_ms: batch_elapsed.as_millis(),
    };

    // Streaming pass (only when streaming is available + compiled in).
    let (streaming, levenshtein) = if let Some(stream) = streaming_stt.as_ref() {
        let stream_res = stream
            .run_streaming(&pcm, sample_rate, lang_owned.clone())
            .await?;
        let lev = levenshtein_norm(&stream_res.text, &batch.text);
        (Some(stream_res), lev)
    } else {
        (None, 0.0)
    };

    // Accuracy pass: compare the batch transcript to the manifest's
    // canonical reference, when one was supplied. Independent of the
    // streaming pass — a fixture with a reference still gets an
    // accuracy number even when no streaming runtime is wired up.
    let accuracy = if fixture.reference.is_empty() {
        None
    } else {
        Some(levenshtein_norm(&batch.text, &fixture.reference))
    };

    let ttff_ratio = streaming.as_ref().map(|s| ratio(s.ttff_ms, batch.ttff_ms));
    let ttc_ratio = streaming
        .as_ref()
        .map(|s| ratio(s.elapsed_ms, batch.elapsed_ms));

    let equiv_threshold = fixture
        .equivalence_threshold
        .unwrap_or(TIER1_LEVENSHTEIN_THRESHOLD);
    // When no separate accuracy threshold is set, fall back to the
    // equivalence threshold so existing manifests preserve their
    // pre-split behaviour.
    let acc_threshold = fixture.accuracy_threshold.unwrap_or(equiv_threshold);

    // Two independent gates against (potentially) distinct per-fixture
    // thresholds:
    //   * equiv  — stream-lane vs batch-lane Levenshtein.
    //   * acc    — batch-lane vs manifest reference Levenshtein.
    // A fixture is `Pass` only when every gate that can be evaluated
    // passes. If neither gate has data (no streaming runtime AND no
    // reference text) we report `Skipped` rather than a vacuous pass.
    let equiv_evaluated = streaming.is_some();
    let equiv_input = equiv_evaluated.then_some(levenshtein);
    let verdict = decide_verdict(equiv_input, accuracy, equiv_threshold, acc_threshold);
    let equiv_pass = equiv_input.is_none_or(|v| v <= equiv_threshold);

    let mut note = String::new();
    if fixture.synthetic_placeholder {
        note.push_str("synthetic placeholder (not real speech); ");
    }
    if let Some(t) = fixture.equivalence_threshold {
        note.push_str(&format!("per-fixture equiv threshold {t}; "));
    }
    if let Some(t) = fixture.accuracy_threshold {
        note.push_str(&format!("per-fixture acc threshold {t}; "));
    }
    if equiv_evaluated && !equiv_pass {
        note.push_str(&format!(
            "equiv {levenshtein:.3} > {equiv_threshold:.3}; "
        ));
    }
    if let Some(a) = accuracy {
        if a > acc_threshold {
            note.push_str(&format!("acc {a:.3} > {acc_threshold:.3}; "));
        }
    }

    let skip_reason = if matches!(verdict, Verdict::Skipped) {
        Some(SkipReason::NoStreaming)
    } else {
        None
    };

    Ok(EquivalenceResult {
        fixture: fixture.name.clone(),
        language: fixture.language.clone(),
        synthetic_placeholder: fixture.synthetic_placeholder,
        duration_s,
        modes: Modes { batch, streaming },
        metrics: Metrics {
            stt_levenshtein_norm: levenshtein,
            stt_accuracy_levenshtein: accuracy,
            ttff_ratio,
            ttc_ratio,
        },
        verdict,
        skip_reason,
        note: note.trim_end_matches("; ").to_string(),
    })
}

fn ratio(num_ms: u128, den_ms: u128) -> f32 {
    if den_ms == 0 {
        return f32::INFINITY;
    }
    num_ms as f32 / den_ms as f32
}

/// Combine the optional equivalence (stream↔batch) and accuracy
/// (batch↔reference) Levenshtein values into a single fixture verdict.
///
/// * `equiv` — `Some` when a streaming pass was run, carrying the
///   stream↔batch distance; `None` when streaming wasn't available.
/// * `accuracy` — `Some` when the manifest supplied a non-empty
///   `reference`, carrying the batch↔reference distance; `None`
///   otherwise.
/// * `threshold` — single per-fixture threshold applied to both gates.
///
/// Returns `Skipped` when neither input was evaluated, `Pass` when
/// every evaluated input is at or below the threshold, `Fail` when at
/// least one evaluated input exceeds it.
fn decide_verdict(
    equiv: Option<f32>,
    accuracy: Option<f32>,
    equiv_threshold: f32,
    acc_threshold: f32,
) -> Verdict {
    if equiv.is_none() && accuracy.is_none() {
        return Verdict::Skipped;
    }
    let equiv_ok = equiv.is_none_or(|v| v <= equiv_threshold);
    let acc_ok = accuracy.is_none_or(|v| v <= acc_threshold);
    if equiv_ok && acc_ok {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}

/// Build a `Verdict::Skipped` row with a typed skip reason.
pub(crate) fn skipped_with_reason(
    fixture: &ManifestFixture,
    reason: SkipReason,
    note: impl Into<String>,
) -> EquivalenceResult {
    EquivalenceResult {
        fixture: fixture.name.clone(),
        language: fixture.language.clone(),
        synthetic_placeholder: fixture.synthetic_placeholder,
        duration_s: 0.0,
        modes: Modes {
            batch: ModeResult {
                text: String::new(),
                elapsed_ms: 0,
                ttff_ms: 0,
            },
            streaming: None,
        },
        metrics: Metrics {
            stt_levenshtein_norm: 0.0,
            stt_accuracy_levenshtein: None,
            ttff_ratio: None,
            ttc_ratio: None,
        },
        verdict: Verdict::Skipped,
        skip_reason: Some(reason),
        note: note.into(),
    }
}

// ---------------------------------------------------------------------
// Streaming-pass adapter.
// ---------------------------------------------------------------------

/// Trait-object boundary so the harness can stay STT-agnostic *and*
/// compile cleanly without the `streaming` cargo feature (in which case
/// no implementation is registered and the streaming pass is skipped).
#[async_trait::async_trait]
pub trait StreamingSttHandle: Send + Sync {
    async fn run_streaming(
        &self,
        pcm: &[f32],
        sample_rate: u32,
        lang: Option<String>,
    ) -> Result<ModeResult>;
}

#[cfg(feature = "equivalence")]
pub use streaming_impl::WhisperStreamingHandle;

#[cfg(feature = "equivalence")]
mod streaming_impl {
    use super::{ModeResult, StreamingSttHandle};
    use anyhow::Result;
    use fono_stt::{StreamFrame, StreamingStt, UpdateLane};
    use futures::stream::{BoxStream, StreamExt};
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::UnboundedReceiverStream;

    /// Adapter that builds a 30 ms PCM-chunked `BoxStream<StreamFrame>`
    /// from a buffer and consumes the resulting `TranscriptUpdate`s,
    /// concatenating Finalize-lane text in order and recording the
    /// first-preview wallclock for TTFF.
    pub struct WhisperStreamingHandle {
        pub stt: Arc<dyn StreamingStt>,
    }

    impl WhisperStreamingHandle {
        #[must_use]
        pub fn new(stt: Arc<dyn StreamingStt>) -> Self {
            Self { stt }
        }
    }

    #[async_trait::async_trait]
    impl StreamingSttHandle for WhisperStreamingHandle {
        async fn run_streaming(
            &self,
            pcm: &[f32],
            sample_rate: u32,
            lang: Option<String>,
        ) -> Result<ModeResult> {
            let chunk_samples = ((sample_rate as usize / 1000) * 30).max(1);
            let (tx, rx) = mpsc::unbounded_channel::<StreamFrame>();
            for chunk in pcm.chunks(chunk_samples) {
                let _ = tx.send(StreamFrame::Pcm(chunk.to_vec()));
            }
            let _ = tx.send(StreamFrame::Eof);
            drop(tx);
            let frames: BoxStream<'static, StreamFrame> = UnboundedReceiverStream::new(rx).boxed();

            let started = Instant::now();
            let mut updates = self
                .stt
                .stream_transcribe(frames, sample_rate, lang)
                .await?;
            let mut finalized: Vec<String> = Vec::new();
            let mut ttff: Option<u128> = None;
            while let Some(u) = updates.next().await {
                if ttff.is_none() {
                    ttff = Some(started.elapsed().as_millis());
                }
                if matches!(u.lane, UpdateLane::Finalize) {
                    finalized.push(u.text);
                }
            }
            let elapsed = started.elapsed();
            let text = finalized.join(" ");
            Ok(ModeResult {
                text,
                elapsed_ms: elapsed.as_millis(),
                ttff_ms: ttff.unwrap_or(elapsed.as_millis()),
            })
        }
    }
}

// ---------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_and_lowers() {
        assert_eq!(normalize_for_compare("  Hello,  WORLD\t\n"), "hello, world");
        assert_eq!(normalize_for_compare(""), "");
        assert_eq!(normalize_for_compare("   "), "");
    }

    #[test]
    fn levenshtein_norm_basic_pairs() {
        assert!((levenshtein_norm("hello world", "hello world")).abs() < 1e-6);
        // One char insertion in 11 chars -> ~0.083
        let d = levenshtein_norm("hello world", "hello worlds");
        assert!(d > 0.0 && d < 0.15, "got {d}");
        // Empty pair should be zero (max(0,0).max(1) = 1; dist = 0).
        assert!((levenshtein_norm("", "")).abs() < 1e-6);
        // Disjoint strings -> 1.0.
        let d = levenshtein_norm("abcdef", "xyzwuv");
        assert!((d - 1.0).abs() < 1e-6);
    }

    #[test]
    fn levenshtein_norm_is_case_insensitive() {
        assert!((levenshtein_norm("Hello World", "hello   world")).abs() < 1e-6);
    }

    #[test]
    fn report_round_trips_serde() {
        let res = EquivalenceResult {
            fixture: "demo".into(),
            language: "en".into(),
            synthetic_placeholder: false,
            duration_s: 3.0,
            modes: Modes {
                batch: ModeResult {
                    text: "hi".into(),
                    elapsed_ms: 100,
                    ttff_ms: 100,
                },
                streaming: Some(ModeResult {
                    text: "hi".into(),
                    elapsed_ms: 110,
                    ttff_ms: 30,
                }),
            },
            metrics: Metrics {
                stt_levenshtein_norm: 0.0,
                stt_accuracy_levenshtein: None,
                ttff_ratio: Some(0.3),
                ttc_ratio: Some(1.1),
            },
            verdict: Verdict::Pass,
            skip_reason: None,
            note: String::new(),
        };
        let report = EquivalenceReport {
            fono_version: env!("CARGO_PKG_VERSION").into(),
            stt_backend: "fake".into(),
            tier: "tier1".into(),
            threshold_levenshtein: TIER1_LEVENSHTEIN_THRESHOLD,
            results: vec![res],
            pinned_params: Some(BoundaryKnobs::defaults()),
            model_capabilities: None,
        };
        let json = serde_json::to_string(&report).expect("serialize");
        let back: EquivalenceReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.results.len(), 1);
        assert_eq!(back.results[0].verdict, Verdict::Pass);
        assert_eq!(back.overall_verdict(), Verdict::Pass);
    }

    #[test]
    fn overall_verdict_fails_on_any_fail() {
        let mut report = EquivalenceReport {
            fono_version: "0".into(),
            stt_backend: "x".into(),
            tier: "tier1".into(),
            threshold_levenshtein: 0.05,
            results: Vec::new(),
            pinned_params: None,
            model_capabilities: None,
        };
        report.results.push(make_result("a", Verdict::Pass));
        report.results.push(make_result("b", Verdict::Fail));
        assert_eq!(report.overall_verdict(), Verdict::Fail);
    }

    #[test]
    fn overall_verdict_skipped_when_all_skipped() {
        let mut report = EquivalenceReport {
            fono_version: "0".into(),
            stt_backend: "x".into(),
            tier: "tier1".into(),
            threshold_levenshtein: 0.05,
            results: Vec::new(),
            pinned_params: None,
            model_capabilities: None,
        };
        // Non-capability skips (NoStreaming) keep the run as Skipped.
        let mut a = make_result("a", Verdict::Skipped);
        a.skip_reason = Some(SkipReason::NoStreaming);
        let mut b = make_result("b", Verdict::Skipped);
        b.skip_reason = Some(SkipReason::NoStreaming);
        report.results.push(a);
        report.results.push(b);
        assert_eq!(report.overall_verdict(), Verdict::Skipped);
    }

    #[test]
    fn overall_verdict_pass_when_all_skipped_capability() {
        // A pure capability-skip run (English-only model on a fully
        // multilingual manifest) reports Pass, not Skipped — there's
        // nothing the harness could have run.
        let mut report = EquivalenceReport {
            fono_version: "0".into(),
            stt_backend: "x".into(),
            tier: "tier1".into(),
            threshold_levenshtein: 0.05,
            results: Vec::new(),
            pinned_params: None,
            model_capabilities: None,
        };
        let mut a = make_result("a", Verdict::Skipped);
        a.skip_reason = Some(SkipReason::Capability);
        let mut b = make_result("b", Verdict::Skipped);
        b.skip_reason = Some(SkipReason::Capability);
        report.results.push(a);
        report.results.push(b);
        assert_eq!(report.overall_verdict(), Verdict::Pass);
    }

    fn make_result(name: &str, v: Verdict) -> EquivalenceResult {
        EquivalenceResult {
            fixture: name.into(),
            language: "en".into(),
            synthetic_placeholder: false,
            duration_s: 0.0,
            modes: Modes {
                batch: ModeResult {
                    text: String::new(),
                    elapsed_ms: 0,
                    ttff_ms: 0,
                },
                streaming: None,
            },
            metrics: Metrics {
                stt_levenshtein_norm: 0.0,
                stt_accuracy_levenshtein: None,
                ttff_ratio: None,
                ttc_ratio: None,
            },
            verdict: v,
            skip_reason: None,
            note: String::new(),
        }
    }

    #[test]
    fn manifest_parses_minimal() {
        let raw = r#"
            [[fixtures]]
            name = "demo"
            path = "demo.wav"
            language = "en"
            synthetic_placeholder = true
            duration_estimate_s = 3.0
        "#;
        let m: Manifest = toml::from_str(raw).expect("parse");
        assert_eq!(m.fixtures.len(), 1);
        assert_eq!(m.fixtures[0].name, "demo");
        assert!(m.fixtures[0].synthetic_placeholder);
    }

    #[test]
    fn harness_default_has_four_distinguishable_a2_variants() {
        let cfg = HarnessConfig::default();
        assert_eq!(cfg.variants.len(), 4);
        let names: Vec<&str> = cfg.variants.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["A2-no-heur", "A2-default", "A2-prosody", "A2-filler"]
        );

        // Exactly one gating row.
        let gating: Vec<&str> = cfg
            .variants
            .iter()
            .filter(|v| v.gating)
            .map(|v| v.name.as_str())
            .collect();
        assert_eq!(gating, vec!["A2-default"]);

        // Knob sets must be pairwise distinct — otherwise the diff
        // report would duplicate a row.
        for i in 0..cfg.variants.len() {
            for j in (i + 1)..cfg.variants.len() {
                assert_ne!(
                    cfg.variants[i].knobs, cfg.variants[j].knobs,
                    "variants {} and {} share an identical knob set",
                    cfg.variants[i].name, cfg.variants[j].name,
                );
            }
        }

        // Specific shape checks: A2-no-heur must have every flag off,
        // A2-default must keep punct + filler on, A2-prosody must
        // isolate the prosody flag, A2-filler must isolate the
        // filler-hold flag.
        let no_heur = &cfg.variants[0].knobs;
        assert!(!no_heur.commit_use_prosody);
        assert!(!no_heur.commit_use_punctuation_hint);
        assert!(!no_heur.commit_hold_on_filler);

        let default = &cfg.variants[1].knobs;
        assert!(!default.commit_use_prosody);
        assert!(default.commit_use_punctuation_hint);
        assert!(default.commit_hold_on_filler);

        let prosody = &cfg.variants[2].knobs;
        assert!(prosody.commit_use_prosody);
        assert!(!prosody.commit_use_punctuation_hint);
        assert!(!prosody.commit_hold_on_filler);

        let filler = &cfg.variants[3].knobs;
        assert!(!filler.commit_use_prosody);
        assert!(!filler.commit_use_punctuation_hint);
        assert!(filler.commit_hold_on_filler);
    }

    #[test]
    fn pinned_params_round_trip_through_report() {
        let report = EquivalenceReport {
            fono_version: "0".into(),
            stt_backend: "fake".into(),
            tier: "tier1".into(),
            threshold_levenshtein: 0.05,
            results: Vec::new(),
            pinned_params: Some(BoundaryKnobs::defaults()),
            model_capabilities: None,
        };
        let json = serde_json::to_string(&report).expect("serialize");
        let back: EquivalenceReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.pinned_params, Some(BoundaryKnobs::defaults()));
    }

    #[test]
    fn decide_verdict_two_gates() {
        // Neither evaluated → Skipped.
        assert_eq!(decide_verdict(None, None, 0.20, 0.20), Verdict::Skipped);
        // Equiv only, passing.
        assert_eq!(
            decide_verdict(Some(0.05), None, 0.20, 0.20),
            Verdict::Pass
        );
        // Equiv only, failing.
        assert_eq!(
            decide_verdict(Some(0.30), None, 0.20, 0.20),
            Verdict::Fail
        );
        // Accuracy only, passing — verdict reflects accuracy even
        // without a streaming pass.
        assert_eq!(
            decide_verdict(None, Some(0.10), 0.20, 0.20),
            Verdict::Pass
        );
        // Accuracy only, failing.
        assert_eq!(
            decide_verdict(None, Some(0.40), 0.20, 0.20),
            Verdict::Fail
        );
        // Both gates evaluated and pass.
        assert_eq!(
            decide_verdict(Some(0.02), Some(0.05), 0.20, 0.20),
            Verdict::Pass
        );
        // Equiv passes, accuracy fails → Fail (catches "tiny.en
        // hallucinates the same gibberish in both lanes").
        assert_eq!(
            decide_verdict(Some(0.00), Some(0.80), 0.20, 0.20),
            Verdict::Fail
        );
        // Equiv fails, accuracy passes → Fail.
        assert_eq!(
            decide_verdict(Some(0.50), Some(0.05), 0.20, 0.20),
            Verdict::Fail
        );
        // Boundary: exactly at threshold counts as pass on both gates.
        assert_eq!(
            decide_verdict(Some(0.20), Some(0.20), 0.20, 0.20),
            Verdict::Pass
        );
        // Split thresholds: equiv tight, accuracy loose.
        assert_eq!(
            decide_verdict(Some(0.05), Some(0.25), 0.05, 0.30),
            Verdict::Pass
        );
        // Split thresholds: equiv loose, accuracy tight — acc fails.
        assert_eq!(
            decide_verdict(Some(0.05), Some(0.25), 0.30, 0.05),
            Verdict::Fail
        );
    }

    #[test]
    fn manifest_alias_levenshtein_threshold_reads_into_equivalence() {
        // Back-compat: legacy manifests using `levenshtein_threshold`
        // continue to deserialize into the renamed
        // `equivalence_threshold` field via #[serde(alias)].
        let raw = r#"
            [[fixtures]]
            name = "legacy"
            path = "legacy.wav"
            language = "en"
            duration_estimate_s = 5.0
            levenshtein_threshold = 0.42
        "#;
        let m: Manifest = toml::from_str(raw).expect("parse legacy alias");
        assert_eq!(m.fixtures[0].equivalence_threshold, Some(0.42));
        assert!(m.fixtures[0].accuracy_threshold.is_none());
    }

    #[test]
    fn metrics_back_compat_deserializes_without_accuracy_field() {
        // Old reports written before stt_accuracy_levenshtein existed
        // must continue to deserialize cleanly. The field defaults to
        // None via #[serde(default)].
        let legacy = r#"{
            "stt_levenshtein_norm": 0.05,
            "ttff_ratio": 0.4,
            "ttc_ratio": 1.1
        }"#;
        let m: Metrics = serde_json::from_str(legacy).expect("legacy deserializes");
        assert!((m.stt_levenshtein_norm - 0.05).abs() < 1e-6);
        assert!(m.stt_accuracy_levenshtein.is_none());
        assert_eq!(m.ttff_ratio, Some(0.4));
        assert_eq!(m.ttc_ratio, Some(1.1));
    }
}
