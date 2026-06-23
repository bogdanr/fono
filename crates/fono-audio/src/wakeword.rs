// SPDX-License-Identifier: GPL-3.0-only
//! Wake-word detection trait + backends.
//!
//! Shaped deliberately like [`crate::vad`]: a [`WakeWord`] backend consumes
//! short frames of 16 kHz mono `f32` audio (10–30 ms, the same frame contract
//! as [`crate::vad::Vad`]) and returns a scored [`WakeDecision`]. The backend
//! ring-buffers partial frames internally and only emits a *fired* decision
//! once a full detection hop has been processed, so callers can forward
//! arbitrary-length slices straight from
//! [`crate::capture::AudioCapture::start_with_forwarder`].
//!
//! Two backends mirror the VAD split:
//! - [`EnergyWakeStub`] — a no-op / energy placeholder that compiles and runs
//!   with **no model present** (the analogue of
//!   [`crate::vad::WebRtcVadStub`]). It never fires by default, so the daemon
//!   and pipeline stay functional before a model is fetched.
//! - [`OnnxWakeWord`] (feature `wakeword-onnx`) — the real openWakeWord
//!   detector: a melspectrogram graph → frozen `speech_embedding` backbone →
//!   one-or-more per-phrase classifiers, each an `ort` [`Session`] built with
//!   `with_intra_threads(1)` (mirroring `fono-tts`). Multiple classifiers
//!   share the single embedding pass, which is what makes extra phrases cheap.

use anyhow::Result;

/// Number of 16 kHz samples per detection hop (~80 ms). The openWakeWord
/// front-end advances its melspectrogram one hop at a time, so this is the
/// natural windowing granularity for the streaming detector.
pub const HOP_SAMPLES: usize = 1280;

/// Outcome of feeding audio to a [`WakeWord`] backend.
///
/// `fired` is `true` only when a configured phrase crossed its sensitivity
/// threshold on the most recent hop. `score` is the highest classifier score
/// seen on that hop (whether or not it fired) so callers can surface a
/// confidence read-out; `phrase` names the model that fired, when one did.
#[derive(Debug, Clone, PartialEq)]
pub struct WakeDecision {
    pub fired: bool,
    pub score: f32,
    pub phrase: Option<String>,
}

impl WakeDecision {
    /// A non-firing decision carrying the observed (sub-threshold) score.
    #[must_use]
    pub fn silent(score: f32) -> Self {
        Self { fired: false, score, phrase: None }
    }

    /// A firing decision for `phrase` at `score`.
    #[must_use]
    pub fn fired(phrase: impl Into<String>, score: f32) -> Self {
        Self { fired: true, score, phrase: Some(phrase.into()) }
    }
}

/// A pluggable wake-word detector.
///
/// `feed` accepts a 10–30 ms frame of 16 kHz mono `f32` samples (same frame
/// contract as [`crate::vad::Vad::classify`]). The backend buffers internally
/// and returns the decision for the most recent hop completed by this frame;
/// when no hop completes, it returns a [`WakeDecision::silent`].
pub trait WakeWord: Send {
    fn feed(&mut self, frame: &[f32]) -> Result<WakeDecision>;
}

/// Root-mean-square energy of a frame (0.0 for an empty frame).
fn rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt()
}

/// Accumulates partial PCM and yields complete fixed-size hops.
///
/// The capture forwarder delivers arbitrary-length slices; the streaming
/// detector needs uniform hops. `HopBuffer` is the deterministic, testable
/// core of that windowing (see the unit tests) and is reused by
/// [`OnnxWakeWord`].
pub struct HopBuffer {
    buf: Vec<f32>,
    hop: usize,
}

impl HopBuffer {
    /// New buffer emitting hops of `hop` samples (clamped to at least 1).
    #[must_use]
    pub fn new(hop: usize) -> Self {
        Self { buf: Vec::new(), hop: hop.max(1) }
    }

    /// Append `frame` and return every complete `hop`-sized window now
    /// available, in order. Leftover samples stay buffered for the next call.
    pub fn drain_hops(&mut self, frame: &[f32]) -> Vec<Vec<f32>> {
        self.buf.extend_from_slice(frame);
        let mut hops = Vec::new();
        while self.buf.len() >= self.hop {
            hops.push(self.buf.drain(..self.hop).collect());
        }
        hops
    }

    /// Samples currently buffered but not yet emitted as a hop.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    /// The configured hop size.
    #[must_use]
    pub fn hop(&self) -> usize {
        self.hop
    }
}

/// Energy / no-op stub used when no ONNX model is present — the wake-word
/// analogue of [`crate::vad::WebRtcVadStub`].
///
/// With the default [`threshold`](Self::threshold) of `None` it **never
/// fires**, which is the safe state the daemon needs so the pipeline compiles
/// and runs before a model is fetched. Setting a `Some(threshold)` turns it
/// into a crude energy gate that fires a placeholder [`phrase`](Self::phrase)
/// when a hop's RMS crosses the threshold — useful for exercising downstream
/// wiring deterministically in tests.
pub struct EnergyWakeStub {
    pub threshold: Option<f32>,
    pub phrase: String,
    hops: HopBuffer,
}

impl Default for EnergyWakeStub {
    fn default() -> Self {
        Self { threshold: None, phrase: "stub".into(), hops: HopBuffer::new(HOP_SAMPLES) }
    }
}

impl EnergyWakeStub {
    /// A stub that fires its placeholder phrase when a hop's RMS ≥ `threshold`.
    #[must_use]
    pub fn with_threshold(threshold: f32, phrase: impl Into<String>) -> Self {
        Self {
            threshold: Some(threshold),
            phrase: phrase.into(),
            hops: HopBuffer::new(HOP_SAMPLES),
        }
    }
}

impl WakeWord for EnergyWakeStub {
    fn feed(&mut self, frame: &[f32]) -> Result<WakeDecision> {
        let mut best = WakeDecision::silent(0.0);
        for hop in self.hops.drain_hops(frame) {
            let energy = rms(&hop);
            let decision = match self.threshold {
                Some(t) if energy >= t => WakeDecision::fired(self.phrase.clone(), energy),
                _ => WakeDecision::silent(energy),
            };
            // Prefer a fired hop; otherwise keep the loudest sub-threshold one.
            if (decision.fired && !best.fired)
                || (decision.fired == best.fired && decision.score > best.score)
            {
                best = decision;
            }
        }
        Ok(best)
    }
}

#[cfg(feature = "wakeword-onnx")]
pub use onnx::{OnnxWakeWord, PhraseModelSpec, WakeModelPaths};

#[cfg(feature = "wakeword-onnx")]
mod onnx {
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};

    use anyhow::Result;
    use ort::session::builder::GraphOptimizationLevel;
    use ort::session::Session;
    use ort::value::Tensor;

    use super::{HopBuffer, WakeDecision, WakeWord, HOP_SAMPLES};

    /// Mel bins emitted per melspectrogram frame (openWakeWord uses 32).
    const MEL_BINS: usize = 32;
    /// Mel frames the embedding backbone consumes per inference window.
    const EMBED_WINDOW_MELS: usize = 76;
    /// Mel frames advanced between successive embedding windows.
    const EMBED_MEL_STEP: usize = 8;
    /// Embeddings the per-phrase classifier consumes per inference window.
    const CLASSIFIER_WINDOW_EMB: usize = 16;
    /// Embedding vector width emitted by the `speech_embedding` backbone.
    const EMBED_DIM: usize = 96;
    /// Raw-audio lookback (3 mel-frame strides @ 160 samples) prepended to each
    /// hop before the melspectrogram. openWakeWord's streaming melspec feeds the
    /// last `n_samples + 160*3` samples per update so every 1280-sample hop
    /// yields the full 8 mel frames (`ceil(1760/160 - 3) = 8`) with correct STFT
    /// context across the hop boundary. Without it the model sees an isolated
    /// 1280-sample block (only 5 frames, with edge artifacts), the mel→embedding
    /// stepping de-aligns, and classifier scores collapse — the cause of
    /// unreliable firing.
    const MELSPEC_LOOKBACK: usize = 160 * 3;

    /// One per-phrase classifier loaded against the shared embedding backbone.
    pub struct PhraseModelSpec {
        /// Phrase id reported in a [`WakeDecision`] (e.g. `"hey_fono"`).
        pub phrase: String,
        /// Path to the classifier `.ort` graph.
        pub model: PathBuf,
        /// Score (0..=1) at or above which this phrase fires.
        pub threshold: f32,
    }

    /// Paths to the three-stage openWakeWord graphs plus the active phrases.
    pub struct WakeModelPaths {
        pub melspec: PathBuf,
        pub embedding: PathBuf,
        pub phrases: Vec<PhraseModelSpec>,
    }

    struct PhraseClassifier {
        phrase: String,
        session: Session,
        input: String,
        output: String,
        threshold: f32,
    }

    /// openWakeWord detector: melspectrogram → frozen embedding → per-phrase
    /// classifiers, all on the shared `ort` runtime. Single-threaded per the
    /// `with_intra_threads(1)` policy used by the Piper/Kokoro engines.
    pub struct OnnxWakeWord {
        melspec: Session,
        melspec_in: String,
        melspec_out: String,
        embedding: Session,
        embedding_in: String,
        embedding_out: String,
        classifiers: Vec<PhraseClassifier>,
        hops: HopBuffer,
        /// Tail of the previous hop's raw audio (up to [`MELSPEC_LOOKBACK`]
        /// samples), prepended to the next hop so the streaming melspectrogram
        /// has cross-boundary STFT context (openWakeWord parity).
        mel_lookback: Vec<f32>,
        /// Flattened mel frames awaiting an embedding window (rows of `MEL_BINS`).
        mel_ring: Vec<f32>,
        /// Most-recent embeddings awaiting a classifier window.
        emb_ring: VecDeque<Vec<f32>>,
    }

    fn build_session(path: &Path, label: &str) -> Result<(Session, String, String)> {
        // Idempotent: ensure the process-wide ONNX Runtime env exists before
        // the first session is built (mirrors `fono-tts`'s `ensure_runtime`).
        let _ = ort::init().with_name("fono").commit();
        let session = Session::builder()
            .map_err(|e| anyhow::anyhow!("create ort session builder: {e}"))?
            // `.ort` models are pre-optimised and the minimal runtime has the
            // optimiser compiled out, so setting a level errors — recover the
            // builder and carry on (the documented `ort` minimal idiom).
            .with_optimization_level(GraphOptimizationLevel::Disable)
            .unwrap_or_else(ort::Error::recover)
            .with_intra_threads(1)
            .map_err(|e| anyhow::anyhow!("set intra-op threads: {e}"))?
            .commit_from_file(path)
            .map_err(|e| anyhow::anyhow!("load wake model {} ({label}): {e}", path.display()))?;
        let input =
            session.inputs().first().map_or_else(|| "input".to_string(), |i| i.name().to_string());
        let output = session
            .outputs()
            .first()
            .map_or_else(|| "output".to_string(), |o| o.name().to_string());
        Ok((session, input, output))
    }

    impl OnnxWakeWord {
        /// Load the shared melspectrogram + embedding graphs and every phrase
        /// classifier.
        pub fn load(paths: &WakeModelPaths) -> Result<Self> {
            let (melspec, melspec_in, melspec_out) = build_session(&paths.melspec, "melspec")?;
            let (embedding, embedding_in, embedding_out) =
                build_session(&paths.embedding, "embedding")?;
            let mut classifiers = Vec::with_capacity(paths.phrases.len());
            for spec in &paths.phrases {
                let (session, input, output) = build_session(&spec.model, &spec.phrase)?;
                classifiers.push(PhraseClassifier {
                    phrase: spec.phrase.clone(),
                    session,
                    input,
                    output,
                    threshold: spec.threshold,
                });
            }
            let mut detector = Self {
                melspec,
                melspec_in,
                melspec_out,
                embedding,
                embedding_in,
                embedding_out,
                classifiers,
                hops: HopBuffer::new(HOP_SAMPLES),
                mel_lookback: Vec::new(),
                mel_ring: Vec::new(),
                emb_ring: VecDeque::with_capacity(CLASSIFIER_WINDOW_EMB),
            };
            detector.prime()?;
            Ok(detector)
        }

        /// Pre-fill the streaming buffers with silence so the detector emits a
        /// (non-firing) classifier score from the very first hop, instead of
        /// needing ~2 s of audio to fill the 76-frame mel window and the
        /// 16-embedding classifier window. openWakeWord does the same — it seeds
        /// its melspectrogram buffer and feature buffer at construction so
        /// detection is live immediately. Without priming, every mic re-open
        /// (the listener re-opens the stream each time the FSM returns to Idle,
        /// i.e. after every dictation/assistant session) has a ~2 s dead zone in
        /// which the wake phrase cannot fire — a major source of "missed" wakes.
        ///
        /// Seeding with the silent-mel value keeps the primed classifier output
        /// below threshold (silence never wakes), so this cannot cause a
        /// spurious fire on start; the seeded frames/embeddings are flushed out
        /// by real audio within ~1 s of speech arriving.
        fn prime(&mut self) -> Result<()> {
            // openWakeWord's `mel/10 + 2` transform maps a silent (≈0) mel bin to
            // 2.0; use that as the neutral fill for the post-transform mel ring.
            const SILENCE_MEL: f32 = 2.0;
            let window = vec![SILENCE_MEL; EMBED_WINDOW_MELS * MEL_BINS];
            let emb = self.run_embedding(&window)?;
            self.mel_ring = window;
            self.emb_ring.clear();
            for _ in 0..CLASSIFIER_WINDOW_EMB {
                self.emb_ring.push_back(emb.clone());
            }
            Ok(())
        }

        fn run_melspec(&mut self, hop: &[f32]) -> Result<Vec<f32>> {
            // openWakeWord's melspectrogram graph expects int16-scale audio
            // (±32768), but Fono's capture pipeline delivers normalized f32 in
            // ±1.0 (the cpal convention every other consumer assumes — e.g. the
            // Wyoming f32→i16 path multiplies by 32767). Feeding the raw ±1.0
            // samples leaves the signal ~90 dB below what the graph and its
            // downstream `mel/10 + 2` normalisation were calibrated for, so the
            // mel bins collapse to a near-constant value and the classifier
            // barely discriminates — the cause of unreliable wake firing. Scale
            // back to int16 range before the melspec inference.
            let scaled: Vec<f32> = hop.iter().map(|s| s * 32768.0).collect();
            let tensor = Tensor::from_array((vec![1_i64, scaled.len() as i64], scaled))
                .map_err(|e| anyhow::anyhow!("build melspec input tensor: {e}"))?;
            let outputs = self
                .melspec
                .run(ort::inputs![self.melspec_in.as_str() => tensor])
                .map_err(|e| anyhow::anyhow!("run wake melspec: {e}"))?;
            let (_shape, data) = outputs[self.melspec_out.as_str()]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow::anyhow!("extract wake melspec output: {e}"))?;
            // openWakeWord normalises the raw melspectrogram as `mel/10 + 2`.
            Ok(data.iter().map(|m| m / 10.0 + 2.0).collect())
        }

        fn run_embedding(&mut self, window: &[f32]) -> Result<Vec<f32>> {
            // Embedding window shape `[1, 76, 32, 1]`.
            let tensor = Tensor::from_array((
                vec![1_i64, EMBED_WINDOW_MELS as i64, MEL_BINS as i64, 1_i64],
                window.to_vec(),
            ))
            .map_err(|e| anyhow::anyhow!("build embedding input tensor: {e}"))?;
            let outputs = self
                .embedding
                .run(ort::inputs![self.embedding_in.as_str() => tensor])
                .map_err(|e| anyhow::anyhow!("run wake embedding: {e}"))?;
            let (_shape, data) =
                outputs[self.embedding_out.as_str()]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| anyhow::anyhow!("extract wake embedding output: {e}"))?;
            Ok(data.to_vec())
        }

        /// Run every classifier over the current embedding window and return
        /// the best decision (firing beats non-firing; higher score wins).
        fn run_classifiers(&mut self) -> Result<WakeDecision> {
            // Classifier window shape `[1, 16, 96]`.
            let feat: Vec<f32> = self.emb_ring.iter().flatten().copied().collect();
            let mut best = WakeDecision::silent(0.0);
            for c in &mut self.classifiers {
                let tensor = Tensor::from_array((
                    vec![1_i64, CLASSIFIER_WINDOW_EMB as i64, EMBED_DIM as i64],
                    feat.clone(),
                ))
                .map_err(|e| anyhow::anyhow!("build classifier input tensor: {e}"))?;
                let outputs = c
                    .session
                    .run(ort::inputs![c.input.as_str() => tensor])
                    .map_err(|e| anyhow::anyhow!("run wake classifier {}: {e}", c.phrase))?;
                let (_shape, data) = outputs[c.output.as_str()]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| anyhow::anyhow!("extract classifier {} output: {e}", c.phrase))?;
                let score = data.first().copied().unwrap_or(0.0);
                let fired = score >= c.threshold;
                if (fired && !best.fired) || (fired == best.fired && score > best.score) {
                    best = if fired {
                        WakeDecision::fired(c.phrase.clone(), score)
                    } else {
                        WakeDecision::silent(score)
                    };
                }
            }
            Ok(best)
        }

        fn process_hop(&mut self, hop: &[f32]) -> Result<WakeDecision> {
            // Prepend the previous hop's raw-audio tail so the melspectrogram has
            // STFT context across the hop boundary and yields the full 8 mel
            // frames per hop (openWakeWord streaming parity). Then retain this
            // hop's tail as the lookback for the next call.
            let mut input = Vec::with_capacity(self.mel_lookback.len() + hop.len());
            input.extend_from_slice(&self.mel_lookback);
            input.extend_from_slice(hop);
            let keep = hop.len().min(MELSPEC_LOOKBACK);
            self.mel_lookback = hop[hop.len() - keep..].to_vec();
            let mels = self.run_melspec(&input)?;
            self.mel_ring.extend(mels);
            // Slide the embedding window across the mel ring at the fixed step.
            while self.mel_ring.len() >= EMBED_WINDOW_MELS * MEL_BINS {
                let window: Vec<f32> = self.mel_ring[..EMBED_WINDOW_MELS * MEL_BINS].to_vec();
                let emb = self.run_embedding(&window)?;
                self.emb_ring.push_back(emb);
                while self.emb_ring.len() > CLASSIFIER_WINDOW_EMB {
                    self.emb_ring.pop_front();
                }
                self.mel_ring.drain(..EMBED_MEL_STEP * MEL_BINS);
            }
            if self.emb_ring.len() < CLASSIFIER_WINDOW_EMB {
                return Ok(WakeDecision::silent(0.0));
            }
            self.run_classifiers()
        }
    }

    impl WakeWord for OnnxWakeWord {
        fn feed(&mut self, frame: &[f32]) -> Result<WakeDecision> {
            let mut best = WakeDecision::silent(0.0);
            for hop in self.hops.drain_hops(frame) {
                let decision = self.process_hop(&hop)?;
                if (decision.fired && !best.fired)
                    || (decision.fired == best.fired && decision.score > best.score)
                {
                    best = decision;
                }
            }
            Ok(best)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_never_fires_by_default() {
        let mut w = EnergyWakeStub::default();
        // A full hop of loud audio must still not fire the default stub.
        let decision = w.feed(&[0.9; HOP_SAMPLES]).unwrap();
        assert!(!decision.fired);
        assert!(decision.phrase.is_none());
    }

    #[test]
    fn stub_with_threshold_fires_on_loud_hop() {
        let mut w = EnergyWakeStub::with_threshold(0.5, "hey_fono");
        let quiet = w.feed(&[0.01; HOP_SAMPLES]).unwrap();
        assert!(!quiet.fired, "quiet hop must not fire");
        let loud = w.feed(&[0.9; HOP_SAMPLES]).unwrap();
        assert!(loud.fired, "loud hop must fire");
        assert_eq!(loud.phrase.as_deref(), Some("hey_fono"));
    }

    #[test]
    fn stub_buffers_partial_frames_until_a_full_hop() {
        let mut w = EnergyWakeStub::with_threshold(0.5, "hey_fono");
        // Feed less than a hop: no decision should fire yet.
        let partial = w.feed(&[0.9; HOP_SAMPLES - 1]).unwrap();
        assert!(!partial.fired, "an incomplete hop must not produce a fire");
        // The single remaining sample completes the hop and fires.
        let complete = w.feed(&[0.9]).unwrap();
        assert!(complete.fired, "completing the hop must fire");
    }

    #[test]
    fn hop_buffer_windows_varying_length_slices() {
        let mut hb = HopBuffer::new(4);
        // Smaller than a hop: nothing emitted, all buffered.
        assert!(hb.drain_hops(&[1.0, 2.0]).is_empty());
        assert_eq!(hb.pending(), 2);
        // Now push 6 more -> 8 buffered -> exactly two hops of 4.
        let hops = hb.drain_hops(&[3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
        assert_eq!(hops.len(), 2);
        assert_eq!(hops[0], vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(hops[1], vec![5.0, 6.0, 7.0, 8.0]);
        assert_eq!(hb.pending(), 0);
    }

    #[test]
    fn hop_buffer_emits_multiple_hops_from_one_large_slice() {
        let mut hb = HopBuffer::new(2);
        let big: Vec<f32> = (0..7).map(|i| i as f32).collect();
        let hops = hb.drain_hops(&big);
        assert_eq!(hops.len(), 3, "7 samples / hop 2 => 3 full hops");
        assert_eq!(hb.pending(), 1, "one leftover sample stays buffered");
    }

    #[test]
    fn hop_buffer_clamps_zero_hop() {
        // A zero hop would loop forever; the buffer clamps to 1.
        let mut hb = HopBuffer::new(0);
        assert_eq!(hb.hop(), 1);
        assert_eq!(hb.drain_hops(&[1.0, 2.0]).len(), 2);
    }

    #[test]
    fn multi_phrase_plumbing_reports_loudest_via_stub() {
        // The stub stands in for the multi-classifier path: assert the
        // decision carries the configured phrase id when it fires.
        let mut w = EnergyWakeStub::with_threshold(0.2, "hey_jarvis");
        let decision = w.feed(&[0.5; HOP_SAMPLES]).unwrap();
        assert!(decision.fired);
        assert_eq!(decision.phrase.as_deref(), Some("hey_jarvis"));
        assert!(decision.score > 0.2);
    }
}
