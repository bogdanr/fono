// SPDX-License-Identifier: GPL-3.0-only
//! "Activation Heatmap" — the LLM brain visualisation
//! (`WaveformStyle::Cortex`).
//!
//! A chunky heat grid fills the whole panel: columns are the model's
//! real transformer layers (left = input, right = output), rows are a
//! handful of sampled units per layer. Each cell's colour is a
//! grounded 0..1 signal on a per-phase heat ramp; the hottest cells
//! get a faint additive bloom. The scene draws *data*, not structure,
//! and follows the voice pipeline:
//!
//! - **Listening** — a warm red-orange grid reacting to the live mic
//!   FFT: louder bins push their columns hotter (sound flowing in).
//! - **Thinking / prefill** — a sequential per-column ignition wipe
//!   sweeps left → right, driven by the real per-batch prefill events
//!   (`CortexCmd::Prefill`); rows within a column latch at slightly
//!   staggered moments so the wipe reads as real per-row texture, not
//!   a flat bar. The instant the first real token keyframe lands, a
//!   single one-shot deterministic wipe (never a per-frame blend)
//!   snaps columns over to the decode flare grammar and the prefill
//!   field is retired for the rest of the reply.
//! - **Synthesising** (TTS prep, after decode finishes but before
//!   audio starts playing) — keeps replaying the real decode flares
//!   captured during generation, looped and paced by the same real
//!   arrival-time clock, so the grid never goes dark or drifts onto
//!   fabricated motion while waiting on the TTS round-trip.
//! - **Speaking / decode (dense)** — the same real per-token flares
//!   continue, now paced by the audio replay clock so each token's
//!   flare crests in step with the spoken words, lighting only the
//!   row that token's real per-layer sample data actually visits at
//!   each layer (never a flat column fill); the real, already-
//!   synthesised reply audio (Goertzel bands) subtly modulates row
//!   brightness on top, capped well under a third of a cell's
//!   intensity so the flare stays the primary signal.
//! - **Speaking / decode (MoE)** — only the routed experts light up,
//!   warm = amber (RAM-resident) vs cold = blue (offloaded), with a
//!   focal hotspot tracking the active layer.
//!
//! Thinking/Synthesising/Speaking all derive their heat-ramp colour
//! from the same real per-state accent the rest of the overlay
//! already uses (status label, System/360's lit-lamp colour) instead
//! of inventing separate hues, so the whole panel reads as one
//! coherent palette family.
//!
//! When no capture data is available (external backends, or capture
//! disabled) the speaking phase falls back honestly to a dense grid
//! whose travelling column is paced to the word cadence — activity
//! and rhythm without fabricating model internals.
//!
//! ## Rendering approach
//!
//! All ingredients are computed at runtime — no asset files, no new
//! crates:
//!
//! - **Additive glow** ([`add_glow`] / [`GlowAccum`]): a two-lobe
//!   radial falloff (bright core + wide faint halo) composited with
//!   saturating channel addition at quarter resolution. The halo lobe
//!   plays the role of the classic quarter-res bloom pass at a
//!   fraction of its cost — one pass, no intermediate buffer, no blur
//!   kernel — while producing the same luminous read. Bloom is kept
//!   deliberately faint here so the chunky cells never wash out.
//!
//! ## State
//!
//! [`CortexState`] holds the slow-decaying per-layer heat trace, the
//! smoothed per-layer activation, and the keyframe replay clock. It
//! is *advanced* from the renderer's FFT push path and the animation
//! pump (the same clock the other styles redraw on) and *read* by the
//! draw entry point — the honest "real vs synthetic signal" seam
//! lives entirely in the state, so [`draw_cortex`] never knows which
//! source backs a given cell.

// Same lint posture as `renderer.rs` / `r3d.rs`: readable
// visualisation math beats `mul_add` chains — glow falloffs and
// lattice placement genuinely want plain `a + b * c` shapes.
#![allow(clippy::suboptimal_flops, clippy::many_single_char_names, clippy::too_many_arguments)]

/// Default layer count for the spine before real model metadata
/// arrives (plan Task 2.4 plumbs `n_layer()` from the loaded GGUF).
/// 32 reads well on the 640 px strip and is in the ballpark of the
/// small dense models Fono ships.
const DEFAULT_LAYERS: usize = 32;
/// Clamp for pathological metadata so the spine never degenerates.
const MIN_LAYERS: usize = 8;
const MAX_LAYERS: usize = 96;
/// Per-tick exponential decay of the heat trace (ticks arrive at
/// ~20 fps ⇒ 0.985²⁰ ≈ 0.74/s — the "shape of the thought" fades
/// over a few seconds).
const HEAT_DECAY: f32 = 0.985;
/// Fraction of the instantaneous activation folded into the heat
/// trace each tick.
const HEAT_GAIN: f32 = 0.12;
/// EMA coefficient for the per-layer activation smoothing (input →
/// displayed glow). Low = snappy.
const ACT_EMA: f32 = 0.45;
/// Fraction of the lattice behind a pulse head that still glows (the
/// fading tail of a token's path).
const WAKE_FRAC: f32 = 0.22;
/// Estimated seconds per spoken word for the cadence fallback
/// (external backends with no capture data).
const SYNTH_SECS_PER_WORD: f32 = 0.38;

/// Seconds a token pulse spends traveling the lattice before
/// cresting at the right edge (where its word is "spoken").
const BEAD_TRAVEL_SECS: f32 = 0.9;
/// Lifetime of the burst spark at the lattice's right edge.
const SPARK_LIFE_SECS: f32 = 0.45;
/// Seconds a prefill sweep pulse takes to cross the lattice.
const SWEEP_CROSS_SECS: f32 = 0.45;
/// Half-width of the sweep bump, as a fraction of the lattice.
const SWEEP_WIDTH: f32 = 0.18;
/// Prefill batch width (tokens) at which a sweep reaches full
/// amplitude (log scale — a 16-token suffix still reads).
const SWEEP_FULL_TOKENS: f32 = 512.0;
/// Fallback speech-duration estimate when no `AudioTotal` arrives
/// (streaming TTS): ~4 chars/token at ~15 chars/s of speech.
const SECS_PER_TOKEN_EST: f32 = 0.27;
/// Entropy normalisation ceiling for the uncertainty ribbon (bits).
const ENTROPY_NORM_BITS: f32 = 4.0;
/// tok/s that fills the HUD throughput arc completely.
const HUD_TOKPS_FULL: f32 = 40.0;
/// Largest tick step fed to the replay clock — guards against a
/// stalled compositor delivering one huge dt and skipping the show.
const MAX_TICK_DT: f32 = 0.25;
/// Downsampling factor of the glow accumulation buffer (the classic
/// quarter-res fake-bloom pass from plan Task 2.2) in *logical*
/// pixels: glows are splatted at 1/4 logical resolution (1/16 the
/// pixels) and composited back up with one bilinear pass, which also
/// gives the halo its final softness. The physical factor scales with
/// HiDPI (`GLOW_DOWN × scale`) so glow cost tracks logical panel
/// size, not physical pixel count.
const GLOW_DOWN: u32 = 4;

/// Heatmap grid rows (sampled units per layer column). Six reads well
/// on the wide 810×96 strip while keeping cells legibly chunky. Also
/// the row count [`path_rows`] routes real token paths through, so
/// the decode flare's per-row texture always matches what is drawn.
const GRID_ROWS: usize = 6;
/// MoE expert residency tints (packed `0x00RR_GGBB`): warm = amber
/// (RAM-resident), cold = blue (offloaded to disk). Reserved for the
/// deferred, opt-in synthetic warm/cold tint (plan G3); the shipped
/// constellation uses the weight-honest palette so nothing implies
/// real residency until G5 lands actual `mincore()` scanning.
#[allow(dead_code)]
const EXPERT_WARM: u32 = 0x00FF_B347;
#[allow(dead_code)]
const EXPERT_COLD: u32 = 0x0056_8CFF;

/// True-OFF activation threshold (plan Task A3). Cells whose value
/// falls below this render the real (dark) panel background so
/// non-activating params/experts read as genuinely off; only a hair
/// of hint is drawn in the narrow band just under the threshold.
const ACT_THRESHOLD: f32 = 0.12;
/// Duration of the one-shot prefill→decode "snap" wipe (plan Task 3):
/// a single deterministic left-to-right cut from the prefill flood to
/// the token-flare decode grammar, fired exactly once when the first
/// real token keyframe lands. Never re-evaluated as a per-frame blend
/// — once `decode_clock` (reset to 0 at that moment) passes this many
/// seconds the prefill field is fully retired.
const SNAP_SECS: f32 = 0.22;
/// Calm resting brightness a prefill column holds once the ignition
/// wipe has passed it — not zero (dead), not full-hot (still
/// "reading"), so the phase shows a clear beginning-middle-end shape.
const PREFILL_REST_LEVEL: f32 = 0.30;
/// Per-row phase offset (as a fraction of [`SWEEP_WIDTH`]) applied to
/// the prefill ignition front so consecutive rows latch at slightly
/// different times — the real per-row stagger/texture plan Task 2
/// asks for, instead of a flat bar advancing in lockstep.
const ROW_STAGGER: f32 = 0.55;
/// Hard cap on the additive per-row voice glow during speaking, as a
/// fraction of full cell intensity — sound decorates, it never
/// dominates the decode column (synergy subtlety constraint).
const AUDIO_ROW_CAP: f32 = 0.26;
/// Hard cap on the gentle global amplitude pulse added during
/// speaking.
const AUDIO_PULSE_CAP: f32 = 0.10;
/// Listening spectrum: number of frequency bands sampled from the mic
/// FFT. Resampled to the grid column count at draw time, so this is
/// independent of how many columns actually fit.
const SPEC_BANDS: usize = 56;
/// Noise-floor gate subtracted from each normalised mic bin so a quiet
/// room settles the energy dots to the bottom row instead of showing a
/// spurious mid-band cluster. Sits just above the dB-normalised idle
/// floor the session pushes.
const SPEC_NOISE_FLOOR: f32 = 0.16;
/// Snappy EMA toward each band's target energy (rise fast, fall via
/// the peak-hold cap so the dots feel responsive but not jittery).
const SPEC_EMA: f32 = 0.5;
/// Per-second fall of the peak-hold caps (classic EQ "cap" that hangs
/// at the last peak then drifts down).
const SPEC_PEAK_FALL: f32 = 0.9;
/// Number of stars in the MoE constellation field. Most stay dark
/// (true-OFF); expert ids map into this fixed field via a hash so the
/// scene scales gracefully from 8 to 256+ experts without becoming
/// sub-pixel noise.
const N_STARS: usize = 84;
/// Per-tick decay of the constellation heat-trace (which regions of
/// expert space carried the reply).
const CONSTELLATION_HEAT_DECAY: f32 = 0.97;
/// Constellation palette (`0x00RR_GGBB`): ignited star core, the dim
/// implied-population substrate, and the thin routing threads.
const CONSTELLATION_HOT: u32 = 0x00CF_E8FF;
const CONSTELLATION_DIM: u32 = 0x0020_2838;
const CONSTELLATION_THREAD: u32 = 0x0066_A8E0;

/// Cool, dim colour for the listening weight-field (the shimmering
/// ambient grid behind the peak dots). Kept low-alpha so the hot
/// energy dots pop against it (plan C, redesigned).
const WEIGHT_FIELD_COLOR: u32 = 0x0024_3446;

/// Brighter cool-blue used for sparse "firing" cells in the listening
/// weight-field — brief, desynchronised flashes that make the resting
/// brain read as gently active (idle thought) rather than a dead grid.
/// Distinct from the warm energy dots so the two never read as the same
/// signal. Kept sparse + capped so it never dominates (plan C).
const WEIGHT_FIELD_SPARK: u32 = 0x0058_9AD8;

/// Pipeline phase the scene is in, derived from [`crate::OverlayState`]
/// by [`CortexState::on_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Phase {
    /// Overlay hidden — decay to black.
    #[default]
    Idle,
    /// Mic is hot (dictation/assistant recording): live FFT drives
    /// the grid.
    Listening,
    /// Prompt submitted / generation burst / TTS synth in flight
    /// (also the polish cleanup burst): captured keyframes are
    /// applied live as they arrive; synthetic turbulence fills gaps.
    Thinking,
    /// Reply audio is playing: timed replay of the keyframes, paced
    /// so each bead crests as its word is spoken.
    Speaking,
}

/// One ingested keyframe, post-merge (strided gaps filled from the
/// previous frame) with raw (un-normalised) layer norms.
#[derive(Debug, Clone, Default)]
struct ReplayFrame {
    token_index: u64,
    /// Raw merged per-layer L2 norms.
    norms: Vec<f32>,
    /// Token-distribution entropy in bits (0 when unknown).
    entropy: f32,
    /// Real arrival timestamp on [`CortexState::decode_clock`] (0 for
    /// the very first token of the reply, since that arrival is what
    /// latches and resets the clock). This is the crest schedule for
    /// the thinking-phase decode flare (plan Task 4) — frames are
    /// paced by exactly when they really arrived, never by render-tick
    /// counting.
    at: f32,
}

/// A token pulse in flight along the lattice (answering phase).
#[derive(Debug, Clone, Copy)]
pub struct Bead {
    /// Head position along the lattice, 0 (left/layer 1) ..= 1 (right).
    pub x: f32,
    /// Overall glow energy, 0..1 (mean normalised activation of the
    /// pulse's keyframe, or a hashed level in cadence mode).
    pub energy: f32,
    /// Token index — the stable seed of the path this pulse takes.
    token: u64,
    /// Index into the frame list when real capture data backs this
    /// pulse (`None` in cadence mode).
    frame: Option<usize>,
}

/// Right-edge burst spark ("the word left the model").
#[derive(Debug, Clone, Copy)]
struct Spark {
    age: f32,
    energy: f32,
}

/// A prefill sweep pulse crossing the spine left→right (thinking
/// phase): one per prompt-prefill batch, amplitude from batch width.
#[derive(Debug, Clone, Copy)]
struct Sweep {
    /// Head position, 0..1 across the spine (advances with time).
    pos: f32,
    /// Peak amplitude, 0..1.
    amp: f32,
}

/// Per-frame animated state for the cortex scene. Owned by
/// `RendererState`; advanced by [`CortexState::tick`] on the FFT
/// push path (20 fps — the same clock the other styles redraw on),
/// fed replay data by [`CortexState::apply`], phase-switched by
/// [`CortexState::on_state`], and read by [`draw_cortex`].
#[derive(Debug, Default)]
pub struct CortexState {
    /// Layer count reported by the loaded model (0 = unknown, use
    /// [`DEFAULT_LAYERS`]).
    model_layers: usize,
    /// Smoothed per-layer activation, 0..1.
    activation: Vec<f32>,
    /// Slow-decaying per-layer heat trace, 0..1.
    heat: Vec<f32>,
    /// Current pipeline phase (from the overlay state).
    phase: Phase,
    /// Ingested keyframes for the current reply, in token order.
    frames: Vec<ReplayFrame>,
    /// Running per-layer max of raw norms (normalisation scale).
    layer_peak: Vec<f32>,
    /// Last merged raw norms (fills the stride gaps of new frames).
    merged: Vec<f32>,
    /// Total decoded tokens (known after `ReplyEnd`).
    total_tokens: Option<u64>,
    /// Decode throughput from `ReplyEnd` (HUD).
    tok_per_sec: f32,
    /// KV-cache fill 0..1 from `ReplyEnd` (HUD).
    ctx_fill: f32,
    /// Cumulative enqueued reply-audio seconds (`AudioTotal`).
    audio_secs: f32,
    /// Whole reply audio finished playing.
    playback_done: bool,
    /// Playback clock, seconds since the speaking phase began.
    clock: f32,
    /// Replay-timeline length the clock runs against (cached by the
    /// speaking tick; feeds [`Self::playback_frac`]).
    timeline_secs: f32,
    /// Thinking-phase decode clock: real wall-clock seconds since the
    /// first token keyframe of this reply landed (reset to 0 at that
    /// exact moment). This is the "already used elsewhere for TTS
    /// sync" replay-clock mechanism (plan Task 4) applied to
    /// Thinking's decode tail: every captured frame stamps its real
    /// arrival time on this clock ([`ReplayFrame::at`]), and the
    /// decode flare is spawned by comparing that stamp against this
    /// clock -- never by counting render ticks.
    decode_clock: f32,
    /// Total real decode duration once known (snapshotted from
    /// [`Self::decode_clock`] at `ReplyEnd`) -- the period the captured
    /// trace loops over while Thinking lingers in the TTS round-trip
    /// (`AssistantSynthesising`) after generation has finished.
    gen_total_secs: Option<f32>,
    /// Beads in flight this tick (speaking phase; rebuilt per tick).
    beads: Vec<Bead>,
    /// Right-edge sparks in flight.
    sparks: Vec<Spark>,
    /// Prefill sweep pulses crossing the lattice (thinking phase).
    sweeps: Vec<Sweep>,
    /// Per-column row last touched by a token path (`u8::MAX` =
    /// none) — anchors the cooling embers and the crest sparks.
    ember_row: Vec<u8>,
    /// Wall-clock anchor for the tick delta.
    last_tick: Option<std::time::Instant>,
    /// True once a captured keyframe reported MoE routing — switches
    /// the speaking scene from the dense travelling-column look to the
    /// sparse expert-cell look. Stays false for dense models and for
    /// the degraded (no-capture) path, so both fall back honestly to
    /// the dense heatmap driven by cadence.
    moe: bool,
    /// Latest per-layer routed expert ids (MoE only), indexed by
    /// layer. Empty rows mean "no routing observed for this layer".
    routing: Vec<Vec<i32>>,
    /// Latest per-layer routing weights (MoE only), parallel to
    /// `routing`. Drives constellation ignition brightness (plan G1).
    weights: Vec<Vec<f32>>,
    /// Highest expert id seen this reply (for the "k / N" HUD scale).
    max_expert_id: i32,
    /// Largest observed top-k width (the "k" in the HUD readout).
    top_k: usize,
    /// Slow-decaying per-star heat trace for the MoE constellation —
    /// which regions of expert space carried the reply (plan G2).
    star_heat: Vec<f32>,
    /// Smoothed per-band mic energy for the listening spectrum
    /// (X = frequency, linearly sampled; `SPEC_BANDS` long, resampled to
    /// the grid column count at draw time). Reuses the real mic FFT bins
    /// pushed by the session — no new DSP.
    spec_bands: Vec<f32>,
    /// Slow-falling peak-hold caps, parallel to `spec_bands` (classic
    /// EQ cap that hangs at the last peak then drifts down).
    spec_peak: Vec<f32>,
    /// Real reply-audio spectrum timeline: `(at_secs, bands, amp)`
    /// windows computed from the genuine synthesised TTS PCM and
    /// sampled against the speaking clock (plan E1). Empty on backends
    /// that never push audio bands.
    audio_frames: Vec<(f32, Vec<f32>, f32)>,
    /// Thinking-phase decode latch: set once (and only once) the first
    /// token keyframe lands, which also resets [`Self::decode_clock`]
    /// to 0 and starts the one-shot prefill→decode snap (plan Task 3).
    decode_latched: bool,
    /// Progress 0..1 of the one-shot prefill→decode snap wipe, driven
    /// by `decode_clock / SNAP_SECS`. Reaches 1.0 once and stays there
    /// — never re-evaluated as an ongoing blend.
    decode_snap: f32,
    /// Per-layer prefill "read" latch, 0..1 (plan Task 2): monotonic
    /// resting brightness a column snaps to once the ignition wipe has
    /// passed it, persisting until the next `ReplyBegin`/`clear`.
    prefill_lit: Vec<f32>,
}

impl CortexState {
    /// Effective spine length.
    #[must_use]
    pub fn layer_count(&self) -> usize {
        if self.model_layers == 0 {
            DEFAULT_LAYERS
        } else {
            self.model_layers.clamp(MIN_LAYERS, MAX_LAYERS)
        }
    }

    /// Set the real transformer layer count read from the loaded
    /// model's metadata. `0` reverts to the default spine.
    pub fn set_model_layers(&mut self, n: usize) {
        if self.model_layers != n {
            self.model_layers = n;
            self.activation.clear();
            self.heat.clear();
        }
    }

    /// Drop animation + replay state on style swap so stale data
    /// doesn't flash when the user switches back to the style.
    pub fn clear(&mut self) {
        self.activation.clear();
        self.heat.clear();
        self.sweeps.clear();
        self.clear_replay();
    }

    fn clear_replay(&mut self) {
        self.frames.clear();
        self.layer_peak.clear();
        self.merged.clear();
        self.total_tokens = None;
        self.tok_per_sec = 0.0;
        self.ctx_fill = 0.0;
        self.audio_secs = 0.0;
        self.playback_done = false;
        self.clock = 0.0;
        self.timeline_secs = 0.0;
        self.decode_clock = 0.0;
        self.gen_total_secs = None;
        self.beads.clear();
        self.sparks.clear();
        self.ember_row.clear();
        self.moe = false;
        self.routing.clear();
        self.weights.clear();
        self.max_expert_id = 0;
        self.top_k = 0;
        self.star_heat.clear();
        self.audio_frames.clear();
        self.decode_latched = false;
        self.decode_snap = 0.0;
        self.prefill_lit.clear();
        // `sweeps` deliberately survives: prefill pulses are published
        // *before* the generation's `ReplyBegin`, and cutting a sweep
        // mid-flight on that reset would eat the suffix-prefill pulse.
    }

    /// Track the overlay state so the phase machine follows the
    /// pipeline: listening (mic hot) → thinking (generation burst) →
    /// speaking (timed replay). Called on every `SetState`.
    pub fn on_state(&mut self, state: crate::OverlayState) {
        use crate::OverlayState as S;
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
            // Playback starts now — the replay clock is relative to
            // this moment (first audio chunk enqueued).
            self.clock = 0.0;
        }
        self.phase = phase;
    }

    /// Ingest one replay command from the orchestrator (see
    /// [`crate::CortexCmd`]).
    pub fn apply(&mut self, cmd: crate::CortexCmd) {
        use crate::CortexCmd as C;
        match cmd {
            C::ReplyBegin { n_layer } => {
                self.clear_replay();
                if n_layer > 0 {
                    self.set_model_layers(n_layer as usize);
                }
            }
            C::Frame(f) => self.ingest_frame(&f),
            C::Prefill { n_tokens } => {
                // Amplitude grows with the log of the batch width so a
                // short cached-suffix prefill still reads while a full
                // cold prompt hits hard.
                let amp = ((n_tokens.max(1) as f32).ln() / SWEEP_FULL_TOKENS.ln()).clamp(0.25, 1.0);
                self.sweeps.push(Sweep { pos: 0.0, amp });
            }
            C::ReplyEnd { total_tokens, gen_ms, ctx_used, ctx_capacity } => {
                self.total_tokens = Some(total_tokens.max(1));
                if gen_ms > 0 {
                    self.tok_per_sec = total_tokens as f32 * 1000.0 / gen_ms as f32;
                }
                if ctx_capacity > 0 {
                    self.ctx_fill = (ctx_used as f32 / ctx_capacity as f32).clamp(0.0, 1.0);
                }
                // Snapshot the real decode duration so the Thinking tail
                // (waiting on the TTS round-trip) can loop-replay the
                // captured trace over its real length instead of
                // fabricating a filler period (plan Task 4).
                if self.decode_latched {
                    self.gen_total_secs = Some(self.decode_clock.max(0.1));
                }
            }
            C::AudioTotal { secs } => {
                if secs.is_finite() {
                    self.audio_secs = self.audio_secs.max(secs.max(0.0));
                }
            }
            C::AudioBands { at_secs, bands, amp } => {
                // Real spectrum window from the genuine TTS PCM. Stored
                // on a timeline so the speaking scene samples it against
                // the playback clock (plan E1). Cap the history so a
                // very long reply can't grow this unbounded.
                if at_secs.is_finite() && amp.is_finite() {
                    self.audio_frames.push((at_secs.max(0.0), bands, amp.clamp(0.0, 1.0)));
                    if self.audio_frames.len() > 4096 {
                        self.audio_frames.remove(0);
                    }
                }
            }
            C::PlaybackDone => self.playback_done = true,
        }
    }

    /// Merge a captured keyframe: strided `0.0` gaps are filled from
    /// the previous merged frame so every stored frame has full layer
    /// coverage, and the per-layer peak (normalisation scale) is
    /// updated.
    fn ingest_frame(&mut self, f: &crate::CortexFrame) {
        let n = f.layer_norms.len().max(self.merged.len());
        self.merged.resize(n, 0.0);
        self.layer_peak.resize(n, 0.0);
        for (i, slot) in self.merged.iter_mut().enumerate() {
            let v = f.layer_norms.get(i).copied().unwrap_or(0.0);
            if v > 0.0 {
                *slot = v;
            }
        }
        for (peak, &v) in self.layer_peak.iter_mut().zip(self.merged.iter()) {
            *peak = peak.max(v);
        }
        // The first real token keyframe of a reply latches the decode
        // phase and restarts the decode clock at 0 (plan Task 3/4): every
        // frame's arrival timestamp below is relative to this moment, so
        // the crest schedule the decode flare reads is exactly when the
        // tokens really arrived, not a render-tick count.
        if !self.decode_latched {
            self.decode_latched = true;
            self.decode_clock = 0.0;
        }
        self.frames.push(ReplayFrame {
            token_index: f.token_index,
            norms: self.merged.clone(),
            entropy: f.entropy_bits.unwrap_or(0.0).max(0.0),
            at: self.decode_clock,
        });
        // MoE routing: remember the latest per-layer expert choices so
        // the speaking scene can light only the routed experts. A dense
        // model never carries `experts`, so `moe` stays false and the
        // scene keeps the dense travelling-column look.
        if !f.experts.is_empty() {
            self.moe = true;
            for e in &f.experts {
                let l = e.layer as usize;
                if l >= self.routing.len() {
                    self.routing.resize(l + 1, Vec::new());
                    self.weights.resize(l + 1, Vec::new());
                }
                self.routing[l].clone_from(&e.ids);
                self.weights[l].clone_from(&e.weights);
                self.top_k = self.top_k.max(e.ids.len());
                for &id in &e.ids {
                    self.max_expert_id = self.max_expert_id.max(id);
                }
            }
        }
    }

    /// Normalised (0..1) activation of `layer` in `frame`.
    fn frame_act(&self, frame: usize, layer: usize) -> f32 {
        let Some(f) = self.frames.get(frame) else { return 0.0 };
        let norm = f.norms.get(layer).copied().unwrap_or(0.0);
        let peak = self.layer_peak.get(layer).copied().unwrap_or(0.0);
        if peak <= f32::EPSILON {
            0.0
        } else {
            (norm / peak).clamp(0.0, 1.0)
        }
    }

    /// Mean normalised activation of a frame (bead energy).
    fn frame_energy(&self, frame: usize) -> f32 {
        let n = self.layer_count();
        if n == 0 {
            return 0.0;
        }
        (0..n).map(|i| self.frame_act(frame, i)).sum::<f32>() / n as f32
    }

    /// Number of tokens the replay timeline spans: the reply's total
    /// when known, otherwise the highest keyframe index seen so far.
    fn token_span(&self) -> f32 {
        let known = self.total_tokens.unwrap_or(0);
        let seen = self.frames.last().map_or(0, |f| f.token_index + 1);
        known.max(seen).max(1) as f32
    }

    /// Remember, for every column a bead is currently crossing, which
    /// row its real token path visits there ([`path_rows`]) — the
    /// cooling embers that make the reply's "worn routes" visible
    /// between flares (real per-column, per-token structure, not a
    /// flat resting glow).
    fn update_embers(&mut self, n: usize) {
        if self.ember_row.len() != n {
            self.ember_row = vec![u8::MAX; n];
        }
        let beads = &self.beads;
        let ember = &mut self.ember_row;
        for b in beads {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let col = (b.x.clamp(0.0, 1.0) * (n - 1) as f32).round() as usize;
            if let Some(slot) = ember.get_mut(col) {
                #[allow(clippy::cast_possible_truncation)]
                {
                    *slot = path_rows(b.token, col, GRID_ROWS)[col] as u8;
                }
            }
        }
    }

    /// Advance the animation one tick from the latest FFT frame. The
    /// bins drive the grid directly while listening (live mic
    /// spectrum) and pad the thinking phase (the orchestrator's
    /// synthetic turbulence field); the answering phase ignores them
    /// and runs the timed keyframe replay instead.
    pub fn tick(&mut self, latest: &[f32]) {
        let now = std::time::Instant::now();
        let dt = self
            .last_tick
            .map_or(0.05, |t| now.duration_since(t).as_secs_f32())
            .clamp(0.0, MAX_TICK_DT);
        self.last_tick = Some(now);
        self.tick_dt(latest, dt);
    }

    /// Deterministic tick core (`dt` injectable for tests).
    fn tick_dt(&mut self, latest: &[f32], dt: f32) {
        let n = self.layer_count();
        if self.activation.len() != n {
            self.activation = vec![0.0; n];
            self.heat = vec![0.0; n];
        }
        // Sparks age on every tick so they finish fading even when
        // the phase moves on mid-burst.
        for s in &mut self.sparks {
            s.age += dt;
        }
        self.sparks.retain(|s| s.age < SPARK_LIFE_SECS);
        // Prefill sweeps advance on every tick for the same reason.
        for s in &mut self.sweeps {
            s.pos += dt / SWEEP_CROSS_SECS;
        }
        self.sweeps.retain(|s| s.pos < 1.0 + SWEEP_WIDTH);
        // Listening spectrum source (plan C, redesigned): X = frequency
        // (linearly sampled), Y = energy. Reuses the real mic FFT bins
        // already pushed by the session (`latest`) — no new DSP, exactly
        // like the System/360 style. We deliberately do NOT auto-gain per
        // band: that pumped quiet mid-band noise into a false bright
        // cluster and divided the always-loud low bins back down so voice
        // never lit the left of the panel. The bins are already dB-scaled
        // and normalised upstream, so we use them directly, gate the idle
        // noise floor, lift contrast, then track a snappy EMA plus
        // slow-falling peak-hold caps.
        if self.phase == Phase::Listening {
            if self.spec_bands.len() != SPEC_BANDS {
                self.spec_bands = vec![0.0; SPEC_BANDS];
                self.spec_peak = vec![0.0; SPEC_BANDS];
            }
            let peak_fall = SPEC_PEAK_FALL * dt;
            let denom = (SPEC_BANDS - 1).max(1) as f32;
            for b in 0..SPEC_BANDS {
                // Linear frequency sample position into the mic bins.
                let p = b as f32 / denom;
                let raw = sample_frac(latest, p);
                // Gate the idle noise floor so a quiet room rests at the
                // bottom, then rescale and lift contrast so speech reads.
                let cleaned = ((raw - SPEC_NOISE_FLOOR) / (1.0 - SPEC_NOISE_FLOOR)).max(0.0);
                let target = cleaned.powf(0.7);
                let s = &mut self.spec_bands[b];
                *s += (target - *s) * SPEC_EMA;
                // Peak-hold cap: jump to new peaks, else drift down.
                if *s >= self.spec_peak[b] {
                    self.spec_peak[b] = *s;
                } else {
                    self.spec_peak[b] = (self.spec_peak[b] - peak_fall).max(*s);
                }
            }
        }
        let targets: Vec<f32> = match self.phase {
            Phase::Idle => vec![0.0; n],
            Phase::Listening => (0..n).map(|i| resample(latest, i, n)).collect(),
            Phase::Thinking => self.thinking_targets(latest, n, dt),
            Phase::Speaking => self.replay_targets(n, dt),
        };
        for ((act, heat), target) in
            self.activation.iter_mut().zip(self.heat.iter_mut()).zip(targets)
        {
            *act += (target - *act) * ACT_EMA;
            *heat = (*heat * HEAT_DECAY + *act * HEAT_GAIN).clamp(0.0, 1.0);
        }
    }

    /// Thinking-phase targets. Two mutually exclusive regimes, chosen
    /// once and never blended (plan Task 2/3):
    ///
    /// - **Prefill** (before the first real token keyframe lands): a
    ///   sequential column-ignition wipe, paced by the real
    ///   `CortexCmd::Prefill` batch progress (`Self::sweeps`). Columns
    ///   the wipe front has already crossed latch to
    ///   [`PREFILL_REST_LEVEL`] in [`Self::prefill_lit`] and stay
    ///   there — a real beginning-middle-end shape, not a blob that
    ///   passes through and vanishes. With no real prefill events at
    ///   all (external backends, no capture) a synthetic keep-alive
    ///   wipe fills the gap honestly.
    /// - **Decode** (from the moment the first frame lands): the same
    ///   token-flare replay grammar Speaking uses, paced by
    ///   [`Self::decode_clock`] — a real wall-clock replay clock, not
    ///   render-tick counting (plan Task 4). While tokens are still
    ///   arriving live, each frame's flare fires exactly at its real
    ///   arrival timestamp. Once generation has finished
    ///   (`ReplyEnd` set [`Self::gen_total_secs`]) but Thinking
    ///   lingers (`AssistantSynthesising`, waiting on the TTS
    ///   round-trip), the captured trace loops over its own real
    ///   length instead of fading to a fabricated filler.
    ///
    /// The prefill→decode handoff itself is a single one-shot wipe
    /// (`Self::decode_snap`, plan Task 3): for the `SNAP_SECS` right
    /// after latch, columns behind the snap front already show decode
    /// while columns ahead of it still show the last prefill state;
    /// once the snap completes the prefill field is never evaluated
    /// again this reply.
    fn thinking_targets(&mut self, latest: &[f32], n: usize, dt: f32) -> Vec<f32> {
        if !self.decode_latched {
            // No real prefill events and nothing crossing? Keep the
            // brain visibly working: loop a moderate synthetic sweep so
            // external backends (no capture) still show the
            // prompt-crunch phase.
            if self.frames.is_empty() && self.sweeps.is_empty() {
                self.sweeps.push(Sweep { pos: -SWEEP_WIDTH, amp: 0.75 });
            }
            if self.prefill_lit.len() != n {
                self.prefill_lit = vec![0.0; n];
            }
            // Columns the wipe front has already crossed latch to a
            // calm resting brightness and stay there (monotonic).
            for s in &self.sweeps {
                for (i, lit) in self.prefill_lit.iter_mut().enumerate() {
                    let frac = i as f32 / (n - 1).max(1) as f32;
                    if frac <= s.pos {
                        *lit = lit.max(PREFILL_REST_LEVEL);
                    }
                }
            }
            let mut targets = self.prefill_lit.clone();
            // The moving ignition front itself: a narrow bright spike
            // right at each in-flight sweep's leading edge.
            for s in &self.sweeps {
                for (i, t) in targets.iter_mut().enumerate() {
                    let frac = i as f32 / (n - 1).max(1) as f32;
                    let d = ((frac - s.pos) / SWEEP_WIDTH).abs();
                    if d < 1.0 {
                        *t = t.max(s.amp * (1.0 - d));
                    }
                }
            }
            if self.frames.is_empty() {
                for (i, t) in targets.iter_mut().enumerate() {
                    *t = t.max(resample(latest, i, n) * 0.2);
                }
            }
            return targets;
        }
        // Decode tail: real replay clock (plan Task 4), never gated by
        // render-tick counting.
        let prev = self.decode_clock;
        self.decode_clock += dt;
        self.decode_snap = (self.decode_clock / SNAP_SECS).min(1.0);
        let loop_period = self.gen_total_secs.filter(|&t| t > 0.05);
        let (replay_prev, replay_now) = match loop_period {
            Some(total) => (prev.rem_euclid(total), self.decode_clock.rem_euclid(total)),
            None => (prev, self.decode_clock),
        };
        self.spawn_flares(replay_prev, replay_now, |f| f.at);
        self.update_embers(n);
        let idx = self
            .frames
            .iter()
            .position(|f| f.at >= replay_now)
            .unwrap_or_else(|| self.frames.len().saturating_sub(1));
        (0..n).map(|i| self.frame_act(idx, i)).collect()
    }

    /// Spawn beads/sparks for every captured frame whose real crest
    /// timestamp (`crest_of`) falls in `[replay_prev, replay_now)` —
    /// the shared token-flare replay engine (plan Task 6) driving both
    /// [`Self::replay_targets`] (Speaking, crested against the audio
    /// timeline) and [`Self::thinking_targets`] (the Thinking decode
    /// tail, crested against each frame's real arrival time), so the
    /// two phases speak one continuous visual language.
    fn spawn_flares(
        &mut self,
        replay_prev: f32,
        replay_now: f32,
        crest_of: impl Fn(&ReplayFrame) -> f32,
    ) {
        self.beads.clear();
        for idx in 0..self.frames.len() {
            let crest = crest_of(&self.frames[idx]);
            if replay_now >= crest - BEAD_TRAVEL_SECS && replay_now < crest {
                let x = 1.0 - (crest - replay_now) / BEAD_TRAVEL_SECS;
                let token = self.frames[idx].token_index;
                self.beads.push(Bead {
                    x,
                    energy: self.frame_energy(idx),
                    token,
                    frame: Some(idx),
                });
            }
            if crest > replay_prev && crest <= replay_now {
                self.sparks.push(Spark { age: 0.0, energy: self.frame_energy(idx) });
            }
        }
    }

    /// Speaking-phase targets: advance the playback clock, rebuild
    /// the pulse set, fire crest sparks, and light each column from
    /// the wake of the paths passing over it plus a soft ambient
    /// level from the keyframe currently being "spoken".
    ///
    /// With no keyframes (external backend, capture off) the engine
    /// falls back to **cadence mode**: pulses fire at an estimated
    /// word rate over the known audio duration, honestly showing
    /// activity and pace without fabricating internals.
    #[allow(clippy::too_many_lines)]
    fn replay_targets(&mut self, n: usize, dt: f32) -> Vec<f32> {
        let cadence = self.frames.is_empty();
        // External TTS paths that never report an audio duration
        // (e.g. streaming synthesis) still get word pulses: the
        // timeline grows open-endedly with the clock until the
        // speaking state ends.
        let open_ended = cadence && self.audio_secs <= 0.25;
        let span = if open_ended {
            ((self.clock / SYNTH_SECS_PER_WORD).ceil() + 2.0).max(1.0)
        } else if cadence {
            (self.audio_secs / SYNTH_SECS_PER_WORD).ceil().max(1.0)
        } else {
            self.token_span()
        };
        let total_secs = if self.audio_secs > 0.25 {
            self.audio_secs
        } else if cadence {
            span * SYNTH_SECS_PER_WORD
        } else {
            span * SECS_PER_TOKEN_EST
        }
        .max(0.5);
        self.timeline_secs = total_secs;
        let prev = self.clock;
        // Once the audio has drained, fast-forward so trailing pulses
        // wrap up promptly instead of ghost-playing over silence.
        let step = if self.playback_done { dt * 2.5 } else { dt };
        self.clock = (self.clock + step).min(total_secs + BEAD_TRAVEL_SECS);
        if cadence {
            self.beads.clear();
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let words = span as u64;
            for k in 0..words {
                let crest = (k as f32 + 0.5) / span * total_secs;
                if self.clock >= crest - BEAD_TRAVEL_SECS && self.clock < crest {
                    let x = 1.0 - (crest - self.clock) / BEAD_TRAVEL_SECS;
                    self.beads.push(Bead {
                        x,
                        energy: 0.5 + 0.5 * hash01(k.wrapping_mul(0x9E37)),
                        token: k,
                        frame: None,
                    });
                }
                if crest > prev && crest <= self.clock {
                    self.sparks.push(Spark { age: 0.0, energy: 0.7 });
                }
            }
        } else {
            // Same shared token-flare replay engine the Thinking decode
            // tail uses (plan Task 6), crested against the real audio
            // timeline instead of each frame's arrival timestamp.
            self.spawn_flares(prev, self.clock, |f| f.token_index as f32 / span * total_secs);
        }
        self.update_embers(n);
        // Ambient: the keyframe nearest the currently spoken token
        // keeps the lattice faintly alive under the paths.
        let ambient = if cadence {
            None
        } else {
            let pos_tokens = (self.clock / total_secs) * span;
            Some(
                self.frames
                    .iter()
                    .position(|f| f.token_index as f32 >= pos_tokens)
                    .unwrap_or(self.frames.len() - 1),
            )
        };
        let targets = (0..n)
            .map(|i| {
                let frac = i as f32 / (n - 1).max(1) as f32;
                let mut t = ambient.map_or(0.05, |a| self.frame_act(a, i) * 0.25);
                for b in &self.beads {
                    let d = b.x - frac;
                    if (0.0..WAKE_FRAC).contains(&d) {
                        let base = b.frame.map_or(b.energy, |f| self.frame_act(f, i));
                        t = t.max(base * (1.0 - d / WAKE_FRAC));
                    }
                }
                t
            })
            .collect();
        // MoE constellation heat-trace (plan G2): decay every star and
        // deposit the real routing weights of the currently-decoded
        // layer, so the regions of expert space that carried the reply
        // slowly accumulate. Purely real routing — no synthetic roll.
        if self.moe {
            if self.star_heat.len() != N_STARS {
                self.star_heat = vec![0.0; N_STARS];
            }
            for hcell in &mut self.star_heat {
                *hcell *= CONSTELLATION_HEAT_DECAY;
            }
            let front = self.beads.iter().fold(f32::NEG_INFINITY, |m, b| m.max(b.x));
            let sweep = if front.is_finite() { front.clamp(0.0, 1.0) } else { 0.55 };
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let layer =
                ((sweep * (n.saturating_sub(1)) as f32).round() as usize).min(n.saturating_sub(1));
            if let Some(ids) = self.routing.get(layer) {
                let ws = self.weights.get(layer);
                let wmax =
                    ws.map(|w| w.iter().copied().fold(0.0f32, f32::max)).unwrap_or(1.0).max(1e-3);
                for (k, &id) in ids.iter().enumerate() {
                    let w = ws.and_then(|w| w.get(k)).copied().unwrap_or(1.0) / wmax;
                    let s = expert_star(id);
                    self.star_heat[s] = (self.star_heat[s] + w.clamp(0.0, 1.0) * 0.14).min(1.0);
                }
            }
        }
        targets
    }

    /// Smoothed activation for layer `i` (0 when out of range).
    #[must_use]
    pub fn activation(&self, i: usize) -> f32 {
        self.activation.get(i).copied().unwrap_or(0.0)
    }

    /// Heat-trace value for layer `i` (0 when out of range).
    #[must_use]
    pub fn heat(&self, i: usize) -> f32 {
        self.heat.get(i).copied().unwrap_or(0.0)
    }

    /// Beads currently in flight (speaking phase; empty otherwise).
    #[must_use]
    pub fn beads(&self) -> &[Bead] {
        &self.beads
    }

    /// Whether the scene needs continuous repaints that no external
    /// data push will trigger. Listening self-drives from mic FFT
    /// pushes, and Idle is static, so only the thinking and speaking
    /// phases need the backend to pump frames on a timer.
    #[must_use]
    pub fn needs_animation_frames(&self) -> bool {
        matches!(self.phase, Phase::Thinking | Phase::Speaking)
    }

    /// Active right-edge sparks as `(life_frac 0..1, energy)`.
    #[must_use]
    pub fn spark_states(&self) -> Vec<(f32, f32)> {
        self.sparks.iter().map(|s| ((s.age / SPARK_LIFE_SECS).clamp(0.0, 1.0), s.energy)).collect()
    }

    /// Whether replay data exists for the ribbon / HUD overlays.
    #[must_use]
    pub fn has_replay(&self) -> bool {
        !self.frames.is_empty() && matches!(self.phase, Phase::Thinking | Phase::Speaking)
    }

    /// Listening spectrum energy (0..1) for grid column `col` of
    /// `cols`. X = frequency (log-spaced), sampled from the smoothed
    /// mic-FFT bands with linear interpolation (plan C, redesigned).
    #[must_use]
    pub fn spec_col(&self, col: usize, cols: usize) -> f32 {
        if self.spec_bands.is_empty() || cols == 0 {
            return 0.0;
        }
        let p = if cols > 1 { col as f32 / (cols - 1) as f32 } else { 0.0 };
        sample_frac(&self.spec_bands, p)
    }

    /// Slow-falling peak-hold cap (0..1) for grid column `col` of
    /// `cols`, parallel to [`Self::spec_col`].
    #[must_use]
    pub fn spec_peak_col(&self, col: usize, cols: usize) -> f32 {
        if self.spec_peak.is_empty() || cols == 0 {
            return 0.0;
        }
        let p = if cols > 1 { col as f32 / (cols - 1) as f32 } else { 0.0 };
        sample_frac(&self.spec_peak, p)
    }

    /// Sample the real reply-audio spectrum timeline at the current
    /// playback clock. Returns `None` when no audio bands were pushed.
    fn audio_sample(&self) -> Option<usize> {
        if self.audio_frames.is_empty() {
            return None;
        }
        let mut best = 0usize;
        let mut best_d = f32::MAX;
        for (i, (t, _, _)) in self.audio_frames.iter().enumerate() {
            let d = (t - self.clock).abs();
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        Some(best)
    }

    /// Real reply-audio band energy (0..1) for grid `row` of `nrows`,
    /// sampled at the playback clock. Rows map to frequency bands with
    /// the lowest band at the bottom (same axis as listening). `0` when
    /// no real audio spectrum is available.
    #[must_use]
    pub fn audio_row(&self, row: usize, nrows: usize) -> f32 {
        let Some(i) = self.audio_sample() else { return 0.0 };
        let bands = &self.audio_frames[i].1;
        if bands.is_empty() || nrows == 0 {
            return 0.0;
        }
        let freq = nrows.saturating_sub(1).saturating_sub(row);
        resample(bands, freq, nrows)
    }

    /// Current real reply-audio amplitude (0..1) sampled at the
    /// playback clock, plus whether any real audio band data exists.
    #[must_use]
    pub fn audio_amp_now(&self) -> (f32, bool) {
        self.audio_sample().map_or((0.0, false), |i| (self.audio_frames[i].2, true))
    }

    /// Thinking decode-latch state and the one-shot prefill→decode
    /// snap wipe's progress (plan Task 3): `(latched, snap_t 0..1)`.
    /// The snap front sweeps left→right across the columns as `snap_t`
    /// advances from 0 to 1 over `SNAP_SECS`; once it reaches 1.0 the
    /// prefill field must never be evaluated again this reply.
    #[must_use]
    pub fn decode_snap(&self) -> (bool, f32) {
        (self.decode_latched, self.decode_snap)
    }

    /// Per-layer prefill "read" latch (0..1, plan Task 2) for `layer`
    /// — the calm resting brightness a column holds once the ignition
    /// wipe has crossed it. `0.0` when not yet read or out of range.
    #[must_use]
    pub fn prefill_lit(&self, layer: usize) -> f32 {
        self.prefill_lit.get(layer).copied().unwrap_or(0.0)
    }

    /// In-flight prefill ignition-wipe fronts as `(position 0..1, peak
    /// amplitude 0..1)` — real per-batch `CortexCmd::Prefill` pulses
    /// (plus the synthetic keep-alive sweep when no capture data
    /// exists at all).
    pub fn prefill_sweeps(&self) -> impl Iterator<Item = (f32, f32)> + '_ {
        self.sweeps.iter().map(|s| (s.pos, s.amp))
    }

    /// Routed expert ids for `layer` (empty when none observed).
    #[must_use]
    pub fn routing_at(&self, layer: usize) -> &[i32] {
        self.routing.get(layer).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Routing weights for `layer` (empty when none observed).
    #[must_use]
    pub fn weights_at(&self, layer: usize) -> &[f32] {
        self.weights.get(layer).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Constellation heat-trace for `star` (0..1).
    #[must_use]
    pub fn star_heat(&self, star: usize) -> f32 {
        self.star_heat.get(star).copied().unwrap_or(0.0)
    }

    /// MoE HUD readout `(k, n)` — the top-k width and the (estimated)
    /// total expert population — available once routing was observed.
    /// `n` is derived from the highest expert id seen, so it reflects
    /// real scale even for 128/256-expert models.
    #[must_use]
    pub fn moe_hud(&self) -> Option<(usize, usize)> {
        if !self.moe || self.top_k == 0 {
            return None;
        }
        #[allow(clippy::cast_sign_loss)]
        let n = (self.max_expert_id.max(0) as usize) + 1;
        Some((self.top_k, n.max(self.top_k)))
    }

    /// True while the MoE constellation should be shown (routing seen
    /// and in the speaking phase).
    #[must_use]
    pub fn is_moe_speaking(&self) -> bool {
        self.moe && self.phase == Phase::Speaking
    }

    /// Normalised token entropy (0..1) at `frac` of the reply
    /// timeline — linear interpolation between keyframes. Feeds the
    /// uncertainty ribbon.
    #[must_use]
    pub fn entropy_at(&self, frac: f32) -> f32 {
        if self.frames.is_empty() {
            return 0.0;
        }
        let pos = frac.clamp(0.0, 1.0) * (self.token_span() - 1.0);
        let mut prev: Option<&ReplayFrame> = None;
        for f in &self.frames {
            let ft = f.token_index as f32;
            if ft >= pos {
                let e = prev.map_or(f.entropy, |p| {
                    let pt = p.token_index as f32;
                    let w = if ft > pt { (pos - pt) / (ft - pt) } else { 1.0 };
                    p.entropy + (f.entropy - p.entropy) * w.clamp(0.0, 1.0)
                });
                return (e / ENTROPY_NORM_BITS).clamp(0.0, 1.0);
            }
            prev = Some(f);
        }
        (self.frames.last().map_or(0.0, |f| f.entropy) / ENTROPY_NORM_BITS).clamp(0.0, 1.0)
    }

    /// Playback progress 0..1 along the replay timeline (0 outside
    /// the speaking phase).
    #[must_use]
    pub fn playback_frac(&self) -> f32 {
        if self.phase != Phase::Speaking || self.timeline_secs <= 0.0 {
            return 0.0;
        }
        (self.clock / self.timeline_secs).clamp(0.0, 1.0)
    }

    /// HUD arc fills as `(tok_per_sec_frac, ctx_fill)`, available
    /// once `ReplyEnd` delivered the stats.
    #[must_use]
    pub fn hud(&self) -> Option<(f32, f32)> {
        if self.tok_per_sec <= 0.0 && self.ctx_fill <= 0.0 {
            return None;
        }
        Some(((self.tok_per_sec / HUD_TOKPS_FULL).clamp(0.0, 1.0), self.ctx_fill))
    }
}

/// Linear resample of an FFT bin array onto layer column `i` of `n`.
fn resample(bins: &[f32], i: usize, n: usize) -> f32 {
    if bins.is_empty() {
        return 0.0;
    }
    if bins.len() == 1 {
        return bins[0].clamp(0.0, 1.0);
    }
    let pos = i as f32 / (n - 1).max(1) as f32 * (bins.len() - 1) as f32;
    let lo = pos.floor() as usize;
    let hi = (lo + 1).min(bins.len() - 1);
    let frac = pos - lo as f32;
    (bins[lo] * (1.0 - frac) + bins[hi] * frac).clamp(0.0, 1.0)
}

/// Sample a 0..1-normalised bin array at fractional position `p`
/// (0 = first bin, 1 = last) with linear interpolation. Used to
/// log-spread the mic FFT across the listening spectrum bands and to
/// resample those bands onto the grid columns at draw time.
fn sample_frac(bins: &[f32], p: f32) -> f32 {
    if bins.is_empty() {
        return 0.0;
    }
    if bins.len() == 1 {
        return bins[0].clamp(0.0, 1.0);
    }
    let pos = p.clamp(0.0, 1.0) * (bins.len() - 1) as f32;
    let lo = pos.floor() as usize;
    let hi = (lo + 1).min(bins.len() - 1);
    let frac = pos - lo as f32;
    (bins[lo] * (1.0 - frac) + bins[hi] * frac).clamp(0.0, 1.0)
}

/// Additive two-lobe radial glow centred at `(cx, cy)`.
///
/// The inner lobe (`radius`) is a bright `(1 - d²/r²)²` core; the
/// outer lobe (`2 × radius`) is a faint halo at a quarter of the
/// intensity — the "fake bloom". Channels saturate-add on top of the
/// existing framebuffer; the alpha channel is lifted by the largest
/// added component so the premultiplied invariant (`channel ≤
/// alpha`) holds against the translucent panel background.
///
/// No square roots: both falloffs are expressed in d², and the
/// bounding box is clipped before the per-pixel loop.
pub fn add_glow(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    cx: f32,
    cy: f32,
    radius: f32,
    color: u32,
    intensity: f32,
) {
    if radius <= 0.5 || intensity <= 0.0 {
        return;
    }
    let halo_r = radius * 2.0;
    let inv_core_r2 = 1.0 / (radius * radius);
    let inv_halo_r2 = 1.0 / (halo_r * halo_r);
    let cr = ((color >> 16) & 0xFF) as f32;
    let cg = ((color >> 8) & 0xFF) as f32;
    let cb = (color & 0xFF) as f32;
    let x_min = ((cx - halo_r).floor() as i32).max(0);
    let x_max = ((cx + halo_r).ceil() as i32).min(stride as i32 - 1);
    let y_min = ((cy - halo_r).floor() as i32).max(0);
    let y_max = ((cy + halo_r).ceil() as i32).min(h as i32 - 1);
    for yi in y_min..=y_max {
        let dy = yi as f32 + 0.5 - cy;
        let dy2 = dy * dy;
        let row = yi as u32 * stride;
        for xi in x_min..=x_max {
            let dx = xi as f32 + 0.5 - cx;
            let d2 = dx * dx + dy2;
            // Core lobe.
            let core = (1.0 - d2 * inv_core_r2).max(0.0);
            // Halo lobe (the fake bloom).
            let halo = (1.0 - d2 * inv_halo_r2).max(0.0);
            let g = (core * core + halo * halo * 0.25) * intensity;
            if g <= 0.003 {
                continue;
            }
            let idx = (row + xi as u32) as usize;
            if let Some(slot) = buf.get_mut(idx) {
                let px = *slot;
                let add_r = (cr * g) as u32;
                let add_g = (cg * g) as u32;
                let add_b = (cb * g) as u32;
                let r = ((px >> 16) & 0xFF) + add_r;
                let gch = ((px >> 8) & 0xFF) + add_g;
                let b = (px & 0xFF) + add_b;
                let max_add = add_r.max(add_g).max(add_b);
                let a = ((px >> 24) & 0xFF) + max_add;
                *slot = (a.min(255) << 24) | (r.min(255) << 16) | (gch.min(255) << 8) | b.min(255);
            }
        }
    }
}

/// Lerp an `0xAARRGGBB` colour toward white by `t` (0..1). Used for
/// heat-trace whitening of the lattice nodes.
fn whiten(color: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let a = (color >> 24) & 0xFF;
    let r = ((color >> 16) & 0xFF) as f32;
    let g = ((color >> 8) & 0xFF) as f32;
    let b = (color & 0xFF) as f32;
    let r = (r + (255.0 - r) * t) as u32;
    let g = (g + (255.0 - g) * t) as u32;
    let b = (b + (255.0 - b) * t) as u32;
    (a << 24) | (r << 16) | (g << 8) | b
}

/// Lerp between two `0xAARRGGBB` colours by `t` (0..1). Carries the
/// activation → uncertainty hue axis (accent → magenta).
fn mix_color(c0: u32, c1: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |shift: u32| {
        let a = ((c0 >> shift) & 0xFF) as f32;
        let b = ((c1 >> shift) & 0xFF) as f32;
        ((a + (b - a) * t) as u32) << shift
    };
    ch(24) | ch(16) | ch(8) | ch(0)
}

/// SplitMix64-derived hash mapped to 0..1 — stable across runs, so a
/// token always draws the same path.
fn hash01(x: u64) -> f32 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    ((z >> 40) as f32) / ((1u64 << 24) as f32)
}

/// The grid row token `token`'s path visits in each column `0..=upto`
/// of a `rows`-row lattice: starts at a hashed row and wanders at
/// most one row per column, so consecutive layers stay connected and
/// every token draws a visibly different route through the lattice.
/// This is the real per-row texture generator for the decode flare
/// (plan Task 5): the row is stable and deterministic for a given
/// real token id, so a token's flare always lights the same
/// connected path rather than a flat column fill.
fn path_rows(token: u64, upto: usize, rows: usize) -> Vec<usize> {
    let rows = rows.max(1);
    let mut out = Vec::with_capacity(upto + 1);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let mut r = ((hash01(token) * rows as f32) as i32).clamp(0, rows as i32 - 1);
    out.push(r as usize);
    for k in 1..=upto {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let step = (hash01(token.wrapping_mul(31).wrapping_add(k as u64 * 7919)) * 3.0) as i32 - 1;
        r = (r + step).clamp(0, rows as i32 - 1);
        out.push(r as usize);
    }
    out
}

/// Quarter-resolution additive glow accumulator — the deferred
/// fake-bloom pass. All scene glows splat into this buffer at
/// `1/GLOW_DOWN` resolution (so a big halo touches 1/16 the pixels),
/// then [`Self::composite`] bilinearly upsamples the accumulated
/// energy onto the framebuffer in one pass. Reused across frames via
/// a thread-local in [`draw_cortex`], so the steady state allocates
/// nothing.
#[derive(Default)]
struct GlowAccum {
    w: u32,
    h: u32,
    /// Physical-pixel downsampling factor for the current surface
    /// (`GLOW_DOWN × HiDPI scale`, min `GLOW_DOWN`).
    down: u32,
    /// RGB energy per low-res cell (`w * h * 3`), in 0..255 units.
    data: Vec<f32>,
    /// Dirty bounding box in low-res cells (`x0, x1, y0, y1`,
    /// inclusive); `None` when the buffer is clean.
    dirty: Option<(u32, u32, u32, u32)>,
}

impl GlowAccum {
    /// Size for a full-res surface and clear last frame's energy
    /// (dirty region only — steady-state cost scales with lit area).
    fn reset(&mut self, full_w: u32, full_h: u32, scale: f32) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let down = GLOW_DOWN * (scale.round().max(1.0) as u32);
        let w = full_w.div_ceil(down);
        let h = full_h.div_ceil(down);
        if self.w != w || self.h != h || self.down != down {
            self.w = w;
            self.h = h;
            self.down = down;
            self.data.clear();
            self.data.resize((w * h * 3) as usize, 0.0);
            self.dirty = None;
            return;
        }
        if let Some((x0, x1, y0, y1)) = self.dirty.take() {
            for y in y0..=y1 {
                let row = (y * self.w + x0) as usize * 3;
                let end = (y * self.w + x1 + 1) as usize * 3;
                self.data[row..end].fill(0.0);
            }
        }
    }

    fn mark(&mut self, x0: u32, x1: u32, y0: u32, y1: u32) {
        self.dirty = Some(match self.dirty {
            None => (x0, x1, y0, y1),
            Some((a0, a1, b0, b1)) => (a0.min(x0), a1.max(x1), b0.min(y0), b1.max(y1)),
        });
    }

    /// Splat a two-lobe glow (bright `(1-d²/r²)²` core + wide faint
    /// halo) at full-res centre `(cx, cy)` with full-res `radius`.
    /// No square roots; the bounding box is clipped up front.
    fn add(&mut self, cx: f32, cy: f32, radius: f32, color: u32, intensity: f32) {
        if radius <= 0.5 || intensity <= 0.0 || self.w == 0 {
            return;
        }
        let s = self.down as f32;
        let (cx, cy, radius) = (cx / s, cy / s, radius / s);
        let halo_r = (radius * 2.0).max(1.0);
        let core_r = radius.max(0.5);
        let inv_core_r2 = 1.0 / (core_r * core_r);
        let inv_halo_r2 = 1.0 / (halo_r * halo_r);
        let cr = ((color >> 16) & 0xFF) as f32;
        let cg = ((color >> 8) & 0xFF) as f32;
        let cb = (color & 0xFF) as f32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let x_min = ((cx - halo_r).floor().max(0.0)) as u32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let y_min = ((cy - halo_r).floor().max(0.0)) as u32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let x_max = (((cx + halo_r).ceil().max(0.0)) as u32).min(self.w - 1);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let y_max = (((cy + halo_r).ceil().max(0.0)) as u32).min(self.h - 1);
        if x_min > x_max || y_min > y_max {
            return;
        }
        self.mark(x_min, x_max, y_min, y_max);
        for yi in y_min..=y_max {
            let dy = yi as f32 + 0.5 - cy;
            let dy2 = dy * dy;
            let row = (yi * self.w) as usize * 3;
            for xi in x_min..=x_max {
                let dx = xi as f32 + 0.5 - cx;
                let d2 = dx * dx + dy2;
                let core = (1.0 - d2 * inv_core_r2).max(0.0);
                let halo = (1.0 - d2 * inv_halo_r2).max(0.0);
                let g = (core * core + halo * halo * 0.25) * intensity;
                if g <= 0.003 {
                    continue;
                }
                let idx = row + xi as usize * 3;
                self.data[idx] += cr * g;
                self.data[idx + 1] += cg * g;
                self.data[idx + 2] += cb * g;
            }
        }
    }

    /// Whether any energy sits in the 3×3 low-res neighbourhood of
    /// cell `(bx, by)` — the reach of bilinear sampling for full-res
    /// pixels inside that cell's block. Lets [`Self::composite`] skip
    /// dark blocks wholesale so its cost scales with lit area, not
    /// panel area (the difference between ~7 ms and ~3 ms at 2×
    /// HiDPI).
    fn block_lit(&self, bx: u32, by: u32) -> bool {
        let lw = self.w as usize;
        for y in by.saturating_sub(1)..=(by + 1).min(self.h - 1) {
            let row = y as usize * lw * 3;
            for x in bx.saturating_sub(1)..=(bx + 1).min(self.w - 1) {
                let i = row + x as usize * 3;
                if self.data[i] + self.data[i + 1] + self.data[i + 2] > 1.0 {
                    return true;
                }
            }
        }
        false
    }

    /// Bilinearly upsample the accumulated energy and saturate-add it
    /// onto the framebuffer (alpha lifted by the largest added
    /// channel, preserving the premultiplied invariant). Visits the
    /// dirty region block-by-block, skipping blocks with no energy in
    /// bilinear reach (see [`Self::block_lit`]).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn composite(&self, buf: &mut [u32], stride: u32, h: u32) {
        let Some((dx0, dx1, dy0, dy1)) = self.dirty else { return };
        // Low-res block range, padded one cell for bilinear reach.
        let bx0 = dx0.saturating_sub(1);
        let by0 = dy0.saturating_sub(1);
        let bx1 = (dx1 + 1).min(self.w - 1);
        let by1 = (dy1 + 1).min(self.h - 1);
        for by in by0..=by1 {
            for bx in bx0..=bx1 {
                if self.block_lit(bx, by) {
                    self.composite_block(buf, stride, h, bx, by);
                }
            }
        }
    }

    /// Composite one `down`×`down` full-res block for low-res cell
    /// `(bx, by)`.
    ///
    /// Separable bilinear: per output row, the two low-res rows are
    /// blended once into three local cell values (`bx-1`, `bx`,
    /// `bx+1` — the horizontal reach of pixels inside this block),
    /// then each pixel is a single horizontal lerp of those locals.
    /// This drops the inner loop from twelve scattered buffer fetches
    /// and four weight products per pixel to three fused lerps on
    /// registers — the difference between the cortex costing ~4× and
    /// ~1.5× the terrain baseline per frame.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn composite_block(&self, buf: &mut [u32], stride: u32, h: u32, bx: u32, by: u32) {
        let fx0 = bx * self.down;
        let fy0 = by * self.down;
        let fx1 = ((bx + 1) * self.down - 1).min(stride - 1);
        let fy1 = ((by + 1) * self.down - 1).min(h - 1);
        let lw = self.w as usize;
        let inv_down = 1.0 / self.down as f32;
        // Clamped low-res columns reachable from this block.
        let cxm = (bx as usize).saturating_sub(1);
        let cx = bx as usize;
        let cxp = (cx + 1).min(lw - 1);
        let bxf = bx as f32;
        for y in fy0..=fy1 {
            // Low-res sample row pair + vertical weight (cell centres).
            let sy = (y as f32 + 0.5) * inv_down - 0.5;
            let y0 = sy.floor().max(0.0) as usize;
            let y1c = (y0 + 1).min(self.h as usize - 1);
            let wy = (sy - y0 as f32).clamp(0.0, 1.0);
            let row0 = y0 * lw * 3;
            let row1 = y1c * lw * 3;
            // Row-blended cell values (three cells × RGB).
            let mut vm = [0.0f32; 3];
            let mut v0 = [0.0f32; 3];
            let mut vp = [0.0f32; 3];
            for c in 0..3 {
                vm[c] = self.data[row0 + cxm * 3 + c]
                    + (self.data[row1 + cxm * 3 + c] - self.data[row0 + cxm * 3 + c]) * wy;
                v0[c] = self.data[row0 + cx * 3 + c]
                    + (self.data[row1 + cx * 3 + c] - self.data[row0 + cx * 3 + c]) * wy;
                vp[c] = self.data[row0 + cxp * 3 + c]
                    + (self.data[row1 + cxp * 3 + c] - self.data[row0 + cxp * 3 + c]) * wy;
            }
            // Interpolated values can't exceed the cell values, so a
            // dark cell triple means the whole row is skippable.
            let row_max =
                (vm[0] + vm[1] + vm[2]).max(v0[0] + v0[1] + v0[2]).max(vp[0] + vp[1] + vp[2]);
            if row_max <= 1.0 {
                continue;
            }
            let out_row = (y * stride) as usize;
            for x in fx0..=fx1 {
                let sx = (x as f32 + 0.5) * inv_down - 0.5;
                // Left neighbour is `bx-1` for the block's first half,
                // `bx` for the second.
                let (a, b, wx) = if sx < bxf {
                    (&vm, &v0, (sx - (bxf - 1.0)).clamp(0.0, 1.0))
                } else {
                    (&v0, &vp, (sx - bxf).clamp(0.0, 1.0))
                };
                let rf = a[0] + (b[0] - a[0]) * wx;
                let gf = a[1] + (b[1] - a[1]) * wx;
                let bf = a[2] + (b[2] - a[2]) * wx;
                if rf + gf + bf <= 1.0 {
                    continue;
                }
                let slot = &mut buf[out_row + x as usize];
                let px = *slot;
                let add_r = rf as u32;
                let add_g = gf as u32;
                let add_b = bf as u32;
                let r = ((px >> 16) & 0xFF) + add_r;
                let g = ((px >> 8) & 0xFF) + add_g;
                let b = (px & 0xFF) + add_b;
                let a = ((px >> 24) & 0xFF) + add_r.max(add_g).max(add_b);
                *slot = (a.min(255) << 24) | (r.min(255) << 16) | (g.min(255) << 8) | b.min(255);
            }
        }
    }
}

std::thread_local! {
    /// Per-render-thread reusable glow buffer (the overlay renders
    /// from exactly one thread; tests get their own instance).
    static GLOW: std::cell::RefCell<GlowAccum> = std::cell::RefCell::new(GlowAccum::default());
}

/// Draw the Activation Heatmap scene into the panel area
/// `(x0..x1, y_top..y_bot)`.
///
/// A chunky heat grid fills the whole strip: columns run left → right
/// as the model's layers (depth), rows are sampled units per layer.
/// Cell colour is a grounded 0..1 signal on a per-phase heat ramp
/// (the same [`CortexState`] activation / heat / replay-clock the old
/// scene used — the honest "real vs synthetic" seam lives there, so
/// this drawing code never knows which source backs a cell):
///
/// - **Listening** — a cool, high-contrast ramp driven by the live
///   mic spectrum, with a mic-level bar on the left edge.
/// - **Thinking / prefill** — the whole grid floods (model engaged)
///   with a bright fill-wave sweeping left → right.
/// - **Speaking / decode (dense)** — a bright hot column travels
///   left → right through the grid, synced to TTS via the replay clock.
/// - **Speaking / decode (MoE)** — only routed experts light up,
///   warm = amber (RAM) vs cold = blue (offloaded), with a bright
///   focal hotspot at the active layer.
///
/// A future "flow field" style can slot in beside this as a separate
/// selectable style; only the heatmap is built here.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn draw_cortex(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    cortex: &CortexState,
    x0: f32,
    x1: f32,
    y_top: f32,
    y_bot: f32,
    accent: u32,
    scale: f32,
    elapsed_secs: f32,
) {
    let panel_w = (x1 - x0).max(1.0);
    let panel_h = (y_bot - y_top).max(1.0);
    if panel_w < 8.0 || panel_h < 8.0 {
        return;
    }
    // Slim vertical margin so cells don't jam against the panel edge.
    let my = (panel_h * 0.05).max(scale);
    let gy0 = y_top + my;
    let gy1 = (y_bot - my).max(gy0 + 1.0);
    let area_h = (gy1 - gy0).max(1.0);
    // Integer square lattice (plan Task A1): identical square cells,
    // one uniform integer gap, pixel-aligned and centred.
    let lat = Lattice::compute(x0, gy0, panel_w, area_h, scale);
    let phase = cortex.phase;
    let moe = cortex.is_moe_speaking();

    GLOW.with(|glow| {
        let mut glow = glow.borrow_mut();
        glow.reset(stride, h, scale);

        if moe {
            draw_constellation(buf, stride, h, cortex, &lat, x0, gy0, panel_w, area_h, &mut glow);
        } else {
            draw_lattice_scene(
                buf,
                stride,
                h,
                cortex,
                &lat,
                phase,
                accent,
                elapsed_secs,
                &mut glow,
            );
        }

        // Minimal HUD (top-right): decode throughput + KV fill.
        if cortex.has_replay() {
            if let Some((tokps, ctx_fill)) = cortex.hud() {
                draw_hud_arcs(&mut glow, x1, y_top, accent, scale, tokps, ctx_fill);
            }
        }

        glow.composite(buf, stride, h);
    });
}

/// Integer square-lattice geometry (plan Task A1): identical square
/// cells separated by ONE uniform integer gap, pixel-aligned and
/// centred so every gap is the same width — no fractional rounding,
/// so no uneven gaps.
struct Lattice {
    /// Square cell edge in physical pixels.
    cell: i32,
    /// Uniform gap between cells (and edge inset) in physical pixels.
    gap: i32,
    /// Columns that actually fit.
    cols: usize,
    /// Rows (fixed at [`GRID_ROWS`]).
    rows: usize,
    /// Grid origin (top-left of the first cell).
    ox: i32,
    oy: i32,
}

impl Lattice {
    fn compute(x0: f32, y0: f32, w: f32, h: f32, scale: f32) -> Self {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let gap = ((3.0 * scale).round() as i32).max(2);
        let rows = GRID_ROWS as i32;
        // Cell size derived so `rows` square cells fit the height.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let cell = (((h - (rows - 1) as f32 * gap as f32) / rows as f32).floor() as i32).max(3);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let cols = (((w + gap as f32) / (cell + gap) as f32).floor() as i32).max(1);
        let total_w = cols * cell + (cols - 1) * gap;
        let total_h = rows * cell + (rows - 1) * gap;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ox = (x0 + (w - total_w as f32) * 0.5).round() as i32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let oy = (y0 + (h - total_h as f32) * 0.5).round() as i32;
        Self { cell, gap, cols: cols as usize, rows: rows as usize, ox, oy }
    }

    /// `(x0, y0, x1, y1)` of cell `(col, row)` in physical pixels.
    fn cell_rect(&self, col: usize, row: usize) -> (f32, f32, f32, f32) {
        let step = self.cell + self.gap;
        let x0 = self.ox + col as i32 * step;
        let y0 = self.oy + row as i32 * step;
        (x0 as f32, y0 as f32, (x0 + self.cell) as f32, (y0 + self.cell) as f32)
    }
}

/// Dense square-lattice scene (listening / thinking / speaking on
/// non-MoE models). Rows carry the frequency axis (listening + the
/// speaking voice glow); columns carry model depth (the decode
/// column). Cells below [`ACT_THRESHOLD`] render the real dark panel
/// background (plan Task A3), so the grid looks sparse and meaningful.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn draw_lattice_scene(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    cortex: &CortexState,
    lat: &Lattice,
    phase: Phase,
    accent: u32,
    elapsed_secs: f32,
    glow: &mut GlowAccum,
) {
    let (deep, mid, hot) = heat_palette(phase, accent);
    let cols = lat.cols;
    let rows = lat.rows;
    let n = cortex.layer_count();

    // Listening (plan C, redesigned): "peak dots over a weight field".
    // X = frequency (log-spaced), Y = energy, with exactly one
    // significantly-emphasised dot per frequency column over a dim
    // shimmering ambient grid. Handled in its own pass, so the
    // thinking/speaking depth loop below never runs for listening.
    if phase == Phase::Listening {
        draw_listening_spectrum(buf, stride, h, cortex, lat, elapsed_secs, glow);
        return;
    }
    // Real per-flare replay data (plan Task 6): the same beads +
    // one-shot snap latch drive Thinking's decode tail AND Speaking,
    // so the two phases share one flare grammar instead of each
    // inventing its own travelling-column formula.
    let beads = cortex.beads();
    let (decode_latched, snap_t) = cortex.decode_snap();
    // Real reply-audio (plan E): per-row band glow + global pulse.
    let (audio_amp, has_audio) = cortex.audio_amp_now();

    for col in 0..cols {
        let nx = if cols > 1 { col as f32 / (cols - 1) as f32 } else { 0.0 };
        // Bin the model's layers into the columns that actually fit
        // (plan Task A2), so squares stay square for any layer count.
        let layer = if cols <= 1 { 0 } else { (col * n) / cols }.min(n.saturating_sub(1));
        let act = cortex.activation(layer);
        let heat = cortex.heat(layer);
        let ember = cortex.ember_row.get(col).copied().unwrap_or(u8::MAX);

        // Which row(s), if any, a token flare currently crossing this
        // column lights, and how strongly — one row per bead, taken
        // from that token's real per-layer sample data (plan Task 5:
        // true per-row values, never a flat column fill).
        let mut flares: [(usize, f32); 4] = [(usize::MAX, 0.0); 4];
        let mut n_flares = 0usize;
        for b in beads {
            let dd = nx - b.x;
            if (0.0..WAKE_FRAC).contains(&dd) && n_flares < flares.len() {
                let row = *path_rows(b.token, layer, rows).last().unwrap_or(&0);
                let base = b.frame.map_or(b.energy, |f| cortex.frame_act(f, layer));
                let strength = (base * (1.0 - dd / WAKE_FRAC)).clamp(0.0, 1.0);
                flares[n_flares] = (row, strength);
                n_flares += 1;
            }
        }
        let flares = &flares[..n_flares];

        // Task 3: a single one-shot spatial cut, never a per-frame
        // blend — columns the snap front has already reached show the
        // decode flare grammar; columns still ahead of it (or before
        // the first token has landed at all) keep showing the prefill
        // wipe. Once `snap_t` reaches 1.0 every column is decode and
        // this branch is never true again this reply.
        let use_prefill = phase == Phase::Thinking && (!decode_latched || nx > snap_t);

        for row in 0..rows {
            let (cx0, cy0, cx1, cy1) = lat.cell_rect(col, row);
            let cxc = (cx0 + cx1) * 0.5;
            let cyc = (cy0 + cy1) * 0.5;
            let jitter = cell_hash(col * 13 + row, row * 7 + col);

            let mut v = if use_prefill {
                prefill_cell_value(cortex, act, row, rows, nx, jitter)
            } else {
                // Decode flare (plan Task 5/6): a dim ambient base from
                // the slow heat trace, a resting ember on the row this
                // column's real token path last touched (the "worn
                // route" between flares), and the real per-row flare(s)
                // crossing right now. At most one row per column reads
                // hot from any single flare, so the column never fills
                // flat.
                let mut t = 0.10 + 0.22 * heat;
                if usize::from(ember) == row {
                    t = t.max(0.26 + 0.08 * jitter);
                }
                for &(frow, strength) in flares {
                    if frow == row {
                        t = t.max((0.16 + 0.80 * strength).clamp(0.0, 1.0));
                    }
                }
                t.clamp(0.0, 1.0)
            };

            // Speaking synergy (plan E2): the real spoken voice lights
            // ROWS (frequency) as a capped, shimmering additive glow,
            // plus a gentle global amplitude pulse. The flare stays the
            // primary motion; sound only decorates (honesty invariant —
            // the cell still shows a real activation value, audio only
            // modulates brightness).
            if phase == Phase::Speaking && has_audio {
                let band = cortex.audio_row(row, rows);
                let shimmer =
                    0.6 + 0.4 * (((elapsed_secs * 7.0) + row as f32 * 1.3).sin() * 0.5 + 0.5);
                v += (band * shimmer).min(1.0) * AUDIO_ROW_CAP;
                v += audio_amp * AUDIO_PULSE_CAP;
                v = v.clamp(0.0, 1.0);
            }

            // True-OFF (plan A3): below threshold ⇒ real dark panel bg.
            // A hair of hint is kept only in the band just under it.
            if v < ACT_THRESHOLD {
                if v > ACT_THRESHOLD * 0.6 {
                    let t = (v - ACT_THRESHOLD * 0.6) / (ACT_THRESHOLD * 0.4);
                    let cc = heat_ramp(deep, mid, hot, 0.0);
                    fill_cell(buf, stride, h, cx0, cy0, cx1, cy1, cc, 0.35 * t);
                }
                continue;
            }
            // Renormalise above-threshold values across the full ramp so
            // just-on cells read deep and full cells read hot.
            let t = ((v - ACT_THRESHOLD) / (1.0 - ACT_THRESHOLD)).clamp(0.0, 1.0);
            let cc = heat_ramp(deep, mid, hot, t);
            fill_cell(buf, stride, h, cx0, cy0, cx1, cy1, cc, 0.96);
            // Per-flare, per-cell glow only (plan Task 1): every glow
            // is anchored at, and sized against, the cell that is
            // actually hot — never a floating shape independent of the
            // grid.
            if t > 0.8 {
                glow.add(cxc, cyc, lat.cell as f32 * 0.5, hot, 0.22 * (t - 0.7));
            }
            if !use_prefill && flares.iter().any(|&(frow, s)| frow == row && s > 0.35) {
                glow.add(cxc, cyc, lat.cell as f32 * 0.9, hot, 0.28);
            }
        }
    }
}

/// Prefill-regime cell value (plan Task 2): the per-layer resting
/// "read" brightness the real `CortexCmd::Prefill` progress has
/// already latched (folded into `act` by [`CortexState::tick_dt`]),
/// plus the moving ignition front itself. The front is offset per row
/// by [`ROW_STAGGER`] (a fraction of [`SWEEP_WIDTH`]) so consecutive
/// rows latch at slightly different moments — a real diagonal-feeling
/// wipe, not a flat bar advancing in lockstep — and the resting level
/// is textured by the same per-cell jitter the rest of the scene uses
/// so no two rows in a column ever read identically (no uniform
/// column fill).
fn prefill_cell_value(
    cortex: &CortexState,
    act: f32,
    row: usize,
    rows: usize,
    nx: f32,
    jitter: f32,
) -> f32 {
    let mut v = (0.18 + 0.62 * act + 0.08 * jitter).clamp(0.0, 1.0);
    let row_off = if rows > 1 {
        (row as f32 / (rows - 1) as f32 - 0.5) * SWEEP_WIDTH * ROW_STAGGER
    } else {
        0.0
    };
    for (pos, amp) in cortex.prefill_sweeps() {
        let d = ((nx - row_off) - pos) / SWEEP_WIDTH;
        if d.abs() < 1.0 {
            v = v.max((amp * (1.0 - d.abs())).clamp(0.0, 1.0));
        }
    }
    v.clamp(0.0, 1.0)
}

/// Listening scene (plan C, redesigned): "peak dots over a weight
/// field". X = frequency (log-spaced mic FFT bands, reusing the real
/// bins the session already pushes), Y = energy. Every grid cell shows
/// a dim, slowly-shimmering ambient "weight" box so the whole panel
/// reads as living texture; over it, each frequency column emphasises
/// exactly ONE hot dot at the height of its current energy, plus a
/// slow-falling peak-hold cap. Silence rests the dots near the bottom,
/// so the scene is always alive but never a flat wall (fixes the old
/// single-row collapse).
#[allow(clippy::too_many_arguments)]
fn draw_listening_spectrum(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    cortex: &CortexState,
    lat: &Lattice,
    elapsed_secs: f32,
    glow: &mut GlowAccum,
) {
    let (deep, mid, hot) = (LISTEN_DEEP, LISTEN_MID, LISTEN_HOT);
    let cols = lat.cols;
    let rows = lat.rows;
    if cols == 0 || rows == 0 {
        return;
    }

    // Weight-field: every cell a dim ambient box so the whole grid
    // reads as a living brain at rest. A slow travelling shimmer gives
    // an organic base pulse, and — crucially — a small, ever-changing
    // subset of cells briefly "fires" (a brighter cool-blue flash that
    // rises fast and decays) so the resting network looks like it is
    // idly thinking rather than sitting dead. Sparse and capped so it
    // never competes with the warm energy dots.
    //
    // Each cell owns a phase offset (hashed) and cycles on a slow
    // period; only the narrow window near the top of its cycle lights
    // up, so at any instant only a handful of cells are firing and they
    // never pulse in lockstep.
    for row in 0..rows {
        for col in 0..cols {
            let (x0, y0, x1, y1) = lat.cell_rect(col, row);
            let cf = col as f32;
            let rf = row as f32;

            // Base shimmer: two counter-travelling ripples, kept low but
            // now with a visible floor so the grid is never fully black.
            let w1 = (elapsed_secs * 1.05 - cf * 0.33 + rf * 0.21).sin();
            let w2 = (elapsed_secs * 0.63 + cf * 0.17 - rf * 0.44).sin();
            let shimmer = ((w1 + w2) * 0.25 + 0.5).clamp(0.0, 1.0);
            let base_a = 0.035 + 0.04 * shimmer;
            fill_cell(buf, stride, h, x0, y0, x1, y1, WEIGHT_FIELD_COLOR, base_a);

            // Sparse firing: each cell cycles at ~0.5 Hz with a hashed
            // phase; only the top ~15% of the cycle ignites, so few cells
            // are lit at once and never synchronously.
            let phase = cell_hash(col * 7 + 3, row * 11 + 5);
            let cycle = (elapsed_secs * 0.5 + phase).fract();
            if cycle > 0.85 {
                let fire = (cycle - 0.85) / 0.15; // 0->1 across the window
                let env = (fire * std::f32::consts::PI).sin(); // smooth rise+fall
                let fa = 0.14 * env;
                fill_cell(buf, stride, h, x0, y0, x1, y1, WEIGHT_FIELD_SPARK, fa);
                let cx = (x0 + x1) * 0.5;
                let cy = (y0 + y1) * 0.5;
                glow.add(cx, cy, lat.cell as f32 * 0.5, WEIGHT_FIELD_SPARK, 0.08 * env);
            }
        }
    }

    // Energy axis geometry: every dot snaps to a grid row (row 0 = loud
    // top, row N-1 = quiet bottom) so both the energy dots and the
    // peak-hold caps sit cleanly on cells rather than floating between.
    let step = (lat.cell + lat.gap) as f32;
    let col_cx = |c: usize| lat.ox as f32 + c as f32 * step + lat.cell as f32 * 0.5;
    let row_cy = |r: usize| lat.oy as f32 + r as f32 * step + lat.cell as f32 * 0.5;
    let dot_r = (lat.cell as f32 * 0.42).max(1.5);

    let nb = cortex.spec_bands.len();
    let np = cortex.spec_peak.len();
    for col in 0..cols {
        // Sample this column's frequency band (spectrum length is
        // independent of column count; resample proportionally).
        let e = if nb == 0 { 0.0 } else { cortex.spec_bands[(col * nb / cols).min(nb - 1)] };
        let pk = if np == 0 { e } else { cortex.spec_peak[(col * np / cols).min(np - 1)] };
        let cx = col_cx(col);

        // Emphasised energy dot: brightness + colour climb with energy.
        // Snap to the nearest grid row so the dot sits on a cell rather
        // than floating at an interpolated height (row 0 = loud/top).
        let erow = ((1.0 - e.clamp(0.0, 1.0)) * (rows - 1) as f32).round() as usize;
        let ey = row_cy(erow.min(rows - 1));
        let color = heat_ramp(deep, mid, hot, 0.35 + 0.65 * e);
        fill_dot(buf, stride, h, cx, ey, dot_r * (0.8 + 0.5 * e), color, (0.55 + 0.4 * e).min(1.0));
        glow.add(cx, ey, lat.cell as f32 * (0.7 + 0.8 * e), hot, (0.3 + 0.7 * e).min(1.2));

        // Peak-hold cap: thin bright marker that hangs, then drifts down.
        // Snap it to the nearest grid row so it sits on a cell rather than
        // floating at an interpolated height (row 0 = loud/top).
        let prow = ((1.0 - pk.clamp(0.0, 1.0)) * (rows - 1) as f32).round() as usize;
        let py = row_cy(prow.min(rows - 1));
        fill_dot(buf, stride, h, cx, py, dot_r * 0.55, 0x00FF_FFFF, 0.5);
    }
}

/// MoE Constellation scene (plan G): a sparse field of dim stars where
/// only the REAL routed top-k experts of the currently-decoded layer
/// ignite (brightness = real routing weight), with thin threads
/// between them in weight order and a slow heat-trace of the regions
/// that carried the reply. Most stars stay dark (true-OFF). Expert ids
/// map into the fixed star field via a hash, so it stays legible from
/// 8 to 256+ experts. No synthetic hash roll decides which ignite.
#[allow(clippy::too_many_arguments)]
fn draw_constellation(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    cortex: &CortexState,
    lat: &Lattice,
    ax0: f32,
    ay0: f32,
    aw: f32,
    ah: f32,
    glow: &mut GlowAccum,
) {
    // Real reply-audio: gentle global brightness pulse (same subtlety
    // cap / honesty invariant as the heatmap).
    let (audio_amp, has_audio) = cortex.audio_amp_now();
    let pulse = if has_audio { 1.0 + AUDIO_PULSE_CAP * audio_amp * 2.0 } else { 1.0 };
    let star_r = (lat.cell as f32 * 0.16).max(1.0);

    // Dim implied population + slow heat-trace residue.
    for s in 0..N_STARS {
        let (px, py) = star_pos(s, ax0, ay0, aw, ah);
        let ht = cortex.star_heat(s);
        if ht > 0.02 {
            glow.add(px, py, lat.cell as f32 * 0.7, CONSTELLATION_HOT, (0.14 * ht).min(0.4));
        }
        fill_dot(buf, stride, h, px, py, star_r, CONSTELLATION_DIM, 0.5);
    }

    // Currently-decoded layer's REAL routing (depth = progression).
    let n = cortex.layer_count();
    let front = cortex.beads().iter().fold(f32::NEG_INFINITY, |m, b| m.max(b.x));
    let sweep = if front.is_finite() { front.clamp(0.0, 1.0) } else { 0.55 };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let layer = ((sweep * n.saturating_sub(1) as f32).round() as usize).min(n.saturating_sub(1));
    let ids = cortex.routing_at(layer);
    let ws = cortex.weights_at(layer);
    let wmax = ws.iter().copied().fold(0.0f32, f32::max).max(1e-3);

    // Ignite the real top-k experts (brightness = real weight) and
    // remember their positions for the threads (top-k weight order).
    let mut pts: Vec<(f32, f32)> = Vec::with_capacity(ids.len());
    for (k, &id) in ids.iter().enumerate() {
        let s = expert_star(id);
        let (px, py) = star_pos(s, ax0, ay0, aw, ah);
        let w = if ids.is_empty() {
            0.0
        } else {
            ws.get(k).copied().unwrap_or(1.0 / ids.len() as f32) / wmax
        };
        let b = (w.clamp(0.05, 1.0)) * pulse;
        fill_dot(
            buf,
            stride,
            h,
            px,
            py,
            star_r * (1.6 + 1.4 * b),
            CONSTELLATION_HOT,
            (0.6 * b).min(1.0),
        );
        glow.add(
            px,
            py,
            lat.cell as f32 * (0.6 + 0.9 * b),
            CONSTELLATION_HOT,
            (0.4 + 0.7 * b).min(1.3),
        );
        pts.push((px, py));
    }
    // Thin threads between the chosen experts in top-k order.
    for w2 in pts.windows(2) {
        thread(glow, w2[0], w2[1], CONSTELLATION_THREAD, star_r);
    }
}

/// Deterministic star position for star index `s` inside the panel
/// area, with a small margin so ignited glows stay on-panel.
fn star_pos(s: usize, ax0: f32, ay0: f32, aw: f32, ah: f32) -> (f32, f32) {
    let hx = hash01(s as u64 * 2 + 1);
    let hy = hash01(s as u64 * 7 + 3);
    let mx = aw * 0.05;
    let my = ah * 0.14;
    (ax0 + mx + hx * (aw - 2.0 * mx), ay0 + my + hy * (ah - 2.0 * my))
}

/// Map a real expert id into the fixed star field via a hash, so any
/// expert count (8 / 128 / 256) spreads across the implied population
/// without becoming sub-pixel noise (plan G — id-band mapping).
fn expert_star(id: i32) -> usize {
    #[allow(clippy::cast_sign_loss)]
    let seed = (id as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (hash01(seed) * N_STARS as f32) as usize % N_STARS
}

/// Draw a fast-decaying thin light-thread between two star positions as
/// a short chain of faint glow dots (no dedicated line primitive).
fn thread(glow: &mut GlowAccum, p0: (f32, f32), p1: (f32, f32), color: u32, r: f32) {
    const STEPS: usize = 8;
    for i in 1..STEPS {
        let t = i as f32 / STEPS as f32;
        let x = p0.0 + (p1.0 - p0.0) * t;
        let y = p0.1 + (p1.1 - p0.1) * t;
        glow.add(x, y, r * 1.2, color, 0.10);
    }
}

/// Fill a small square dot centred at `(cx, cy)` with radius `r`.
fn fill_dot(buf: &mut [u32], stride: u32, h: u32, cx: f32, cy: f32, r: f32, color: u32, a: f32) {
    fill_cell(buf, stride, h, cx - r, cy - r, cx + r, cy + r, color, a);
}

/// Smoothstep on 0..1.
fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Deterministic per-cell hash in 0..1 (SplitMix-derived) so a cell's
/// jitter / expert roll is stable across frames.
fn cell_hash(a: usize, b: usize) -> f32 {
    hash01(
        (a as u64)
            .wrapping_mul(0x9E37_79B1)
            .wrapping_add((b as u64).wrapping_mul(0x85EB_CA77))
            .wrapping_add(0x1_2345),
    )
}

/// Synthetic-but-stable expert residency: warm (RAM) vs cold (disk).
/// Real residency isn't carried in the keyframe stream, so this is a
/// deterministic per-expert stand-in kept behind the same "synthetic
/// signal" seam as the rest of the degraded path. Deferred/opt-in
/// (plan G3): the shipped constellation drives ignition purely from
/// real routing weights, so this is not wired into the render — it is
/// retained as the home for the flagged synthetic tint until G5 lands
/// real `mincore()`-based residency.
#[allow(dead_code)]
fn expert_warm(id: i32) -> bool {
    #[allow(clippy::cast_sign_loss)]
    let seed = id as u64;
    hash01(seed.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ 0xA5A5) < 0.45
}

/// Listening's dedicated warm red-orange ramp anchors (FIX 1): a
/// lifted maroon floor, a saturated red mid, and a bright orange hot
/// so cells pop clearly instead of reading dark-on-dark. Kept as its
/// own fixed palette (independent of the per-state accent) since it
/// is the already-shipped coherence reference the other phases are
/// tuned against.
const LISTEN_DEEP: u32 = 0x0078_1A12;
const LISTEN_MID: u32 = 0x00E8_3E22;
const LISTEN_HOT: u32 = 0x00FF_B863;

/// Derive a (deep, mid, hot) heat-ramp from the real per-state
/// `accent` colour the rest of the overlay already uses for this
/// phase (status label, System/360's lit-lamp colour) — the same
/// "accent lerped toward white" trick `draw_system_360` uses for its
/// brightest lamps. Thinking and Speaking both route through this so
/// they read as one coherent palette family instead of each inventing
/// its own disconnected hue; they still stay visually distinct from
/// each other because the app already assigns them different accents
/// (amber vs sky-blue).
fn accent_ramp(accent: u32) -> (u32, u32, u32) {
    let base = accent & 0x00FF_FFFF;
    let deep = mix_color(0x0004_0404, base, 0.42);
    let hot = mix_color(base, 0x00FF_FFFF, 0.55);
    (deep, base, hot)
}

/// Per-phase (deep, mid, hot) anchors for the dense heat ramp
/// (`0x00RR_GGBB`).
fn heat_palette(phase: Phase, accent: u32) -> (u32, u32, u32) {
    match phase {
        Phase::Listening => (LISTEN_DEEP, LISTEN_MID, LISTEN_HOT),
        Phase::Thinking | Phase::Speaking => accent_ramp(accent),
        Phase::Idle => (0x000A_0A10, 0x0016_1622, 0x0030_3040),
    }
}

/// Multi-stop heat ramp: `t` in 0..1 walks near-black floor → deep →
/// mid → hot, so `t≈0` cells still show faint grid structure.
fn heat_ramp(deep: u32, mid: u32, hot: u32, t: f32) -> u32 {
    const FLOOR: u32 = 0x000A_0A10;
    let t = t.clamp(0.0, 1.0);
    let seg = t * 3.0;
    if seg < 1.0 {
        mix_color(FLOOR, deep, smooth(seg))
    } else if seg < 2.0 {
        mix_color(deep, mid, smooth(seg - 1.0))
    } else {
        mix_color(mid, hot, smooth(seg - 2.0))
    }
}

/// Alpha-blend a straight `0x00RR_GGBB` colour at coverage `a` (0..1)
/// into the premultiplied ARGB framebuffer over `(x0..x1, y0..y1)`.
/// Keeps the premultiplied invariant (each channel ≤ alpha): `a·src ≤
/// a·255` and the destination already satisfies `chan ≤ alpha`.
#[allow(clippy::too_many_arguments, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn fill_cell(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    color: u32,
    a: f32,
) {
    let a = a.clamp(0.0, 1.0);
    if a <= 0.0 {
        return;
    }
    let sr = ((color >> 16) & 0xFF) as f32;
    let sg = ((color >> 8) & 0xFF) as f32;
    let sb = (color & 0xFF) as f32;
    let xi0 = x0.floor().max(0.0) as i32;
    let xi1 = (x1.ceil() as i32).min(stride as i32);
    let yi0 = y0.floor().max(0.0) as i32;
    let yi1 = (y1.ceil() as i32).min(h as i32);
    let inv = 1.0 - a;
    for y in yi0..yi1 {
        let rowbase = y as u32 * stride;
        for x in xi0..xi1 {
            let idx = (rowbase + x as u32) as usize;
            if let Some(slot) = buf.get_mut(idx) {
                let px = *slot;
                let da = ((px >> 24) & 0xFF) as f32;
                let dr = ((px >> 16) & 0xFF) as f32;
                let dg = ((px >> 8) & 0xFF) as f32;
                let db = (px & 0xFF) as f32;
                let out_a = (a * 255.0 + da * inv).min(255.0) as u32;
                let out_r = (a * sr + dr * inv).min(255.0) as u32;
                let out_g = (a * sg + dg * inv).min(255.0) as u32;
                let out_b = (a * sb + db * inv).min(255.0) as u32;
                *slot = (out_a << 24) | (out_r << 16) | (out_g << 8) | out_b;
            }
        }
    }
}

/// Minimal HUD: two slim arcs in the top-right corner — decode
/// throughput (outer) and KV-cache fill (inner) — drawn as chains of
/// small additive glow dots (no dedicated 2D arc primitive needed).
#[allow(clippy::too_many_arguments)]
fn draw_hud_arcs(
    glow: &mut GlowAccum,
    x1: f32,
    y_top: f32,
    accent: u32,
    scale: f32,
    tokps: f32,
    ctx_fill: f32,
) {
    let cx = x1 - 16.0 * scale;
    let cy = y_top + 14.0 * scale;
    for (radius, fill) in [(10.0 * scale, tokps), (6.0 * scale, ctx_fill)] {
        const STEPS: usize = 22;
        // 270° sweep starting at the lower-left (135°), clockwise.
        for s in 0..=STEPS {
            let t = s as f32 / STEPS as f32;
            let angle = (135.0 + 270.0 * t).to_radians();
            let px = cx + radius * angle.cos();
            let py = cy + radius * angle.sin();
            let lit = t <= fill && fill > 0.0;
            let (color, intensity) = if lit { (whiten(accent, 0.35), 0.8) } else { (accent, 0.12) };
            glow.add(px, py, 1.4 * scale, color, intensity);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_count_defaults_and_clamps() {
        let mut c = CortexState::default();
        assert_eq!(c.layer_count(), DEFAULT_LAYERS);
        c.set_model_layers(48);
        assert_eq!(c.layer_count(), 48);
        c.set_model_layers(2);
        assert_eq!(c.layer_count(), MIN_LAYERS);
        c.set_model_layers(400);
        assert_eq!(c.layer_count(), MAX_LAYERS);
        c.set_model_layers(0);
        assert_eq!(c.layer_count(), DEFAULT_LAYERS);
    }

    #[test]
    fn tick_resizes_and_accumulates_heat() {
        let mut c = CortexState::default();
        c.on_state(crate::OverlayState::Recording { db: -20 });
        c.tick(&[1.0; 16]);
        assert_eq!(c.activation.len(), DEFAULT_LAYERS);
        let a1 = c.activation(0);
        assert!(a1 > 0.0 && a1 <= 1.0);
        let h1 = c.heat(0);
        assert!(h1 > 0.0);
        // Heat keeps building while activation is sustained…
        for _ in 0..20 {
            c.tick(&[1.0; 16]);
        }
        assert!(c.heat(0) > h1);
        // …and decays once the input goes silent.
        let peak = c.heat(0);
        for _ in 0..40 {
            c.tick(&[0.0; 16]);
        }
        assert!(c.heat(0) < peak);
    }

    #[test]
    fn set_model_layers_resets_trace() {
        let mut c = CortexState::default();
        c.on_state(crate::OverlayState::Recording { db: -20 });
        c.tick(&[1.0; 8]);
        assert!(c.activation(0) > 0.0);
        c.set_model_layers(24);
        assert_eq!(c.layer_count(), 24);
        assert!(c.activation.is_empty());
        c.tick(&[0.5; 8]);
        assert_eq!(c.activation.len(), 24);
    }

    fn frame(token_index: u64, norms: &[f32], entropy: f32) -> crate::CortexCmd {
        crate::CortexCmd::Frame(crate::CortexFrame {
            token_index,
            layer_norms: norms.to_vec(),
            experts: Vec::new(),
            token_prob: None,
            entropy_bits: Some(entropy),
        })
    }

    #[test]
    fn prefill_sweep_crosses_the_spine_and_expires() {
        let mut c = CortexState::default();
        c.on_state(crate::OverlayState::AssistantThinking);
        c.apply(crate::CortexCmd::Prefill { n_tokens: 512 });
        // Early in the crossing the bump sits on the left half.
        c.tick_dt(&[], 0.05);
        let n = c.layer_count();
        let left: f32 = (0..n / 2).map(|i| c.activation(i)).sum();
        let right: f32 = (n / 2..n).map(|i| c.activation(i)).sum();
        assert!(left > right, "sweep should start on the left ({left} vs {right})");
        assert!(c.activation.iter().any(|&a| a > 0.3), "full-amplitude sweep should be bright");
        // After crossing time + margin the pulse is gone.
        for _ in 0..40 {
            c.tick_dt(&[], 0.05);
        }
        // After crossing time + margin the real prefill pulse is gone;
        // only the looping synthetic keep-alive sweep (amp 0.75) may
        // remain since this state has no capture data.
        assert!(
            c.sweeps.iter().all(|s| s.amp < 0.9),
            "full-amplitude prefill sweep should expire after crossing"
        );
        // A ReplyBegin reset must NOT kill an in-flight sweep (prefill
        // events precede the generation's ReplyBegin).
        c.apply(crate::CortexCmd::Prefill { n_tokens: 16 });
        let before = c.sweeps.len();
        c.apply(crate::CortexCmd::ReplyBegin { n_layer: 32 });
        assert_eq!(c.sweeps.len(), before, "ReplyBegin must not clear in-flight sweeps");
    }

    #[test]
    fn replay_ingest_merges_stride_gaps_and_tracks_peaks() {
        let mut c = CortexState::default();
        c.apply(crate::CortexCmd::ReplyBegin { n_layer: 4 });
        assert_eq!(c.layer_count(), MIN_LAYERS, "tiny models clamp to the minimum spine");
        c.apply(frame(0, &[2.0, 0.0, 4.0, 0.0], 1.0));
        c.apply(frame(5, &[0.0, 3.0, 0.0, 1.0], 2.0));
        // Second frame keeps the first frame's values in its gaps.
        assert_eq!(c.frames.len(), 2);
        assert_eq!(c.frames[1].norms, vec![2.0, 3.0, 4.0, 1.0]);
        // Peak layer normalises to full activation.
        assert!((c.frame_act(1, 2) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn speaking_replay_spawns_beads_sparks_and_advances_clock() {
        let mut c = CortexState::default();
        c.apply(crate::CortexCmd::ReplyBegin { n_layer: 4 });
        for i in 0..10 {
            c.apply(frame(i * 4, &[1.0, 1.0, 1.0, 1.0], 1.5));
        }
        c.apply(crate::CortexCmd::ReplyEnd {
            total_tokens: 40,
            gen_ms: 2000,
            ctx_used: 512,
            ctx_capacity: 4096,
        });
        c.apply(crate::CortexCmd::AudioTotal { secs: 4.0 });
        c.on_state(crate::OverlayState::AssistantSpeaking);
        let mut saw_bead = false;
        let mut saw_spark = false;
        for _ in 0..100 {
            c.tick_dt(&[], 0.05);
            saw_bead |= !c.beads().is_empty();
            saw_spark |= !c.spark_states().is_empty();
        }
        assert!(saw_bead, "pulses should cross the lattice during replay");
        assert!(saw_spark, "crest sparks should fire as tokens are spoken");
        assert!(c.playback_frac() > 0.9, "clock should near the end of the timeline");
        assert!(c.hud().is_some());
        assert!(c.has_replay());
        // Entropy ribbon source: 1.5 bits normalised, constant across
        // the timeline.
        let e = c.entropy_at(0.5);
        assert!(e > 0.0 && e < 1.0);
    }

    #[test]
    fn playback_done_fast_forwards_the_clock() {
        let mut a = CortexState::default();
        let mut b = CortexState::default();
        for c in [&mut a, &mut b] {
            c.apply(crate::CortexCmd::ReplyBegin { n_layer: 4 });
            c.apply(frame(0, &[1.0; 4], 1.0));
            c.apply(crate::CortexCmd::ReplyEnd {
                total_tokens: 100,
                gen_ms: 1000,
                ctx_used: 0,
                ctx_capacity: 0,
            });
            c.apply(crate::CortexCmd::AudioTotal { secs: 10.0 });
            c.on_state(crate::OverlayState::AssistantSpeaking);
        }
        b.apply(crate::CortexCmd::PlaybackDone);
        for _ in 0..10 {
            a.tick_dt(&[], 0.05);
            b.tick_dt(&[], 0.05);
        }
        assert!(b.clock > a.clock, "drained playback should fast-forward");
    }

    #[test]
    fn reply_begin_resets_previous_replay() {
        let mut c = CortexState::default();
        c.apply(crate::CortexCmd::ReplyBegin { n_layer: 4 });
        c.apply(frame(0, &[1.0; 4], 1.0));
        c.apply(crate::CortexCmd::PlaybackDone);
        c.apply(crate::CortexCmd::ReplyBegin { n_layer: 16 });
        assert!(c.frames.is_empty());
        assert!(!c.playback_done);
        assert_eq!(c.layer_count(), 16);
    }

    #[test]
    fn add_glow_saturates_and_keeps_premultiplied_invariant() {
        const W: u32 = 32;
        const H: u32 = 32;
        let mut buf = vec![0xCC17_171Bu32; (W * H) as usize];
        for _ in 0..8 {
            add_glow(&mut buf, W, H, 16.0, 16.0, 8.0, 0xFFFF_FFFF, 1.0);
        }
        let centre = buf[(16 * W + 16) as usize];
        assert_eq!(centre & 0x00FF_FFFF, 0x00FF_FFFF, "centre saturates to white");
        for &px in &buf {
            let a = (px >> 24) & 0xFF;
            for shift in [16, 8, 0] {
                assert!((px >> shift) & 0xFF <= a, "premultiplied invariant: {px:08X}");
            }
        }
    }

    #[test]
    fn add_glow_zero_radius_or_intensity_is_noop() {
        const W: u32 = 8;
        let mut buf = vec![0u32; 64];
        add_glow(&mut buf, W, 8, 4.0, 4.0, 0.0, 0xFFFF_FFFF, 1.0);
        add_glow(&mut buf, W, 8, 4.0, 4.0, 4.0, 0xFFFF_FFFF, 0.0);
        assert!(buf.iter().all(|&p| p == 0));
    }

    #[test]
    fn cadence_mode_animates_without_keyframes() {
        // External backend (no capture): only the audio duration is
        // known, yet the lattice must still fire word pulses.
        let mut c = CortexState::default();
        c.apply(crate::CortexCmd::ReplyBegin { n_layer: 32 });
        c.apply(crate::CortexCmd::AudioTotal { secs: 5.0 });
        c.on_state(crate::OverlayState::AssistantSpeaking);
        let mut saw_bead = false;
        let mut saw_spark = false;
        for _ in 0..80 {
            c.tick_dt(&[], 0.05);
            saw_bead |= !c.beads().is_empty();
            saw_spark |= !c.spark_states().is_empty();
        }
        assert!(saw_bead, "cadence mode should fire word pulses");
        assert!(saw_spark, "cadence mode should fire crest sparks");
        assert!(c.ember_row.iter().any(|&r| (r as usize) < GRID_ROWS), "paths should leave embers");
    }

    #[test]
    fn path_rows_are_stable_connected_and_in_range() {
        let a = path_rows(42, 47, GRID_ROWS);
        let b = path_rows(42, 47, GRID_ROWS);
        assert_eq!(a, b, "a token's path must be deterministic");
        assert_eq!(a.len(), 48);
        for w in a.windows(2) {
            assert!(w[0].abs_diff(w[1]) <= 1, "path may wander at most one row per column");
        }
        assert!(a.iter().all(|&r| r < GRID_ROWS));
        // Different tokens take different routes (overwhelmingly).
        assert_ne!(path_rows(1, 47, GRID_ROWS), path_rows(2, 47, GRID_ROWS));
    }

    #[test]
    fn draw_cortex_paints_pixels_without_panic() {
        const W: u32 = 320;
        const H: u32 = 80;
        let mut buf = vec![0xCC17_171Bu32; (W * H) as usize];
        let mut c = CortexState::default();
        c.on_state(crate::OverlayState::Recording { db: -20 });
        for _ in 0..5 {
            c.tick(&[0.8; 32]);
        }
        draw_cortex(&mut buf, W, H, &c, 4.0, 316.0, 4.0, 76.0, 0xFF38_BDF8, 1.0, 1.25);
        let painted = buf.iter().filter(|&&p| p != 0xCC17_171B).count();
        assert!(painted > 200, "cortex should paint many pixels, got {painted}");
    }
}
