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
//! `fono uninstall` is filesystem-driven: it removes every known
//! install path that actually exists on disk. There is **no**
//! `install_marker.toml` — the installer only writes the binary,
//! desktop / autostart / icon files, systemd unit, and shell
//! completions, all at fixed canonical paths. Uninstall (and `fono
//! doctor`) infer state from those paths directly.
//!
//! Plan: `plans/2026-05-02-fono-install-subcommand-v3.md`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::install::assets::{DESKTOP, ICON_SVG, SERVER_CONFIG_SEED, SYSTEMD_SYSTEM_UNIT};

mod assets {
    //! Embedded packaging assets. Single source of truth at
    //! `packaging/assets/`; the four distro packaging recipes can
    //! also read from there so the embedded copy and the distro
    //! copy never drift.

    pub const DESKTOP: &str = include_str!("../../../packaging/assets/fono.desktop");
    pub const ICON_SVG: &[u8] = include_bytes!("../../../packaging/assets/fono.svg");
    pub const SYSTEMD_SYSTEM_UNIT: &str = include_str!("../../../packaging/assets/fono.service");
    /// Minimal `/etc/fono/config.toml` seeded by `fono install --server`
    /// when no config exists yet. Enables the Wyoming STT listener on
    /// `0.0.0.0:10300` so the daemon is LAN-reachable out of the box.
    pub const SERVER_CONFIG_SEED: &str =
        include_str!("../../../packaging/assets/server-config.toml");
}

// ---------------------------------------------------------------------
// Layout (system-wide, fixed paths — no overrides)
// ---------------------------------------------------------------------

const BIN_PATH: &str = "/usr/local/bin/fono";
const DESKTOP_MENU: &str = "/usr/share/applications/fono.desktop";
const DESKTOP_AUTOSTART: &str = "/etc/xdg/autostart/fono.desktop";
const ICON_PATH: &str = "/usr/share/icons/hicolor/scalable/apps/fono.svg";
const SYSTEMD_UNIT: &str = "/lib/systemd/system/fono.service";
const SYSTEM_CONFIG_DIR: &str = "/etc/fono";
const SYSTEM_CONFIG_FILE: &str = "/etc/fono/config.toml";
const SYSTEM_CACHE_DIR: &str = "/var/cache/fono";
const COMPLETION_BASH: &str = "/usr/share/bash-completion/completions/fono";
const COMPLETION_ZSH: &str = "/usr/share/zsh/site-functions/_fono";
const COMPLETION_FISH: &str = "/usr/share/fish/vendor_completions.d/fono.fish";

const SERVICE_USER: &str = "fono";

// ---------------------------------------------------------------------
// Install mode
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
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
    let dir = path.parent().ok_or_else(|| anyhow!("path {} has no parent dir", path.display()))?;
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
    tmp.persist(path).map_err(|e| anyhow!("persist into {}: {}", path.display(), e.error))?;
    Ok(())
}

/// Like [`write_atomic`] but additionally `chown`s the persisted file
/// to `(uid, gid)`. Used by the server-mode installer to seed
/// `/etc/fono/config.toml` as `root:fono 0640` so the daemon (running
/// as `fono`) can read it but it isn't world-readable.
///
/// `chown` happens on the temp file *before* the rename so the final
/// inode is never momentarily root:root world-readable.
#[cfg(unix)]
fn write_atomic_owned(path: &Path, bytes: &[u8], mode: u32, uid: u32, gid: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    let dir = path.parent().ok_or_else(|| anyhow!("path {} has no parent dir", path.display()))?;
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
    chown_path(tmp.path(), uid, gid)
        .with_context(|| format!("chown {uid}:{gid} {}", tmp.path().display()))?;
    tmp.persist(path).map_err(|e| anyhow!("persist into {}: {}", path.display(), e.error))?;
    Ok(())
}

#[cfg(unix)]
fn chown_path(path: &Path, uid: u32, gid: u32) -> Result<()> {
    use std::ffi::CString;
    let c = CString::new(path.as_os_str().as_encoded_bytes())
        .with_context(|| format!("path contains NUL: {}", path.display()))?;
    // SAFETY: chown is async-signal-safe; we pass a valid C string and
    // numeric ids. Negative return means errno is set.
    let rc = unsafe { libc_chown(c.as_ptr(), uid, gid) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        return Err(anyhow!("chown failed: {err}"));
    }
    Ok(())
}

#[cfg(unix)]
extern "C" {
    #[link_name = "chown"]
    fn libc_chown(path: *const std::os::raw::c_char, uid: u32, gid: u32) -> i32;
}

/// Look up `(uid, gid)` for a system user via `getent passwd`. Used
/// by the server-mode seed flow to chown `/etc/fono/config.toml`
/// without hard-coding numeric ids (system UIDs vary per distro).
#[cfg(unix)]
fn resolve_system_user_ids(name: &str) -> Result<(u32, u32)> {
    let out = Command::new("getent")
        .args(["passwd", name])
        .output()
        .with_context(|| format!("spawn getent passwd {name}"))?;
    if !out.status.success() {
        bail!("getent passwd {name} failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let line = String::from_utf8_lossy(&out.stdout);
    // passwd format: name:x:uid:gid:gecos:home:shell
    let fields: Vec<&str> = line.trim_end().split(':').collect();
    if fields.len() < 4 {
        bail!("malformed getent passwd output for {name}: {line:?}");
    }
    let uid: u32 = fields[2].parse().with_context(|| format!("parse uid for {name}"))?;
    let gid: u32 = fields[3].parse().with_context(|| format!("parse gid for {name}"))?;
    Ok((uid, gid))
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
    let _ = try_run("update-desktop-database", &["-q", "/usr/share/applications"]);
}

fn refresh_icon_cache() {
    let _ = try_run("gtk-update-icon-cache", &["-q", "-t", "-f", "/usr/share/icons/hicolor"]);
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

/// Best-effort post-`enable --now` health check. systemd's `enable --now`
/// returns success the moment the unit is *started*, but a service that
/// crashes during early init (TTY-required wizard, missing audio device,
/// bad config) will fail a couple of seconds later and end up in a
/// `Restart=on-failure` loop the user only notices much later. Wait
/// briefly, then surface a one-line summary; if the unit is not active
/// after the wait, dump the last few journal lines so the failure mode
/// is visible without requiring the user to know the right `journalctl`
/// invocation.
fn verify_service_running(unit: &str) {
    use std::{thread, time::Duration};

    // Give systemd a moment to actually run ExecStart and let the
    // process either settle or crash. 2 s is enough for the daemon's
    // startup path on every machine we test on; if it isn't, the user
    // sees a "still activating" line and can re-run `systemctl status`.
    thread::sleep(Duration::from_secs(2));

    let active = Command::new("systemctl")
        .args(["is-active", unit])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    if active == "active" {
        eprintln!("  · {unit} is active (running)");
        return;
    }

    eprintln!(
        "  · {unit} is {} (expected `active`)",
        if active.is_empty() { "<unknown>" } else { active.as_str() }
    );
    eprintln!("    --- last 20 journal lines for {unit} ---");
    if let Ok(out) =
        Command::new("journalctl").args(["-u", unit, "-n", "20", "--no-pager"]).output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            eprintln!("    {line}");
        }
    } else {
        eprintln!("    (journalctl unavailable)");
    }
    eprintln!("    --- end ---");
    eprintln!(
        "    investigate with: systemctl status {unit} && journalctl -u {unit} -n 100 --no-pager"
    );
}

// ---------------------------------------------------------------------
// `fono doctor` integration
// ---------------------------------------------------------------------

/// One-line install-state summary for `fono doctor`. Distinguishes
/// three states so users can tell at a glance whether `fono update` /
/// `fono uninstall` will work on this binary.
#[must_use]
pub fn doctor_state() -> String {
    let exe = std::env::current_exe().ok().and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)));
    let exe_str =
        exe.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "<unknown>".into());

    if let Some(ref p) = exe {
        if fono_update::is_package_managed(p) {
            return format!("package-managed ({exe_str})");
        }
    }

    detect_installed_mode().map_or_else(
        || format!("ad-hoc on PATH ({exe_str})"),
        |mode| format!("self-installed via `fono install` ({} mode, {exe_str})", mode.as_str()),
    )
}

/// Returns the installed [`Mode`] iff at least one canonical install
/// path exists on disk. Used by both `doctor_state` and the
/// mode-switch refusal in `run_install`.
fn detect_installed_mode() -> Option<Mode> {
    let bin = Path::new(BIN_PATH).exists();
    let unit = Path::new(SYSTEMD_UNIT).exists();
    let desktop = Path::new(DESKTOP_MENU).exists()
        || Path::new(DESKTOP_AUTOSTART).exists()
        || Path::new(ICON_PATH).exists();
    if unit {
        Some(Mode::Server)
    } else if bin || desktop {
        Some(Mode::Desktop)
    } else {
        None
    }
}

// ---------------------------------------------------------------------
// Headless detection (--server auto-default)
// ---------------------------------------------------------------------

/// CLI-level mode selector: `Server` and `Desktop` are explicit
/// overrides (from `--server` / `--desktop`); `Auto` triggers
/// `detect_headless()` so `sudo fono install` on a server picks the
/// systemd-unit lane without the operator having to remember the flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallModeArg {
    Server,
    Desktop,
    Auto,
}

/// Best-effort verdict: is this host running with no graphical session?
///
/// Returns `(true, reason)` only when we're confident the host is
/// headless (no inherited DISPLAY/WAYLAND_DISPLAY, no active loginctl
/// session of type x11/wayland, no display manager active, no
/// X11/Wayland sockets on disk, and either `systemctl get-default =
/// multi-user.target` or systemd is absent entirely). Every other case
/// returns `(false, _)` so the caller falls through to today's silent
/// desktop default. Conservative on purpose: a false negative is
/// recoverable with `--server`; a false positive would surprise a
/// workstation user with an unwanted systemd unit.
pub(crate) fn detect_headless() -> (bool, &'static str) {
    detect_headless_with(&RealProbes)
}

trait HeadlessProbes {
    fn env(&self, key: &str) -> Option<String>;
    /// Run a command, return `Some((status_success, stdout_utf8))` on
    /// successful spawn; `None` on spawn failure.
    fn run(&self, prog: &str, args: &[&str]) -> Option<(bool, String)>;
    /// True iff any direct child of `dir` whose name starts with
    /// `prefix` exists. Used for `/tmp/.X11-unix/X*` and
    /// `/run/user/*/wayland-*`.
    fn dir_has_entry(&self, dir: &Path, prefix: &str) -> bool;
    /// True iff any subdir of `/run/user` contains a child whose name
    /// starts with `wayland-`. Spots active Wayland sessions
    /// independently of loginctl.
    fn any_user_runtime_wayland_socket(&self) -> bool;
}

struct RealProbes;

impl HeadlessProbes for RealProbes {
    fn env(&self, key: &str) -> Option<String> {
        std::env::var_os(key).map(|v| v.to_string_lossy().into_owned())
    }
    fn run(&self, prog: &str, args: &[&str]) -> Option<(bool, String)> {
        let out = Command::new(prog)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        Some((out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned()))
    }
    fn dir_has_entry(&self, dir: &Path, prefix: &str) -> bool {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return false;
        };
        for ent in rd.flatten() {
            if ent.file_name().to_string_lossy().starts_with(prefix) {
                return true;
            }
        }
        false
    }
    fn any_user_runtime_wayland_socket(&self) -> bool {
        let Ok(rd) = std::fs::read_dir("/run/user") else {
            return false;
        };
        for ent in rd.flatten() {
            if self.dir_has_entry(&ent.path(), "wayland-") {
                return true;
            }
        }
        false
    }
}

fn detect_headless_with(p: &dyn HeadlessProbes) -> (bool, &'static str) {
    // 1. Caller's inherited graphical env (e.g. `sudo -E` preserved
    //    DISPLAY). Strongest desktop signal when present.
    if p.env("DISPLAY").is_some() || p.env("WAYLAND_DISPLAY").is_some() {
        return (false, "DISPLAY or WAYLAND_DISPLAY inherited from caller");
    }

    // 2. loginctl: any active graphical user session means desktop.
    //    `list-sessions` output formatting varies across systemd
    //    versions, so we only use it to enumerate session IDs and
    //    re-query each one for its Type/State/Class via show-session.
    if let Some((true, out)) = p.run("loginctl", &["list-sessions", "--no-legend", "--no-pager"]) {
        for line in out.lines() {
            let Some(id) = line.split_whitespace().next() else {
                continue;
            };
            let Some((true, info)) = p.run(
                "loginctl",
                &["show-session", id, "--property=Type", "--property=State", "--property=Class"],
            ) else {
                continue;
            };
            let (mut ty, mut state, mut class) = ("", "", "");
            for kv in info.lines() {
                if let Some(v) = kv.strip_prefix("Type=") {
                    ty = v;
                } else if let Some(v) = kv.strip_prefix("State=") {
                    state = v;
                } else if let Some(v) = kv.strip_prefix("Class=") {
                    class = v;
                }
            }
            if (ty == "x11" || ty == "wayland")
                && (state == "active" || state == "online")
                && class == "user"
            {
                return (false, "active graphical loginctl session");
            }
        }
    }

    // 3. Display manager unit active.
    for dm in [
        "gdm.service",
        "sddm.service",
        "lightdm.service",
        "lxdm.service",
        "xdm.service",
        "greetd.service",
        "ly.service",
    ] {
        if let Some((_, out)) = p.run("systemctl", &["is-active", dm]) {
            if out.trim() == "active" {
                return (false, "display manager service active");
            }
        }
    }

    // 4. Filesystem session sockets — independent of systemd; catches
    //    OpenRC / runit / non-systemd Linux distros running X or
    //    Wayland.
    if p.dir_has_entry(Path::new("/tmp/.X11-unix"), "X") {
        return (false, "X11 socket present under /tmp/.X11-unix");
    }
    if p.any_user_runtime_wayland_socket() {
        return (false, "Wayland socket present under /run/user");
    }

    // 5. Once we've eliminated every positive desktop signal above
    //    (no DISPLAY, no graphical loginctl session, no DM active, no
    //    X11/Wayland socket on disk), the box has no graphical session
    //    *right now* — installing in server mode is the correct
    //    choice regardless of what `systemctl get-default` says. Many
    //    server installs inherit `graphical.target` as the default
    //    from a desktop-flavoured base image but are run headless;
    //    treating that as a desktop signal would mis-classify them.
    //    The default-target probe is now informational only: a
    //    `multi-user.target` value names the trigger more precisely
    //    in the banner; anything else (including spawn failure /
    //    systemd absent) falls back to a generic reason.
    let reason = match p.run("systemctl", &["get-default"]) {
        Some((true, out)) if out.trim() == "multi-user.target" => {
            "systemctl get-default = multi-user.target"
        }
        Some((true, _)) => "no graphical session detected",
        _ => "no systemd and no graphical session detected",
    };
    (true, reason)
}

// ---------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------

pub fn run_install(mode: InstallModeArg, dry_run: bool) -> Result<()> {
    let want = match mode {
        InstallModeArg::Server => Mode::Server,
        InstallModeArg::Desktop => Mode::Desktop,
        InstallModeArg::Auto => {
            let (headless, reason) = detect_headless();
            if headless {
                eprintln!(
                    "→ auto-detected headless host ({reason}); installing in server mode (pass --desktop to override)"
                );
                Mode::Server
            } else {
                Mode::Desktop
            }
        }
    };

    if dry_run {
        let plan = build_install_plan(want == Mode::Server);
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
    // Detection is filesystem-based: a server install drops a systemd
    // unit, a desktop install drops a menu/autostart/icon — the binary
    // alone is ambiguous so we only refuse when the *other* mode's
    // unique artefacts are present.
    match (&want, detect_installed_mode()) {
        (Mode::Server, Some(Mode::Desktop)) => bail!(
            "desktop install detected (e.g. {DESKTOP_MENU}); run `sudo fono uninstall` first if you want to switch to server mode"
        ),
        (Mode::Desktop, Some(Mode::Server)) => bail!(
            "server install detected ({SYSTEMD_UNIT}); run `sudo fono uninstall` first if you want to switch to desktop mode"
        ),
        _ => {}
    }

    if want == Mode::Server {
        run_install_server()
    } else {
        run_install_desktop()
    }
}

pub fn run_uninstall(dry_run: bool) -> Result<()> {
    let state = detect_install_state();

    if state.files.is_empty() {
        bail!(
            "no fono installation detected at any of the known system paths \
             (e.g. {BIN_PATH}, {SYSTEMD_UNIT}, {DESKTOP_MENU}); nothing to uninstall"
        );
    }

    if dry_run {
        let mut plan = Plan { mode: Some(state.mode.clone()), ..Plan::default() };
        if state.enabled_service {
            plan.step("systemctl disable --now fono.service");
        }
        for f in &state.files {
            plan.step(format!("remove {f}"));
        }
        if state.mode == Mode::Desktop {
            if let Some(cache) = caller_cache_dir() {
                if cache.exists() {
                    plan.step(format!(
                        "remove {} (reproducible model / hwcheck cache)",
                        cache.display()
                    ));
                }
            }
        }
        if state.mode == Mode::Server && Path::new(SYSTEM_CACHE_DIR).exists() {
            plan.step(format!("remove {SYSTEM_CACHE_DIR} (reproducible model / hwcheck cache)"));
        }
        if state.system_user_removable {
            plan.step(format!(
                "userdel {SERVICE_USER} (only if no /etc/fono or /var/lib/fono left)"
            ));
        }
        plan.print(&format!(
            "fono uninstall --dry-run ({} mode) — would perform:",
            state.mode.as_str()
        ));
        return Ok(());
    }

    require_root()?;
    run_uninstall_real(&state);
    Ok(())
}

/// Filesystem-derived snapshot of what's currently installed.
struct InstallState {
    mode: Mode,
    /// Known system paths that exist on disk, in *removal* order
    /// (reverse of install order).
    files: Vec<String>,
    /// True iff the systemd unit is currently enabled. Drives whether
    /// uninstall runs `systemctl disable --now`.
    enabled_service: bool,
    /// True iff a system user named `fono` exists *and* looks like one
    /// we created (system UID, nologin shell). Drives whether
    /// uninstall offers to run `userdel fono`. Conservative on
    /// purpose: refuses to remove pre-existing `fono` users that
    /// happen to belong to a real human or another service.
    system_user_removable: bool,
}

fn detect_install_state() -> InstallState {
    // Candidate paths in install order; we keep those that exist (or
    // exist as broken symlinks — still worth `rm`-ing).
    let candidates: &[&str] = &[
        BIN_PATH,
        DESKTOP_MENU,
        DESKTOP_AUTOSTART,
        ICON_PATH,
        SYSTEMD_UNIT,
        COMPLETION_BASH,
        COMPLETION_ZSH,
        COMPLETION_FISH,
    ];
    let mut files: Vec<String> = candidates
        .iter()
        .filter(|p| Path::new(p).symlink_metadata().is_ok())
        .map(|p| (*p).to_string())
        .collect();
    files.reverse();

    // Mode: systemd unit is the canonical server-mode signal.
    let mode = if Path::new(SYSTEMD_UNIT).exists() { Mode::Server } else { Mode::Desktop };

    let enabled_service = mode == Mode::Server && systemctl_unit_enabled("fono.service");

    let system_user_removable = mode == Mode::Server && looks_like_our_system_user(SERVICE_USER);

    InstallState { mode, files, enabled_service, system_user_removable }
}

/// Returns true iff `name` exists in passwd as a system-style user we
/// would plausibly have created: shell is `nologin` (any path).
/// Avoids `userdel`-ing a real interactive user that happens to share
/// the name. Without the marker this is the safest heuristic.
fn looks_like_our_system_user(name: &str) -> bool {
    let out = match Command::new("getent").args(["passwd", name]).output() {
        Ok(o) if o.status.success() => o,
        _ => return false,
    };
    let line = String::from_utf8_lossy(&out.stdout);
    // passwd format: name:x:uid:gid:gecos:home:shell
    let Some(shell) = line.trim_end().rsplit(':').next() else {
        return false;
    };
    shell.ends_with("/nologin") || shell.ends_with("/false")
}

fn systemctl_unit_enabled(unit: &str) -> bool {
    if !systemctl_available() {
        return false;
    }
    Command::new("systemctl")
        .args(["is-enabled", unit])
        .output()
        .ok()
        .map(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            matches!(s.as_str(), "enabled" | "enabled-runtime" | "alias" | "static" | "indirect")
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------
// Plan-building (used for --dry-run and as the source-of-truth list of
// targets the real install path follows)
// ---------------------------------------------------------------------

fn build_install_plan(server: bool) -> Plan {
    let mut plan = Plan::default();
    if server {
        plan.mode = Some(Mode::Server);
        plan.step(format!("ensure system user `{SERVICE_USER}` exists (useradd --system)"));
        plan.step(format!("install running binary -> {BIN_PATH} (mode 0755)"));
        plan.step(format!("write system unit -> {SYSTEMD_UNIT}"));
        plan.step(format!(
            "seed {SYSTEM_CONFIG_FILE} with Wyoming listener on 0.0.0.0:10300 (only if absent)"
        ));
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
    plan
}

// ---------------------------------------------------------------------
// Desktop install
// ---------------------------------------------------------------------

fn run_install_desktop() -> Result<()> {
    eprintln!("→ installing fono (desktop mode)");

    // Binary
    copy_running_binary_to(Path::new(BIN_PATH))?;
    eprintln!("  · {BIN_PATH}");

    // Menu desktop entry
    write_atomic(Path::new(DESKTOP_MENU), DESKTOP.as_bytes(), 0o644)?;
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
    eprintln!("  · {DESKTOP_AUTOSTART}");

    // Icon
    write_atomic(Path::new(ICON_PATH), ICON_SVG, 0o644)?;
    eprintln!("  · {ICON_PATH}");

    refresh_desktop_database();
    refresh_icon_cache();

    // Completions
    write_completions(BIN_PATH);

    // Pre-create the shared log file 0666 so any fono process (XDG
    // autostart, manual `fono`, `sudo fono`) can append. Single-user
    // box; failure is non-fatal.
    if let Err(e) = ensure_log_file() {
        tracing::warn!("could not pre-create {}: {e}", fono_core::paths::LOG_FILE);
    }

    let no_start = std::env::var_os("FONO_INSTALL_NO_START").is_some_and(|v| v == "1");

    // Offer to install the runtime packages Fono needs on this
    // session BEFORE we start the daemon. winit can only construct
    // one EventLoop per process, so the overlay backend selected at
    // daemon startup sticks for the process's lifetime. Installing
    // the libs first means the daemon we spawn next picks the real
    // X11 / wlr-layer-shell backend on the first try and the user
    // never has to restart it manually.
    if !no_start {
        offer_install_missing_packages();
    }

    // If a daemon is already running (e.g. from a previous login's
    // XDG autostart, or a `fono` started manually in a terminal),
    // ask it to exit before we autostart a fresh one. Without this
    // the new daemon would steal the IPC socket while the old
    // process kept running its hotkey + capture threads — and, more
    // importantly, the old process would still be the one with the
    // pre-libxkbcommon-x11 noop overlay, defeating the whole point
    // of reordering above. Best-effort; failure is non-fatal.
    if !no_start {
        shutdown_existing_daemon();
    }

    let autostart_outcome =
        if no_start { AutostartOutcome::SkippedByEnv } else { try_autostart_for_sudo_user() };

    // Once the daemon is up (or we know it can't be auto-started),
    // hand control to the setup wizard for the invoking user. The
    // wizard is interactive — it needs both `$SUDO_USER` (to know
    // whose `~/.config/fono/` to write to) and a real TTY for its
    // prompts. When either is missing we just print a clear next-step
    // line and let the user run `fono setup` themselves.
    //
    // We intentionally run setup *after* starting the daemon: the
    // wizard's final step sends an IPC `Reload`, which is a no-op when
    // the daemon isn't running yet but propagates the new config
    // immediately when it is.
    let setup_outcome =
        if no_start { SetupOutcome::SkippedByEnv } else { try_run_setup_for_sudo_user() };

    println!();
    match autostart_outcome {
        AutostartOutcome::Started(ref user) => {
            println!("Fono installed (desktop mode) and started in the background for `{user}`.");
            println!("It will also start automatically on next login via {DESKTOP_AUTOSTART}.");
        }
        AutostartOutcome::SkippedByEnv => {
            println!(
                "Fono installed (desktop mode). Auto-start skipped (FONO_INSTALL_NO_START=1)."
            );
            println!(
                "It will start automatically on next graphical login via {DESKTOP_AUTOSTART}."
            );
        }
        AutostartOutcome::Headless => {
            println!("Fono installed (desktop mode), but this session looks headless");
            println!("(no DISPLAY / WAYLAND_DISPLAY / XDG_RUNTIME_DIR). The XDG autostart");
            println!("entry at {DESKTOP_AUTOSTART}");
            println!("will start it on the user's next graphical login. If you wanted a");
            println!("headless install, re-run with `sudo fono install --server`.");
        }
        AutostartOutcome::SpawnFailed => {
            println!("Fono installed (desktop mode), but the background start failed.");
            println!("Run `fono` from a terminal inside your graphical session to start it now.");
            println!("The XDG autostart entry will start it on next login.");
        }
    }
    println!();
    match setup_outcome {
        SetupOutcome::Completed => {
            println!("Setup wizard completed.");
        }
        SetupOutcome::SkippedByEnv => {
            println!("Run `fono setup` to choose your STT/LLM/TTS backends and hotkeys.");
        }
        SetupOutcome::NotInteractive => {
            println!("Run `fono setup` from an interactive terminal to choose your STT/LLM/TTS");
            println!("backends and hotkeys.");
        }
        SetupOutcome::SpawnFailed => {
            println!("Run `fono setup` to choose your STT/LLM/TTS backends and hotkeys.");
        }
    }
    println!();
    println!("Per-user config will live under ~/.config/fono/, history under");
    println!("~/.local/share/fono/.");
    Ok(())
}

/// Outcome of the post-install daemon-launch attempt. Drives the
/// printed summary so the user always knows exactly what happened
/// (and why nothing happened, when that's the case).
enum AutostartOutcome {
    /// Launched fono for the named user (`root` is a valid value
    /// when the operator deliberately invoked the installer as bare
    /// root — we honour the choice rather than refuse it).
    Started(String),
    SkippedByEnv,
    Headless,
    SpawnFailed,
}

/// Outcome of the post-install `fono setup` invocation. Same purpose
/// as [`AutostartOutcome`].
enum SetupOutcome {
    Completed,
    SkippedByEnv,
    NotInteractive,
    SpawnFailed,
}

/// Used by desktop-mode uninstall to wipe the per-user `~/.cache/fono`
fn caller_home_dir() -> Option<PathBuf> {
    let user = resolve_target_user()?;
    // passwd format: name:x:uid:gid:gecos:home:shell
    let out = Command::new("getent").args(["passwd", &user]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let fields: Vec<&str> = line.trim_end().split(':').collect();
    if fields.len() < 7 {
        return None;
    }
    let home = fields[5];
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home))
}

/// Per-user XDG cache dir Fono writes to. Mirrors
/// `fono_core::paths::Paths::resolve()` for the desktop case: honours
/// `XDG_CACHE_HOME` if it points at an absolute path (rare under
/// `sudo`, which usually scrubs the env), otherwise falls back to
/// `$HOME/.cache/fono`.
fn caller_cache_dir() -> Option<PathBuf> {
    let home = caller_home_dir()?;
    let root = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".cache"));
    Some(root.join(fono_core::paths::APP_NAME))
}

/// Target user for the post-install spawn: `$SUDO_USER` if set,
/// otherwise `root` when we're already root, otherwise `None`
/// (unreachable past `require_root()`, kept for tests).
fn resolve_target_user() -> Option<String> {
    if let Some(v) = std::env::var_os("SUDO_USER") {
        let s = v.to_string_lossy().into_owned();
        if !s.is_empty() && s != "root" {
            return Some(s);
        }
    }
    if current_euid() == 0 {
        return Some("root".into());
    }
    None
}

/// Create `/var/log/fono.log` 0666 so any user can append to it.
fn ensure_log_file() -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let path = fono_core::paths::LOG_FILE;
    let _ = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))
}

/// Ask any already-running fono daemon to exit, so the autostart that
/// follows starts a clean process. Tries the system service socket
/// (`/var/lib/fono/fono.sock`) and the target user's per-user XDG
/// socket (`~/.local/state/fono/fono.sock`, or `$XDG_STATE_HOME/fono/fono.sock`
/// when set). Best-effort: missing sockets, unreachable daemons, and
/// IPC errors are all silently ignored — there's nothing to do in
/// those cases and the user shouldn't see scary lines.
fn shutdown_existing_daemon() {
    let sockets = candidate_daemon_sockets_for_target_user();
    if sockets.is_empty() {
        return;
    }
    let any_present = sockets.iter().any(|p| p.exists());
    if !any_present {
        return;
    }
    // Spin a tiny current-thread tokio runtime just for this one
    // request. install.rs is otherwise sync; pulling in a runtime
    // here is cheap and self-contained. Build *and* run it on a
    // dedicated OS thread because `run_install` is called from
    // inside the top-level async dispatcher in `cli::run` — calling
    // `block_on` directly here would panic with "Cannot start a
    // runtime from within a runtime" the moment a previous daemon
    // is actually running.
    let sent = std::thread::spawn({
        let sockets = sockets.clone();
        move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                return false;
            };
            rt.block_on(async {
                // 1 s connect/read budget — the daemon either replies fast
                // or it's wedged and we shouldn't block install on it.
                tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    fono_ipc::request_any(&sockets, &fono_ipc::Request::Shutdown),
                )
                .await
                .ok()
                .and_then(Result::ok)
                .is_some()
            })
        }
    })
    .join()
    .unwrap_or(false);
    if sent {
        eprintln!("  · asked existing fono daemon to exit");
        // Give the old daemon a moment to release the socket file
        // and tear down its hotkey grabs before the next autostart
        // races for them. Empirically 300 ms is enough on a quiet
        // box; we cap at 1.5 s.
        for _ in 0..15 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if !sockets.iter().any(|p| p.exists()) {
                break;
            }
        }
    }
}

/// Candidate IPC sockets to probe when shutting down a previously-
/// running daemon. Mirrors `Paths::client_ipc_socket_candidates` but
/// resolves the per-user path against `$SUDO_USER`'s home rather than
/// root's, since `sudo` typically scrubs `$XDG_STATE_HOME`.
fn candidate_daemon_sockets_for_target_user() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    out.push(PathBuf::from(fono_core::paths::SYSTEM_IPC_SOCKET));
    if let Some(home) = caller_home_dir() {
        // $XDG_STATE_HOME if set and absolute, else $HOME/.local/state
        let state_root = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".local/state"));
        out.push(state_root.join("fono").join("fono.sock"));
    }
    out
}

/// Spawn `fono` in the background as the target user. Logs append to
/// `/var/log/fono.log` if writable, else `/dev/null`.
///
/// `sudo fono install` runs with the *sudo-scrubbed* environment:
/// `DISPLAY` is usually preserved, but `WAYLAND_DISPLAY`,
/// `XDG_RUNTIME_DIR`, and `DBUS_SESSION_BUS_ADDRESS` are not. Without
/// those, the spawned daemon's hotkey-backend auto-detector
/// (`fono_hotkey::detect_backend`) lands on `X11` on GNOME-Wayland
/// hosts and the GNOME-gsettings shim never runs — F7/F8 only fire in
/// Xwayland windows, which looks to the user like "hotkeys don't
/// work". Reconstruct the missing graphical-session env from
/// `/run/user/<uid>` *inside the shell command*, so the resolution
/// happens after `runuser` / `sudo -u` has switched to the target
/// user. This mirrors what the next-login XDG autostart entry gets
/// for free.
fn try_autostart_for_sudo_user() -> AutostartOutcome {
    let Some(user) = resolve_target_user() else {
        return AutostartOutcome::SpawnFailed;
    };
    let has_graphical_env = std::env::var_os("DISPLAY").is_some()
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var_os("XDG_RUNTIME_DIR").is_some();
    if !has_graphical_env {
        return AutostartOutcome::Headless;
    }

    let log = fono_core::paths::LOG_FILE;
    // The `RUNTIME=...` preamble runs *as the target user* (after
    // runuser/sudo has switched uids), so `id -u` returns the right
    // uid even when `XDG_RUNTIME_DIR` was scrubbed by sudo. The
    // `WAYLAND_DISPLAY` loop picks the first real socket and skips
    // the `.lock` companion files. All exports are no-ops when the
    // variable was already inherited.
    let shell_cmd = format!(
        "RUNTIME=\"${{XDG_RUNTIME_DIR:-/run/user/$(id -u)}}\"; \
         if [ -d \"$RUNTIME\" ]; then \
           export XDG_RUNTIME_DIR=\"$RUNTIME\"; \
           [ -z \"${{DBUS_SESSION_BUS_ADDRESS:-}}\" ] && [ -S \"$RUNTIME/bus\" ] && \
             export DBUS_SESSION_BUS_ADDRESS=\"unix:path=$RUNTIME/bus\"; \
           if [ -z \"${{WAYLAND_DISPLAY:-}}\" ]; then \
             for s in \"$RUNTIME\"/wayland-*; do \
               case \"$s\" in *.lock) continue;; esac; \
               [ -S \"$s\" ] || continue; \
               WAYLAND_DISPLAY=\"${{s##*/}}\"; export WAYLAND_DISPLAY; \
               break; \
             done; \
           fi; \
         fi; \
         LOG={log}; [ -w \"$LOG\" ] || LOG=/dev/null; \
         setsid fono </dev/null >>\"$LOG\" 2>&1 &"
    );
    let spawn = |prog: &str, args: &[&str]| -> bool {
        Command::new(prog)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
    };

    let started_msg = |who: &str| eprintln!("  · started fono as `{who}` (logs: {log})");

    if user == "root" {
        if spawn("sh", &["-c", &shell_cmd]) {
            started_msg(&user);
            return AutostartOutcome::Started(user);
        }
        return AutostartOutcome::SpawnFailed;
    }
    if which_in_path("runuser") && spawn("runuser", &["-u", &user, "--", "sh", "-c", &shell_cmd]) {
        started_msg(&user);
        return AutostartOutcome::Started(user);
    }
    if which_in_path("sudo") && spawn("sudo", &["-u", &user, "sh", "-c", &shell_cmd]) {
        started_msg(&user);
        return AutostartOutcome::Started(user);
    }
    AutostartOutcome::SpawnFailed
}

/// Run `fono setup` synchronously as the target user, inheriting stdio
/// so the wizard prompts the user on the current terminal.
fn try_run_setup_for_sudo_user() -> SetupOutcome {
    use std::io::IsTerminal;

    let Some(user) = resolve_target_user() else {
        return SetupOutcome::SpawnFailed;
    };
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return SetupOutcome::NotInteractive;
    }

    let bin = BIN_PATH;
    eprintln!();
    eprintln!("→ launching `fono setup` as `{user}`");

    let run_status = |prog: &str, args: &[&str]| -> Option<std::process::ExitStatus> {
        Command::new(prog)
            .args(args)
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()
            .ok()
    };

    if user == "root" {
        return match run_status(bin, &["setup"]) {
            Some(s) if s.success() => SetupOutcome::Completed,
            _ => SetupOutcome::SpawnFailed,
        };
    }
    if which_in_path("runuser") {
        if let Some(s) = run_status("runuser", &["-u", &user, "--", bin, "setup"]) {
            if s.success() {
                return SetupOutcome::Completed;
            }
        }
    }
    if which_in_path("sudo") {
        if let Some(s) = run_status("sudo", &["-u", &user, bin, "setup"]) {
            if s.success() {
                return SetupOutcome::Completed;
            }
        }
    }
    SetupOutcome::SpawnFailed
}

fn which_in_path(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success() || s.code().is_some())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------
// Session-aware missing-packages prompt
// ---------------------------------------------------------------------

/// Coarse session classification used to pick which runtime packages
/// Fono recommends. Detected from environment variables — the same
/// inputs the runtime injector / overlay backends consult — so the
/// recommendation always matches what the daemon will actually do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Session {
    /// Native Wayland session under GNOME / Mutter. Mutter doesn't
    /// implement `zwp_virtual_keyboard_manager_v1` (so `wtype` types
    /// silently into the void) nor `zwlr_layer_shell_v1` (so the
    /// overlay falls through to Xwayland). Both Fono surfaces end up
    /// using the Xwayland path here — recommend `xdotool` +
    /// `libxkbcommon-x11`.
    GnomeWayland,
    /// Native Wayland session under a wlroots compositor (sway,
    /// hyprland, river, Wayfire, …). Both layer-shell and
    /// virtual-keyboard are implemented natively — recommend `wtype`
    /// only.
    WlrootsWayland,
    /// Native Wayland session under KDE Plasma's KWin. KWin
    /// implements both protocols Fono needs (Plasma 5.27+).
    KdeWayland,
    /// Pure X11 session (no `WAYLAND_DISPLAY`). Recommend `xdotool`
    /// + `libxkbcommon-x11`.
    X11,
    /// Couldn't classify (no DISPLAY, no WAYLAND_DISPLAY, or an
    /// unrecognised desktop). Default to the X11/Xwayland-leaning
    /// recommendation since it's the broadest one that works.
    Unknown,
}

impl Session {
    pub(crate) fn detect() -> Self {
        Self::detect_with(|k| std::env::var_os(k).map(|v| v.to_string_lossy().into_owned()))
    }

    /// Test seam: injectable env lookup.
    fn detect_with(env: impl Fn(&str) -> Option<String>) -> Self {
        let wayland = env("WAYLAND_DISPLAY").is_some()
            || env("XDG_SESSION_TYPE").as_deref() == Some("wayland");
        let x11 = env("DISPLAY").is_some();
        if !wayland && !x11 {
            return Self::Unknown;
        }
        if !wayland {
            return Self::X11;
        }
        let current = env("XDG_CURRENT_DESKTOP").unwrap_or_default().to_ascii_lowercase();
        let session = env("XDG_SESSION_DESKTOP").unwrap_or_default().to_ascii_lowercase();
        let hyprland = env("HYPRLAND_INSTANCE_SIGNATURE").is_some();
        let sway = env("SWAYSOCK").is_some();
        let needle = |s: &str| current.split(':').any(|p| p == s) || session == s;
        if hyprland
            || sway
            || needle("sway")
            || needle("hyprland")
            || needle("river")
            || needle("wayfire")
            || needle("niri")
            || needle("cosmic")
            || needle("labwc")
        {
            return Self::WlrootsWayland;
        }
        if needle("kde") || needle("plasma") {
            return Self::KdeWayland;
        }
        if needle("gnome") || needle("ubuntu") || needle("ubuntu:gnome") {
            return Self::GnomeWayland;
        }
        // Wayland but unrecognised compositor — be conservative and
        // recommend the Xwayland-leaning set so the user at least
        // gets typing through XWayland.
        Self::Unknown
    }

    fn label(self) -> &'static str {
        match self {
            Self::GnomeWayland => "GNOME-Wayland",
            Self::WlrootsWayland => "wlroots Wayland",
            Self::KdeWayland => "KDE-Wayland",
            Self::X11 => "X11",
            Self::Unknown => "this session",
        }
    }

    /// Packages Fono recommends installing on this session, in the
    /// order they'll be presented to the user.
    fn desired_packages(self) -> Vec<Pkg> {
        match self {
            // On GNOME-Wayland Fono defaults to clipboard delivery
            // (Ctrl+V to paste) — see `crates/fono-inject/src/inject.rs`
            // `detect_auto`. The overlay still rides Xwayland so we
            // recommend `libxkbcommon-x11`, but we deliberately do
            // NOT recommend `xdotool` here: that would trigger GNOME's
            // "Allow input emulation" permission dialog on the first
            // dictation, which is alarming for users who installed the
            // tool ten seconds earlier. Users who want one-key paste
            // run `fono use inject xdotool` to opt in.
            Self::GnomeWayland => vec![Pkg::LibxkbcommonX11],
            // Pure X11 / unknown desktops: no scary permission dialog,
            // so the auto-typing pair is fair game.
            Self::X11 | Self::Unknown => vec![Pkg::LibxkbcommonX11, Pkg::Xdotool],
            // Native Wayland with both protocols — layer-shell
            // overlay + virtual-keyboard typing work directly. No
            // Xwayland dependency on the hot path.
            Self::WlrootsWayland | Self::KdeWayland => vec![Pkg::Wtype],
        }
    }

    /// Human-readable recommendation for which keystroke-injection
    /// tool to install on this session. Used by the daemon WARN line
    /// when no injector is detected; mirrors the desired_packages
    /// rationale.
    pub(crate) fn recommend_injector(self) -> &'static str {
        match self {
            Self::GnomeWayland => {
                "Fono defaults to clipboard delivery on GNOME-Wayland to avoid \
                 GNOME's input-emulation permission dialog. Press Ctrl+V to paste. \
                 To enable one-key auto-typing (which will trigger that GNOME prompt \
                 once), run `fono use inject xdotool`."
            }
            Self::X11 | Self::Unknown => {
                "Install `xdotool` to enable auto-typing (Fono uses it via XWayland \
                 on this session). `ydotool` also works on any Wayland session but \
                 requires a uinput daemon you set up yourself."
            }
            Self::WlrootsWayland | Self::KdeWayland => {
                "Install `wtype` to enable auto-typing on this Wayland session. \
                 `ydotool` is a universal alternative but requires a uinput daemon \
                 you set up yourself."
            }
        }
    }
}

/// Runtime packages Fono knows how to detect + offer to install.
/// Deliberately a closed set: `ydotool` is intentionally excluded
/// (the binary alone isn't enough — it needs a running daemon with
/// uinput permissions, which an installer can't safely automate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pkg {
    /// `libxkbcommon-x11.so.0` — winit's X11 / Xwayland overlay
    /// backend dlopens it at runtime.
    LibxkbcommonX11,
    /// `xdotool` — Fono's key-injection backend on X11 + the
    /// XWayland fallback on every Wayland session.
    Xdotool,
    /// `wtype` — Fono's key-injection backend on Wayland
    /// compositors that implement `zwp_virtual_keyboard_manager_v1`
    /// (wlroots family + KWin).
    Wtype,
}

impl Pkg {
    fn human(self) -> &'static str {
        match self {
            Self::LibxkbcommonX11 => "libxkbcommon-x11",
            Self::Xdotool => "xdotool",
            Self::Wtype => "wtype",
        }
    }

    fn purpose(self) -> &'static str {
        match self {
            Self::LibxkbcommonX11 => "on-screen recording overlay (X11 / Xwayland backend)",
            Self::Xdotool => "auto-typing into XWayland windows",
            Self::Wtype => "auto-typing on Wayland compositors that support virtual-keyboard",
        }
    }

    /// True iff this package's runtime is already available on the
    /// host. Libraries are probed via `dlopen` (so non-standard
    /// prefixes / Nix / snap LD_LIBRARY_PATH overlays read as
    /// installed); binaries are probed via PATH lookup.
    fn present(self) -> bool {
        match self {
            Self::LibxkbcommonX11 => libxkbcommon_x11_loadable(),
            Self::Xdotool => which_in_path("xdotool"),
            Self::Wtype => which_in_path("wtype"),
        }
    }

    /// Per-PM package name for this runtime. Every known
    /// (Pkg, PkgManager) pair currently resolves; if we ever add a
    /// PM that genuinely doesn't carry one, switch this back to
    /// `Option<&'static str>` and have the caller drop the entry.
    fn package_name_on(self, pm: PkgManager) -> &'static str {
        match (self, pm) {
            (Self::LibxkbcommonX11, PkgManager::Apt | PkgManager::Zypper) => "libxkbcommon-x11-0",
            (Self::LibxkbcommonX11, PkgManager::Dnf | PkgManager::ApkAlpine) => "libxkbcommon-x11",
            // pacman: ships in `libxkbcommon`; user normally has it
            // already (it's a Hyprland / Sway dep) so the prompt is
            // a no-op when present. We still offer it for parity.
            (Self::LibxkbcommonX11, PkgManager::Pacman) => "libxkbcommon",
            (Self::Xdotool, _) => "xdotool",
            (Self::Wtype, _) => "wtype",
        }
    }
}

/// Distros where Fono knows how to drive the package manager. Slackware
/// is deliberately not included: `libxkbcommon-x11` ships inside the
/// core `libxkbcommon` package there, `xdotool` / `wtype` live in
/// SBo, and pulling SBo's build chain on every NimbleX user is the
/// wrong tradeoff. Slackware users are documented in
/// `packaging/slackbuild/fono/README` instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PkgManager {
    Apt,
    Dnf,
    Pacman,
    Zypper,
    ApkAlpine,
}

impl PkgManager {
    /// Build an install command for the supplied set of packages.
    /// Order in `pkgs` is preserved on the command line so the
    /// presented order matches the printed prompt.
    fn install_command(self, pkgs: &[&'static str]) -> (&'static str, Vec<String>) {
        let mut args: Vec<String> = match self {
            Self::Apt => vec!["install".into(), "-y".into()],
            Self::Dnf => vec!["install".into(), "-y".into()],
            Self::Pacman => vec!["-S".into(), "--noconfirm".into(), "--needed".into()],
            Self::Zypper => vec!["--non-interactive".into(), "install".into()],
            Self::ApkAlpine => vec!["add".into()],
        };
        for p in pkgs {
            args.push((*p).into());
        }
        let prog = match self {
            Self::Apt => "apt-get",
            Self::Dnf => "dnf",
            Self::Pacman => "pacman",
            Self::Zypper => "zypper",
            Self::ApkAlpine => "apk",
        };
        (prog, args)
    }
}

/// Probe for `libxkbcommon-x11.so.0` via `dlopen`. We test the loader
/// rather than scanning `/usr/lib/**`, so non-standard prefixes (Nix,
/// snap LD_LIBRARY_PATH overlays) read as installed.
fn libxkbcommon_x11_loadable() -> bool {
    // SAFETY: Library::new is sound; the handle drops immediately.
    unsafe {
        libloading::Library::new("libxkbcommon-x11.so.0").is_ok()
            || libloading::Library::new("libxkbcommon-x11.so").is_ok()
    }
}

/// Detect Slackware (which bundles libxkbcommon-x11 inside the core
/// `libxkbcommon` package and tracks xdotool / wtype via SBo). On
/// those systems Fono should not second-guess the distro's packaging.
fn is_slackware() -> bool {
    Path::new("/etc/slackware-version").exists()
}

/// Pick the first recognised package manager on the host.
fn detect_pkg_manager() -> Option<PkgManager> {
    if which_in_path("apt-get") {
        Some(PkgManager::Apt)
    } else if which_in_path("dnf") {
        Some(PkgManager::Dnf)
    } else if which_in_path("pacman") {
        Some(PkgManager::Pacman)
    } else if which_in_path("zypper") {
        Some(PkgManager::Zypper)
    } else if which_in_path("apk") {
        Some(PkgManager::ApkAlpine)
    } else {
        None
    }
}

/// Format an install command as a single shell-quotable string for
/// printing. `prog` is never user-controlled (one of five known PM
/// binaries) so the quoting can be trivial.
fn format_install_cmd(prog: &str, args: &[String]) -> String {
    let joined = args.join(" ");
    format!("sudo {prog} {joined}")
}

/// Session-aware preflight that offers to install the runtime
/// packages Fono needs on the detected session. Stays silent when
/// nothing is missing, when running on Slackware, or when the
/// package manager isn't one we know how to drive. Honours the
/// stdin/stdout TTY gate (non-interactive shells get a hint line
/// instead of a prompt).
fn offer_install_missing_packages() {
    use std::io::{BufRead, IsTerminal};

    if is_slackware() {
        return;
    }
    let session = Session::detect();
    let desired = session.desired_packages();
    let missing: Vec<Pkg> = desired.into_iter().filter(|p| !p.present()).collect();
    if missing.is_empty() {
        return;
    }

    let Some(pm) = detect_pkg_manager() else {
        eprintln!();
        eprintln!("Note: Fono recommends installing the following on {}:", session.label());
        for p in &missing {
            eprintln!("  · {} — {}", p.human(), p.purpose());
        }
        eprintln!("  (no recognised package manager on this host; install via your distro)");
        return;
    };

    // Resolve PM-specific names; drop anything the PM doesn't carry.
    let names: Vec<&'static str> = missing.iter().map(|p| p.package_name_on(pm)).collect();
    let (prog, args) = pm.install_command(&names);
    let cmd = format_install_cmd(prog, &args);

    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        eprintln!();
        eprintln!("Note: Fono recommends installing the following on {}:", session.label());
        for p in &missing {
            eprintln!("  · {} — {}", p.human(), p.purpose());
        }
        eprintln!("Install later with: {cmd}");
        return;
    }

    eprintln!();
    eprintln!("Fono detected a {} session and recommends installing:", session.label());
    for p in &missing {
        eprintln!("  · {} — {}", p.human(), p.purpose());
    }
    eprintln!("Dictation still works without these, but auto-typing and/or the on-screen");
    eprintln!("overlay will be degraded.");
    eprint!("Install now via `{cmd}`? [Y/n] ");
    let _ = std::io::Write::flush(&mut std::io::stderr());

    let mut line = String::new();
    let stdin = std::io::stdin();
    if stdin.lock().read_line(&mut line).is_err() {
        eprintln!("(could not read answer — skipping)");
        return;
    }
    let answer = line.trim().to_ascii_lowercase();
    if !(answer.is_empty() || answer == "y" || answer == "yes") {
        eprintln!("Skipping. You can install them later with: {cmd}");
        return;
    }

    eprintln!("→ running: {prog} {}", args.join(" "));
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let status = Command::new(prog).args(&arg_refs).stdin(std::process::Stdio::null()).status();
    match status {
        Ok(s) if s.success() => {
            for p in &missing {
                eprintln!("  · installed {}", p.human());
            }
        }
        Ok(s) => {
            eprintln!(
                "  · {prog} exited with status {} — install the packages manually if you want full functionality",
                s.code().map_or_else(|| "<signal>".into(), |c| c.to_string())
            );
        }
        Err(e) => {
            eprintln!("  · failed to spawn {prog}: {e}");
        }
    }
}

// ---------------------------------------------------------------------
// Server install
// ---------------------------------------------------------------------

fn run_install_server() -> Result<()> {
    // `created_user` is informational only; uninstall infers the user's
    // existence from `getent passwd` and its shell, so we don't need
    // to persist a "we created it" flag anywhere.
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
    eprintln!("  · {BIN_PATH}");

    // Systemd unit
    write_atomic(Path::new(SYSTEMD_UNIT), SYSTEMD_SYSTEM_UNIT.as_bytes(), 0o644)?;
    eprintln!("  · {SYSTEMD_UNIT}");

    // Server config — seed /etc/fono/config.toml when absent so the
    // daemon comes up with the Wyoming listener already enabled on
    // 0.0.0.0:10300. Existing operator configs are left alone.
    let seeded = seed_server_config()?;

    // Completions
    write_completions(BIN_PATH);

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

    if enabled_service {
        verify_service_running("fono.service");
        if seeded {
            // The seeded config binds Wyoming on 0.0.0.0:10300; probe
            // 127.0.0.1:10300 (always reachable from the installer's
            // own host regardless of the bind value) to confirm the
            // listener actually came up. The verify_service_running
            // call above already slept 2 s, so the daemon has had
            // time to spawn the server task by the time we get here.
            verify_wyoming_listener("127.0.0.1:10300");
        }
    }

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
    if seeded {
        println!("Wyoming STT server: listening on 0.0.0.0:10300");
        println!();
        println!("To restrict edit {SYSTEM_CONFIG_FILE}");
    } else {
        println!("Existing {SYSTEM_CONFIG_FILE} left in place. If you want the");
        println!("Wyoming server to listen on the LAN, set");
        println!("`[server.wyoming].enabled = true` and `bind = \"0.0.0.0\"` there,");
        println!("then `sudo systemctl restart fono.service`.");
    }
    println!();
    println!("Service config lives under /etc/fono/, state under /var/lib/fono/,");
    println!("cache under /var/cache/fono/. See docs/providers.md for Wyoming");
    println!("server configuration.");
    Ok(())
}

/// Seed `/etc/fono/config.toml` with the embedded
/// [`SERVER_CONFIG_SEED`] when no config exists yet, and ensure the
/// containing directory has the right ownership / mode for the daemon
/// (`root:fono 0750`).
///
/// Returns `Ok(true)` when the seed was written, `Ok(false)` when an
/// existing config was preserved. Never overwrites operator state.
fn seed_server_config() -> Result<bool> {
    let (uid, gid) = resolve_system_user_ids(SERVICE_USER)
        .with_context(|| format!("look up uid/gid for `{SERVICE_USER}`"))?;

    // Ensure /etc/fono exists with root:fono 0750. systemd's
    // ConfigurationDirectory= would normally do this on service
    // start, but we haven't started the unit yet at this point.
    let dir = Path::new(SYSTEM_CONFIG_DIR);
    std::fs::create_dir_all(dir).with_context(|| format!("mkdir -p {}", dir.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o750))
            .with_context(|| format!("chmod 0750 {}", dir.display()))?;
    }
    chown_path(dir, 0, gid)
        .with_context(|| format!("chown root:{SERVICE_USER} {}", dir.display()))?;

    let path = Path::new(SYSTEM_CONFIG_FILE);
    if path.exists() {
        eprintln!("  · {SYSTEM_CONFIG_FILE} already present — leaving it alone");
        return Ok(false);
    }
    write_atomic_owned(path, SERVER_CONFIG_SEED.as_bytes(), 0o640, 0, gid)
        .with_context(|| format!("seed {SYSTEM_CONFIG_FILE}"))?;
    // Silence unused-variable warning on cfg(not(unix)) test runs; the
    // uid we need is root (0), but we resolved fono's uid above to
    // confirm the user exists before we wrote the file.
    let _ = uid;
    eprintln!("  · {SYSTEM_CONFIG_FILE} (seeded: Wyoming STT server on 0.0.0.0:10300)");
    Ok(true)
}

/// Post-install TCP probe to confirm the Wyoming listener actually
/// bound. `systemctl is-active` reports `active` the moment systemd
/// successfully spawned the process — it can't tell us whether the
/// daemon's internal server task got far enough to `listen(2)`.
/// Without this probe, a server install that silently fails to bind
/// (port in use, bind address invalid, panic in the server task) ends
/// up looking healthy at install time and the operator only discovers
/// it via the cryptic client-side error.
fn verify_wyoming_listener(addr: &str) {
    use std::net::ToSocketAddrs;
    use std::time::Duration;

    let Some(socket) = addr.to_socket_addrs().ok().and_then(|mut it| it.next()) else {
        eprintln!("  · could not parse Wyoming probe address {addr}");
        return;
    };
    match std::net::TcpStream::connect_timeout(&socket, Duration::from_secs(2)) {
        Ok(_) => eprintln!("  · Wyoming STT server reachable on {addr} (TCP probe OK)"),
        Err(e) => {
            eprintln!(
                "  · Wyoming STT server NOT reachable on {addr} ({e}); \
                 inspect with `journalctl -u fono.service -n 50 --no-pager`"
            );
        }
    }
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

fn write_completions(bin_path: &str) {
    for (shell, dst) in
        [("bash", COMPLETION_BASH), ("zsh", COMPLETION_ZSH), ("fish", COMPLETION_FISH)]
    {
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
            tracing::debug!(shell, "skipping completion: {} missing", grandparent.display());
            continue;
        }

        match Command::new(bin_path).args(["completions", shell]).output() {
            Ok(out) if out.status.success() && !out.stdout.is_empty() => {
                if let Err(e) = write_atomic(dst_path, &out.stdout, 0o644) {
                    tracing::warn!(shell, "writing {dst} failed: {e:#}");
                } else {
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

fn run_uninstall_real(state: &InstallState) {
    eprintln!("→ uninstalling fono ({} mode)", state.mode.as_str());

    if state.mode == Mode::Server && state.enabled_service && systemctl_available() {
        let _ = try_run("systemctl", &["disable", "--now", "fono.service"]);
        eprintln!("  · systemctl disable --now fono.service");
    }

    // Already in removal order.
    for f in &state.files {
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

    if state.mode == Mode::Server && systemctl_available() {
        let _ = try_run("systemctl", &["daemon-reload"]);
    }
    if state.mode == Mode::Server {
        // Reproducible system cache: Whisper / Sherpa / polish model
        // weights, hwcheck JSON, downloaded archives — mirrors what
        // the desktop branch wipes below from ~/.cache/fono. Safe to
        // remove: re-downloaded automatically the next time a model
        // is requested. Leaving multi-GB blobs under /var/cache after
        // an explicit uninstall is the kind of surprise the desktop
        // branch already learned to avoid.
        let cache = Path::new(SYSTEM_CACHE_DIR);
        if cache.exists() {
            match std::fs::remove_dir_all(cache) {
                Ok(()) => eprintln!("  · removed {SYSTEM_CACHE_DIR}"),
                Err(e) => {
                    tracing::warn!(path = SYSTEM_CACHE_DIR, "remove_dir_all failed: {e:#}");
                    eprintln!(
                        "  · could not remove {SYSTEM_CACHE_DIR} ({e}); delete manually if no longer needed"
                    );
                }
            }
        }
    }
    if state.mode == Mode::Desktop {
        refresh_desktop_database();
        refresh_icon_cache();
        // Reproducible per-user cache: Whisper / Sherpa / polish model
        // weights, hwcheck JSON, downloaded archives. Safe to wipe —
        // re-downloaded on next `fono setup` — and removing it here
        // avoids leaving multi-GB orphans under the user's home after
        // `sudo fono uninstall`. Honour the original feedback:
        // "uninstall should also remove .cache/fono".
        if let Some(cache) = caller_cache_dir() {
            if cache.exists() {
                match std::fs::remove_dir_all(&cache) {
                    Ok(()) => eprintln!("  · removed {}", cache.display()),
                    Err(e) => {
                        tracing::warn!(
                            path = %cache.display(),
                            "remove_dir_all failed: {e:#}"
                        );
                        eprintln!(
                            "  · could not remove {} ({e}); delete manually if no longer needed",
                            cache.display()
                        );
                    }
                }
            }
        }
    }

    if state.mode == Mode::Server && state.system_user_removable {
        if service_state_remaining() {
            eprintln!(
                "  · keeping system user `{SERVICE_USER}` (state remains under /etc/fono or /var/lib/fono)"
            );
        } else if try_run("userdel", &[SERVICE_USER]) {
            eprintln!("  · removed system user `{SERVICE_USER}`");
        } else {
            tracing::warn!("userdel {SERVICE_USER} failed; remove manually if no longer needed");
        }
    }

    println!();
    println!("Fono uninstalled.");
    if state.mode == Mode::Desktop {
        println!("Per-user config (~/.config/fono) and history (~/.local/share/fono)");
        println!("are kept and belong to the user. The reproducible cache");
        println!("(~/.cache/fono) was removed.");
    } else {
        println!("Service config (/etc/fono) and state (/var/lib/fono) are kept");
        println!("for a future re-install. The reproducible cache (/var/cache/fono)");
        println!("was removed.");
    }
}

fn service_state_remaining() -> bool {
    // /var/cache/fono is removed by run_uninstall_real before this is
    // consulted, so checking it would always read `false` here — keep
    // it out of the predicate.
    ["/etc/fono", "/var/lib/fono"].iter().any(|p| Path::new(p).exists())
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
        for t in [BIN_PATH, DESKTOP_MENU, DESKTOP_AUTOSTART, ICON_PATH, COMPLETION_BASH] {
            assert!(joined.contains(t), "desktop plan missing {t}");
        }
        assert!(!joined.contains(SYSTEMD_UNIT));
        assert!(!joined.contains("install_marker"));
    }

    #[test]
    fn build_server_plan_lists_all_targets() {
        let plan = build_install_plan(true);
        assert_eq!(plan.mode, Some(Mode::Server));
        let joined = plan.steps.join("\n");
        for t in [BIN_PATH, SYSTEMD_UNIT, SYSTEM_CONFIG_FILE, COMPLETION_BASH] {
            assert!(joined.contains(t), "server plan missing {t}");
        }
        assert!(joined.contains("useradd"));
        assert!(joined.contains("systemctl enable"));
        assert!(joined.contains("0.0.0.0:10300"), "server plan should preview Wyoming bind addr");
        assert!(!joined.contains(DESKTOP_AUTOSTART));
        assert!(!joined.contains(ICON_PATH));
        assert!(!joined.contains("install_marker"));
    }

    #[test]
    fn embedded_server_config_seed_is_valid_toml() {
        // Round-trip the embedded seed through the real Config schema
        // so any future schema drift (renamed field, type change)
        // fails this test the moment it lands rather than at install
        // time on someone's server.
        let cfg: fono_core::config::Config =
            toml::from_str(SERVER_CONFIG_SEED).expect("seed parses as fono_core::config::Config");
        assert!(cfg.server.wyoming.enabled, "seed must enable Wyoming listener");
        assert_eq!(cfg.server.wyoming.bind, "0.0.0.0");
        assert_eq!(cfg.server.wyoming.port, 10_300);
    }

    #[test]
    fn embedded_server_config_seed_has_security_note() {
        // The operator's first edit destination is the seeded file
        // itself; the inline comment must spell out the security
        // tradeoff so they understand why bind = 0.0.0.0 and how to
        // restrict it.
        assert!(SERVER_CONFIG_SEED.contains("0.0.0.0"));
        assert!(
            SERVER_CONFIG_SEED.lines().any(|l| {
                let t = l.trim_start();
                t.starts_with('#') && t.to_ascii_lowercase().contains("auth")
            }),
            "seed must contain a `#` comment mentioning auth"
        );
    }

    #[test]
    fn detect_install_state_on_clean_system_is_empty() {
        // Running tests inside a sandbox where none of the canonical
        // install paths exist — the detector must report "nothing".
        // (CI containers and dev boxes both satisfy this; if a tester
        // somehow has a real install on /usr/local/bin/fono this
        // assertion will fire and they'll know to uninstall first.)
        let state = detect_install_state();
        if Path::new(BIN_PATH).exists() || Path::new(SYSTEMD_UNIT).exists() {
            // A real install is present — skip rather than mis-fail.
            return;
        }
        assert!(state.files.is_empty());
        assert!(!state.enabled_service);
        assert!(!state.system_user_removable);
    }

    #[test]
    fn embedded_assets_nonempty() {
        assert!(DESKTOP.contains("Exec=fono"));
        assert!(SYSTEMD_SYSTEM_UNIT.contains("ExecStart=/usr/local/bin/fono"));
        assert!(SYSTEMD_SYSTEM_UNIT.contains("User=fono"));
        // SVG must contain the SVG opening tag (catches accidental
        // truncation; can't use is_empty since the slice is a const).
        assert!(std::str::from_utf8(ICON_SVG).unwrap_or("").contains("<svg"));
    }

    fn env_from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| pairs.iter().find(|(p, _)| *p == k).map(|(_, v)| (*v).to_owned())
    }

    #[test]
    fn session_detects_gnome_wayland_from_ubuntu_default() {
        // Ubuntu 24.04 GNOME-Wayland surface — exactly the case that
        // motivated this work.
        let s = Session::detect_with(env_from(&[
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("DISPLAY", ":0"),
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "ubuntu:GNOME"),
            ("XDG_SESSION_DESKTOP", "ubuntu"),
        ]));
        assert_eq!(s, Session::GnomeWayland);
        // GNOME-Wayland intentionally does NOT auto-offer xdotool: typing
        // via XTEST triggers GNOME's "Allow input emulation" permission
        // dialog, which scares evaluators who didn't ask for it. Users
        // who want auto-typing opt in via `fono use inject xdotool`.
        assert_eq!(s.desired_packages(), vec![Pkg::LibxkbcommonX11]);
    }

    #[test]
    fn session_detects_wlroots_from_sway_socket() {
        let s = Session::detect_with(env_from(&[
            ("WAYLAND_DISPLAY", "wayland-1"),
            ("SWAYSOCK", "/run/user/1000/sway-ipc.1000.42.sock"),
            ("XDG_CURRENT_DESKTOP", "sway"),
        ]));
        assert_eq!(s, Session::WlrootsWayland);
        assert_eq!(s.desired_packages(), vec![Pkg::Wtype]);
    }

    #[test]
    fn session_detects_wlroots_from_hyprland_signature() {
        let s = Session::detect_with(env_from(&[
            ("WAYLAND_DISPLAY", "wayland-1"),
            ("HYPRLAND_INSTANCE_SIGNATURE", "deadbeef"),
        ]));
        assert_eq!(s, Session::WlrootsWayland);
    }

    #[test]
    fn session_detects_kde_wayland() {
        let s = Session::detect_with(env_from(&[
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("XDG_CURRENT_DESKTOP", "KDE"),
            ("XDG_SESSION_TYPE", "wayland"),
        ]));
        assert_eq!(s, Session::KdeWayland);
        assert_eq!(s.desired_packages(), vec![Pkg::Wtype]);
    }

    #[test]
    fn session_detects_pure_x11() {
        let s = Session::detect_with(env_from(&[("DISPLAY", ":0")]));
        assert_eq!(s, Session::X11);
        assert_eq!(s.desired_packages(), vec![Pkg::LibxkbcommonX11, Pkg::Xdotool]);
    }

    #[test]
    fn session_unknown_when_no_display_vars() {
        let s = Session::detect_with(env_from(&[]));
        assert_eq!(s, Session::Unknown);
    }

    #[test]
    fn pkg_names_per_manager() {
        // Apt / Zypper carry the dash-zero suffixed lib name.
        assert_eq!(Pkg::LibxkbcommonX11.package_name_on(PkgManager::Apt), "libxkbcommon-x11-0");
        assert_eq!(Pkg::LibxkbcommonX11.package_name_on(PkgManager::Zypper), "libxkbcommon-x11-0");
        // DNF / Alpine use the unsuffixed name.
        assert_eq!(Pkg::LibxkbcommonX11.package_name_on(PkgManager::Dnf), "libxkbcommon-x11");
        assert_eq!(Pkg::LibxkbcommonX11.package_name_on(PkgManager::ApkAlpine), "libxkbcommon-x11");
        // Pacman ships it inside libxkbcommon.
        assert_eq!(Pkg::LibxkbcommonX11.package_name_on(PkgManager::Pacman), "libxkbcommon");
        // Binaries have the same upstream name everywhere.
        for pm in [
            PkgManager::Apt,
            PkgManager::Dnf,
            PkgManager::Pacman,
            PkgManager::Zypper,
            PkgManager::ApkAlpine,
        ] {
            assert_eq!(Pkg::Xdotool.package_name_on(pm), "xdotool");
            assert_eq!(Pkg::Wtype.package_name_on(pm), "wtype");
        }
    }

    #[test]
    fn install_command_quotes_packages_in_order() {
        let (prog, args) = PkgManager::Apt.install_command(&["libxkbcommon-x11-0", "xdotool"]);
        assert_eq!(prog, "apt-get");
        assert_eq!(args, vec!["install", "-y", "libxkbcommon-x11-0", "xdotool"]);
    }

    #[test]
    fn recommend_injector_mentions_xdotool_on_gnome_wayland() {
        assert!(Session::GnomeWayland.recommend_injector().contains("xdotool"));
        assert!(Session::X11.recommend_injector().contains("xdotool"));
        assert!(Session::WlrootsWayland.recommend_injector().contains("wtype"));
        assert!(Session::KdeWayland.recommend_injector().contains("wtype"));
    }

    // -----------------------------------------------------------------
    // Headless detection
    // -----------------------------------------------------------------

    use std::collections::{HashMap, HashSet};

    #[derive(Default)]
    struct FakeProbes {
        env: HashMap<String, String>,
        runs: HashMap<(String, Vec<String>), (bool, String)>,
        dirs: HashSet<(String, String)>,
        wayland_socket: bool,
    }

    impl FakeProbes {
        fn with_env(mut self, k: &str, v: &str) -> Self {
            self.env.insert(k.into(), v.into());
            self
        }
        fn with_run(mut self, prog: &str, args: &[&str], ok: bool, out: &str) -> Self {
            let key = (prog.into(), args.iter().map(|s| (*s).to_string()).collect());
            self.runs.insert(key, (ok, out.into()));
            self
        }
        fn with_dir_entry(mut self, dir: &str, prefix: &str) -> Self {
            self.dirs.insert((dir.into(), prefix.into()));
            self
        }
        fn with_wayland_socket(mut self) -> Self {
            self.wayland_socket = true;
            self
        }
    }

    impl HeadlessProbes for FakeProbes {
        fn env(&self, k: &str) -> Option<String> {
            self.env.get(k).cloned()
        }
        fn run(&self, prog: &str, args: &[&str]) -> Option<(bool, String)> {
            let key = (prog.to_string(), args.iter().map(|s| (*s).to_string()).collect::<Vec<_>>());
            self.runs.get(&key).cloned()
        }
        fn dir_has_entry(&self, dir: &Path, prefix: &str) -> bool {
            self.dirs.contains(&(dir.display().to_string(), prefix.into()))
        }
        fn any_user_runtime_wayland_socket(&self) -> bool {
            self.wayland_socket
        }
    }

    #[test]
    fn headless_false_when_caller_has_display() {
        let p = FakeProbes::default().with_env("DISPLAY", ":0");
        let (h, reason) = detect_headless_with(&p);
        assert!(!h);
        assert!(reason.contains("DISPLAY"));
    }

    #[test]
    fn headless_false_when_caller_has_wayland_display() {
        let p = FakeProbes::default().with_env("WAYLAND_DISPLAY", "wayland-0");
        let (h, _) = detect_headless_with(&p);
        assert!(!h);
    }

    #[test]
    fn headless_false_with_active_wayland_loginctl_session() {
        let p = FakeProbes::default()
            .with_run(
                "loginctl",
                &["list-sessions", "--no-legend", "--no-pager"],
                true,
                "5 1000 alice seat0 -\n",
            )
            .with_run(
                "loginctl",
                &["show-session", "5", "--property=Type", "--property=State", "--property=Class"],
                true,
                "Type=wayland\nState=active\nClass=user\n",
            );
        let (h, reason) = detect_headless_with(&p);
        assert!(!h);
        assert!(reason.contains("loginctl"));
    }

    #[test]
    fn headless_false_when_display_manager_active() {
        // No loginctl, no env — gdm.service alone should flip it.
        let p = FakeProbes::default().with_run(
            "systemctl",
            &["is-active", "gdm.service"],
            true,
            "active\n",
        );
        let (h, reason) = detect_headless_with(&p);
        assert!(!h);
        assert!(reason.contains("display manager"));
    }

    #[test]
    fn headless_false_when_x11_socket_present() {
        let p = FakeProbes::default().with_dir_entry("/tmp/.X11-unix", "X");
        let (h, reason) = detect_headless_with(&p);
        assert!(!h);
        assert!(reason.contains("X11"));
    }

    #[test]
    fn headless_false_when_wayland_socket_present_under_run_user() {
        let p = FakeProbes::default().with_wayland_socket();
        let (h, reason) = detect_headless_with(&p);
        assert!(!h);
        assert!(reason.contains("Wayland"));
    }

    #[test]
    fn headless_true_when_multi_user_target_default() {
        let p = FakeProbes::default().with_run(
            "systemctl",
            &["get-default"],
            true,
            "multi-user.target\n",
        );
        let (h, reason) = detect_headless_with(&p);
        assert!(h);
        assert!(reason.contains("multi-user"));
    }

    #[test]
    fn headless_true_when_graphical_target_default_but_no_session() {
        // Server box that inherited `graphical.target` from a desktop
        // base image (common on Ubuntu/Debian server installs) but
        // is run headless — no DISPLAY, no graphical loginctl
        // session, no DM active, no sockets. Must classify as
        // headless: get-default describes what *would* boot, not
        // what's running. Regression for the 2026-05-22 .74 bug.
        let p = FakeProbes::default().with_run(
            "systemctl",
            &["get-default"],
            true,
            "graphical.target\n",
        );
        let (h, reason) = detect_headless_with(&p);
        assert!(h);
        assert!(reason.contains("no graphical session"));
    }

    #[test]
    fn headless_true_when_no_systemd_and_no_graphical_signals() {
        // Empty probe set — no env, no commands succeed, no sockets.
        // On the supported Linux targets this means "no systemd and
        // no graphical session", which is a confident headless verdict.
        let p = FakeProbes::default();
        let (h, reason) = detect_headless_with(&p);
        assert!(h);
        assert!(reason.contains("no systemd"));
    }

    #[test]
    fn headless_ignores_inactive_loginctl_sessions() {
        // Closing session (state=closing) should not count as desktop.
        let p = FakeProbes::default()
            .with_run(
                "loginctl",
                &["list-sessions", "--no-legend", "--no-pager"],
                true,
                "5 1000 alice seat0 -\n",
            )
            .with_run(
                "loginctl",
                &["show-session", "5", "--property=Type", "--property=State", "--property=Class"],
                true,
                "Type=wayland\nState=closing\nClass=user\n",
            )
            .with_run("systemctl", &["get-default"], true, "multi-user.target\n");
        let (h, _) = detect_headless_with(&p);
        assert!(h);
    }
}
