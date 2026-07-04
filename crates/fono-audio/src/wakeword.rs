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

use std::collections::VecDeque;

use anyhow::Result;

/// Number of 16 kHz samples per detection hop (~80 ms). The openWakeWord
/// front-end advances its melspectrogram one hop at a time, so this is the
/// natural windowing granularity for the streaming detector.
pub const HOP_SAMPLES: usize = 1280;

/// Threshold crossings required within the trailing [`WAKE_WINDOW_HOPS`] window
/// before a phrase fires (see [`ActivationWindow`]). Mirrors openWakeWord's
/// `trigger_level`: needing two crossings (not one) means a lone single-hop
/// spike — the confident false positive plain thresholding lets through —
/// cannot fire, while the raw score is still compared at *full value* per hop
/// (never averaged), so the narrow high-confidence peak of a real utterance is
/// preserved (averaging blunted it and cost recall).
pub const WAKE_TRIGGER_LEVEL: usize = 2;

/// Trailing window, in hops, over which [`WAKE_TRIGGER_LEVEL`] crossings are
/// counted (see [`ActivationWindow`]). Five 80 ms hops = 400 ms: the two
/// crossings may be up to three hops apart and still count together, so a
/// genuine wake whose peak flickers — crosses, dips for a hop or two, crosses
/// again — still fires. This is *spread tolerance*, not latency: firing happens
/// the instant the count reaches the trigger level, so two consecutive
/// crossings still fire at 160 ms; the wider window only rescues the harder
/// flickering utterances that a tighter gate would drop entirely.
pub const WAKE_WINDOW_HOPS: usize = 5;

/// Trailing window, in hops, over which [`EnergyGate`] measures loudness.
/// Twelve 80 ms hops ≈ 1 s — enough to cover the spoken phrase, which sits at
/// the recent end of the model's ~2 s receptive field. Longer would not help
/// recall (we take the *peak* over the window) and would slightly hurt
/// rejection by sweeping in unrelated earlier sounds.
pub const WAKE_ENERGY_WINDOW_HOPS: usize = 12;

/// How many times louder than the tracked ambient noise floor the recent audio
/// must be for a wake crossing to count (see [`EnergyGate`]). This is a
/// *ratio*, not an absolute level, so it self-calibrates to each machine / mic
/// / room: the observed false fires happen when the model hallucinates the
/// phrase out of near-silent room tone (audio *at* the ambient floor, ratio
/// ≈ 1×), while genuine speech — even from across a room — is a burst well
/// above it. 2× (~6 dB) clears the at-ambient phantoms with margin while
/// staying gentle enough to hear far-field speech. Tuned empirically; a
/// hardcoded constant on purpose (no user-facing knob).
pub const WAKE_ENERGY_MARGIN: f32 = 2.0;

/// Absolute minimum recent peak (0..=1 sample amplitude) below which a wake
/// crossing never counts, regardless of the ratio. A safety net for a
/// dead-silent room where the tracked floor approaches zero and the ratio
/// alone would admit almost anything.
pub const WAKE_ENERGY_EPSILON: f32 = 0.01;

/// EMA rate at which the ambient floor falls toward a quieter level — fast, so
/// it re-settles on true ambient within a few hops after speech ends.
const NOISE_FLOOR_FALL: f32 = 0.25;

/// EMA rate at which the ambient floor rises toward a louder level — slow, so a
/// speech burst barely lifts it and stays a clear burst above the ambient.
const NOISE_FLOOR_RISE: f32 = 0.002;

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

/// Peak amplitude of a frame — the loudest `|sample|` (0.0 for an empty frame).
/// Speech is bursty, so the peak over a window catches short loud transients
/// that an averaged/RMS measure can miss; this is what [`EnergyGate`] tracks.
/// Only the ONNX detector feeds hops through the [`EnergyGate`].
#[cfg(feature = "wakeword-onnx")]
fn peak(frame: &[f32]) -> f32 {
    frame.iter().copied().fold(0.0_f32, |m, s| m.max(s.abs()))
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

/// openWakeWord-style activation gate: the debounce that makes the detector
/// robust without blunting its peak sensitivity.
///
/// Each hop's *raw* classifier score is compared to threshold at full value;
/// this gate only tracks the boolean crossing over a trailing sliding window
/// of `window` hops. The phrase fires the instant `trigger_level` crossings
/// are present in that window. Because the raw score is never averaged, the
/// short high-confidence peak of a genuine wake word is preserved (a moving
/// average smeared it out and cost recall); because a fire needs
/// `trigger_level` crossings, an isolated single-hop spike — even a confident
/// ~0.99 one — cannot fire. Counting over a *window* (rather than requiring
/// consecutive crossings) tolerates a flickering peak — cross, dip for a hop
/// or two, cross again — which is the common cause of missed wakes. Firing on
/// count-reached (not window-full) means two consecutive crossings still fire
/// at the earliest possible hop; the window only bounds how far apart the two
/// crossings may be, not the latency. Pure and unit-tested; one lives in each
/// phrase classifier.
pub struct ActivationWindow {
    recent: VecDeque<bool>,
    window: usize,
    trigger_level: usize,
}

impl ActivationWindow {
    /// New gate firing when `trigger_level` crossings (clamped to at least 1)
    /// are present in the trailing `window` hops (clamped to at least
    /// `trigger_level`, so the count can always be reached).
    #[must_use]
    pub fn new(window: usize, trigger_level: usize) -> Self {
        let trigger_level = trigger_level.max(1);
        let window = window.max(trigger_level);
        Self { recent: VecDeque::with_capacity(window), window, trigger_level }
    }

    /// Feed one hop's threshold decision. `over_threshold` is `raw >= threshold`
    /// for this hop. Returns `true` on the hop that brings the trailing-window
    /// crossing count up to `trigger_level` (a fire), clearing the window so the
    /// post-fire refractory window — not a lingering count — governs any re-fire.
    pub fn update(&mut self, over_threshold: bool) -> bool {
        self.recent.push_back(over_threshold);
        while self.recent.len() > self.window {
            self.recent.pop_front();
        }
        if self.recent.iter().filter(|&&hit| hit).count() >= self.trigger_level {
            self.recent.clear();
            return true;
        }
        false
    }
}

/// Ambient-relative loudness gate: rejects wake crossings that occur while the
/// audio is merely at the room's background level.
///
/// The dominant false-fire mode is the model hallucinating the phrase out of
/// near-silent room tone — the crossing lands while the audio sits *at* the
/// ambient floor. Genuine speech (near or far) is a burst *above* it. This gate
/// tracks a slow-moving estimate of the ambient noise floor and only admits a
/// crossing when the recent peak is at least [`WAKE_ENERGY_MARGIN`]× that
/// floor. Because the test is a *ratio* against a self-calibrating floor, it
/// works the same on any mic / gain / room without an absolute threshold. The
/// floor falls quickly toward quiet (re-settling on true ambient after speech)
/// and rises slowly (so a speech burst barely lifts it). It does **not** defend
/// against *content* noise — radio/TV speech is a burst too — that needs a
/// smarter model, not a loudness gate. Pure and unit-tested; one lives in the
/// detector.
pub struct EnergyGate {
    recent: VecDeque<f32>,
    window: usize,
    margin: f32,
    noise_floor: f32,
    floor_initialized: bool,
}

impl EnergyGate {
    /// New gate measuring the peak over the trailing `window` hops (clamped to
    /// at least 1) and requiring it to exceed the ambient floor by `margin`×
    /// (clamped to at least 1×).
    #[must_use]
    pub fn new(window: usize, margin: f32) -> Self {
        let window = window.max(1);
        Self {
            recent: VecDeque::with_capacity(window),
            window,
            margin: margin.max(1.0),
            noise_floor: 0.0,
            floor_initialized: false,
        }
    }

    /// Feed one hop's peak amplitude (max `|sample|`, 0..=1). Updates the
    /// ambient floor estimate and returns whether the trailing-window peak is
    /// loud enough — at least `margin`× the floor, and above the absolute
    /// [`WAKE_ENERGY_EPSILON`] safety net — to admit a wake crossing.
    pub fn update(&mut self, hop_peak: f32) -> bool {
        let hop_peak = hop_peak.max(0.0);
        if !self.floor_initialized {
            // Seed on the first hop so the floor calibrates to this machine's
            // ambient immediately instead of ramping up from zero.
            self.noise_floor = hop_peak;
            self.floor_initialized = true;
        } else if hop_peak < self.noise_floor {
            self.noise_floor =
                NOISE_FLOOR_FALL.mul_add(hop_peak - self.noise_floor, self.noise_floor);
        } else {
            self.noise_floor =
                NOISE_FLOOR_RISE.mul_add(hop_peak - self.noise_floor, self.noise_floor);
        }
        self.recent.push_back(hop_peak);
        while self.recent.len() > self.window {
            self.recent.pop_front();
        }
        let recent_peak = self.recent.iter().copied().fold(0.0_f32, f32::max);
        recent_peak >= (self.noise_floor * self.margin).max(WAKE_ENERGY_EPSILON)
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

    use super::{
        peak, ActivationWindow, EnergyGate, HopBuffer, WakeDecision, WakeWord, HOP_SAMPLES,
        WAKE_ENERGY_MARGIN, WAKE_ENERGY_WINDOW_HOPS, WAKE_TRIGGER_LEVEL, WAKE_WINDOW_HOPS,
    };

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
        /// openWakeWord-style activation gate over this classifier's raw per-hop
        /// threshold crossings. The phrase fires when `trigger_level` crossings
        /// fall within the trailing window — raw scores are compared at full
        /// value (never averaged), so the short high-confidence peak of a
        /// genuine wake is preserved while a lone single-hop spike cannot fire.
        gate: ActivationWindow,
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
        /// Loudness gate shared across all phrase classifiers: rejects wake
        /// crossings on near-silent audio (the quiet-room hallucination class),
        /// self-calibrating to each machine's ambient noise floor.
        energy_gate: EnergyGate,
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
                    gate: ActivationWindow::new(WAKE_WINDOW_HOPS, WAKE_TRIGGER_LEVEL),
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
                energy_gate: EnergyGate::new(WAKE_ENERGY_WINDOW_HOPS, WAKE_ENERGY_MARGIN),
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
        fn run_classifiers(&mut self, energy_ok: bool) -> Result<WakeDecision> {
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
                let raw = data.first().copied().unwrap_or(0.0);
                // Compare the raw hop score at full value (never averaged — that
                // blunts the short peak of a real wake and costs recall) and let
                // the activation gate debounce it: a fire needs `trigger_level`
                // crossings within the trailing window, so a lone spike cannot
                // trigger. The reported score is the raw hop score.
                let score = raw;
                // A crossing only counts when the audio was loud enough to
                // plausibly be nearby speech: this rejects the quiet-room
                // hallucinations (high score on near-silent audio) that the
                // score/window gates cannot, without touching genuine speech.
                let fired = c.gate.update(raw >= c.threshold && energy_ok);
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
            // Update the loudness gate every hop (even before the embedding
            // window fills) so the ambient noise floor keeps tracking.
            let energy_ok = self.energy_gate.update(peak(hop));
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
            self.run_classifiers(energy_ok)
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

    /// A lone threshold crossing (a single-hop spike) must not fire: only one
    /// crossing sits in the window, below `trigger_level` 2. This is the
    /// confident single-hop false positive from the original 24 h log.
    #[test]
    fn window_ignores_a_lone_spike() {
        let mut g = ActivationWindow::new(5, 2);
        assert!(!g.update(true), "one crossing is below trigger_level 2");
        for _ in 0..5 {
            assert!(!g.update(false), "misses alone never fire");
        }
    }

    /// Two consecutive crossings fire on the second hop — a clean wake still
    /// triggers at the earliest possible moment (160 ms), so the window costs
    /// no latency in the common case.
    #[test]
    fn window_fires_on_two_consecutive_crossings() {
        let mut g = ActivationWindow::new(5, 2);
        assert!(!g.update(true), "first crossing");
        assert!(g.update(true), "second consecutive crossing fires immediately");
    }

    /// The whole point of the window: a flickering peak — cross, dip, cross —
    /// still fires, whereas a strict consecutive gate would drop it. Here the
    /// two crossings are three hops apart, at the edge of the 5-hop window.
    #[test]
    fn window_fires_across_a_dip() {
        let mut g = ActivationWindow::new(5, 2);
        assert!(!g.update(true)); // crossing at hop 0
        assert!(!g.update(false)); // dip
        assert!(!g.update(false)); // dip
        assert!(g.update(true), "second crossing within the 5-hop window fires");
    }

    /// Two crossings farther apart than the window cannot fire: the first has
    /// aged out before the second arrives, so isolated spikes never accumulate.
    #[test]
    fn window_does_not_fire_when_crossings_age_out() {
        let mut g = ActivationWindow::new(3, 2);
        assert!(!g.update(true)); // crossing
        assert!(!g.update(false));
        assert!(!g.update(false)); // window now [T,F,F]
        assert!(!g.update(true), "first crossing has aged out; only one in window");
    }

    /// After firing, the window clears so the refractory window (not a lingering
    /// count) governs re-fires: two fresh crossings are needed again.
    #[test]
    fn window_clears_after_firing() {
        let mut g = ActivationWindow::new(5, 2);
        g.update(true);
        assert!(g.update(true), "fires on the second crossing");
        assert!(!g.update(true), "post-fire clear: one crossing is not enough again");
        assert!(g.update(true), "the next crossing reaches trigger_level");
    }

    /// A window smaller than the trigger level is clamped up so the count is
    /// always reachable; a zero trigger level clamps to 1.
    #[test]
    fn window_clamps_degenerate_params() {
        let mut g = ActivationWindow::new(1, 2); // window clamped up to 2
        assert!(!g.update(true));
        assert!(g.update(true), "two crossings fit the clamped window and fire");
        let mut z = ActivationWindow::new(0, 0); // trigger clamps to 1
        assert!(z.update(true), "trigger_level clamps to 1: first crossing fires");
    }

    /// The core failure from the logs: the model hallucinates the phrase out of
    /// steady room tone, so the "loud" hop sits *at* the ambient floor (ratio
    /// ≈ 1×). With a 2× margin that crossing must be rejected.
    #[test]
    fn energy_rejects_audio_at_the_ambient_floor() {
        let mut g = EnergyGate::new(12, 2.0);
        // Let the floor settle on a steady ambient of 0.04 (well above epsilon).
        for _ in 0..64 {
            let _ = g.update(0.04);
        }
        assert!(!g.update(0.04), "a hop at ambient (ratio 1x) must not pass the 2x gate");
    }

    /// A genuine speech burst well above the ambient floor passes.
    #[test]
    fn energy_admits_a_loud_burst_above_ambient() {
        let mut g = EnergyGate::new(12, 2.0);
        for _ in 0..64 {
            let _ = g.update(0.04);
        }
        assert!(g.update(0.30), "a burst ~7x the ambient floor must pass");
    }

    /// Far-field bias: with the shipped 2x margin, speech only ~2.5x above the
    /// ambient floor (across-the-room quiet) still passes.
    #[test]
    fn energy_admits_far_field_at_low_ratio() {
        let mut g = EnergyGate::new(12, 2.0);
        for _ in 0..64 {
            let _ = g.update(0.04);
        }
        assert!(g.update(0.10), "far-field ~2.5x ambient must clear the 2x margin");
    }

    /// The absolute epsilon safety net: in a dead-silent room the tracked floor
    /// approaches zero, but a still-tiny peak must not be admitted just because
    /// its ratio to ~0 is large.
    #[test]
    fn energy_epsilon_blocks_near_silence() {
        let mut g = EnergyGate::new(12, 2.0);
        for _ in 0..64 {
            let _ = g.update(0.0);
        }
        assert!(
            !g.update(WAKE_ENERGY_EPSILON * 0.5),
            "a peak below the absolute epsilon must be rejected even at a huge ratio"
        );
    }

    /// The peak window catches a bursty crossing even after it has passed: a
    /// single loud hop keeps the window "hot" for the next few hops.
    #[test]
    fn energy_peak_persists_across_the_window() {
        let mut g = EnergyGate::new(3, 2.0);
        for _ in 0..64 {
            let _ = g.update(0.04);
        }
        assert!(g.update(0.30), "loud hop passes");
        // The next hop is quiet, but the loud sample is still within the 3-hop
        // peak window, so the gate stays open.
        assert!(g.update(0.04), "the recent-peak window keeps the gate open");
    }
}
