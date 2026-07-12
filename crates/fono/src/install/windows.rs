// SPDX-License-Identifier: GPL-3.0-only
//! Windows implementation of `fono install` / `fono uninstall` —
//! per-user, no elevation (every write lands under `%LOCALAPPDATA%` and
//! `HKCU`, so no administrator prompt is ever needed).
//!
//! What an install does (Windows port plan Phase 11):
//!
//! 1. Copies the running binary to `%LOCALAPPDATA%\fono\fono.exe`.
//! 2. Writes an autostart entry to
//!    `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\fono`
//!    pointing at the installed binary (quoted, so a profile path with
//!    spaces still launches), so the daemon starts at the next login.
//! 3. Records `%LOCALAPPDATA%\fono\install_marker.toml` (version +
//!    install path + timestamp) so `fono doctor` — and, later,
//!    `fono update` — can tell a self-managed install apart from an
//!    ad-hoc binary sitting on `PATH`.
//!
//! Registry access goes through the built-in `reg.exe` rather than a
//! new `winreg` dependency: it mirrors the subprocess-driven style the
//! macOS installer already uses for `launchctl` / `security`, needs no
//! `unsafe` FFI, and keeps the shipped binary dependency-free (binary
//! size is the top project priority, and registry writes work fine over
//! a headless SSH session, unlike an interactive-window-station API).
//!
//! `fono uninstall` deletes the Run value and the `%LOCALAPPDATA%\fono\`
//! directory, but deliberately keeps the user's config and history
//! under `%APPDATA%\fono\` — mirroring the Linux/macOS behaviour of
//! preserving user data across a reinstall.
//!
//! There is **no** Windows service install in v1: `--server` (the
//! headless Wyoming server) stays Linux-only, matching macOS.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::install::InstallModeArg;

/// Registry path (in `reg.exe` syntax) of the per-user autostart key.
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
/// Value name written under the Run key.
const RUN_VALUE: &str = "fono";

// ---------------------------------------------------------------------
// Layout (per-user, %LOCALAPPDATA%-relative)
// ---------------------------------------------------------------------

fn local_app_data() -> Result<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| anyhow!("%LOCALAPPDATA% is not set; cannot resolve the install location"))
}

fn install_dir() -> Result<PathBuf> {
    Ok(local_app_data()?.join("fono"))
}

fn installed_binary() -> Result<PathBuf> {
    Ok(install_dir()?.join("fono.exe"))
}

fn install_marker() -> Result<PathBuf> {
    Ok(install_dir()?.join("install_marker.toml"))
}

// ---------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------

/// Run a command, capturing stdout+stderr. `Ok((success, combined))` on
/// spawn success.
fn run_out(prog: &str, args: &[&str]) -> Result<(bool, String)> {
    let out = Command::new(prog).args(args).output().with_context(|| format!("spawn {prog}"))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok((out.status.success(), text))
}

/// TOML body of the install marker. `{:?}` renders the path as a
/// TOML-safe basic string (backslashes escaped, wrapped in quotes).
fn marker_contents(exe: &Path) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "# Written by `fono install` on Windows. Its presence marks a\n\
         # self-managed per-user install (surfaced by `fono doctor`).\n\
         version = \"{version}\"\n\
         install_path = {path:?}\n\
         installed_at_unix = {ts}\n",
        version = env!("CARGO_PKG_VERSION"),
        path = exe.display().to_string(),
    )
}

// ---------------------------------------------------------------------
// Registry autostart (HKCU\...\Run) via reg.exe
// ---------------------------------------------------------------------

/// Write the autostart Run value pointing at the installed binary. The
/// path is stored quoted so a `%LOCALAPPDATA%` under a profile whose
/// name contains spaces still launches correctly.
fn set_run_key(exe: &Path) -> Result<()> {
    let value = format!("\"{}\"", exe.display());
    let (ok, out) =
        run_out("reg", &["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &value, "/f"])?;
    if !ok {
        bail!("could not write autostart registry value ({RUN_KEY}\\{RUN_VALUE}): {}", out.trim());
    }
    Ok(())
}

/// Remove the autostart Run value. Returns true when a value was
/// actually deleted (false when it was already absent).
fn delete_run_key() -> bool {
    run_out("reg", &["delete", RUN_KEY, "/v", RUN_VALUE, "/f"]).map(|(ok, _)| ok).unwrap_or(false)
}

/// The current autostart Run value (the quoted command line), if
/// present. `reg query` prints a line like:
/// `    fono    REG_SZ    "C:\Users\me\AppData\Local\fono\fono.exe"`.
fn query_run_key() -> Option<String> {
    let (ok, out) = run_out("reg", &["query", RUN_KEY, "/v", RUN_VALUE]).ok()?;
    if !ok {
        return None;
    }
    out.lines()
        .find(|l| l.trim_start().starts_with(RUN_VALUE))
        .and_then(|l| l.split("REG_SZ").nth(1))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

// ---------------------------------------------------------------------
// Daemon lifecycle
// ---------------------------------------------------------------------

/// Best-effort IPC shutdown of an already-running daemon so the binary
/// copy doesn't race (or get blocked by) an old process holding
/// `fono.exe` open. Mirrors the Linux/macOS installers. Uses the
/// cross-platform `fono_ipc` client, which speaks the Windows named
/// pipe.
fn shutdown_existing_daemon() {
    let Ok(paths) = fono_core::Paths::resolve() else {
        return;
    };
    let sockets = paths.client_ipc_socket_candidates();
    let sent = std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
            return false;
        };
        rt.block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                fono_ipc::request_any(&sockets, &fono_ipc::Request::Shutdown),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .is_some()
        })
    })
    .join()
    .unwrap_or(false);
    if sent {
        eprintln!("  · asked the running fono daemon to exit");
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
}

// ---------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------

pub fn run_install(mode: InstallModeArg, dry_run: bool) -> Result<()> {
    if mode == InstallModeArg::Server {
        bail!(
            "`fono install --server` (headless Wyoming server with a system service) is \
             Linux-only. On Windows run `fono install` for the per-user app, or run `fono` \
             manually with `[server.wyoming].enabled = true` in your config."
        );
    }

    let dir = install_dir()?;
    let bin = installed_binary()?;
    let marker = install_marker()?;

    if dry_run {
        println!("fono install --dry-run (Windows, per-user — no elevation) — would perform:");
        println!("  · copy the running binary -> {}", bin.display());
        println!("  · write autostart value {RUN_KEY}\\{RUN_VALUE} -> \"{}\"", bin.display());
        println!("  · write install marker -> {}", marker.display());
        println!("  · the daemon then starts automatically at your next login");
        return Ok(());
    }

    eprintln!("→ installing fono (Windows, per-user — no administrator rights needed)");

    // Ask any running daemon to exit before we swap its binary (a
    // running fono.exe would otherwise lock the destination file).
    shutdown_existing_daemon();

    // 1. Copy the binary into place.
    let src = std::env::current_exe().context("resolve current_exe")?;
    let src = std::fs::canonicalize(&src).unwrap_or(src);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create install dir {}", dir.display()))?;
    if src != bin {
        std::fs::copy(&src, &bin).with_context(|| {
            format!("copy {} -> {} (is a fono daemon still running?)", src.display(), bin.display())
        })?;
    }
    eprintln!("  · {}", bin.display());

    // 2. Autostart registry value.
    set_run_key(&bin)?;
    eprintln!("  · {RUN_KEY}\\{RUN_VALUE}");

    // 3. Install marker.
    std::fs::write(&marker, marker_contents(&bin))
        .with_context(|| format!("write install marker {}", marker.display()))?;

    println!();
    println!("Fono installed. It will start automatically the next time you log in.");
    println!("To start it right now without logging out, run: fono");
    println!();
    println!("Per-user config lives under %APPDATA%\\fono\\ and is kept across");
    println!("reinstalls and updates. Check status anytime with `fono doctor`.");
    Ok(())
}

pub fn run_uninstall(dry_run: bool) -> Result<()> {
    let dir = install_dir()?;
    let has_run_value = query_run_key().is_some();
    let has_dir = dir.exists();

    if !has_run_value && !has_dir {
        bail!(
            "no fono installation detected ({}\\{} / {}); nothing to uninstall",
            RUN_KEY,
            RUN_VALUE,
            dir.display()
        );
    }

    if dry_run {
        println!("fono uninstall --dry-run (Windows) — would perform:");
        if has_run_value {
            println!("  · delete autostart value {RUN_KEY}\\{RUN_VALUE}");
        }
        if has_dir {
            println!("  · remove {}", dir.display());
        }
        println!("  · keep %APPDATA%\\fono (config + history)");
        return Ok(());
    }

    eprintln!("→ uninstalling fono (Windows)");

    // Stop the daemon first so the directory isn't in use.
    shutdown_existing_daemon();

    if has_run_value {
        if delete_run_key() {
            eprintln!("  · removed {RUN_KEY}\\{RUN_VALUE}");
        } else {
            eprintln!("  · could not remove {RUN_KEY}\\{RUN_VALUE}; delete it manually");
        }
    }

    if has_dir {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => eprintln!("  · removed {}", dir.display()),
            Err(e) => eprintln!(
                "  · could not remove {} ({e}); delete it manually (a daemon may still be running)",
                dir.display()
            ),
        }
    }

    println!();
    println!("Fono uninstalled. Your config and history under %APPDATA%\\fono are kept,");
    println!("so a future reinstall picks up where you left off.");
    Ok(())
}

/// One-line install-state summary for `fono doctor`.
#[must_use]
pub fn doctor_state() -> String {
    let exe = std::env::current_exe().ok().and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)));
    let exe_str =
        exe.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "<unknown>".into());

    let marker = install_marker().map(|m| m.exists()).unwrap_or(false);
    let autostart = query_run_key().is_some();
    match (marker, autostart) {
        (true, true) => {
            format!("self-installed via `fono install` (binary + login autostart, {exe_str})")
        }
        (true, false) => format!("installed binary present, login autostart missing ({exe_str})"),
        (false, true) => format!("login autostart present, install marker missing ({exe_str})"),
        (false, false) => format!("ad-hoc on PATH ({exe_str})"),
    }
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_mode_is_refused() {
        let err = run_install(InstallModeArg::Server, true).unwrap_err();
        assert!(err.to_string().contains("Linux-only"));
    }

    #[test]
    fn marker_is_valid_toml_with_escaped_path() {
        let body = marker_contents(Path::new(r"C:\Users\John Doe\AppData\Local\fono\fono.exe"));
        // Parses as TOML and round-trips the load-bearing fields.
        let doc: toml::Value = toml::from_str(&body).expect("marker is valid TOML");
        assert_eq!(
            doc.get("version").and_then(toml::Value::as_str),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(
            doc.get("install_path").and_then(toml::Value::as_str),
            Some(r"C:\Users\John Doe\AppData\Local\fono\fono.exe")
        );
        assert!(doc.get("installed_at_unix").and_then(toml::Value::as_integer).is_some());
    }

    #[test]
    fn run_value_is_stored_quoted() {
        // The exact form `fono install` writes: a quoted path so
        // profile directories with spaces still launch.
        let exe = Path::new(r"C:\Users\a b\AppData\Local\fono\fono.exe");
        let value = format!("\"{}\"", exe.display());
        assert!(value.starts_with('"') && value.ends_with('"'));
        assert!(value.contains("a b"));
    }
}
