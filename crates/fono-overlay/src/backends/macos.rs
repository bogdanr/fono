// SPDX-License-Identifier: GPL-3.0-only
//! Native macOS overlay backend — a borderless, non-activating,
//! click-through, always-on-top `NSPanel` software-blitted from the
//! shared renderer in [`crate::renderer`].
//!
//! ## Threading model
//!
//! AppKit is main-thread-only, but on macOS the daemon's main thread
//! is already parked in the AppKit run-loop pump that `fono::main`
//! installs (see `fono-tray`'s `run_main_pump`). This backend
//! therefore splits in two:
//!
//! - A **worker thread** (`fono-overlay-mac`) owns the
//!   [`RendererState`] and the `OverlayCmd` channel — the exact same
//!   command handling as the winit/X11 backend — and renders each
//!   frame into a plain `Vec<u32>` ARGB framebuffer.
//! - Finished frames are handed to the **main thread** through the
//!   process-wide dispatcher installed by
//!   [`set_main_thread_dispatcher`], where a small blit job creates /
//!   resizes / shows / hides the `NSPanel` and uploads the pixels via
//!   `NSBitmapImageRep` → `NSImage` → `NSImageView`.
//!
//! Frames are coalesced newest-wins through a single-slot mailbox: the
//! worker only dispatches a new blit job when the slot was empty, so a
//! slow pump tick can never queue up an unbounded backlog — it just
//! skips straight to the latest frame.
//!
//! `fono-overlay` deliberately does **not** depend on `fono-tray`
//! (the dependency arrow between peer crates points nowhere); the
//! binary wires the pump's `dispatch_main` into this module at daemon
//! startup. Headless invocations never install a dispatcher, so
//! `try_spawn` fails with a clean `NotAvailable` and the selector
//! falls through to `noop` — dictation still works, just without the
//! visual indicator.

#![allow(clippy::too_many_lines)]

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex, OnceLock};

use fono_core::config::WaveformStyle;
use objc2::rc::Retained;
use objc2::{msg_send, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBitmapImageRep, NSColor, NSImage, NSImageView, NSPanel, NSScreen,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{ns_string, NSPoint, NSRect, NSSize};

use crate::backend::{BackendCapabilities, BackendError, BackendId, OverlayCmd, SpawnedBackend};
use crate::renderer::{
    self, RendererState, ACCENT_WIDTH, BOTTOM_OFFSET, PADDING_BOT, PADDING_TOP, PADDING_X,
    STATUS_FONT_PX, STATUS_TO_TEXT, WIN_MIN_HEIGHT, WIN_WAVEFORM_HEIGHT, WIN_WIDTH,
};
use crate::OverlayState;

// ---------------------------------------------------------------------------
//  Main-thread dispatcher seam
// ---------------------------------------------------------------------------

/// A closure that ships a job to the AppKit main thread and returns
/// whether it was accepted (`false` = no pump installed / pump gone).
pub type MainDispatcher = Box<dyn Fn(Box<dyn FnOnce() + Send>) -> bool + Send + Sync>;

static DISPATCHER: OnceLock<MainDispatcher> = OnceLock::new();

/// Install the process-wide main-thread dispatcher. Called once by
/// `fono::main` on macOS daemon startup, wiring in the tray pump's
/// `dispatch_main`. Later calls are ignored.
pub fn set_main_thread_dispatcher(d: MainDispatcher) {
    let _ = DISPATCHER.set(d);
}

fn dispatch(job: Box<dyn FnOnce() + Send>) -> bool {
    DISPATCHER.get().is_some_and(|d| d(job))
}

// ---------------------------------------------------------------------------
//  Worker → main-thread frame mailbox
// ---------------------------------------------------------------------------

/// One rendered frame, ready to blit. `visible == false` carries no
/// pixels — it just orders the panel out.
struct Frame {
    visible: bool,
    /// ARGB8888, premultiplied, `px_w * px_h` pixels.
    buf: Vec<u32>,
    px_w: u32,
    px_h: u32,
    /// Logical (point) size the panel frame should take.
    log_w: f64,
    log_h: f64,
}

struct Shared {
    /// Newest-wins single-slot mailbox.
    frame: Mutex<Option<Frame>>,
    /// Backing scale factor as f32 bits, written by the main thread
    /// (panel / screen truth), read by the worker for rendering.
    scale_bits: AtomicU32,
}

impl Shared {
    fn scale(&self) -> f32 {
        f32::from_bits(self.scale_bits.load(Ordering::Relaxed))
    }

    fn set_scale(&self, s: f32) {
        self.scale_bits.store(s.to_bits(), Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
//  Spawn
// ---------------------------------------------------------------------------

pub fn try_spawn(style: WaveformStyle) -> Result<SpawnedBackend, BackendError> {
    if DISPATCHER.get().is_none() {
        return Err(BackendError::NotAvailable(
            "AppKit main-thread pump not installed (headless session or non-daemon invocation)"
                .into(),
        ));
    }

    // Reasonable retina-first default until the probe job below
    // reports the real backing scale; the overlay starts Hidden, so
    // no visible frame renders before the answer lands.
    let shared =
        Arc::new(Shared { frame: Mutex::new(None), scale_bits: AtomicU32::new(2.0f32.to_bits()) });

    // Probe the real scale factor from the main screen. Also serves
    // as the liveness check: a dead pump refuses the job.
    let probe = Arc::clone(&shared);
    let accepted = dispatch(Box::new(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            if let Some(screen) = NSScreen::mainScreen(mtm) {
                probe.set_scale(screen.backingScaleFactor() as f32);
            }
        }
    }));
    if !accepted {
        return Err(BackendError::NotAvailable("AppKit main-thread pump has exited".into()));
    }

    let (tx, rx) = channel::<OverlayCmd>();
    let worker_shared = Arc::clone(&shared);
    let join = std::thread::Builder::new()
        .name("fono-overlay-mac".into())
        .spawn(move || run_worker(rx, worker_shared, style))
        .map_err(|e| BackendError::SpawnFailed(format!("spawn fono-overlay-mac thread: {e}")))?;

    Ok(SpawnedBackend {
        id: BackendId::MacPanel,
        capabilities: BackendCapabilities {
            transparency: true,
            client_positioning: true,
            focus_passthrough: true,
            click_passthrough: true,
        },
        tx,
        // The worker blocks on the command channel itself, so the
        // send alone is the wake-up; nothing else to rouse.
        waker: Box::new(|| {}),
        join,
    })
}

// ---------------------------------------------------------------------------
//  Worker thread — renderer + command loop
// ---------------------------------------------------------------------------

fn run_worker(rx: Receiver<OverlayCmd>, shared: Arc<Shared>, style: WaveformStyle) {
    let mut renderer = RendererState::new(style);
    let mut shown = false;

    'outer: loop {
        // Block for the first command, then drain the burst — the
        // same batch-then-render shape as the winit backend's
        // `about_to_wait`.
        let Ok(first) = rx.recv() else { break };
        let mut needs_redraw = false;
        let mut pending = Some(first);
        while let Some(cmd) = pending.take() {
            match cmd {
                OverlayCmd::SetState(s) => {
                    renderer.set_state(s);
                    if matches!(s, OverlayState::Hidden) {
                        shown = false;
                        push_frame(&shared, hidden_frame());
                    } else {
                        shown = true;
                        needs_redraw = true;
                    }
                }
                OverlayCmd::UpdateText(t) => {
                    if renderer.update_text(t) {
                        needs_redraw = true;
                    }
                }
                OverlayCmd::AudioLevel(v) => {
                    renderer.push_level(v);
                    if renderer.is_visible() {
                        needs_redraw = true;
                    }
                }
                OverlayCmd::AudioSamples(s) => {
                    renderer.push_samples(s);
                    if renderer.samples_push_needs_redraw() && renderer.is_visible() {
                        needs_redraw = true;
                    }
                }
                OverlayCmd::FftBins(bins) => {
                    renderer.push_fft_bins(bins);
                    update_heatmap_cache(&mut renderer, shared.scale());
                    if renderer.fft_push_needs_redraw() && renderer.is_visible() {
                        needs_redraw = true;
                    }
                }
                OverlayCmd::SetWaveformStyle(style) => {
                    let (changed, _crossed) = renderer.set_waveform_style(style);
                    if changed {
                        renderer.clear_for_style_swap();
                        if renderer.is_visible() {
                            needs_redraw = true;
                        }
                        tracing::debug!("overlay(mac): style -> {style:?}");
                    }
                }
                OverlayCmd::SetVolumeBar(mode) => {
                    if renderer.set_volume_bar(mode) {
                        needs_redraw = true;
                    }
                }
                OverlayCmd::GateMetrics { inst_rms, voiced_rms, silence_rms } => {
                    renderer.set_gate_metrics(renderer::GateMetrics {
                        inst_rms,
                        voiced_rms,
                        silence_rms,
                    });
                    if renderer.is_visible()
                        && matches!(renderer.volume_bar, fono_core::config::VolumeBarMode::Advanced)
                    {
                        needs_redraw = true;
                    }
                }
                OverlayCmd::Shutdown => break 'outer,
            }
            pending = rx.try_recv().ok();
        }

        if needs_redraw && shown && renderer.is_visible() {
            push_frame(&shared, render_frame(&renderer, shared.scale()));
        }
    }

    // Tear the panel down on the main thread. Best-effort: on a dead
    // pump the daemon is exiting anyway.
    let _ = dispatch(Box::new(|| {
        PANEL.with(|slot| {
            if let Some(ui) = slot.borrow_mut().take() {
                ui.panel.orderOut(None);
            }
        });
    }));
}

fn hidden_frame() -> Frame {
    Frame { visible: false, buf: Vec::new(), px_w: 0, px_h: 0, log_w: 0.0, log_h: 0.0 }
}

/// Render the current renderer state into an owned ARGB framebuffer
/// at the given backing scale.
fn render_frame(renderer: &RendererState, scale: f32) -> Frame {
    let log_h = renderer.target_logical_height().clamp(
        WIN_MIN_HEIGHT.min(WIN_WAVEFORM_HEIGHT),
        // target_logical_height already clamps to the renderer's own
        // max; this outer clamp only guards against NaN weirdness.
        4096.0,
    );
    let px_w = (WIN_WIDTH * scale).round().max(1.0) as u32;
    let px_h = (log_h * scale).round().max(1.0) as u32;
    let mut buf = vec![0u32; (px_w as usize) * (px_h as usize)];
    renderer.redraw(&mut buf, px_w, px_h, scale);
    Frame { visible: true, buf, px_w, px_h, log_w: f64::from(WIN_WIDTH), log_h: f64::from(log_h) }
}

/// Same heatmap content-rect math as the winit backend, using the
/// physical size the next frame will render at.
fn update_heatmap_cache(renderer: &mut RendererState, scale: f32) {
    let w = (WIN_WIDTH * scale).round();
    let h = (renderer.target_logical_height() * scale).round();
    let cx0 = ((PADDING_X + ACCENT_WIDTH) * scale).round() as i32;
    let cx1 = PADDING_X.mul_add(-scale, w).round() as i32;
    let pad_top = PADDING_TOP * scale;
    let cy0 = STATUS_TO_TEXT.mul_add(scale, STATUS_FONT_PX.mul_add(scale, pad_top)).round() as i32;
    let cy1 = PADDING_BOT.mul_add(-scale, h).round() as i32;
    renderer.update_heatmap_cache(cx0, cx1, cy0, cy1);
}

/// Put a frame in the mailbox; dispatch a blit job only when the slot
/// was empty (newest-wins coalescing keeps the pump backlog at ≤ 1).
fn push_frame(shared: &Arc<Shared>, frame: Frame) {
    let was_empty = {
        let Ok(mut g) = shared.frame.lock() else { return };
        let was_empty = g.is_none();
        *g = Some(frame);
        was_empty
    };
    if was_empty {
        let blit = Arc::clone(shared);
        let _ = dispatch(Box::new(move || blit_on_main(&blit)));
    }
}

// ---------------------------------------------------------------------------
//  Main-thread side — the actual NSPanel
// ---------------------------------------------------------------------------

struct PanelUi {
    panel: Retained<NSPanel>,
    image_view: Retained<NSImageView>,
}

thread_local! {
    /// Main-thread-only panel state. `Retained<NSPanel>` is `!Send`,
    /// which is exactly why this lives in a TLS slot the dispatched
    /// jobs (all main-thread) share.
    static PANEL: RefCell<Option<PanelUi>> = const { RefCell::new(None) };
}

fn blit_on_main(shared: &Arc<Shared>) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(frame) = shared.frame.lock().ok().and_then(|mut g| g.take()) else { return };

    if !frame.visible {
        PANEL.with(|slot| {
            if let Some(ui) = slot.borrow().as_ref() {
                ui.panel.orderOut(None);
            }
        });
        return;
    }

    PANEL.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(create_panel(mtm));
        }
        let Some(ui) = slot.as_ref() else { return };

        // Keep the worker's notion of scale in sync with reality
        // (panel truth once it exists; display changes are picked up
        // on the next frame).
        let scale = ui.panel.backingScaleFactor();
        shared.set_scale(scale as f32);

        // Frame rect: bottom-centered on the panel's (or main)
        // screen. Cocoa's origin is bottom-left, so the Linux
        // BOTTOM_OFFSET maps directly onto the y coordinate.
        let screen = ui.panel.screen().or_else(|| NSScreen::mainScreen(mtm));
        let target = screen.map_or_else(
            || NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(frame.log_w, frame.log_h)),
            |s| {
                let sf = s.frame();
                let x = sf.origin.x + (sf.size.width - frame.log_w) / 2.0;
                let y = sf.origin.y + f64::from(BOTTOM_OFFSET);
                NSRect::new(NSPoint::new(x, y), NSSize::new(frame.log_w, frame.log_h))
            },
        );
        ui.panel.setFrame_display(target, true);

        if let Some(image) = image_from_argb(&frame) {
            ui.image_view.setImage(Some(&image));
            let bounds = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(frame.log_w, frame.log_h));
            ui.image_view.setFrame(bounds);
        }

        ui.panel.orderFrontRegardless();
    });
}

/// `NSStatusWindowLevel` (25). High enough to float above normal and
/// floating windows without fighting screen savers / system alerts.
const OVERLAY_WINDOW_LEVEL: isize = 25;

fn create_panel(mtm: MainThreadMarker) -> PanelUi {
    let rect = NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(f64::from(WIN_WIDTH), f64::from(WIN_MIN_HEIGHT)),
    );
    let mask = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
    // SAFETY: standard designated initializer on a freshly allocated
    // NSPanel; `defer: false` creates the window device immediately.
    let panel: Retained<NSPanel> = unsafe {
        msg_send![
            NSPanel::alloc(mtm),
            initWithContentRect: rect,
            styleMask: mask,
            backing: NSBackingStoreType::Buffered,
            defer: false,
        ]
    };

    // We hold the panel via `Retained`; Cocoa must not also
    // autorelease it on close.
    unsafe { panel.setReleasedWhenClosed(false) };

    // Non-activating, click-through, transparent, always-on-top,
    // present on every Space, invisible to the window cycler.
    panel.setLevel(OVERLAY_WINDOW_LEVEL);
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(false);
    panel.setIgnoresMouseEvents(true);
    panel.setHidesOnDeactivate(false);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );

    let image_view = NSImageView::new(mtm);
    panel.setContentView(Some(&image_view));

    PanelUi { panel, image_view }
}

/// Wrap the worker's ARGB framebuffer into an `NSImage` sized in
/// logical points (so retina blits map 1 buffer pixel : 1 device
/// pixel). The renderer produces premultiplied alpha, which is
/// `NSBitmapImageRep`'s default interpretation.
fn image_from_argb(frame: &Frame) -> Option<Retained<NSImage>> {
    let (w, h) = (frame.px_w as usize, frame.px_h as usize);
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            w as isize,
            h as isize,
            8,
            4,
            true,
            false,
            ns_string!("NSDeviceRGBColorSpace"),
            (w * 4) as isize,
            32,
        )
    }?;
    // SAFETY: the rep allocated its own meshed RGBA buffer of
    // exactly bytesPerRow * h bytes; we fill it completely.
    unsafe {
        let data = rep.bitmapData();
        if data.is_null() {
            return None;
        }
        let out = std::slice::from_raw_parts_mut(data, w * h * 4);
        for (px, chunk) in frame.buf.iter().zip(out.chunks_exact_mut(4)) {
            chunk[0] = (px >> 16) as u8; // R
            chunk[1] = (px >> 8) as u8; // G
            chunk[2] = *px as u8; // B
            chunk[3] = (px >> 24) as u8; // A
        }
    }
    let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(frame.log_w, frame.log_h));
    image.addRepresentation(&rep);
    Some(image)
}
