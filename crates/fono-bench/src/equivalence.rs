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
    /// Optional per-fixture override for the levenshtein threshold.
    #[serde(default)]
    pub levenshtein_threshold: Option<f32>,
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
    pub stt_levenshtein_norm: f32,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquivalenceResult {
    pub fixture: String,
    pub language: String,
    pub synthetic_placeholder: bool,
    pub modes: Modes,
    pub metrics: Metrics,
    pub verdict: Verdict,
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
}

impl EquivalenceReport {
    #[must_use]
    pub fn overall_verdict(&self) -> Verdict {
        if self.results.is_empty() {
            return Verdict::Skipped;
        }
        let mut saw_pass = false;
        for r in &self.results {
            match r.verdict {
                Verdict::Fail => return Verdict::Fail,
                Verdict::Pass => saw_pass = true,
                Verdict::Skipped => {}
            }
        }
        if saw_pass {
            Verdict::Pass
        } else {
            Verdict::Skipped
        }
    }
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
            curr[j + 1] = (prev[j + 1] + 1)
                .min(curr[j] + 1)
                .min(prev[j] + cost);
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
    quick_max_seconds: Option<f32>,
) -> Result<EquivalenceResult> {
    let path = fixture_root.join(&fixture.path);
    if let Some(max) = quick_max_seconds {
        if fixture.duration_estimate_s > max {
            return Ok(skipped(
                fixture,
                format!(
                    "fixture longer than --quick ceiling ({:.1}s > {:.1}s)",
                    fixture.duration_estimate_s, max
                ),
            ));
        }
    }

    let wav = crate::wav::read(&path)
        .with_context(|| format!("read fixture {}", path.display()))?;
    let pcm = wav.samples;
    let sample_rate = wav.sample_rate;

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
        let stream_res = stream.run_streaming(&pcm, sample_rate, lang_owned.clone()).await?;
        let lev = levenshtein_norm(&stream_res.text, &batch.text);
        (Some(stream_res), lev)
    } else {
        (None, 0.0)
    };

    let ttff_ratio = streaming
        .as_ref()
        .map(|s| ratio(s.ttff_ms, batch.ttff_ms));
    let ttc_ratio = streaming
        .as_ref()
        .map(|s| ratio(s.elapsed_ms, batch.elapsed_ms));

    let threshold = fixture
        .levenshtein_threshold
        .unwrap_or(TIER1_LEVENSHTEIN_THRESHOLD);
    let verdict = if streaming.is_none() {
        // No streaming pass means we can't compare; not a failure but
        // not a pass either.
        Verdict::Skipped
    } else if levenshtein <= threshold {
        Verdict::Pass
    } else {
        Verdict::Fail
    };

    let mut note = String::new();
    if fixture.synthetic_placeholder {
        note.push_str("synthetic placeholder (not real speech); ");
    }
    if let Some(t) = fixture.levenshtein_threshold {
        note.push_str(&format!("per-fixture threshold {t}; "));
    }

    Ok(EquivalenceResult {
        fixture: fixture.name.clone(),
        language: fixture.language.clone(),
        synthetic_placeholder: fixture.synthetic_placeholder,
        modes: Modes { batch, streaming },
        metrics: Metrics {
            stt_levenshtein_norm: levenshtein,
            ttff_ratio,
            ttc_ratio,
        },
        verdict,
        note: note.trim_end_matches("; ").to_string(),
    })
}

fn ratio(num_ms: u128, den_ms: u128) -> f32 {
    if den_ms == 0 {
        return f32::INFINITY;
    }
    num_ms as f32 / den_ms as f32
}

fn skipped(fixture: &ManifestFixture, reason: impl Into<String>) -> EquivalenceResult {
    EquivalenceResult {
        fixture: fixture.name.clone(),
        language: fixture.language.clone(),
        synthetic_placeholder: fixture.synthetic_placeholder,
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
            ttff_ratio: None,
            ttc_ratio: None,
        },
        verdict: Verdict::Skipped,
        note: reason.into(),
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
            let frames: BoxStream<'static, StreamFrame> =
                UnboundedReceiverStream::new(rx).boxed();

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
                ttff_ratio: Some(0.3),
                ttc_ratio: Some(1.1),
            },
            verdict: Verdict::Pass,
            note: String::new(),
        };
        let report = EquivalenceReport {
            fono_version: env!("CARGO_PKG_VERSION").into(),
            stt_backend: "fake".into(),
            tier: "tier1".into(),
            threshold_levenshtein: TIER1_LEVENSHTEIN_THRESHOLD,
            results: vec![res],
        };
        let json = serde_json::to_string(&report).expect("serialize");
        let back: EquivalenceReport =
            serde_json::from_str(&json).expect("deserialize");
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
        };
        report.results.push(make_result("a", Verdict::Skipped));
        report.results.push(make_result("b", Verdict::Skipped));
        assert_eq!(report.overall_verdict(), Verdict::Skipped);
    }

    fn make_result(name: &str, v: Verdict) -> EquivalenceResult {
        EquivalenceResult {
            fixture: name.into(),
            language: "en".into(),
            synthetic_placeholder: false,
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
                ttff_ratio: None,
                ttc_ratio: None,
            },
            verdict: v,
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
}
