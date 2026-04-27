// SPDX-License-Identifier: GPL-3.0-only
//! `winit` + `softbuffer` overlay window driver. Plan R5.
//!
//! ## Slice A scope
//!
//! The overlay runs in the **daemon process** on a dedicated background
//! thread (subprocess refactor lands in Slice B per ADR 0009). The
//! window:
//!
//! 1. Is borderless, always-on-top, click-through where supported.
//! 2. Fills with a status-coloured background (red while recording,
//!    amber while processing, blue while live-dictating, hidden
//!    otherwise).
//! 3. Pushes the latest preview/finalize text into the window **title**
//!    so the user can see it without us shipping a glyph rasterizer.
//!    Pixel-glyph rendering (cosmic-text/fontdue) is deferred to Slice
//!    B; rationale captured in ADR 0009.
//!
//! The handle returned by [`spawn`] is `Send + Sync` and exposes
//! [`OverlayHandle::set_state`] / [`OverlayHandle::update_text`] /
//! [`OverlayHandle::shutdown`] which marshal commands across to the
//! winit thread via an mpsc channel.

use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::OverlayState;

/// Commands sent from the main thread to the overlay's winit thread.
enum OverlayCmd {
    SetState(OverlayState),
    UpdateText(String),
    Shutdown,
}

/// Handle to a running overlay window. Cheap to clone via [`Arc`].
#[derive(Clone)]
pub struct OverlayHandle {
    inner: Arc<HandleInner>,
}

struct HandleInner {
    tx: Sender<OverlayCmd>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl OverlayHandle {
    pub fn set_state(&self, state: OverlayState) {
        let _ = self.inner.tx.send(OverlayCmd::SetState(state));
    }

    pub fn update_text(&self, text: impl Into<String>) {
        let _ = self.inner.tx.send(OverlayCmd::UpdateText(text.into()));
    }

    /// Stop the overlay and join its thread. Idempotent.
    pub fn shutdown(&self) {
        let _ = self.inner.tx.send(OverlayCmd::Shutdown);
        if let Ok(mut g) = self.inner.join.lock() {
            if let Some(j) = g.take() {
                let _ = j.join();
            }
        }
    }
}

/// Marker type — kept for symmetry with the slim build.
pub struct RealOverlay;

impl RealOverlay {
    /// Spawn a background thread running the overlay's winit event
    /// loop. Returns immediately; the window only becomes visible once
    /// the first non-`Hidden` state arrives.
    pub fn spawn() -> std::io::Result<OverlayHandle> {
        let (tx, rx) = channel::<OverlayCmd>();
        let join = std::thread::Builder::new()
            .name("fono-overlay".into())
            .spawn(move || {
                if let Err(e) = run_event_loop(rx) {
                    tracing::warn!("overlay: event loop ended with error: {e:#}");
                }
            })?;
        Ok(OverlayHandle {
            inner: Arc::new(HandleInner {
                tx,
                join: Mutex::new(Some(join)),
            }),
        })
    }
}

#[allow(clippy::items_after_statements, clippy::too_many_lines)]
fn run_event_loop(rx: std::sync::mpsc::Receiver<OverlayCmd>) -> Result<(), String> {
    use std::num::NonZeroU32;
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::window::{Window, WindowId, WindowLevel};

    let event_loop = EventLoop::new().map_err(|e| format!("EventLoop::new: {e}"))?;
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);

    struct App {
        window: Option<std::sync::Arc<Window>>,
        // softbuffer is single-threaded; lazily constructed on first draw.
        surface: Option<softbuffer::Surface<std::sync::Arc<Window>, std::sync::Arc<Window>>>,
        state: OverlayState,
        text: String,
        rx: std::sync::mpsc::Receiver<OverlayCmd>,
        shutdown: bool,
    }

    impl ApplicationHandler for App {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attrs = Window::default_attributes()
                .with_title("Fono")
                .with_decorations(false)
                .with_resizable(false)
                .with_window_level(WindowLevel::AlwaysOnTop)
                .with_inner_size(winit::dpi::LogicalSize::new(420.0, 60.0))
                .with_visible(false); // hidden until first non-Hidden state
            let win = el.create_window(attrs).map_or_else(
                |e| {
                    tracing::warn!("overlay: create_window failed: {e}");
                    None
                },
                |w| Some(std::sync::Arc::new(w)),
            );
            if let Some(w) = win {
                let ctx = match softbuffer::Context::new(std::sync::Arc::clone(&w)) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("overlay: softbuffer ctx: {e}");
                        return;
                    }
                };
                let surface = match softbuffer::Surface::new(&ctx, std::sync::Arc::clone(&w)) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("overlay: softbuffer surface: {e}");
                        return;
                    }
                };
                self.window = Some(w);
                self.surface = Some(surface);
            }
        }

        fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, ev: WindowEvent) {
            if matches!(ev, WindowEvent::CloseRequested) {
                el.exit();
                return;
            }
            if matches!(ev, WindowEvent::RedrawRequested) {
                redraw(self);
            }
        }

        fn about_to_wait(&mut self, el: &ActiveEventLoop) {
            // Drain pending commands.
            while let Ok(cmd) = self.rx.try_recv() {
                match cmd {
                    OverlayCmd::SetState(s) => {
                        self.state = s;
                        if let Some(w) = self.window.as_ref() {
                            w.set_visible(!matches!(s, OverlayState::Hidden));
                            w.request_redraw();
                        }
                    }
                    OverlayCmd::UpdateText(t) => {
                        self.text = t;
                        if let Some(w) = self.window.as_ref() {
                            // Render text by appending it to the window
                            // title — Slice A simplification (ADR
                            // 0009). Truncate to keep titles sane.
                            let display = if self.text.chars().count() > 80 {
                                let s: String = self.text.chars().take(77).collect();
                                format!("Fono — {s}…")
                            } else {
                                format!("Fono — {}", self.text)
                            };
                            w.set_title(&display);
                            w.request_redraw();
                        }
                    }
                    OverlayCmd::Shutdown => {
                        self.shutdown = true;
                        el.exit();
                        return;
                    }
                }
            }
            // Wait briefly so we can poll the channel without burning CPU.
            // Using `WaitUntil` would be tighter but `Wait` + a periodic
            // user-event nudge is simpler; the orchestrator's update
            // cadence is already low (≤ 10 Hz), so a tiny poll loop is
            // acceptable for Slice A.
            std::thread::sleep(std::time::Duration::from_millis(33));
        }
    }

    fn redraw(app: &mut App) {
        let Some(window) = app.window.as_ref() else {
            return;
        };
        let Some(surface) = app.surface.as_mut() else {
            return;
        };
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        let nzw = NonZeroU32::new(w).unwrap();
        let nzh = NonZeroU32::new(h).unwrap();
        if surface.resize(nzw, nzh).is_err() {
            return;
        }
        let Ok(mut buf) = surface.buffer_mut() else {
            return;
        };
        // BGRA on most platforms via softbuffer (it abstracts to 0xRRGGBB).
        let colour: u32 = match app.state {
            OverlayState::Hidden => 0x0000_0000,
            OverlayState::Recording { .. } => 0x00C0_3030, // red
            OverlayState::Processing => 0x00C0_8030,       // amber
            OverlayState::LiveDictating => 0x0030_60C0,    // blue
        };
        for px in buf.iter_mut() {
            *px = colour;
        }
        let _ = buf.present();
    }

    let mut app = App {
        window: None,
        surface: None,
        state: OverlayState::Hidden,
        text: String::new(),
        rx,
        shutdown: false,
    };
    event_loop
        .run_app(&mut app)
        .map_err(|e| format!("run_app: {e}"))
}
