// SPDX-License-Identifier: GPL-3.0-only
//! Global hotkey registration (via `global-hotkey`) and the
//! Idle/Recording/Processing FSM. Phase 3 of
//! `docs/plans/2026-04-24-fono-design-v1.md`.
//!
//! # Cross-platform layout (Windows port plan Task 1.4)
//!
//! This crate is already split correctly for new OS ports:
//! - [`listener`], [`fsm`], [`parse`], and [`detect`] are OS-agnostic;
//!   the `global-hotkey` crate provides the per-OS registration
//!   backend (X11 `XGrabKey` on Linux, Carbon `RegisterEventHotKey`
//!   on macOS, Win32 `RegisterHotKey` on Windows).
//! - [`portal`] (`org.freedesktop.portal.GlobalShortcuts`) and
//!   [`gnome_gsettings`] are Linux-only desktop integrations, gated
//!   `#[cfg(target_os = "linux")]` below.
//!
//! A Windows port therefore needs no trait split here — only the
//! `detect::detect_backend` probe learns a new arm (port plan
//! Phase 8).

pub mod detect;
pub mod fsm;
#[cfg(target_os = "linux")]
pub mod gnome_gsettings;
pub mod listener;
pub mod parse;
#[cfg(target_os = "linux")]
pub mod portal;
pub mod xerror;

pub use detect::{detect_backend, spawn as spawn_with_backend, HotkeyBackend};
pub use fsm::{HotkeyAction, HotkeyEvent, RecordingFsm, RecordingMode, State};
pub use listener::{spawn as spawn_listener, HotkeyBindings, HotkeyControl, ListenerHandle};
pub use parse::{parse_hotkey, ParsedHotkey};

/// Re-export of the `crossbeam-channel` `Sender` carrying [`HotkeyControl`]
/// messages. Lets dependent crates clone and forward control commands
/// without depending on `crossbeam-channel` directly.
pub type HotkeyControlSender = crossbeam_channel::Sender<HotkeyControl>;

/// Per-role "is the key physically held down right now?" flags, shared
/// between the hotkey listener (writer) and the orchestrator's
/// silence-watch task (reader).
///
/// The listener emits `TogglePressed` / `AssistantPressed` on every
/// press and synthesises a second toggle on long-release — so by the
/// time the orchestrator sees the action the hold-vs-toggle decision
/// has already been collapsed to `RecordingMode::Toggle`. These flags
/// recover the missing distinction at the audio-decision layer: while
/// the relevant flag is `true`, the silence watchdog skips the
/// `Recording → Pondering` overlay flip and any auto-stop commit, so
/// holding the key down keeps the recording in plain `RECORDING`
/// regardless of pauses.
///
/// Cloning is cheap (two `Arc<AtomicBool>` bumps); the orchestrator
/// holds clones for the lifetime of the daemon and the listener
/// thread keeps its own clones for writes.
#[derive(Debug, Clone, Default)]
pub struct KeyHeldFlags {
    /// True while the dictation key is physically held down.
    pub dictation: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// True while the assistant key is physically held down.
    pub assistant: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// True when a **tap** (short press) of the assistant key should
    /// enter/leave full-duplex live mode rather than fall through to
    /// the legacy toggle behaviour. The orchestrator writes this on
    /// startup/reload to mirror "a realtime assistant model is loaded
    /// **and** `[assistant.realtime].live_mode = true`"; the listener
    /// reads it on a tap-release to decide whether to emit
    /// [`HotkeyAction::AssistantTapped`]. Shared (writer = orchestrator,
    /// reader = listener thread) so a `fono use` / reload that swaps the
    /// backend takes effect without re-spawning the listener.
    pub assistant_live_available: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl KeyHeldFlags {
    /// All flags cleared. Equivalent to `Default::default()`; provided
    /// for call-site readability where the intent is "build a pair of
    /// fresh shared flags".
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}
