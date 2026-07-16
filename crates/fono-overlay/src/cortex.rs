// SPDX-License-Identifier: GPL-3.0-only
//! Glas Cortex — the "watch it think" LED bar (`WaveformStyle::Cortex`).
//!
//! A wordless, event-driven visualizer rendered as a fixed **6×46 LED
//! grid**: a Rust port of the reference `cortex-live-engine.js` from
//! the 2026-07 design (see `plans/2026-07-15-glas-cortex-rewrite-v1.md`
//! and the implementation spec it was written against). It makes a
//! running language model legible at a glance — reading vs writing,
//! how deep/hard it is working, which experts it routes through (MoE),
//! and how confident it is — without a single label.
//!
//! ## Visual language
//!
//! - **X (46 cols) = network depth**: col 0 = first layer, col 45 =
//!   last. The model's real layer count is *mapped onto* the fixed
//!   grid (`layer(col) = round(col/(COLS-1)·(nLayer-1))`) — the grid
//!   never resizes to the layer count.
//! - **A pulse sweeping left→right = one token being computed.** The
//!   head is rendered as crisp *cells* (three stepped tiers), never a
//!   blur.
//! - **Prefill = a wide cool flood** (indigo→cyan) across all columns:
//!   many prompt tokens ingested in parallel, one inhale.
//!   **Decode = a single warm (ember) pulse per token** — the
//!   autoregressive loop made visible.
//! - **Dense rows = an equalizer**: per column, rows fill center-out
//!   proportional to that layer's log-normalized activation norm.
//!   **MoE rows = 6 expert lanes**: only the routed lanes spark
//!   (`lane = id % 6`), the rest of the column stays dark — the
//!   sparsity story.
//! - **Confidence lives in the pulse itself**: brightness
//!   `0.5 + 0.5·token_prob`; high entropy desaturates the column
//!   toward grey. No separate edge flash.
//!
//! ## Timing — a human-pace metronome over real data
//!
//! Real decode runs 20–100+ tok/s and real keyframes arrive sparsely
//! (the tap strides tokens and the governor can widen the interval so
//! far that a whole reply carries a single keyframe), so the clock is
//! a steady **metronome at ~3 pulses/s** — the pace of the reference
//! web demo — that never stops while a reply is live or its audio is
//! playing. The *timing* is grounded in the two signals we always
//! capture: the real decoded **token count** and the real **audio
//! duration**. During playback the retained trace is revealed in
//! `token_index` order, paced so the reveal spans the utterance;
//! between real keyframes a *carry* sweep re-shows the last-known real
//! state (with subtle per-token texture so repeats never look frozen).
//! Waiting on the first token (prefill compute) shows a slow cool
//! scan. Nothing is fabricated — carries and scans only re-animate
//! captured state — but the rhythm is continuous.
//!
//! ## Never-dead behavior
//!
//! The brightness field decays as `exp(-dt/0.30)` so pulses stay
//! crisp; between sweeps a dim (~0.17) breathing **resting field** of
//! the last-known layer norms / expert routing keeps the bar alive
//! through real capture gaps (the tap strides layers and the governor
//! widens intervals); idle shows a slow breath drifting across the
//! columns.
//!
//! ## Traceless (cloud) fallback
//!
//! When a *local* assistant turn runs on a backend that produces no
//! `brain_tap` keyframes (a cloud model), the bar would otherwise sit
//! idle through the whole reply. After a short grace window it instead
//! drives a **simulated MoE** sweep — sparse expert lanes drifting
//! across depth and time — so the panel reads as an active, routing
//! network. This is the one path that is *not* grounded in real
//! activity; it engages only when no real trace exists and never for
//! network requests (which don't move the overlay into a busy phase).
//!
//! ## State / draw split
//!
//! [`CortexState`] owns everything animated and is advanced by
//! [`CortexState::tick`] (renderer FFT push + animation pump) and fed
//! by [`CortexState::apply`] / [`CortexState::on_state`];
//! [`draw_cortex`] is a pure read of the `field[6][46]` brightness
//! array plus the in-flight pulse heads. Colors come from two fixed
//! ramps (cool intake / warm compute) drawn onto a fully transparent
//! panel — each tile's opacity tracks its brightness so unlit cells
//! disappear and only lit LEDs float over the desktop. The per-state
//! accent is deliberately not used.

// Readable visualisation math beats `mul_add` chains, and the
// fixed-grid rasteriser genuinely wants plain casts.
#![allow(
    clippy::suboptimal_flops,
    clippy::many_single_char_names,
    clippy::too_many_arguments,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use std::collections::VecDeque;

use crate::{CortexCmd, CortexModelKind, OverlayState};

/// Fixed grid columns (network depth axis).
pub const COLS: usize = 46;
/// Fixed grid rows (equalizer height / expert lanes).
pub const ROWS: usize = 6;

/// Field decay time constant (seconds): `field *= exp(-dt/τ)`.
const FIELD_TAU: f32 = 0.30;
/// Largest dt fed to the clock — a stalled compositor delivering one
/// huge step must not skip the whole show (mirrors the JS `dt > 0.1`
/// clamp).
const MAX_TICK_DT: f32 = 0.1;
/// Resting-field amplitude (dim breathing floor of held state).
const REST_AMP: f32 = 0.17;
/// Metronome beat: one sweep fires every beat while a reply is live
/// or playing (≈3 pulses/s — the "human-relatable thinking pace" of
/// the reference web demo at its ~3 tok/s setting).
const BEAT: f32 = 0.34;
/// Grace period (seconds) after entering Thinking with no real
/// keyframes before the simulated-MoE fallback kicks in. Local
/// embedded turns publish `ReplyBegin` within a few hundred ms, so
/// this window keeps the simulation off for grounded turns and only
/// engages it for traceless (cloud) backends.
const SIM_GRACE: f32 = 0.7;
/// Speaking cursor speed cap (tokens/second). Bounds how fast the
/// monotonic playback cursor may advance so a mid-reply correction
/// (audio/total revealed late) eases in smoothly instead of
/// fast-forwarding through the trace.
const PLAY_MAX_TPS: f32 = 12.0;
/// Floor on the "remaining audio" estimate (seconds) when pacing the
/// Speaking cursor, so an under-reported audio length can't blow the
/// velocity up to infinity.
const PLAY_MIN_TAIL: f32 = 0.5;
/// Decode sweep duration — slightly longer than a beat so
/// consecutive sweeps overlap into continuous motion.
const DECODE_DUR: f32 = 0.46;
/// Prefill flood duration (one fast pass, seconds).
const PREFILL_DUR: f32 = 0.62;
/// Draw cutoff: cells below this brightness stay fully transparent.
const DRAW_FLOOR: f32 = 0.02;
/// Listening: noise floor subtracted from normalised mic bins so a
/// quiet room settles dark instead of shimmering mid-band.
const SPEC_NOISE_FLOOR: f32 = 0.16;
/// Listening: EMA toward each column's target energy.
const SPEC_EMA: f32 = 0.5;

/// Cool ramp (intake / prefill / idle): `#0c0c28 → #222a96 → #2882d6
/// → #3ce0d6 → #e4fcff`.
const RAMP_COOL: [(f32, [f32; 3]); 5] = [
    (0.00, [12.0, 12.0, 40.0]),
    (0.30, [34.0, 42.0, 150.0]),
    (0.58, [40.0, 130.0, 214.0]),
    (0.80, [60.0, 224.0, 214.0]),
    (1.00, [228.0, 252.0, 255.0]),
];
/// Warm ramp (active compute / decode — the Fono ember): `#1a0c22 →
/// #782860 → #d9342f → #ff8b5e → #fff7ec`.
const RAMP_WARM: [(f32, [f32; 3]); 5] = [
    (0.00, [26.0, 12.0, 34.0]),
    (0.28, [120.0, 40.0, 96.0]),
    (0.55, [217.0, 52.0, 47.0]),
    (0.80, [255.0, 139.0, 94.0]),
    (1.00, [255.0, 247.0, 236.0]),
];

/// Piecewise-linear ramp lookup.
fn ramp(stops: &[(f32, [f32; 3])], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    for i in 1..stops.len() {
        if t <= stops[i].0 {
            let (t0, c0) = stops[i - 1];
            let (t1, c1) = stops[i];
            let f = (t - t0) / (t1 - t0).max(1e-6);
            return [
                c0[0] + (c1[0] - c0[0]) * f,
                c0[1] + (c1[1] - c0[1]) * f,
                c0[2] + (c1[2] - c0[2]) * f,
            ];
        }
    }
    stops[stops.len() - 1].1
}

/// Deterministic hash noise in `0..1` (the JS engine's `h1`).
fn h1(n: f32) -> f32 {
    let s = (n * 127.1).sin() * 43_758.547;
    s - s.floor()
}

/// Column → real layer index for an `n`-layer model (spec §2).
fn col_to_layer(c: usize, n: usize) -> usize {
    if n <= 1 {
        return 0;
    }
    ((c as f32 / (COLS - 1) as f32) * (n - 1) as f32).round() as usize
}

/// Pipeline phase, derived from [`OverlayState`] by
/// [`CortexState::on_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Phase {
    /// Overlay hidden — slow breath, decay to dark.
    #[default]
    Idle,
    /// Mic is hot: the live FFT drives a cool equalizer.
    Listening,
    /// Prompt submitted / generation burst / TTS synth in flight:
    /// prefill floods and the first decode pulses play here.
    Thinking,
    /// Reply audio is playing: grounded replay paced to span it.
    Speaking,
}

/// Sweep flavor — selects ramp, mood and per-column deposit shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PulseKind {
    Prefill,
    Decode,
}

/// One left→right compute front in flight.
#[derive(Debug, Clone)]
struct Pulse {
    kind: PulseKind,
    /// Birth timestamp on [`CortexState::clock`].
    born: f32,
    /// Seconds for the head to cross the grid.
    dur: f32,
    /// Head column position last tick (fractional; starts left of 0).
    last_head: f32,
    /// Per-column brightness payload (dense magnitude / prefill
    /// texture; for MoE it just marks active columns).
    profile: [f32; COLS],
    /// MoE: lit `(lane, brightness)` pairs per column.
    moe: Option<Vec<Vec<(usize, f32)>>>,
    /// Normalised entropy (0..1) — desaturates the columns it paints.
    entropy: f32,
    /// Peak amplitude (`0.5 + 0.5·token_prob` for decode).
    amp: f32,
}

/// One captured keyframe queued for grounded replay.
#[derive(Debug, Clone)]
struct QueuedFrame {
    token: u64,
    norms: Vec<f32>,
    /// `(layer, ids, weights)` triples (MoE only).
    experts: Vec<(u32, Vec<i32>, Vec<f32>)>,
    prob: f32,
    entropy_bits: f32,
}

/// Animated state for the Glas Cortex style. `Default` gives a dark,
/// idle panel; the renderer feeds it via [`Self::on_state`],
/// [`Self::apply`] and [`Self::tick`].
pub struct CortexState {
    phase: Phase,
    /// Monotonic animation clock (seconds), advanced by ticks.
    clock: f32,
    /// Wall-clock anchor for real dt between ticks.
    last_tick: Option<std::time::Instant>,
    /// The LED brightness field (rows × cols).
    field: [[f32; COLS]; ROWS],
    /// Per-column mood: 0 = cool ramp, 1 = warm ramp.
    mood: [f32; COLS],
    /// Per-column normalised entropy (drives desaturation).
    ent: [f32; COLS],
    pulses: Vec<Pulse>,
    /// Last-known per-layer norms, merged across strided frames.
    held: Vec<f32>,
    /// Last-known routed experts per layer: `(ids, weights)`.
    held_exp: Vec<Option<(Vec<i32>, Vec<f32>)>>,
    n_layer: usize,
    kind: CortexModelKind,
    n_experts_total: Option<u32>,
    n_experts_active: Option<u32>,
    /// Running log-norm normalisation band (winsorised: the first
    /// frame's values are excluded so a BOS outlier can't pin it).
    log_min: f32,
    log_max: f32,
    saw_first: bool,
    /// True between `ReplyBegin` and the replay queue draining after
    /// `ReplyEnd` — the resting field shows only during a reply.
    reply_active: bool,
    /// Grounded-replay queue + schedule (on [`Self::clock`]).
    queue: VecDeque<QueuedFrame>,
    next_fire_at: f32,
    /// Full keyframe trace of the current reply, retained so the
    /// Speaking phase can replay the whole show paced to the
    /// utterance (the live queue drains during Thinking).
    trace: Vec<QueuedFrame>,
    /// Fired-frame count (real keyframes only; telemetry + tests).
    fired: u64,
    /// Real keyframes consumed since playback started (Speaking
    /// pacing: spread the trace across the reply audio).
    replay_fired: usize,
    /// Last real keyframe's confidence / normalised entropy — carry
    /// sweeps reuse them so the rhythm stays grounded.
    last_prob: f32,
    last_entropy: f32,
    /// Cumulative reply audio enqueued (seconds) + playback state.
    audio_secs: f32,
    playback_done: bool,
    /// Real decoded token count for the current reply (`ReplyEnd`).
    /// Anchors the Speaking reveal cadence: the sparse trace is spread
    /// across this many tokens so its pacing matches the real reply
    /// length regardless of how few keyframes the governor let through.
    total_tokens: u64,
    /// Clock timestamp when the Speaking phase began.
    speak_start: f32,
    /// Monotonic playback position in token space (Speaking). Advances
    /// every beat toward `total_tokens` at a velocity re-derived from
    /// the *current* (still-growing) audio length, and is clamped so it
    /// never jumps backward — TTS synthesises sentence-by-sentence while
    /// generation is still running, so both `audio_secs` and
    /// `total_tokens` climb throughout the reply; pacing the morph off a
    /// monotonic cursor removes the lurch that recomputing position
    /// from the raw ratio produced.
    play_pos: f32,
    /// Listening: smoothed per-column mic energy.
    spec: [f32; COLS],
    /// Clock timestamp when the overlay entered the Thinking/Speaking
    /// group (spanning the whole busy stretch, not reset on the
    /// Thinking→Speaking hand-off). `None` when idle/listening. Gates
    /// the simulated fallback so it only engages after `SIM_GRACE`.
    busy_since: Option<f32>,
    /// Synthetic token counter driving the simulated-MoE fallback
    /// (traceless cloud backends): advances one per beat so the
    /// routing pattern drifts across depth and time.
    sim_tok: f32,
}

impl Default for CortexState {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            clock: 0.0,
            last_tick: None,
            field: [[0.0; COLS]; ROWS],
            mood: [0.0; COLS],
            ent: [0.0; COLS],
            pulses: Vec::new(),
            held: Vec::new(),
            held_exp: Vec::new(),
            n_layer: 24,
            kind: CortexModelKind::Dense,
            n_experts_total: None,
            n_experts_active: None,
            log_min: f32::INFINITY,
            log_max: f32::NEG_INFINITY,
            saw_first: false,
            reply_active: false,
            queue: VecDeque::new(),
            next_fire_at: 0.0,
            trace: Vec::new(),
            fired: 0,
            replay_fired: 0,
            last_prob: 0.7,
            last_entropy: 0.3,
            audio_secs: 0.0,
            playback_done: false,
            total_tokens: 0,
            speak_start: 0.0,
            play_pos: 0.0,
            spec: [0.0; COLS],
            busy_since: None,
            sim_tok: 0.0,
        }
    }
}

impl CortexState {
    /// Full reset (style swap / overlay teardown).
    pub fn clear(&mut self) {
        *self = Self { last_tick: self.last_tick, ..Self::default() };
    }

    /// Reset the per-reply replay state, keeping in-flight pulses and
    /// the lit field: the assistant/polish backends publish `Prefill`
    /// *before* `ReplyBegin`, and cutting that flood mid-pass would
    /// eat the visible "reading" beat.
    fn clear_reply(&mut self) {
        self.held.clear();
        self.held_exp.clear();
        self.log_min = f32::INFINITY;
        self.log_max = f32::NEG_INFINITY;
        self.saw_first = false;
        self.queue.clear();
        self.next_fire_at = self.clock;
        self.trace.clear();
        self.replay_fired = 0;
        self.audio_secs = 0.0;
        self.playback_done = false;
        self.total_tokens = 0;
        self.play_pos = 0.0;
        self.sim_tok = 0.0;
    }

    /// Track the overlay state so the phase machine follows the
    /// pipeline: listening (mic hot) → thinking (generation burst) →
    /// speaking (playback replay). Called on every `SetState`.
    pub fn on_state(&mut self, state: OverlayState) {
        use OverlayState as S;
        let phase = match state {
            S::Hidden => Phase::Idle,
            S::AssistantThinking
            | S::AssistantSynthesising
            | S::Processing
            | S::Polishing { .. } => Phase::Thinking,
            S::AssistantSpeaking => Phase::Speaking,
            _ => Phase::Listening,
        };
        if phase == Phase::Speaking && self.phase != Phase::Speaking {
            self.speak_start = self.clock;
            self.begin_playback_replay();
        }
        // Track the busy stretch (Thinking or Speaking) as one span so
        // the simulated-MoE fallback survives the Thinking→Speaking
        // hand-off; clear it when we return to idle/listening.
        let was_busy = matches!(self.phase, Phase::Thinking | Phase::Speaking);
        let now_busy = matches!(phase, Phase::Thinking | Phase::Speaking);
        if now_busy && !was_busy {
            self.busy_since = Some(self.clock);
            self.sim_tok = 0.0;
        } else if !now_busy {
            self.busy_since = None;
        }
        if phase != self.phase {
            tracing::debug!(
                "cortex: phase {:?} -> {phase:?} (queue={} trace={} held={})",
                self.phase,
                self.queue.len(),
                self.trace.len(),
                self.held.len()
            );
        }
        self.phase = phase;
    }

    /// Reply audio is starting: reload the full retained trace so the
    /// whole decode show replays paced to span the utterance. Without
    /// this the live queue (drained during Thinking) would leave the
    /// Speaking phase a static resting floor.
    fn begin_playback_replay(&mut self) {
        // Speaking replays the retained real trace (paced by
        // `beat_speaking` across the reply audio); reset the reveal
        // cursor and drop any live queue left over from the Thinking
        // pass — the trace already holds every captured frame.
        self.queue.clear();
        self.replay_fired = 0;
        self.play_pos = 0.0;
        self.next_fire_at = self.clock;
    }

    /// Ingest one replay command from the orchestrator.
    pub fn apply(&mut self, cmd: CortexCmd) {
        match cmd {
            CortexCmd::ReplyBegin { n_layer, kind, n_experts_total, n_experts_active } => {
                tracing::debug!(
                    "cortex: reply_begin n_layer={n_layer} kind={kind:?} \
                     experts={n_experts_total:?}/{n_experts_active:?}"
                );
                self.clear_reply();
                self.reply_active = true;
                if n_layer > 0 {
                    self.n_layer = n_layer as usize;
                }
                self.kind = kind;
                self.n_experts_total = n_experts_total;
                self.n_experts_active = n_experts_active;
            }
            CortexCmd::Prefill { n_tokens } => {
                tracing::debug!("cortex: prefill n_tokens={n_tokens}");
                // A prefill event is only published when the tap is armed
                // (a grounded local turn), so it marks the reply live and
                // suppresses the traceless simulated fallback.
                self.reply_active = true;
                self.fire_prefill(n_tokens);
            }
            CortexCmd::Frame(f) => {
                // A frame implies a live reply even if `ReplyBegin` got
                // lost (defensive; the resting field needs the flag).
                self.reply_active = true;
                if self.held.len() < f.layer_norms.len() {
                    self.n_layer = self.n_layer.max(f.layer_norms.len());
                }
                self.queue.push_back(QueuedFrame {
                    token: f.token_index,
                    norms: f.layer_norms,
                    experts: f.experts.into_iter().map(|e| (e.layer, e.ids, e.weights)).collect(),
                    prob: f.token_prob.unwrap_or(0.7).clamp(0.0, 1.0),
                    entropy_bits: f.entropy_bits.unwrap_or(2.0).max(0.0),
                });
                // Retain for the Speaking replay (bounded; the tap
                // itself caps a reply at 256 keyframes).
                if self.trace.len() < 512 {
                    if let Some(frame) = self.queue.back() {
                        self.trace.push(frame.clone());
                    }
                }
                tracing::trace!(
                    "cortex: frame token={} queue={} trace={}",
                    self.queue.back().map_or(0, |f| f.token),
                    self.queue.len(),
                    self.trace.len()
                );
            }
            CortexCmd::ReplyEnd { total_tokens, gen_ms, .. } => {
                self.total_tokens = total_tokens;
                tracing::debug!(
                    "cortex: reply_end total_tokens={total_tokens} gen_ms={gen_ms} \
                     trace={} queue={}",
                    self.trace.len(),
                    self.queue.len()
                );
            }
            CortexCmd::AudioTotal { secs } => {
                if secs.is_finite() {
                    self.audio_secs = self.audio_secs.max(secs.max(0.0));
                    tracing::debug!("cortex: audio_total {:.2}s", self.audio_secs);
                }
            }
            // The LED grammar is driven by compute events; the voice
            // itself is what the user *hears*, so its spectrum is
            // deliberately not double-encoded here.
            CortexCmd::AudioBands { .. } => {}
            CortexCmd::PlaybackDone => {
                tracing::debug!("cortex: playback_done (queue={})", self.queue.len());
                self.playback_done = true;
            }
        }
    }

    /// Advance the animation. `bins` is the live mic FFT during
    /// listening (empty from the timer-driven animation pump). Uses
    /// real wall-clock dt, clamped so a stalled compositor can't skip
    /// the show.
    pub fn tick(&mut self, bins: &[f32]) {
        let now = std::time::Instant::now();
        let dt = self
            .last_tick
            .map_or(1.0 / 60.0, |t| now.duration_since(t).as_secs_f32())
            .min(MAX_TICK_DT);
        self.last_tick = Some(now);
        self.tick_dt(bins, dt);
    }

    /// Deterministic-dt tick (tests, gallery, bench).
    pub fn tick_dt(&mut self, bins: &[f32], dt: f32) {
        let dt = dt.clamp(0.0, MAX_TICK_DT);
        self.clock += dt;

        // Field decay: fast → crisp pulses, legible cadence.
        let decay = (-dt / FIELD_TAU).exp();
        for row in &mut self.field {
            for v in row.iter_mut() {
                *v *= decay;
            }
        }

        // The metronome: one sweep per beat, continuously, while a
        // reply is live or its audio plays — a steady human-relatable
        // "thinking pace" regardless of how sparse the real keyframes
        // are. Real keyframes are consumed once, in order; between
        // them, carry sweeps re-show the last-known real state.
        if matches!(self.phase, Phase::Thinking | Phase::Speaking)
            && self.clock >= self.next_fire_at
        {
            if self.reply_active {
                self.beat();
            } else if self.simulating() {
                // Traceless (cloud) backend: no real keyframes will
                // arrive, so drive a plausible simulated-MoE sweep so
                // the bar still "thinks" while the remote model works.
                self.beat_sim();
            }
        }

        // Advance pulses; deposit each newly crossed column.
        let mut i = 0;
        while i < self.pulses.len() {
            let age = self.clock - self.pulses[i].born;
            let prog = age / self.pulses[i].dur.max(1e-3);
            // Head runs slightly past the right edge so the last
            // column ignites.
            let head = prog * (COLS as f32 - 1.0 + 3.0) - 1.5;
            let from = (self.pulses[i].last_head.floor() as i64 + 1).max(0) as usize;
            let to = head.floor().min(COLS as f32 - 1.0);
            if to >= 0.0 {
                for c in from..=(to as usize) {
                    self.deposit_column(i, c);
                }
            }
            self.pulses[i].last_head = head;
            if prog < 1.25 {
                i += 1;
            } else {
                self.pulses.swap_remove(i);
            }
        }

        match self.phase {
            Phase::Idle => {
                if self.pulses.is_empty() {
                    self.idle_breath();
                }
            }
            Phase::Listening => {
                if !bins.is_empty() {
                    self.ingest_spectrum(bins);
                }
                self.listening_field();
            }
            Phase::Thinking | Phase::Speaking => {
                // Resting field: a dim, breathing floor showing the
                // last-known state between sweeps. Real traces sample
                // sparsely, leaving long true gaps — this keeps the
                // panel alive with grounded held state instead of
                // going black. Sweeps ride over it.
                if self.reply_active && !self.held.is_empty() {
                    self.resting_floor();
                } else if self.pulses.is_empty() {
                    self.idle_breath();
                }
            }
        }
    }

    /// Whether the backend should pump timer frames (no external data
    /// push would otherwise trigger repaints). Listening self-drives
    /// from the mic FFT push; Idle is hidden.
    #[must_use]
    pub fn needs_animation_frames(&self) -> bool {
        matches!(self.phase, Phase::Thinking | Phase::Speaking)
    }

    /// Frames replayed so far this session (tests/telemetry).
    #[must_use]
    pub fn frames_fired(&self) -> u64 {
        self.fired
    }

    // ---- event → pulse ------------------------------------------------

    /// Prefill flood: fills every column, all rows, cool ramp, one
    /// fast pass. Amplitude grows with the log of the batch width.
    fn fire_prefill(&mut self, n_tokens: u32) {
        let amp = ((n_tokens.max(1) as f32).ln_1p() / 400.0_f32.ln()).clamp(0.0, 1.0);
        let mut profile = [0.0_f32; COLS];
        for (c, p) in profile.iter_mut().enumerate() {
            *p = 0.72 + 0.28 * h1(c as f32 * 3.7);
        }
        self.pulses.push(Pulse {
            kind: PulseKind::Prefill,
            born: self.clock,
            dur: PREFILL_DUR,
            last_head: -1.0,
            profile,
            moe: None,
            entropy: 0.25,
            amp: 0.7 + 0.3 * amp,
        });
    }

    /// Whether the simulated-MoE fallback should drive the beat: the
    /// overlay is busy (Thinking/Speaking) but no real reply keyframes
    /// have arrived, and the grace window has elapsed. This only ever
    /// engages for a *local* assistant turn on a traceless backend —
    /// network requests never move the overlay into a busy phase, and
    /// grounded (embedded) turns set `reply_active` before the grace
    /// window ends.
    fn simulating(&self) -> bool {
        !self.reply_active
            && matches!(self.phase, Phase::Thinking | Phase::Speaking)
            && self.busy_since.is_some_and(|s| self.clock - s > SIM_GRACE)
    }

    /// Simulated decode beat for traceless backends (cloud models with
    /// no `brain_tap` keyframes). Fires a sparse MoE-style expert-lane
    /// sweep whose routing drifts across depth (columns) and time
    /// (`sim_tok`), so the bar reads as an active, sparsely-routing
    /// network. This is explicitly *not* grounded in real activity —
    /// it is a stand-in shown only when no real trace exists, so the
    /// panel isn't dead while a cloud model is working.
    fn beat_sim(&mut self) {
        self.next_fire_at = self.clock + BEAT;
        self.sim_tok += 1.0;
        let t = self.sim_tok;
        let mut profile = [0.0_f32; COLS];
        let mut cols: Vec<Vec<(usize, f32)>> = Vec::with_capacity(COLS);
        for (c, p) in profile.iter_mut().enumerate() {
            let ph = c as f32 * 0.37 + t * 0.55;
            let mut lanes: Vec<(usize, f32)> = Vec::new();
            // ~83% of columns route (the rest stay dark → sparsity).
            if h1(ph * 5.0 + 1.0) >= 0.17 {
                let primary = (h1(ph) * ROWS as f32) as usize % ROWS;
                lanes.push((primary, 0.62 + 0.38 * h1(ph * 1.7)));
                // Occasional co-active second expert.
                if h1(ph * 2.3 + 4.0) > 0.6 {
                    let off = 1 + (h1(ph * 3.1) * (ROWS as f32 - 2.0)) as usize;
                    lanes.push(((primary + off) % ROWS, 0.4 + 0.3 * h1(ph * 0.9)));
                }
            }
            *p = if lanes.is_empty() { 0.0 } else { 1.0 };
            cols.push(lanes);
        }
        self.pulses.push(Pulse {
            kind: PulseKind::Decode,
            born: self.clock,
            dur: DECODE_DUR,
            last_head: -1.0,
            profile,
            moe: Some(cols),
            entropy: 0.2,
            amp: 0.85,
        });
    }

    /// One metronome beat. Thinking replays live keyframes as they
    /// arrive; Speaking reveals the retained real trace paced across
    /// the reply audio. Both fall back to a grounded carry sweep so a
    /// beat is never silent.
    fn beat(&mut self) {
        match self.phase {
            Phase::Speaking => self.beat_speaking(),
            _ => self.beat_thinking(),
        }
    }

    /// Live pass (Thinking / Synthesising): fire the next captured
    /// keyframe as it arrives, one per beat; between them a grounded
    /// carry sweep, or a cool "still reading" scan before the first
    /// token.
    fn beat_thinking(&mut self) {
        if let Some(f) = self.queue.pop_front() {
            self.fire_decode(&f);
            self.next_fire_at = self.clock + BEAT;
            return;
        }
        if self.held.is_empty() {
            // Waiting on the first token (prefill compute): a slower
            // cool scan — "still reading".
            self.fire_wait_scan();
            self.next_fire_at = self.clock + 2.0 * BEAT;
        } else {
            // Carry: re-sweep the last-known real state so the rhythm
            // never breaks between sparse keyframes / during synth.
            self.launch_decode_pulse(self.last_prob, self.last_entropy, 0.88, self.clock * 3.0);
            self.next_fire_at = self.clock + BEAT;
        }
    }

    /// Playback pass (Speaking): the equalizer **morphs continuously**
    /// through the retained real trace, paced so the show spans the
    /// reply audio — grounded in the real token count + real audio
    /// duration. The playback cursor (`play_pos`, token space) is
    /// **monotonic**: every beat it advances toward the best-known
    /// `total` at a velocity re-derived from the remaining audio, and it
    /// never moves backward. This matters because TTS synthesises
    /// sentence-by-sentence while generation is still running, so both
    /// `audio_secs` and `total_tokens` climb *during* playback — pacing
    /// off the raw ratio made the cursor lurch forward then snap back;
    /// the cursor only ever eases its speed, never its position.
    fn beat_speaking(&mut self) {
        self.next_fire_at = self.clock + BEAT;
        if self.trace.is_empty() {
            // No real data at all: keep the rhythm with a carry/scan.
            if self.held.is_empty() {
                self.fire_wait_scan();
            } else {
                let seed = self.speak_start + self.clock * 0.5;
                self.launch_decode_pulse(self.last_prob, self.last_entropy, 0.9, seed);
            }
            return;
        }
        let last_tok = self.trace.last().map_or(0, |f| f.token);
        let total = self.total_tokens.max(last_tok + 1).max(1) as f32;
        if self.playback_done {
            // Audio finished: stop stretching — drain any remaining real
            // anchors one per beat and let the cursor track the last one
            // revealed, so the show completes promptly.
            if self.replay_fired < self.trace.len() {
                let f = self.trace[self.replay_fired].clone();
                self.play_pos = f.token as f32;
                self.merge_anchor(&f);
                self.replay_fired += 1;
            } else {
                self.play_pos = total;
            }
        } else {
            // Advance the monotonic cursor toward `total`. Velocity is
            // "remaining tokens / remaining audio", so as the audio
            // length grows the cursor eases off instead of snapping back;
            // a floor on the remaining audio keeps it finite when the
            // audio was under-reported, and a cap keeps a big correction
            // from reading as a fast-forward blur.
            let elapsed = (self.clock - self.speak_start).max(0.0);
            let remaining_tokens = (total - self.play_pos).max(0.0);
            let remaining_audio = (self.audio_secs - elapsed).max(PLAY_MIN_TAIL);
            let vel = (remaining_tokens / remaining_audio).min(PLAY_MAX_TPS);
            self.play_pos = (self.play_pos + vel * BEAT).min(total);
            // Reveal every anchor the cursor has now passed — updates
            // routing, confidence and the log-norm band — *without* each
            // firing its own sweep; the morph pulse below carries the
            // visible shape.
            while self.replay_fired < self.trace.len()
                && self.trace[self.replay_fired].token as f32 <= self.play_pos
            {
                let f = self.trace[self.replay_fired].clone();
                self.merge_anchor(&f);
                self.replay_fired += 1;
            }
        }
        let pos = self.play_pos;
        if self.held.is_empty() {
            self.fire_wait_scan();
            return;
        }
        // Time-interpolated dense shape at `pos`: smooth morph through
        // the real captures (MoE ignores this and reads revealed lanes).
        let norms = self.morph_norms_at(pos);
        let seed = self.speak_start + self.clock;
        self.launch_decode_pulse_with_norms(&norms, self.last_prob, self.last_entropy, 1.0, seed);
    }

    /// One token's compute front: merge the (strided) frame into the
    /// held state, then launch a warm decode pulse carrying the
    /// per-column payload.
    fn fire_decode(&mut self, f: &QueuedFrame) {
        self.merge_anchor(f);
        self.launch_decode_pulse(self.last_prob, self.last_entropy, 1.0, f.token as f32);
    }

    /// Merge one captured keyframe into the running state — held norms
    /// (strided, so only the observed layers update), routed experts,
    /// the winsorised log-norm band, confidence/entropy, and the fired
    /// counter — *without* launching a sweep. Used both by
    /// [`Self::fire_decode`] and by the Speaking morph path, which
    /// reveals anchors for their routing/confidence then overwrites the
    /// dense shape with a time-interpolated frame.
    fn merge_anchor(&mut self, f: &QueuedFrame) {
        let n = self.n_layer.max(f.norms.len()).max(1);
        self.n_layer = n;
        if self.held.len() != n {
            self.held.resize(n, 0.0);
            self.held_exp.resize(n, None);
        }
        for (l, &v) in f.norms.iter().enumerate() {
            if v > 0.0 {
                self.held[l] = v;
                if self.saw_first {
                    let lv = v.ln_1p();
                    self.log_min = self.log_min.min(lv);
                    self.log_max = self.log_max.max(lv);
                }
            }
        }
        for (layer, ids, weights) in &f.experts {
            if let Some(slot) = self.held_exp.get_mut(*layer as usize) {
                *slot = Some((ids.clone(), weights.clone()));
            }
        }
        self.saw_first = true;
        self.fired += 1;
        self.last_prob = f.prob;
        self.last_entropy = (f.entropy_bits / 6.0).clamp(0.0, 1.0);
    }

    /// Per-layer real norms **time-interpolated** to playback token
    /// position `pos`. For each layer we find the trace anchors that
    /// actually observed it and interpolate that layer's value between
    /// the ones bracketing `pos` (flat before the first / after the
    /// last real observation of that layer), then spatially fill any
    /// layer never observed at all. The result evolves smoothly as
    /// `pos` advances — the equalizer morphs *through* the real
    /// captures instead of snapping between them — while every value
    /// still lies between two genuine observations of that exact layer.
    fn morph_norms_at(&self, pos: f32) -> Vec<f32> {
        let n = self.n_layer.max(1);
        let mut v = vec![0.0_f32; n];
        if self.trace.is_empty() {
            return v;
        }
        for (l, slot) in v.iter_mut().enumerate() {
            let mut prev: Option<(f32, f32)> = None;
            let mut next: Option<(f32, f32)> = None;
            for f in &self.trace {
                let val = f.norms.get(l).copied().unwrap_or(0.0);
                if val <= 0.0 {
                    continue;
                }
                let tok = f.token as f32;
                if tok <= pos {
                    prev = Some((tok, val));
                } else {
                    next = Some((tok, val));
                    break;
                }
            }
            *slot = match (prev, next) {
                (Some((pt, pv)), Some((nt, nv))) => {
                    let t = ((pos - pt) / (nt - pt).max(1e-3)).clamp(0.0, 1.0);
                    pv + (nv - pv) * t
                }
                (Some((_, pv)), None) => pv,
                (None, Some((_, nv))) => nv,
                (None, None) => 0.0,
            };
        }
        Self::spatial_fill(&mut v);
        v
    }

    /// Held per-layer norms with unobserved layers filled by linear
    /// interpolation between the nearest observed neighbours (flat at
    /// the ends). The tap observes only every `LAYER_STRIDE`-th layer
    /// per frame and, when the governor widens the interval, a whole
    /// reply may carry a single sparse frame — interpolating between
    /// the real samples fills the bar without fabricating structure
    /// (layer-output norm varies smoothly with depth), turning a
    /// sparse dozen-column flicker into a full, readable equalizer.
    fn filled_held(&self) -> Vec<f32> {
        let n = self.n_layer.max(1);
        let mut v = vec![0.0_f32; n];
        for (i, slot) in v.iter_mut().enumerate() {
            *slot = self.held.get(i).copied().unwrap_or(0.0);
        }
        Self::spatial_fill(&mut v);
        v
    }

    /// Fill zero (unobserved) entries of a per-layer vector by linear
    /// interpolation between the nearest observed neighbours, flat at
    /// the ends. Shared by [`Self::filled_held`] (spatial fill of the
    /// merged held state) and [`Self::morph_norms_at`] (after its
    /// per-layer temporal interpolation). Leaves an all-zero vector
    /// untouched.
    fn spatial_fill(v: &mut [f32]) {
        let obs: Vec<usize> = (0..v.len()).filter(|&i| v[i] > 0.0).collect();
        let (Some(&first), Some(&last)) = (obs.first(), obs.last()) else {
            return; // nothing observed yet
        };
        let (fv, lv) = (v[first], v[last]);
        v[..first].iter_mut().for_each(|x| *x = fv);
        v[last + 1..].iter_mut().for_each(|x| *x = lv);
        for w in obs.windows(2) {
            let (a, b) = (w[0], w[1]);
            if b > a + 1 {
                let (va, vb) = (v[a], v[b]);
                for (k, i) in ((a + 1)..b).enumerate() {
                    let t = (k + 1) as f32 / (b - a) as f32;
                    v[i] = va + (vb - va) * t;
                }
            }
        }
    }

    /// Launch one warm decode sweep from the current held state.
    /// `gain` dims carry sweeps slightly so fresh keyframes read
    /// stronger than re-shows; `seed` gives each sweep subtle
    /// per-token micro-texture so repeated carries never look frozen
    /// (it perturbs brightness only, never which columns are lit).
    fn launch_decode_pulse(&mut self, prob: f32, entropy: f32, gain: f32, seed: f32) {
        let filled = self.filled_held();
        self.launch_decode_pulse_with_norms(&filled, prob, entropy, gain, seed);
    }

    /// As [`Self::launch_decode_pulse`] but the dense equalizer shape is
    /// taken from an explicit per-layer `norms` slice rather than the
    /// merged held state. The Speaking morph path passes
    /// [`Self::morph_norms_at`] so the bar interpolates smoothly through
    /// the real captures; MoE ignores `norms` and reads routed lanes
    /// from the held expert state (revealed as anchors are passed).
    fn launch_decode_pulse_with_norms(
        &mut self,
        norms: &[f32],
        prob: f32,
        entropy: f32,
        gain: f32,
        seed: f32,
    ) {
        let n = self.n_layer.max(1);
        let mut profile = [0.0_f32; COLS];
        let moe = if self.kind == CortexModelKind::Moe {
            let mut cols = Vec::with_capacity(COLS);
            for (c, p) in profile.iter_mut().enumerate() {
                let l = col_to_layer(c, n);
                let lanes = self
                    .held_exp
                    .get(l)
                    .and_then(Option::as_ref)
                    .map_or_else(Vec::new, |ex| self.lanes_for(ex));
                *p = if lanes.is_empty() { 0.0 } else { 1.0 };
                cols.push(lanes);
            }
            Some(cols)
        } else {
            for (c, p) in profile.iter_mut().enumerate() {
                let l = col_to_layer(c, n);
                let mag = self.magnitude(norms.get(l).copied().unwrap_or(0.0), l, n);
                // Flowing two-octave shimmer: the noise pattern
                // *translates* with `seed` (which steps once per beat)
                // so the bar keeps churning like live compute instead of
                // reading as a uniform breath between the sparse real
                // anchors. Brightness only — it never changes which
                // columns/layers are lit, so the grounded shape stands.
                let flow = seed * 1.6;
                let churn = 0.80
                    + 0.14 * h1(c as f32 * 0.7 - flow)
                    + 0.06 * h1(c as f32 * 1.9 + flow * 0.5);
                *p = mag * churn;
            }
            None
        };
        self.pulses.push(Pulse {
            kind: PulseKind::Decode,
            born: self.clock,
            dur: DECODE_DUR,
            last_head: -1.0,
            profile,
            moe,
            entropy,
            amp: (0.5 + 0.5 * prob) * gain,
        });
    }

    /// Cool low-amplitude scan while the model is still reading the
    /// prompt (reply begun, no keyframe yet) — continuous "working on
    /// it" motion grounded in the fact that prefill is running.
    fn fire_wait_scan(&mut self) {
        let mut profile = [0.0_f32; COLS];
        for (c, p) in profile.iter_mut().enumerate() {
            *p = 0.5 + 0.3 * h1(c as f32 * 2.9 + self.clock);
        }
        self.pulses.push(Pulse {
            kind: PulseKind::Prefill,
            born: self.clock,
            dur: PREFILL_DUR * 1.4,
            last_head: -1.0,
            profile,
            moe: None,
            entropy: 0.25,
            amp: 0.42,
        });
    }

    /// Routed experts → lit `(lane, brightness)` pairs. Lane =
    /// `id % 6` with collision bumping; the lit-lane budget adapts to
    /// the model's real routing ratio when the engine reported expert
    /// counts (spec §9.1: `<8% → 1`, `8–20% → 2`, `≥20% → 3`), and a
    /// 2nd/3rd lane lights only when its real routing weight is
    /// genuinely co-active (within 60% of the top weight).
    fn lanes_for(&self, ex: &(Vec<i32>, Vec<f32>)) -> Vec<(usize, f32)> {
        let (ids, ws) = ex;
        if ids.is_empty() {
            return Vec::new();
        }
        let budget = match (self.n_experts_total, self.n_experts_active) {
            (Some(total), Some(active)) if total > 0 => {
                let ratio = active as f32 / total as f32;
                if ratio < 0.08 {
                    1
                } else if ratio < 0.20 {
                    2
                } else {
                    3
                }
            }
            _ => 3,
        };
        let k = budget.min(ids.len());
        let top_w = ws.first().copied().unwrap_or(1.0).max(1e-6);
        let mut lanes: Vec<(usize, f32)> = Vec::with_capacity(k);
        let mut used = [false; ROWS];
        #[allow(clippy::needless_range_loop)] // parallel indexing of ids+ws
        for i in 0..k {
            let real_w = ws.get(i).copied();
            // Co-activity gate only applies to real weights; when the
            // weights tensor wasn't observed, fall back to the JS
            // engine's synthetic best-first decay.
            if i > 0 {
                if let Some(w) = real_w {
                    if w < 0.6 * top_w {
                        break;
                    }
                }
            }
            let w = real_w.unwrap_or_else(|| (-0.6 * i as f32).exp());
            let mut lane = ids[i].rem_euclid(ROWS as i32) as usize;
            if used[lane] {
                if let Some(free) = (0..ROWS).map(|d| (lane + d) % ROWS).find(|&l| !used[l]) {
                    lane = free;
                }
            }
            used[lane] = true;
            lanes.push((lane, 0.45 + 0.55 * w.clamp(0.0, 1.0)));
        }
        lanes
    }

    /// Norm → magnitude 0..1: log, running-normalised within the
    /// reply, with a mild depth rise so late layers read taller.
    fn magnitude(&self, v: f32, l: usize, n: usize) -> f32 {
        if v <= 0.0 {
            return 0.0;
        }
        let lv = v.ln_1p();
        let t = if self.log_min.is_finite() && self.log_max > self.log_min {
            ((lv - self.log_min) / (self.log_max - self.log_min)).clamp(0.0, 1.0)
        } else {
            0.5
        };
        let depth = if n > 1 { l as f32 / (n - 1) as f32 } else { 0.5 };
        (0.15 + 0.85 * (0.72 * t + 0.28 * depth)).clamp(0.0, 1.0).powf(0.9)
    }

    // ---- per-tick field writers ---------------------------------------

    /// Deposit pulse `pi`'s payload into column `c` (the compute front
    /// just crossed it).
    fn deposit_column(&mut self, pi: usize, c: usize) {
        // Extract the payload up front: the pulse borrow must end
        // before the field/mood writes below.
        let (kind, amp, entropy, m, lanes) = {
            let p = &self.pulses[pi];
            let lanes = p.moe.as_ref().map(|moe| moe[c].clone());
            (p.kind, p.amp, p.entropy, p.profile[c], lanes)
        };
        self.mood[c] = if kind == PulseKind::Prefill { 0.45 } else { 1.0 };
        self.ent[c] = entropy;
        match kind {
            PulseKind::Prefill => {
                let m = m * amp;
                for r in 0..ROWS {
                    let rr = 1.0 - (r as f32 - 2.5).abs() / 2.5;
                    let v = m * (0.55 + 0.45 * rr);
                    if v > self.field[r][c] {
                        self.field[r][c] = v;
                    }
                }
            }
            PulseKind::Decode => {
                if let Some(lanes) = lanes {
                    // Sparse expert lanes + a faint ghost on unused
                    // lanes of an active column (sparsity story).
                    if lanes.is_empty() {
                        return;
                    }
                    let mut lit = [0.0_f32; ROWS];
                    for &(lane, b) in &lanes {
                        let v = b * amp;
                        if v > lit[lane] {
                            lit[lane] = v;
                        }
                    }
                    for (r, &l) in lit.iter().enumerate() {
                        let v = if l > 0.0 { l } else { 0.035 * amp };
                        if v > self.field[r][c] {
                            self.field[r][c] = v;
                        }
                    }
                } else {
                    // Dense equalizer: center-out vertical fill.
                    if m <= 0.0 {
                        return;
                    }
                    for r in 0..ROWS {
                        let dist = (r as f32 - 2.5).abs() / 2.5;
                        if m < dist * 0.72 {
                            continue; // low magnitude → only core rows
                        }
                        let v = (m * (1.0 - dist * 0.45)).clamp(0.0, 1.0) * amp;
                        if v > self.field[r][c] {
                            self.field[r][c] = v;
                        }
                    }
                }
            }
        }
    }

    /// Dim breathing floor from the held state, drawn between sweeps
    /// during a reply. Grounded: shows the *last-known* layer norms /
    /// expert routing, never synthetic activity.
    fn resting_floor(&mut self) {
        let n = self.n_layer.max(1);
        let breath = 0.8 + 0.2 * (self.clock * 2.0).sin();
        let filled = self.filled_held();
        for c in 0..COLS {
            let l = col_to_layer(c, n);
            if self.kind == CortexModelKind::Moe {
                let Some(ex) = self.held_exp.get(l).and_then(Option::as_ref) else { continue };
                let lanes = self.lanes_for(ex);
                if lanes.is_empty() {
                    continue;
                }
                let mut lit = [false; ROWS];
                for &(lane, _) in &lanes {
                    lit[lane] = true;
                }
                for (r, &on) in lit.iter().enumerate() {
                    let v = if on { REST_AMP } else { 0.03 } * breath;
                    if v > self.field[r][c] {
                        self.field[r][c] = v;
                    }
                }
            } else {
                let m = self.magnitude(filled.get(l).copied().unwrap_or(0.0), l, n);
                if m <= 0.0 {
                    continue;
                }
                for r in 0..ROWS {
                    let dist = (r as f32 - 2.5).abs() / 2.5;
                    if m < dist * 0.72 {
                        continue;
                    }
                    let v = (m * (1.0 - dist * 0.45)).clamp(0.0, 1.0) * REST_AMP * breath;
                    if v > self.field[r][c] {
                        self.field[r][c] = v;
                    }
                }
            }
        }
    }

    /// Idle: a slow breath drifting across the columns so the bar is
    /// never dead; the mood eases back toward cool.
    fn idle_breath(&mut self) {
        let b = 0.05 + 0.045 * (self.clock * 1.4).sin();
        for c in 0..COLS {
            let e = b * (0.5 + 0.5 * (self.clock * 0.9 + c as f32 * 0.5).sin());
            for r in 0..ROWS {
                let rr = 1.0 - (r as f32 - 2.5).abs() / 2.5;
                let v = e * (0.4 + 0.6 * rr) * (0.6 + 0.4 * h1(c as f32 * 2.1 + r as f32 * 5.3));
                if v > self.field[r][c] {
                    self.field[r][c] = v;
                }
            }
            self.mood[c] *= 0.96;
        }
    }

    /// Resample the live mic FFT onto the 46 columns (EMA-smoothed,
    /// noise-floor gated).
    fn ingest_spectrum(&mut self, bins: &[f32]) {
        for (c, s) in self.spec.iter_mut().enumerate() {
            let pos = c as f32 / (COLS - 1) as f32 * (bins.len() - 1) as f32;
            let i0 = pos.floor() as usize;
            let i1 = (i0 + 1).min(bins.len() - 1);
            let f = pos - i0 as f32;
            let raw = bins[i0] * (1.0 - f) + bins[i1] * f;
            let target =
                ((raw.clamp(0.0, 1.0) - SPEC_NOISE_FLOOR) / (1.0 - SPEC_NOISE_FLOOR)).max(0.0);
            *s += (target - *s) * SPEC_EMA;
        }
    }

    /// Listening scene: the mic spectrum as a cool center-out
    /// equalizer on the same grid grammar as dense decode, plus the
    /// idle shimmer underneath so silence still breathes.
    fn listening_field(&mut self) {
        self.idle_breath();
        for c in 0..COLS {
            // Brighter than unity so the mic equalizer reads with
            // presence rather than a dim shimmer; clamped after the
            // vertical falloff so peaks saturate cleanly.
            let m = self.spec[c].clamp(0.0, 1.0) * 1.25;
            if m <= 0.0 {
                continue;
            }
            for r in 0..ROWS {
                let dist = (r as f32 - 2.5).abs() / 2.5;
                if m < dist * 0.72 {
                    continue;
                }
                let v = (m * (1.0 - dist * 0.45)).clamp(0.0, 1.0);
                if v > self.field[r][c] {
                    self.field[r][c] = v;
                }
            }
            self.mood[c] = 0.0;
            self.ent[c] = 0.0;
        }
    }
}

// ---- rasteriser -------------------------------------------------------

/// Fixed 6×46 grid geometry: square cells, one uniform integer gap
/// (scaled), pixel-aligned and centred in the panel area.
struct Grid {
    cell: i32,
    gap: i32,
    ox: i32,
    oy: i32,
}

impl Grid {
    fn compute(x0: f32, y0: f32, w: f32, h: f32, scale: f32) -> Self {
        let gap = (scale.round() as i32).max(1);
        let cols = COLS as i32;
        let rows = ROWS as i32;
        let by_w = (w as i32 - (cols + 1) * gap) / cols;
        let by_h = (h as i32 - (rows + 1) * gap) / rows;
        let cell = by_w.min(by_h).max(2);
        let total_w = cols * cell + (cols + 1) * gap;
        let total_h = rows * cell + (rows + 1) * gap;
        let ox = (x0 + (w - total_w as f32) * 0.5).round() as i32 + gap;
        let oy = (y0 + (h - total_h as f32) * 0.5).round() as i32 + gap;
        Self { cell, gap, ox, oy }
    }

    /// Top-left pixel of cell `(col, row)`.
    fn cell_origin(&self, col: usize, row: usize) -> (i32, i32) {
        let step = self.cell + self.gap;
        (self.ox + col as i32 * step, self.oy + row as i32 * step)
    }
}

/// Brightness → tile opacity. Dim cells fade toward transparent so
/// the near-black bottom of each ramp never paints as a solid black
/// square; bright cells reach full opacity and read as crisp LEDs.
/// The gentle mid-lift keeps mid-brightness tiles visible without
/// muddying the transparent look.
fn cell_alpha(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    (v * (1.7 - 0.7 * v)).clamp(0.0, 1.0)
}

/// Alpha-blend an unpremultiplied `0x00RR_GGBB` color at `alpha` over
/// the premultiplied-ARGB buffer.
fn blend_rect(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    x: i32,
    y: i32,
    w: i32,
    hh: i32,
    rgb: u32,
    alpha: f32,
) {
    let a = alpha.clamp(0.0, 1.0);
    let sr = ((rgb >> 16) & 0xFF) as f32 * a;
    let sg = ((rgb >> 8) & 0xFF) as f32 * a;
    let sb = (rgb & 0xFF) as f32 * a;
    let sa = a * 255.0;
    let inv = 1.0 - a;
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w).min(stride as i32);
    let y1 = (y + hh).min(h as i32);
    for yy in y0..y1 {
        let row = yy as usize * stride as usize;
        for xx in x0..x1 {
            let d = buf[row + xx as usize];
            let da = ((d >> 24) & 0xFF) as f32;
            let dr = ((d >> 16) & 0xFF) as f32;
            let dg = ((d >> 8) & 0xFF) as f32;
            let db = (d & 0xFF) as f32;
            let oa = (sa + da * inv).min(255.0) as u32;
            let or = (sr + dr * inv).min(255.0) as u32;
            let og = (sg + dg * inv).min(255.0) as u32;
            let ob = (sb + db * inv).min(255.0) as u32;
            buf[row + xx as usize] = (oa << 24) | (or << 16) | (og << 8) | ob;
        }
    }
}

/// Cell brightness + column mood/entropy → packed `0x00RR_GGBB`.
fn cell_color(v: f32, warm: bool, entropy: f32) -> u32 {
    let stops: &[(f32, [f32; 3])] = if warm { &RAMP_WARM } else { &RAMP_COOL };
    let mut col = ramp(stops, v);
    if entropy > 0.55 {
        // Uncertainty desaturates the column toward grey.
        let g = (col[0] + col[1] + col[2]) / 3.0;
        let k = (entropy - 0.55) * 0.55;
        for ch in &mut col {
            *ch += (g - *ch) * k;
        }
    }
    ((col[0].clamp(0.0, 255.0) as u32) << 16)
        | ((col[1].clamp(0.0, 255.0) as u32) << 8)
        | (col[2].clamp(0.0, 255.0) as u32)
}

/// Sweep-head brightness tiers: the head column at full tier plus two
/// trailing columns stepped down — crisp cells, no blur (spec §4.1).
const HEAD_TIERS: [(i32, f32); 3] = [(0, 1.0), (-1, 0.66), (-2, 0.42)];

/// Draw the Glas Cortex LED bar into the waveform strip
/// `(x0..x1, y_top..y_bot)`. Pure read of [`CortexState`]; the
/// per-state `accent` is deliberately unused — the two fixed ramps
/// (cool intake / warm compute) *are* the design.
pub fn draw_cortex(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    cortex: &CortexState,
    x0: f32,
    x1: f32,
    y_top: f32,
    y_bot: f32,
    _accent: u32,
    scale: f32,
    _elapsed_secs: f32,
) {
    let panel_w = (x1 - x0).max(1.0);
    let panel_h = (y_bot - y_top).max(1.0);
    if panel_w < 8.0 || panel_h < 8.0 {
        return;
    }
    // No near-black stage backing: unlit tiles let the panel show
    // through instead of painting an extra dark slab. Each lit tile is
    // alpha-blended by its own brightness (see `cell_alpha`), so dim
    // cells fade into the panel cleanly instead of rendering as
    // near-black opaque squares.
    let grid = Grid::compute(x0, y_top, panel_w, panel_h, scale);

    // Settled field.
    for c in 0..COLS {
        let warm = cortex.mood[c] > 0.5;
        let e = cortex.ent[c];
        for r in 0..ROWS {
            let v = cortex.field[r][c];
            if v < DRAW_FLOOR {
                continue;
            }
            let (cx, cy) = grid.cell_origin(c, r);
            blend_rect(
                buf,
                stride,
                h,
                cx,
                cy,
                grid.cell,
                grid.cell,
                cell_color(v, warm, e),
                cell_alpha(v),
            );
        }
    }

    // Sweep heads — crisp stepped cells over the field.
    for p in &cortex.pulses {
        let hc = p.last_head.round() as i32;
        let warm = p.kind == PulseKind::Decode;
        for (off, b) in HEAD_TIERS {
            let c = hc + off;
            if c < 0 || c >= COLS as i32 {
                continue;
            }
            for r in 0..ROWS {
                let rr = 1.0 - (r as f32 - 2.5).abs() / 2.5;
                let v = b * (0.55 + 0.45 * rr) * p.amp;
                if v < 0.04 {
                    continue;
                }
                let (cx, cy) = grid.cell_origin(c as usize, r);
                blend_rect(
                    buf,
                    stride,
                    h,
                    cx,
                    cy,
                    grid.cell,
                    grid.cell,
                    cell_color(v, warm, p.entropy),
                    cell_alpha(v),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CortexFrame;

    fn begin(state: &mut CortexState, n_layer: u32, kind: CortexModelKind) {
        state.apply(CortexCmd::ReplyBegin {
            n_layer,
            kind,
            n_experts_total: None,
            n_experts_active: None,
        });
    }

    fn frame(token: u64, norms: Vec<f32>, prob: f32, entropy: f32) -> CortexCmd {
        CortexCmd::Frame(CortexFrame {
            token_index: token,
            layer_norms: norms,
            experts: Vec::new(),
            token_prob: Some(prob),
            entropy_bits: Some(entropy),
        })
    }

    fn tick_for(state: &mut CortexState, secs: f32) {
        let steps = (secs / 0.05).ceil() as usize;
        for _ in 0..steps {
            state.tick_dt(&[], 0.05);
        }
    }

    #[test]
    fn layer_mapping_endpoints() {
        assert_eq!(col_to_layer(0, 40), 0);
        assert_eq!(col_to_layer(COLS - 1, 40), 39);
        assert_eq!(col_to_layer(0, 1), 0);
        assert_eq!(col_to_layer(COLS - 1, 1), 0);
        // Monotonic across the strip.
        let mut prev = 0;
        for c in 0..COLS {
            let l = col_to_layer(c, 96);
            assert!(l >= prev && l < 96);
            prev = l;
        }
    }

    #[test]
    fn grid_never_exceeds_fixed_dims_and_fits_panel() {
        for &(w, h, s) in &[(810.0, 96.0, 1.25), (640.0, 100.0, 1.0), (507.0, 67.0, 1.0)] {
            let g = Grid::compute(0.0, 0.0, w, h, s);
            let right = g.ox + COLS as i32 * (g.cell + g.gap) - g.gap;
            let bottom = g.oy + ROWS as i32 * (g.cell + g.gap) - g.gap;
            assert!(right <= w as i32, "grid overflows width at {w}x{h}");
            assert!(bottom <= h as i32, "grid overflows height at {w}x{h}");
            assert!(g.cell >= 2);
        }
    }

    #[test]
    fn keyframes_fire_once_in_order_at_the_beat() {
        let mut s = CortexState::default();
        s.on_state(OverlayState::AssistantThinking);
        begin(&mut s, 8, CortexModelKind::Dense);
        s.apply(frame(0, vec![1.0; 8], 0.9, 1.0));
        s.apply(frame(3, vec![2.0; 8], 0.8, 1.5));
        s.apply(frame(43, vec![3.0; 8], 0.7, 2.0));
        assert_eq!(s.frames_fired(), 0);
        tick_for(&mut s, 0.05);
        assert_eq!(s.frames_fired(), 1, "first keyframe fires on the first beat");
        tick_for(&mut s, 0.20);
        assert_eq!(s.frames_fired(), 1, "next beat not due yet");
        tick_for(&mut s, 0.20);
        assert_eq!(s.frames_fired(), 2, "one keyframe per beat");
        tick_for(&mut s, 0.40);
        assert_eq!(s.frames_fired(), 3);
        // Keyframes are never replayed within the pass; carry sweeps
        // keep the rhythm alive without touching the counter.
        tick_for(&mut s, 5.0);
        assert_eq!(s.frames_fired(), 3);
        assert!(!s.pulses.is_empty(), "carry sweeps keep the animation continuous");
    }

    #[test]
    fn speaking_replays_full_trace_after_thinking_drained_it() {
        let mut s = CortexState::default();
        s.on_state(OverlayState::AssistantThinking);
        begin(&mut s, 8, CortexModelKind::Dense);
        for t in 0..5 {
            s.apply(frame(t * 3, vec![1.0 + t as f32; 8], 0.8, 1.5));
        }
        // Thinking: the live pass drains the whole queue.
        tick_for(&mut s, 10.0);
        assert_eq!(s.frames_fired(), 5, "live pass fires every frame");
        // Synthesising keeps the phase machine in Thinking; then the
        // reply audio arrives and playback starts.
        s.on_state(OverlayState::AssistantSynthesising);
        s.apply(CortexCmd::AudioTotal { secs: 4.0 });
        s.on_state(OverlayState::AssistantSpeaking);
        // Speaking must NOT be a static floor: the retained trace
        // replays, paced to span the audio.
        tick_for(&mut s, 0.05);
        assert_eq!(s.frames_fired(), 6, "replay starts immediately at playback");
        tick_for(&mut s, 6.0);
        assert_eq!(s.frames_fired(), 10, "the full trace replays during playback");
        // Still never looped within the pass.
        tick_for(&mut s, 5.0);
        assert_eq!(s.frames_fired(), 10);
    }

    #[test]
    fn playback_done_drains_at_the_beat() {
        let mut s = CortexState::default();
        s.on_state(OverlayState::AssistantSpeaking);
        begin(&mut s, 4, CortexModelKind::Dense);
        for t in 0..4 {
            s.apply(frame(t * 20, vec![1.0; 4], 0.9, 1.0));
        }
        s.apply(CortexCmd::PlaybackDone);
        tick_for(&mut s, 0.05);
        assert_eq!(s.frames_fired(), 1);
        tick_for(&mut s, 3.0 * BEAT + 0.1);
        assert_eq!(s.frames_fired(), 4, "drain one per beat after playback ends");
    }

    #[test]
    fn never_black_during_active_reply() {
        let mut s = CortexState::default();
        s.on_state(OverlayState::AssistantThinking);
        begin(&mut s, 8, CortexModelKind::Dense);
        s.apply(frame(0, vec![5.0; 8], 0.9, 1.0));
        // Long gap: everything the pulse deposited decays away, but the
        // resting floor must keep the panel alive.
        tick_for(&mut s, 6.0);
        let max = s.field.iter().flatten().fold(0.0_f32, |a, &v| a.max(v));
        assert!(max >= DRAW_FLOOR, "resting field must stay visible, got {max}");
    }

    #[test]
    fn traceless_backend_simulates_after_grace_but_not_before() {
        // A cloud/traceless local turn: the overlay goes busy but no
        // `ReplyBegin`/`Frame` ever arrives. Before the grace window the
        // bar must not simulate; after it, a simulated MoE sweep drives
        // the panel so it isn't dead while the remote model works.
        let mut s = CortexState::default();
        s.on_state(OverlayState::AssistantThinking);
        // Within the grace window: no simulated pulses yet.
        tick_for(&mut s, SIM_GRACE - 0.2);
        assert!(!s.simulating(), "must not simulate inside the grace window");
        // Past the grace window: simulation kicks in and lights the bar.
        tick_for(&mut s, 0.5);
        assert!(s.simulating(), "traceless busy turn simulates after grace");
        assert!(!s.pulses.is_empty(), "simulated beats keep the bar alive");
        let max = s.field.iter().flatten().fold(0.0_f32, |a, &v| a.max(v));
        assert!(max >= DRAW_FLOOR, "simulated activity must be visible, got {max}");
        // A grounded turn (real keyframes) must suppress the simulation.
        let mut g = CortexState::default();
        g.on_state(OverlayState::AssistantThinking);
        begin(&mut g, 8, CortexModelKind::Dense);
        g.apply(frame(0, vec![1.0; 8], 0.9, 1.0));
        tick_for(&mut g, SIM_GRACE + 1.0);
        assert!(!g.simulating(), "grounded reply never simulates");
    }

    #[test]
    fn moe_lane_selection_deterministic_with_collision_bump() {
        let mut s = CortexState::default();
        begin(&mut s, 4, CortexModelKind::Moe);
        // ids 7 and 13 collide on lane 1; the second bumps to a free
        // lane. Weights co-active (within 60% of top).
        let ex = (vec![7, 13, 2], vec![0.5, 0.35, 0.15]);
        let lanes = s.lanes_for(&ex);
        assert_eq!(lanes.len(), 2, "third expert gated by co-activity (0.15 < 0.6·0.5)");
        assert_eq!(lanes[0].0, 1);
        assert_ne!(lanes[1].0, 1, "collision bumped to a free lane");
        assert_eq!(lanes, s.lanes_for(&ex), "deterministic");
    }

    #[test]
    fn moe_lane_budget_adapts_to_sparsity() {
        let mut s = CortexState::default();
        s.apply(CortexCmd::ReplyBegin {
            n_layer: 4,
            kind: CortexModelKind::Moe,
            n_experts_total: Some(128),
            n_experts_active: Some(8), // 6.25% < 8% → 1 lane
        });
        let ex = (vec![1, 2, 3], vec![0.4, 0.35, 0.25]);
        assert_eq!(s.lanes_for(&ex).len(), 1);
        s.apply(CortexCmd::ReplyBegin {
            n_layer: 4,
            kind: CortexModelKind::Moe,
            n_experts_total: Some(8),
            n_experts_active: Some(2), // 25% → up to 3 lanes
        });
        assert_eq!(s.lanes_for(&ex).len(), 3);
    }

    #[test]
    fn entropy_desaturates_above_threshold() {
        let sat = cell_color(0.8, true, 0.0);
        let desat = cell_color(0.8, true, 1.0);
        let spread = |c: u32| {
            let r = (c >> 16) & 0xFF;
            let g = (c >> 8) & 0xFF;
            let b = c & 0xFF;
            r.max(g).max(b) - r.min(g).min(b)
        };
        assert!(spread(desat) < spread(sat), "high entropy must desaturate");
        assert_eq!(cell_color(0.8, true, 0.55), sat, "no desaturation at the threshold");
    }

    #[test]
    fn prefill_flood_is_cool_and_covers_all_columns() {
        let mut s = CortexState::default();
        s.on_state(OverlayState::AssistantThinking);
        s.apply(CortexCmd::Prefill { n_tokens: 61 });
        tick_for(&mut s, PREFILL_DUR + 0.1);
        for c in 0..COLS {
            assert!(s.mood[c] < 0.5, "prefill paints cool mood at col {c}");
            let col_max = (0..ROWS).map(|r| s.field[r][c]).fold(0.0_f32, f32::max);
            assert!(col_max > 0.0, "prefill floods col {c}");
        }
    }

    #[test]
    fn sparse_trace_still_animates_continuously_across_the_reply() {
        // The exact failure from the field log: 27 real tokens but the
        // governor let only ONE keyframe through, and a long spoken
        // reply. The bar must still pulse continuously (a sweep on
        // every beat) for the whole utterance, grounded in the real
        // token count + audio duration — not fire once and freeze.
        let mut s = CortexState::default();
        s.on_state(OverlayState::AssistantThinking);
        begin(&mut s, 35, CortexModelKind::Dense);
        s.apply(frame(2, vec![4.0; 35], 0.9, 1.2)); // the single keyframe
        s.apply(CortexCmd::ReplyEnd {
            total_tokens: 27,
            gen_ms: 2045,
            ctx_used: 0,
            ctx_capacity: 0,
        });
        s.on_state(OverlayState::AssistantSynthesising);
        s.apply(CortexCmd::AudioTotal { secs: 9.37 });
        s.on_state(OverlayState::AssistantSpeaking);

        // Sample the pulse activity across the whole reply: every
        // ~BEAT window must contain at least one live sweep.
        let mut silent_windows = 0;
        for _ in 0..((9.37 / BEAT) as usize) {
            tick_for(&mut s, BEAT);
            if s.pulses.is_empty() {
                silent_windows += 1;
            }
        }
        assert_eq!(silent_windows, 0, "every beat window must carry a sweep");
        // The one real keyframe is revealed exactly once.
        assert_eq!(s.frames_fired(), 1, "the single real keyframe fires once");
    }

    #[test]
    fn filled_held_interpolates_sparse_layers() {
        // Seed the held state via a real keyframe that observes only
        // layers 1 and 5 (0.0 marks "not observed this frame").
        let mut s = CortexState::default();
        s.on_state(OverlayState::AssistantThinking);
        begin(&mut s, 8, CortexModelKind::Dense);
        let mut norms = vec![0.0_f32; 8];
        norms[1] = 2.0;
        norms[5] = 6.0;
        s.apply(frame(0, norms, 0.9, 1.0));
        tick_for(&mut s, 0.05); // fires the keyframe → merges into held
        let f = s.filled_held();
        assert_eq!(f.len(), 8);
        assert!((f[0] - 2.0).abs() < 1e-5, "flat-extrapolate before first observed");
        assert!((f[1] - 2.0).abs() < 1e-5);
        assert!((f[3] - 4.0).abs() < 1e-5, "midpoint interpolates between 2 and 6");
        assert!((f[5] - 6.0).abs() < 1e-5);
        assert!((f[7] - 6.0).abs() < 1e-5, "flat-extrapolate after last observed");
        // Monotonic across the interpolated gap — no fabricated bumps.
        for w in f.windows(2) {
            assert!(w[1] >= w[0] - 1e-6);
        }
    }

    #[test]
    fn speaking_cursor_is_monotonic_when_audio_grows_midplayback() {
        // The field scenario: TTS synthesises sentence-by-sentence while
        // generation is still running, so audio_total climbs
        // 4.8 → 9.9 → 17.5 → 24.5s and total_tokens isn't known until
        // late. The playback cursor must only ever advance — never snap
        // backward — so the morph reads as steady progress, not a lurch.
        let mut s = CortexState::default();
        s.on_state(OverlayState::AssistantThinking);
        begin(&mut s, 35, CortexModelKind::Dense);
        for t in [1_u64, 6, 14, 16, 18, 20] {
            s.apply(frame(t, vec![2.0 + t as f32 * 0.1; 35], 0.9, 1.2));
        }
        s.apply(CortexCmd::AudioTotal { secs: 4.80 });
        s.on_state(OverlayState::AssistantSpeaking);
        let mut prev = s.play_pos;
        // Late anchors + audio growth arrive during playback:
        // (elapsed_at, new_anchor_token, new_audio_secs).
        let anchors: &[(f32, u64)] = &[(1.2, 47), (3.6, 83)];
        let audio: &[(f32, f32)] = &[(1.7, 9.94), (3.9, 17.54), (4.2, 24.54)];
        let mut ai = 0;
        let mut si = 0;
        let mut elapsed = 0.0_f32;
        for _ in 0..70 {
            tick_for(&mut s, BEAT);
            elapsed += BEAT;
            while ai < anchors.len() && elapsed >= anchors[ai].0 {
                s.apply(frame(anchors[ai].1, vec![3.2; 35], 0.85, 1.5));
                ai += 1;
            }
            while si < audio.len() && elapsed >= audio[si].0 {
                s.apply(CortexCmd::AudioTotal { secs: audio[si].1 });
                si += 1;
            }
            assert!(s.play_pos >= prev - 1e-4, "cursor went backward: {prev} -> {}", s.play_pos);
            prev = s.play_pos;
        }
        // It also eventually reaches the real reply length (the last
        // observed token + 1 = 84 here; no ReplyEnd was sent).
        assert!(s.play_pos >= 83.0, "cursor should span the whole reply, got {}", s.play_pos);
    }

    #[test]
    fn morph_norms_interpolates_in_time_between_anchors() {
        // Two real captures of the SAME layer at tokens 0 and 10 with
        // clearly different norms. The Speaking morph must return a
        // value that slides smoothly between them as playback advances
        // — never snapping — and clamps flat outside the observed span.
        let mut s = CortexState::default();
        s.on_state(OverlayState::AssistantThinking);
        begin(&mut s, 8, CortexModelKind::Dense);
        s.apply(frame(0, vec![2.0; 8], 0.9, 1.0));
        s.apply(frame(10, vec![8.0; 8], 0.9, 1.0));
        // The trace retains both anchors regardless of live draining.
        let at = |pos: f32| s.morph_norms_at(pos)[4];
        assert!((at(0.0) - 2.0).abs() < 1e-4, "flat at/before first anchor");
        assert!((at(-3.0) - 2.0).abs() < 1e-4, "flat before first anchor");
        assert!((at(5.0) - 5.0).abs() < 1e-4, "midpoint interpolates 2→8");
        assert!((at(2.5) - 3.5).abs() < 1e-4, "quarter-way interpolation");
        assert!((at(10.0) - 8.0).abs() < 1e-4, "reaches the second anchor");
        assert!((at(99.0) - 8.0).abs() < 1e-4, "flat after last anchor");
        // Monotonic sweep across the span — no fabricated wobble.
        let mut prev = at(0.0);
        for i in 1..=10 {
            let cur = at(i as f32);
            assert!(cur >= prev - 1e-6, "morph must advance monotonically");
            prev = cur;
        }
    }

    #[test]
    fn speaking_equalizer_shape_evolves_across_the_reply() {
        // With multiple anchors the visible bar must actually CHANGE
        // shape through the utterance (the "dull, frozen" complaint),
        // not repaint one snapshot. Capture the field column profile
        // early vs late in playback and require it to differ.
        let mut s = CortexState::default();
        s.on_state(OverlayState::AssistantThinking);
        begin(&mut s, 8, CortexModelKind::Dense);
        // Rising ramp so early vs late frames have distinct shapes.
        for t in 0..6 {
            s.apply(frame(t * 6, vec![1.0 + 2.0 * t as f32; 8], 0.9, 1.0));
        }
        s.apply(CortexCmd::ReplyEnd {
            total_tokens: 30,
            gen_ms: 3000,
            ctx_used: 0,
            ctx_capacity: 0,
        });
        s.on_state(OverlayState::AssistantSynthesising);
        s.apply(CortexCmd::AudioTotal { secs: 6.0 });
        s.on_state(OverlayState::AssistantSpeaking);
        let snapshot = |s: &CortexState| -> Vec<f32> {
            (0..COLS).map(|c| (0..ROWS).map(|r| s.field[r][c]).fold(0.0_f32, f32::max)).collect()
        };
        tick_for(&mut s, 0.5);
        let early = snapshot(&s);
        tick_for(&mut s, 4.5);
        let late = snapshot(&s);
        let diff: f32 = early.iter().zip(&late).map(|(a, b)| (a - b).abs()).sum();
        assert!(diff > 0.1, "equalizer shape must evolve across the reply, diff={diff}");
    }

    #[test]
    fn draw_smoke_renders_within_bounds() {
        let (w, h) = (820_u32, 100_u32);
        let mut buf = vec![0_u32; (w * h) as usize];
        let mut s = CortexState::default();
        s.on_state(OverlayState::AssistantThinking);
        begin(&mut s, 32, CortexModelKind::Dense);
        s.apply(CortexCmd::Prefill { n_tokens: 100 });
        s.apply(frame(0, vec![4.0; 32], 0.9, 1.0));
        tick_for(&mut s, 0.3);
        draw_cortex(&mut buf, w, h, &s, 5.0, 815.0, 2.0, 98.0, 0xFFF5_9E0B, 1.25, 0.3);
        assert!(buf.iter().any(|&p| p & 0x00FF_FFFF != 0), "something must be drawn");
        // Nothing outside the strip: left margin column stays untouched.
        for y in 0..h {
            assert_eq!(buf[(y * w) as usize], 0, "pixel left of x0 written at row {y}");
        }
    }

    #[test]
    fn clear_resets_but_reply_begin_keeps_inflight_prefill() {
        let mut s = CortexState::default();
        s.on_state(OverlayState::AssistantThinking);
        s.apply(CortexCmd::Prefill { n_tokens: 32 });
        assert_eq!(s.pulses.len(), 1);
        // ReplyBegin arrives after the prefill pulse (real event order)
        // and must not cut the flood mid-pass.
        begin(&mut s, 16, CortexModelKind::Dense);
        assert_eq!(s.pulses.len(), 1, "prefill pulse survives ReplyBegin");
        s.clear();
        assert_eq!(s.pulses.len(), 0);
        assert_eq!(s.frames_fired(), 0);
    }
}
