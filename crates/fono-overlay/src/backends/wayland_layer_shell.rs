// SPDX-License-Identifier: GPL-3.0-only
//! `zwlr_layer_shell_v1` overlay backend — primary Wayland path.
//!
//! Works on every compositor that implements the wlr-protocols layer
//! shell: sway, hyprland, river, KDE Plasma 5.27+, COSMIC, Wayfire,
//! niri, labwc, etc. On Mutter / GNOME the registry walk fails fast
//! (the protocol is absent) and the selection logic in
//! [`crate::backend`] falls through to the xdg fallback.
//!
//! ## Surface model
//!
//! - One `wl_surface` wrapped in a `zwlr_layer_surface_v1`.
//! - `Layer::Top`, `Anchor::BOTTOM`, `keyboard_interactivity = None`.
//! - Margin of [`BOTTOM_OFFSET`] px from the screen edge so the
//!   panel sits where the X11 overlay did.
//! - Empty `wl_region` set as the input region so pointer events
//!   pass through to the window underneath.
//!
//! ## Threading
//!
//! Owns a dedicated OS thread (no Tokio). The thread multiplexes the
//! wayland socket fd and a self-pipe waker via `rustix::event::poll`,
//! processes commands from the orchestrator, repaints when needed.

#![allow(clippy::too_many_lines)]

use std::os::fd::AsFd;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use fono_core::config::WaveformStyle;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_output, wl_region, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle};

use crate::backend::{BackendCapabilities, BackendError, BackendId, OverlayCmd, SpawnedBackend};
use crate::backends::wayland_shm::{
    self, dispatch_wayland, drain_commands, make_waker_closure, poll_event_sources, ShmCanvas,
    Waker,
};
use crate::renderer::{self, RendererState, BOTTOM_OFFSET, WIN_WIDTH};

pub fn try_spawn(style: WaveformStyle) -> Result<SpawnedBackend, BackendError> {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Err(BackendError::NotAvailable(
            "WAYLAND_DISPLAY unset (wlr-layer-shell backend requires Wayland)".into(),
        ));
    }
    // Probe + bind the globals on the spawning thread so we can return
    // a clean `NotAvailable` before spinning the worker. SCTK's binders
    // do a sync roundtrip internally to enumerate the registry.
    let conn = Connection::connect_to_env()
        .map_err(|e| BackendError::NotAvailable(format!("Connection::connect_to_env: {e}")))?;
    let (globals, event_queue) = registry_queue_init(&conn)
        .map_err(|e| BackendError::NotAvailable(format!("registry_queue_init: {e}")))?;
    let qh: QueueHandle<LayerState> = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|e| BackendError::NotAvailable(format!("wl_compositor not bound: {e}")))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| {
        BackendError::NotAvailable(format!("zwlr_layer_shell_v1 not advertised: {e}"))
    })?;
    let shm = Shm::bind(&globals, &qh)
        .map_err(|e| BackendError::NotAvailable(format!("wl_shm not bound: {e}")))?;

    // Build the layer surface up front so the worker thread can drive
    // it immediately.
    let surface = compositor.create_surface(&qh);
    // Empty input region: pointer events fall through to whatever
    // window is underneath.
    let region = compositor.wl_compositor().create_region(&qh, ());
    surface.set_input_region(Some(&region));
    region.destroy();

    let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Top, Some("fono"), None);
    layer.set_anchor(Anchor::BOTTOM);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    let initial_h = renderer::target_height(0).round() as u32;
    layer.set_size(WIN_WIDTH as u32, initial_h);
    layer.set_margin(0, 0, BOTTOM_OFFSET as i32, 0);
    // Initial commit with no buffer attached — the compositor responds
    // with a configure that drives the first paint.
    layer.commit();

    let canvas = ShmCanvas::new(&shm, WIN_WIDTH as u32, initial_h)
        .map_err(|e| BackendError::SpawnFailed(format!("ShmCanvas::new: {e}")))?;

    let waker = Waker::new().map_err(|e| BackendError::SpawnFailed(format!("Waker::new: {e}")))?;
    let waker_closure = make_waker_closure(waker.write);

    let (tx, rx) = channel::<OverlayCmd>();

    let state = LayerState {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        shm,
        compositor,
        layer,
        canvas,
        renderer: RendererState::new(style),
        configured: false,
        scale: 1,
        exit: false,
        pending_size: None,
    };

    let join = std::thread::Builder::new()
        .name("fono-overlay-wlr".into())
        .spawn(move || {
            if let Err(e) = run_loop(conn, event_queue, state, rx, waker.read) {
                tracing::warn!("overlay(wlr): event loop ended: {e:#}");
            }
        })
        .map_err(|e| BackendError::SpawnFailed(format!("spawn fono-overlay-wlr thread: {e}")))?;

    Ok(SpawnedBackend {
        id: BackendId::WlrLayerShell,
        capabilities: BackendCapabilities {
            transparency: true,
            client_positioning: true,
            focus_passthrough: true,
            click_passthrough: true,
        },
        tx,
        waker: waker_closure,
        join,
    })
}

// ---------------------------------------------------------------------------
//  Protocol state
// ---------------------------------------------------------------------------

struct LayerState {
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,
    #[allow(dead_code)]
    compositor: CompositorState,
    layer: LayerSurface,
    canvas: ShmCanvas,
    renderer: RendererState,
    /// True once the compositor has sent the first configure ack.
    /// Until then, painting is unsafe — the surface has no role yet.
    configured: bool,
    /// Integer scale advertised by the output the surface is on.
    scale: i32,
    exit: bool,
    /// Set when the renderer's `target_logical_height()` should be
    /// pushed to the layer surface on the next loop pass.
    pending_size: Option<(u32, u32)>,
}

impl LayerState {
    fn request_resize(&mut self) {
        let h = self.renderer.target_logical_height().round() as u32;
        let w = WIN_WIDTH as u32;
        let (cur_w, cur_h) = self.canvas.dimensions();
        if cur_w != w || cur_h != h {
            self.layer.set_size(w, h);
            self.pending_size = Some((w, h));
            self.canvas.set_dimensions(w, h);
        }
    }

    fn paint(&mut self) {
        if !self.configured {
            return;
        }
        if let Err(e) = self.canvas.paint_and_present(
            self.layer.wl_surface(),
            &self.renderer,
            self.scale as f32,
        ) {
            tracing::warn!("overlay(wlr): paint failed: {e}");
        }
    }
}

impl CompositorHandler for LayerState {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        if new_factor > 0 && new_factor != self.scale {
            self.scale = new_factor;
            self.layer.wl_surface().set_buffer_scale(new_factor);
            // Force a repaint at the new scale.
            self.paint();
        }
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for LayerState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for LayerState {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl LayerShellHandler for LayerState {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        // The compositor pulled our surface (output unplugged, session
        // ending, etc). Tear down cleanly.
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let (mut w, mut h) = configure.new_size;
        if w == 0 {
            w = WIN_WIDTH as u32;
        }
        if h == 0 {
            h = self.renderer.target_logical_height().round() as u32;
        }
        self.canvas.set_dimensions(w, h);
        self.configured = true;
        // Drive the first / next paint synchronously so the configure
        // ack is paired with a real buffer.
        self.paint();
    }
}

delegate_compositor!(LayerState);
delegate_output!(LayerState);
delegate_shm!(LayerState);
delegate_layer!(LayerState);
delegate_registry!(LayerState);

// WlRegion has no events; SCTK doesn't provide a delegate for it.
// We use it once to set an empty input region and then destroy it.
impl Dispatch<wl_region::WlRegion, ()> for LayerState {
    fn event(
        _: &mut Self,
        _: &wl_region::WlRegion,
        _: wl_region::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

// Same for wl_compositor when SCTK's CompositorState binding routes
// the WlCompositor user_data through GlobalData — region creation
// goes via wl_compositor.create_region, which expects this dispatch
// path to be present. SCTK's delegate_compositor covers this.

impl ProvidesRegistryState for LayerState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

// ---------------------------------------------------------------------------
//  Event loop
// ---------------------------------------------------------------------------

fn run_loop(
    conn: Connection,
    mut event_queue: wayland_client::EventQueue<LayerState>,
    mut state: LayerState,
    rx: Receiver<OverlayCmd>,
    waker_read: std::os::fd::OwnedFd,
) -> std::io::Result<()> {
    let wl_fd = wayland_shm::wayland_fd(&conn);
    let waker_borrow = waker_read.as_fd();

    // Pump the queue once so the initial layer-shell configure shows
    // up and `configured` flips before the orchestrator starts pushing
    // state changes.
    let _ = event_queue.roundtrip(&mut state);

    loop {
        // 1. Drain orchestrator commands.
        let drain = drain_commands(&rx, &mut state.renderer);
        if drain.exit {
            state.exit = true;
        }

        // 2. Apply size / visibility transitions.
        if drain.needs_resize {
            if state.renderer.is_visible() {
                state.request_resize();
            } else {
                // Hidden: unmap the surface so the compositor stops
                // drawing the rounded panel entirely.
                ShmCanvas::unmap(state.layer.wl_surface());
            }
        }
        if drain.needs_redraw && state.renderer.is_visible() {
            state.paint();
        }
        // Self-driven animation pump: the Glass Cortex thinking /
        // speaking phases and the text-only reply auto-scroll animate
        // with no incoming data to trigger repaints. Tick + paint when
        // the scene needs it.
        let animating = state.renderer.wants_animation_frame() && state.renderer.is_visible();
        if animating {
            state.renderer.animation_tick();
            state.paint();
        }

        if state.exit {
            break;
        }

        // 3. Poll for wayland events / new commands. Only spin at the
        // ~60 fps animation cadence while something is actually
        // animating; otherwise block until the compositor or the
        // orchestrator's waker rouses us, so a static overlay (idle,
        // recording, a settled reply panel) costs zero CPU instead of
        // waking 60×/s.
        let timeout = if animating { Duration::from_millis(16) } else { Duration::from_secs(3600) };
        poll_event_sources(wl_fd, waker_borrow, timeout);
        // Drain any waker bytes regardless of poll result; the
        // closure may have written several.
        let mut buf = [0u8; 64];
        while rustix::io::read(waker_borrow, &mut buf).map(|n| n > 0).unwrap_or(false) {}

        // 4. Dispatch wayland events.
        dispatch_wayland(&conn, &mut event_queue, &mut state)?;
    }

    // Best-effort cleanup. Dropping `LayerSurface` sends the destroy
    // request; the rest of the protocol state cleans up on drop too.
    let _ = event_queue.roundtrip(&mut state);
    Ok(())
}
