// SPDX-License-Identifier: GPL-3.0-only
//! Focused-window detection for per-app context rules.

use anyhow::Result;

#[derive(Debug, Clone, Default)]
pub struct FocusInfo {
    pub window_class: Option<String>,
    pub window_title: Option<String>,
    /// PID of the focused window's owning process, when available.
    /// Populated on X11 via `_NET_WM_PID`, on sway via the tree JSON `pid`
    /// field, and on Hyprland via `hyprctl activewindow -j`. Reserved for
    /// Phase C `/proc` terminal deep-enrichment — not yet consumed.
    pub window_pid: Option<u32>,
}

/// Best-effort focus detection. Always returns `Ok`; callers must gracefully
/// degrade (base prompt only) when all fields are `None`.
///
/// Detection order (B.5):
/// 1. Wayland-native paths (sway, Hyprland, GNOME) — only when
///    `XDG_SESSION_TYPE == "wayland"`.
/// 2. X11 path (behind `x11-focus` feature) — also tried as XWayland
///    fallback on Wayland sessions.
/// 3. `FocusInfo::default()` — all fields `None`.
///
/// macOS: `NSWorkspace.frontmostApplication` — app name, bundle id and
/// pid come for free with no TCC permission. The *window title* is not
/// populated there (reading other apps' titles needs the Screen
/// Recording permission); class-based rules still classify.
///
/// Windows: the Win32 foreground-window probe — `GetForegroundWindow`
/// for the active window, `GetWindowTextW` for its title, and
/// `GetWindowThreadProcessId` + `QueryFullProcessImageNameW` for the
/// owning process, whose bare executable name (e.g. `chrome.exe`,
/// `Code.exe`, `WindowsTerminal.exe`) is returned as `window_class` so
/// the classifier's Windows rules match (Windows port plan Phase 9).
/// Over a non-interactive session (e.g. headless SSH) there is no
/// foreground window and the probe degrades to an empty `FocusInfo`.
///
/// Other OSes: degrades to an empty `FocusInfo`.
pub fn detect_focus() -> Result<FocusInfo> {
    #[cfg(target_os = "macos")]
    {
        Ok(macos_focus())
    }
    #[cfg(target_os = "linux")]
    {
        Ok(detect_focus_linux_desktop())
    }
    #[cfg(target_os = "windows")]
    {
        Ok(windows_focus())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Ok(FocusInfo::default())
    }
}

// ---------------------------------------------------------------------------
// Windows — Win32 foreground window (GetForegroundWindow +
// QueryFullProcessImageNameW)
// ---------------------------------------------------------------------------

/// Foreground-window probe via the Win32 API. Populates `window_class`
/// with the owning process's bare executable name (e.g. `"chrome.exe"`,
/// `"Code.exe"`, `"WindowsTerminal.exe"`) so the case-insensitive
/// classifier rules match, `window_title` with the window caption, and
/// `window_pid` with the owning process id. Requires no elevated
/// privilege. Returns an empty `FocusInfo` (never an error) when there
/// is no foreground window — e.g. over a non-interactive window station.
#[cfg(target_os = "windows")]
fn windows_focus() -> FocusInfo {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    };

    // SAFETY: `GetForegroundWindow` takes no arguments and has no
    // preconditions. It returns a null handle when there is no
    // foreground window, which we check before any further use.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        tracing::debug!(
            target: "fono::context",
            "windows_focus: no foreground window (non-interactive session?)"
        );
        return FocusInfo::default();
    }

    // Window caption via GetWindowTextW.
    let window_title = unsafe {
        // SAFETY: `hwnd` is a valid foreground-window handle.
        let len = GetWindowTextLengthW(hwnd);
        if len > 0 {
            let mut buf = vec![0u16; len as usize + 1];
            // SAFETY: `buf` is a valid writable buffer of `buf.len()`
            // u16s; the call writes at most that many code units.
            let copied = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
            (copied > 0).then(|| String::from_utf16_lossy(&buf[..copied as usize]))
        } else {
            None
        }
    };

    // Owning process id.
    let mut pid: u32 = 0;
    // SAFETY: `hwnd` is valid; `&mut pid` is a valid out-pointer.
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    let window_pid = (pid != 0).then_some(pid);

    let window_class = window_pid.and_then(windows_process_exe_name);
    tracing::debug!(
        target: "fono::context",
        class = ?window_class,
        title = ?window_title,
        pid = ?window_pid,
        "detect_focus: Win32 foreground window"
    );
    FocusInfo { window_class, window_title, window_pid }
}

/// Resolve a process id to its bare executable file name (e.g.
/// `"chrome.exe"`) via `QueryFullProcessImageNameW`. Returns `None` when
/// the process cannot be opened (e.g. a higher-integrity process) or the
/// query fails — the classifier then simply finds no per-app rule.
#[cfg(target_os = "windows")]
fn windows_process_exe_name(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: `OpenProcess` returns a null handle on failure (e.g. access
    // denied), which we check before use.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }

    let mut buf = vec![0u16; 1024];
    let mut size = buf.len() as u32;
    // SAFETY: `handle` is a valid process handle; `buf`/`size` describe a
    // valid writable buffer. On success `size` is updated to the number
    // of code units written (excluding the NUL terminator).
    let ok = unsafe {
        QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut size)
    };
    // SAFETY: `handle` came from `OpenProcess` and is not used afterwards.
    unsafe { CloseHandle(handle) };

    if ok == 0 || size == 0 {
        return None;
    }
    let full = String::from_utf16_lossy(&buf[..size as usize]);
    // Return only the file name so the classifier's Windows rules match.
    full.rsplit(['\\', '/']).next().filter(|s| !s.is_empty()).map(ToOwned::to_owned)
}

// ---------------------------------------------------------------------------
// macOS — NSWorkspace.frontmostApplication
// ---------------------------------------------------------------------------

/// Frontmost-application probe via AppKit. Populates `window_class` with
/// the app's localized name (e.g. `"Terminal"`, `"iTerm2"`, `"Safari"`,
/// `"Code"`) so the existing case-insensitive classifier rules match,
/// and `window_pid` with the owning process id. Requires no permission.
/// Over headless SSH (no WindowServer) there is no frontmost app — the
/// probe degrades to an empty `FocusInfo`, never an error.
#[cfg(target_os = "macos")]
fn macos_focus() -> FocusInfo {
    use objc2_app_kit::NSWorkspace;

    let workspace = NSWorkspace::sharedWorkspace();
    let Some(app) = workspace.frontmostApplication() else {
        tracing::debug!(
            target: "fono::context",
            "macos_focus: no frontmost application (headless / no WindowServer?)"
        );
        return FocusInfo::default();
    };
    let window_class = app
        .localizedName()
        .map(|s| s.to_string())
        .or_else(|| app.bundleIdentifier().map(|s| s.to_string()));
    let window_pid = u32::try_from(app.processIdentifier()).ok();
    tracing::debug!(
        target: "fono::context",
        class = ?window_class,
        pid = ?window_pid,
        "detect_focus: NSWorkspace succeeded"
    );
    FocusInfo { window_class, window_title: None, window_pid }
}

/// Linux detection cascade (the historical `detect_focus` body).
/// Gated on `target_os = "linux"` — not merely `not(macos)` — because
/// `sway_focus` speaks the i3 IPC protocol over a Unix-domain socket
/// and the whole cascade probes Linux display-server environments
/// that carry no signal elsewhere (Windows port plan Task 1.3).
#[cfg(target_os = "linux")]
fn detect_focus_linux_desktop() -> FocusInfo {
    let session_type = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    tracing::debug!(target: "fono::context", session_type = %session_type, "detect_focus: starting");

    // B.1 — sway / wlroots IPC (raw framing, no extra crate).
    // Tried whenever $SWAYSOCK or $I3SOCK is present — no dependency on
    // XDG_SESSION_TYPE which is often "tty" when fono starts from a terminal.
    if std::env::var("SWAYSOCK").is_ok() || std::env::var("I3SOCK").is_ok() {
        match sway_focus() {
            Ok(info) => {
                tracing::debug!(
                    target: "fono::context",
                    class = ?info.window_class,
                    title = ?info.window_title,
                    pid = ?info.window_pid,
                    "detect_focus: sway succeeded"
                );
                return info;
            }
            Err(e) => tracing::debug!(target: "fono::context", "sway_focus failed: {e:#}"),
        }
    }

    // B.2 — Hyprland via `hyprctl activewindow -j` subprocess.
    if std::env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() {
        match hyprland_focus() {
            Ok(info) => {
                tracing::debug!(
                    target: "fono::context",
                    class = ?info.window_class,
                    title = ?info.window_title,
                    pid = ?info.window_pid,
                    "detect_focus: hyprland succeeded"
                );
                return info;
            }
            Err(e) => tracing::debug!(target: "fono::context", "hyprland_focus failed: {e:#}"),
        }
    }

    // B.3 — GNOME Shell via `gdbus call` subprocess.
    // Note: GNOME 46+ restricts GetWindows to trusted callers. When it fails
    // with AccessDenied the X11 fallback below still covers XWayland apps.
    if is_gnome_session() && session_type != "x11" {
        match gnome_focus() {
            Ok(info) => {
                tracing::debug!(
                    target: "fono::context",
                    class = ?info.window_class,
                    title = ?info.window_title,
                    "detect_focus: gnome succeeded"
                );
                return info;
            }
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.contains("AccessDenied") || msg.contains("GDBus.Error") {
                    tracing::debug!(
                        target: "fono::context",
                        "gnome_focus: GetWindows denied (GNOME 46+ policy) — falling back to X11"
                    );
                } else {
                    tracing::debug!(target: "fono::context", "gnome_focus failed: {msg}");
                }
            }
        }
    }

    // B.4 — KDE / Wayland gap note:
    // KDE runs most apps under XWayland, so the X11 fallback below already
    // covers the common case. Native Wayland KDE clients are a known gap.

    #[cfg(feature = "x11-focus")]
    {
        match x11_focus() {
            Ok(info) => {
                tracing::debug!(
                    target: "fono::context",
                    class = ?info.window_class,
                    title = ?info.window_title,
                    pid = ?info.window_pid,
                    "detect_focus: x11 succeeded"
                );
                return info;
            }
            Err(e) => tracing::debug!(target: "fono::context", "x11_focus failed: {e:#}"),
        }
    }

    tracing::debug!(target: "fono::context", "detect_focus: all paths failed — returning empty FocusInfo");
    FocusInfo::default()
}

// ---------------------------------------------------------------------------
// B.1 — sway / wlroots raw IPC
// ---------------------------------------------------------------------------

/// Query the focused window from sway (or i3) using the raw IPC socket
/// protocol. Reads `$SWAYSOCK` (fallback: `$I3SOCK`), sends a `get_tree`
/// request (type 4), and extracts the focused node's `app_id`, `name`, and
/// `pid`.
///
/// IPC frame format (header = 14 bytes):
///   magic[6]  = b"i3-ipc"
///   length[4] = u32 LE payload byte count
///   type[4]   = u32 LE message type
#[cfg(target_os = "linux")]
fn sway_focus() -> Result<FocusInfo> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let sock_path = std::env::var("SWAYSOCK")
        .or_else(|_| std::env::var("I3SOCK"))
        .map_err(|_| anyhow::anyhow!("neither $SWAYSOCK nor $I3SOCK is set"))?;

    let mut stream = UnixStream::connect(&sock_path)?;

    // Send get_tree request (type 4, empty payload).
    let mut req = [0u8; 14];
    req[..6].copy_from_slice(b"i3-ipc");
    // bytes 6..10 = payload_length (u32 LE) = 0
    // bytes 10..14 = message_type (u32 LE) = 4
    req[10..14].copy_from_slice(&4u32.to_le_bytes());
    stream.write_all(&req)?;

    // Read 14-byte response header.
    let mut hdr = [0u8; 14];
    stream.read_exact(&mut hdr)?;
    if &hdr[..6] != b"i3-ipc" {
        anyhow::bail!("sway IPC: invalid magic in response header");
    }
    let payload_len = u32::from_le_bytes(hdr[6..10].try_into().unwrap()) as usize;

    // Read payload.
    let mut payload = vec![0u8; payload_len];
    stream.read_exact(&mut payload)?;

    let tree: serde_json::Value = serde_json::from_slice(&payload)?;

    // Walk the tree recursively to find the focused node.
    let node = find_sway_focused(&tree)
        .ok_or_else(|| anyhow::anyhow!("sway IPC: no focused node found in tree"))?;

    let window_class = node
        .get("app_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        // Native Wayland apps populate `app_id`. XWayland apps leave it
        // null and surface WM_CLASS under `window_properties.class`
        // instead — fall back to that so xterm / xdotool-launched apps
        // / Electron-on-XWayland windows still classify.
        .or_else(|| {
            node.get("window_properties")
                .and_then(|wp| wp.get("class"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            node.get("window_properties")
                .and_then(|wp| wp.get("instance"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
        });

    let window_title =
        node.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(ToOwned::to_owned);

    let window_pid = node.get("pid").and_then(serde_json::Value::as_u64).map(|p| p as u32);

    Ok(FocusInfo { window_class, window_title, window_pid })
}

/// Recursively search a sway tree node for the focused leaf.
#[cfg(target_os = "linux")]
fn find_sway_focused(node: &serde_json::Value) -> Option<&serde_json::Value> {
    if node.get("focused").and_then(serde_json::Value::as_bool) == Some(true) {
        return Some(node);
    }
    for key in &["nodes", "floating_nodes"] {
        if let Some(children) = node.get(key).and_then(|v| v.as_array()) {
            for child in children {
                if let Some(found) = find_sway_focused(child) {
                    return Some(found);
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// B.2 — Hyprland via `hyprctl activewindow -j`
// ---------------------------------------------------------------------------

/// Query the focused window from Hyprland by spawning `hyprctl activewindow
/// -j` and parsing its JSON output. Synchronous subprocess call; typically
/// completes in ~5 ms — acceptable at hotkey-press granularity.
#[cfg(target_os = "linux")]
fn hyprland_focus() -> Result<FocusInfo> {
    use std::process::Command;

    let out = Command::new("hyprctl").args(["activewindow", "-j"]).output()?;
    if !out.status.success() {
        anyhow::bail!(
            "hyprctl exited with status {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;

    let window_class =
        v.get("class").and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(ToOwned::to_owned);

    let window_title =
        v.get("title").and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(ToOwned::to_owned);

    let window_pid = v.get("pid").and_then(serde_json::Value::as_u64).map(|p| p as u32);

    Ok(FocusInfo { window_class, window_title, window_pid })
}

// ---------------------------------------------------------------------------
// B.3 — GNOME Shell via `gdbus call`
// ---------------------------------------------------------------------------

/// Return `true` when the running desktop session appears to be GNOME.
#[cfg(target_os = "linux")]
fn is_gnome_session() -> bool {
    if let Ok(desktop) = std::env::var("XDG_CURRENT_DESKTOP") {
        if desktop.to_ascii_uppercase().contains("GNOME") {
            return true;
        }
    }
    std::env::var("GNOME_DESKTOP_SESSION_ID").is_ok()
}

/// Query the focused window from GNOME Shell using
/// `gdbus call … org.gnome.Shell.Introspect.GetWindows`.
///
/// Wrapped in a 15 ms wall-clock timeout via a background thread + channel.
/// The `gdbus` subprocess call is synchronous but bounded; typical latency
/// on a loaded GNOME session is ~5 ms.
///
/// GNOME's Introspect interface does not always expose `pid` for XWayland
/// clients; `window_pid` is left as `None` here.
#[cfg(target_os = "linux")]
fn gnome_focus() -> Result<FocusInfo> {
    use std::sync::mpsc;
    use std::time::Duration;

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(gdbus_get_focused_window());
    });

    rx.recv_timeout(Duration::from_millis(15))
        .map_err(|_| anyhow::anyhow!("gnome_focus: gdbus call timed out (>15 ms)"))?
}

/// Inner blocking call used by `gnome_focus`.
#[cfg(target_os = "linux")]
fn gdbus_get_focused_window() -> Result<FocusInfo> {
    use std::process::Command;

    let out = Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.gnome.Shell",
            "--object-path",
            "/org/gnome/Shell/Introspect",
            "--method",
            "org.gnome.Shell.Introspect.GetWindows",
        ])
        .output()?;

    if !out.status.success() {
        anyhow::bail!(
            "gdbus exited with status {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let text = String::from_utf8_lossy(&out.stdout);
    parse_gnome_introspect_output(&text)
}

/// Parse the GVariant text output from `org.gnome.Shell.Introspect.GetWindows`.
///
/// The output format is a tuple of dicts, e.g.:
/// ```text
/// ({'title': <'Firefox'>, 'wm-class': <'firefox'>, 'is-focused': <true>}, ...)
/// ```
///
/// This is not JSON — we do a minimal manual scan rather than a full GVariant
/// parser. Only the focused entry (containing `'is-focused': <true>`) is
/// inspected; class and title are extracted with simple substring searches.
#[cfg(target_os = "linux")]
fn parse_gnome_introspect_output(text: &str) -> Result<FocusInfo> {
    // Find the dict block that contains `'is-focused': <true>`.
    let mut depth = 0i32;
    let mut block_start = 0usize;
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0usize;

    while i < len {
        match chars[i] {
            '{' => {
                if depth == 0 {
                    block_start = i;
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let block: String = chars[block_start..=i].iter().collect();
                    if block.contains("'is-focused': <true>") {
                        let window_class = extract_gnome_string(&block, "wm-class");
                        let window_title = extract_gnome_string(&block, "title");
                        return Ok(FocusInfo { window_class, window_title, window_pid: None });
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }

    anyhow::bail!("gnome_focus: no focused window found in GetWindows output")
}

/// Extract `'key': <'value'>` from a GVariant dict block.
#[cfg(target_os = "linux")]
fn extract_gnome_string(block: &str, key: &str) -> Option<String> {
    let needle = format!("'{key}': <'");
    let start = block.find(&needle)? + needle.len();
    let rest = &block[start..];
    let end = rest.find("'>")?;
    let value = &rest[..end];
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

// ---------------------------------------------------------------------------
// X11 path (behind `x11-focus` feature flag)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "x11-focus", target_os = "linux"))]
fn x11_focus() -> Result<FocusInfo> {
    use anyhow::anyhow;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, GetPropertyReply};

    let (conn, screen_num) = x11rb::connect(None)?;
    let screen = &conn.setup().roots[screen_num];

    let active_atom = conn.intern_atom(false, b"_NET_ACTIVE_WINDOW")?.reply()?.atom;
    let reply: GetPropertyReply =
        conn.get_property(false, screen.root, active_atom, AtomEnum::WINDOW, 0, 1)?.reply()?;
    let window = reply
        .value32()
        .and_then(|mut it| it.next())
        .ok_or_else(|| anyhow!("_NET_ACTIVE_WINDOW unset"))?;

    // WM_CLASS is two NUL-separated strings: instance and class.
    let class_reply =
        conn.get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 1024)?.reply()?;
    let class_bytes = class_reply.value;
    let window_class = class_bytes
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .nth(1) // second field = class
        .map(|b| String::from_utf8_lossy(b).into_owned());

    let title_atom = conn.intern_atom(false, b"_NET_WM_NAME")?.reply()?.atom;
    let utf8_atom = conn.intern_atom(false, b"UTF8_STRING")?.reply()?.atom;
    let title_reply = conn.get_property(false, window, title_atom, utf8_atom, 0, 1024)?.reply()?;
    let window_title = if title_reply.value.is_empty() {
        let fallback = conn
            .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 1024)?
            .reply()?;
        Some(String::from_utf8_lossy(&fallback.value).into_owned())
    } else {
        Some(String::from_utf8_lossy(&title_reply.value).into_owned())
    };

    // Read _NET_WM_PID for Phase C terminal deep-enrichment.
    let pid_atom = conn.intern_atom(false, b"_NET_WM_PID")?.reply()?.atom;
    let pid_reply =
        conn.get_property(false, window, pid_atom, AtomEnum::CARDINAL, 0, 1)?.reply()?;
    let window_pid = pid_reply.value32().and_then(|mut it| it.next());

    Ok(FocusInfo { window_class, window_title, window_pid })
}
