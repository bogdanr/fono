// SPDX-License-Identifier: GPL-3.0-only
//! Background global-hotkey listener.
//!
//! Owns a [`GlobalHotKeyManager`] on a dedicated OS thread, registers the
//! two recording hotkeys (hold / toggle) plus an optional
//! cancel key, and translates incoming events into [`HotkeyAction`]s that
//! are forwarded to the daemon's FSM through a tokio channel.
//!
//! The cancel hotkey (default `Escape`) is only grabbed while a recording
//! session is active so it stays available to other applications the rest
//! of the time. The orchestrator drives this via [`HotkeyControl`] messages
//! sent on the channel returned by [`spawn`].

use anyhow::{Context, Result};
use crossbeam_channel::{select, unbounded, Sender};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::fsm::HotkeyAction;
use crate::parse::{parse_hotkey, ParsedHotkey};

/// Configured hotkey strings (as they appear in `config.toml`).
#[derive(Debug, Clone)]
pub struct HotkeyBindings {
    pub hold: String,
    pub toggle: String,
    pub cancel: String,
    /// Voice-assistant push-to-talk key. Empty disables the assistant
    /// hotkey path (the IPC + CLI surfaces still work).
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

#[derive(Copy, Clone, Debug)]
enum Role {
    Hold,
    Toggle,
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
/// [`HotkeyAction`]s into `tx`.
pub fn spawn(
    bindings: HotkeyBindings,
    tx: mpsc::UnboundedSender<HotkeyAction>,
) -> Result<ListenerHandle> {
    // Install our X11 error handler before any XGrabKey call so we can
    // turn raw `BadAccess` stderr noise into actionable tracing output.
    crate::xerror::install();
    // Pre-parse so we fail the daemon early on a bad config.
    let hold = parse_hotkey(&bindings.hold)
        .with_context(|| format!("parsing hotkeys.hold = {:?}", bindings.hold))?
        .into_hotkey();
    let toggle = parse_hotkey(&bindings.toggle)
        .with_context(|| format!("parsing hotkeys.toggle = {:?}", bindings.toggle))?
        .into_hotkey();
    // Cancel is parsed but NOT registered at startup; we only grab it
    // while recording so the key stays usable in other apps the rest
    // of the time.
    let cancel = parse_hotkey(&bindings.cancel)
        .ok()
        .map(ParsedHotkey::into_hotkey);
    // Assistant is optional — empty disables the F10 path. A bad
    // (non-empty) string is logged but doesn't fail daemon startup,
    // since the user can still trigger via IPC / CLI.
    let assistant = if bindings.assistant.trim().is_empty() {
        None
    } else {
        match parse_hotkey(&bindings.assistant) {
            Ok(p) => Some(p.into_hotkey()),
            Err(e) => {
                warn!(
                    "could not parse hotkeys.assistant = {:?}: {e:#}; \
                     F10 disabled (use `fono assistant ...` from CLI / tray)",
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
            if let Err(e) = run_manager(hold, toggle, cancel, assistant, tx, &bindings, ctrl_rx) {
                warn!("hotkey manager exited: {e:#}");
            }
        })
        .context("spawn hotkey thread")?;
    Ok(ListenerHandle {
        thread,
        control: ctrl_tx,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_manager(
    hold: global_hotkey::hotkey::HotKey,
    toggle: global_hotkey::hotkey::HotKey,
    cancel: Option<global_hotkey::hotkey::HotKey>,
    assistant: Option<global_hotkey::hotkey::HotKey>,
    tx: mpsc::UnboundedSender<HotkeyAction>,
    bindings: &HotkeyBindings,
    ctrl_rx: crossbeam_channel::Receiver<HotkeyControl>,
) -> Result<()> {
    let manager = GlobalHotKeyManager::new().context(
        "GlobalHotKeyManager::new() failed (Wayland compositors without the \
         org.freedesktop.portal.GlobalShortcuts portal can't grab keys)",
    )?;

    let mut roles: HashMap<u32, Role> = HashMap::new();
    register(&manager, hold, Role::Hold, &bindings.hold, &mut roles);
    register(&manager, toggle, Role::Toggle, &bindings.toggle, &mut roles);
    if let Some(hk) = assistant {
        register(
            &manager,
            hk,
            Role::Assistant,
            &bindings.assistant,
            &mut roles,
        );
    }

    if roles.is_empty() {
        anyhow::bail!("no hotkeys were successfully registered");
    }

    // Track whether the cancel hotkey is currently grabbed so we never
    // double-register or unregister-when-not-registered (both error).
    let mut cancel_active = false;

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
                if let Some(action) = map_event(role, event.state) {
                    tracing::debug!("hotkey {role:?} {:?} -> {action:?}", event.state);
                    if tx.send(action).is_err() {
                        info!("hotkey action channel closed; listener shutting down");
                        break;
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

fn map_event(role: Role, state: HotKeyState) -> Option<HotkeyAction> {
    match (role, state) {
        (Role::Hold, HotKeyState::Pressed) => Some(HotkeyAction::HoldPressed),
        (Role::Hold, HotKeyState::Released) => Some(HotkeyAction::HoldReleased),
        (Role::Toggle, HotKeyState::Pressed) => Some(HotkeyAction::TogglePressed),
        (Role::Cancel, HotKeyState::Pressed) => Some(HotkeyAction::CancelPressed),
        (Role::Assistant, HotKeyState::Pressed) => Some(HotkeyAction::AssistantPressed),
        (Role::Assistant, HotKeyState::Released) => Some(HotkeyAction::AssistantReleased),
        _ => None,
    }
}
