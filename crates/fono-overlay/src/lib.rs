// SPDX-License-Identifier: GPL-3.0-only
//! Recording-indicator overlay. Phase 7 Task 7.2.
//!
//! The `winit`+`softbuffer` implementation is feature-gated; the default
//! build exposes a no-op `Overlay` so the orchestrator compiles everywhere.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayState {
    Hidden,
    Recording { db: i8 },
    Processing,
}

pub struct Overlay {
    state: OverlayState,
}

impl Default for Overlay {
    fn default() -> Self {
        Self {
            state: OverlayState::Hidden,
        }
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

    #[must_use]
    pub fn state(&self) -> OverlayState {
        self.state
    }
}
