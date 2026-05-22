// SPDX-License-Identifier: GPL-3.0-only
//! Portal-based global hotkey listener for Wayland sessions.
//!
//! Talks to `org.freedesktop.portal.GlobalShortcuts` via `ashpd` so
//! Fono can grab keys on compositors that refuse synthetic X11 grabs
//! (GNOME-Wayland, KDE-Wayland, Hyprland, sway). On X11 / Xwayland-only
//! sessions, the existing `global-hotkey`-based [`crate::listener`]
//! stays the active backend.
//!
//! ## Architecture
//!
//! - **Single persistent session** binds dictation + assistant. One
//!   dialog at first launch ("Allow Fono to use these shortcuts?");
//!   per-backend permission caches mean subsequent launches reuse the
//!   approval silently.
//! - **Esc cancel is not yet bound through the portal.** Binding it
//!   permanently would grab `Esc` system-wide while Fono runs (which
//!   violates the user-stated invariant that bare `Esc` must work in
//!   other apps). The proper fix is a transient second session opened
//!   on `HotkeyControl::EnableCancel` and closed on `DisableCancel`,
//!   leaning on the backend permission cache to skip the re-prompt.
//!   That refactor is tracked as a follow-up. Until it lands, `Esc`
//!   cancel falls back to the tray "Cancel" entry on Wayland.
//!
//! ## Long-press semantics
//!
//! The X11 listener fakes long-press by stamping `Instant` on
//! `Pressed` and synthesising a second `TogglePressed` on `Released`
//! if the elapsed time exceeds [`crate::listener::LONG_PRESS_THRESHOLD`].
//! The portal emits `Activated` / `Deactivated` signals with
//! millisecond timestamps from the compositor, so the same logic
//! applies verbatim — see [`map_event`].

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use ashpd::desktop::global_shortcuts::{Activated, Deactivated, GlobalShortcuts, NewShortcut};
use ashpd::WindowIdentifier;
use crossbeam_channel::unbounded;
use futures::stream::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::fsm::HotkeyAction;
use crate::listener::{HotkeyBindings, HotkeyControl, ListenerHandle, LONG_PRESS_THRESHOLD};

const SHORTCUT_DICTATION: &str = "dictation";
const SHORTCUT_ASSISTANT: &str = "assistant";

/// Spawn the portal-based listener. Same return shape as
/// [`crate::listener::spawn`] so the daemon's call site can dispatch on
/// the resolved backend without surface-level changes.
pub fn spawn(
    bindings: HotkeyBindings,
    tx: mpsc::UnboundedSender<HotkeyAction>,
) -> Result<ListenerHandle> {
    let (ctrl_tx, ctrl_rx) = unbounded::<HotkeyControl>();

    // Pre-flight the portal connection synchronously so the caller's
    // fallback path engages on backends that don't ship the
    // GlobalShortcuts interface (notably xdg-desktop-portal-gnome
    // < 47 — Ubuntu 24.04 still ships 46.2 which lacks it).
    let (preflight_tx, preflight_rx) = std::sync::mpsc::channel::<Result<()>>();

    let bindings_run = bindings;
    let tx_run = tx;
    let thread = std::thread::Builder::new()
        .name("fono-hotkey-portal".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .thread_name("fono-portal-rt")
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = preflight_tx.send(Err(anyhow::anyhow!("build runtime: {e:#}")));
                    return;
                }
            };
            rt.block_on(async move {
                // Synchronous preflight: open the proxy AND create a
                // session. The CreateSession call is what xdg-desktop-
                // portal-gnome >= 47 rejects on unsandboxed callers
                // with `org.freedesktop.portal.Error.NotAllowed: An
                // app id is required` — if we only checked
                // `GlobalShortcuts::new()` (which succeeds because the
                // interface is advertised), the failure would surface
                // asynchronously after `spawn()` had already returned
                // Ok, and the detect.rs gsettings/X11 fallback chain
                // would never engage. Eat the round-trip up front so
                // the caller can fall back deterministically.
                let proxy = match GlobalShortcuts::new().await {
                    Ok(p) => p,
                    Err(e) => {
                        let _ = preflight_tx.send(Err(anyhow::anyhow!(
                            "GlobalShortcuts portal not available: {e}"
                        )));
                        return;
                    }
                };
                let session = match proxy.create_session().await {
                    Ok(s) => Arc::new(s),
                    Err(e) => {
                        let _ = preflight_tx
                            .send(Err(anyhow::anyhow!("portal CreateSession failed: {e}")));
                        return;
                    }
                };
                let _ = preflight_tx.send(Ok(()));
                if let Err(e) =
                    run_portal_with_proxy(proxy, session, bindings_run, tx_run, ctrl_rx).await
                {
                    warn!("portal listener exited: {e:#}");
                }
            });
        })
        .context("spawn portal hotkey thread")?;

    // Wait briefly for the preflight result. A healthy portal replies
    // in well under 500 ms; 2 s is generous.
    match preflight_rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(Ok(())) => Ok(ListenerHandle { thread, control: ctrl_tx }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(anyhow::anyhow!("portal preflight timed out after 2 s — falling back")),
    }
}

async fn run_portal_with_proxy(
    proxy: GlobalShortcuts<'_>,
    session: Arc<ashpd::desktop::Session<'_, GlobalShortcuts<'_>>>,
    bindings: HotkeyBindings,
    tx: mpsc::UnboundedSender<HotkeyAction>,
    ctrl_rx: crossbeam_channel::Receiver<HotkeyControl>,
) -> Result<()> {
    run_portal_inner(proxy, session, bindings, tx, ctrl_rx).await
}

#[allow(clippy::cognitive_complexity, clippy::too_many_lines)]
async fn run_portal_inner(
    proxy: GlobalShortcuts<'_>,
    session: Arc<ashpd::desktop::Session<'_, GlobalShortcuts<'_>>>,
    bindings: HotkeyBindings,
    tx: mpsc::UnboundedSender<HotkeyAction>,
    ctrl_rx: crossbeam_channel::Receiver<HotkeyControl>,
) -> Result<()> {
    debug!("portal session created");

    // Build the NewShortcut requests from config strings. The portal's
    // "preferred_trigger" format follows the XDG shortcuts spec — we pass
    // the user's binding verbatim and rely on the backend's parser
    // (works for "F7", "F8", "Ctrl+Space", etc.).
    let mut shortcuts = Vec::new();
    let dictation_trigger = bindings.dictation.trim().to_string();
    if !dictation_trigger.is_empty() {
        shortcuts.push(
            NewShortcut::new(SHORTCUT_DICTATION, "Toggle voice dictation")
                .preferred_trigger(Some(dictation_trigger.as_str())),
        );
    }
    let assistant_trigger = bindings.assistant.trim().to_string();
    if !assistant_trigger.is_empty() {
        shortcuts.push(
            NewShortcut::new(SHORTCUT_ASSISTANT, "Toggle voice assistant")
                .preferred_trigger(Some(assistant_trigger.as_str())),
        );
    }
    if shortcuts.is_empty() {
        anyhow::bail!("no portal shortcuts to bind (both dictation and assistant are empty)");
    }

    // First try ListShortcuts — if the session already has the bindings
    // cached on the backend (e.g. quick reconnect after daemon restart),
    // we skip the dialog. Backends that don't persist sessions across
    // client disconnects will return an empty list and we proceed to
    // BindShortcuts.
    let listed = proxy
        .list_shortcuts(&session)
        .await
        .ok()
        .and_then(|req| req.response().ok())
        .map(|r| r.shortcuts().to_vec())
        .unwrap_or_default();

    if listed.is_empty() {
        info!(
            "portal: binding shortcuts (dictation={:?}, assistant={:?}); \
             your desktop may show a one-time consent dialog",
            dictation_trigger, assistant_trigger
        );
        // No top-level window yet (daemon is headless at this point);
        // pass a null parent — supported by all backends.
        let bind = proxy
            .bind_shortcuts(&session, &shortcuts, &WindowIdentifier::default())
            .await
            .context("portal BindShortcuts call failed")?;
        match bind.response() {
            Ok(resp) => {
                let bound: Vec<_> = resp
                    .shortcuts()
                    .iter()
                    .map(|s| format!("{}={}", s.id(), s.trigger_description()))
                    .collect();
                info!("portal: bound {} shortcut(s): {}", resp.shortcuts().len(), bound.join(", "));
            }
            Err(e) => {
                anyhow::bail!(
                    "portal BindShortcuts rejected by the user or backend: {e}. \
                     Re-launch fono and approve the consent dialog, or set \
                     `FONO_HOTKEY_BACKEND=x11` to skip the portal."
                );
            }
        }
    } else {
        let names: Vec<_> =
            listed.iter().map(|s| format!("{}={}", s.id(), s.trigger_description())).collect();
        info!(
            "portal: resumed existing session with {} shortcut(s): {}",
            listed.len(),
            names.join(", ")
        );
    }

    // Subscribe to the activation streams.
    let mut activated =
        proxy.receive_activated().await.context("portal: receive_activated subscribe failed")?;
    let mut deactivated = proxy
        .receive_deactivated()
        .await
        .context("portal: receive_deactivated subscribe failed")?;

    // Per-role press timestamps, mirroring the X11 listener semantics
    // so short-press / long-press behaviour is identical on both
    // backends.
    let mut dictation_press_at: Option<Instant> = None;
    let mut assistant_press_at: Option<Instant> = None;

    // Bridge crossbeam ctrl_rx into the async task without blocking the
    // runtime. A scratch tokio task forwards messages.
    let (ctrl_async_tx, mut ctrl_async_rx) = mpsc::unbounded_channel::<HotkeyControl>();
    std::thread::Builder::new()
        .name("fono-portal-ctrl-bridge".into())
        .spawn(move || {
            while let Ok(msg) = ctrl_rx.recv() {
                if ctrl_async_tx.send(msg).is_err() {
                    break;
                }
            }
        })
        .ok();

    info!("portal listener armed; waiting for shortcut activations");
    loop {
        tokio::select! {
            ev = activated.next() => {
                let Some(ev) = ev else {
                    debug!("portal: activated stream closed; listener shutting down");
                    break;
                };
                if !forward_activated(&ev, &mut dictation_press_at, &mut assistant_press_at, &tx) {
                    break;
                }
            }
            ev = deactivated.next() => {
                let Some(ev) = ev else {
                    debug!("portal: deactivated stream closed; listener shutting down");
                    break;
                };
                if !forward_deactivated(&ev, &mut dictation_press_at, &mut assistant_press_at, &tx) {
                    break;
                }
            }
            ctrl = ctrl_async_rx.recv() => {
                let Some(ctrl) = ctrl else {
                    debug!("portal: ctrl channel closed; listener shutting down");
                    break;
                };
                match ctrl {
                    HotkeyControl::EnableCancel | HotkeyControl::DisableCancel => {
                        // The portal cancel-session refactor (transient
                        // second session) is a follow-up. For now we
                        // log and let the orchestrator's existing
                        // tray/IPC cancel paths cover the gap on
                        // Wayland.
                        debug!(
                            "portal: {ctrl:?} ignored (Esc cancel on Wayland portal is a follow-up; \
                             use the tray or `fono cancel` for now)"
                        );
                    }
                }
            }
        }
    }

    Ok(())
}

/// Translate an `Activated` portal event into the matching FSM action
/// and forward it. Returns `false` if the action channel has closed
/// (caller should shut down).
fn forward_activated(
    ev: &Activated,
    dictation_press_at: &mut Option<Instant>,
    assistant_press_at: &mut Option<Instant>,
    tx: &mpsc::UnboundedSender<HotkeyAction>,
) -> bool {
    let action = match ev.shortcut_id() {
        SHORTCUT_DICTATION => {
            *dictation_press_at = Some(Instant::now());
            HotkeyAction::TogglePressed
        }
        SHORTCUT_ASSISTANT => {
            *assistant_press_at = Some(Instant::now());
            HotkeyAction::AssistantPressed
        }
        other => {
            warn!("portal: unknown shortcut id {other:?} activated; ignoring");
            return true;
        }
    };
    tracing::debug!("portal Activated {} -> {action:?}", ev.shortcut_id());
    tx.send(action).is_ok()
}

/// Translate a `Deactivated` event. Short presses emit no extra
/// action (recording stays latched on); long presses emit a second
/// `TogglePressed` / `AssistantPressed` to stop (push-to-talk).
fn forward_deactivated(
    ev: &Deactivated,
    dictation_press_at: &mut Option<Instant>,
    assistant_press_at: &mut Option<Instant>,
    tx: &mpsc::UnboundedSender<HotkeyAction>,
) -> bool {
    let (slot, action) = match ev.shortcut_id() {
        SHORTCUT_DICTATION => (dictation_press_at, HotkeyAction::TogglePressed),
        SHORTCUT_ASSISTANT => (assistant_press_at, HotkeyAction::AssistantPressed),
        other => {
            warn!("portal: unknown shortcut id {other:?} deactivated; ignoring");
            return true;
        }
    };
    let Some(t0) = slot.take() else {
        return true;
    };
    if t0.elapsed() >= LONG_PRESS_THRESHOLD {
        tracing::debug!(
            "portal Deactivated {} (held {} ms) -> {action:?}",
            ev.shortcut_id(),
            t0.elapsed().as_millis()
        );
        return tx.send(action).is_ok();
    }
    tracing::debug!("portal Deactivated {} (short press, no synthetic stop)", ev.shortcut_id());
    true
}
