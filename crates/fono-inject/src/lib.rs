// SPDX-License-Identifier: GPL-3.0-only
//! Text injection + focus detection. Phase 6 of
//! `docs/plans/2026-04-24-fono-design-v1.md`.
//!
//! The default backend chain (Linux):
//! 1. `enigo` (X11 via libxdo) — compiled only with the `enigo-backend`
//!    feature to keep the default workspace build free of C system deps.
//! 2. `wtype` (Wayland) — spawned as a subprocess if available.
//! 3. `ydotool` (Wayland root) — spawned as a subprocess if available.
//! 4. `xdotool` (X11 / XWayland) — spawned as a subprocess if available.
//! 5. `xtest-type` — pure-Rust XTEST per-character typing, the
//!    universal X11 fallback when none of the above are installed.
//!
//! Focus detection:
//! - X11 via `x11rb` behind the `x11-focus` feature.
//! - Always returns `None` on Wayland (callers must gracefully degrade).

pub mod classifier;
pub mod clipboard_probe;
pub mod focus;
pub mod inject;
pub mod permissions;
pub mod terminal;
#[cfg(target_os = "linux")]
pub mod wayland_probe;
#[cfg(feature = "x11-paste")]
pub mod xtest_type;

pub use classifier::{
    BuiltinRule, CodingAgentKind, ContextClassifier, ContextProfile, ProjectKind, TerminalContext,
    BUILTIN_RULES,
};
pub use clipboard_probe::{detect as detect_clipboard_manager, ClipboardManager};
pub use focus::{detect_focus, FocusInfo};
pub use inject::{
    copy_to_clipboard, copy_to_clipboard_all, type_text, type_text_with_outcome, warm_backend,
    ClipboardAttempt, InjectOutcome, Injector,
};
pub use permissions::{accessibility_prompt, accessibility_trusted, ACCESSIBILITY_SETTINGS_URL};
pub use terminal::{proc_enrichment_available, terminal_context};
#[cfg(target_os = "linux")]
pub use wayland_probe::compositor_supports_virtual_keyboard;
#[cfg(feature = "x11-paste")]
pub use xtest_type::type_via_xtest;
