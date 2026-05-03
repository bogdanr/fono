// SPDX-License-Identifier: GPL-3.0-only
//! Recording-indicator and live-dictation overlay.
//!
//! The default build exposes a no-op `Overlay` so the orchestrator
//! compiles everywhere. With the `real-window` cargo feature the
//! overlay spawns a background thread that drives a `winit` event loop
//! + `softbuffer` framebuffer, drawing the current state and (for live
//!   dictation) the latest preview/finalize text. Plan R5.
//!
//! Slice A keeps the overlay in-process (winit thread). The subprocess
//! refactor that v6 plan R5.6 wants is deferred to Slice B; rationale
//! captured in `docs/decisions/0009-interactive-slice-simplifications.md`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayState {
    Hidden,
    Recording {
        db: i8,
    },
    Processing,
    /// Live dictation in progress. The text is shown via
    /// [`OverlayHandle::update_text`].
    LiveDictating,
}

/// Compile-time-stub overlay used when no backend is enabled. Tracks
/// state and text in memory so callers always have a usable handle even
/// in slim builds.
#[derive(Debug, Default)]
pub struct Overlay {
    state: OverlayState,
    text: String,
}

impl Default for OverlayState {
    fn default() -> Self {
        Self::Hidden
    }
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

    /// No-op stub — present so callers compile in non-`real-window`
    /// builds (server / headless).
    pub fn push_level(&self, _amplitude: f32) {}

    /// No-op stub — present so callers compile in non-`real-window`
    /// builds (server / headless).
    pub fn push_samples(&self, _samples: Vec<f32>) {}

    /// No-op stub — present so callers compile in non-`real-window`
    /// builds (server / headless).
    pub fn push_fft_bins(&self, _bins: Vec<f32>) {}

    /// No-op stub — present so callers compile in non-`real-window`
    /// builds (server / headless).
    pub fn set_volume_bar(&self, _enabled: bool) {}

    #[must_use]
    pub fn state(&self) -> OverlayState {
        self.state
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

#[cfg(feature = "real-window")]
pub mod real;

#[cfg(feature = "real-window")]
pub use real::{OverlayHandle, RealOverlay};

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
}
