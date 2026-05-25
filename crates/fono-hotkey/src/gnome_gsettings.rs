// SPDX-License-Identifier: GPL-3.0-only
//! GNOME-Wayland gsettings fallback for global hotkeys.
//!
//! Ubuntu 24.04 ships `xdg-desktop-portal-gnome` 46.2, which does
//! **not** implement the `org.freedesktop.portal.GlobalShortcuts`
//! interface — that landed in v47 (Ubuntu 24.10+). On GNOME 46 the
//! only client-driven way to register a system-wide F7 / F8 binding
//! is via the legacy `org.gnome.settings-daemon.plugins.media-keys`
//! `custom-keybindings` gsettings array.
//!
//! ## Permanent bindings
//!
//! - F7 → `fono toggle`
//! - F8 → `fono assistant`
//!
//! Each invokes the Fono CLI, which routes via IPC to the running
//! daemon. The downside vs the portal listener is no press/release
//! events (so no long-press push-to-talk on this path) and the
//! binding is system-wide for the user's GNOME session — but it
//! works **today** on stock Ubuntu 24.04 GNOME-Wayland with zero
//! extra packages. The bindings are written once and verified
//! idempotent on subsequent daemon launches.
//!
//! ## Dynamic cancel binding (Escape)
//!
//! Esc is *not* registered at startup. Instead, [`spawn`] starts a
//! worker thread that listens on a `crossbeam_channel` for the same
//! [`crate::listener::HotkeyControl`] messages the X11 / portal
//! listeners consume: `EnableCancel` writes a third dconf entry
//! (`fono-cancel`, binding=`Escape`, command=`<exe> cancel`) and
//! appends its path to the custom-keybindings array;
//! `DisableCancel` removes it again. Effect: bare `Escape` is
//! only grabbed by gnome-shell while a recording / assistant turn
//! is actively in flight — outside that window, Esc reaches the
//! focused app normally. This mirrors the X11 listener's
//! `XGrabKey` / `XUngrabKey` semantics for the cancel role.
//!
//! gnome-shell observes dconf changes via the standard `g_settings_*`
//! change-notify path; new bindings take effect within ~50 ms in our
//! measurements on Ubuntu 24.04. That's well below the minimum
//! plausible cancel latency (the user has to press F7, hear the
//! recording-start cue, then decide to press Esc).
//!
//! Uninstalling Fono leaves the F7/F8 entries behind (they stop
//! firing harmlessly when the binary is gone). The worker thread's
//! `Drop` cleans up the dynamic cancel entry on daemon exit so an
//! abrupt shutdown doesn't leave Esc stuck globally bound.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use crossbeam_channel::{unbounded, Sender};
use tracing::{debug, warn};

use crate::listener::{HotkeyControl, ListenerHandle};

const SCHEMA_LIST: &str = "org.gnome.settings-daemon.plugins.media-keys";
const SCHEMA_ENTRY: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
const PATH_DICTATION: &str =
    "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/fono-dictation/";
const PATH_ASSISTANT: &str =
    "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/fono-assistant/";
const PATH_CANCEL: &str =
    "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/fono-cancel/";

/// Returns true on GNOME (Wayland or X11). The `XDG_CURRENT_DESKTOP`
/// env var is colon-delimited per the XDG spec — Ubuntu typically
/// sets it to `ubuntu:GNOME`.
#[must_use]
pub fn is_gnome_session() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .ok()
        .map(|v| v.to_ascii_uppercase().split(':').any(|p| p == "GNOME"))
        .unwrap_or(false)
}

/// Install the permanent F7 / F8 bindings and spawn a worker thread
/// that handles dynamic `EnableCancel` / `DisableCancel` requests by
/// writing / unwriting an `Escape → <exe> cancel` dconf entry.
///
/// Returns a [`ListenerHandle`] shaped exactly like the X11 / portal
/// backends, so the daemon's existing `EnableCancel` send-site at
/// `crates/fono/src/daemon.rs:600` transparently reaches the worker
/// without per-backend branching. The thread exits — and drops the
/// cancel entry if any — when `control` is dropped (i.e. on daemon
/// shutdown).
///
/// Idempotent: re-running with identical bindings is a no-op as far
/// as the user is concerned (gsettings is set, but the value is
/// unchanged).
pub fn spawn(
    dictation_key: &str,
    assistant_key: &str,
    cancel_key: &str,
) -> Result<(std::path::PathBuf, ListenerHandle)> {
    let exe =
        std::env::current_exe().map_err(|e| anyhow::anyhow!("locate current executable: {e}"))?;
    let exe_s = exe.to_string_lossy().to_string();

    // Permanent entries.
    set_entry(PATH_DICTATION, "Fono dictation", &format!("{exe_s} toggle"), dictation_key)?;
    set_entry(PATH_ASSISTANT, "Fono assistant", &format!("{exe_s} assistant"), assistant_key)?;

    // Ensure both paths are in the array. The cancel path is *not*
    // added here — the worker thread manages it dynamically. We do
    // remove any stale `fono-cancel` left over from a previous crash
    // so the daemon starts in a known-clean state.
    let existing = read_list().unwrap_or_default();
    let mut merged: Vec<String> = existing
        .into_iter()
        .filter(|p| p != PATH_DICTATION && p != PATH_ASSISTANT && p != PATH_CANCEL)
        .collect();
    merged.push(PATH_DICTATION.to_string());
    merged.push(PATH_ASSISTANT.to_string());
    write_list(&merged)?;

    // Best-effort: also reset any stale cancel entry. Failure is
    // benign — the path just won't be in the array.
    let _ = reset_entry(PATH_CANCEL);

    let (ctrl_tx, ctrl_rx) = unbounded::<HotkeyControl>();
    let cancel_key = cancel_key.trim().to_string();
    let exe_cancel_cmd = format!("{exe_s} cancel");
    let cancel_bound = Arc::new(AtomicBool::new(false));
    let cancel_bound_thread = Arc::clone(&cancel_bound);

    let thread = std::thread::Builder::new()
        .name("fono-hotkey-gsettings".into())
        .spawn(move || {
            // Worker loop. Bail (and clean up) when the control
            // channel is dropped, which happens on daemon shutdown.
            while let Ok(msg) = ctrl_rx.recv() {
                match msg {
                    HotkeyControl::EnableCancel => {
                        if cancel_key.is_empty() {
                            debug!(
                                "gsettings: EnableCancel ignored (no cancel binding configured)"
                            );
                            continue;
                        }
                        if cancel_bound_thread.load(Ordering::SeqCst) {
                            continue;
                        }
                        if let Err(e) =
                            set_entry(PATH_CANCEL, "Fono cancel", &exe_cancel_cmd, &cancel_key)
                                .and_then(|()| add_to_list(PATH_CANCEL))
                        {
                            warn!(
                                "gsettings: failed to bind cancel key {cancel_key:?}: {e:#}. \
                                 Use `fono cancel` to abort instead."
                            );
                            continue;
                        }
                        cancel_bound_thread.store(true, Ordering::SeqCst);
                        debug!("gsettings: cancel binding {cancel_key:?} added (Esc grabbed)");
                    }
                    HotkeyControl::DisableCancel => {
                        if !cancel_bound_thread.load(Ordering::SeqCst) {
                            continue;
                        }
                        if let Err(e) =
                            remove_from_list(PATH_CANCEL).and_then(|()| reset_entry(PATH_CANCEL))
                        {
                            warn!("gsettings: failed to unbind cancel key: {e:#}");
                            // Leave cancel_bound = true so the next
                            // EnableCancel will retry instead of
                            // assuming the binding is already gone.
                            continue;
                        }
                        cancel_bound_thread.store(false, Ordering::SeqCst);
                        debug!("gsettings: cancel binding removed (Esc released)");
                    }
                }
            }
            // Channel closed → daemon shutdown. Best-effort clean up
            // so we don't leave Esc bound system-wide.
            if cancel_bound_thread.load(Ordering::SeqCst) {
                let _ = remove_from_list(PATH_CANCEL);
                let _ = reset_entry(PATH_CANCEL);
                debug!("gsettings: cancel binding cleaned up on shutdown");
            }
        })
        .map_err(|e| anyhow::anyhow!("spawn gsettings worker thread: {e}"))?;

    Ok((exe, ListenerHandle { thread, control: ctrl_tx }))
}

/// Back-compat wrapper retained so out-of-tree callers (and tests)
/// that only need the static F7 / F8 install can keep using
/// [`install`]. Internally this calls [`spawn`] with an empty cancel
/// key and discards the listener handle, so the worker thread
/// exits immediately (no dynamic Esc binding).
pub fn install(dictation_key: &str, assistant_key: &str) -> Result<std::path::PathBuf> {
    let (exe, _handle) = spawn(dictation_key, assistant_key, "")?;
    Ok(exe)
}

/// Snd a clone-friendly handle into the worker thread.
pub type CancelControl = Sender<HotkeyControl>;

fn read_list() -> anyhow::Result<Vec<String>> {
    let out =
        Command::new("gsettings").args(["get", SCHEMA_LIST, "custom-keybindings"]).output()?;
    if !out.status.success() {
        anyhow::bail!("gsettings get failed");
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    // Output looks like @as [] or ['/path1/', '/path2/']
    let s = raw.trim();
    let inner = s.trim_start_matches("@as").trim().trim_start_matches('[').trim_end_matches(']');
    Ok(inner
        .split(',')
        .map(|p| p.trim().trim_matches(|c| c == '\'' || c == '"').to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

fn write_list(paths: &[String]) -> anyhow::Result<()> {
    let quoted: Vec<String> = paths.iter().map(|p| format!("'{p}'")).collect();
    let value = format!("[{}]", quoted.join(", "));
    let status = Command::new("gsettings")
        .args(["set", SCHEMA_LIST, "custom-keybindings", &value])
        .status()?;
    if !status.success() {
        anyhow::bail!("gsettings set custom-keybindings failed");
    }
    Ok(())
}

fn add_to_list(path: &str) -> anyhow::Result<()> {
    let existing = read_list().unwrap_or_default();
    if existing.iter().any(|p| p == path) {
        return Ok(());
    }
    let mut merged = existing;
    merged.push(path.to_string());
    write_list(&merged)
}

fn remove_from_list(path: &str) -> anyhow::Result<()> {
    let existing = read_list().unwrap_or_default();
    if !existing.iter().any(|p| p == path) {
        return Ok(());
    }
    let merged: Vec<String> = existing.into_iter().filter(|p| p != path).collect();
    write_list(&merged)
}

fn set_entry(path: &str, name: &str, command: &str, binding: &str) -> anyhow::Result<()> {
    let schema_path = format!("{SCHEMA_ENTRY}:{path}");
    for (key, value) in [("name", name), ("command", command), ("binding", binding)] {
        let status = Command::new("gsettings").args(["set", &schema_path, key, value]).status()?;
        if !status.success() {
            anyhow::bail!("gsettings set {schema_path} {key} failed");
        }
    }
    Ok(())
}

fn reset_entry(path: &str) -> anyhow::Result<()> {
    let schema_path = format!("{SCHEMA_ENTRY}:{path}");
    // `gsettings reset-recursively` clears every key under the path.
    // Failure here is non-fatal — the path may not exist (clean
    // install case).
    let _ = Command::new("gsettings").args(["reset-recursively", &schema_path]).status()?;
    Ok(())
}
