// SPDX-License-Identifier: GPL-3.0-only
//! Focused-window detection for per-app context rules.

use anyhow::Result;

#[derive(Debug, Clone, Default)]
pub struct FocusInfo {
    pub window_class: Option<String>,
    pub window_title: Option<String>,
}

/// Best-effort focus detection. Returns defaults (all `None`) on Wayland or
/// when the X11 backend feature is not enabled — callers must gracefully
/// degrade (base prompt only, per Phase 6 Task 6.2).
pub fn detect_focus() -> Result<FocusInfo> {
    #[cfg(feature = "x11-focus")]
    {
        if std::env::var("XDG_SESSION_TYPE").as_deref() != Ok("wayland") {
            if let Ok(info) = x11_focus() {
                return Ok(info);
            }
        }
    }
    Ok(FocusInfo::default())
}

#[cfg(feature = "x11-focus")]
fn x11_focus() -> Result<FocusInfo> {
    use anyhow::anyhow;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, GetPropertyReply};

    let (conn, screen_num) = x11rb::connect(None)?;
    let screen = &conn.setup().roots[screen_num];

    let active_atom = conn
        .intern_atom(false, b"_NET_ACTIVE_WINDOW")?
        .reply()?
        .atom;
    let reply: GetPropertyReply = conn
        .get_property(false, screen.root, active_atom, AtomEnum::WINDOW, 0, 1)?
        .reply()?;
    let window = reply
        .value32()
        .and_then(|mut it| it.next())
        .ok_or_else(|| anyhow!("_NET_ACTIVE_WINDOW unset"))?;

    // WM_CLASS is two NUL-separated strings: instance and class.
    let class_reply = conn
        .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 1024)?
        .reply()?;
    let class_bytes = class_reply.value;
    let class = class_bytes
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .nth(1) // second field = class
        .map(|b| String::from_utf8_lossy(b).into_owned());

    let title_atom = conn.intern_atom(false, b"_NET_WM_NAME")?.reply()?.atom;
    let utf8_atom = conn.intern_atom(false, b"UTF8_STRING")?.reply()?.atom;
    let title_reply = conn
        .get_property(false, window, title_atom, utf8_atom, 0, 1024)?
        .reply()?;
    let title = if title_reply.value.is_empty() {
        let fallback = conn
            .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 1024)?
            .reply()?;
        Some(String::from_utf8_lossy(&fallback.value).into_owned())
    } else {
        Some(String::from_utf8_lossy(&title_reply.value).into_owned())
    };

    Ok(FocusInfo {
        window_class: class,
        window_title: title,
    })
}
