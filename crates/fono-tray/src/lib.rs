// SPDX-License-Identifier: GPL-3.0-only
//! Tray-icon integration. Phase 7 Task 7.1.
//!
//! When the `tray-backend` feature is enabled (default for the `fono`
//! binary on Linux), this crate spawns a real system-tray icon on a
//! dedicated thread. Without the feature, a no-op [`Tray`] keeps the code
//! paths compiling for headless builds and cross-platform CI.
//!
//! UX features beyond a static menu:
//!
//! - **Recent transcriptions submenu** — the last `RECENT_SLOTS`
//!   dictations are shown as clickable menu items. Clicking one fires
//!   [`TrayAction::PasteHistory`] with the slot index (0 = newest); the
//!   daemon then re-injects/copies that text. This is the clipit-style
//!   workflow users asked for.

use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use tokio::sync::mpsc;

/// How many recent transcriptions to surface in the tray menu.
pub const RECENT_SLOTS: usize = 10;

/// Provider that returns the most recent transcription labels (newest
/// first) for display in the tray's "Recent" submenu. Called from the
/// tray thread on a poll interval.
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
    OpenConfig,
    Quit,
}

/// Handle returned by [`Tray::spawn`]. Dropping it tears down the tray
/// thread on a best-effort basis.
pub struct Tray {
    shared_state: Arc<AtomicU8>,
    #[allow(dead_code)] // only the real backend reads this
    actions_rx_sentinel: (),
}

impl Tray {
    /// Update the tray icon to reflect the given FSM state.
    pub fn set_state(&self, state: TrayState) {
        self.shared_state.store(state as u8, Ordering::Relaxed);
        #[cfg(feature = "tray-backend")]
        backend::request_redraw(state);
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

/// Spawn the tray on a dedicated thread.
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
#[allow(unused_variables, clippy::too_many_arguments)]
pub fn spawn(
    tooltip: &str,
    recent_provider: RecentProvider,
    stt_labels: Vec<String>,
    llm_labels: Vec<String>,
    active_provider: ActiveProvider,
) -> (Tray, mpsc::UnboundedReceiver<TrayAction>) {
    let shared = Arc::new(AtomicU8::new(TrayState::Idle as u8));
    let (tx, rx) = mpsc::unbounded_channel();

    #[cfg(feature = "tray-backend")]
    {
        if let Err(e) = backend::spawn(
            tooltip.to_string(),
            Arc::clone(&shared),
            tx,
            recent_provider,
            stt_labels,
            llm_labels,
            active_provider,
        ) {
            tracing::warn!("tray backend failed to start: {e:#}; continuing without tray");
        }
    }

    (
        Tray {
            shared_state: shared,
            actions_rx_sentinel: (),
        },
        rx,
    )
}

// -------------------------------------------------------------------------
// Real backend (Linux / libappindicator via `tray-icon`).
// -------------------------------------------------------------------------

#[cfg(feature = "tray-backend")]
mod backend {
    use super::{ActiveProvider, RecentProvider, TrayAction, TrayState, RECENT_SLOTS};
    use anyhow::{Context, Result};
    use std::sync::{
        atomic::{AtomicU8, Ordering},
        Arc, OnceLock,
    };
    use tokio::sync::mpsc;
    use tray_icon::{
        menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
        TrayIconBuilder, TrayIconEvent,
    };

    struct MenuIds {
        status: u32,
        toggle: u32,
        pause: u32,
        config: u32,
        quit: u32,
        recent_slots: [u32; RECENT_SLOTS],
        stt_slots: Vec<u32>,
        llm_slots: Vec<u32>,
    }

    static MENU_IDS: OnceLock<MenuIds> = OnceLock::new();

    pub fn request_redraw(_state: TrayState) {
        // The actual redraw happens on the tray (GTK) thread which polls
        // the shared AtomicU8 every 50ms and calls TrayIcon::set_icon
        // when the state changes. Nothing to do here — set_state has
        // already updated the atomic.
    }

    pub fn spawn(
        tooltip: String,
        shared: Arc<AtomicU8>,
        actions: mpsc::UnboundedSender<TrayAction>,
        recent_provider: RecentProvider,
        stt_labels: Vec<String>,
        llm_labels: Vec<String>,
        active_provider: ActiveProvider,
    ) -> Result<()> {
        std::thread::Builder::new()
            .name("fono-tray".into())
            .spawn(move || {
                if let Err(e) = run(
                    &tooltip,
                    shared,
                    actions,
                    recent_provider,
                    stt_labels,
                    llm_labels,
                    active_provider,
                ) {
                    tracing::warn!("tray thread exited: {e:#}");
                }
            })
            .context("spawn tray thread")?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[allow(clippy::too_many_lines, clippy::too_many_arguments)]
    fn run(
        tooltip: &str,
        shared: Arc<AtomicU8>,
        actions: mpsc::UnboundedSender<TrayAction>,
        recent_provider: RecentProvider,
        stt_labels: Vec<String>,
        llm_labels: Vec<String>,
        active_provider: ActiveProvider,
    ) -> Result<()> {
        // tray-icon uses gtk on Linux and requires its main loop.
        gtk::init().context("gtk::init() failed — is gtk+-3.0 installed?")?;

        let (menu, status_item, recent_items, stt_items, llm_items) =
            build_menu(&stt_labels, &llm_labels)?;
        let tray = TrayIconBuilder::new()
            .with_tooltip(tooltip)
            .with_menu(Box::new(menu))
            .with_icon(icon_for(TrayState::Idle))
            .build()
            .context("TrayIconBuilder::build() failed — is libappindicator3 installed?")?;

        // Forward menu click events into the action channel and repaint
        // the icon when the FSM state changes. We poll the tray-icon
        // crate's crossbeam channels and the shared state from a 50 ms
        // timeout instead of `glib::idle_add_local`, which would re-fire
        // immediately on every main-loop iteration and pin a CPU at 100%.
        let mut last_state: u8 = TrayState::Idle as u8;
        let mut last_recent: Vec<String> = Vec::new();
        let mut last_active: (u8, u8) = (u8::MAX, u8::MAX);
        let mut tick: u32 = 0;
        glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            while let Ok(ev) = MenuEvent::receiver().try_recv() {
                if let Some(action) = MENU_IDS
                    .get()
                    .and_then(|ids| map_menu_event(ids, ev.id.0.parse::<u32>().unwrap_or(0)))
                {
                    let _ = actions.send(action);
                }
            }
            while TrayIconEvent::receiver().try_recv().is_ok() {
                // left-click etc. — ignored for now, menu handles it.
            }
            let cur = shared.load(Ordering::Relaxed);
            if cur != last_state {
                last_state = cur;
                let st = decode_state(cur);
                if let Err(e) = tray.set_icon(Some(icon_for(st))) {
                    tracing::warn!("tray set_icon failed: {e:#}");
                }
                status_item.set_text(status_label(st));
            }
            // Refresh the Recent submenu and STT/LLM active markers
            // every ~2 s. Cheap (history read + a single config snapshot
            // read) but skip when nothing changed so we don't churn
            // KDE/GNOME indicator state.
            tick = tick.wrapping_add(1);
            if tick % 40 == 0 {
                let next = recent_provider();
                if next != last_recent {
                    update_recent(&recent_items, &next);
                    last_recent = next;
                }
                let active = active_provider();
                if active != last_active {
                    update_active(&stt_items, &stt_labels, active.0);
                    update_active(&llm_items, &llm_labels, active.1);
                    last_active = active;
                }
            }
            glib::ControlFlow::Continue
        });

        tracing::info!("tray icon ready");
        gtk::main();
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    #[allow(clippy::too_many_arguments)]
    fn run(
        tooltip: &str,
        _shared: Arc<AtomicU8>,
        _actions: mpsc::UnboundedSender<TrayAction>,
        _recent_provider: RecentProvider,
        _stt_labels: Vec<String>,
        _llm_labels: Vec<String>,
        _active_provider: ActiveProvider,
    ) -> Result<()> {
        let _tray = TrayIconBuilder::new()
            .with_tooltip(tooltip)
            .with_icon(icon_for(TrayState::Idle))
            .build()
            .context("TrayIconBuilder::build() failed")?;
        loop {
            std::thread::park();
        }
    }

    type MenuParts = (
        Menu,
        MenuItem,
        [MenuItem; RECENT_SLOTS],
        Vec<MenuItem>,
        Vec<MenuItem>,
    );

    fn build_menu(stt_labels: &[String], llm_labels: &[String]) -> Result<MenuParts> {
        let menu = Menu::new();
        let status = MenuItem::new(status_label(TrayState::Idle), false, None);
        let toggle = MenuItem::new("Toggle recording  (Ctrl+Alt+Space)", true, None);
        let pause = MenuItem::new("Pause hotkeys", true, None);

        // Recent transcriptions submenu — pre-allocate `RECENT_SLOTS`
        // items so we can refresh labels in place rather than rebuilding
        // the menu (which causes flicker on KDE/GNOME indicators).
        let recent_submenu = Submenu::new("Recent transcriptions", true);
        let recent_items: [MenuItem; RECENT_SLOTS] =
            std::array::from_fn(|_| MenuItem::new("(empty)", false, None));
        for it in &recent_items {
            recent_submenu.append(it).ok();
        }

        // STT / LLM submenus — one MenuItem per backend label. The
        // active item gets a leading "● " prefix; others get "  ".
        // Prefix migration happens in `update_active` on every poll.
        let stt_submenu = Submenu::new("STT backend", true);
        let stt_items: Vec<MenuItem> = stt_labels
            .iter()
            .map(|s| MenuItem::new(format!("  {s}"), true, None))
            .collect();
        for it in &stt_items {
            stt_submenu.append(it).ok();
        }
        let llm_submenu = Submenu::new("LLM backend", true);
        let llm_items: Vec<MenuItem> = llm_labels
            .iter()
            .map(|s| MenuItem::new(format!("  {s}"), true, None))
            .collect();
        for it in &llm_items {
            llm_submenu.append(it).ok();
        }

        let config = MenuItem::new("Edit config", true, None);
        let quit = MenuItem::new("Quit", true, None);

        menu.append(&status)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&toggle)?;
        menu.append(&pause)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&recent_submenu)?;
        menu.append(&stt_submenu)?;
        menu.append(&llm_submenu)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&config)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&quit)?;

        let recent_slots: [u32; RECENT_SLOTS] = std::array::from_fn(|i| id_of(&recent_items[i]));
        let stt_slots: Vec<u32> = stt_items.iter().map(id_of).collect();
        let llm_slots: Vec<u32> = llm_items.iter().map(id_of).collect();

        let _ = MENU_IDS.set(MenuIds {
            status: id_of(&status),
            toggle: id_of(&toggle),
            pause: id_of(&pause),
            config: id_of(&config),
            quit: id_of(&quit),
            recent_slots,
            stt_slots,
            llm_slots,
        });
        Ok((menu, status, recent_items, stt_items, llm_items))
    }

    fn update_recent(items: &[MenuItem; RECENT_SLOTS], labels: &[String]) {
        for (i, item) in items.iter().enumerate() {
            if let Some(label) = labels.get(i) {
                item.set_text(format!("{}. {}", i + 1, truncate_label(label, 60)));
                item.set_enabled(true);
            } else {
                item.set_text(if i == 0 {
                    "(no transcriptions yet)"
                } else {
                    ""
                });
                item.set_enabled(false);
            }
        }
    }

    /// Repaint a STT/LLM submenu so the active backend gets a leading
    /// "● " marker and everything else gets two-spaces of padding (so
    /// label widths stay consistent and click targets don't jump).
    fn update_active(items: &[MenuItem], labels: &[String], active_idx: u8) {
        for (i, item) in items.iter().enumerate() {
            let label = labels.get(i).map_or_else(|| "?".to_string(), Clone::clone);
            let prefix = if u8::try_from(i).is_ok_and(|i_u8| i_u8 == active_idx) {
                "● "
            } else {
                "  "
            };
            item.set_text(format!("{prefix}{label}"));
        }
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

    fn decode_state(raw: u8) -> TrayState {
        match raw {
            1 => TrayState::Recording,
            2 => TrayState::Processing,
            3 => TrayState::Paused,
            _ => TrayState::Idle,
        }
    }

    fn id_of(item: &MenuItem) -> u32 {
        item.id().0.parse::<u32>().unwrap_or(0)
    }

    fn map_menu_event(ids: &MenuIds, id: u32) -> Option<TrayAction> {
        if id == ids.status {
            return Some(TrayAction::ShowStatus);
        }
        if id == ids.toggle {
            return Some(TrayAction::ToggleRecording);
        }
        if id == ids.pause {
            return Some(TrayAction::Pause);
        }
        if id == ids.config {
            return Some(TrayAction::OpenConfig);
        }
        if id == ids.quit {
            return Some(TrayAction::Quit);
        }
        for (i, slot_id) in ids.recent_slots.iter().enumerate() {
            if id == *slot_id {
                return Some(TrayAction::PasteHistory(i));
            }
        }
        for (i, slot_id) in ids.stt_slots.iter().enumerate() {
            if id == *slot_id {
                return Some(TrayAction::UseStt(u8::try_from(i).unwrap_or(u8::MAX)));
            }
        }
        for (i, slot_id) in ids.llm_slots.iter().enumerate() {
            if id == *slot_id {
                return Some(TrayAction::UseLlm(u8::try_from(i).unwrap_or(u8::MAX)));
            }
        }
        None
    }

    /// Solid-colour 32x32 icon tinted by FSM state. Keeping the icon
    /// generated in-code means we don't need a PNG at packaging time.
    fn icon_for(state: TrayState) -> tray_icon::Icon {
        const SIZE: u32 = 32;
        let (r, g, b) = match state {
            TrayState::Idle => (0x3b, 0x82, 0xf6),       // blue
            TrayState::Recording => (0xef, 0x44, 0x44),  // red
            TrayState::Processing => (0xf5, 0x9e, 0x0b), // amber
            TrayState::Paused => (0x6b, 0x72, 0x80),     // grey
        };
        let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
        let cx = SIZE as i32 / 2;
        let cy = SIZE as i32 / 2;
        let radius = (SIZE as i32 / 2) - 2;
        for y in 0..SIZE as i32 {
            for x in 0..SIZE as i32 {
                let dx = x - cx;
                let dy = y - cy;
                let inside = dx * dx + dy * dy <= radius * radius;
                if inside {
                    rgba.extend_from_slice(&[r, g, b, 0xff]);
                } else {
                    rgba.extend_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        tray_icon::Icon::from_rgba(rgba, SIZE, SIZE).expect("valid icon bytes")
    }
}
