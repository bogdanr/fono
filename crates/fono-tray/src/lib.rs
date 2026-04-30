// SPDX-License-Identifier: GPL-3.0-only
//! Tray-icon integration. Phase 7 Task 7.1.
//!
//! When the `tray-backend` feature is enabled (default for the `fono`
//! binary on Linux), this crate spawns a real system-tray icon as a
//! tokio task driving a pure-Rust StatusNotifierItem (SNI) D-Bus
//! service via [`ksni`]. Without the feature, a no-op [`Tray`] keeps
//! the code paths compiling for headless builds and cross-platform CI.
//!
//! # No GTK, no shared libraries
//!
//! Earlier versions used `tray-icon`'s libappindicator backend, which
//! dragged GTK3 + glib + cairo + pango + gio + gdk-pixbuf into the
//! binary's `NEEDED` list. SNI is the protocol every modern desktop
//! tray host already speaks (KDE Plasma, GNOME with the SNI shell
//! extension, sway+waybar, hyprland+waybar, i3+i3status, xfce4-panel,
//! lxqt-panel) — there's no reason to drag a toolkit through it.
//! `ksni` talks SNI + `com.canonical.dbusmenu` over `zbus` directly,
//! pure Rust, no C dependencies. Phase 2 Task 2.1 of
//! `plans/2026-04-30-fono-single-binary-size-v1.md`.
//!
//! # UX features
//!
//! - **Recent transcriptions submenu** — the last `RECENT_SLOTS`
//!   dictations are shown as clickable menu items. Clicking one fires
//!   [`TrayAction::PasteHistory`] with the slot index (0 = newest); the
//!   daemon then re-injects/copies that text. This is the clipit-style
//!   workflow users asked for.
//! - **STT / LLM backend submenus** — switch the active provider on
//!   the fly; the active backend wears a leading "● " marker.
//! - **Microphone submenu** — list Pulse/PipeWire input devices and
//!   set default via the daemon.
//! - **Update submenu entry** — appears only when the background
//!   checker has detected a new release.

use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use tokio::sync::mpsc;

/// How many recent transcriptions to surface in the tray menu.
pub const RECENT_SLOTS: usize = 10;

/// Provider that returns the most recent transcription labels (newest
/// first) for display in the tray's "Recent" submenu. Called from the
/// tray task on a poll interval.
pub type RecentProvider = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// Provider that returns `(stt_idx, llm_idx)` — indices into
/// `stt_labels` / `llm_labels` (the slices passed to [`spawn`]) for
/// the currently-active STT and LLM backends. Polled every ~2 s; the
/// tray repaints the active marker (`●`) when the indices change.
///
/// `u8::MAX` for either index means "unknown / not in the list" and
/// renders no checkmark — useful when the active backend isn't one
/// fono knows about (custom OpenAI-compatible endpoint etc.).
pub type ActiveProvider = Arc<dyn Fn() -> (u8, u8) + Send + Sync>;

/// Provider returning the current update label for the tray's
/// "Update" menu item, or `None` to keep the item hidden/disabled.
/// Called on the same ~2 s poll as the recent/active providers.
///
/// Convention:
/// - `None` → entry is hidden.
/// - `Some(label)` → e.g. "Update to v0.3.0" — clicking fires
///   [`TrayAction::ApplyUpdate`].
pub type UpdateProvider = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Provider returning `(devices, active_idx)` for the "Microphone"
/// submenu — `devices` is the live input-device list (display
/// names) and `active_idx` is the index of the device the OS reports
/// as the current default, or `u8::MAX` when none of the listed
/// devices is the default ("Auto / system default" row stays marked).
/// Polled every ~2 s; the tray refreshes the submenu in place when
/// either changes.
pub type MicrophonesProvider = Arc<dyn Fn() -> (Vec<String>, u8) + Send + Sync>;

/// FSM-aligned tray state used to tint the icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TrayState {
    Idle = 0,
    Recording = 1,
    Processing = 2,
    Paused = 3,
}

/// User actions fired from the tray menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    ShowStatus,
    ToggleRecording,
    Pause,
    /// Re-paste the i-th most recent transcription (0 = newest).
    PasteHistory(usize),
    /// Switch the active STT backend. Index into the `stt_labels`
    /// slice passed to [`spawn`]. Provider-switching plan task R2.1.
    UseStt(u8),
    /// Switch the active LLM backend. Index into the `llm_labels`
    /// slice passed to [`spawn`].
    UseLlm(u8),
    /// User clicked the "Update to vX" entry. The daemon handles
    /// this by running a check and applying the update via
    /// `fono-update::apply_update`.
    ApplyUpdate,
    /// Switch the active input device. The `u8` is an index into the
    /// device list returned by [`MicrophonesProvider`] at the time of
    /// the click. On Pulse / PipeWire hosts the daemon dispatches
    /// this to `pactl set-default-source`; the cpal branch hides
    /// the submenu so this is never fired there.
    SetInputDevice(u8),
    OpenConfig,
    Quit,
}

/// Handle returned by [`spawn`]. Dropping it tears down the tray task
/// on a best-effort basis (the underlying `mpsc` channel closes; the
/// ksni service then exits at the next tick).
pub struct Tray {
    shared_state: Arc<AtomicU8>,
    #[cfg(feature = "tray-backend")]
    state_tx: Option<mpsc::UnboundedSender<TrayState>>,
}

impl Tray {
    /// Update the tray icon to reflect the given FSM state.
    pub fn set_state(&self, state: TrayState) {
        self.shared_state.store(state as u8, Ordering::Relaxed);
        #[cfg(feature = "tray-backend")]
        if let Some(tx) = self.state_tx.as_ref() {
            let _ = tx.send(state);
        }
    }

    /// Last state stored via [`Tray::set_state`]. Useful for tests.
    pub fn state(&self) -> TrayState {
        match self.shared_state.load(Ordering::Relaxed) {
            1 => TrayState::Recording,
            2 => TrayState::Processing,
            3 => TrayState::Paused,
            _ => TrayState::Idle,
        }
    }
}

/// Spawn the tray on the ambient tokio runtime.
///
/// Returns `(handle, actions_rx)`. `actions_rx` yields [`TrayAction`]s the
/// user clicked in the menu. If the feature is off (or init fails) the
/// returned receiver simply never fires.
///
/// Submenu inputs:
/// * `recent_provider` — invoked every ~2 s for the "Recent
///   transcriptions" submenu. Pass a noop (`|| Vec::new()`) to disable.
/// * `stt_labels` / `llm_labels` — display strings for each STT / LLM
///   backend, in canonical order (the order the daemon also iterates
///   when decoding indices back to `SttBackend` / `LlmBackend`).
/// * `active_provider` — invoked on the same poll; returns the indices
///   of the currently-active STT and LLM in the slices above. Used to
///   paint the active-marker (`●`) and migrate it on `Reload`.
/// * `update_provider` — `Some(label)` shows / refreshes the "Update
///   to vX" entry; `None` hides it.
/// * `microphones_provider` — `(devices, active_idx)` for the
///   Microphone submenu; empty list hides the submenu.
#[allow(unused_variables, clippy::too_many_arguments)]
pub fn spawn(
    tooltip: &str,
    recent_provider: RecentProvider,
    stt_labels: Vec<String>,
    llm_labels: Vec<String>,
    active_provider: ActiveProvider,
    update_provider: UpdateProvider,
    microphones_provider: MicrophonesProvider,
) -> (Tray, mpsc::UnboundedReceiver<TrayAction>) {
    let shared = Arc::new(AtomicU8::new(TrayState::Idle as u8));
    let (action_tx, action_rx) = mpsc::unbounded_channel();

    #[cfg(feature = "tray-backend")]
    {
        let (state_tx, state_rx) = mpsc::unbounded_channel::<TrayState>();
        let started = backend::spawn(
            tooltip.to_string(),
            action_tx,
            state_rx,
            recent_provider,
            stt_labels,
            llm_labels,
            active_provider,
            update_provider,
            microphones_provider,
        );
        let state_tx = if started { Some(state_tx) } else { None };
        (
            Tray {
                shared_state: shared,
                state_tx,
            },
            action_rx,
        )
    }

    #[cfg(not(feature = "tray-backend"))]
    {
        (
            Tray {
                shared_state: shared,
            },
            action_rx,
        )
    }
}

// -------------------------------------------------------------------------
// Real backend (pure-Rust SNI via `ksni`).
// -------------------------------------------------------------------------

#[cfg(feature = "tray-backend")]
mod backend {
    use super::{
        ActiveProvider, MicrophonesProvider, RecentProvider, TrayAction, TrayState, UpdateProvider,
        RECENT_SLOTS,
    };
    use fono_core::notify::{self, Urgency};
    use ksni::{
        menu::{StandardItem, SubMenu},
        Handle, MenuItem, ToolTip, TrayMethods,
    };
    use tokio::sync::mpsc;

    const MISSING_WATCHER_NOTIFICATION_TIMEOUT_MS: u32 = 20_000;
    const MISSING_WATCHER_NOTIFICATION_TITLE: &str = "Fono tray unavailable";
    const MISSING_WATCHER_NOTIFICATION_BODY: &str = "No StatusNotifierWatcher found. Start a tray host, e.g. Waybar tray, KDE tray, xfce4-panel, or snixembed, then restart Fono.";

    /// Microphone slots in the "Microphone" submenu. Pre-allocated for
    /// the same reason as the STT/LLM lists: rebuilding causes flicker
    /// on KDE/GNOME indicator hosts. Eight covers the common case
    /// (laptop builtin + USB headset + dock + a second USB device).
    const MIC_SLOTS: usize = 8;

    /// Backing model for the SNI tray. ksni periodically queries this
    /// (via the `Tray` trait methods) to repaint the icon and menu
    /// when the desktop's tray host requests a refresh, so we keep
    /// every UI input as a plain field and let the trait methods
    /// transform them into menu items / icon pixmaps lazily.
    struct KsniTray {
        tooltip: String,
        state: TrayState,
        recent: Vec<String>,
        stt_labels: Vec<String>,
        llm_labels: Vec<String>,
        active: (u8, u8),
        update_label: Option<String>,
        microphones: (Vec<String>, u8),
        actions: mpsc::UnboundedSender<TrayAction>,
    }

    impl ksni::Tray for KsniTray {
        fn id(&self) -> String {
            // Unique-per-application id; keeping it stable across
            // sessions so panel hosts can persist position / order.
            "fono".into()
        }

        fn title(&self) -> String {
            status_label(self.state).to_string()
        }

        fn tool_tip(&self) -> ToolTip {
            ToolTip {
                title: self.tooltip.clone(),
                description: status_label(self.state).into(),
                ..Default::default()
            }
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            vec![icon_for(self.state)]
        }

        // Left-click/status activation. Tray hosts that call the SNI
        // `Activate` method (including snixembed) show the same status
        // notification as the explicit menu action; right-click still
        // opens the D-Bus menu through the host's normal ContextMenu
        // path.
        fn activate(&mut self, _x: i32, _y: i32) {
            let _ = self.actions.send(TrayAction::ShowStatus);
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            build_menu(self)
        }
    }

    /// Spawn the SNI tray task. Returns `true` on success; on failure
    /// the caller falls back to a "no tray, hotkeys still work" path.
    /// Caller-side, success means the [`Tray`] handle gets a real
    /// [`mpsc::UnboundedSender<TrayState>`]; failure means it gets
    /// `None` and `set_state` becomes a no-op.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        tooltip: String,
        actions: mpsc::UnboundedSender<TrayAction>,
        state_rx: mpsc::UnboundedReceiver<TrayState>,
        recent_provider: RecentProvider,
        stt_labels: Vec<String>,
        llm_labels: Vec<String>,
        active_provider: ActiveProvider,
        update_provider: UpdateProvider,
        microphones_provider: MicrophonesProvider,
    ) -> bool {
        // We need to be inside a tokio runtime to spawn the ksni
        // service; the daemon always is. Probe `Handle::try_current`
        // and bail cleanly if not (tests / odd embedders).
        if tokio::runtime::Handle::try_current().is_err() {
            tracing::warn!("tray backend skipped: no current tokio runtime");
            return false;
        }
        tokio::spawn(async move {
            if let Err(e) = run(
                tooltip,
                actions,
                state_rx,
                recent_provider,
                stt_labels,
                llm_labels,
                active_provider,
                update_provider,
                microphones_provider,
            )
            .await
            {
                if is_missing_status_notifier_watcher(&e) {
                    notify_missing_status_notifier_watcher();
                    tracing::warn!(
                        "tray unavailable: no StatusNotifierWatcher is registered on the session bus; \
                         start a tray host/watcher (for example KDE Plasma's tray, waybar with tray, \
                         xfce4-panel, or snixembed) or run with --no-tray. Dictation and the overlay \
                         continue without the tray icon."
                    );
                } else {
                    tracing::warn!("tray task exited: {e:#}");
                }
            }
        });
        true
    }

    fn notify_missing_status_notifier_watcher() {
        notify::send(
            MISSING_WATCHER_NOTIFICATION_TITLE,
            MISSING_WATCHER_NOTIFICATION_BODY,
            "dialog-error",
            MISSING_WATCHER_NOTIFICATION_TIMEOUT_MS,
            Urgency::Critical,
        );
    }

    fn is_missing_status_notifier_watcher(err: &anyhow::Error) -> bool {
        let msg = err.to_string();
        msg.contains("org.kde.StatusNotifierWatcher") || msg.contains("StatusNotifierWatcher")
    }

    #[allow(clippy::too_many_arguments)]
    async fn run(
        tooltip: String,
        actions: mpsc::UnboundedSender<TrayAction>,
        mut state_rx: mpsc::UnboundedReceiver<TrayState>,
        recent_provider: RecentProvider,
        stt_labels: Vec<String>,
        llm_labels: Vec<String>,
        active_provider: ActiveProvider,
        update_provider: UpdateProvider,
        microphones_provider: MicrophonesProvider,
    ) -> anyhow::Result<()> {
        let model = KsniTray {
            tooltip,
            state: TrayState::Idle,
            recent: Vec::new(),
            stt_labels,
            llm_labels,
            active: (u8::MAX, u8::MAX),
            update_label: None,
            microphones: (Vec::new(), u8::MAX),
            actions,
        };

        // `TrayMethods::spawn` connects to the session bus, registers
        // with `org.kde.StatusNotifierWatcher`, and returns a handle.
        // On hosts without a watcher (no DISPLAY, no D-Bus session
        // bus, etc.) this errors immediately — we surface it as a
        // warn! and let the rest of the daemon run unaffected.
        let handle: Handle<KsniTray> = model
            .spawn()
            .await
            .map_err(|e| anyhow::anyhow!("ksni::Tray::spawn failed: {e}"))?;

        tracing::debug!("tray icon ready (SNI)");

        // Poll providers every 2 seconds and push the diff into the
        // ksni model. Cheap (history read + a config snapshot read)
        // but skip when nothing changed so we don't churn KDE/GNOME
        // indicator state.
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                Some(state) = state_rx.recv() => {
                    handle.update(|t: &mut KsniTray| t.state = state).await;
                }
                _ = interval.tick() => {
                    let recent = recent_provider();
                    let active = active_provider();
                    let upd = update_provider();
                    let mics = microphones_provider();
                    handle.update(move |t: &mut KsniTray| {
                        if t.recent != recent { t.recent = recent; }
                        if t.active != active { t.active = active; }
                        if t.update_label != upd { t.update_label = upd; }
                        if t.microphones != mics { t.microphones = mics; }
                    }).await;
                }
                else => break,
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines, clippy::vec_init_then_push)]
    fn build_menu(t: &KsniTray) -> Vec<MenuItem<KsniTray>> {
        let mut items: Vec<MenuItem<KsniTray>> = Vec::new();

        // Status row (disabled, informational).
        items.push(
            StandardItem {
                label: status_label(t.state).into(),
                enabled: false,
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);

        items.push(
            StandardItem {
                label: "Toggle recording  (F9)".into(),
                activate: send_action(TrayAction::ToggleRecording),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Pause hotkeys".into(),
                activate: send_action(TrayAction::Pause),
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);

        // Recent transcriptions submenu.
        let mut recent_items: Vec<MenuItem<KsniTray>> = Vec::new();
        if t.recent.is_empty() {
            recent_items.push(
                StandardItem {
                    label: "(no transcriptions yet)".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        } else {
            for (i, label) in t.recent.iter().take(RECENT_SLOTS).enumerate() {
                let action = TrayAction::PasteHistory(i);
                recent_items.push(
                    StandardItem {
                        label: format!("{}. {}", i + 1, truncate_label(label, 60)),
                        activate: send_action(action),
                        ..Default::default()
                    }
                    .into(),
                );
            }
        }
        items.push(
            SubMenu {
                label: "Recent transcriptions".into(),
                submenu: recent_items,
                ..Default::default()
            }
            .into(),
        );

        // STT backend submenu.
        let stt_items: Vec<MenuItem<KsniTray>> = t
            .stt_labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let active = u8::try_from(i).is_ok_and(|i_u8| i_u8 == t.active.0);
                let prefix = if active { "● " } else { "  " };
                let idx_u8 = u8::try_from(i).unwrap_or(u8::MAX);
                StandardItem {
                    label: format!("{prefix}{label}"),
                    activate: send_action(TrayAction::UseStt(idx_u8)),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        items.push(
            SubMenu {
                label: "STT backend".into(),
                submenu: stt_items,
                ..Default::default()
            }
            .into(),
        );

        // LLM backend submenu.
        let llm_items: Vec<MenuItem<KsniTray>> = t
            .llm_labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let active = u8::try_from(i).is_ok_and(|i_u8| i_u8 == t.active.1);
                let prefix = if active { "● " } else { "  " };
                let idx_u8 = u8::try_from(i).unwrap_or(u8::MAX);
                StandardItem {
                    label: format!("{prefix}{label}"),
                    activate: send_action(TrayAction::UseLlm(idx_u8)),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        items.push(
            SubMenu {
                label: "LLM backend".into(),
                submenu: llm_items,
                ..Default::default()
            }
            .into(),
        );

        // Microphone submenu — only when the daemon supplied at least
        // one Pulse/PipeWire device. Empty list means we're on a
        // cpal-only host; hide the menu entirely so we don't offer a
        // switch we can't honour.
        if !t.microphones.0.is_empty() {
            let auto_active = t.microphones.1 == u8::MAX;
            let mut mic_items: Vec<MenuItem<KsniTray>> = Vec::new();
            mic_items.push(
                StandardItem {
                    label: if auto_active {
                        "● Auto (system default)".into()
                    } else {
                        "  Auto (system default)".into()
                    },
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
            mic_items.push(MenuItem::Separator);
            for (i, name) in t.microphones.0.iter().take(MIC_SLOTS).enumerate() {
                let active = u8::try_from(i).is_ok_and(|i_u8| i_u8 == t.microphones.1);
                let prefix = if active { "● " } else { "  " };
                let idx_u8 = u8::try_from(i).unwrap_or(u8::MAX);
                mic_items.push(
                    StandardItem {
                        label: format!("{prefix}{}", truncate_label(name, 60)),
                        activate: send_action(TrayAction::SetInputDevice(idx_u8)),
                        ..Default::default()
                    }
                    .into(),
                );
            }
            items.push(
                SubMenu {
                    label: "Microphone".into(),
                    submenu: mic_items,
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(MenuItem::Separator);

        // Update entry — surfaced only when the background checker
        // has detected a newer release. Hidden otherwise so users
        // never see a passive "Check for updates…" button.
        if let Some(label) = t.update_label.as_ref() {
            items.push(
                StandardItem {
                    label: label.clone(),
                    activate: send_action(TrayAction::ApplyUpdate),
                    ..Default::default()
                }
                .into(),
            );
        }

        items.push(
            StandardItem {
                label: "Edit config".into(),
                activate: send_action(TrayAction::OpenConfig),
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit".into(),
                activate: send_action(TrayAction::Quit),
                ..Default::default()
            }
            .into(),
        );

        items
    }

    /// Build a menu-item activate callback that fires `action` on the
    /// tray's action channel. The closure ignores the `&mut KsniTray`
    /// argument because every action is a pure outbound message — the
    /// daemon owns the state machine, not the tray.
    fn send_action(action: TrayAction) -> Box<dyn Fn(&mut KsniTray) + Send + Sync + 'static> {
        Box::new(move |t: &mut KsniTray| {
            let _ = t.actions.send(action);
        })
    }

    fn truncate_label(s: &str, max_chars: usize) -> String {
        let trimmed = s.replace('\n', " ");
        let trimmed = trimmed.trim();
        if trimmed.chars().count() <= max_chars {
            trimmed.to_string()
        } else {
            let mut out: String = trimmed.chars().take(max_chars).collect();
            out.push('…');
            out
        }
    }

    fn status_label(state: TrayState) -> &'static str {
        match state {
            TrayState::Idle => "Fono — idle",
            TrayState::Recording => "Fono — recording",
            TrayState::Processing => "Fono — processing",
            TrayState::Paused => "Fono — paused",
        }
    }

    /// Solid-colour 32x32 ARGB icon tinted by FSM state. Generated
    /// in-code so we don't need a PNG at packaging time. SNI's
    /// pixmap format is ARGB32 in network byte order (A, R, G, B);
    /// not RGBA — the byte order is the one bit easy to get wrong.
    fn icon_for(state: TrayState) -> ksni::Icon {
        const SIZE: i32 = 32;
        let (r, g, b) = match state {
            TrayState::Idle => (0x3b, 0x82, 0xf6),       // blue
            TrayState::Recording => (0xef, 0x44, 0x44),  // red
            TrayState::Processing => (0xf5, 0x9e, 0x0b), // amber
            TrayState::Paused => (0x6b, 0x72, 0x80),     // grey
        };
        let mut data = Vec::with_capacity((SIZE * SIZE * 4) as usize);
        let cx = SIZE / 2;
        let cy = SIZE / 2;
        let radius = (SIZE / 2) - 2;
        for y in 0..SIZE {
            for x in 0..SIZE {
                let dx = x - cx;
                let dy = y - cy;
                let inside = dx * dx + dy * dy <= radius * radius;
                if inside {
                    data.extend_from_slice(&[0xff, r, g, b]);
                } else {
                    data.extend_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        ksni::Icon {
            width: SIZE,
            height: SIZE,
            data,
        }
    }
}
