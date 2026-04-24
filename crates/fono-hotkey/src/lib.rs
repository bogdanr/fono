// SPDX-License-Identifier: GPL-3.0-only
//! Global hotkey registration (via `global-hotkey`) and the
//! Idle/Recording/Processing FSM. Phase 3 of
//! `docs/plans/2026-04-24-fono-design-v1.md`.

pub mod fsm;
pub mod parse;

pub use fsm::{HotkeyAction, HotkeyEvent, RecordingFsm, RecordingMode, State};
pub use parse::{parse_hotkey, ParsedHotkey};
