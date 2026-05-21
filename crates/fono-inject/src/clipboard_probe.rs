// SPDX-License-Identifier: GPL-3.0-only
//! Probe for an active clipboard manager on the local session.
//!
//! Two detection paths, in order:
//!
//! 1. **ICCCM strict managers** — own the `CLIPBOARD_MANAGER` X11
//!    selection so cooperating apps can hand off contents via
//!    `SAVE_TARGETS` on exit. The bullet-proof signal.
//! 2. **Polling managers** — watch the `CLIPBOARD` selection via
//!    `XFixes` or periodic polling and save a history, but do *not*
//!    claim `CLIPBOARD_MANAGER`. clipit, xfce4-clipman, copyq's
//!    default mode, parcellite default, greenclip, clipmenu, diodon
//!    all fall in this bucket. We detect these by scanning `/proc`
//!    for known executable names, which is the only honest signal
//!    short of asking the user.

use std::path::Path;

/// Names commonly used by the executables of clipboard managers we
/// recognise. Matched case-insensitively against `/proc/<pid>/comm`,
/// which is truncated to 15 bytes — long names like `xfce4-clipman`
/// appear as `xfce4-clipman` (13 chars, fits) and `gpaste-daemon`
/// (13 chars, fits); anything ≥16 chars would need a different probe.
const KNOWN_MANAGER_BINS: &[&str] = &[
    "clipit",
    "parcellite",
    "xfce4-clipman",
    "klipper",
    "copyq",
    "gpaste-daemon",
    "greenclip",
    "clipmenu",
    "clipmenud",
    "diodon",
    "cliphist",
    "wl-clip-persist",
    "clipcatd",
];

/// Detection result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardManager {
    /// Owns the ICCCM `CLIPBOARD_MANAGER` selection. Bullet-proof
    /// handoff on exit; `arboard`'s `SAVE_TARGETS` will succeed.
    Icccm,
    /// A known polling clipboard manager is running but does not own
    /// `CLIPBOARD_MANAGER`. The history watcher will still capture
    /// dictations from the live `CLIPBOARD` selection while fono is
    /// running, but there is no formal handoff on exit. The contained
    /// string is the executable name we matched (e.g. `"clipit"`).
    Polling(String),
    /// Neither path matched.
    None,
}

/// Detect a clipboard manager. Returns `None` outside Linux or when
/// the X server is unreachable (and the `/proc` scan also turns up
/// nothing). The Wayland case is handled the same way for now: most
/// Wayland clipboard managers (`wl-clip-persist`, `cliphist`) appear
/// in `/proc` so the polling-mode path covers them too.
pub fn detect() -> ClipboardManager {
    if icccm_clipboard_manager_present().unwrap_or(false) {
        return ClipboardManager::Icccm;
    }
    if let Some(name) = scan_proc_for_known_manager() {
        return ClipboardManager::Polling(name);
    }
    ClipboardManager::None
}

/// True when an X11 client owns the `CLIPBOARD_MANAGER` selection.
/// Returns `None` if we cannot connect to the X server at all (e.g. no
/// `DISPLAY`); the caller should treat that as "unknown", not "absent".
fn icccm_clipboard_manager_present() -> Option<bool> {
    #[cfg(feature = "x11-paste")]
    {
        use x11rb::protocol::xproto::ConnectionExt;
        let (conn, _screen) = x11rb::connect(None).ok()?;
        let atom = conn.intern_atom(false, b"CLIPBOARD_MANAGER").ok()?.reply().ok()?.atom;
        let owner = conn.get_selection_owner(atom).ok()?.reply().ok()?.owner;
        Some(owner != x11rb::NONE)
    }
    #[cfg(not(feature = "x11-paste"))]
    {
        None
    }
}

/// Scan `/proc/*/comm` for a process whose executable name matches one
/// of [`KNOWN_MANAGER_BINS`]. Linux-only; returns `None` everywhere
/// else (the `/proc` directory simply won't exist).
fn scan_proc_for_known_manager() -> Option<String> {
    let proc = Path::new("/proc");
    let entries = std::fs::read_dir(proc).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let pid = name.to_str().and_then(|s| s.parse::<u32>().ok());
        if pid.is_none() {
            continue;
        }
        let comm_path = entry.path().join("comm");
        let Ok(contents) = std::fs::read_to_string(&comm_path) else {
            continue;
        };
        let comm = contents.trim().to_ascii_lowercase();
        if comm.is_empty() {
            continue;
        }
        if KNOWN_MANAGER_BINS.iter().any(|m| m.eq_ignore_ascii_case(&comm)) {
            return Some(comm);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_known_variant() {
        // Just exercise the entry point — result depends on host state.
        let _ = detect();
    }

    #[test]
    fn known_manager_table_is_lowercase_and_short_enough_for_proc_comm() {
        for name in KNOWN_MANAGER_BINS {
            assert!(name.is_ascii(), "{name} must be ASCII");
            assert_eq!(name.to_ascii_lowercase(), *name, "{name} must already be lowercase");
            assert!(
                name.len() <= 15,
                "{name} is {} bytes; /proc/<pid>/comm truncates at 15",
                name.len()
            );
        }
    }
}
