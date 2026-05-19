// SPDX-License-Identifier: GPL-3.0-only
//! Global hotkey registration (via `global-hotkey`) and the
//! Idle/Recording/Processing FSM. Phase 3 of
//! `docs/plans/2026-04-24-fono-design-v1.md`.

pub mod detect;
pub mod fsm;
#[cfg(target_os = "linux")]
pub mod gnome_gsettings;
pub mod listener;
pub mod parse;
#[cfg(target_os = "linux")]
pub mod portal;
pub mod xerror;

pub use detect::{
    detect_backend, spawn as spawn_with_backend, HotkeyBackendChoice, ResolvedBackend,
};
pub use fsm::{HotkeyAction, HotkeyEvent, RecordingFsm, RecordingMode, State};
pub use listener::{spawn as spawn_listener, HotkeyBindings, HotkeyControl, ListenerHandle};
pub use parse::{parse_hotkey, ParsedHotkey};

/// Re-export of the `crossbeam-channel` `Sender` carrying [`HotkeyControl`]
/// messages. Lets dependent crates clone and forward control commands
/// without depending on `crossbeam-channel` directly.
pub type HotkeyControlSender = crossbeam_channel::Sender<HotkeyControl>;
