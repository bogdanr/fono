// SPDX-License-Identifier: GPL-3.0-only
//! `winit` + `softbuffer` overlay backend — the original X11 path.
//!
//! After the 2026-05-19 winit Wayland strip (workspace `winit` dep
//! lost its `wayland*` features) this backend is X11-only. On
//! Wayland-only hosts (`DISPLAY` unset) `try_spawn` fails fast and
//! the selection logic in [`crate::backend`] falls through to the
//! Wayland-native backends.
//!
//! The renderer is in [`crate::renderer`]. This module owns only
//! the winit event-loop machinery, the override-redirect window
//! attributes, and the `softbuffer` framebuffer present.

#![allow(clippy::items_after_statements, clippy::too_many_lines)]

use std::num::NonZeroU32;
use std::sync::mpsc::{channel, Receiver};

use fono_core::config::WaveformStyle;

use crate::backend::{BackendCapabilities, BackendError, BackendId, OverlayCmd, SpawnedBackend};
use crate::renderer::{
    self, RendererState, ACCENT_WIDTH, BOTTOM_OFFSET, PADDING_BOT, PADDING_TOP, PADDING_X,
    STATUS_FONT_PX, STATUS_TO_TEXT, WIN_MIN_HEIGHT, WIN_WAVEFORM_HEIGHT, WIN_WIDTH,
};
use crate::OverlayState;

/// Returns `true` iff `libxkbcommon-x11.so.0` (or the unversioned
/// alias `libxkbcommon-x11.so`) can be dlopen'd from the dynamic
/// loader's search path. Used as a preflight to avoid winit's
/// hard-panic on missing helper.
fn libxkbcommon_x11_loadable() -> bool {
    // SAFETY: `Library::new` is sound; we drop the handle immediately
    // so no symbols are resolved into our address space beyond the
    // load probe itself.
    unsafe {
        libloading::Library::new("libxkbcommon-x11.so.0").is_ok()
            || libloading::Library::new("libxkbcommon-x11.so").is_ok()
    }
}

pub fn try_spawn(style: WaveformStyle) -> Result<SpawnedBackend, BackendError> {
    if std::env::var_os("DISPLAY").is_none() {
        return Err(BackendError::NotAvailable("DISPLAY unset (X11 backend requires Xorg)".into()));
    }
    // Preflight libxkbcommon-x11. winit's X11 backend dlopens it
    // deep inside `EventLoop::build()` via `xkbcommon-dl`, and the
    // failure path there is a hard `panic!` that kills the overlay
    // thread (visible to users as a 2 s spawn-timeout + stderr noise).
    // The stock Ubuntu 26.04 Wayland live image ships only the core
    // `libxkbcommon.so.0`, not the X11 helper — and any minimal
    // Wayland-only install can hit the same wall. Probe ourselves
    // and surface a clean, actionable NotAvailable instead.
    if !libxkbcommon_x11_loadable() {
        return Err(BackendError::NotAvailable(
            "libxkbcommon-x11 not installed — install your distro's package \
             (`libxkbcommon-x11-0` on Debian/Ubuntu, `libxkbcommon-x11` on \
             Fedora/RHEL/Alpine, `libxkbcommon` on Arch)"
                .into(),
        ));
    }
    let (tx, rx) = channel::<OverlayCmd>();
    let (proxy_tx, proxy_rx) =
        std::sync::mpsc::channel::<Result<winit::event_loop::EventLoopProxy<()>, String>>();
    let join = std::thread::Builder::new()
        .name("fono-overlay-x11".into())
        .spawn(move || {
            if let Err(e) = run_event_loop(rx, proxy_tx, style) {
                tracing::warn!("overlay(x11): event loop ended with error: {e:#}");
            }
        })
        .map_err(|e| BackendError::SpawnFailed(format!("spawn fono-overlay-x11 thread: {e}")))?;
    let proxy = match proxy_rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(Ok(p)) => p,
        Ok(Err(msg)) => {
            let _ = join.join();
            return Err(BackendError::SpawnFailed(msg));
        }
        Err(e) => {
            return Err(BackendError::SpawnFailed(format!(
                "X11 event loop did not become ready within 2s: {e}"
            )));
        }
    };
    let waker: Box<dyn Fn() + Send + Sync> = Box::new(move || {
        let _ = proxy.send_event(());
    });
    Ok(SpawnedBackend {
        id: BackendId::X11OverrideRedirect,
        capabilities: BackendCapabilities {
            transparency: true,
            client_positioning: true,
            focus_passthrough: true,
            click_passthrough: true,
        },
        tx,
        waker,
        join,
    })
}

fn run_event_loop(
    rx: Receiver<OverlayCmd>,
    proxy_tx: std::sync::mpsc::Sender<Result<winit::event_loop::EventLoopProxy<()>, String>>,
    style: WaveformStyle,
) -> Result<(), String> {
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::ActiveEventLoop;
    #[cfg(not(target_os = "linux"))]
    use winit::event_loop::EventLoop;
    use winit::window::{Window, WindowId, WindowLevel};

    let event_loop = {
        #[cfg(target_os = "linux")]
        {
            use winit::event_loop::EventLoop;
            use winit::platform::x11::EventLoopBuilderExtX11;
            let mut builder = EventLoop::<()>::with_user_event();
            <_ as EventLoopBuilderExtX11>::with_any_thread(&mut builder, true);
            builder.build().map_err(|e| format!("EventLoop::with_user_event().build(): {e}"))
        }
        #[cfg(not(target_os = "linux"))]
        {
            EventLoop::<()>::with_user_event()
                .build()
                .map_err(|e| format!("EventLoop::with_user_event().build(): {e}"))
        }
    };
    let event_loop = match event_loop {
        Ok(event_loop) => event_loop,
        Err(msg) => {
            let _ = proxy_tx.send(Err(msg.clone()));
            return Err(msg);
        }
    };
    let _ = proxy_tx.send(Ok(event_loop.create_proxy()));
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);

    struct App {
        window: Option<std::sync::Arc<Window>>,
        surface: Option<softbuffer::Surface<std::sync::Arc<Window>, std::sync::Arc<Window>>>,
        renderer: RendererState,
        rx: Receiver<OverlayCmd>,
    }

    impl App {
        fn ensure_window(&mut self, el: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let initial_h = if renderer::is_text_style(self.renderer.style) {
                WIN_MIN_HEIGHT
            } else {
                WIN_WAVEFORM_HEIGHT
            };
            let attrs = Window::default_attributes()
                .with_title("Fono")
                .with_decorations(false)
                .with_resizable(false)
                .with_transparent(true)
                .with_window_level(WindowLevel::AlwaysOnTop)
                .with_inner_size(winit::dpi::LogicalSize::new(WIN_WIDTH, initial_h))
                .with_active(false)
                .with_visible(false);
            #[cfg(all(unix, not(target_os = "macos")))]
            let attrs = {
                use winit::platform::x11::{WindowAttributesExtX11, WindowType};
                attrs
                    .with_x11_window_type(vec![WindowType::Notification])
                    .with_override_redirect(true)
            };
            let win = el.create_window(attrs).map_or_else(
                |e| {
                    tracing::warn!("overlay(x11): create_window failed: {e}");
                    None
                },
                |w| Some(std::sync::Arc::new(w)),
            );
            if let Some(w) = win {
                if let Some(monitor) = w.current_monitor().or_else(|| el.primary_monitor()) {
                    let mon_size = monitor.size();
                    let win_size = w.outer_size();
                    let x = (mon_size.width.saturating_sub(win_size.width)) / 2;
                    let y = mon_size.height.saturating_sub(win_size.height + BOTTOM_OFFSET);
                    w.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
                }
                let ctx = match softbuffer::Context::new(std::sync::Arc::clone(&w)) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("overlay(x11): softbuffer ctx: {e}");
                        return;
                    }
                };
                let surface = match softbuffer::Surface::new(&ctx, std::sync::Arc::clone(&w)) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("overlay(x11): softbuffer surface: {e}");
                        return;
                    }
                };
                self.window = Some(w);
                self.surface = Some(surface);
            }
        }

        fn reposition_for_height(&self) {
            let Some(w) = self.window.as_ref() else { return };
            if let Some(monitor) = w.current_monitor() {
                let mon_size = monitor.size();
                let win_size = w.outer_size();
                let x = (mon_size.width.saturating_sub(win_size.width)) / 2;
                let y = mon_size.height.saturating_sub(win_size.height + BOTTOM_OFFSET);
                w.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
            }
        }
    }

    impl ApplicationHandler<()> for App {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            // X11: eager window creation. `with_visible(false)` +
            // override-redirect keeps the window unmapped until the
            // first SetState flips it on.
            self.ensure_window(el);
        }

        fn user_event(&mut self, _el: &ActiveEventLoop, _ev: ()) {}

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
            let mut needs_redraw = false;
            let mut needs_resize = false;
            while let Ok(cmd) = self.rx.try_recv() {
                match cmd {
                    OverlayCmd::SetState(s) => {
                        self.renderer.set_state(s);
                        if matches!(s, OverlayState::Hidden) {
                            if let Some(w) = self.window.as_ref() {
                                w.set_visible(false);
                            }
                        } else {
                            self.ensure_window(el);
                            if let Some(w) = self.window.as_ref() {
                                w.set_visible(true);
                                needs_redraw = true;
                                // A state transition changes the target
                                // height (e.g. a tall reply panel → a
                                // short waveform on the next turn). Without
                                // this the window keeps the previous turn's
                                // inner size and re-opens tall (GitHub #15
                                // follow-up). Mirrors the Wayland backend,
                                // which resizes on every SetState.
                                needs_resize = true;
                            }
                        }
                    }
                    OverlayCmd::UpdateText(t) => {
                        if self.renderer.update_text(t) && self.window.is_some() {
                            needs_redraw = true;
                            needs_resize = true;
                        }
                    }
                    OverlayCmd::AudioLevel(v) => {
                        self.renderer.push_level(v);
                        if self.window.is_some() && self.renderer.is_visible() {
                            needs_redraw = true;
                        }
                    }
                    OverlayCmd::AudioSamples(s) => {
                        self.renderer.push_samples(s);
                        if self.renderer.samples_push_needs_redraw()
                            && self.window.is_some()
                            && self.renderer.is_visible()
                        {
                            needs_redraw = true;
                        }
                    }
                    OverlayCmd::FftBins(bins) => {
                        self.renderer.push_fft_bins(bins);
                        if let Some(window) = self.window.as_ref() {
                            let scale = window.scale_factor() as f32;
                            let size = window.inner_size();
                            let cx0 = ((PADDING_X + ACCENT_WIDTH) * scale).round() as i32;
                            let cx1 = PADDING_X.mul_add(-scale, size.width as f32).round() as i32;
                            let pad_top = PADDING_TOP * scale;
                            let cy0 = STATUS_TO_TEXT
                                .mul_add(scale, STATUS_FONT_PX.mul_add(scale, pad_top))
                                .round() as i32;
                            let cy1 =
                                PADDING_BOT.mul_add(-scale, size.height as f32).round() as i32;
                            self.renderer.update_heatmap_cache(cx0, cx1, cy0, cy1);
                        }
                        if self.renderer.fft_push_needs_redraw()
                            && self.window.is_some()
                            && self.renderer.is_visible()
                        {
                            needs_redraw = true;
                        }
                    }
                    OverlayCmd::SetWaveformStyle(style) => {
                        let (changed, crossed) = self.renderer.set_waveform_style(style);
                        if changed {
                            self.renderer.clear_for_style_swap();
                            if self.window.is_some() && self.renderer.is_visible() {
                                needs_redraw = true;
                                if crossed {
                                    needs_resize = true;
                                }
                            }
                            tracing::debug!("overlay(x11): style -> {style:?}");
                        }
                    }
                    OverlayCmd::SetVolumeBar(mode) => {
                        if self.renderer.set_volume_bar(mode) && self.window.is_some() {
                            needs_redraw = true;
                            needs_resize = true;
                        }
                    }
                    OverlayCmd::GateMetrics { inst_rms, voiced_rms, silence_rms } => {
                        self.renderer.set_gate_metrics(crate::renderer::GateMetrics {
                            inst_rms,
                            voiced_rms,
                            silence_rms,
                        });
                        if self.window.is_some()
                            && self.renderer.is_visible()
                            && matches!(
                                self.renderer.volume_bar,
                                fono_core::config::VolumeBarMode::Advanced
                            )
                        {
                            needs_redraw = true;
                        }
                    }
                    OverlayCmd::Cortex(cmd) => {
                        if self.renderer.push_cortex_cmd(cmd)
                            && self.window.is_some()
                            && self.renderer.is_visible()
                        {
                            needs_redraw = true;
                        }
                    }
                    OverlayCmd::Shutdown => {
                        el.exit();
                        return;
                    }
                }
            }
            if needs_resize {
                if let Some(w) = self.window.as_ref() {
                    let h = self.renderer.target_logical_height();
                    let _ = w.request_inner_size(winit::dpi::LogicalSize::new(WIN_WIDTH, h));
                    self.reposition_for_height();
                }
            }
            if needs_redraw && self.window.is_some() {
                // Synchronous render — see commit history for the
                // rationale on bypassing winit's queued
                // RedrawRequested path under transparent
                // override-redirect windows.
                redraw(self);
            }
            // Self-driven animation pump: the Glass Cortex thinking /
            // speaking phases animate with no incoming data to trigger
            // repaints, so drive them on a ~30 fps timer. Idle,
            // listening (mic-FFT driven) and other styles fall back to
            // `Wait` so a static overlay costs zero CPU.
            if self.renderer.wants_animation_frame() && self.window.is_some() {
                self.renderer.animation_tick();
                redraw(self);
                el.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                    std::time::Instant::now() + std::time::Duration::from_millis(33),
                ));
            } else {
                el.set_control_flow(winit::event_loop::ControlFlow::Wait);
            }
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
        // X11 + override-redirect supports per-pixel alpha via
        // XRender ARGB8888 visuals, so clear to fully transparent;
        // the renderer paints the rounded panel on top.
        buf.fill(0x0000_0000);
        if !app.renderer.is_visible() {
            let _ = buf.present();
            return;
        }
        let scale = window.scale_factor() as f32;
        app.renderer.redraw(&mut buf, w, h, scale);
        let _ = buf.present();
    }

    let mut app = App { window: None, surface: None, renderer: RendererState::new(style), rx };
    event_loop.run_app(&mut app).map_err(|e| format!("run_app: {e}"))
}
