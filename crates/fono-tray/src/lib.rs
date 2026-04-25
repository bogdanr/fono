// SPDX-License-Identifier: GPL-3.0-only
//! Tray-icon integration. Phase 7 Task 7.1.
//!
//! When the `tray-backend` feature is enabled (default for the `fono`
//! binary on Linux), this crate spawns a real system-tray icon on a
//! dedicated thread. Without the feature, a no-op [`Tray`] keeps the code
//! paths compiling for headless builds and cross-platform CI.

use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use tokio::sync::mpsc;

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
    OpenHistory,
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
#[allow(unused_variables)]
pub fn spawn(tooltip: &str) -> (Tray, mpsc::UnboundedReceiver<TrayAction>) {
    let shared = Arc::new(AtomicU8::new(TrayState::Idle as u8));
    let (tx, rx) = mpsc::unbounded_channel();

    #[cfg(feature = "tray-backend")]
    {
        if let Err(e) = backend::spawn(tooltip.to_string(), Arc::clone(&shared), tx) {
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
    use super::{TrayAction, TrayState};
    use anyhow::{Context, Result};
    use std::sync::{
        atomic::{AtomicU8, Ordering},
        Arc, OnceLock,
    };
    use tokio::sync::mpsc;
    use tray_icon::{
        menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
        TrayIconBuilder, TrayIconEvent,
    };

    struct MenuIds {
        status: u32,
        toggle: u32,
        pause: u32,
        history: u32,
        config: u32,
        quit: u32,
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
    ) -> Result<()> {
        std::thread::Builder::new()
            .name("fono-tray".into())
            .spawn(move || {
                if let Err(e) = run(&tooltip, shared, actions) {
                    tracing::warn!("tray thread exited: {e:#}");
                }
            })
            .context("spawn tray thread")?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn run(
        tooltip: &str,
        shared: Arc<AtomicU8>,
        actions: mpsc::UnboundedSender<TrayAction>,
    ) -> Result<()> {
        // tray-icon uses gtk on Linux and requires its main loop.
        gtk::init().context("gtk::init() failed — is gtk+-3.0 installed?")?;

        let (menu, status_item) = build_menu(&actions)?;
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
            glib::ControlFlow::Continue
        });

        tracing::info!("tray icon ready");
        gtk::main();
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn run(
        tooltip: &str,
        _shared: Arc<AtomicU8>,
        _actions: mpsc::UnboundedSender<TrayAction>,
    ) -> Result<()> {
        let _tray = TrayIconBuilder::new()
            .with_tooltip(tooltip)
            .with_icon(icon_for(TrayState::Idle))
            .build()
            .context("TrayIconBuilder::build() failed")?;
        // Block forever; the underlying platform loop is driven elsewhere.
        loop {
            std::thread::park();
        }
    }

    fn build_menu(_actions: &mpsc::UnboundedSender<TrayAction>) -> Result<(Menu, MenuItem)> {
        let menu = Menu::new();
        let status = MenuItem::new(status_label(TrayState::Idle), false, None);
        let toggle = MenuItem::new("Toggle recording  (Ctrl+Alt+Space)", true, None);
        let pause = MenuItem::new("Pause hotkeys", true, None);
        let history = MenuItem::new("Open history", true, None);
        let config = MenuItem::new("Edit config", true, None);
        let quit = MenuItem::new("Quit", true, None);

        menu.append(&status)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&toggle)?;
        menu.append(&pause)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&history)?;
        menu.append(&config)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&quit)?;

        let _ = MENU_IDS.set(MenuIds {
            status: id_of(&status),
            toggle: id_of(&toggle),
            pause: id_of(&pause),
            history: id_of(&history),
            config: id_of(&config),
            quit: id_of(&quit),
        });
        Ok((menu, status))
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
            Some(TrayAction::ShowStatus)
        } else if id == ids.toggle {
            Some(TrayAction::ToggleRecording)
        } else if id == ids.pause {
            Some(TrayAction::Pause)
        } else if id == ids.history {
            Some(TrayAction::OpenHistory)
        } else if id == ids.config {
            Some(TrayAction::OpenConfig)
        } else if id == ids.quit {
            Some(TrayAction::Quit)
        } else {
            None
        }
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
