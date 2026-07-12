// SPDX-License-Identifier: GPL-3.0-only
//! Native Windows overlay backend — a borderless, non-activating,
//! click-through, always-on-top layered tool-window updated with
//! `UpdateLayeredWindow` from the shared renderer in
//! [`crate::renderer`].
//!
//! ## Why `UpdateLayeredWindow` and not softbuffer
//!
//! The Windows port plan (Task 10.1) sketched a winit + softbuffer
//! window. softbuffer presents on Windows via a GDI `BitBlt`, which
//! discards the alpha channel — so the renderer's premultiplied ARGB
//! output (translucent "glass" panel, anti-aliased rounded corners)
//! would blit as an opaque rectangle. The correct Win32 technique for
//! a per-pixel-alpha, click-through floating surface is a **layered
//! window** fed by `UpdateLayeredWindow` with an `AC_SRC_ALPHA`
//! blend. That consumes the renderer's premultiplied buffer verbatim
//! (0xAARRGGBB little-endian == the BGRA byte order a top-down 32-bpp
//! DIB wants), so no channel swap or extra compositing pass is needed.
//!
//! Because the surface is driven entirely by `UpdateLayeredWindow`
//! (not by `WM_PAINT`), the backend needs neither winit nor softbuffer
//! — only `windows-sys`, already in the dependency graph via
//! `fono-tray` / `fono-inject` / cpal. It therefore mirrors the macOS
//! backend's shape (a worker thread owning the [`RendererState`] and
//! the `OverlayCmd` channel, blocking on the channel with a ~30 fps
//! animation timeout) rather than the winit/X11 event-loop shape.
//!
//! ## Threading model
//!
//! A single worker thread (`fono-overlay-win32`) both owns the window
//! and renders. The window's message queue belongs to its creating
//! thread, so keeping creation, rendering, presenting, and teardown on
//! one thread avoids any cross-thread HWND hazards. The window is
//! click-through (`WS_EX_TRANSPARENT`) and never activates
//! (`WS_EX_NOACTIVATE`), so it receives essentially no input messages;
//! the queue is drained opportunistically each loop turn. The worker
//! blocks on the command channel itself, so the public `Waker` is a
//! no-op (the send alone rouses it) — same as the macOS/noop backends.

#![allow(clippy::too_many_lines)]

use std::ffi::c_void;
use std::sync::mpsc::{channel, Receiver};

use fono_core::config::WaveformStyle;
use windows_sys::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, AC_SRC_ALPHA,
    AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC,
    HGDIOBJ,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PeekMessageW, RegisterClassW,
    ShowWindow, SystemParametersInfoW, TranslateMessage, UpdateLayeredWindow, MSG, PM_REMOVE,
    SPI_GETWORKAREA, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::backend::{BackendCapabilities, BackendError, BackendId, OverlayCmd, SpawnedBackend};
use crate::renderer::{
    self, RendererState, ACCENT_WIDTH, BOTTOM_OFFSET, PADDING_BOT, PADDING_TOP, PADDING_X,
    STATUS_FONT_PX, STATUS_TO_TEXT, WIN_MIN_HEIGHT, WIN_WAVEFORM_HEIGHT, WIN_WIDTH,
};
use crate::OverlayState;

// ---------------------------------------------------------------------------
//  Spawn
// ---------------------------------------------------------------------------

pub fn try_spawn(style: WaveformStyle) -> Result<SpawnedBackend, BackendError> {
    let (tx, rx) = channel::<OverlayCmd>();
    // Confirm the window can actually be created before we commit to
    // this backend, so a failure falls through to `noop` cleanly.
    let (ready_tx, ready_rx) = channel::<Result<(), String>>();
    let join = std::thread::Builder::new()
        .name("fono-overlay-win32".into())
        .spawn(move || run_worker(rx, ready_tx, style))
        .map_err(|e| BackendError::SpawnFailed(format!("spawn fono-overlay-win32 thread: {e}")))?;

    match ready_rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(Ok(())) => {}
        Ok(Err(msg)) => {
            let _ = join.join();
            return Err(BackendError::NotAvailable(msg));
        }
        Err(e) => {
            return Err(BackendError::SpawnFailed(format!(
                "Win32 overlay window did not become ready within 2s: {e}"
            )));
        }
    }

    Ok(SpawnedBackend {
        id: BackendId::Win32LayeredToolWindow,
        capabilities: BackendCapabilities {
            transparency: true,
            client_positioning: true,
            focus_passthrough: true,
            click_passthrough: true,
        },
        tx,
        // The worker blocks on the command channel, so the send itself
        // is the wake-up; nothing else to rouse.
        waker: Box::new(|| {}),
        join,
    })
}

// ---------------------------------------------------------------------------
//  Win32 window class
// ---------------------------------------------------------------------------

/// UTF-16, NUL-terminated. For class / window names passed to Win32.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> isize {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Register the overlay window class once per process. `RegisterClassW`
/// is idempotent-by-guard here: a second registration would fail, but
/// only one backend is ever spawned per run.
fn register_class(class_name: &[u16]) {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: `wc` is fully initialised; `lpfnWndProc` points at a
        // valid `extern "system"` fn; the class-name pointer outlives
        // the call (owned by the caller for the whole spawn).
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: unsafe { GetModuleHandleW(std::ptr::null()) },
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        unsafe { RegisterClassW(&wc) };
    });
}

// ---------------------------------------------------------------------------
//  Layered surface (window + reusable top-down DIB)
// ---------------------------------------------------------------------------

/// Owns the layered window and a memory-DC-backed 32-bpp top-down DIB
/// sized to the current frame. Recreates the DIB only when the frame
/// dimensions change.
struct Surface {
    hwnd: HWND,
    memdc: HDC,
    dib: HBITMAP,
    old_obj: HGDIOBJ,
    bits: *mut u32,
    w: i32,
    h: i32,
}

impl Surface {
    fn new() -> Result<Self, String> {
        let class_name = wide("FonoOverlayWindowClass");
        register_class(&class_name);
        let window_name = wide("Fono");
        let ex_style =
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST;
        // SAFETY: all pointers are valid for the call; the class was
        // registered above; a null parent/menu/param is valid for a
        // top-level popup.
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                class_name.as_ptr(),
                window_name.as_ptr(),
                WS_POPUP,
                0,
                0,
                WIN_WIDTH as i32,
                WIN_MIN_HEIGHT as i32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            return Err("CreateWindowExW returned null (no interactive desktop?)".into());
        }
        // SAFETY: null HDC asks GDI for a screen-compatible memory DC.
        let memdc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
        if memdc.is_null() {
            // SAFETY: hwnd is a live window we just created.
            unsafe { DestroyWindow(hwnd) };
            return Err("CreateCompatibleDC failed".into());
        }
        Ok(Self {
            hwnd,
            memdc,
            dib: std::ptr::null_mut(),
            old_obj: std::ptr::null_mut(),
            bits: std::ptr::null_mut(),
            w: 0,
            h: 0,
        })
    }

    /// Ensure the DIB matches `(w, h)` physical pixels, recreating it
    /// on a size change. Returns a mutable pixel slice on success.
    fn ensure_dib(&mut self, w: i32, h: i32) -> Option<&mut [u32]> {
        if w <= 0 || h <= 0 {
            return None;
        }
        if self.dib.is_null() || self.w != w || self.h != h {
            self.release_dib();
            let header = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // negative => top-down (row 0 at top)
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB as u32,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            };
            let bmi = BITMAPINFO { bmiHeader: header, bmiColors: [unsafe { std::mem::zeroed() }] };
            let mut bits: *mut c_void = std::ptr::null_mut();
            // SAFETY: `bmi` is a valid top-down 32-bpp descriptor; `bits`
            // receives the DIB pixel pointer owned by the returned bitmap.
            let dib = unsafe {
                CreateDIBSection(
                    self.memdc,
                    &bmi,
                    DIB_RGB_COLORS,
                    &mut bits,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if dib.is_null() || bits.is_null() {
                return None;
            }
            // SAFETY: selecting the DIB into the memory DC; keep the old
            // object to restore before deletion.
            self.old_obj = unsafe { SelectObject(self.memdc, dib) };
            self.dib = dib;
            self.bits = bits.cast::<u32>();
            self.w = w;
            self.h = h;
        }
        // SAFETY: `bits` points at exactly `w * h` u32 pixels for the
        // lifetime of the selected DIB.
        Some(unsafe { std::slice::from_raw_parts_mut(self.bits, (w as usize) * (h as usize)) })
    }

    fn release_dib(&mut self) {
        if !self.dib.is_null() {
            // SAFETY: restore the DC's original object, then free the DIB.
            unsafe {
                if !self.old_obj.is_null() {
                    SelectObject(self.memdc, self.old_obj);
                }
                DeleteObject(self.dib);
            }
            self.dib = std::ptr::null_mut();
            self.old_obj = std::ptr::null_mut();
            self.bits = std::ptr::null_mut();
        }
    }

    /// Present the current DIB at screen `(x, y)` with per-pixel alpha.
    fn present(&self, x: i32, y: i32) {
        let dst = POINT { x, y };
        let src = POINT { x: 0, y: 0 };
        let size = SIZE { cx: self.w, cy: self.h };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        // SAFETY: hwnd is layered; memdc holds a selected top-down DIB
        // of `size` pixels; all pointers are to stack locals live for
        // the call. crKey is unused (ULW_ALPHA path).
        unsafe {
            UpdateLayeredWindow(
                self.hwnd,
                std::ptr::null_mut(),
                &dst,
                &size,
                self.memdc,
                &src,
                0 as COLORREF,
                &blend,
                ULW_ALPHA,
            );
        }
    }

    fn scale(&self) -> f32 {
        // SAFETY: hwnd is a live window.
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 {
            1.0
        } else {
            dpi as f32 / 96.0
        }
    }

    fn show(&self, visible: bool) {
        // SAFETY: hwnd is a live window; SW_SHOWNOACTIVATE keeps focus
        // where it is (the overlay must never steal activation).
        unsafe { ShowWindow(self.hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE }) };
    }

    /// Drain any pending window messages (there are essentially none —
    /// the surface takes no input — but a click-through layered window
    /// still benefits from servicing the odd system message).
    fn pump(&self) {
        // SAFETY: `msg` is written by PeekMessageW before we read it.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, self.hwnd, 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        self.release_dib();
        // SAFETY: memdc / hwnd are live handles owned solely by this
        // worker thread; freed exactly once here.
        unsafe {
            if !self.memdc.is_null() {
                DeleteDC(self.memdc);
            }
            if !self.hwnd.is_null() {
                DestroyWindow(self.hwnd);
            }
        }
    }
}

/// Primary-monitor work area (excludes the taskbar). Falls back to a
/// zero rect if the query fails; the caller clamps.
fn work_area() -> RECT {
    let mut wa = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    // SAFETY: `wa` is a valid RECT the call fills in.
    unsafe {
        SystemParametersInfoW(SPI_GETWORKAREA, 0, (&mut wa as *mut RECT).cast::<c_void>(), 0);
    }
    wa
}

// ---------------------------------------------------------------------------
//  Worker thread — renderer + command loop
// ---------------------------------------------------------------------------

fn run_worker(
    rx: Receiver<OverlayCmd>,
    ready_tx: std::sync::mpsc::Sender<Result<(), String>>,
    style: WaveformStyle,
) {
    let mut surface = match Surface::new() {
        Ok(s) => {
            let _ = ready_tx.send(Ok(()));
            s
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    let mut renderer = RendererState::new(style);
    let mut shown = false;

    'outer: loop {
        // Block for the first command, then drain the burst — the same
        // batch-then-render shape as the other backends. While the
        // Glass Cortex thinking / speaking phases animate with no
        // incoming data, wait with a ~30 fps timeout and render an
        // animation frame when it elapses; otherwise block indefinitely
        // (a static overlay costs zero CPU).
        let mut needs_redraw = false;
        let mut animate = false;
        let mut pending = if renderer.wants_animation_frame() {
            match rx.recv_timeout(std::time::Duration::from_millis(33)) {
                Ok(c) => Some(c),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    animate = true;
                    None
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => break,
            }
        };

        while let Some(cmd) = pending.take() {
            match cmd {
                OverlayCmd::SetState(s) => {
                    renderer.set_state(s);
                    if matches!(s, OverlayState::Hidden) {
                        shown = false;
                        surface.show(false);
                    } else {
                        shown = true;
                        surface.show(true);
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
                    update_heatmap_cache(&mut renderer, surface.scale());
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
                        tracing::debug!("overlay(win32): style -> {style:?}");
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
                OverlayCmd::Cortex(cmd) => {
                    if renderer.push_cortex_cmd(cmd) && renderer.is_visible() {
                        needs_redraw = true;
                    }
                }
                OverlayCmd::Shutdown => break 'outer,
            }
            pending = rx.try_recv().ok();
        }

        if animate {
            renderer.animation_tick();
            needs_redraw = true;
        }
        if needs_redraw && shown && renderer.is_visible() {
            render_and_present(&mut surface, &renderer);
        }
        surface.pump();
    }
    // `surface` drops here: DIB freed, DC deleted, window destroyed.
}

/// Render the current renderer state into the surface's DIB and push it
/// to the layered window, anchored bottom-centre on the primary
/// monitor's work area (mirrors the Linux/macOS placement).
fn render_and_present(surface: &mut Surface, renderer: &RendererState) {
    let scale = surface.scale();
    let log_h =
        renderer.target_logical_height().clamp(WIN_MIN_HEIGHT.min(WIN_WAVEFORM_HEIGHT), 4096.0);
    let px_w = (WIN_WIDTH * scale).round().max(1.0) as i32;
    let px_h = (log_h * scale).round().max(1.0) as i32;

    let Some(buf) = surface.ensure_dib(px_w, px_h) else { return };
    // Clear to fully-transparent premultiplied (0), then paint. Pixels
    // outside the rounded panel stay transparent for the layered blit.
    buf.fill(0);
    renderer.redraw(buf, px_w as u32, px_h as u32, scale);

    let wa = work_area();
    let (wa_w, wa_h) = (wa.right - wa.left, wa.bottom - wa.top);
    // Fall back to a plain 1080p-ish guess if the work-area query
    // returned zeros (rare; e.g. very early session init).
    let (wa_left, wa_bottom, wa_w) =
        if wa_w <= 0 || wa_h <= 0 { (0, 1040, 1920) } else { (wa.left, wa.bottom, wa_w) };
    let x = wa_left + (wa_w - px_w) / 2;
    let y = wa_bottom - px_h - (BOTTOM_OFFSET as f32 * scale).round() as i32;
    surface.present(x, y);
}

/// Same heatmap content-rect math as the other backends, using the
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
