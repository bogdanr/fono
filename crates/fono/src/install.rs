// SPDX-License-Identifier: GPL-3.0-only
//! `fono install` / `fono uninstall` — system-wide self-installer.
//!
//! Two modes selected via `--server`:
//!
//! - **Desktop** (default): installs the binary, menu desktop entry,
//!   XDG autostart entry, icon, and shell completions. Daemon launches
//!   automatically on next graphical login via the autostart file.
//! - **Server** (`--server`): installs the binary, a hardened
//!   system-wide systemd unit, the `fono` system user, and shell
//!   completions. The unit is enabled and started immediately.
//!
//! Symmetric `fono uninstall` reads the install marker written at
//! install time and reverses exactly what was put down.
//!
//! Plan: `plans/2026-05-02-fono-install-subcommand-v3.md`.

use std::io::Write;
use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::install::assets::{DESKTOP, ICON_SVG, SYSTEMD_SYSTEM_UNIT};

mod assets {
    //! Embedded packaging assets. Single source of truth at
    //! `packaging/assets/`; the four distro packaging recipes can
    //! also read from there so the embedded copy and the distro
    //! copy never drift.

    pub const DESKTOP: &str = include_str!("../../../packaging/assets/fono.desktop");
    pub const ICON_SVG: &[u8] = include_bytes!("../../../packaging/assets/fono.svg");
    pub const SYSTEMD_SYSTEM_UNIT: &str = include_str!("../../../packaging/assets/fono.service");
}

// ---------------------------------------------------------------------
// Layout (system-wide, fixed paths — no overrides)
// ---------------------------------------------------------------------

const BIN_PATH: &str = "/usr/local/bin/fono";
const DESKTOP_MENU: &str = "/usr/share/applications/fono.desktop";
const DESKTOP_AUTOSTART: &str = "/etc/xdg/autostart/fono.desktop";
const ICON_PATH: &str = "/usr/share/icons/hicolor/scalable/apps/fono.svg";
const SYSTEMD_UNIT: &str = "/lib/systemd/system/fono.service";
const COMPLETION_BASH: &str = "/usr/share/bash-completion/completions/fono";
const COMPLETION_ZSH: &str = "/usr/share/zsh/site-functions/_fono";
const COMPLETION_FISH: &str = "/usr/share/fish/vendor_completions.d/fono.fish";
const MARKER_PATH: &str = "/usr/local/share/fono/install_marker.toml";

const SERVICE_USER: &str = "fono";

// ---------------------------------------------------------------------
// Install marker
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Desktop,
    Server,
}

impl Mode {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Server => "server",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Marker {
    pub mode: Mode,
    pub version: String,
    pub installed_at: String,
    /// Files written by the installer, in the order they were created.
    /// Uninstall removes them in reverse order.
    pub files: Vec<String>,
    /// True if `useradd fono` ran during install (server mode); the
    /// uninstaller uses this to decide whether to attempt `userdel`.
    #[serde(default)]
    pub created_service_user: bool,
    /// True if `systemctl enable --now fono.service` ran during
    /// install. Uninstall runs `systemctl disable --now` only when set.
    #[serde(default)]
    pub enabled_service: bool,
}

impl Marker {
    fn load(path: &Path) -> Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(Some(toml::from_str(&s).context("parse install marker")?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(anyhow!("read {}: {}", path.display(), e)),
        }
    }

    fn save(&self, path: &Path) -> Result<()> {
        let s = toml::to_string_pretty(self).context("serialise install marker")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir -p {}", parent.display()))?;
        }
        std::fs::write(path, s).with_context(|| format!("write {}", path.display()))
    }
}

fn now_iso8601() -> String {
    // Deliberately avoid pulling chrono in just for the timestamp;
    // the Unix-epoch seconds string is precise enough for an audit
    // marker and matches what `fono-update`'s cache uses.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("@{secs}")
}

// ---------------------------------------------------------------------
// Plan / Report (used by both real install and --dry-run)
// ---------------------------------------------------------------------

/// What an install (or uninstall) is about to do, or just did.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub mode: Option<Mode>,
    pub steps: Vec<String>,
}

impl Plan {
    fn step(&mut self, s: impl Into<String>) {
        self.steps.push(s.into());
    }

    fn print(&self, header: &str) {
        println!("{header}");
        for step in &self.steps {
            println!("  · {step}");
        }
    }
}

// ---------------------------------------------------------------------
// Pre-flight
// ---------------------------------------------------------------------

#[cfg(unix)]
fn current_euid() -> u32 {
    // SAFETY: geteuid is async-signal-safe and always succeeds.
    unsafe { libc_geteuid() }
}

#[cfg(unix)]
extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}

#[cfg(not(unix))]
fn current_euid() -> u32 {
    1
}

fn require_root() -> Result<()> {
    if current_euid() != 0 {
        bail!("this command must be run as root: try `sudo fono install`");
    }
    Ok(())
}

fn refuse_if_package_managed() -> Result<()> {
    let exe = std::env::current_exe().context("resolve current_exe")?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    if fono_update::is_package_managed(&exe) {
        bail!(
            "{} is owned by your distro's package manager; \
             update through it instead of running `fono install`",
            exe.display()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Atomic file write
// ---------------------------------------------------------------------

#[cfg(unix)]
fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("path {} has no parent dir", path.display()))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".fono-install-")
        .tempfile_in(dir)
        .with_context(|| format!("create temp file in {}", dir.display()))?;
    tmp.as_file_mut()
        .write_all(bytes)
        .with_context(|| format!("write temp file for {}", path.display()))?;
    tmp.as_file_mut().flush().ok();
    tmp.as_file_mut().sync_all().ok();
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("chmod {:o} {}", mode, tmp.path().display()))?;
    tmp.persist(path)
        .map_err(|e| anyhow!("persist into {}: {}", path.display(), e.error))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_atomic(_path: &Path, _bytes: &[u8], _mode: u32) -> Result<()> {
    bail!("install is supported on Linux only");
}

fn copy_running_binary_to(dest: &Path) -> Result<()> {
    let src = std::env::current_exe().context("resolve current_exe")?;
    let src = std::fs::canonicalize(&src).unwrap_or(src);
    if src == dest {
        // Idempotent re-install — running binary is already in place.
        return Ok(());
    }
    let bytes =
        std::fs::read(&src).with_context(|| format!("read running binary at {}", src.display()))?;
    write_atomic(dest, &bytes, 0o755)
}

// ---------------------------------------------------------------------
// Best-effort post-write hooks
// ---------------------------------------------------------------------

fn try_run(prog: &str, args: &[&str]) -> bool {
    Command::new(prog)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn refresh_desktop_database() {
    let _ = try_run(
        "update-desktop-database",
        &["-q", "/usr/share/applications"],
    );
}

fn refresh_icon_cache() {
    let _ = try_run(
        "gtk-update-icon-cache",
        &["-q", "-t", "-f", "/usr/share/icons/hicolor"],
    );
}

fn systemctl_available() -> bool {
    Command::new("systemctl")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------
// `fono doctor` integration
// ---------------------------------------------------------------------

/// One-line install-state summary for `fono doctor`. Distinguishes the
/// four states so users can tell at a glance whether `fono update` /
/// `fono uninstall` will work on this binary.
#[must_use]
pub fn doctor_state() -> String {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)));
    let exe_str = exe
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unknown>".into());

    if let Some(ref p) = exe {
        if fono_update::is_package_managed(p) {
            return format!("package-managed ({exe_str})");
        }
    }

    match Marker::load(Path::new(MARKER_PATH)) {
        Ok(Some(m)) => format!(
            "self-installed via `fono install` ({} mode, v{}, {exe_str})",
            m.mode.as_str(),
            m.version
        ),
        Ok(None) => format!("ad-hoc on PATH ({exe_str})"),
        Err(_) => format!("ad-hoc on PATH ({exe_str}; marker unreadable)"),
    }
}

// ---------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------

pub fn run_install(server: bool, dry_run: bool) -> Result<()> {
    if dry_run {
        let plan = build_install_plan(server);
        plan.print(&format!(
            "fono install --dry-run ({} mode) — would perform:",
            plan.mode.as_ref().map(Mode::as_str).unwrap_or("?")
        ));
        return Ok(());
    }

    require_root()?;
    refuse_if_package_managed()?;

    // Mode-switch refusal — same mode is idempotent (overwrite in
    // place); different mode means the operator must uninstall first.
    let want = if server { Mode::Server } else { Mode::Desktop };
    if let Some(existing) = Marker::load(Path::new(MARKER_PATH))? {
        if existing.mode != want {
            bail!(
                "{} install detected at {}; run `sudo fono uninstall` first if you want to switch to {} mode",
                existing.mode.as_str(),
                MARKER_PATH,
                want.as_str()
            );
        }
    }

    if server {
        run_install_server()
    } else {
        run_install_desktop()
    }
}

pub fn run_uninstall(dry_run: bool) -> Result<()> {
    let marker = Marker::load(Path::new(MARKER_PATH))?
        .ok_or_else(|| anyhow!("no install marker found at {MARKER_PATH}; nothing to uninstall"))?;

    if dry_run {
        let mut plan = Plan {
            mode: Some(marker.mode.clone()),
            ..Plan::default()
        };
        if marker.enabled_service {
            plan.step("systemctl disable --now fono.service");
        }
        for f in marker.files.iter().rev() {
            plan.step(format!("remove {f}"));
        }
        if marker.created_service_user {
            plan.step(format!(
                "userdel {SERVICE_USER} (only if no /etc/fono, /var/lib/fono, /var/cache/fono left)"
            ));
        }
        plan.print(&format!(
            "fono uninstall --dry-run ({} mode) — would perform:",
            marker.mode.as_str()
        ));
        return Ok(());
    }

    require_root()?;
    run_uninstall_real(&marker);
    Ok(())
}

// ---------------------------------------------------------------------
// Plan-building (used for --dry-run and as the source-of-truth list of
// targets the real install path follows)
// ---------------------------------------------------------------------

fn build_install_plan(server: bool) -> Plan {
    let mut plan = Plan::default();
    if server {
        plan.mode = Some(Mode::Server);
        plan.step(format!(
            "ensure system user `{SERVICE_USER}` exists (useradd --system)"
        ));
        plan.step(format!("install running binary -> {BIN_PATH} (mode 0755)"));
        plan.step(format!("write system unit -> {SYSTEMD_UNIT}"));
        plan.step("systemctl daemon-reload");
        plan.step("systemctl enable --now fono.service");
    } else {
        plan.mode = Some(Mode::Desktop);
        plan.step(format!("install running binary -> {BIN_PATH} (mode 0755)"));
        plan.step(format!("write desktop entry -> {DESKTOP_MENU}"));
        plan.step(format!("write XDG autostart entry -> {DESKTOP_AUTOSTART}"));
        plan.step(format!("write icon -> {ICON_PATH}"));
        plan.step("update-desktop-database (best-effort)");
        plan.step("gtk-update-icon-cache (best-effort)");
    }
    plan.step(format!(
        "write completions -> {COMPLETION_BASH}, {COMPLETION_ZSH}, {COMPLETION_FISH}"
    ));
    plan.step(format!("write install marker -> {MARKER_PATH}"));
    plan
}

// ---------------------------------------------------------------------
// Desktop install
// ---------------------------------------------------------------------

fn run_install_desktop() -> Result<()> {
    let mut files: Vec<String> = Vec::new();

    eprintln!("→ installing fono (desktop mode)");

    // Binary
    let bin_path = Path::new(BIN_PATH);
    copy_running_binary_to(bin_path)?;
    files.push(BIN_PATH.into());
    eprintln!("  · {BIN_PATH}");

    // Menu desktop entry
    write_atomic(Path::new(DESKTOP_MENU), DESKTOP.as_bytes(), 0o644)?;
    files.push(DESKTOP_MENU.into());
    eprintln!("  · {DESKTOP_MENU}");

    // XDG autostart entry — same desktop file plus the GNOME autostart
    // hint so sessions that consult `X-GNOME-Autostart-enabled` light
    // it up. Other XDG-compliant DEs ignore the extra key and pick the
    // entry up purely from its presence in /etc/xdg/autostart.
    let mut autostart = String::from(DESKTOP);
    if !autostart.ends_with('\n') {
        autostart.push('\n');
    }
    autostart.push_str("X-GNOME-Autostart-enabled=true\n");
    write_atomic(Path::new(DESKTOP_AUTOSTART), autostart.as_bytes(), 0o644)?;
    files.push(DESKTOP_AUTOSTART.into());
    eprintln!("  · {DESKTOP_AUTOSTART}");

    // Icon
    write_atomic(Path::new(ICON_PATH), ICON_SVG, 0o644)?;
    files.push(ICON_PATH.into());
    eprintln!("  · {ICON_PATH}");

    refresh_desktop_database();
    refresh_icon_cache();

    // Completions
    write_completions(BIN_PATH, &mut files);

    // Marker
    let marker = Marker {
        mode: Mode::Desktop,
        version: env!("CARGO_PKG_VERSION").to_string(),
        installed_at: now_iso8601(),
        files: {
            let mut v = files.clone();
            v.push(MARKER_PATH.into());
            v
        },
        created_service_user: false,
        enabled_service: false,
    };
    marker.save(Path::new(MARKER_PATH))?;
    eprintln!("  · {MARKER_PATH}");

    println!();
    println!("Fono installed (desktop mode). It will start automatically on next");
    println!("graphical login via {DESKTOP_AUTOSTART}.");
    println!();
    println!("To start it now without logging out, run `fono` from a terminal");
    println!("inside your graphical session.");
    println!();
    println!("Per-user config will live under ~/.config/fono/, history under");
    println!("~/.local/share/fono/. The first run launches the setup wizard.");
    Ok(())
}

// ---------------------------------------------------------------------
// Server install
// ---------------------------------------------------------------------

fn run_install_server() -> Result<()> {
    let mut files: Vec<String> = Vec::new();
    let created_user = ensure_service_user()?;
    let mut enabled_service = false;

    eprintln!("→ installing fono (server mode)");
    if created_user {
        eprintln!("  · created system user `{SERVICE_USER}`");
    } else {
        eprintln!("  · system user `{SERVICE_USER}` already exists");
    }

    // Binary
    copy_running_binary_to(Path::new(BIN_PATH))?;
    files.push(BIN_PATH.into());
    eprintln!("  · {BIN_PATH}");

    // Systemd unit
    write_atomic(
        Path::new(SYSTEMD_UNIT),
        SYSTEMD_SYSTEM_UNIT.as_bytes(),
        0o644,
    )?;
    files.push(SYSTEMD_UNIT.into());
    eprintln!("  · {SYSTEMD_UNIT}");

    // Completions
    write_completions(BIN_PATH, &mut files);

    if systemctl_available() {
        if !try_run("systemctl", &["daemon-reload"]) {
            tracing::warn!("systemctl daemon-reload failed (continuing)");
        }
        if try_run("systemctl", &["enable", "--now", "fono.service"]) {
            enabled_service = true;
            eprintln!("  · systemctl enable --now fono.service");
        } else {
            tracing::warn!(
                "systemctl enable --now fono.service failed; check `systemctl status fono.service`"
            );
        }
    } else {
        eprintln!(
            "  · systemd not detected; the unit file is in place but you must wire it into your init system manually"
        );
    }

    let marker = Marker {
        mode: Mode::Server,
        version: env!("CARGO_PKG_VERSION").to_string(),
        installed_at: now_iso8601(),
        files: {
            let mut v = files.clone();
            v.push(MARKER_PATH.into());
            v
        },
        created_service_user: created_user,
        enabled_service,
    };
    marker.save(Path::new(MARKER_PATH))?;
    eprintln!("  · {MARKER_PATH}");

    println!();
    println!("Fono installed (server mode).");
    if enabled_service {
        println!("The fono.service unit is enabled and running. Check status with:");
        println!("  systemctl status fono.service");
    } else {
        println!("The fono.service unit was written but not enabled (no systemd or");
        println!("enable failed). Enable manually with:");
        println!("  sudo systemctl enable --now fono.service");
    }
    println!();
    println!("Service config lives under /etc/fono/, state under /var/lib/fono/,");
    println!("cache under /var/cache/fono/. See docs/providers.md for Wyoming");
    println!("server configuration.");
    Ok(())
}

fn ensure_service_user() -> Result<bool> {
    if user_exists(SERVICE_USER) {
        return Ok(false);
    }
    let ok = try_run(
        "useradd",
        &[
            "--system",
            "--no-create-home",
            "--shell",
            "/usr/sbin/nologin",
            "--user-group",
            SERVICE_USER,
        ],
    );
    if !ok {
        bail!(
            "failed to create system user `{SERVICE_USER}` via useradd; \
             create it manually and re-run `fono install --server`"
        );
    }
    Ok(true)
}

fn user_exists(name: &str) -> bool {
    Command::new("getent")
        .args(["passwd", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------
// Completions (shared)
// ---------------------------------------------------------------------

fn write_completions(bin_path: &str, files: &mut Vec<String>) {
    for (shell, dst) in [
        ("bash", COMPLETION_BASH),
        ("zsh", COMPLETION_ZSH),
        ("fish", COMPLETION_FISH),
    ] {
        // Skip silently when the parent directory's grandparent (e.g.
        // /usr/share/fish) is missing — that shell isn't installed
        // system-wide and we shouldn't conjure up its skeleton.
        let dst_path = Path::new(dst);
        let Some(parent) = dst_path.parent() else {
            continue;
        };
        let Some(grandparent) = parent.parent() else {
            continue;
        };
        if !grandparent.exists() {
            tracing::debug!(
                shell,
                "skipping completion: {} missing",
                grandparent.display()
            );
            continue;
        }

        match Command::new(bin_path).args(["completions", shell]).output() {
            Ok(out) if out.status.success() && !out.stdout.is_empty() => {
                if let Err(e) = write_atomic(dst_path, &out.stdout, 0o644) {
                    tracing::warn!(shell, "writing {dst} failed: {e:#}");
                } else {
                    files.push(dst.into());
                    eprintln!("  · {dst}");
                }
            }
            Ok(out) => {
                tracing::warn!(
                    shell,
                    "completions {shell} returned non-zero or empty: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            Err(e) => {
                tracing::warn!(shell, "spawning completions {shell} failed: {e:#}");
            }
        }
    }
}

// ---------------------------------------------------------------------
// Uninstall
// ---------------------------------------------------------------------

fn run_uninstall_real(marker: &Marker) {
    eprintln!("→ uninstalling fono ({} mode)", marker.mode.as_str());

    if marker.mode == Mode::Server && marker.enabled_service && systemctl_available() {
        let _ = try_run("systemctl", &["disable", "--now", "fono.service"]);
        eprintln!("  · systemctl disable --now fono.service");
    }

    // Remove files in reverse order of installation. The marker itself
    // is the last entry of the list and gets removed last.
    for f in marker.files.iter().rev() {
        match std::fs::remove_file(f) {
            Ok(()) => eprintln!("  · removed {f}"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("  · already gone: {f}");
            }
            Err(e) => {
                tracing::warn!(file = f.as_str(), "remove failed: {e:#}");
            }
        }
    }

    if marker.mode == Mode::Server && systemctl_available() {
        let _ = try_run("systemctl", &["daemon-reload"]);
    }
    if marker.mode == Mode::Desktop {
        refresh_desktop_database();
        refresh_icon_cache();
    }

    if marker.mode == Mode::Server && marker.created_service_user {
        if service_state_remaining() {
            eprintln!(
                "  · keeping system user `{SERVICE_USER}` (state remains under /etc/fono, /var/lib/fono, or /var/cache/fono)"
            );
        } else if try_run("userdel", &[SERVICE_USER]) {
            eprintln!("  · removed system user `{SERVICE_USER}`");
        } else {
            tracing::warn!("userdel {SERVICE_USER} failed; remove manually if no longer needed");
        }
    }

    // Best-effort: rmdir the marker's parent if it's now empty.
    if let Some(parent) = Path::new(MARKER_PATH).parent() {
        let _ = std::fs::remove_dir(parent);
    }

    println!();
    println!("Fono uninstalled.");
    if marker.mode == Mode::Desktop {
        println!("Per-user config (~/.config/fono), history (~/.local/share/fono),");
        println!("and cache (~/.cache/fono) are kept and belong to the user.");
    }
}

fn service_state_remaining() -> bool {
    ["/etc/fono", "/var/lib/fono", "/var/cache/fono"]
        .iter()
        .any(|p| Path::new(p).exists())
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_desktop_plan_lists_all_targets() {
        let plan = build_install_plan(false);
        assert_eq!(plan.mode, Some(Mode::Desktop));
        let joined = plan.steps.join("\n");
        for t in [
            BIN_PATH,
            DESKTOP_MENU,
            DESKTOP_AUTOSTART,
            ICON_PATH,
            COMPLETION_BASH,
            MARKER_PATH,
        ] {
            assert!(joined.contains(t), "desktop plan missing {t}");
        }
        assert!(!joined.contains(SYSTEMD_UNIT));
    }

    #[test]
    fn build_server_plan_lists_all_targets() {
        let plan = build_install_plan(true);
        assert_eq!(plan.mode, Some(Mode::Server));
        let joined = plan.steps.join("\n");
        for t in [BIN_PATH, SYSTEMD_UNIT, COMPLETION_BASH, MARKER_PATH] {
            assert!(joined.contains(t), "server plan missing {t}");
        }
        assert!(joined.contains("useradd"));
        assert!(joined.contains("systemctl enable"));
        assert!(!joined.contains(DESKTOP_AUTOSTART));
        assert!(!joined.contains(ICON_PATH));
    }

    #[test]
    fn marker_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install_marker.toml");
        let m = Marker {
            mode: Mode::Server,
            version: "9.9.9".into(),
            installed_at: "@1234".into(),
            files: vec![
                "/usr/local/bin/fono".into(),
                "/lib/systemd/system/fono.service".into(),
            ],
            created_service_user: true,
            enabled_service: true,
        };
        m.save(&path).unwrap();
        let loaded = Marker::load(&path).unwrap().unwrap();
        assert_eq!(loaded.mode, Mode::Server);
        assert_eq!(loaded.version, "9.9.9");
        assert_eq!(loaded.files.len(), 2);
        assert!(loaded.created_service_user);
        assert!(loaded.enabled_service);
    }

    #[test]
    fn marker_load_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        assert!(Marker::load(&path).unwrap().is_none());
    }

    #[test]
    fn embedded_assets_nonempty() {
        assert!(DESKTOP.contains("Exec=fono"));
        assert!(SYSTEMD_SYSTEM_UNIT.contains("ExecStart=/usr/local/bin/fono"));
        assert!(SYSTEMD_SYSTEM_UNIT.contains("User=fono"));
        // SVG must contain the SVG opening tag (catches accidental
        // truncation; can't use is_empty since the slice is a const).
        assert!(std::str::from_utf8(ICON_SVG)
            .unwrap_or("")
            .contains("<svg"));
    }
}
