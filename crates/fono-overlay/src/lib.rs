// SPDX-License-Identifier: GPL-3.0-only
//! Recording-indicator and live-dictation overlay.
//!
//! ## Architecture (2026-05-19 split)
//!
//! The overlay is composed of two cleanly separated layers:
//!
//! - [`renderer`] — pure software-rasterised drawing into an ARGB
//!   premultiplied `&mut [u32]` framebuffer. No `winit`, no
//!   `softbuffer`, no `wayland-client`. Unit-testable.
//! - [`backend`] — `OverlayBackend` trait + three implementations
//!   under [`backends`]: `wlr-layer-shell` (primary Wayland),
//!   `x11-override-redirect` (the original winit + softbuffer path;
//!   used on native X11 and via Xwayland on Wayland sessions where
//!   layer-shell isn't available, e.g. GNOME), and `noop` (terminal
//!   fallback).
//!
//! The runtime backend selection in [`backend::spawn_overlay`] reads
//! `WAYLAND_DISPLAY` / `DISPLAY` / `FONO_OVERLAY_BACKEND` and walks
//! a candidate list until one backend's `try_spawn` succeeds. The
//! `noop` backend is the terminal sink so the daemon never aborts on
//! a missing graphics environment.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OverlayState {
    #[default]
    Hidden,
    Recording {
        db: i8,
    },
    /// Dictation recording paused: user has stopped speaking long
    /// enough that the silence-watch state machine flipped from
    /// `Speaking` to `Pondering`. Same waveform shape as
    /// [`Self::Recording`] but the renderer paints the status label
    /// as "Pondering…" with a walking-letter highlight whose
    /// position is driven by `walk_progress` (0..=10_000, fixed-point
    /// `0.0..=1.0` mapped over `auto_stop_silence_ms` — or a default
    /// visual window when auto-stop is off). Slice 2 of
    /// `plans/2026-05-22-fono-auto-stop-silence-v1.md`: visual only,
    /// no auto-stop commit yet.
    Pondering {
        db: i8,
        walk_progress: u16,
    },
    /// Voice-assistant recording (F8 hold-to-talk). Same waveform
    /// shapes as [`Self::Recording`], but the renderer uses a green
    /// palette + "Assistant" title so the user can see at a glance
    /// which pipeline they triggered. The orchestrator drives the
    /// same level/sample/FFT push paths.
    AssistantRecording {
        db: i8,
    },
    /// Voice-assistant recording paused: mirrors [`Self::Pondering`]
    /// for the assistant pipeline (F8 toggle). Renderer keeps the
    /// green assistant palette + waveform shape but swaps the label
    /// to "PONDERING" with the same walking-letter highlight driven
    /// by `walk_progress` (0..=10_000). See
    /// `plans/2026-05-22-assistant-pondering-parity-v1.md`.
    AssistantPondering {
        db: i8,
        walk_progress: u16,
    },
    /// Voice-assistant post-release: STT + LLM streaming + first
    /// TTS synthesis. The orchestrator pushes synthetic
    /// time-evolving frames at 20 fps; each waveform style gets a
    /// hand-tuned animation (FFT bell sweep, heatmap intersecting
    /// paths, oscilloscope standing wave, centre-symmetric bars).
    /// Renderer paints with an amber palette + "THINKING" title so
    /// the user can tell apart from real-audio recording at a
    /// glance.
    AssistantThinking,
    /// Voice-assistant has started receiving LLM tokens but the
    /// first TTS audio chunk hasn't reached the playback queue yet
    /// — i.e. the model is generating, the [`SentenceSplitter`] is
    /// buffering until a full sentence emerges, and the TTS HTTP
    /// roundtrip is in flight. The user hears silence during this
    /// stretch, so we lump it in with "thinking" visually (same
    /// amber palette, same synthetic animation) and only swap the
    /// label to "SYNTHESISING" so it's still distinguishable in
    /// logs / screenshots / bug reports. The FSM stays in
    /// `AssistantThinking` for this phase; only the overlay flips.
    AssistantSynthesising,
    /// Voice-assistant TTS audio is actually playing back. Visually
    /// the same shape as [`Self::AssistantThinking`] /
    /// [`Self::AssistantSynthesising`] but with a sky-blue palette
    /// and a "SPEAKING" title so the user sees the pipeline has
    /// moved on from "preparing the reply" to "saying the reply".
    /// Driven by the orchestrator the moment the first synthesised
    /// audio chunk is enqueued, in lockstep with the FSM
    /// `AssistantThinking → AssistantSpeaking` transition.
    AssistantSpeaking,
    Processing,
    /// Dictation post-release: the batch STT and optional LLM cleanup
    /// pipeline is running and is expected to take long enough (local
    /// backends) to warrant live feedback. The visible label stays
    /// `"POLISHING"`; `phase` only controls the walking-letter
    /// direction so STT reads left-to-right and cleanup reads
    /// right-to-left without adding more text to the panel.
    Polishing {
        phase: PolishingPhase,
        walk_progress: u16,
    },
    /// Live dictation in progress. The text is shown via
    /// [`OverlayHandle::update_text`].
    LiveDictating,
    /// MCP `fono.listen` relevance gate dropped the previous
    /// utterance. The panel flashes this state for ~700 ms after
    /// each rejection so the user gets a discriminable visual ack
    /// ("Fono heard you but is still waiting for a real answer")
    /// before reverting to [`Self::Recording`] for the next
    /// utterance attempt. Slice 5 of
    /// `plans/2026-05-26-mcp-listen-overlay-and-silence-parity-v7.md`.
    ///
    /// Renderer contract: neutral-grey accent, label `"IGNORED"`,
    /// VU bar hidden (we're not metering anything — the mic is
    /// being re-armed). The `reason` is plumbed through so future
    /// iterations can surface sub-labels without another enum
    /// migration.
    Ignoring {
        reason: IgnoreReason,
    },
}

/// Which internal post-recording step the dictation polishing overlay
/// is visualising. The renderer deliberately keeps the public label as
/// `"POLISHING"`; this discriminator only changes the highlight walk
/// direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolishingPhase {
    /// Batch STT is turning recorded audio into raw text. The label
    /// highlight walks left-to-right.
    Transcribing,
    /// The optional LLM cleanup step is editing the raw transcript.
    /// The label highlight walks right-to-left.
    Cleanup,
}

/// Why the MCP relevance gate ignored an utterance, surfaced in the
/// overlay's `Ignoring` flash and in debug logs. Mirrors (but is
/// intentionally not the same type as) `fono_mcp_server::relevance::
/// IgnoreReason` — the overlay crate must not depend on the MCP
/// server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoreReason {
    /// Looked like radio / TV / side conversation. Either the LLM
    /// classifier returned `BACKGROUND` or a future on-device
    /// classifier flagged the utterance as off-topic.
    BackgroundSpeech,
    /// Heuristic dropped the utterance as too short / filler-only /
    /// otherwise low-information. Kept distinct from
    /// `BackgroundSpeech` so future visual treatments can
    /// differentiate (e.g. a smaller flash for filler vs a larger
    /// one for a TV news anchor mid-sentence).
    LowConfidence,
    /// Transcript matched the agent's prompt closely enough that we
    /// assume AEC didn't fully cancel the TTS playback.
    EchoFromPrompt,
}

/// One sampled token's forward-pass keyframe, as consumed by the
/// Glass Cortex replay engine. A plain-data mirror of
/// `fono_core::brain_tap::BrainKeyframe` so the overlay crate never
/// depends on the llama feature stack — the orchestrator converts.
#[derive(Debug, Clone, Default)]
pub struct CortexFrame {
    /// Index of the token within its generation (0-based).
    pub token_index: u64,
    /// L2 norm of each layer's output hidden state, indexed by layer;
    /// `0.0` where the layer was not observed in this keyframe (the
    /// capture strides layers — the replay engine merges).
    pub layer_norms: Vec<f32>,
    /// MoE router choices per layer; empty on dense models. Carried
    /// through now so the Phase 3 honeycomb work is pure rendering.
    pub experts: Vec<CortexExperts>,
    /// Probability of the sampled token (model confidence).
    pub token_prob: Option<f32>,
    /// Shannon entropy of the token distribution, in bits.
    pub entropy_bits: Option<f32>,
}

/// Routed experts for one layer of one sampled token (MoE models).
#[derive(Debug, Clone)]
pub struct CortexExperts {
    pub layer: u32,
    pub ids: Vec<i32>,
    pub weights: Vec<f32>,
}

/// Glass Cortex replay commands pushed by the orchestrator alongside
/// the regular overlay state/audio commands. The renderer's replay
/// engine (see `cortex` module) turns the generation-burst keyframes
/// into an animation paced to TTS playback.
#[derive(Debug, Clone)]
pub enum CortexCmd {
    /// A local LLM generation started (assistant reply or polish
    /// cleanup). Resets the replay buffers; `n_layer` sizes the spine.
    ReplyBegin { n_layer: u32 },
    /// One prompt-prefill batch (`n_tokens` wide) finished decoding.
    /// Fires a left→right sweep pulse along the spine — the prompt
    /// visibly flowing through the layers during the thinking phase.
    Prefill { n_tokens: u32 },
    /// One captured keyframe (arrives during the generation burst).
    Frame(CortexFrame),
    /// Generation finished: token count + decode wall-clock (for the
    /// tok/s HUD) and KV-cache fill (for the context arc).
    ReplyEnd { total_tokens: u64, gen_ms: u64, ctx_used: u32, ctx_capacity: u32 },
    /// Cumulative seconds of reply audio enqueued for playback so far
    /// (monotonic within a turn). Sizes the replay timeline; absent
    /// (e.g. streaming TTS) the engine estimates from token count.
    AudioTotal { secs: f32 },
    /// A short window of the **real** reply audio's spectrum, tagged
    /// with its position (`at_secs`) on the cumulative reply-audio
    /// timeline. Computed cheaply (a small band split + RMS) from the
    /// actual synthesised TTS PCM — not a synthetic field — so the
    /// speaking scene can modulate the grid by the genuinely spoken
    /// voice. `bands` are low→high frequency energies (0..1); `amp`
    /// is the window RMS (0..1). The replay engine samples this
    /// timeline against its playback clock, so the modulation tracks
    /// the voice even though the samples are pushed at enqueue time.
    AudioBands { at_secs: f32, bands: Vec<f32>, amp: f32 },
    /// Reply audio finished playing (or the turn was cancelled).
    PlaybackDone,
}

/// Compile-time-stub overlay used in tests + by callers that need an
/// owned `Overlay` without spawning a real backend. Tracks state and
/// text in memory so callers always have a usable handle.
#[derive(Debug, Default)]
pub struct Overlay {
    state: OverlayState,
    text: String,
}

impl Overlay {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_state(&mut self, state: OverlayState) {
        self.state = state;
        tracing::trace!("overlay state -> {state:?}");
    }

    pub fn update_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        tracing::trace!("overlay text -> {} chars", self.text.len());
    }

    pub fn push_level(&self, _amplitude: f32) {}
    pub fn push_samples(&self, _samples: Vec<f32>) {}
    pub fn push_fft_bins(&self, _bins: Vec<f32>) {}
    pub fn set_volume_bar(&self, _mode: fono_core::config::VolumeBarMode) {}
    pub fn push_gate_metrics(&self, _inst: f32, _voiced: f32, _silence: f32) {}
    pub fn set_waveform_style(&self, _style: fono_core::config::WaveformStyle) {}
    pub fn push_cortex(&self, _cmd: CortexCmd) {}

    #[must_use]
    pub fn state(&self) -> OverlayState {
        self.state
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

// Renderer + backends live behind `real-window` (which transitively
// gates ab_glyph + the backend impls). The `backend` module surface
// is always compiled so the noop backend + selection logic are
// available even in the slim build.

#[cfg(any(
    feature = "real-window",
    feature = "backend-x11",
    feature = "backend-wlr",
    feature = "backend-macos"
))]
pub mod renderer;

#[cfg(any(
    feature = "real-window",
    feature = "backend-x11",
    feature = "backend-wlr",
    feature = "backend-macos"
))]
pub mod r3d;

#[cfg(any(
    feature = "real-window",
    feature = "backend-x11",
    feature = "backend-wlr",
    feature = "backend-macos"
))]
pub mod cortex;

pub mod backend;
pub mod backends;

pub use backend::{spawn_overlay, BackendCapabilities, BackendId, OverlayHandle};

/// Public marker kept for source compatibility with pre-split call
/// sites. `RealOverlay::spawn` is a thin wrapper over
/// [`backend::spawn_overlay`].
#[cfg(feature = "real-window")]
pub struct RealOverlay;

#[cfg(feature = "real-window")]
impl RealOverlay {
    /// Spawn the overlay using the best backend available in the
    /// current session. Always returns `Ok` in practice — the noop
    /// backend is a terminal fallback.
    pub fn spawn(style: fono_core::config::WaveformStyle) -> std::io::Result<OverlayHandle> {
        backend::spawn_overlay(style)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_tracks_state_and_text() {
        let mut o = Overlay::new();
        assert_eq!(o.state(), OverlayState::Hidden);
        o.set_state(OverlayState::LiveDictating);
        o.update_text("hello world");
        assert_eq!(o.state(), OverlayState::LiveDictating);
        assert_eq!(o.text(), "hello world");
    }

    #[test]
    fn live_dictating_state_is_distinct() {
        let mut o = Overlay::new();
        o.set_state(OverlayState::Recording { db: -20 });
        o.set_state(OverlayState::LiveDictating);
        assert_eq!(o.state(), OverlayState::LiveDictating);
    }

    #[test]
    fn backend_id_parses_known_overrides() {
        assert_eq!(BackendId::parse("wlr"), Some(BackendId::WlrLayerShell));
        assert_eq!(BackendId::parse("x11"), Some(BackendId::X11OverrideRedirect));
        assert_eq!(BackendId::parse("mac"), Some(BackendId::MacPanel));
        assert_eq!(BackendId::parse("nspanel"), Some(BackendId::MacPanel));
        assert_eq!(BackendId::parse("win32"), Some(BackendId::Win32LayeredToolWindow));
        assert_eq!(BackendId::parse("windows"), Some(BackendId::Win32LayeredToolWindow));
        assert_eq!(BackendId::parse("noop"), Some(BackendId::Noop));
        // Surrounding whitespace is tolerated (cmd.exe `set VAR=win32 `
        // easily captures a trailing space) and matching is
        // case-insensitive.
        assert_eq!(BackendId::parse("  win32  "), Some(BackendId::Win32LayeredToolWindow));
        assert_eq!(BackendId::parse("WIN32"), Some(BackendId::Win32LayeredToolWindow));
        assert_eq!(BackendId::parse("not-a-backend"), None);
        // The retired wayland-xdg backend's old aliases now fall
        // through to automatic selection, with a warning logged at
        // runtime.
        assert_eq!(BackendId::parse("xdg"), None);
        assert_eq!(BackendId::parse("wayland-xdg"), None);
    }

    // Selection-list invariants. The candidate list is a *preference
    // order*; actual protocol availability is decided by each backend's
    // `try_spawn` at runtime. The GNOME-Wayland fall-through to
    // Xwayland (X11 override-redirect) is driven by
    // `zwlr_layer_shell_v1` not being advertised — not modelled here,
    // since we only mock env-var presence.

    #[test]
    fn selection_wayland_only_falls_through_to_noop() {
        // No DISPLAY — Xwayland disabled. Layer-shell first, then
        // noop. The xdg_toplevel fallback was retired (2026-05-20)
        // because it could not deliver a usable panel UX on Mutter.
        let picks =
            backend::pick_backend_with(None, backend::HostOs::Linux, |k| k == "WAYLAND_DISPLAY");
        assert_eq!(picks, vec![BackendId::WlrLayerShell, BackendId::Noop]);
    }

    #[test]
    fn selection_uses_x11_when_only_display_set() {
        let picks = backend::pick_backend_with(None, backend::HostOs::Linux, |k| k == "DISPLAY");
        assert_eq!(picks, vec![BackendId::X11OverrideRedirect, BackendId::Noop]);
    }

    #[test]
    fn selection_falls_back_to_noop_with_no_session() {
        let picks = backend::pick_backend_with(None, backend::HostOs::Linux, |_| false);
        assert_eq!(picks, vec![BackendId::Noop]);
    }

    #[test]
    fn selection_prefers_xwayland_when_layer_shell_unavailable() {
        // The GNOME / Ubuntu 24.04 default: Wayland session with
        // Xwayland present. We try layer-shell first (correct on
        // sway / KDE / etc.); on GNOME it fails at runtime and we
        // fall through to the X11 override-redirect backend running
        // under Xwayland — which Mutter honours (client-positioned,
        // on-top, not in Alt+Tab).
        let picks = backend::pick_backend_with(None, backend::HostOs::Linux, |k| {
            k == "WAYLAND_DISPLAY" || k == "DISPLAY"
        });
        assert_eq!(
            picks,
            vec![BackendId::WlrLayerShell, BackendId::X11OverrideRedirect, BackendId::Noop]
        );
    }

    #[test]
    fn selection_macos_offers_panel_then_noop() {
        // macOS has exactly one display server, so env vars carry no
        // signal — the table is fixed and viability is decided by the
        // panel backend's own pump check at spawn time. A stray
        // DISPLAY (XQuartz installed) must not change the table.
        let picks = backend::pick_backend_with(None, backend::HostOs::MacOs, |_| false);
        assert_eq!(picks, vec![BackendId::MacPanel, BackendId::Noop]);
        let picks = backend::pick_backend_with(None, backend::HostOs::MacOs, |k| k == "DISPLAY");
        assert_eq!(picks, vec![BackendId::MacPanel, BackendId::Noop]);
    }

    #[test]
    fn selection_other_os_is_noop_only() {
        let picks = backend::pick_backend_with(None, backend::HostOs::Other, |_| true);
        assert_eq!(picks, vec![BackendId::Noop]);
    }

    #[test]
    fn selection_windows_offers_layered_toolwindow_then_noop() {
        // Windows, like macOS, has one display server — env vars carry
        // no signal. The table is fixed; viability is decided by the
        // backend's own window-creation check at spawn time (a
        // non-interactive session, e.g. a service, fails cleanly to
        // noop).
        let picks = backend::pick_backend_with(None, backend::HostOs::Windows, |_| false);
        assert_eq!(picks, vec![BackendId::Win32LayeredToolWindow, BackendId::Noop]);
        let picks = backend::pick_backend_with(None, backend::HostOs::Windows, |k| k == "DISPLAY");
        assert_eq!(picks, vec![BackendId::Win32LayeredToolWindow, BackendId::Noop]);
    }

    #[test]
    fn forced_backend_override_short_circuits_selection() {
        // FONO_OVERLAY_BACKEND=noop wins regardless of the env probe.
        let picks = backend::pick_backend_with(Some("noop"), backend::HostOs::Linux, |_| true);
        assert_eq!(picks, vec![BackendId::Noop, BackendId::Noop]);
        let picks = backend::pick_backend_with(Some("wlr"), backend::HostOs::Linux, |_| false);
        assert_eq!(picks, vec![BackendId::WlrLayerShell, BackendId::Noop]);
        // FONO_OVERLAY_BACKEND=noop works on macOS too (kill switch).
        let picks = backend::pick_backend_with(Some("noop"), backend::HostOs::MacOs, |_| false);
        assert_eq!(picks, vec![BackendId::Noop, BackendId::Noop]);
        // Unknown value falls through to automatic selection.
        let picks = backend::pick_backend_with(Some("garbage"), backend::HostOs::Linux, |_| false);
        assert_eq!(picks, vec![BackendId::Noop]);
    }
}
