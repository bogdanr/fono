// SPDX-License-Identifier: GPL-3.0-only
//! Tray-icon integration. Phase 7 Task 7.1.
//!
//! The concrete `tray-icon` backend is feature-gated because the crate pulls
//! a large dependency tree (zbus, dbus, libappindicator). The default build
//! exposes a `TrayState` enum and a no-op `Tray` that callers use; enabling
//! the `tray-backend` feature swaps in the real implementation.

/// FSM-aligned tray state used to tint the icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    Idle,
    Recording,
    Processing,
    Paused,
}

/// User actions fired from the tray menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    ShowStatus,
    Pause,
    OpenHistory,
    OpenConfig,
    Quit,
}

/// Minimal no-op tray used when the backend is compiled out (e.g. headless
/// daemon mode or CI builds).
pub struct Tray {
    state: TrayState,
}

impl Default for Tray {
    fn default() -> Self {
        Self {
            state: TrayState::Idle,
        }
    }
}

impl Tray {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_state(&mut self, state: TrayState) {
        self.state = state;
        tracing::debug!("tray state -> {state:?}");
    }

    #[must_use]
    pub fn state(&self) -> TrayState {
        self.state
    }
}
