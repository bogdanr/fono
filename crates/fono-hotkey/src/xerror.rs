// SPDX-License-Identifier: GPL-3.0-only
//! Process-global X11 error handler.
//!
//! `global_hotkey` calls `XGrabKey` synchronously, but X reports
//! `BadAccess` (e.g. another app already owns the key) **asynchronously**
//! through libxlib's default error handler — which prints the raw
//! `X Error of failed request: BadAccess …` line to stderr without the
//! Rust caller ever knowing. The grab silently fails and the hotkey
//! looks "armed" in the log even though the key never reaches us.
//!
//! Installing our own `XSetErrorHandler` once at daemon startup
//! converts these into actionable `tracing::error!` messages and
//! suppresses the stderr noise.

#[cfg(target_os = "linux")]
mod linux {
    use std::os::raw::c_int;
    use std::sync::Once;

    use x11_dl::xlib;

    static INIT: Once = Once::new();

    /// Install a process-global X error handler. Idempotent.
    pub fn install() {
        INIT.call_once(|| {
            let Ok(lib) = xlib::Xlib::open() else {
                tracing::debug!("xerror: x11-dl could not open libX11; skipping handler install");
                return;
            };
            unsafe {
                (lib.XSetErrorHandler)(Some(handler));
            }
            tracing::debug!("xerror: installed custom X11 error handler");
        });
    }

    /// X error opcode for `XGrabKey` — we treat BadAccess on this as a
    /// hotkey-conflict and emit a friendly message.
    const X_GRAB_KEY: u8 = 33;
    const BAD_ACCESS: u8 = 10;

    extern "C" fn handler(_display: *mut xlib::Display, error: *mut xlib::XErrorEvent) -> c_int {
        if error.is_null() {
            return 0;
        }
        // Safety: the caller (libX11) guarantees this pointer is valid
        // for the duration of the handler call.
        let e = unsafe { &*error };
        let request = e.request_code;
        let code = e.error_code;

        if request == X_GRAB_KEY && code == BAD_ACCESS {
            tracing::error!(
                "X11 hotkey grab denied (BadAccess on X_GrabKey): another application \
                 (window manager, browser, terminal, screen-recorder, etc.) already owns \
                 one of the keys you bound. Change `[hotkeys].hold` or `[hotkeys].toggle` \
                 in ~/.config/fono/config.toml to a different key (e.g. F11, Pause, \
                 ScrollLock, or a Mod+letter combination)"
            );
        } else {
            tracing::warn!("X11 error: request_code={request} error_code={code} (non-fatal)");
        }
        0
    }
}

#[cfg(target_os = "linux")]
pub use linux::install;

#[cfg(not(target_os = "linux"))]
pub fn install() {}
