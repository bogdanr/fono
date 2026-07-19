// SPDX-License-Identifier: GPL-3.0-only
//! Speaker-verification back-end scoring (the cheap, model-independent half
//! of the "who is speaking" engine — Slice 2 of
//! `plans/2026-07-17-speaker-verification-v1.md`).
//!
//! An embedding model (a separate, feature-gated `ort` session — added in a
//! later slice) turns an utterance into a fixed-width `f32` vector. Everything
//! *after* that point — turning two embeddings into a comparable score — is
//! plain arithmetic and lives here so it can be unit-tested with no model,
//! no ONNX runtime, and no network. Research grounding (arXiv 2606.22369,
//! Kiwano) shows this inference-time back-end wins ~30 % relative EER for
//! free:
//!
//! - **Length-normalisation** ([`l2_normalize`]): project every embedding onto
//!   the unit sphere so a dot product *is* the cosine similarity.
//! - **Centering** ([`Cohort::center`]): subtract the impostor-cohort mean
//!   before normalising, removing the shared "session" direction that
//!   otherwise inflates every score.
//! - **AS-Norm** ([`Cohort::as_norm`]): adaptive symmetric score
//!   normalisation against a shipped impostor cohort (~200 embeddings,
//!   ~200 KB in the model pack), which rescales a raw cosine into a
//!   z-score-like quantity that is far more stable across channels and
//!   speakers than the raw cosine alone.
//!
//! The decision layer (threshold, min-speech accumulation) and the enrollment
//! store live elsewhere; this module only answers "how alike are these two
//! voices, calibrated against the cohort?".

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use realfft::{num_complex::Complex, RealFftPlanner, RealToComplex};

/// L2-normalise a vector in place (project onto the unit hypersphere). A
/// zero (or numerically tiny) vector is left untouched — there is no
/// meaningful direction to normalise.
pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Cosine similarity of two equal-length vectors, in `-1.0..=1.0`. Returns
/// `0.0` when the lengths differ or either vector is (near) zero, so callers
/// never divide by zero.
#[must_use]
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom > f32::EPSILON {
        dot / denom
    } else {
        0.0
    }
}

/// Leave-one-out consistency of a set of embeddings: for each embedding, its
/// cosine similarity to the mean of the *other* embeddings. A high score means
/// the utterance is typical of the set; a low (or negative) score flags an
/// outlier (a clip captured on a different mic, in noise, or of the wrong
/// speaker). This is deliberately **derived on demand** rather than stored,
/// because it changes whenever any utterance is added or removed.
///
/// With fewer than two embeddings there is nothing to compare against, so a
/// score of `1.0` is returned for each (treated as trivially consistent).
/// Empty input yields an empty vector.
#[must_use]
pub fn consistency_scores(embeddings: &[Vec<f32>]) -> Vec<f32> {
    let n = embeddings.len();
    if n < 2 {
        return vec![1.0; n];
    }
    let dim = embeddings[0].len();
    // Column-wise sum of all embeddings; each leave-one-out mean subtracts the
    // held-out row and divides by (n - 1).
    let mut sum = vec![0.0f32; dim];
    for e in embeddings {
        if e.len() == dim {
            for (s, x) in sum.iter_mut().zip(e.iter()) {
                *s += x;
            }
        }
    }
    let others = (n - 1) as f32;
    embeddings
        .iter()
        .map(|e| {
            if e.len() != dim {
                return 0.0;
            }
            let centroid: Vec<f32> =
                sum.iter().zip(e.iter()).map(|(s, x)| (s - x) / others).collect();
            cosine(e, &centroid)
        })
        .collect()
}

/// The L2-normalised mean of a set of already centred/normalised embeddings —
/// the canonical single-vector representation of an enrolled speaker, matching
/// the form [`decide`] expects as an [`EnrolledSpeaker`] centroid and the
/// calibration flow scores against. Rows whose width differs from the first are
/// dropped defensively. Returns an empty vector for empty input or when no rows
/// are usable.
#[must_use]
pub fn centroid(embeddings: &[Vec<f32>]) -> Vec<f32> {
    let Some(dim) = embeddings.first().map(Vec::len) else {
        return Vec::new();
    };
    let mut sum = vec![0.0f32; dim];
    let mut n = 0.0f32;
    for e in embeddings {
        if e.len() == dim {
            for (s, x) in sum.iter_mut().zip(e.iter()) {
                *s += x;
            }
            n += 1.0;
        }
    }
    if n == 0.0 {
        return Vec::new();
    }
    for s in &mut sum {
        *s /= n;
    }
    l2_normalize(&mut sum);
    sum
}

/// The shipped impostor cohort: a fixed set of embeddings from speakers who
/// are, by construction, *not* the user. Used both to centre embeddings and
/// to normalise scores (AS-Norm). Members are stored already L2-normalised.
#[derive(Debug, Clone, Default)]
pub struct Cohort {
    /// L2-normalised impostor embeddings (one per row).
    members: Vec<Vec<f32>>,
    /// Element-wise mean of the raw (pre-normalisation) cohort embeddings,
    /// used by [`Self::center`]. Same width as an embedding.
    mean: Vec<f32>,
}

impl Cohort {
    /// Build a cohort from raw impostor embeddings. The mean is computed from
    /// the raw vectors (for centering); the stored members are the centred +
    /// L2-normalised forms, matching how [`Self::as_norm`] expects to compare
    /// against them. Rows whose width does not match the first row are
    /// dropped defensively.
    #[must_use]
    pub fn from_raw(rows: &[Vec<f32>]) -> Self {
        let Some(dim) = rows.first().map(Vec::len) else {
            return Self::default();
        };
        let usable: Vec<&Vec<f32>> = rows.iter().filter(|r| r.len() == dim).collect();
        let mut mean = vec![0.0f32; dim];
        for row in &usable {
            for (m, x) in mean.iter_mut().zip(row.iter()) {
                *m += x;
            }
        }
        if !usable.is_empty() {
            let n = usable.len() as f32;
            for m in &mut mean {
                *m /= n;
            }
        }
        let members = usable
            .iter()
            .map(|row| {
                let mut centred: Vec<f32> =
                    row.iter().zip(mean.iter()).map(|(x, m)| x - m).collect();
                l2_normalize(&mut centred);
                centred
            })
            .collect();
        Self { members, mean }
    }

    /// Number of impostor embeddings in the cohort.
    #[must_use]
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Whether the cohort carries no members.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Centre and L2-normalise a raw embedding against the cohort mean,
    /// producing the canonical form used for scoring. When the cohort is
    /// empty (no mean available) the embedding is only length-normalised.
    #[must_use]
    pub fn center(&self, raw: &[f32]) -> Vec<f32> {
        let mut out: Vec<f32> = if self.mean.len() == raw.len() {
            raw.iter().zip(self.mean.iter()).map(|(x, m)| x - m).collect()
        } else {
            raw.to_vec()
        };
        l2_normalize(&mut out);
        out
    }

    /// The `top_k` highest cosine scores of `emb` against the cohort,
    /// descending. `emb` must already be centred/normalised
    /// (see [`Self::center`]).
    fn top_k_scores(&self, emb: &[f32], top_k: usize) -> Vec<f32> {
        let mut scores: Vec<f32> = self.members.iter().map(|m| cosine(emb, m)).collect();
        scores.sort_unstable_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(top_k.min(scores.len()));
        scores
    }

    /// Adaptive Symmetric Normalisation of a raw cosine `raw_score` between an
    /// enrolment embedding and a test embedding, both already centred/
    /// normalised. `top_k` is the impostor-cohort size to select per side
    /// (~200–300 in the literature; clamped to the cohort length).
    ///
    /// The normalised score is
    /// `0.5 * ((s - μ_e)/σ_e + (s - μ_t)/σ_t)`, where `(μ, σ)` are the mean
    /// and standard deviation of each side's top-k impostor cosines. This
    /// rescales the raw cosine into a channel-robust z-score-like quantity.
    /// When the cohort is empty the raw score is returned unchanged.
    #[must_use]
    pub fn as_norm(&self, enroll: &[f32], test: &[f32], raw_score: f32, top_k: usize) -> f32 {
        if self.members.is_empty() {
            return raw_score;
        }
        let (mu_e, sd_e) = mean_std(&self.top_k_scores(enroll, top_k));
        let (mu_t, sd_t) = mean_std(&self.top_k_scores(test, top_k));
        let ze = (raw_score - mu_e) / sd_e;
        let zt = (raw_score - mu_t) / sd_t;
        0.5 * (ze + zt)
    }

    /// AS-Norm scores of every cohort member against `centroid`, each member
    /// treated as an impostor "test" embedding — in the same space as
    /// [`Self::as_norm`]. Lets the calibration ("test my voice") flow build an
    /// impostor score distribution from the shipped cohort with **no** impostor
    /// audio upload. `centroid` must already be centred/normalised (see
    /// [`Self::center`]); members already are. Empty when the cohort is empty.
    #[must_use]
    pub fn impostor_scores(&self, centroid: &[f32], top_k: usize) -> Vec<f32> {
        self.members.iter().map(|m| self.as_norm(centroid, m, cosine(centroid, m), top_k)).collect()
    }
}

/// Mean and (population) standard deviation of a slice. The standard
/// deviation is floored at [`f32::EPSILON`] so callers never divide by zero
/// on a degenerate (all-equal or single-element) cohort slice.
fn mean_std(xs: &[f32]) -> (f32, f32) {
    if xs.is_empty() {
        return (0.0, 1.0);
    }
    let n = xs.len() as f32;
    let mean = xs.iter().sum::<f32>() / n;
    let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / n;
    (mean, var.sqrt().max(f32::EPSILON))
}

/// Assumed sample rate of every mono PCM buffer flowing through the speaker
/// path (enrolment, verification, accumulation). The engine and the [`Fbank`]
/// front-end both assume this rate by construction.
pub const SAMPLE_RATE: u32 = 16_000;

/// Default per-side impostor-cohort size for [`Cohort::as_norm`] (the "top-k"
/// of the AS-Norm literature, typically 200–300). Clamped to the cohort
/// length internally, so a smaller shipped cohort is harmless.
pub const DEFAULT_AS_NORM_TOP_K: usize = 200;

/// One enrolled speaker reduced to a single centred, length-normalised
/// centroid embedding (the mean of that speaker's enrolment utterances, run
/// through [`Cohort::center`]). The store/consumer builds these; the decision
/// layer scores against them.
#[derive(Debug, Clone)]
pub struct EnrolledSpeaker {
    /// Human-readable speaker name (the store's display label).
    pub name: String,
    /// Centred + L2-normalised centroid embedding.
    pub embedding: Vec<f32>,
}

/// The outcome of comparing one test utterance against the enrolled speakers.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerDecision {
    /// Best-matching enrolled speaker whose score cleared the threshold, or
    /// `None` when nobody matched (an unknown / rejected speaker).
    pub name: Option<String>,
    /// AS-Norm score of the best candidate (whether or not it matched), kept
    /// for logging and calibration. `0.0` when there were no candidates.
    pub score: f32,
    /// Confidence in `0.0..=1.0`: a logistic of the score's margin over the
    /// threshold — `0.5` exactly at the threshold, →1 well above, →0 well
    /// below. Model-independent and monotone in the score.
    pub confidence: f32,
    /// Whether at least `min_speech_secs` of audio backed this decision. When
    /// `false` the score is provisional — the caller kept accumulating audio.
    pub sufficient_audio: bool,
}

/// Logistic confidence from an AS-Norm score's margin over the decision
/// threshold: `0.5` at the threshold, saturating towards `0`/`1` away from it.
#[must_use]
fn confidence_from_margin(score: f32, threshold: f32) -> f32 {
    1.0 / (1.0 + (-(score - threshold)).exp())
}

/// Score a centred/normalised `test` embedding against every enrolled speaker
/// with AS-Norm and return the best [`SpeakerDecision`]. `name` is `Some` only
/// when the winning score reaches `threshold`. Both `candidates` and `test`
/// must already be centred/normalised (see [`Cohort::center`]);
/// `sufficient_audio` is threaded through from the [`SpeechAccumulator`]. With
/// no candidates the result is an empty reject.
#[must_use]
pub fn decide(
    candidates: &[EnrolledSpeaker],
    test: &[f32],
    cohort: &Cohort,
    threshold: f32,
    sufficient_audio: bool,
) -> SpeakerDecision {
    let best = candidates
        .iter()
        .map(|c| {
            let raw = cosine(&c.embedding, test);
            (c, cohort.as_norm(&c.embedding, test, raw, DEFAULT_AS_NORM_TOP_K))
        })
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    match best {
        Some((c, score)) => {
            let name = (score >= threshold).then(|| c.name.clone());
            let confidence = confidence_from_margin(score, threshold);
            SpeakerDecision { name, score, confidence, sufficient_audio }
        }
        None => SpeakerDecision { name: None, score: 0.0, confidence: 0.0, sufficient_audio },
    }
}

/// Accumulates mono PCM across the wake phrase and the following command until
/// `min_speech_secs` of audio has been gathered, so the verification decision
/// is made on enough voice. Short commands keep accumulating until the minimum
/// is met.
#[derive(Debug, Clone)]
pub struct SpeechAccumulator {
    samples: Vec<f32>,
    min_samples: usize,
}

impl SpeechAccumulator {
    /// New accumulator requiring `min_speech_secs` of 16 kHz audio before
    /// [`Self::is_sufficient`] turns true. Negative values clamp to zero.
    #[must_use]
    pub fn new(min_speech_secs: f32) -> Self {
        let min_samples = (min_speech_secs.max(0.0) * SAMPLE_RATE as f32).ceil() as usize;
        Self { samples: Vec::new(), min_samples }
    }

    /// Append a chunk of 16 kHz mono PCM.
    pub fn push(&mut self, pcm: &[f32]) {
        self.samples.extend_from_slice(pcm);
    }

    /// Seconds of audio accumulated so far.
    #[must_use]
    pub fn seconds(&self) -> f32 {
        self.samples.len() as f32 / SAMPLE_RATE as f32
    }

    /// Whether at least `min_speech_secs` has accumulated.
    #[must_use]
    pub fn is_sufficient(&self) -> bool {
        self.samples.len() >= self.min_samples
    }

    /// The accumulated PCM, fed to the engine to embed.
    #[must_use]
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Drop the accumulated audio, keeping the configured minimum.
    pub fn clear(&mut self) {
        self.samples.clear();
    }
}

/// Default false-accept-rate target for the operating-point threshold in
/// [`calibrate`]. 1 % is a sensible desktop-dictation default: a strict-ish
/// point that keeps impostor acceptances rare without being paranoid. The
/// EER threshold is reported alongside as the balanced alternative.
pub const DEFAULT_TARGET_FAR: f32 = 0.01;

/// The outcome of a "test my voice" run: the genuine and impostor score
/// distributions, the equal-error-rate estimate, and two candidate operating
/// thresholds (balanced EER point and a target-FAR point). All scores are
/// AS-Norm outputs on the user's own mic/room, so this is a *practical*
/// self-EER ("your mic, your room"), not a benchmark figure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CalibrationReport {
    /// Mean of the genuine-trial scores (self vs own centroid).
    pub genuine_mean: f32,
    /// Standard deviation of the genuine-trial scores (population, ddof=0).
    pub genuine_std: f32,
    /// Number of genuine trials scored.
    pub genuine_trials: usize,
    /// Mean of the impostor-trial scores (self vs cohort / other speakers).
    pub impostor_mean: f32,
    /// Standard deviation of the impostor-trial scores.
    pub impostor_std: f32,
    /// Number of impostor trials scored.
    pub impostor_trials: usize,
    /// Equal-error-rate estimate in `[0, 1]` (where FAR ≈ FRR).
    pub eer: f32,
    /// Score threshold at the equal-error point (balanced FAR/FRR).
    pub eer_threshold: f32,
    /// The false-accept-rate target used for [`Self::far_threshold`].
    pub target_far: f32,
    /// Score threshold achieving `target_far` on the impostor distribution.
    pub far_threshold: f32,
}

/// Population mean and standard deviation (ddof = 0) of a score array, for
/// calibration *reporting*. Unlike the private AS-Norm [`mean_std`] helper this
/// does not floor the std, so a constant distribution reports `std = 0`, and an
/// empty slice is `(0, 0)`.
#[must_use]
pub fn score_mean_std(xs: &[f32]) -> (f32, f32) {
    if xs.is_empty() {
        return (0.0, 0.0);
    }
    let n = xs.len() as f32;
    let mean = xs.iter().sum::<f32>() / n;
    let var = xs.iter().map(|&x| (x - mean) * (x - mean)).sum::<f32>() / n;
    (mean, var.sqrt())
}

/// The equal-error-rate estimate and its threshold from genuine and impostor
/// score arrays. A trial is *accepted* when its score is `>=` the threshold, so
/// as the threshold rises the false-accept rate (impostors accepted) falls and
/// the false-reject rate (genuines rejected) rises; the EER is where the two
/// cross. Sweeps every observed score as a candidate threshold and returns the
/// point of minimal `|FAR − FRR|`. Returns `(0.5, 0.0)` if either side is empty.
#[must_use]
pub fn eer_and_threshold(genuine: &[f32], impostor: &[f32]) -> (f32, f32) {
    if genuine.is_empty() || impostor.is_empty() {
        return (0.5, 0.0);
    }
    let g = genuine.len() as f32;
    let i = impostor.len() as f32;
    // Candidate thresholds: every observed score (both sides), ascending.
    let mut cands: Vec<f32> = genuine.iter().chain(impostor.iter()).copied().collect();
    cands.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    cands.dedup();
    let mut best_eer = 1.0;
    let mut best_thr = cands[0];
    let mut best_gap = f32::INFINITY;
    for &t in &cands {
        let far = impostor.iter().filter(|&&s| s >= t).count() as f32 / i;
        let frr = genuine.iter().filter(|&&s| s < t).count() as f32 / g;
        let gap = (far - frr).abs();
        if gap < best_gap {
            best_gap = gap;
            best_eer = 0.5 * (far + frr);
            best_thr = t;
        }
    }
    (best_eer, best_thr)
}

/// The lowest threshold whose false-accept rate on `impostor` is `<=`
/// `target_far` — i.e. the operating point that keeps impostor acceptances at
/// or below the target. Equivalent to the `1 − target_far` quantile of the
/// impostor scores. Returns `0.0` for an empty impostor set.
#[must_use]
pub fn threshold_for_far(impostor: &[f32], target_far: f32) -> f32 {
    if impostor.is_empty() {
        return 0.0;
    }
    let target = target_far.clamp(0.0, 1.0);
    let mut sorted = impostor.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    // Smallest threshold t (chosen from observed scores, plus one above the max)
    // with (#impostor >= t)/n <= target. Scanning high→low, stop just before the
    // accepted fraction exceeds the target.
    let mut thr = sorted[n - 1] + f32::EPSILON.max(sorted[n - 1].abs() * 1e-6);
    for k in (0..n).rev() {
        let accepted = (n - k) as f32 / n as f32;
        if accepted <= target {
            thr = sorted[k];
        } else {
            break;
        }
    }
    thr
}

/// Compute a full [`CalibrationReport`] from genuine and impostor score arrays
/// using `target_far` for the strict operating point.
#[must_use]
pub fn calibrate(genuine: &[f32], impostor: &[f32], target_far: f32) -> CalibrationReport {
    let (genuine_mean, genuine_std) = score_mean_std(genuine);
    let (impostor_mean, impostor_std) = score_mean_std(impostor);
    let (eer, eer_threshold) = eer_and_threshold(genuine, impostor);
    let far_threshold = threshold_for_far(impostor, target_far);
    CalibrationReport {
        genuine_mean,
        genuine_std,
        genuine_trials: genuine.len(),
        impostor_mean,
        impostor_std,
        impostor_trials: impostor.len(),
        eer,
        eer_threshold,
        target_far,
        far_threshold,
    }
}

/// Summary statistics for per-embedding latency measurements (milliseconds),
/// surfaced by "test my voice" as "≈X ms/utterance on this machine".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatencyStats {
    pub count: usize,
    pub mean_ms: f32,
    pub p50_ms: f32,
    pub p95_ms: f32,
}

/// Mean / median / 95th-percentile of `samples_ms` (nearest-rank percentiles).
/// All-zero for an empty slice.
#[must_use]
pub fn latency_stats(samples_ms: &[f32]) -> LatencyStats {
    if samples_ms.is_empty() {
        return LatencyStats { count: 0, mean_ms: 0.0, p50_ms: 0.0, p95_ms: 0.0 };
    }
    let n = samples_ms.len();
    let mean_ms = samples_ms.iter().sum::<f32>() / n as f32;
    let mut sorted = samples_ms.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |p: f32| {
        let rank = (p * n as f32).ceil() as usize;
        sorted[rank.clamp(1, n) - 1]
    };
    LatencyStats { count: n, mean_ms, p50_ms: pct(0.5), p95_ms: pct(0.95) }
}

/// Front-end filterbank configuration a speaker-embedding model expects. The
/// engine (feature `speaker-onnx`, a later slice) turns 16 kHz mono PCM into
/// these features before the `ort` session runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FbankConfig {
    pub sample_rate: u32,
    pub n_mels: usize,
    pub frame_length_ms: f32,
    pub frame_shift_ms: f32,
    /// Low band edge of the mel filterbank (Hz).
    pub f_min_hz: f32,
    /// High band edge of the mel filterbank (Hz).
    pub f_max_hz: f32,
}

/// SHA-256 sentinel for assets without a pinned digest yet (same convention
/// and value as `wake_registry::UNPINNED` and `fono_stt::registry::UNPINNED`).
/// The downloader logs the computed hash and accepts the file; tighten to a
/// real pin once the artifact is hosted on the mirror.
pub const UNPINNED: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// ONNX Runtime version the speaker `.ort` assets are converted for. Must
/// match the linked runtime ABI (ADR 0032) — the same value the voice/wake
/// stacks and `scripts/fetch-onnxruntime.sh` pin.
pub const ORT_VERSION: &str = "1.24.2";

/// Release tag the speaker assets live under on the mirror, named for the ONNX
/// Runtime ABI (ADR 0033), e.g. `ort-1.24.2`.
pub const RELEASE_TAG: &str = "ort-1.24.2";

/// Default mirror base URL: the `fono-voice` release-download root — the same
/// mirror the voices and wake models use (ADR 0033). Override via the
/// `base_url` argument to [`fetch_model`] for forks / self-hosting / a CDN.
pub const DEFAULT_BASE_URL: &str = "https://github.com/bogdanr/fono-voice/releases/download";

/// `true` for a real 64-hex SHA-256 pin (i.e. not the all-zeros [`UNPINNED`]
/// sentinel). An unpinned asset is treated as not-yet-hosted: unfetchable, and
/// its cohort (if any) is reported absent so AS-Norm degrades to plain cosine.
#[must_use]
pub fn is_pinned(sha256: &str) -> bool {
    sha256.len() == 64 && !sha256.chars().all(|c| c == '0')
}

/// One downloadable file in a model pack: its on-disk basename (also the
/// mirror asset name), the release tag it lives under, and its pinned hash.
/// The full URL is built at fetch time via [`asset_url`] from a base URL + the
/// tag + the file — mirroring `wake_registry::WakeAsset` — so forks and
/// self-hosting only swap the base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeakerAsset {
    /// Local cache basename, also the mirror asset name (e.g.
    /// `redimnet2-b3.ort`, `redimnet2-b3.cohort.bin`).
    pub file: &'static str,
    /// Release tag the asset lives under in the mirror (e.g. `ort-1.24.2`).
    pub release_tag: &'static str,
    /// Lowercase-hex SHA-256, or [`UNPINNED`] for a not-yet-hosted artifact.
    pub sha256: &'static str,
}

/// One entry in the speaker-embedding model registry: a named model plus the
/// metadata the engine and UI need, and the SHA-256-pinned download pack (the
/// `.ort` embedding graph + the AS-Norm impostor-cohort sidecar). Deliberately
/// **no tier/`auto` ladder** (see the plan's "Model selection" section) —
/// models are additive rows.
///
/// The `.ort` graphs and cohort sidecars are hosted on the `fono-voice` mirror
/// under [`RELEASE_TAG`]; until an asset is uploaded its `sha256` stays
/// [`UNPINNED`] (unfetchable), exactly like the wake registry's pending
/// entries. The known, model-intrinsic metadata below is always valid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpeakerModel {
    /// Registry key, matched against `[speaker].model`.
    pub name: &'static str,
    /// Short human description for the web UI picker.
    pub description: &'static str,
    /// Parameter count in millions (a size/CPU hint for the UI).
    pub params_millions: f32,
    /// Embedding width (the `f32` vector length the model emits).
    pub embedding_dim: usize,
    /// Front-end filterbank the model expects.
    pub fbank: FbankConfig,
    /// The `.ort` embedding graph asset.
    pub graph: SpeakerAsset,
    /// The AS-Norm impostor-cohort sidecar asset (`<name>.cohort.bin`).
    pub cohort: SpeakerAsset,
}

/// Default registry model name (see `[speaker].model`). ReDimNet2-B3 is the
/// efficiency sweet spot (4.1 M params, ~2.7 GMACs, MIT) and already beats the
/// original ReDimNet-B6 on clean EER at roughly a quarter of the parameters.
pub const DEFAULT_MODEL: &str = "redimnet2-b3";

/// Front-end shared by the whole ReDimNet2 family (`feat_type='tf'`): 16 kHz,
/// 25 ms / 10 ms framing, 72 mel bands over 20–7600 Hz.
const REDIMNET2_FBANK: FbankConfig = FbankConfig {
    sample_rate: 16_000,
    n_mels: 72,
    frame_length_ms: 25.0,
    frame_shift_ms: 10.0,
    f_min_hz: 20.0,
    f_max_hz: 7600.0,
};

/// Built-in speaker-embedding model registry. Additive rows, no schema change.
/// Both tiers share one graph operator set and a 192-d embedding, so a single
/// hosted runtime serves either.
const REGISTRY: &[SpeakerModel] = &[
    SpeakerModel {
        name: "redimnet2-b3",
        description: "ReDimNet2-B3 — 4.1M params, robust mixture-trained (MIT)",
        params_millions: 4.1,
        embedding_dim: 192,
        fbank: REDIMNET2_FBANK,
        // Hosted on the mirror (ort-1.24.2 release); pinned from the published
        // .ort bytes (byte-identical to the local torch→onnx→ort conversion on
        // onnxruntime 1.24.2).
        graph: SpeakerAsset {
            file: "redimnet2-b3.ort",
            release_tag: RELEASE_TAG,
            sha256: "7bb30475c5924b525b6684a00ff768aa9185f69e6265d5ff9653a48d70eee0e2",
        },
        // Impostor cohort: 600 Common Voice (CC0) speakers embedded through
        // this graph; selection pinned in calibration/speaker-cohort/selection.tsv.
        cohort: SpeakerAsset {
            file: "redimnet2-b3.cohort.bin",
            release_tag: RELEASE_TAG,
            sha256: "3562c1c29c0e11ddbb4c72d392e104aeab1192e650bdd12d16214af3f52bc091",
        },
    },
    SpeakerModel {
        name: "redimnet2-b6",
        description: "ReDimNet2-B6 — 12.3M params, max accuracy (MIT)",
        params_millions: 12.3,
        embedding_dim: 192,
        fbank: REDIMNET2_FBANK,
        // Hosted on the mirror (ort-1.24.2 release); pinned from the published
        // .ort bytes.
        graph: SpeakerAsset {
            file: "redimnet2-b6.ort",
            release_tag: RELEASE_TAG,
            sha256: "903015c8f462c98b28ec361f2a774bd00b3acbb4086b83b8c3b04824cd26f087",
        },
        // Same 600-speaker Common Voice cohort, embedded through the b6 graph.
        cohort: SpeakerAsset {
            file: "redimnet2-b6.cohort.bin",
            release_tag: RELEASE_TAG,
            sha256: "ee2877a2f56ad7d04f9e6b9011f89cd3bca0d11889f407d8175aecc55b8f6d5c",
        },
    },
];

/// All registry models.
#[must_use]
pub fn registry() -> &'static [SpeakerModel] {
    REGISTRY
}

/// Look up a registry model by name.
#[must_use]
pub fn model(name: &str) -> Option<&'static SpeakerModel> {
    REGISTRY.iter().find(|m| m.name == name)
}

/// Build the full download URL for an asset: `{base}/{release_tag}/{file}`.
/// Identical join to `wake_registry::asset_url` / `fono_tts::voices::asset_url`.
#[must_use]
pub fn asset_url(base_url: &str, release_tag: &str, file: &str) -> String {
    format!("{}/{}/{}", base_url.trim_end_matches('/'), release_tag, file)
}

/// Cache directory speaker-model assets live in, mirroring the `models/<kind>`
/// convention used by the STT, voice, and wake-word model dirs.
#[must_use]
pub fn speaker_dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join("models").join("speaker")
}

/// Resolved on-disk paths for a fetched speaker model: the `.ort` graph and,
/// once its sidecar is hosted, the AS-Norm impostor cohort. Returned by
/// [`fetch_model`] and computable offline via [`resolved_paths`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSpeakerModel {
    /// The embedding graph `.ort` file.
    pub graph: PathBuf,
    /// The cohort sidecar, present only once the asset is hosted/pinned;
    /// `None` degrades AS-Norm to plain cosine scoring.
    pub cohort: Option<PathBuf>,
}

/// Compute where a model's files would live on disk, without downloading.
/// Returns `None` for an unknown name. The cohort path is reported only when
/// its asset is pinned (hosted); an unpinned cohort yields `None` so callers
/// treat AS-Norm as unavailable.
#[must_use]
pub fn resolved_paths(name: &str, cache_dir: &Path) -> Option<ResolvedSpeakerModel> {
    let m = model(name)?;
    let dir = speaker_dir(cache_dir);
    Some(ResolvedSpeakerModel {
        graph: dir.join(m.graph.file),
        cohort: is_pinned(m.cohort.sha256).then(|| dir.join(m.cohort.file)),
    })
}

/// Fetch and verify a speaker model by name: the `.ort` embedding graph
/// (required) and, when hosted, its AS-Norm cohort sidecar. Assets are
/// downloaded into the speaker cache dir via [`fono_download::download`] and
/// checked against the pinned SHA-256; a cached file whose hash already
/// matches is reused (no network). `base_url` overrides [`DEFAULT_BASE_URL`]
/// (forks / self-hosting / CDN); pass `None` for the default mirror.
///
/// The cohort is fetched only when its asset is pinned; an unpinned (not yet
/// hosted) cohort is skipped and reported as `None`, leaving AS-Norm to
/// degrade to plain cosine scoring rather than failing the whole fetch.
pub async fn fetch_model(
    name: &str,
    cache_dir: &Path,
    base_url: Option<&str>,
) -> Result<ResolvedSpeakerModel> {
    let m = model(name).with_context(|| format!("unknown speaker model '{name}'"))?;
    let base = base_url.unwrap_or(DEFAULT_BASE_URL);
    let dir = speaker_dir(cache_dir);
    let graph = Box::pin(fetch_asset(&m.graph, base, &dir)).await?;
    let cohort = if is_pinned(m.cohort.sha256) {
        Some(Box::pin(fetch_asset(&m.cohort, base, &dir)).await?)
    } else {
        tracing::debug!(model = name, "cohort sidecar not hosted yet; AS-Norm disabled");
        None
    };
    Ok(ResolvedSpeakerModel { graph, cohort })
}

/// Fetch one asset into `dir`, reusing a cached copy whose SHA-256 already
/// matches. Returns the absolute path to the verified file. Mirrors
/// `wake_registry::fetch_asset`.
async fn fetch_asset(asset: &SpeakerAsset, base_url: &str, dir: &Path) -> Result<PathBuf> {
    let dest = dir.join(asset.file);
    if dest.is_file() && is_pinned(asset.sha256) {
        let actual = fono_download::sha256_file(&dest)
            .await
            .with_context(|| format!("hash cached {}", dest.display()))?;
        if actual.eq_ignore_ascii_case(asset.sha256) {
            tracing::debug!("speaker asset {} present and verified (cache hit)", asset.file);
            return Ok(dest);
        }
        tracing::warn!("cached {} failed checksum; re-downloading", dest.display());
    }
    let url = asset_url(base_url, asset.release_tag, asset.file);
    fono_download::download(&url, &dest, asset.sha256)
        .await
        .with_context(|| format!("download speaker asset {url}"))?;
    Ok(dest)
}

/// Pre-emphasis coefficient (0.97, as ReDimNet2's front-end).
const PRE_EMPHASIS: f32 = 0.97;
/// Small floor added before the log so silent frames stay finite.
const LOG_FLOOR: f32 = 1e-10;

fn hz_to_mel(f: f32) -> f32 {
    1127.0 * (f / 700.0).ln_1p()
}

fn mel_to_hz(m: f32) -> f32 {
    700.0 * (m / 1127.0).exp_m1()
}

/// Symmetric Hann window of length `n` (denominator `n-1`), matching
/// ReDimNet2's `feat_type='tf'` front-end.
fn hann_window(n: usize) -> Vec<f32> {
    if n <= 1 {
        return vec![1.0; n];
    }
    let denom = (n - 1) as f32;
    (0..n)
        .map(|i| 0.5f32.mul_add(-(2.0 * std::f32::consts::PI * i as f32 / denom).cos(), 0.5))
        .collect()
}

/// Log-mel filterbank ("fbank") front-end: 16 kHz mono PCM → per-frame
/// log-mel features (72 bands for ReDimNet2) with per-utterance cepstral mean
/// normalisation (CMN). Parameterised by [`FbankConfig`] (from the registry).
///
/// Reconciled to ReDimNet2's `feat_type='tf'` front-end: symmetric Hann
/// window, 0.97 pre-emphasis, HTK mel (`2595·log10(1+f/700)`), **power**
/// spectrum (`real²+imag²`, no sqrt), natural log, CMN. Two residual quirks of
/// the upstream graph are deferred to the Slice 5 Python-oracle cross-check
/// before they matter: it frames the DFT over `nfft/2` (256) truncated bins
/// with `linspace(0, sr/2, 256)` mel spacing (vs the standard `nfft/2+1`
/// rfft bins here), and applies a per-signal mean/std normalisation upstream
/// of pre-emphasis.
pub struct Fbank {
    n_mels: usize,
    frame_len: usize,
    frame_shift: usize,
    window: Vec<f32>,
    /// Per-filter sparse triangular weights: `(fft_bin, weight)`.
    filters: Vec<Vec<(usize, f32)>>,
    /// Centre frequency (Hz) of each mel filter (diagnostics / tests).
    centers: Vec<f32>,
    plan: Arc<dyn RealToComplex<f32>>,
}

impl Fbank {
    /// Build a front-end for the given filterbank config.
    #[must_use]
    pub fn new(cfg: FbankConfig) -> Self {
        let sr = cfg.sample_rate as f32;
        let frame_len = (cfg.frame_length_ms / 1000.0 * sr).round() as usize;
        let frame_shift = (cfg.frame_shift_ms / 1000.0 * sr).round() as usize;
        let nfft = frame_len.next_power_of_two().max(1);
        let window = hann_window(frame_len);
        let (filters, centers) =
            mel_filterbank(cfg.n_mels, nfft, cfg.sample_rate, cfg.f_min_hz, cfg.f_max_hz);
        let plan = RealFftPlanner::<f32>::new().plan_fft_forward(nfft);
        Self { n_mels: cfg.n_mels, frame_len, frame_shift, window, filters, centers, plan }
    }

    /// Number of mel bands.
    #[must_use]
    pub fn n_mels(&self) -> usize {
        self.n_mels
    }

    /// Compute CMN-normalised log-mel features as `[n_frames][n_mels]`.
    /// Returns empty when the input is shorter than one frame.
    #[must_use]
    pub fn compute(&self, samples: &[f32]) -> Vec<Vec<f32>> {
        let mut feats = self.log_mel_frames(samples);
        apply_cmn(&mut feats, self.n_mels);
        feats
    }

    /// Log-mel features **without** CMN (the raw per-frame spectra). Split out
    /// so tests can inspect spectral localisation before mean-subtraction
    /// flattens a stationary tone.
    fn log_mel_frames(&self, samples: &[f32]) -> Vec<Vec<f32>> {
        if samples.len() < self.frame_len {
            return Vec::new();
        }
        let n_frames = 1 + (samples.len() - self.frame_len) / self.frame_shift;
        let mut input = self.plan.make_input_vec();
        let mut output = self.plan.make_output_vec();
        let mut feats = Vec::with_capacity(n_frames);
        for f in 0..n_frames {
            let start = f * self.frame_shift;
            let frame = &samples[start..start + self.frame_len];
            input.fill(0.0);
            for i in 0..self.frame_len {
                let pre = if i == 0 {
                    frame[0] * (1.0 - PRE_EMPHASIS)
                } else {
                    PRE_EMPHASIS.mul_add(-frame[i - 1], frame[i])
                };
                input[i] = pre * self.window[i];
            }
            // realfft overwrites the input scratch; a fresh plan call per frame
            // is fine at utterance cadence (event-driven, not continuous).
            let _ = self.plan.process(&mut input, &mut output);
            feats.push(self.mel_energies(&output));
        }
        feats
    }

    fn mel_energies(&self, spectrum: &[Complex<f32>]) -> Vec<f32> {
        self.filters
            .iter()
            .map(|filt| {
                let e: f32 = filt.iter().map(|&(bin, w)| spectrum[bin].norm_sqr() * w).sum();
                (e + LOG_FLOOR).ln()
            })
            .collect()
    }

    /// Mel filter centre frequencies (Hz), for diagnostics / tests.
    #[must_use]
    pub fn centers(&self) -> &[f32] {
        &self.centers
    }
}

/// Build `n_mels` triangular mel filters over the `nfft/2 + 1` FFT bins,
/// returning each filter's sparse `(bin, weight)` list and its centre Hz.
fn mel_filterbank(
    n_mels: usize,
    nfft: usize,
    sample_rate: u32,
    low_hz: f32,
    high_hz: f32,
) -> (Vec<Vec<(usize, f32)>>, Vec<f32>) {
    let n_bins = nfft / 2 + 1;
    let sr = sample_rate as f32;
    let mel_low = hz_to_mel(low_hz);
    let mel_high = hz_to_mel(high_hz);
    // n_mels + 2 equally-spaced mel points => n_mels triangles.
    let points: Vec<f32> = (0..n_mels + 2)
        .map(|i| mel_to_hz(mel_low + (mel_high - mel_low) * i as f32 / (n_mels + 1) as f32))
        .collect();
    let bin_hz = sr / nfft as f32;
    let mut filters = Vec::with_capacity(n_mels);
    let mut centers = Vec::with_capacity(n_mels);
    for m in 1..=n_mels {
        let (left, center, right) = (points[m - 1], points[m], points[m + 1]);
        centers.push(center);
        let mut filt = Vec::new();
        for k in 0..n_bins {
            let f = k as f32 * bin_hz;
            let w = if f >= left && f <= center && center > left {
                (f - left) / (center - left)
            } else if f > center && f <= right && right > center {
                (right - f) / (right - center)
            } else {
                0.0
            };
            if w > 0.0 {
                filt.push((k, w));
            }
        }
        filters.push(filt);
    }
    (filters, centers)
}

/// Subtract each mel dimension's mean across all frames (per-utterance CMN).
fn apply_cmn(feats: &mut [Vec<f32>], n_mels: usize) {
    if feats.is_empty() {
        return;
    }
    let n = feats.len() as f32;
    for d in 0..n_mels {
        let mean = feats.iter().map(|f| f[d]).sum::<f32>() / n;
        for f in feats.iter_mut() {
            f[d] -= mean;
        }
    }
}

/// ONNX embedding engine (feature `speaker-onnx`). Turns 16 kHz mono PCM into
/// a centred, length-normalised speaker embedding via an `ort` session, ready
/// for [`cosine`] / [`Cohort::as_norm`] scoring.
///
/// The graph itself (a ReDimNet-family `.ort` export) and its impostor cohort
/// ship in the model pack wired up in Slice 1; this type is the generic
/// runtime around whatever pack is loaded. Exact numerical parity with the
/// Python oracle is asserted in Slice 5.
#[cfg(feature = "speaker-onnx")]
pub mod engine {
    use std::path::Path;

    use anyhow::Result;
    use ort::session::builder::GraphOptimizationLevel;
    use ort::session::Session;
    use ort::value::Tensor;

    use super::Cohort;

    /// A loaded speaker-embedding model plus its impostor cohort. The
    /// ReDimNet2 `.ort` export takes **raw 16 kHz mono waveform** and computes
    /// its mel front-end inside the graph, so no external fbank is needed here.
    pub struct SpeakerEngine {
        session: Session,
        input_name: String,
        output_name: String,
        cohort: Cohort,
    }

    impl SpeakerEngine {
        /// Load an embedding graph from `model_path`, pairing it with the
        /// impostor `cohort` from the same pack.
        pub fn load(model_path: &Path, cohort: Cohort) -> Result<Self> {
            // Idempotent process-wide ONNX Runtime env (mirrors wakeword/tts).
            let _ = ort::init().with_name("fono").commit();
            let session = Session::builder()
                .map_err(|e| anyhow::anyhow!("create ort session builder: {e}"))?
                // `.ort` models are pre-optimised and the minimal runtime has
                // the optimiser compiled out, so setting a level errors —
                // recover the builder (the documented `ort` minimal idiom).
                .with_optimization_level(GraphOptimizationLevel::Disable)
                .unwrap_or_else(ort::Error::recover)
                .with_intra_threads(1)
                .map_err(|e| anyhow::anyhow!("set intra-op threads: {e}"))?
                .commit_from_file(model_path)
                .map_err(|e| anyhow::anyhow!("load speaker model {}: {e}", model_path.display()))?;
            let input_name = session
                .inputs()
                .first()
                .map_or_else(|| "waveform".to_string(), |i| i.name().to_string());
            let output_name = session
                .outputs()
                .first()
                .map_or_else(|| "embs".to_string(), |o| o.name().to_string());
            Ok(Self { session, input_name, output_name, cohort })
        }

        /// The impostor cohort shipped with this model (for scoring).
        #[must_use]
        pub fn cohort(&self) -> &Cohort {
            &self.cohort
        }

        /// Embed 16 kHz mono PCM into a centred, length-normalised embedding.
        /// Returns `None` when the audio is too short for even one mel frame.
        ///
        /// The graph's input is the **raw waveform** shaped `[1, T]` (rank 2);
        /// the mel front-end runs inside the model. Exact numerical parity with
        /// the Python oracle is asserted in Slice 5.
        pub fn embed(&mut self, samples: &[f32]) -> Result<Option<Vec<f32>>> {
            // Need at least one 25 ms frame (400 samples at 16 kHz).
            if samples.len() < 400 {
                return Ok(None);
            }
            let tensor = Tensor::from_array((vec![1_i64, samples.len() as i64], samples.to_vec()))
                .map_err(|e| anyhow::anyhow!("build speaker waveform tensor: {e}"))?;
            let outputs = self
                .session
                .run(ort::inputs![self.input_name.as_str() => tensor])
                .map_err(|e| anyhow::anyhow!("run speaker embedding: {e}"))?;
            let (_shape, data) = outputs[self.output_name.as_str()]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow::anyhow!("extract speaker embedding: {e}"))?;
            // center() subtracts the cohort mean and L2-normalises.
            Ok(Some(self.cohort.center(data)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_unit_length() {
        let mut v = vec![3.0, 4.0];
        l2_normalize(&mut v);
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_zero_is_noop() {
        let mut v = vec![0.0, 0.0, 0.0];
        l2_normalize(&mut v);
        assert_eq!(v, vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn default_model_is_in_registry() {
        let m = model(DEFAULT_MODEL).expect("default model present");
        assert_eq!(m.name, "redimnet2-b3");
        assert_eq!(m.embedding_dim, 192);
        assert_eq!(m.fbank.sample_rate, 16_000);
        assert_eq!(m.fbank.n_mels, 72);
    }

    #[test]
    fn unknown_model_is_none() {
        assert!(model("no-such-model").is_none());
        assert!(!registry().is_empty());
    }

    fn default_fbank() -> Fbank {
        Fbank::new(model(DEFAULT_MODEL).unwrap().fbank)
    }

    fn tone(freq: f32, secs: f32, sr: u32) -> Vec<f32> {
        let n = (secs * sr as f32) as usize;
        (0..n)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * freq * i as f32 / sr as f32).sin())
            .collect()
    }

    #[test]
    fn fbank_frame_count_and_dims() {
        let fb = default_fbank();
        // 1 s @ 16 kHz, 25 ms frame (400) / 10 ms shift (160):
        // 1 + (16000 - 400) / 160 = 98 frames, 72 mels each.
        let feats = fb.compute(&tone(440.0, 1.0, 16_000));
        assert_eq!(feats.len(), 98);
        assert!(feats.iter().all(|f| f.len() == 72));
    }

    #[test]
    fn fbank_too_short_is_empty() {
        let fb = default_fbank();
        assert!(fb.compute(&[0.0; 100]).is_empty());
    }

    #[test]
    fn fbank_cmn_gives_zero_mean_per_dim() {
        let fb = default_fbank();
        let feats = fb.compute(&tone(300.0, 0.5, 16_000));
        assert!(!feats.is_empty());
        for d in 0..fb.n_mels() {
            let mean = feats.iter().map(|f| f[d]).sum::<f32>() / feats.len() as f32;
            assert!(mean.abs() < 1e-4, "dim {d} mean {mean} should be ~0 after CMN");
        }
    }

    #[test]
    fn fbank_tone_energy_localises_near_pitch() {
        let fb = default_fbank();
        // Average the pre-CMN spectra of a 1 kHz tone; the peak mel band's
        // centre frequency should sit near 1 kHz.
        let frames = fb.log_mel_frames(&tone(1000.0, 0.3, 16_000));
        assert!(!frames.is_empty());
        let mut avg = vec![0.0f32; fb.n_mels()];
        for fr in &frames {
            for (a, x) in avg.iter_mut().zip(fr.iter()) {
                *a += x;
            }
        }
        let peak = avg
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        let center = fb.centers()[peak];
        assert!(
            (center - 1000.0).abs() < 250.0,
            "peak band centre {center} Hz should be near 1 kHz"
        );
    }

    #[test]
    fn cosine_identical_is_one() {
        let a = vec![1.0, 2.0, 3.0];
        assert!((cosine(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn cosine_opposite_is_minus_one() {
        assert!((cosine(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_length_mismatch_or_zero_is_zero() {
        assert!(cosine(&[1.0, 2.0], &[1.0]).abs() < f32::EPSILON);
        assert!(cosine(&[0.0, 0.0], &[1.0, 2.0]).abs() < f32::EPSILON);
    }

    #[test]
    fn consistency_scores_edge_cases() {
        assert!(consistency_scores(&[]).is_empty());
        assert_eq!(consistency_scores(&[vec![1.0, 0.0]]), vec![1.0]);
    }

    #[test]
    fn consistency_flags_the_outlier() {
        // Three tight vectors near +x, one outlier pointing away.
        let embs = vec![
            vec![1.0, 0.05, 0.0],
            vec![1.0, -0.05, 0.0],
            vec![0.98, 0.0, 0.03],
            vec![-1.0, 0.0, 0.0], // outlier
        ];
        let s = consistency_scores(&embs);
        assert_eq!(s.len(), 4);
        // The outlier scores far below the three consistent members.
        let outlier = s[3];
        for &good in &s[..3] {
            assert!(good > 0.9, "consistent member should score high, got {good}");
            assert!(outlier < good, "outlier {outlier} should score below member {good}");
        }
        assert!(outlier < 0.0, "outlier points away, expect negative cosine, got {outlier}");
    }

    #[test]
    fn centroid_is_l2_normalised_mean_and_empty_on_bad_input() {
        // Mean of the two axis vectors points at 45°, then normalised to unit
        // length: (1/√2, 1/√2).
        let c = centroid(&[vec![1.0, 0.0], vec![0.0, 1.0]]);
        assert_eq!(c.len(), 2);
        let norm = c.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "centroid must be unit length, got {norm}");
        assert!((c[0] - c[1]).abs() < 1e-6, "symmetric inputs → symmetric centroid");
        // Empty input and all-mismatched widths yield an empty centroid.
        assert!(centroid(&[]).is_empty());
    }

    #[test]
    fn cohort_from_raw_normalises_members() {
        let rows = vec![vec![1.0, 0.0, 0.0], vec![0.0, 2.0, 0.0], vec![0.0, 0.0, 3.0]];
        let cohort = Cohort::from_raw(&rows);
        assert_eq!(cohort.len(), 3);
        assert!(!cohort.is_empty());
        for m in &cohort.members {
            let norm = m.iter().map(|x| x * x).sum::<f32>().sqrt();
            // Non-zero after centering => unit length.
            assert!((norm - 1.0).abs() < 1e-5 || norm < 1e-5);
        }
    }

    #[test]
    fn cohort_from_raw_drops_mismatched_rows() {
        let rows = vec![vec![1.0, 0.0], vec![0.0, 1.0, 0.0], vec![0.0, 1.0]];
        let cohort = Cohort::from_raw(&rows);
        assert_eq!(cohort.len(), 2, "the width-3 row is dropped");
    }

    #[test]
    fn empty_cohort_as_norm_is_identity() {
        let cohort = Cohort::default();
        let e = vec![1.0, 0.0];
        let t = vec![1.0, 0.0];
        let raw = cosine(&e, &t);
        assert!((cohort.as_norm(&e, &t, raw, 10) - raw).abs() < f32::EPSILON);
    }

    #[test]
    fn as_norm_rewards_genuine_over_impostor() {
        // A cohort clustered around one direction; a "genuine" pair points
        // away from the cohort (high raw cosine to each other, low to the
        // cohort) and an "impostor" pair sits inside the cohort cloud.
        let raw_cohort: Vec<Vec<f32>> = (0..8)
            .map(|i| {
                let j = i as f32 * 0.01;
                vec![1.0 + j, 0.1 - j, 0.0]
            })
            .collect();
        let cohort = Cohort::from_raw(&raw_cohort);

        // Genuine: two near-identical embeddings orthogonal to the cohort.
        let g_enroll = cohort.center(&[0.0, 0.0, 1.0]);
        let g_test = cohort.center(&[0.0, 0.01, 1.0]);
        let g_raw = cosine(&g_enroll, &g_test);
        let g_norm = cohort.as_norm(&g_enroll, &g_test, g_raw, 8);

        // Impostor: two embeddings pointing along the cohort direction.
        let i_enroll = cohort.center(&[1.0, 0.1, 0.0]);
        let i_test = cohort.center(&[1.02, 0.08, 0.0]);
        let i_raw = cosine(&i_enroll, &i_test);
        let i_norm = cohort.as_norm(&i_enroll, &i_test, i_raw, 8);

        assert!(
            g_norm > i_norm,
            "AS-Norm should separate genuine ({g_norm}) above impostor ({i_norm})"
        );
    }

    #[test]
    fn impostor_scores_cover_every_member_and_are_empty_without_cohort() {
        let raw_cohort: Vec<Vec<f32>> = (0..6)
            .map(|i| {
                let j = i as f32 * 0.02;
                vec![1.0 + j, 0.1 - j, 0.0]
            })
            .collect();
        let cohort = Cohort::from_raw(&raw_cohort);
        // A genuine-looking centroid orthogonal to the cohort cloud.
        let centroid = cohort.center(&[0.0, 0.0, 1.0]);
        let scores = cohort.impostor_scores(&centroid, 6);
        assert_eq!(scores.len(), 6, "one impostor score per cohort member");
        assert!(scores.iter().all(|s| s.is_finite()));

        // Empty cohort → no impostor scores.
        assert!(Cohort::default().impostor_scores(&centroid, 6).is_empty());
    }

    #[test]
    fn asset_url_joins_without_double_slashes() {
        assert_eq!(
            asset_url("https://example.test/dl/", "ort-1.24.2", "redimnet2-b3.ort"),
            "https://example.test/dl/ort-1.24.2/redimnet2-b3.ort"
        );
        assert_eq!(
            asset_url("https://example.test/dl", "ort-1.24.2", "redimnet2-b3.ort"),
            "https://example.test/dl/ort-1.24.2/redimnet2-b3.ort"
        );
    }

    #[test]
    fn every_graph_is_an_ort_on_the_fono_voice_mirror_with_abi_tag() {
        // Distribution must match the voice/wake stacks (ADR 0033): `.ort`
        // graph files under the ABI-named release tag on the fono-voice mirror.
        assert!(DEFAULT_BASE_URL.contains("fono-voice"), "assets ride the fono-voice mirror");
        assert_eq!(RELEASE_TAG, format!("ort-{ORT_VERSION}"));
        for m in registry() {
            assert!(
                std::path::Path::new(m.graph.file)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("ort")),
                "{} graph must be a .ort",
                m.name
            );
            assert_eq!(m.graph.release_tag, RELEASE_TAG, "{} graph must use the ABI tag", m.name);
            assert_eq!(
                m.cohort.file,
                format!("{}.cohort.bin", m.name),
                "cohort basename must be <name>.cohort.bin",
            );
            assert_eq!(m.cohort.release_tag, RELEASE_TAG, "{} cohort must use the ABI tag", m.name);
        }
    }

    #[test]
    fn graph_basename_is_name_dot_ort() {
        // The engine loads `<name>.ort`; the cached basename must equal that.
        for m in registry() {
            assert_eq!(
                m.graph.file,
                format!("{}.ort", m.name),
                "graph basename must be <name>.ort"
            );
        }
    }

    #[test]
    fn graphs_and_cohorts_are_hosted_and_pinned() {
        // Both the ReDimNet2 graphs and their AS-Norm impostor-cohort sidecars
        // are hosted on the mirror and pinned from the published bytes, so
        // AS-Norm is active end-to-end (empty-cohort fallback stays for
        // malformed/missing files only).
        for m in registry() {
            assert!(is_pinned(m.graph.sha256), "{} graph should be pinned once hosted", m.name);
            assert!(is_pinned(m.cohort.sha256), "{} cohort should be pinned once hosted", m.name);
        }
        assert!(is_pinned("7bb30475c5924b525b6684a00ff768aa9185f69e6265d5ff9653a48d70eee0e2"));
        assert!(!is_pinned(UNPINNED));
        assert!(!is_pinned("abc"));
    }

    #[test]
    fn resolved_paths_land_under_the_speaker_cache_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let r = resolved_paths(DEFAULT_MODEL, tmp.path()).unwrap();
        let dir = speaker_dir(tmp.path());
        assert_eq!(r.graph, dir.join("redimnet2-b3.ort"));
        // Cohort is hosted/pinned, so its cache path is advertised.
        assert_eq!(r.cohort, Some(dir.join("redimnet2-b3.cohort.bin")));
        assert!(resolved_paths("nope", tmp.path()).is_none());
    }

    #[tokio::test]
    async fn fetch_unknown_model_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let err = fetch_model("ghost", tmp.path(), None).await.unwrap_err().to_string();
        assert!(err.contains("unknown speaker model"), "{err}");
    }

    #[test]
    fn accumulator_tracks_seconds_and_sufficiency() {
        let mut acc = SpeechAccumulator::new(1.0); // 1 s = 16000 samples
        assert!(!acc.is_sufficient());
        acc.push(&vec![0.0; SAMPLE_RATE as usize / 2]); // 0.5 s
        assert!((acc.seconds() - 0.5).abs() < 1e-6);
        assert!(!acc.is_sufficient(), "half a second is below the 1 s minimum");
        acc.push(&vec![0.0; SAMPLE_RATE as usize / 2]); // now 1.0 s
        assert!(acc.is_sufficient(), "one full second meets the minimum");
        assert_eq!(acc.samples().len(), SAMPLE_RATE as usize);
        acc.clear();
        assert!(acc.seconds().abs() < f32::EPSILON);
        assert!(!acc.is_sufficient());
    }

    #[test]
    fn accumulator_zero_minimum_is_always_sufficient() {
        let acc = SpeechAccumulator::new(0.0);
        assert!(acc.is_sufficient(), "a zero minimum needs no audio");
    }

    #[test]
    fn decide_with_no_candidates_is_an_empty_reject() {
        let cohort = Cohort::default();
        let d = decide(&[], &[1.0, 0.0], &cohort, 0.5, true);
        assert_eq!(d.name, None);
        assert!(d.score.abs() < f32::EPSILON);
        assert!(d.confidence.abs() < f32::EPSILON);
        assert!(d.sufficient_audio);
    }

    #[test]
    fn decide_matches_the_genuine_speaker_and_rejects_below_threshold() {
        let cohort = Cohort::default(); // empty → AS-Norm is identity (raw cosine)
        let alice =
            EnrolledSpeaker { name: "alice".into(), embedding: cohort.center(&[1.0, 0.0, 0.0]) };
        let bob =
            EnrolledSpeaker { name: "bob".into(), embedding: cohort.center(&[0.0, 1.0, 0.0]) };
        let candidates = vec![alice, bob];

        // A test voice almost identical to Alice clears a modest threshold.
        let test = cohort.center(&[0.98, 0.02, 0.0]);
        let d = decide(&candidates, &test, &cohort, 0.5, true);
        assert_eq!(d.name.as_deref(), Some("alice"));
        assert!(d.score > 0.5);
        assert!(d.confidence > 0.5, "above-threshold score → confidence over 0.5");

        // The same voice against an impossibly high threshold is rejected,
        // but the winning score/confidence are still reported.
        let d = decide(&candidates, &test, &cohort, 5.0, true);
        assert_eq!(d.name, None, "no score can clear a threshold above the cosine ceiling");
        assert!(d.score > 0.5);
        assert!(d.confidence < 0.5, "below-threshold score → confidence under 0.5");
    }

    #[test]
    fn confidence_is_one_half_at_the_threshold() {
        assert!((confidence_from_margin(1.23, 1.23) - 0.5).abs() < 1e-6);
        assert!(confidence_from_margin(2.0, 1.0) > 0.5);
        assert!(confidence_from_margin(0.0, 1.0) < 0.5);
    }

    #[test]
    fn score_mean_std_matches_hand_computation() {
        let (m, s) = score_mean_std(&[1.0, 2.0, 3.0, 4.0]);
        assert!((m - 2.5).abs() < 1e-6);
        // population variance = 1.25 → std ≈ 1.1180
        assert!((s - 1.118_033_9).abs() < 1e-5);
        assert_eq!(score_mean_std(&[]), (0.0, 0.0));
    }

    #[test]
    fn eer_is_zero_for_perfectly_separable_scores() {
        let genuine = [0.8, 0.85, 0.9, 0.95];
        let impostor = [0.1, 0.15, 0.2, 0.25];
        let (eer, thr) = eer_and_threshold(&genuine, &impostor);
        assert!(eer.abs() < 1e-6, "cleanly separable → EER 0, got {eer}");
        // Threshold lands strictly between the two clusters.
        assert!(thr > 0.25 && thr <= 0.8, "threshold {thr} should sit in the gap");
    }

    #[test]
    fn eer_is_high_for_fully_overlapping_scores() {
        let genuine = [0.4, 0.5, 0.6];
        let impostor = [0.4, 0.5, 0.6];
        let (eer, _) = eer_and_threshold(&genuine, &impostor);
        assert!(eer > 0.3, "identical distributions → EER near 0.5, got {eer}");
    }

    #[test]
    fn eer_empty_side_is_the_neutral_default() {
        assert_eq!(eer_and_threshold(&[], &[0.1]), (0.5, 0.0));
        assert_eq!(eer_and_threshold(&[0.9], &[]), (0.5, 0.0));
    }

    #[test]
    fn far_threshold_hits_the_target_quantile() {
        // 100 impostor scores 0.00..0.99. A 1% FAR target admits only the very
        // top scores, so the threshold sits high in the distribution.
        let impostor: Vec<f32> = (0..100).map(|k| k as f32 / 100.0).collect();
        let thr = threshold_for_far(&impostor, 0.01);
        let far = impostor.iter().filter(|&&s| s >= thr).count() as f32 / 100.0;
        assert!(far <= 0.01 + 1e-6, "achieved FAR {far} must be ≤ target");
        assert!(thr >= 0.98, "1% FAR threshold {thr} should be near the top");
    }

    #[test]
    fn far_threshold_zero_target_rejects_all_impostors() {
        let impostor = [0.2, 0.5, 0.9];
        let thr = threshold_for_far(&impostor, 0.0);
        assert!(impostor.iter().all(|&s| s < thr), "0% FAR must reject every impostor");
    }

    #[test]
    fn calibrate_reports_consistent_distributions_and_thresholds() {
        let genuine = [0.7, 0.75, 0.8, 0.85];
        let impostor = [0.1, 0.2, 0.3, 0.4];
        let r = calibrate(&genuine, &impostor, DEFAULT_TARGET_FAR);
        assert_eq!(r.genuine_trials, 4);
        assert_eq!(r.impostor_trials, 4);
        assert!(r.genuine_mean > r.impostor_mean);
        assert!(r.eer.abs() < 1e-6, "separable → EER 0");
        assert!((r.target_far - DEFAULT_TARGET_FAR).abs() < 1e-9);
        // Both operating points cleanly separate this separable data: the strict
        // FAR point rejects every impostor and the EER point accepts every genuine.
        assert!(impostor.iter().all(|&s| s < r.far_threshold), "FAR point must reject impostors");
        assert!(genuine.iter().all(|&s| s >= r.eer_threshold), "EER point must accept genuines");
    }

    #[test]
    fn latency_stats_percentiles_are_nearest_rank() {
        let s: Vec<f32> = (1..=100).map(|k| k as f32).collect();
        let l = latency_stats(&s);
        assert_eq!(l.count, 100);
        assert!((l.mean_ms - 50.5).abs() < 1e-4);
        assert!((l.p50_ms - 50.0).abs() < 1e-6);
        assert!((l.p95_ms - 95.0).abs() < 1e-6);
        let empty = latency_stats(&[]);
        assert_eq!(empty.count, 0);
        assert!(empty.mean_ms.abs() < f32::EPSILON);
    }
}
