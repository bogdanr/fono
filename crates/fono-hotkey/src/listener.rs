// SPDX-License-Identifier: GPL-3.0-only
//! Background global-hotkey listener.
//!
//! Owns a [`GlobalHotKeyManager`] on a dedicated OS thread, registers the
//! three recording hotkeys (hold / toggle / paste-last) plus an optional
//! cancel key, and translates incoming events into [`HotkeyAction`]s that
//! are forwarded to the daemon's FSM through a tokio channel.

use anyhow::{Context, Result};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::fsm::HotkeyAction;
use crate::parse::{parse_hotkey, ParsedHotkey};

/// Configured hotkey strings (as they appear in `config.toml`).
#[derive(Debug, Clone)]
pub struct HotkeyBindings {
    pub hold: String,
    pub toggle: String,
    pub paste_last: String,
    pub cancel: String,
}

#[derive(Copy, Clone, Debug)]
enum Role {
    Hold,
    Toggle,
    PasteLast,
    Cancel,
}

/// Spawn a background thread that registers the hotkeys and forwards
/// [`HotkeyAction`]s into `tx`. Returns an `ActionSender` half the caller
/// can clone for synthetic (IPC-triggered) actions, and a join handle for
/// the manager thread (kept alive for the lifetime of the daemon).
pub fn spawn(
    bindings: HotkeyBindings,
    tx: mpsc::UnboundedSender<HotkeyAction>,
) -> Result<std::thread::JoinHandle<()>> {
    // Pre-parse so we fail the daemon early on a bad config.
    let hold = parse_hotkey(&bindings.hold)
        .with_context(|| format!("parsing hotkeys.hold = {:?}", bindings.hold))?
        .into_hotkey();
    let toggle = parse_hotkey(&bindings.toggle)
        .with_context(|| format!("parsing hotkeys.toggle = {:?}", bindings.toggle))?
        .into_hotkey();
    let paste_last = parse_hotkey(&bindings.paste_last)
        .with_context(|| format!("parsing hotkeys.paste_last = {:?}", bindings.paste_last))?
        .into_hotkey();
    let cancel = parse_hotkey(&bindings.cancel)
        .ok()
        .map(ParsedHotkey::into_hotkey);

    let handle = std::thread::Builder::new()
        .name("fono-hotkey".into())
        .spawn(move || {
            if let Err(e) = run_manager(hold, toggle, paste_last, cancel, tx, &bindings) {
                warn!("hotkey manager exited: {e:#}");
            }
        })
        .context("spawn hotkey thread")?;
    Ok(handle)
}

fn run_manager(
    hold: global_hotkey::hotkey::HotKey,
    toggle: global_hotkey::hotkey::HotKey,
    paste_last: global_hotkey::hotkey::HotKey,
    cancel: Option<global_hotkey::hotkey::HotKey>,
    tx: mpsc::UnboundedSender<HotkeyAction>,
    bindings: &HotkeyBindings,
) -> Result<()> {
    let manager = GlobalHotKeyManager::new().context(
        "GlobalHotKeyManager::new() failed (Wayland compositors without the \
         org.freedesktop.portal.GlobalShortcuts portal can't grab keys)",
    )?;

    let mut roles: HashMap<u32, Role> = HashMap::new();
    register(&manager, hold, Role::Hold, &bindings.hold, &mut roles);
    register(&manager, toggle, Role::Toggle, &bindings.toggle, &mut roles);
    register(
        &manager,
        paste_last,
        Role::PasteLast,
        &bindings.paste_last,
        &mut roles,
    );
    if let Some(c) = cancel {
        register(&manager, c, Role::Cancel, &bindings.cancel, &mut roles);
    }

    if roles.is_empty() {
        anyhow::bail!("no hotkeys were successfully registered");
    }

    let receiver = GlobalHotKeyEvent::receiver();
    info!("hotkey listener armed; waiting for events");
    loop {
        match receiver.recv() {
            Ok(event) => {
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
            Err(e) => {
                warn!("hotkey channel closed: {e}");
                break;
            }
        }
    }

    drop(manager);
    Ok(())
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
        (Role::PasteLast, HotKeyState::Pressed) => Some(HotkeyAction::PasteLastPressed),
        (Role::Cancel, HotKeyState::Pressed) => Some(HotkeyAction::CancelPressed),
        _ => None,
    }
}
