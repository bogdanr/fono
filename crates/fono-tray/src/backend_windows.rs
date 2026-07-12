// SPDX-License-Identifier: GPL-3.0-only
//! Windows notification-area backend: renders the shared [`crate::menu`]
//! model as a `Shell_NotifyIcon` tray icon via the `tray-icon` crate
//! (with `muda` menus, reached through `tray_icon::menu`). Windows port
//! plan Task 6.2.
//!
//! # Architecture: one dedicated thread, two channels
//!
//! `tray-icon` on Windows requires a Win32 message loop running on the
//! *same thread* that created the icon — and its `TrayIcon` / `Menu`
//! handles are `!Send`, so they can never leave that thread. Unlike
//! AppKit (which demands the process **main** thread), Windows is happy
//! with *any* thread that pumps messages, so — unlike the macOS backend
//! — we do **not** need to touch `fono::main`. Instead [`spawn`] starts
//! a dedicated `fono-tray` OS thread that:
//!
//! * creates the `TrayIcon` + initial icon,
//! * runs a non-blocking `PeekMessageW` pump (so the notification-area
//!   window and `muda`'s hidden menu window get their messages),
//! * drains `MenuEvent::receiver()` and forwards each click back to the
//!   daemon as a [`TrayAction`], and
//! * applies rebuild/repaint [`TrayCommand`]s shipped from the tokio
//!   poll task.
//!
//! The poll task itself stays on tokio with the exact same 2 s cadence
//! and snapshot-diffing as the Linux (`ksni`) and macOS backends: it
//! builds the platform-neutral `MenuNode` tree (pure data, `Send`) and
//! sends it over a `std::sync::mpsc` channel to the tray thread, which
//! is the only place the non-`Send` `muda` handles are ever built.
//!
//! If there is no ambient tokio runtime (subcommand invocations, tests)
//! [`spawn`] degrades gracefully — one warn line, `false` return — the
//! same contract as the other two backends.
//!
//! # Click model
//!
//! Windows convention: both left- and right-click open the context
//! menu (`tray-icon`'s default), which is our primary surface. We do
//! not emit [`TrayAction::ActivateLeftClick`] on Windows; the menu's
//! own rows cover every intent.

use std::collections::HashMap;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use tray_icon::menu::{
    CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, TrayIconBuilder};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
};

use super::menu::{self, MenuInputs, MenuNode};
use super::{
    ActiveBackends, ActiveProvider, DiscoveredSttProvider, GpuUpgradeProvider, LlmEnabledProvider,
    McpEnabledProvider, MicrophonesProvider, PreferencesProvider, PreferencesSnapshot,
    RecentProvider, TrayAction, TrayState, UpdateProvider, WyomingEnabledProvider,
};

/// Icon side length in pixels. Windows scales the 16×16 notification
/// slot from this; 32 keeps the tinted circle crisp on high-DPI panels.
const ICON_SIZE: i32 = 32;

/// A unit of work shipped from the tokio poll task to the tray thread.
/// Both variants carry only `Send` data (`Vec<MenuNode>` is pure data,
/// `TrayState` is `Copy`) so they cross the thread boundary freely —
/// the non-`Send` `muda` handles are built on the far side.
enum TrayCommand {
    /// Rebuild the whole context menu from a fresh node tree.
    SetMenu(Vec<MenuNode>),
    /// Repaint the icon + tooltip for a new FSM state.
    SetIcon(TrayState),
}

// -------------------------------------------------------------------------
// Tray thread: owns the (!Send) TrayIcon + Win32 message pump.
// -------------------------------------------------------------------------

/// Body of the dedicated `fono-tray` thread. Creates the tray icon,
/// then loops forever pumping Win32 messages, forwarding menu clicks,
/// and applying commands until the command channel disconnects (daemon
/// shutdown), at which point the `TrayIcon` drops and the icon vanishes.
fn run_tray_thread(
    tooltip: String,
    actions: mpsc::UnboundedSender<TrayAction>,
    cmd_rx: std_mpsc::Receiver<TrayCommand>,
) {
    let builder = TrayIconBuilder::new().with_menu(Box::new(Menu::new())).with_tooltip(&tooltip);
    let builder = match icon(TrayState::Idle) {
        Some(ic) => builder.with_icon(ic),
        None => builder,
    };
    let tray = match builder.build() {
        Ok(tray) => tray,
        Err(err) => {
            tracing::warn!(
                "tray unavailable: could not create the Windows notification-area icon ({err}). \
                 Dictation and hotkeys continue without the tray."
            );
            return;
        }
    };
    tracing::debug!("tray icon ready (Shell_NotifyIcon via tray-icon)");

    // Maps each live menu item's id to the action it renders. Rebuilt
    // wholesale on every `SetMenu` so it always matches the shown menu.
    let mut registry: HashMap<MenuId, TrayAction> = HashMap::new();
    let menu_events = MenuEvent::receiver();

    // SAFETY: a zeroed `MSG` is a valid initial value; the pump only
    // reads fields that `PeekMessageW` populates before use.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        // 1. Drain all pending Win32 messages for this thread's windows
        //    (the tray window and muda's hidden menu window).
        // SAFETY: standard non-blocking message pump; `msg` is a valid
        // local and the null HWND asks for messages from any window on
        // this thread.
        unsafe {
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        // 2. Forward menu clicks to the daemon.
        while let Ok(event) = menu_events.try_recv() {
            if let Some(action) = registry.get(&event.id) {
                let _ = actions.send(*action);
            }
        }

        // 3. Apply rebuild/repaint commands from the poll task.
        loop {
            match cmd_rx.try_recv() {
                Ok(TrayCommand::SetMenu(nodes)) => {
                    let (new_menu, new_registry) = build_menu(&nodes);
                    tray.set_menu(Some(Box::new(new_menu)));
                    registry = new_registry;
                }
                Ok(TrayCommand::SetIcon(state)) => {
                    if let Some(ic) = icon(state) {
                        let _ = tray.set_icon(Some(ic));
                    }
                    let tip = format!("{tooltip}\n{}", menu::status_label(state));
                    let _ = tray.set_tooltip(Some(&tip));
                }
                Err(std_mpsc::TryRecvError::Empty) => break,
                Err(std_mpsc::TryRecvError::Disconnected) => {
                    tracing::debug!("tray thread exiting (daemon shutting down)");
                    return;
                }
            }
        }

        // 4. Idle briefly so the pump doesn't busy-spin. 30 ms keeps
        //    menu clicks and repaints feeling instant while costing
        //    almost no CPU.
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Build a `muda` [`Menu`] and its id→action registry from the
/// platform-neutral node tree. This is the entire Windows renderer: it
/// never changes when the menu content evolves — edit
/// [`crate::menu::build`] instead.
fn build_menu(nodes: &[MenuNode]) -> (Menu, HashMap<MenuId, TrayAction>) {
    let mut registry = HashMap::new();
    let items = build_items(nodes, &mut registry);
    let menu = Menu::new();
    for item in &items {
        if let Err(err) = menu.append(item.as_ref()) {
            tracing::warn!("tray: failed to append a root menu item: {err}");
        }
    }
    (menu, registry)
}

/// Recursively interpret a slice of [`MenuNode`]s into owned `muda`
/// items, recording each actionable item's id in `registry`. Returns
/// boxed trait objects so the caller can append them to a `Menu` or a
/// parent `Submenu` (both accept `&dyn IsMenuItem`).
fn build_items(
    nodes: &[MenuNode],
    registry: &mut HashMap<MenuId, TrayAction>,
) -> Vec<Box<dyn IsMenuItem>> {
    let mut out: Vec<Box<dyn IsMenuItem>> = Vec::with_capacity(nodes.len());
    for node in nodes {
        match node {
            MenuNode::Separator => out.push(Box::new(PredefinedMenuItem::separator())),
            MenuNode::Item { label, action } => {
                // `action == None` renders as a disabled informational row.
                let item = MenuItem::new(label, action.is_some(), None);
                if let Some(action) = action {
                    registry.insert(item.id().clone(), *action);
                }
                out.push(Box::new(item));
            }
            MenuNode::Check { label, checked, action } => {
                let item = CheckMenuItem::new(label, true, *checked, None);
                registry.insert(item.id().clone(), *action);
                out.push(Box::new(item));
            }
            MenuNode::Menu { label, children } => {
                let child_items = build_items(children, registry);
                // `&**b`: &Box<dyn IsMenuItem> -> &dyn IsMenuItem,
                // unambiguously (avoids AsRef inference noise).
                let refs: Vec<&dyn IsMenuItem> = child_items.iter().map(|b| &**b).collect();
                let submenu = Submenu::with_items(label, true, &refs)
                    .unwrap_or_else(|_| Submenu::new(label, true));
                out.push(Box::new(submenu));
            }
        }
    }
    out
}

/// Solid-colour circle icon tinted by FSM state — same rasterizer
/// shape and palette as the Linux/macOS backends (`menu::state_color`),
/// in RGBA byte order for [`Icon::from_rgba`]. Returns `None` only if
/// the (fixed, known-good) geometry is somehow rejected.
fn icon(state: TrayState) -> Option<Icon> {
    let (r, g, b) = menu::state_color(state);
    let mut data = Vec::with_capacity((ICON_SIZE * ICON_SIZE * 4) as usize);
    let center = ICON_SIZE / 2;
    let radius = (ICON_SIZE / 2) - 2;
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let (dx, dy) = (x - center, y - center);
            if dx * dx + dy * dy <= radius * radius {
                data.extend_from_slice(&[r, g, b, 0xff]);
            } else {
                data.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    match Icon::from_rgba(data, ICON_SIZE as u32, ICON_SIZE as u32) {
        Ok(ic) => Some(ic),
        Err(err) => {
            tracing::warn!("tray: failed to rasterize the {ICON_SIZE}x{ICON_SIZE} icon: {err}");
            None
        }
    }
}

// -------------------------------------------------------------------------
// Backend entry point (same contract as the Linux/macOS `spawn`).
// -------------------------------------------------------------------------

/// Everything the menu is rendered from, owned. One snapshot per poll
/// tick; compared against the previous one so unchanged ticks ship
/// nothing to the tray thread.
#[derive(Clone, PartialEq)]
struct Snapshot {
    state: TrayState,
    recent: Vec<String>,
    active: ActiveBackends,
    discovered_stt: Vec<String>,
    update_label: Option<String>,
    gpu_upgrade_label: Option<String>,
    microphones: (Vec<String>, u8),
    prefs: PreferencesSnapshot,
    mcp_server_enabled: bool,
    wyoming_server_enabled: bool,
    llm_server_enabled: bool,
}

/// Spawn the Windows tray. Returns `true` once the tray thread and the
/// tokio poll task are launched; the icon materialises on the tray
/// thread a moment later. Returns `false` (no tray) only when there is
/// no ambient tokio runtime or the OS thread can't be created.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    tooltip: String,
    actions: mpsc::UnboundedSender<TrayAction>,
    mut state_rx: mpsc::UnboundedReceiver<TrayState>,
    recent_provider: RecentProvider,
    stt_labels: Vec<String>,
    polish_labels: Vec<String>,
    assistant_labels: Vec<String>,
    tts_labels: Vec<String>,
    active_provider: ActiveProvider,
    discovered_stt_provider: DiscoveredSttProvider,
    update_provider: UpdateProvider,
    gpu_upgrade_provider: GpuUpgradeProvider,
    microphones_provider: MicrophonesProvider,
    preferences_provider: PreferencesProvider,
    mcp_enabled_provider: McpEnabledProvider,
    wyoming_enabled_provider: WyomingEnabledProvider,
    llm_enabled_provider: LlmEnabledProvider,
) -> bool {
    if tokio::runtime::Handle::try_current().is_err() {
        tracing::warn!("tray backend skipped: no current tokio runtime");
        return false;
    }

    let (cmd_tx, cmd_rx) = std_mpsc::channel::<TrayCommand>();

    // The tray icon and its Win32 pump live on their own OS thread —
    // `TrayIcon`/`Menu` are `!Send` and the pump blocks.
    let thread_actions = actions;
    if let Err(err) = std::thread::Builder::new()
        .name("fono-tray".into())
        .spawn(move || run_tray_thread(tooltip, thread_actions, cmd_rx))
    {
        tracing::warn!("tray backend skipped: could not start the tray thread: {err}");
        return false;
    }

    tokio::spawn(async move {
        let mut snap = Snapshot {
            state: TrayState::Idle,
            recent: Vec::new(),
            active: ActiveBackends::unknown(),
            discovered_stt: Vec::new(),
            update_label: None,
            gpu_upgrade_label: None,
            microphones: (Vec::new(), u8::MAX),
            prefs: PreferencesSnapshot::default(),
            mcp_server_enabled: mcp_enabled_provider(),
            wyoming_server_enabled: wyoming_enabled_provider(),
            llm_server_enabled: llm_enabled_provider(),
        };
        // First render.
        if cmd_tx.send(TrayCommand::SetIcon(snap.state)).is_err() {
            return;
        }
        if !push_menu(&cmd_tx, &snap, &stt_labels, &polish_labels, &assistant_labels, &tts_labels) {
            return;
        }

        let mut interval = tokio::time::interval(Duration::from_secs(2));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                state = state_rx.recv() => {
                    let Some(state) = state else { break };
                    if state == snap.state {
                        continue;
                    }
                    snap.state = state;
                    if cmd_tx.send(TrayCommand::SetIcon(state)).is_err() {
                        break;
                    }
                    // The menu embeds the status row, so a state flip
                    // re-renders it too.
                    if !push_menu(
                        &cmd_tx, &snap, &stt_labels, &polish_labels, &assistant_labels, &tts_labels,
                    ) {
                        break;
                    }
                }
                _ = interval.tick() => {
                    let next = Snapshot {
                        state: snap.state,
                        recent: recent_provider(),
                        active: active_provider(),
                        discovered_stt: discovered_stt_provider(),
                        update_label: update_provider(),
                        gpu_upgrade_label: gpu_upgrade_provider(),
                        microphones: microphones_provider(),
                        prefs: preferences_provider(),
                        mcp_server_enabled: mcp_enabled_provider(),
                        wyoming_server_enabled: wyoming_enabled_provider(),
                        llm_server_enabled: llm_enabled_provider(),
                    };
                    if next == snap {
                        continue;
                    }
                    snap = next;
                    if !push_menu(
                        &cmd_tx, &snap, &stt_labels, &polish_labels, &assistant_labels, &tts_labels,
                    ) {
                        break;
                    }
                }
            }
        }
        tracing::debug!("tray poll task exited (daemon shutting down)");
    });
    true
}

/// Build the menu tree from a snapshot (pure, tokio side) and ship it
/// to the tray thread for rendering. Returns `false` if the tray
/// thread has gone away (channel closed), signalling the poll loop to
/// stop.
fn push_menu(
    cmd_tx: &std_mpsc::Sender<TrayCommand>,
    snap: &Snapshot,
    stt_labels: &[String],
    polish_labels: &[String],
    assistant_labels: &[String],
    tts_labels: &[String],
) -> bool {
    let inputs = MenuInputs {
        state: snap.state,
        recent: &snap.recent,
        stt_labels,
        polish_labels,
        assistant_labels,
        tts_labels,
        active: snap.active,
        discovered_stt: &snap.discovered_stt,
        update_label: snap.update_label.as_deref(),
        gpu_upgrade_label: snap.gpu_upgrade_label.as_deref(),
        microphones: (&snap.microphones.0, snap.microphones.1),
        prefs: &snap.prefs,
        mcp_server_enabled: snap.mcp_server_enabled,
        wyoming_server_enabled: snap.wyoming_server_enabled,
        llm_server_enabled: snap.llm_server_enabled,
    };
    let nodes = menu::build(&inputs);
    cmd_tx.send(TrayCommand::SetMenu(nodes)).is_ok()
}
