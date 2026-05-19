// SPDX-License-Identifier: GPL-3.0-only
//! Wayland compositor capability probe.
//!
//! Exposes [`compositor_supports_virtual_keyboard`] — a cached, bounded
//! probe of the Wayland registry that returns `true` iff the active
//! compositor advertises `zwp_virtual_keyboard_manager_v1`.
//!
//! Why this exists: `wtype` synthesises keystrokes via that protocol.
//! KWin, wlroots (sway / Hyprland), and a handful of others implement
//! it; **Mutter (GNOME-Wayland) does not** (as of GNOME 46). Calling
//! `which("wtype").is_some()` is therefore a false positive on GNOME
//! — `wtype` runs, prints nothing, and the keystrokes are silently
//! dropped by the compositor. The probe replaces that heuristic with
//! a direct registry walk.
//!
//! Cost: one transient Wayland connection, ~5–15 ms on a warm
//! compositor. The result is cached process-lifetime in a `OnceLock`.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use wayland_client::{protocol::wl_registry, Connection, Dispatch, QueueHandle};

static CACHED: OnceLock<bool> = OnceLock::new();

/// Returns `true` iff the active Wayland compositor advertises
/// `zwp_virtual_keyboard_manager_v1` (i.e. `wtype` will actually work).
///
/// Returns `false` on:
/// - X11 / no Wayland session (no `WAYLAND_DISPLAY`).
/// - Compositors lacking the protocol (notably GNOME-Wayland).
/// - Connection failures or > 250 ms timeout (treated as "unsupported"
///   to avoid selecting a backend we are not sure works).
///
/// First call performs the probe; subsequent calls return the cached
/// result for the lifetime of the process.
#[must_use]
pub fn compositor_supports_virtual_keyboard() -> bool {
    *CACHED.get_or_init(probe_once)
}

#[cfg(not(target_os = "linux"))]
fn probe_once() -> bool {
    false
}

#[cfg(target_os = "linux")]
#[allow(clippy::option_if_let_else, clippy::single_match_else)]
fn probe_once() -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return false;
    }
    // Run the probe on a scratch thread so a hung compositor cannot
    // freeze daemon startup. 250 ms is generous — a healthy Mutter or
    // KWin replies in well under 30 ms.
    let (tx, rx) = std::sync::mpsc::channel::<bool>();
    let started = Instant::now();
    std::thread::Builder::new()
        .name("fono-wayland-probe".into())
        .spawn(move || {
            let _ = tx.send(probe_registry());
        })
        .ok();
    match rx.recv_timeout(Duration::from_millis(250)) {
        Ok(v) => {
            tracing::debug!(
                target: "fono::inject::probe",
                ms = started.elapsed().as_millis() as u64,
                supports = v,
                "zwp_virtual_keyboard_manager_v1 probe done"
            );
            v
        }
        Err(_) => {
            tracing::warn!(
                target: "fono::inject::probe",
                "zwp_virtual_keyboard_manager_v1 probe timed out at 250 ms; assuming unsupported"
            );
            false
        }
    }
}

#[cfg(target_os = "linux")]
fn probe_registry() -> bool {
    let Ok(conn) = Connection::connect_to_env() else {
        return false;
    };
    let display = conn.display();
    let mut queue = conn.new_event_queue::<ProbeState>();
    let qh = queue.handle();
    let _registry = display.get_registry(&qh, ());
    let mut state = ProbeState { found: false };
    // Round-trip once: the server emits Global events synchronously
    // for every advertised interface in response to get_registry.
    if queue.roundtrip(&mut state).is_err() {
        return false;
    }
    state.found
}

#[cfg(target_os = "linux")]
struct ProbeState {
    found: bool,
}

#[cfg(target_os = "linux")]
impl Dispatch<wl_registry::WlRegistry, ()> for ProbeState {
    #[allow(clippy::ignored_unit_patterns)]
    fn event(
        state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { interface, .. } = event {
            if interface == "zwp_virtual_keyboard_manager_v1" {
                state.found = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_false_without_wayland_display() {
        // Ensure WAYLAND_DISPLAY is unset for this assertion.
        // SAFETY: tests are single-threaded by default for this crate;
        // if parallelised this needs `serial_test`.
        // We deliberately do not unset/restore globally — just verify
        // that the cached value, if computed in a no-Wayland context,
        // is false. On CI runners without a compositor, this should
        // hold trivially.
        if std::env::var_os("WAYLAND_DISPLAY").is_none() {
            assert!(!compositor_supports_virtual_keyboard());
        }
    }
}
