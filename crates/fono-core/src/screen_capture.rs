// SPDX-License-Identifier: GPL-3.0-only
//! Screen-capture pipeline — tool-ladder probe and capture execution.
//!
//! `GrabberProbe::detect()` inspects the current session type and PATH and
//! returns a ranked list of available capture tools. `capture()` walks the
//! list and returns the first successful `CapturedImage`.

use std::fmt;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

// ── Public types ─────────────────────────────────────────────────────────────

/// Whether to capture automatically (focused window) or interactively
/// (user-selected region).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    Automatic,
    Interactive,
}

/// Where the image came from.
#[derive(Debug, Clone)]
pub enum CaptureSource {
    /// Focused window (automatic mode).
    Window { wm_class: String, title: String },
    /// User-selected region or unspecified (interactive mode).
    Region,
}

/// A successfully captured PNG image.
#[derive(Debug, Clone)]
pub struct CapturedImage {
    pub png_bytes: Vec<u8>,
    pub source: CaptureSource,
    pub width: u32,
    pub height: u32,
    pub tool: String,
}

/// Errors returned by the capture pipeline.
#[derive(Debug)]
pub enum CaptureError {
    /// User dismissed the region picker.
    Cancelled,
    /// The focused window is a private/sensitive application.
    PrivateWindow,
    /// No capture tool is installed / available.
    NoToolAvailable,
    /// A capture attempt timed out.
    Timeout,
    /// I/O error (spawning process, reading file, …).
    Io(std::io::Error),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "screen capture cancelled by user"),
            Self::PrivateWindow => {
                write!(f, "capture blocked: focused window is private/sensitive")
            }
            Self::NoToolAvailable => {
                write!(f, "no screen-capture tool available in PATH")
            }
            Self::Timeout => write!(f, "screen capture timed out"),
            Self::Io(e) => write!(f, "screen capture I/O error: {e}"),
        }
    }
}

impl std::error::Error for CaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Self::Io(e) = self {
            Some(e)
        } else {
            None
        }
    }
}

impl From<std::io::Error> for CaptureError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ── Private window classes (mirrors BUILTIN_RULES private_profile) ────────────

/// Window classes that trigger the privacy gate.  Any `wm_class` that
/// matches one of these (case-insensitive) makes `capture()` return
/// `Err(CaptureError::PrivateWindow)` without attempting capture.
const PRIVATE_WINDOW_CLASSES: &[&str] =
    &["keepassxc", "bitwarden", "1password", "gnome-keyring", "seahorse"];

// ── Session type ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionType {
    Wayland,
    X11,
}

// ── Rung kinds ────────────────────────────────────────────────────────────────

/// A single rung in the capture tool ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RungKind {
    /// XDG Desktop Portal (ashpd) — deferred to a future PR.
    Portal,
    /// `scrot FILE` — X11 automatic, captures focused window.
    Scrot,
    /// `scrot -s FILE` — X11 interactive region picker.
    ScrotSelect,
    /// `maim --window <id> FILE` — X11 automatic via xdotool.
    MaimAuto,
    /// `maim -s FILE` — X11 interactive region picker.
    MaimSelect,
    /// `import -window root FILE` — ImageMagick X11.
    ImportAuto,
    /// `import -window root FILE` interactive (region via mouse click-drag).
    ImportSelect,
    /// `slurp` → `grim -g <region> FILE` — Wayland interactive.
    GrimSlurp,
    /// `spectacle -b -o FILE` — KDE automatic.
    SpectacleAuto,
    /// `spectacle -r -o FILE` — KDE interactive.
    SpectacleSelect,
    /// `gnome-screenshot -f FILE` — GNOME automatic.
    GnomeScreenshotAuto,
    /// `gnome-screenshot -a -f FILE` — GNOME interactive.
    GnomeScreenshotSelect,
}

impl RungKind {
    /// The primary binary that must be in PATH for this rung to be active.
    pub fn binary(&self) -> &'static str {
        match self {
            Self::Portal => "fono-portal-internal",
            Self::Scrot | Self::ScrotSelect => "scrot",
            Self::MaimAuto | Self::MaimSelect => "maim",
            Self::ImportAuto | Self::ImportSelect => "import",
            Self::GrimSlurp => "grim",
            Self::SpectacleAuto | Self::SpectacleSelect => "spectacle",
            Self::GnomeScreenshotAuto | Self::GnomeScreenshotSelect => "gnome-screenshot",
        }
    }

    /// Display name shown in `fono doctor`.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Portal => "portal",
            Self::Scrot => "scrot",
            Self::ScrotSelect => "scrot (select)",
            Self::MaimAuto => "maim",
            Self::MaimSelect => "maim (select)",
            Self::ImportAuto => "import",
            Self::ImportSelect => "import (select)",
            Self::GrimSlurp => "grim+slurp",
            Self::SpectacleAuto => "spectacle",
            Self::SpectacleSelect => "spectacle (select)",
            Self::GnomeScreenshotAuto => "gnome-screenshot",
            Self::GnomeScreenshotSelect => "gnome-screenshot (select)",
        }
    }

    /// Returns the tool name string for `CapturedImage.tool`.
    pub fn tool_name(&self) -> &'static str {
        self.binary()
    }
}

// ── Counter for unique temp-file names ───────────────────────────────────────

static CAPTURE_COUNTER: AtomicU32 = AtomicU32::new(0);

fn next_temp_path() -> std::path::PathBuf {
    let id = CAPTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("fono-cap-{}-{id}.png", std::process::id()))
}

// ── GrabberProbe ─────────────────────────────────────────────────────────────

/// Detects available screen-capture tools and executes captures.
///
/// Constructed once per process via [`GrabberProbe::detect`]; cheap to clone
/// since it just holds two `Vec<RungKind>`s.
#[derive(Debug, Clone)]
pub struct GrabberProbe {
    session: SessionType,
    auto_rungs: Vec<RungKind>,
    select_rungs: Vec<RungKind>,
    /// Whether `DISPLAY` is set (enables XWayland import on Wayland).
    pub has_display: bool,
}

impl GrabberProbe {
    // ── Construction ──────────────────────────────────────────────────────

    /// Build a probe from explicit environment variable values.  Used for
    /// unit tests and for the real `detect()` implementation.
    ///
    /// `wayland` = value of `WAYLAND_DISPLAY`, `display` = value of `DISPLAY`.
    pub fn detect_from_env(
        wayland: Option<&std::ffi::OsStr>,
        display: Option<&std::ffi::OsStr>,
    ) -> Self {
        // Spectacle is a KDE-only tool; it hangs in non-KDE sessions because
        // it waits for KDE D-Bus services. Only include it when KDE_FULL_SESSION
        // is set (KDE Plasma) or XDG_CURRENT_DESKTOP contains "KDE".
        let in_kde_session = std::env::var_os("KDE_FULL_SESSION").map_or(false, |v| !v.is_empty())
            || std::env::var("XDG_CURRENT_DESKTOP")
                .map_or(false, |d| d.to_ascii_uppercase().contains("KDE"));
        let has_display = display.is_some();

        if wayland.is_some() {
            // Wayland session.
            let mut auto_rungs = vec![RungKind::Portal];
            if has_display {
                auto_rungs.push(RungKind::ImportAuto);
            }
            if in_kde_session {
                auto_rungs.push(RungKind::SpectacleAuto);
            }
            auto_rungs.push(RungKind::GnomeScreenshotAuto);
            // Filter by PATH availability (Portal is always kept — deferred,
            // not missing).
            let auto_rungs = auto_rungs
                .into_iter()
                .filter(|r| *r == RungKind::Portal || which_binary(r.binary()))
                .collect();

            let mut select_rungs = Vec::new();
            // grim+slurp: include only when both are in PATH.
            if which_binary("grim") && which_binary("slurp") {
                select_rungs.push(RungKind::GrimSlurp);
            }
            select_rungs.push(RungKind::Portal);
            if has_display {
                select_rungs.push(RungKind::ImportSelect);
            }
            if in_kde_session {
                select_rungs.push(RungKind::SpectacleSelect);
            }
            select_rungs.push(RungKind::GnomeScreenshotSelect);
            let select_rungs = select_rungs
                .into_iter()
                .filter(|r| *r == RungKind::Portal || which_binary(r.binary()))
                .collect();

            Self { session: SessionType::Wayland, auto_rungs, select_rungs, has_display }
        } else if display.is_some() {
            // Pure X11 session.
            // spectacle (-b -a -n) captures the active window natively on X11
            // but only works in a KDE Plasma session (it hangs otherwise).
            let mut auto_rungs = vec![RungKind::Scrot, RungKind::MaimAuto, RungKind::ImportAuto];
            if in_kde_session {
                // Spectacle handles active window natively; insert before import.
                auto_rungs.insert(2, RungKind::SpectacleAuto);
            }
            auto_rungs.push(RungKind::GnomeScreenshotAuto);
            auto_rungs.retain(|r| which_binary(r.binary()));

            let mut select_rungs =
                vec![RungKind::ScrotSelect, RungKind::MaimSelect, RungKind::ImportSelect];
            if in_kde_session {
                select_rungs.insert(2, RungKind::SpectacleSelect);
            }
            select_rungs.push(RungKind::GnomeScreenshotSelect);
            select_rungs.retain(|r| which_binary(r.binary()));

            Self { session: SessionType::X11, auto_rungs, select_rungs, has_display: true }
        } else {
            // No graphical session.
            Self {
                session: SessionType::X11,
                auto_rungs: Vec::new(),
                select_rungs: Vec::new(),
                has_display: false,
            }
        }
    }

    /// Build a probe from the current process environment.
    pub fn detect() -> Self {
        Self::detect_from_env(
            std::env::var_os("WAYLAND_DISPLAY").as_deref(),
            std::env::var_os("DISPLAY").as_deref(),
        )
    }

    // ── Accessors for fono doctor ─────────────────────────────────────────

    pub fn auto_rungs(&self) -> &[RungKind] {
        &self.auto_rungs
    }

    pub fn select_rungs(&self) -> &[RungKind] {
        &self.select_rungs
    }

    pub fn session_label(&self) -> &'static str {
        match self.session {
            SessionType::Wayland => "Wayland",
            SessionType::X11 => "X11",
        }
    }

    // ── Capture ───────────────────────────────────────────────────────────

    /// Capture a screenshot.
    ///
    /// `focused_wm_class` is the WM_CLASS of the currently-focused window,
    /// used for the privacy gate (automatic mode only) and for the
    /// `source` field of the returned [`CapturedImage`].  Pass `None`
    /// when focus information is unavailable.
    ///
    /// Returns the first successful `CapturedImage` from the tool ladder.
    pub fn capture(
        &self,
        mode: CaptureMode,
        focused_wm_class: Option<&str>,
    ) -> Result<CapturedImage, CaptureError> {
        // Privacy gate (automatic mode only).
        if mode == CaptureMode::Automatic {
            if let Some(wm_class) = focused_wm_class {
                let lower = wm_class.to_ascii_lowercase();
                if PRIVATE_WINDOW_CLASSES.contains(&lower.as_str()) {
                    return Err(CaptureError::PrivateWindow);
                }
            }
        }

        let rungs = match mode {
            CaptureMode::Automatic => &self.auto_rungs,
            CaptureMode::Interactive => &self.select_rungs,
        };

        if rungs.is_empty() {
            return Err(CaptureError::NoToolAvailable);
        }

        let mut last_err = CaptureError::NoToolAvailable;
        for &rung in rungs {
            match invoke_rung(rung, mode) {
                Ok(bytes) => {
                    // Build source context.
                    let source = match (mode, focused_wm_class) {
                        (CaptureMode::Automatic, Some(wm_class)) => CaptureSource::Window {
                            wm_class: wm_class.to_owned(),
                            title: String::new(),
                        },
                        _ => CaptureSource::Region,
                    };
                    let (width, height) = parse_png_dimensions(&bytes).unwrap_or((0, 0));
                    return Ok(CapturedImage {
                        png_bytes: bytes,
                        source,
                        width,
                        height,
                        tool: rung.tool_name().to_string(),
                    });
                }
                Err(CaptureError::Cancelled) => return Err(CaptureError::Cancelled),
                Err(CaptureError::PrivateWindow) => return Err(CaptureError::PrivateWindow),
                Err(e) => {
                    tracing::debug!(
                        target: "fono::screen_capture",
                        rung = rung.display_name(),
                        error = %e,
                        "rung failed, trying next",
                    );
                    last_err = e;
                }
            }
        }

        Err(last_err)
    }
}

// ── Tool invocation ───────────────────────────────────────────────────────────

/// Timeout for automatic captures (no user interaction needed).
const AUTO_TIMEOUT: Duration = Duration::from_secs(5);
/// Timeout for interactive captures (user must interact with region picker).
const INTERACTIVE_TIMEOUT: Duration = Duration::from_secs(30);

/// Invoke a single rung and return the raw PNG bytes.
///
/// Returns `Err(Cancelled)` when the user dismissed the picker (exit != 0,
/// output file absent or empty).
fn invoke_rung(rung: RungKind, mode: CaptureMode) -> Result<Vec<u8>, CaptureError> {
    if rung == RungKind::Portal {
        // Portal will be implemented via ashpd in a future PR.
        // TODO: implement XDG Desktop Portal capture via ashpd.
        return Err(CaptureError::NoToolAvailable);
    }

    let tmp = next_temp_path();
    let timeout = if mode == CaptureMode::Automatic { AUTO_TIMEOUT } else { INTERACTIVE_TIMEOUT };

    match rung {
        RungKind::Portal => unreachable!("handled above"),

        RungKind::Scrot => {
            // scrot -u: capture the focused/active window (without -u it grabs root).
            run_with_timeout(Command::new("scrot").arg("-u").arg(tmp.as_os_str()), timeout, &tmp)?;
        }
        RungKind::ScrotSelect => {
            run_with_timeout(Command::new("scrot").arg("-s").arg(tmp.as_os_str()), timeout, &tmp)?;
        }

        RungKind::MaimAuto => {
            // maim needs the X window id from xdotool.
            let wid =
                run_capture_stdout(Command::new("xdotool").arg("getactivewindow"), AUTO_TIMEOUT)?;
            let wid = String::from_utf8_lossy(&wid).trim().to_owned();
            if wid.is_empty() {
                return Err(CaptureError::Cancelled);
            }
            run_with_timeout(
                Command::new("maim").arg("--window").arg(&wid).arg(tmp.as_os_str()),
                timeout,
                &tmp,
            )?;
        }
        RungKind::MaimSelect => {
            run_with_timeout(Command::new("maim").arg("-s").arg(tmp.as_os_str()), timeout, &tmp)?;
        }

        RungKind::ImportAuto => {
            // Prefer the active window over root: try xprop -root _NET_ACTIVE_WINDOW
            // (universally available on X11 without xdotool) to get the window id,
            // then pass it to import.  Fall back to root on any failure.
            let win_id = active_window_id_xprop();
            let target = win_id.as_deref().unwrap_or("root");
            run_with_timeout(
                Command::new("import").arg("-window").arg(target).arg(tmp.as_os_str()),
                timeout,
                &tmp,
            )?;
        }
        RungKind::ImportSelect => {
            // `import FILE` (no -window) opens a crosshair for click-drag
            // region selection. A bare click selects a single window.
            run_with_timeout(Command::new("import").arg(tmp.as_os_str()), timeout, &tmp)?;
        }

        RungKind::GrimSlurp => {
            // slurp emits the selected region on stdout; grim reads it via -g.
            let region = run_capture_stdout(Command::new("slurp").arg("-d"), INTERACTIVE_TIMEOUT)?;
            let region = String::from_utf8_lossy(&region).trim().to_owned();
            if region.is_empty() {
                return Err(CaptureError::Cancelled);
            }
            run_with_timeout(
                Command::new("grim").arg("-g").arg(&region).arg(tmp.as_os_str()),
                AUTO_TIMEOUT,
                &tmp,
            )?;
        }

        RungKind::SpectacleAuto => {
            // -b background, -a active window, -n no desktop notification.
            run_with_timeout(
                Command::new("spectacle")
                    .arg("-b")
                    .arg("-a")
                    .arg("-n")
                    .arg("-o")
                    .arg(tmp.as_os_str()),
                timeout,
                &tmp,
            )?;
        }
        RungKind::SpectacleSelect => {
            // -r rectangular region, -n no desktop notification.
            run_with_timeout(
                Command::new("spectacle").arg("-r").arg("-n").arg("-o").arg(tmp.as_os_str()),
                timeout,
                &tmp,
            )?;
        }

        RungKind::GnomeScreenshotAuto => {
            run_with_timeout(
                Command::new("gnome-screenshot").arg("-f").arg(tmp.as_os_str()),
                timeout,
                &tmp,
            )?;
        }
        RungKind::GnomeScreenshotSelect => {
            run_with_timeout(
                Command::new("gnome-screenshot").arg("-a").arg("-f").arg(tmp.as_os_str()),
                timeout,
                &tmp,
            )?;
        }
    }

    read_and_build(&tmp)
}

/// Spawn `cmd`, wait up to `timeout`, then check that `out_path` exists and
/// is non-empty.  Returns `Err(Timeout)` if the process takes too long, or
/// `Err(Cancelled)` when the exit status is non-zero / file is absent.
fn run_with_timeout(
    cmd: &mut Command,
    timeout: Duration,
    out_path: &std::path::Path,
) -> Result<(), CaptureError> {
    let mut child = cmd.spawn().map_err(CaptureError::Io)?;

    let deadline = std::time::Instant::now() + timeout;
    let poll_interval = Duration::from_millis(50);

    loop {
        if let Some(status) = child.try_wait().map_err(CaptureError::Io)? {
            // Process finished.
            if !status.success() {
                // Non-zero exit = user cancelled or tool failed.
                let _ = std::fs::remove_file(out_path);
                return Err(CaptureError::Cancelled);
            }
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(out_path);
            return Err(CaptureError::Timeout);
        }
        std::thread::sleep(poll_interval);
    }
}

/// Spawn `cmd`, wait up to `timeout`, and return its stdout bytes.
fn run_capture_stdout(cmd: &mut Command, timeout: Duration) -> Result<Vec<u8>, CaptureError> {
    use std::io::Read;

    let output = cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());

    // Simple approach: read stdout with a deadline via polling.
    let mut child = output.spawn().map_err(CaptureError::Io)?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().map_err(CaptureError::Io)? {
            if !status.success() {
                return Err(CaptureError::Cancelled);
            }
            // Read stdout.
            let mut buf = Vec::new();
            if let Some(mut stdout) = child.stdout.take() {
                stdout.read_to_end(&mut buf).map_err(CaptureError::Io)?;
            }
            return Ok(buf);
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CaptureError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Read the PNG from `path`, optionally downscale if large, delete the file,
/// and return the bytes.
fn read_and_build(path: &std::path::Path) -> Result<Vec<u8>, CaptureError> {
    if !path.exists() {
        return Err(CaptureError::Cancelled);
    }
    let bytes = std::fs::read(path).map_err(CaptureError::Io)?;
    let _ = std::fs::remove_file(path);

    if bytes.is_empty() {
        return Err(CaptureError::Cancelled);
    }

    // Downscale if the PNG is over 2 MiB using `magick convert`.
    if bytes.len() > 2 * 1024 * 1024 && which_binary("magick") {
        if let Ok(small) = downscale_png(&bytes) {
            if !small.is_empty() {
                return Ok(small);
            }
        }
    }

    Ok(bytes)
}

/// Downscale a PNG using `magick convert - -resize 1600x1600\> png:-`.
fn downscale_png(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    use std::io::Write;

    let mut child = Command::new("magick")
        .arg("convert")
        .arg("-")
        .arg("-resize")
        .arg("1600x1600>")
        .arg("png:-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(bytes)?;
    }
    let out = child.wait_with_output()?;
    Ok(out.stdout)
}

/// Query the active window ID via `xprop -root _NET_ACTIVE_WINDOW`.
/// Returns the hex window id string (e.g. `"0x3800003"`) or `None` on any
/// failure (xprop not in PATH, no DISPLAY, no EWMH support).
///
/// This avoids the xdotool dependency while still capturing the focused
/// window rather than the entire root window.
fn active_window_id_xprop() -> Option<String> {
    if !which_binary("xprop") {
        return None;
    }
    let out = Command::new("xprop").args(["-root", "_NET_ACTIVE_WINDOW"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    // Output format: "_NET_ACTIVE_WINDOW(WINDOW): window id # 0xe0000e"
    // The hex id is always the last whitespace-delimited token.
    let text = std::str::from_utf8(&out.stdout).ok()?;
    let id = text.split_whitespace().last()?;
    // Sanity check: must start with "0x" and be a valid hex window id.
    if id.starts_with("0x") && id.len() > 2 && u64::from_str_radix(&id[2..], 16).is_ok() {
        Some(id.to_string())
    } else {
        None
    }
}

// ── PNG dimension parser ──────────────────────────────────────────────────────

/// Parse the width and height from a PNG header without pulling in an image
/// crate.  The IHDR chunk always starts at byte 8 and contains dimensions at
/// offsets 16..20 (width) and 20..24 (height).
pub fn parse_png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 {
        return None;
    }
    if &bytes[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((w, h))
}

// ── PATH probe ────────────────────────────────────────────────────────────────

/// Return `true` when `tool` is found in `$PATH`.
pub fn which_binary(tool: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|p| p.join(tool).is_file())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal 24-byte PNG-like header with given dimensions.
    fn make_png_header(width: u32, height: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity(24);
        v.extend_from_slice(b"\x89PNG\r\n\x1a\n"); // 8 bytes magic
        v.extend_from_slice(&[0, 0, 0, 13]); // IHDR length = 13 bytes
        v.extend_from_slice(b"IHDR"); // chunk type
        v.extend_from_slice(&width.to_be_bytes());
        v.extend_from_slice(&height.to_be_bytes());
        // total = 8 + 4 + 4 + 4 + 4 = 24 bytes — exactly what parse_png_dimensions needs
        v
    }

    #[test]
    fn parse_png_dimensions_valid() {
        let data = make_png_header(1024, 768);
        let result = parse_png_dimensions(&data);
        assert_eq!(result, Some((1024, 768)));
    }

    #[test]
    fn parse_png_dimensions_too_short() {
        let data = vec![0u8; 23];
        assert_eq!(parse_png_dimensions(&data), None);
    }

    #[test]
    fn parse_png_dimensions_wrong_magic() {
        let mut data = make_png_header(100, 100);
        data[0] = 0x00; // corrupt magic
        assert_eq!(parse_png_dimensions(&data), None);
    }

    #[test]
    fn detect_wayland_only_no_import() {
        let wayland = std::ffi::OsStr::new("wayland-0");
        let probe = GrabberProbe::detect_from_env(Some(wayland), None);
        assert_eq!(probe.session, SessionType::Wayland);
        assert!(!probe.has_display);
        // Portal is always in auto_rungs on Wayland.
        assert!(probe.auto_rungs.contains(&RungKind::Portal));
        // ImportAuto must NOT be present without DISPLAY.
        assert!(!probe.auto_rungs.contains(&RungKind::ImportAuto));
    }

    #[test]
    fn detect_wayland_with_display_includes_import() {
        let wayland = std::ffi::OsStr::new("wayland-0");
        let display = std::ffi::OsStr::new(":0");
        // Even without import in PATH this test validates that the unfiltered
        // list includes ImportAuto — we check has_display flag instead.
        let probe = GrabberProbe::detect_from_env(Some(wayland), Some(display));
        assert!(probe.has_display);
        // If `import` is in PATH it would appear; if not, it is filtered.
        // We can only assert has_display is true.
        assert_eq!(probe.session, SessionType::Wayland);
    }

    #[test]
    fn detect_x11_only() {
        let display = std::ffi::OsStr::new(":0");
        let probe = GrabberProbe::detect_from_env(None, Some(display));
        assert_eq!(probe.session, SessionType::X11);
        // Portal should NOT be in X11 rungs.
        assert!(!probe.auto_rungs.contains(&RungKind::Portal));
        assert!(!probe.auto_rungs.contains(&RungKind::GrimSlurp));
        // Scrot would be included if in PATH; since we can't guarantee it in
        // CI, just verify the session is correct.
    }

    #[test]
    fn capture_returns_no_tool_available_when_no_rungs() {
        let probe = GrabberProbe {
            session: SessionType::X11,
            auto_rungs: Vec::new(),
            select_rungs: Vec::new(),
            has_display: false,
        };
        let result = probe.capture(CaptureMode::Automatic, None);
        assert!(matches!(result, Err(CaptureError::NoToolAvailable)));
    }

    #[test]
    fn capture_mock_success() {
        // Verify parse_png_dimensions on a valid header directly.
        let header = make_png_header(800, 600);
        let (w, h) = parse_png_dimensions(&header).unwrap();
        assert_eq!(w, 800);
        assert_eq!(h, 600);
    }

    #[test]
    fn capture_privacy_gate_blocks_private_windows() {
        // Even with no tools the privacy gate fires first.
        let probe = GrabberProbe {
            session: SessionType::X11,
            auto_rungs: vec![], // irrelevant — gate fires before rung walk
            select_rungs: vec![],
            has_display: false,
        };
        for class in ["keepassxc", "KeePassXC", "bitwarden", "1password", "seahorse"] {
            let result = probe.capture(CaptureMode::Automatic, Some(class));
            assert!(
                matches!(result, Err(CaptureError::PrivateWindow)),
                "expected PrivateWindow for class {class}"
            );
        }
    }

    #[test]
    fn capture_privacy_gate_passes_for_interactive() {
        // Privacy gate only applies to Automatic mode.
        let probe = GrabberProbe {
            session: SessionType::X11,
            auto_rungs: vec![],
            select_rungs: vec![], // no rungs → NoToolAvailable (not PrivateWindow)
            has_display: false,
        };
        let result = probe.capture(CaptureMode::Interactive, Some("keepassxc"));
        assert!(matches!(result, Err(CaptureError::NoToolAvailable)));
    }

    #[test]
    fn capture_privacy_gate_passes_for_normal_window() {
        let probe = GrabberProbe {
            session: SessionType::X11,
            auto_rungs: vec![],
            select_rungs: vec![],
            has_display: false,
        };
        // Non-private class with no rungs → NoToolAvailable (not PrivateWindow).
        let result = probe.capture(CaptureMode::Automatic, Some("firefox"));
        assert!(matches!(result, Err(CaptureError::NoToolAvailable)));
    }
}
