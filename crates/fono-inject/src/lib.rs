// SPDX-License-Identifier: GPL-3.0-only
//! Text injection + focus detection. Phase 6 of
//! `docs/plans/2026-04-24-fono-design-v1.md`.
//!
//! The default backend chain (Linux):
//! 1. `enigo` (X11 via libxdo) — compiled only with the `enigo-backend`
//!    feature to keep the default workspace build free of C system deps.
//! 2. `wtype` (Wayland) — spawned as a subprocess if available.
//! 3. `ydotool` (Wayland root) — spawned as a subprocess if available.
//!
//! Focus detection:
//! - X11 via `x11rb` behind the `x11-focus` feature.
//! - Always returns `None` on Wayland (callers must gracefully degrade).

pub mod focus;
pub mod inject;

pub use focus::{detect_focus, FocusInfo};
pub use inject::{type_text, Injector};
