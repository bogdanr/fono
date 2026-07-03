// SPDX-License-Identifier: GPL-3.0-only
//! Cross-platform desktop notification helper.
//!
//! - **Linux**: shells out to `notify-send` (libnotify CLI). libnotify's
//!   bus discovery is more forgiving than zbus's pure-Rust path, so this
//!   works in non-canonical environments (root sessions, systemd
//!   `--user` units without `PassEnvironment`, Flatpak/Snap launchers,
//!   container desktops, etc.) where `notify-rust` would fail with
//!   "No such file or directory" trying to find the session bus.
//! - **macOS / Windows**: routes through `notify-rust`, which has solid
//!   platform-native backends on those targets (notify-rust's bugs are
//!   zbus-specific to Linux).
//! - **Other**: no-op + debug log.
//!
//! All call sites in the workspace funnel through [`send`]; the previous
//! ~40 direct `notify_rust::Notification::new()` invocations are gone.

// `Command`/`Stdio` drive the Linux `notify-send` path only; the
// macOS/Windows backends go through `notify-rust` (no subprocess).
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::sync::OnceLock;

/// Notification urgency. On Linux this maps to `notify-send --urgency`;
/// macOS / Windows notifications have no urgency concept, so it is
/// accepted and ignored there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Low,
    Normal,
    Critical,
}

impl Urgency {
    // Only the Linux `notify-send --urgency` wire format needs the
    // string form (macOS/Windows backends have no urgency concept).
    #[cfg(target_os = "linux")]
    fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::Critical => "critical",
        }
    }
}

/// Send a desktop notification. Fire-and-forget; failures are logged at
/// `warn!` (Linux: missing `notify-send`; macOS/Windows: backend error)
/// and never propagate to the caller.
///
/// `icon` is a freedesktop icon name (e.g. `dialog-warning`,
/// `audio-input-microphone`). `timeout_ms` is the popup duration; some
/// notification daemons ignore it.
pub fn send(summary: &str, body: &str, icon: &str, timeout_ms: u32, urgency: Urgency) {
    #[cfg(target_os = "linux")]
    {
        send_linux(summary, body, icon, timeout_ms, urgency);
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        send_via_notify_rust(summary, body, icon, timeout_ms, urgency);
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = (summary, body, icon, timeout_ms, urgency);
        tracing::debug!("notify::send: notifications not supported on this platform");
    }
}

#[cfg(target_os = "linux")]
fn send_linux(summary: &str, body: &str, icon: &str, timeout_ms: u32, urgency: Urgency) {
    let result = Command::new("notify-send")
        .arg(format!("--icon={icon}"))
        .arg(format!("--expire-time={timeout_ms}"))
        .arg(format!("--urgency={}", urgency.as_str()))
        .arg(summary)
        .arg(body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match result {
        Ok(status) if status.success() => {
            tracing::debug!("notify-send: sent ({summary})");
        }
        Ok(status) => {
            tracing::warn!("notify-send: exited non-zero ({status}) for notification: {summary}");
        }
        Err(e) => {
            tracing::warn!(
                "notify-send: not found in PATH ({e}). Install libnotify \
                 (libnotify-bin on Debian/Ubuntu, libnotify on Arch / Fedora / \
                 openSUSE / Alpine) to enable desktop notifications. \
                 Notification was: {summary}"
            );
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn send_via_notify_rust(summary: &str, body: &str, icon: &str, timeout_ms: u32, urgency: Urgency) {
    use notify_rust::{Notification, Timeout};
    // notify-rust's `hint()` (and the XDG urgency hint it carries) only
    // exists on `cfg(all(unix, not(macos)))` — i.e. the Linux D-Bus
    // backend, which Fono reaches via `notify-send` instead. The macOS
    // (NSUserNotification) and Windows (toast) backends have no urgency
    // concept, so it is consumed here without a wire equivalent.
    let _ = urgency;
    match Notification::new()
        .summary(summary)
        .body(body)
        .icon(icon)
        .timeout(Timeout::Milliseconds(timeout_ms))
        .show()
    {
        Ok(_) => tracing::debug!("notify-rust: sent ({summary})"),
        Err(e) => tracing::warn!("notify-rust: failed to send ({summary}): {e}"),
    }
}

/// Cached `notify-send --version` probe. Used by the wizard preflight
/// to suggest installing libnotify if absent. On non-Linux always
/// returns `true` (the macOS/Windows backends are guaranteed by the
/// `notify-rust` dep on those targets).
pub fn is_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        static CACHED: OnceLock<bool> = OnceLock::new();
        *CACHED.get_or_init(|| {
            Command::new("notify-send")
                .arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn send_does_not_panic_when_notify_send_missing() {
        // Run with empty PATH so notify-send is not findable. The helper
        // should log a warn and return cleanly.
        let saved = std::env::var_os("PATH");
        // SAFETY: tests run serially when set_var is used; this test does
        // not run in parallel with anything that depends on PATH.
        unsafe {
            std::env::set_var("PATH", "");
        }
        send("test", "body", "dialog-warning", 1_000, Urgency::Normal);
        if let Some(p) = saved {
            unsafe {
                std::env::set_var("PATH", p);
            }
        }
    }

    #[test]
    fn urgency_strings() {
        assert_eq!(Urgency::Low.as_str(), "low");
        assert_eq!(Urgency::Normal.as_str(), "normal");
        assert_eq!(Urgency::Critical.as_str(), "critical");
    }
}
