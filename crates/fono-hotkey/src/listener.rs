// SPDX-License-Identifier: GPL-3.0-only
//! Background global-hotkey listener.
//!
//! Owns a [`GlobalHotKeyManager`] on a dedicated OS thread, registers the
//! dictation and assistant hotkeys plus an optional cancel key, and
//! translates incoming events into [`HotkeyAction`]s that are forwarded
//! to the daemon's FSM through a tokio channel.
//!
//! Toggle-vs-hold is decided automatically per press based on how long
//! the key is held: a **short press** (< [`LONG_PRESS_THRESHOLD`])
//! latches recording on (toggle behaviour — release does nothing, the
//! next short press stops it); a **long press** keeps recording while
//! the key is held and stops on release (push-to-talk). Implementation:
//! every press emits the toggle action immediately so the user gets
//! instant feedback, and on release we synthesise a second toggle to
//! stop only when the press exceeded the threshold. The cancel hotkey
//! (default `Escape`) is only grabbed while a recording session is
//! active so it stays available to other applications the rest of the
//! time. The orchestrator drives this via [`HotkeyControl`] messages
//! sent on the channel returned by [`spawn`].

use anyhow::{Context, Result};
use crossbeam_channel::{select, unbounded, Sender};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::fsm::HotkeyAction;
use crate::parse::{parse_hotkey, ParsedHotkey};
use crate::KeyHeldFlags;

/// Press duration at which a press flips from "toggle" to "hold"
/// semantics. A release before this elapses leaves recording running
/// (next press stops it); a release after it stops recording.
pub const LONG_PRESS_THRESHOLD: Duration = Duration::from_millis(1000);

/// Configured hotkey strings (as they appear in `config.toml`).
#[derive(Debug, Clone)]
pub struct HotkeyBindings {
    /// Dictation key. Short press toggles, long press holds.
    pub dictation: String,
    pub cancel: String,
    /// Voice-assistant key. Empty disables the assistant hotkey path
    /// (the IPC + CLI surfaces still work). Same auto short/long-press
    /// behaviour as `dictation`.
    pub assistant: String,
}

/// Out-of-band commands the daemon sends to the listener thread to
/// dynamically grab/release the cancel hotkey based on FSM state. This
/// avoids holding a global grab on `Escape` (or whatever the user
/// configured) when no recording is in progress.
#[derive(Debug, Clone, Copy)]
pub enum HotkeyControl {
    /// Register the cancel hotkey (called when entering Recording).
    EnableCancel,
    /// Unregister the cancel hotkey (called when leaving Recording).
    DisableCancel,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Role {
    Dictation,
    Cancel,
    Assistant,
}

/// Handle returned by [`spawn`]: the join handle for the manager thread
/// (kept alive for the lifetime of the daemon) plus a control sender that
/// can be cloned to dynamically toggle the cancel hotkey grab.
pub struct ListenerHandle {
    pub thread: std::thread::JoinHandle<()>,
    pub control: Sender<HotkeyControl>,
}

/// Spawn a background thread that registers the hotkeys and forwards
/// [`HotkeyAction`]s into `tx`. `held_flags` is mutated from the
/// listener thread on every press/release so audio-side consumers
/// (the silence watchdog) can self-suppress while a key is held.
pub fn spawn(
    bindings: HotkeyBindings,
    tx: mpsc::UnboundedSender<HotkeyAction>,
    held_flags: KeyHeldFlags,
) -> Result<ListenerHandle> {
    // Install our X11 error handler before any XGrabKey call so we can
    // turn raw `BadAccess` stderr noise into actionable tracing output.
    crate::xerror::install();
    // Pre-parse so we fail the daemon early on a bad config.
    let dictation = parse_hotkey(&bindings.dictation)
        .with_context(|| format!("parsing hotkeys.dictation = {:?}", bindings.dictation))?
        .into_hotkey();
    // Cancel is parsed but NOT registered at startup; we only grab it
    // while recording so the key stays usable in other apps the rest
    // of the time.
    let cancel = parse_hotkey(&bindings.cancel).ok().map(ParsedHotkey::into_hotkey);
    // Assistant is optional — empty disables the assistant hotkey path.
    // A bad (non-empty) string is logged but doesn't fail daemon
    // startup, since the user can still trigger via IPC / CLI.
    let assistant = if bindings.assistant.trim().is_empty() {
        None
    } else {
        match parse_hotkey(&bindings.assistant) {
            Ok(p) => Some(p.into_hotkey()),
            Err(e) => {
                warn!(
                    "could not parse hotkeys.assistant = {:?}: {e:#}; \
                     assistant hotkey disabled (use `fono assistant ...` from CLI / tray)",
                    bindings.assistant
                );
                None
            }
        }
    };

    let (ctrl_tx, ctrl_rx) = unbounded::<HotkeyControl>();

    let thread = std::thread::Builder::new()
        .name("fono-hotkey".into())
        .spawn(move || {
            if let Err(e) =
                run_manager(dictation, cancel, assistant, tx, &bindings, ctrl_rx, held_flags)
            {
                warn!("hotkey manager exited: {e:#}");
            }
        })
        .context("spawn hotkey thread")?;
    Ok(ListenerHandle { thread, control: ctrl_tx })
}

fn run_manager(
    dictation: global_hotkey::hotkey::HotKey,
    cancel: Option<global_hotkey::hotkey::HotKey>,
    assistant: Option<global_hotkey::hotkey::HotKey>,
    tx: mpsc::UnboundedSender<HotkeyAction>,
    bindings: &HotkeyBindings,
    ctrl_rx: crossbeam_channel::Receiver<HotkeyControl>,
    held_flags: KeyHeldFlags,
) -> Result<()> {
    let manager = GlobalHotKeyManager::new().context(
        "GlobalHotKeyManager::new() failed (Wayland compositors without the \
         org.freedesktop.portal.GlobalShortcuts portal can't grab keys)",
    )?;

    let mut roles: HashMap<u32, Role> = HashMap::new();
    register(&manager, dictation, Role::Dictation, &bindings.dictation, &mut roles);
    if let Some(hk) = assistant {
        register(&manager, hk, Role::Assistant, &bindings.assistant, &mut roles);
    }

    if roles.is_empty() {
        anyhow::bail!("no hotkeys were successfully registered");
    }

    // Track whether the cancel hotkey is currently grabbed so we never
    // double-register or unregister-when-not-registered (both error).
    let mut cancel_active = false;

    // Per-role press timestamps. `Some(t)` means: a Pressed event fired
    // at `t` and we have not yet reconciled the matching Released. The
    // timestamp drives the toggle-vs-hold decision when Released
    // arrives. Cleared on Cancel so a long-held press whose recording
    // was already aborted does NOT synthesise a stop on release (which
    // would otherwise re-arm the FSM in toggle mode).
    let mut dictation_press_at: Option<Instant> = None;
    let mut assistant_press_at: Option<Instant> = None;

    let event_rx = GlobalHotKeyEvent::receiver();
    info!("hotkey listener armed; waiting for events");
    loop {
        select! {
            recv(event_rx) -> evt => {
                let Ok(event) = evt else {
                    warn!("hotkey channel closed");
                    break;
                };
                let Some(role) = roles.get(&event.id).copied() else {
                    continue;
                };
                let actions = map_event(
                    role,
                    event.state,
                    &mut dictation_press_at,
                    &mut assistant_press_at,
                    &held_flags,
                );
                for action in actions {
                    tracing::debug!("hotkey {role:?} {:?} -> {action:?}", event.state);
                    if tx.send(action).is_err() {
                        info!("hotkey action channel closed; listener shutting down");
                        return Ok(());
                    }
                }
            }
            recv(ctrl_rx) -> ctrl => {
                let Ok(ctrl) = ctrl else {
                    debug!("hotkey control channel closed; listener shutting down");
                    break;
                };
                handle_control(ctrl, &manager, cancel, bindings, &mut roles, &mut cancel_active);
            }
        }
    }

    drop(manager);
    Ok(())
}

fn handle_control(
    ctrl: HotkeyControl,
    manager: &GlobalHotKeyManager,
    cancel: Option<global_hotkey::hotkey::HotKey>,
    bindings: &HotkeyBindings,
    roles: &mut HashMap<u32, Role>,
    cancel_active: &mut bool,
) {
    let Some(hk) = cancel else {
        // Cancel binding didn't parse; nothing to do.
        return;
    };
    match ctrl {
        HotkeyControl::EnableCancel => {
            if *cancel_active {
                return;
            }
            match manager.register(hk) {
                Ok(()) => {
                    roles.insert(hk.id(), Role::Cancel);
                    *cancel_active = true;
                    debug!("cancel hotkey {} grabbed (recording)", bindings.cancel);
                }
                Err(e) => {
                    warn!(
                        "could not grab cancel hotkey {:?}: {e} \
                         (another app may already hold it)",
                        bindings.cancel
                    );
                }
            }
        }
        HotkeyControl::DisableCancel => {
            if !*cancel_active {
                return;
            }
            match manager.unregister(hk) {
                Ok(()) => {
                    roles.remove(&hk.id());
                    *cancel_active = false;
                    debug!("cancel hotkey {} released", bindings.cancel);
                }
                Err(e) => {
                    warn!("failed to release cancel hotkey {:?}: {e}", bindings.cancel);
                }
            }
        }
    }
}

fn register(
    manager: &GlobalHotKeyManager,
    hk: global_hotkey::hotkey::HotKey,
    role: Role,
    label: &str,
    roles: &mut HashMap<u32, Role>,
) {
    match manager.register(hk) {
        Ok(()) => {
            roles.insert(hk.id(), role);
            info!("registered hotkey {role:?} = {label}");
        }
        Err(e) => {
            warn!(
                "could not register {role:?} hotkey {label:?}: {e} \
                 (another app may already hold it)"
            );
        }
    }
}

/// Translate a single raw `(role, state)` event into zero, one, or
/// (very rarely) two FSM actions.
///
/// Dictation/assistant flow per press cycle:
///
/// 1. **Press** — emit `TogglePressed` / `AssistantPressed` immediately
///    so the user sees recording start without waiting on the
///    long-press threshold. Stash the press timestamp.
/// 2. **Release** — if the press lasted at least
///    [`LONG_PRESS_THRESHOLD`], emit a second `TogglePressed` /
///    `AssistantPressed` to stop recording (push-to-talk semantics).
///    Shorter presses leave recording latched on (toggle semantics).
///
/// Cancel handling: a `CancelPressed` clears both press timestamps so
/// that the eventual key-up (which may arrive long after the user hit
/// `Escape`) does not synthesise a spurious stop/start pair.
fn map_event(
    role: Role,
    state: HotKeyState,
    dictation_press_at: &mut Option<Instant>,
    assistant_press_at: &mut Option<Instant>,
    held_flags: &KeyHeldFlags,
) -> Vec<HotkeyAction> {
    match (role, state) {
        (Role::Dictation, HotKeyState::Pressed) => {
            *dictation_press_at = Some(Instant::now());
            held_flags.dictation.store(true, Ordering::Relaxed);
            vec![HotkeyAction::TogglePressed]
        }
        (Role::Dictation, HotKeyState::Released) => {
            held_flags.dictation.store(false, Ordering::Relaxed);
            if let Some(t0) = dictation_press_at.take() {
                if t0.elapsed() >= LONG_PRESS_THRESHOLD {
                    return vec![HotkeyAction::TogglePressed];
                }
            }
            vec![]
        }
        (Role::Assistant, HotKeyState::Pressed) => {
            *assistant_press_at = Some(Instant::now());
            held_flags.assistant.store(true, Ordering::Relaxed);
            vec![HotkeyAction::AssistantPressed]
        }
        (Role::Assistant, HotKeyState::Released) => {
            held_flags.assistant.store(false, Ordering::Relaxed);
            if let Some(t0) = assistant_press_at.take() {
                if t0.elapsed() >= LONG_PRESS_THRESHOLD {
                    return vec![HotkeyAction::AssistantPressed];
                }
            }
            vec![]
        }
        (Role::Cancel, HotKeyState::Pressed) => {
            // Discard any in-flight press so the matching release does
            // not re-arm the FSM after we've cancelled the recording.
            // Also clear the held flags — Cancel doesn't deliver a key-up
            // event for the dictation/assistant key, and a stale
            // "still held" flag would suppress pondering on the next
            // recording session.
            *dictation_press_at = None;
            *assistant_press_at = None;
            held_flags.dictation.store(false, Ordering::Relaxed);
            held_flags.assistant.store(false, Ordering::Relaxed);
            vec![HotkeyAction::CancelPressed]
        }
        (Role::Cancel, HotKeyState::Released) => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long_ago() -> Instant {
        Instant::now()
            .checked_sub(LONG_PRESS_THRESHOLD + Duration::from_millis(50))
            .expect("clock should be far enough past epoch in tests")
    }

    #[test]
    fn short_press_emits_toggle_only_on_press() {
        let mut d = None;
        let mut a = None;
        let flags = KeyHeldFlags::new();
        let pressed = map_event(Role::Dictation, HotKeyState::Pressed, &mut d, &mut a, &flags);
        assert_eq!(pressed, vec![HotkeyAction::TogglePressed]);
        assert!(flags.dictation.load(Ordering::Relaxed));
        // Immediate release counts as short press; no synthetic stop.
        let released = map_event(Role::Dictation, HotKeyState::Released, &mut d, &mut a, &flags);
        assert!(released.is_empty(), "short release must not emit");
        assert!(d.is_none());
        assert!(!flags.dictation.load(Ordering::Relaxed));
    }

    #[test]
    fn long_press_synthesises_stop_on_release() {
        let mut d = Some(long_ago());
        let mut a = None;
        let flags = KeyHeldFlags::new();
        flags.dictation.store(true, Ordering::Relaxed);
        let released = map_event(Role::Dictation, HotKeyState::Released, &mut d, &mut a, &flags);
        assert_eq!(released, vec![HotkeyAction::TogglePressed]);
        assert!(d.is_none());
        assert!(!flags.dictation.load(Ordering::Relaxed));
    }

    #[test]
    fn assistant_long_press_synthesises_stop() {
        let mut d = None;
        let mut a = Some(long_ago());
        let flags = KeyHeldFlags::new();
        flags.assistant.store(true, Ordering::Relaxed);
        let released = map_event(Role::Assistant, HotKeyState::Released, &mut d, &mut a, &flags);
        assert_eq!(released, vec![HotkeyAction::AssistantPressed]);
        assert!(a.is_none());
        assert!(!flags.assistant.load(Ordering::Relaxed));
    }

    #[test]
    fn cancel_clears_pending_presses() {
        let mut d = Some(long_ago());
        let mut a = Some(long_ago());
        let flags = KeyHeldFlags::new();
        flags.dictation.store(true, Ordering::Relaxed);
        flags.assistant.store(true, Ordering::Relaxed);
        let actions = map_event(Role::Cancel, HotKeyState::Pressed, &mut d, &mut a, &flags);
        assert_eq!(actions, vec![HotkeyAction::CancelPressed]);
        assert!(d.is_none() && a.is_none());
        // Held flags must reset so the next recording session isn't
        // started with a stale "key still held" state.
        assert!(!flags.dictation.load(Ordering::Relaxed));
        assert!(!flags.assistant.load(Ordering::Relaxed));
        // A subsequent late release after cancel must NOT synthesise
        // a stop (which would re-arm the FSM in toggle semantics).
        let released = map_event(Role::Dictation, HotKeyState::Released, &mut d, &mut a, &flags);
        assert!(released.is_empty());
    }

    /// Pondering suppression hinges on `held` reading `true` for the
    /// entire span between Pressed and Released. Verify the two
    /// transitions track the underlying physical state.
    #[test]
    fn held_flag_tracks_press_and_release_for_dictation() {
        let mut d = None;
        let mut a = None;
        let flags = KeyHeldFlags::new();
        assert!(!flags.dictation.load(Ordering::Relaxed));
        map_event(Role::Dictation, HotKeyState::Pressed, &mut d, &mut a, &flags);
        assert!(flags.dictation.load(Ordering::Relaxed));
        map_event(Role::Dictation, HotKeyState::Released, &mut d, &mut a, &flags);
        assert!(!flags.dictation.load(Ordering::Relaxed));
    }
}
