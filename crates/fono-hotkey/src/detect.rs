// SPDX-License-Identifier: GPL-3.0-only
//! Hotkey backend selection.
//!
//! Picks between the X11 (`global-hotkey`) listener and the
//! Wayland-portal (`xdg-desktop-portal.GlobalShortcuts`) listener at
//! daemon startup, based on the session environment.
//!
//! Auto-detection is the only path users ever hit. The
//! `FONO_HOTKEY_BACKEND={portal,x11,disabled}` env var is a diagnostic
//! escape hatch — unknown values fall through to auto-detection with a
//! warning, matching the `FONO_OVERLAY_BACKEND` selector in
//! `fono-overlay`.

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::info;

use crate::fsm::HotkeyAction;
use crate::listener::{HotkeyBindings, ListenerHandle};

/// Forced backend override, parsed from `FONO_HOTKEY_BACKEND`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyBackend {
    /// `xdg-desktop-portal.GlobalShortcuts` (Wayland sessions).
    Portal,
    /// The X11 / Xwayland `global-hotkey` listener.
    X11,
    /// Skip the listener entirely (headless / SSH / CI runners).
    Disabled,
}

impl HotkeyBackend {
    /// Parse the value of `FONO_HOTKEY_BACKEND` into a forced backend
    /// selection. Returns `None` for empty / unknown input so the
    /// caller falls back to auto-detection.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "portal" => Some(Self::Portal),
            "x11" => Some(Self::X11),
            "disabled" => Some(Self::Disabled),
            _ => None,
        }
    }
}

/// Resolve a backend for the current session. `forced` is the parsed
/// `FONO_HOTKEY_BACKEND` value (`None` = auto-detect).
///
/// Auto-detect matrix:
/// - `WAYLAND_DISPLAY` set → `Portal` (the portal listener falls
///   back gracefully at spawn time if `xdg-desktop-portal-*` isn't
///   running).
/// - `DISPLAY` set, no `WAYLAND_DISPLAY` → `X11`.
/// - Neither set → `Disabled`.
#[must_use]
pub fn detect_backend(forced: Option<HotkeyBackend>) -> HotkeyBackend {
    detect_backend_with(forced, |k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
}

/// Test seam for [`detect_backend`] with an injectable env-lookup.
#[doc(hidden)]
pub fn detect_backend_with(
    forced: Option<HotkeyBackend>,
    env_present: impl Fn(&str) -> bool,
) -> HotkeyBackend {
    if let Some(b) = forced {
        return b;
    }
    if env_present("WAYLAND_DISPLAY") {
        HotkeyBackend::Portal
    } else if env_present("DISPLAY") {
        HotkeyBackend::X11
    } else {
        HotkeyBackend::Disabled
    }
}

/// Top-level orchestrator: detect, dispatch, return a unified handle.
pub fn spawn(
    forced: Option<HotkeyBackend>,
    bindings: HotkeyBindings,
    tx: mpsc::UnboundedSender<HotkeyAction>,
) -> Result<Option<ListenerHandle>> {
    let backend = detect_backend(forced);
    info!("hotkey backend resolved: {backend:?} (forced: {forced:?})");
    match backend {
        HotkeyBackend::Portal => {
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
        HotkeyBackend::X11 => crate::listener::spawn(bindings, tx).map(Some),
        HotkeyBackend::Disabled => {
            info!("hotkey listener disabled (headless session)");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_canonical_names() {
        assert_eq!(HotkeyBackend::parse("portal"), Some(HotkeyBackend::Portal));
        assert_eq!(HotkeyBackend::parse("Portal"), Some(HotkeyBackend::Portal));
        assert_eq!(HotkeyBackend::parse("x11"), Some(HotkeyBackend::X11));
        assert_eq!(HotkeyBackend::parse("disabled"), Some(HotkeyBackend::Disabled));
    }

    #[test]
    fn parse_unknown_returns_none() {
        assert_eq!(HotkeyBackend::parse(""), None);
        assert_eq!(HotkeyBackend::parse("auto"), None);
        assert_eq!(HotkeyBackend::parse("wayland"), None);
        assert_eq!(HotkeyBackend::parse("bogus"), None);
    }

    #[test]
    fn auto_detect_picks_portal_on_wayland() {
        let b = detect_backend_with(None, |k| k == "WAYLAND_DISPLAY");
        assert_eq!(b, HotkeyBackend::Portal);
    }

    #[test]
    fn auto_detect_picks_x11_on_xorg() {
        let b = detect_backend_with(None, |k| k == "DISPLAY");
        assert_eq!(b, HotkeyBackend::X11);
    }

    #[test]
    fn auto_detect_disabled_when_headless() {
        let b = detect_backend_with(None, |_| false);
        assert_eq!(b, HotkeyBackend::Disabled);
    }

    #[test]
    fn forced_override_wins() {
        // Forced X11 wins even with WAYLAND_DISPLAY set.
        let b = detect_backend_with(Some(HotkeyBackend::X11), |_| true);
        assert_eq!(b, HotkeyBackend::X11);
    }
}
