// SPDX-License-Identifier: GPL-3.0-only
//! `fono install` / `fono uninstall` — per-platform self-installer.
//!
//! The public surface (`run_install`, `run_uninstall`, `doctor_state`,
//! [`InstallModeArg`]) is platform-neutral; each OS provides its own
//! implementation module (the "Installer trait split" of the Windows
//! port plan Task 1.6 / macOS port plan Task 9.1 — realised as
//! cfg-dispatched modules with an identical fn signature rather than a
//! literal trait, since exactly one implementation ever exists per
//! compiled binary and a trait object would add ceremony without a
//! seam):
//!
//! - **Linux** (`linux.rs`): system-wide install under `/usr/local`,
//!   XDG desktop/autostart entries or a hardened systemd unit
//!   (`--server`), shell completions.
//! - **macOS** (`macos.rs`): per-user install — `~/Applications/Fono.app`
//!   bundle, a `~/Library/LaunchAgents` plist for login autostart, and
//!   a stable local code-signing identity so TCC permission grants
//!   survive updates. No sudo required.
//! - **Windows** (`windows.rs`): per-user install — the binary under
//!   `%LOCALAPPDATA%\fono\`, a `HKCU\...\Run` registry value for login
//!   autostart, and an install marker. No elevation required.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::Session;
#[cfg(target_os = "linux")]
pub use linux::{doctor_state, run_install, run_uninstall};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{doctor_state, resign_after_update, run_install, run_uninstall};

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::{doctor_state, run_install, run_uninstall};

/// Post-update hook: on macOS, re-seal the app bundle after `fono
/// update` swapped the binary inside it, so the TCC Accessibility grant
/// (keyed to the bundle's designated requirement) survives the update.
/// On every other platform there is nothing to re-sign.
///
/// Returns `None` when nothing needed doing (non-macOS, or a bare
/// binary outside any `.app` bundle), `Some(true)` when the stable
/// local identity re-sealed the bundle, `Some(false)` on the ad-hoc
/// fallback.
#[cfg(not(target_os = "macos"))]
pub fn resign_after_update(
    _installed_at: &std::path::Path,
    _new_version: Option<&str>,
) -> Option<bool> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod unsupported {
    use anyhow::{bail, Result};

    pub fn run_install(_mode: super::InstallModeArg, _dry_run: bool) -> Result<()> {
        bail!("`fono install` is not supported on this platform yet");
    }

    pub fn run_uninstall(_dry_run: bool) -> Result<()> {
        bail!("`fono uninstall` is not supported on this platform yet");
    }

    #[must_use]
    pub fn doctor_state() -> String {
        "not supported on this platform".into()
    }
}
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub use unsupported::{doctor_state, run_install, run_uninstall};

/// CLI-level mode selector shared by every platform: `Server` and
/// `Desktop` are explicit overrides (from `--server` / `--desktop`);
/// `Auto` lets the platform installer pick (on Linux via headless
/// detection; on macOS there is only the per-user desktop lane, and
/// `--server` is refused with pointers to the Linux docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallModeArg {
    Server,
    Desktop,
    Auto,
}
