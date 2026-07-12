// SPDX-License-Identifier: GPL-3.0-only
//! Shared shm + event-loop helpers for the Wayland backend.
//!
//! Currently used only by [`super::wayland_layer_shell`]. The
//! framebuffer painting and cross-thread wake-up plumbing live here
//! so it stays consistent if a future Wayland-native backend lands
//! alongside layer-shell.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use rustix::event::{PollFd, PollFlags};
use rustix::pipe::{pipe_with, PipeFlags};
use smithay_client_toolkit::shm::slot::SlotPool;
use wayland_client::protocol::{wl_shm, wl_surface};
use wayland_client::{Connection, EventQueue};

use crate::backend::OverlayCmd;
use crate::renderer::RendererState;

/// `wl_buffer` stride for ARGB8888.
#[inline]
pub fn stride_for(width: u32) -> i32 {
    width.saturating_mul(4) as i32
}

/// A self-pipe used as a cross-thread waker. Writing any byte to the
/// write end makes the read end readable; the wayland event-loop
/// thread polls both the wayland fd and this read end so it wakes up
/// promptly when the orchestrator pushes a command into the channel.
pub struct Waker {
    pub read: OwnedFd,
    pub write: OwnedFd,
}

impl Waker {
    pub fn new() -> io::Result<Self> {
        let (read, write) = pipe_with(PipeFlags::NONBLOCK | PipeFlags::CLOEXEC)
            .map_err(|e| io::Error::other(format!("pipe2: {e}")))?;
        Ok(Self { read, write })
    }
}

/// Construct a waker closure suitable for the public
/// [`crate::backend::SpawnedBackend::waker`] field. Writing a single
/// byte into the self-pipe is the cheapest cross-thread wake-up
/// available on Linux.
pub fn make_waker_closure(write_fd: OwnedFd) -> Box<dyn Fn() + Send + Sync> {
    // We keep the OwnedFd alive inside the closure — when the handle
    // is dropped, the pipe write end closes, and the event loop's
    // poll() sees EOF on the read end (which we treat as "wake").
    let fd = std::sync::Mutex::new(Some(write_fd));
    Box::new(move || {
        if let Ok(g) = fd.lock() {
            if let Some(fd) = g.as_ref() {
                let _ = rustix::io::write(fd, &[0u8]);
            }
        }
    })
}

/// Manages the SCTK [`SlotPool`] and the most recent ARGB8888 buffer
/// dimensions. Both Wayland backends create one of these at startup
/// and reuse it across resizes.
pub struct ShmCanvas {
    pool: SlotPool,
    width: u32,
    height: u32,
    /// Tracks whether anything has been attached to the surface yet.
    /// Layer-shell needs the first commit to be buffer-free, then
    /// the configure handler drives the first paint.
    pub first_paint_done: bool,
}

impl ShmCanvas {
    pub fn new(
        shm: &smithay_client_toolkit::shm::Shm,
        width: u32,
        height: u32,
    ) -> io::Result<Self> {
        let bytes = (width as usize) * (height as usize) * 4 * 2; // double-buffered
        let pool = SlotPool::new(bytes.max(64 * 64 * 4 * 2), shm)
            .map_err(|e| io::Error::other(format!("SlotPool::new: {e}")))?;
        Ok(Self { pool, width, height, first_paint_done: false })
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn set_dimensions(&mut self, w: u32, h: u32) {
        self.width = w.max(1);
        self.height = h.max(1);
    }

    /// Acquire a fresh ARGB8888 buffer, paint into it via
    /// `RendererState::redraw`, attach + damage + commit. The
    /// SlotPool is double-buffered: if the previous buffer is still
    /// busy on the compositor side, SCTK allocates a second slot,
    /// so we never write under the compositor's feet.
    pub fn paint_and_present(
        &mut self,
        surface: &wl_surface::WlSurface,
        renderer: &RendererState,
        scale: f32,
    ) -> io::Result<()> {
        let w = self.width;
        let h = self.height;
        let stride = stride_for(w);
        let (buffer, canvas) = self
            .pool
            .create_buffer(w as i32, h as i32, stride, wl_shm::Format::Argb8888)
            .map_err(|e| io::Error::other(format!("SlotPool::create_buffer: {e}")))?;

        // Clear to fully transparent; renderer paints the rounded
        // panel on top. ARGB8888 is little-endian on Wayland so a
        // u32 of 0 is correctly transparent.
        canvas.fill(0);
        if renderer.is_visible() {
            // Re-interpret the &mut [u8] canvas as &mut [u32]. Safe
            // because SCTK guarantees 4-byte alignment of the slot;
            // the renderer wants ARGB u32s and `chunks_exact_mut(4)`
            // would force a slow byte path.
            #[allow(clippy::cast_ptr_alignment)]
            let buf_u32: &mut [u32] = unsafe {
                std::slice::from_raw_parts_mut(canvas.as_mut_ptr().cast::<u32>(), canvas.len() / 4)
            };
            renderer.redraw(buf_u32, w, h, scale);
        }

        surface.damage_buffer(0, 0, w as i32, h as i32);
        buffer
            .attach_to(surface)
            .map_err(|e| io::Error::other(format!("buffer.attach_to: {e}")))?;
        surface.commit();
        self.first_paint_done = true;
        Ok(())
    }

    /// Unmap the surface by attaching a null buffer + committing.
    /// Used by both backends to hide the overlay without tearing
    /// down the surface.
    pub fn unmap(surface: &wl_surface::WlSurface) {
        surface.attach(None, 0, 0);
        surface.commit();
    }
}

/// Outcome of one `drain_commands` pass.
pub struct DrainOutcome {
    pub needs_redraw: bool,
    pub needs_resize: bool,
    pub exit: bool,
}

/// Apply every queued [`OverlayCmd`] to the supplied [`RendererState`],
/// returning whether the next loop iteration should redraw / resize /
/// exit. Returned by both Wayland backends from inside their
/// per-iteration drain step so they can decide whether to re-request
/// a configure or call `paint_and_present`.
pub fn drain_commands(rx: &Receiver<OverlayCmd>, renderer: &mut RendererState) -> DrainOutcome {
    let mut needs_redraw = false;
    let mut needs_resize = false;
    let mut exit = false;
    loop {
        match rx.try_recv() {
            Ok(OverlayCmd::SetState(s)) => {
                renderer.set_state(s);
                needs_redraw = true;
                // Both branches want a resize: hidden shrinks the
                // surface to the minimum, visible sizes to the
                // current style's target height.
                let _ = s; // suppress unused-binding clippy on later branches
                needs_resize = true;
            }
            Ok(OverlayCmd::UpdateText(t)) => {
                if renderer.update_text(t) {
                    needs_redraw = true;
                    needs_resize = true;
                }
            }
            Ok(OverlayCmd::AudioLevel(v)) => {
                renderer.push_level(v);
                if renderer.is_visible() {
                    needs_redraw = true;
                }
            }
            Ok(OverlayCmd::AudioSamples(s)) => {
                renderer.push_samples(s);
                if renderer.samples_push_needs_redraw() && renderer.is_visible() {
                    needs_redraw = true;
                }
            }
            Ok(OverlayCmd::FftBins(b)) => {
                renderer.push_fft_bins(b);
                if renderer.fft_push_needs_redraw() && renderer.is_visible() {
                    needs_redraw = true;
                }
            }
            Ok(OverlayCmd::SetWaveformStyle(style)) => {
                let (changed, crossed) = renderer.set_waveform_style(style);
                if changed {
                    renderer.clear_for_style_swap();
                    if renderer.is_visible() {
                        needs_redraw = true;
                        if crossed {
                            needs_resize = true;
                        }
                    }
                }
            }
            Ok(OverlayCmd::SetVolumeBar(mode)) => {
                if renderer.set_volume_bar(mode) {
                    needs_redraw = true;
                    needs_resize = true;
                }
            }
            Ok(OverlayCmd::GateMetrics { inst_rms, voiced_rms, silence_rms }) => {
                renderer.set_gate_metrics(crate::renderer::GateMetrics {
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
            Ok(OverlayCmd::Cortex(cmd)) => {
                if renderer.push_cortex_cmd(cmd) && renderer.is_visible() {
                    needs_redraw = true;
                }
            }
            Ok(OverlayCmd::Shutdown) => {
                exit = true;
                break;
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                exit = true;
                break;
            }
        }
    }
    DrainOutcome { needs_redraw, needs_resize, exit }
}

/// Block until either the Wayland socket has new events, the waker
/// pipe is readable, or `timeout` elapses.
pub fn poll_event_sources(wl_fd: BorrowedFd<'_>, waker_fd: BorrowedFd<'_>, timeout: Duration) {
    let mut fds = [PollFd::new(&wl_fd, PollFlags::IN), PollFd::new(&waker_fd, PollFlags::IN)];
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    let _ = rustix::event::poll(&mut fds, ms);
}

/// Drain pending wayland events from the socket into the queue, then
/// dispatch them. Robust to spurious wake-ups (the prepare/read
/// guard simply returns nothing if no one else is reading).
pub fn dispatch_wayland<T: 'static>(
    conn: &Connection,
    event_queue: &mut EventQueue<T>,
    state: &mut T,
) -> io::Result<()> {
    // Flush outgoing requests first so configure acks etc. are sent.
    event_queue.flush().map_err(|e| io::Error::other(format!("flush: {e}")))?;
    // Read whatever the kernel has queued for us, non-blocking. The
    // `prepare_read` guard is the wayland-client API contract for
    // "I'm about to read from the socket"; calling `.read()` on it
    // doesn't block when O_NONBLOCK is set, which it is by default
    // on the wayland-client backend.
    if let Some(guard) = conn.prepare_read() {
        let _ = guard.read();
    }
    event_queue
        .dispatch_pending(state)
        .map_err(|e| io::Error::other(format!("dispatch_pending: {e}")))?;
    Ok(())
}

/// Borrow helper used by both backends to grab the wayland socket
/// fd. Lives here so the `as_fd` import stays out of the protocol
/// state modules.
pub fn wayland_fd(conn: &Connection) -> BorrowedFd<'_> {
    conn.as_fd()
}
