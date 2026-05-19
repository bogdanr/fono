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
//! This module registers two keybindings:
//! - F7 → `fono toggle`
//! - F8 → `fono assistant`
//!
//! Each invokes the Fono CLI, which routes via IPC to the running
//! daemon. The downside vs the portal listener is no press/release
//! events (so no long-press push-to-talk on this path) and the
//! binding is system-wide for the user's GNOME session — but it
//! works **today** on stock Ubuntu 24.04 GNOME-Wayland with zero
//! extra packages.
//!
//! The bindings are written once and verified idempotent on
//! subsequent daemon launches; uninstalling Fono leaves them behind
//! (the binding stops firing harmlessly when the binary is gone).

use std::process::Command;

const SCHEMA_LIST: &str = "org.gnome.settings-daemon.plugins.media-keys";
const SCHEMA_ENTRY: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
const PATH_DICTATION: &str =
    "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/fono-dictation/";
const PATH_ASSISTANT: &str =
    "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/fono-assistant/";

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

/// Register F7/F8 as GNOME custom keybindings invoking the Fono CLI.
///
/// Idempotent: re-running with identical bindings is a no-op as far
/// as the user is concerned (gsettings is set, but the value is
/// unchanged).
///
/// Returns the path to the CLI binary that was wired in, or an error
/// if any gsettings invocation failed.
pub fn install(dictation_key: &str, assistant_key: &str) -> anyhow::Result<std::path::PathBuf> {
    let exe =
        std::env::current_exe().map_err(|e| anyhow::anyhow!("locate current executable: {e}"))?;
    let exe_s = exe.to_string_lossy().to_string();

    // Register both entries first.
    set_entry(PATH_DICTATION, "Fono dictation", &format!("{exe_s} toggle"), dictation_key)?;
    set_entry(PATH_ASSISTANT, "Fono assistant", &format!("{exe_s} assistant"), assistant_key)?;

    // Now ensure both paths are in the custom-keybindings array.
    // Read existing list, parse, and merge in the two Fono paths.
    let existing = read_list().unwrap_or_default();
    let mut merged: Vec<String> =
        existing.into_iter().filter(|p| p != PATH_DICTATION && p != PATH_ASSISTANT).collect();
    merged.push(PATH_DICTATION.to_string());
    merged.push(PATH_ASSISTANT.to_string());
    write_list(&merged)?;

    Ok(exe)
}

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
