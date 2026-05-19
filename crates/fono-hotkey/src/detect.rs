// SPDX-License-Identifier: GPL-3.0-only
//! Hotkey backend selection.
//!
//! Picks between the X11 (`global-hotkey`) listener and the
//! Wayland-portal (`xdg-desktop-portal.GlobalShortcuts`) listener at
//! daemon startup, based on the session environment.

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::info;

use crate::fsm::HotkeyAction;
use crate::listener::{HotkeyBindings, ListenerHandle};

/// User-facing backend preference. Read from `[hotkeys].backend` in
/// `config.toml`; defaults to [`Self::Auto`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HotkeyBackendChoice {
    /// Auto-detect: portal on Wayland (if available), X11 otherwise.
    #[default]
    Auto,
    /// Force the portal backend (Wayland-only; errors elsewhere).
    Portal,
    /// Force the legacy X11 / Xwayland `global-hotkey` listener.
    X11,
    /// Skip the hotkey listener entirely. Useful for headless / SSH
    /// sessions and CI runners.
    Disabled,
}

impl HotkeyBackendChoice {
    /// Parse from config string. Tolerant of case and minor spellings.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "auto" | "default" => Self::Auto,
            "portal" | "wayland" | "wayland-portal" | "xdp" => Self::Portal,
            "x11" | "xorg" | "xwayland" | "legacy" => Self::X11,
            "disabled" | "off" | "none" => Self::Disabled,
            other => {
                tracing::warn!("unknown [hotkeys].backend = {other:?}; falling back to auto");
                Self::Auto
            }
        }
    }
}

/// Resolved backend after auto-detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedBackend {
    Portal,
    X11,
    Disabled,
}

/// Detect the best backend for the current session.
///
/// Decision matrix:
/// - `WAYLAND_DISPLAY` set → `Portal` (the portal listener will itself
///   fall back gracefully if `xdg-desktop-portal-*` is not running).
/// - `DISPLAY` set, no `WAYLAND_DISPLAY` → `X11`.
/// - Neither set → `Disabled`.
#[must_use]
pub fn detect_backend(choice: HotkeyBackendChoice) -> ResolvedBackend {
    match choice {
        HotkeyBackendChoice::Portal => ResolvedBackend::Portal,
        HotkeyBackendChoice::X11 => ResolvedBackend::X11,
        HotkeyBackendChoice::Disabled => ResolvedBackend::Disabled,
        HotkeyBackendChoice::Auto => {
            let has_wayland =
                std::env::var_os("WAYLAND_DISPLAY").map(|v| !v.is_empty()).unwrap_or(false);
            let has_x11 = std::env::var_os("DISPLAY").map(|v| !v.is_empty()).unwrap_or(false);
            if has_wayland {
                ResolvedBackend::Portal
            } else if has_x11 {
                ResolvedBackend::X11
            } else {
                ResolvedBackend::Disabled
            }
        }
    }
}

/// Top-level orchestrator: detect, dispatch, return a unified handle.
pub fn spawn(
    choice: HotkeyBackendChoice,
    bindings: HotkeyBindings,
    tx: mpsc::UnboundedSender<HotkeyAction>,
) -> Result<Option<ListenerHandle>> {
    let backend = detect_backend(choice);
    info!("hotkey backend resolved: {backend:?} (choice: {choice:?})");
    match backend {
        ResolvedBackend::Portal => {
            #[cfg(target_os = "linux")]
            {
                match crate::portal::spawn(bindings.clone(), tx.clone()) {
                    Ok(h) => Ok(Some(h)),
                    Err(e) => {
                        tracing::warn!("portal hotkey backend unavailable: {e:#}");
                        // GNOME-Wayland 46 (Ubuntu 24.04) ships
                        // xdg-desktop-portal-gnome without
                        // GlobalShortcuts. Fall back to gsettings
                        // custom-keybindings — these route F7 / F8 to
                        // the Fono CLI which talks to this daemon via
                        // IPC. No long-press, but it works today.
                        if crate::gnome_gsettings::is_gnome_session() {
                            match crate::gnome_gsettings::install(
                                &bindings.dictation,
                                &bindings.assistant,
                            ) {
                                Ok(exe) => {
                                    tracing::info!(
                                        "GNOME-Wayland fallback: registered gsettings \
                                         custom-keybindings {} → {} toggle, {} → {} assistant",
                                        bindings.dictation,
                                        exe.display(),
                                        bindings.assistant,
                                        exe.display()
                                    );
                                    return Ok(None);
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "GNOME gsettings fallback also failed: {e:#}; \
                                         trying X11 listener"
                                    );
                                }
                            }
                        }
                        tracing::warn!("falling back to X11 listener (Xwayland-only events)");
                        crate::listener::spawn(bindings, tx).map(Some)
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (bindings, tx);
                anyhow::bail!("portal hotkey backend is Linux-only");
            }
        }
        ResolvedBackend::X11 => crate::listener::spawn(bindings, tx).map(Some),
        ResolvedBackend::Disabled => {
            info!("hotkey listener disabled (headless session)");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_parse_aliases() {
        assert_eq!(HotkeyBackendChoice::parse(""), HotkeyBackendChoice::Auto);
        assert_eq!(HotkeyBackendChoice::parse("auto"), HotkeyBackendChoice::Auto);
        assert_eq!(HotkeyBackendChoice::parse("Portal"), HotkeyBackendChoice::Portal);
        assert_eq!(HotkeyBackendChoice::parse("wayland"), HotkeyBackendChoice::Portal);
        assert_eq!(HotkeyBackendChoice::parse("X11"), HotkeyBackendChoice::X11);
        assert_eq!(HotkeyBackendChoice::parse("disabled"), HotkeyBackendChoice::Disabled);
        assert_eq!(HotkeyBackendChoice::parse("bogus"), HotkeyBackendChoice::Auto);
    }
}
