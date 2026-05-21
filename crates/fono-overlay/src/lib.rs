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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayState {
    Hidden,
    Recording {
        db: i8,
    },
    /// Voice-assistant recording (F10 hold-to-talk). Same waveform
    /// shapes as [`Self::Recording`], but the renderer uses a green
    /// palette + "Assistant" title so the user can see at a glance
    /// which pipeline they triggered. The orchestrator drives the
    /// same level/sample/FFT push paths.
    AssistantRecording {
        db: i8,
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
    /// Dictation post-release: STT and/or polish is running and is
    /// expected to take long enough (local backends) to warrant a
    /// live animation. Same synthetic-frame contract as
    /// [`Self::AssistantThinking`].
    Polishing,
    /// Live dictation in progress. The text is shown via
    /// [`OverlayHandle::update_text`].
    LiveDictating,
}

impl Default for OverlayState {
    fn default() -> Self {
        Self::Hidden
    }
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
    pub fn set_volume_bar(&self, _enabled: bool) {}
    pub fn set_waveform_style(&self, _style: fono_core::config::WaveformStyle) {}

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

#[cfg(any(feature = "real-window", feature = "backend-x11", feature = "backend-wlr"))]
pub mod renderer;

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
        assert_eq!(BackendId::parse("noop"), Some(BackendId::Noop));
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
        let picks = backend::pick_backend_with(None, |k| k == "WAYLAND_DISPLAY");
        assert_eq!(picks, vec![BackendId::WlrLayerShell, BackendId::Noop]);
    }

    #[test]
    fn selection_uses_x11_when_only_display_set() {
        let picks = backend::pick_backend_with(None, |k| k == "DISPLAY");
        assert_eq!(picks, vec![BackendId::X11OverrideRedirect, BackendId::Noop]);
    }

    #[test]
    fn selection_falls_back_to_noop_with_no_session() {
        let picks = backend::pick_backend_with(None, |_| false);
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
        let picks = backend::pick_backend_with(None, |k| k == "WAYLAND_DISPLAY" || k == "DISPLAY");
        assert_eq!(
            picks,
            vec![BackendId::WlrLayerShell, BackendId::X11OverrideRedirect, BackendId::Noop]
        );
    }
    #[test]
    fn forced_backend_override_short_circuits_selection() {
        // FONO_OVERLAY_BACKEND=noop wins regardless of the env probe.
        let picks = backend::pick_backend_with(Some("noop"), |_| true);
        assert_eq!(picks, vec![BackendId::Noop, BackendId::Noop]);
        let picks = backend::pick_backend_with(Some("wlr"), |_| false);
        assert_eq!(picks, vec![BackendId::WlrLayerShell, BackendId::Noop]);
        // Unknown value falls through to automatic selection.
        let picks = backend::pick_backend_with(Some("garbage"), |_| false);
        assert_eq!(picks, vec![BackendId::Noop]);
    }
}
