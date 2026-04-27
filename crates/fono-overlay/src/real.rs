// SPDX-License-Identifier: GPL-3.0-only
//! `winit` + `softbuffer` overlay window driver. Plan R5.
//!
//! ## Slice A scope (post user-feedback revision 2026-04-27)
//!
//! The overlay runs in the **daemon process** on a dedicated background
//! thread (subprocess refactor lands in Slice B per ADR 0009). The
//! window is:
//!
//! * Borderless, always-on-top, transparent — relies on the
//!   compositor for translucency. On X11 a compositor (picom, KWin,
//!   Mutter) must be running for the alpha channel to take effect; on
//!   Wayland a compositor is mandatory by definition. Without a
//!   compositor the BG renders as a solid charcoal rectangle, still
//!   readable just not glassy.
//! * Bottom-center on the primary monitor, ~48 px above the bottom
//!   edge. Compositor may override on Wayland.
//! * Dynamically resized vertically to fit the wrapped text content,
//!   clamped between 80 and 240 px so it never dominates the screen.
//! * Drawn entirely in software via `softbuffer` ARGB pixels — a dark
//!   rounded panel with a coloured accent stripe down the left edge
//!   indicating state, a small status label, and the live transcript
//!   word-wrapped to fit the panel width.
//! * Wakes immediately on incoming commands via an `EventLoopProxy`
//!   user-event so `set_state` / `update_text` calls from the
//!   orchestrator are reflected on screen with sub-frame latency.

#![allow(clippy::suboptimal_flops, clippy::branches_sharing_code)]

use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::OverlayState;

/// Commands sent from the main thread to the overlay's winit thread.
#[derive(Debug)]
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
    proxy: Mutex<Option<winit::event_loop::EventLoopProxy<()>>>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl OverlayHandle {
    fn send(&self, cmd: OverlayCmd) {
        let _ = self.inner.tx.send(cmd);
        if let Ok(g) = self.inner.proxy.lock() {
            if let Some(p) = g.as_ref() {
                let _ = p.send_event(());
            }
        }
    }

    pub fn set_state(&self, state: OverlayState) {
        self.send(OverlayCmd::SetState(state));
    }

    pub fn update_text(&self, text: impl Into<String>) {
        self.send(OverlayCmd::UpdateText(text.into()));
    }

    /// Stop the overlay and join its thread. Idempotent.
    pub fn shutdown(&self) {
        self.send(OverlayCmd::Shutdown);
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
    pub fn spawn() -> std::io::Result<OverlayHandle> {
        let (tx, rx) = channel::<OverlayCmd>();
        let (proxy_tx, proxy_rx) = std::sync::mpsc::channel();
        let join = std::thread::Builder::new()
            .name("fono-overlay".into())
            .spawn(move || {
                if let Err(e) = run_event_loop(rx, proxy_tx) {
                    tracing::warn!("overlay: event loop ended with error: {e:#}");
                }
            })?;
        let proxy = proxy_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .ok();
        Ok(OverlayHandle {
            inner: Arc::new(HandleInner {
                tx,
                proxy: Mutex::new(proxy),
                join: Mutex::new(Some(join)),
            }),
        })
    }
}

// ---------------------------------------------------------------------------
//  Layout constants — tuned by eye against a 1080p display.
// ---------------------------------------------------------------------------

/// Logical pixels.
const WIN_WIDTH: f32 = 640.0;
/// Minimum and maximum logical heights. Min fits status row + one line
/// of text; max prevents the overlay from dominating the screen.
const WIN_MIN_HEIGHT: f32 = 80.0;
const WIN_MAX_HEIGHT: f32 = 240.0;
/// Inset from the bottom edge.
const BOTTOM_OFFSET: u32 = 48;

const PADDING_X: f32 = 24.0;
const PADDING_TOP: f32 = 14.0;
const PADDING_BOT: f32 = 16.0;
const ACCENT_WIDTH: f32 = 4.0;
const CORNER_RADIUS: f32 = 12.0;
const STATUS_FONT_PX: f32 = 13.0;
const TEXT_FONT_PX: f32 = 20.0;
const STATUS_TO_TEXT: f32 = 14.0;
const LINE_GAP: f32 = 6.0;

/// `0xAARRGGBB`. The compositor honours the alpha byte when
/// `with_transparent(true)` is set on the window. ~93 % opaque dark
/// charcoal — just translucent enough to feel modern, opaque enough
/// for legibility.
const COLOR_BG: u32 = 0xEE17_171B;
const COLOR_TEXT: u32 = 0xFFEC_ECF1;
const COLOR_TEXT_DIM: u32 = 0xCCAA_AAB2;

/// Accent stripe colour per state.
fn accent_color(state: OverlayState) -> u32 {
    match state {
        OverlayState::Hidden => 0x0000_0000,
        // Soft red (recording).
        OverlayState::Recording { .. } => 0xFFE0_5454,
        // Warm amber (processing / polishing).
        OverlayState::Processing => 0xFFE0_A040,
        // Vibrant indigo (live dictation).
        OverlayState::LiveDictating => 0xFF63_7AFF,
    }
}

fn state_label(state: OverlayState) -> &'static str {
    match state {
        OverlayState::Hidden => "",
        OverlayState::Recording { .. } => "RECORDING",
        OverlayState::Processing => "POLISHING",
        OverlayState::LiveDictating => "LIVE",
    }
}

// ---------------------------------------------------------------------------
//  Font discovery
// ---------------------------------------------------------------------------

fn load_system_font() -> Option<ab_glyph::FontArc> {
    const CANDIDATES: &[&str] = &[
        "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/NotoSans-Bold.ttf",
        "/usr/share/fonts/TTF/NotoSans-Regular.ttf",
        "/usr/share/fonts/noto/NotoSans-Bold.ttf",
        "/usr/share/fonts/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/TTF/LiberationSans-Bold.ttf",
        "/usr/share/fonts/TTF/LiberationSans-Regular.ttf",
        "/usr/share/fonts/liberation-sans/LiberationSans-Bold.ttf",
        "/usr/share/fonts/liberation/LiberationSans-Bold.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
        "/Library/Fonts/Arial.ttf",
        "C:\\Windows\\Fonts\\segoeui.ttf",
        "C:\\Windows\\Fonts\\arial.ttf",
    ];
    for p in CANDIDATES {
        let Ok(bytes) = std::fs::read(p) else { continue };
        if let Ok(f) = ab_glyph::FontArc::try_from_vec(bytes) {
            tracing::debug!("overlay: loaded font from {p}");
            return Some(f);
        }
    }
    tracing::warn!(
        "overlay: no system font found; install dejavu / noto / liberation \
         fonts to enable text rendering"
    );
    None
}

// ---------------------------------------------------------------------------
//  Pixel helpers
// ---------------------------------------------------------------------------

/// Premultiplied alpha-over composite of `fg` (ARGB) onto `bg` (ARGB).
/// `coverage_alpha` (0..=255) further attenuates `fg`'s alpha — used
/// for sub-pixel glyph anti-aliasing.
#[inline]
fn blend(bg: u32, fg: u32, coverage_alpha: u8) -> u32 {
    let fa = ((fg >> 24) & 0xFF) as u16 * u16::from(coverage_alpha) / 255;
    if fa == 0 {
        return bg;
    }
    let fa = fa as u32;
    let inv = 255 - fa;
    let fr = (fg >> 16) & 0xFF;
    let fg_g = (fg >> 8) & 0xFF;
    let fb = fg & 0xFF;
    let ba = (bg >> 24) & 0xFF;
    let br = (bg >> 16) & 0xFF;
    let bg_g = (bg >> 8) & 0xFF;
    let bb = bg & 0xFF;
    let out_a = (fa * 255 + ba * inv) / 255;
    let out_r = (fr * fa + br * inv) / 255;
    let out_g = (fg_g * fa + bg_g * inv) / 255;
    let out_b = (fb * fa + bb * inv) / 255;
    (out_a << 24) | (out_r << 16) | (out_g << 8) | out_b
}

/// Draw a filled rounded rectangle with anti-aliased corners.
fn fill_round_rect(buf: &mut [u32], stride: u32, h: u32, rect: (f32, f32, f32, f32), radius: f32, color: u32) {
    let (x0, y0, x1, y1) = rect;
    let r = radius.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0);
    let yi0 = y0.max(0.0) as i32;
    let yi1 = (y1.min(h as f32) as i32).max(yi0);
    for yi in yi0..yi1 {
        let yf = yi as f32 + 0.5;
        let xi0 = x0.max(0.0) as i32;
        let xi1 = (x1.min(stride as f32) as i32).max(xi0);
        for xi in xi0..xi1 {
            let xf = xi as f32 + 0.5;
            // Distance to nearest corner — only relevant in the
            // corner quadrants. For pixels in the rectilinear interior
            // we just fill at full alpha.
            let dx = if xf < x0 + r {
                x0 + r - xf
            } else if xf > x1 - r {
                xf - (x1 - r)
            } else {
                0.0
            };
            let dy = if yf < y0 + r {
                y0 + r - yf
            } else if yf > y1 - r {
                yf - (y1 - r)
            } else {
                0.0
            };
            let coverage = if dx == 0.0 && dy == 0.0 {
                255u8
            } else {
                let d2 = dx * dx + dy * dy;
                if d2 >= (r + 0.5) * (r + 0.5) {
                    0
                } else {
                    // Soft edge across 1 px: cov = clamp(r + 0.5 - d, 0, 1).
                    let d = d2.sqrt();
                    let cov = (r + 0.5 - d).clamp(0.0, 1.0);
                    (cov * 255.0) as u8
                }
            };
            if coverage == 0 {
                continue;
            }
            let idx = (yi as u32 * stride + xi as u32) as usize;
            if let Some(slot) = buf.get_mut(idx) {
                *slot = blend(*slot, color, coverage);
            }
        }
    }
}

// ---------------------------------------------------------------------------
//  Word wrapping
// ---------------------------------------------------------------------------

/// Wrap `text` into lines that fit within `max_width` pixels at
/// `size_px`. Splits on whitespace; very long single words are
/// truncated with an ellipsis to avoid runaway widths.
fn wrap_text(font: &ab_glyph::FontArc, text: &str, size_px: f32, max_width: f32) -> Vec<String> {
    use ab_glyph::{Font, ScaleFont};
    let scaled = font.as_scaled(size_px);
    let advance = |s: &str| -> f32 {
        s.chars()
            .map(|c| scaled.h_advance(font.glyph_id(c)))
            .sum::<f32>()
    };
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            // First word on a fresh line — accept even if too wide;
            // we'll truncate below.
            current.push_str(word);
            continue;
        }
        let candidate_width = advance(&current) + scaled.h_advance(font.glyph_id(' ')) + advance(word);
        if candidate_width <= max_width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    // Truncate any individual line that's still too wide (very long
    // single word).
    for line in &mut lines {
        if advance(line) > max_width {
            // Drop chars from the end until it fits, append ellipsis.
            while !line.is_empty() && advance(line) + scaled.h_advance(font.glyph_id('…')) > max_width {
                line.pop();
            }
            line.push('…');
        }
    }
    lines
}

#[allow(clippy::too_many_arguments)]
fn draw_line(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    font: &ab_glyph::FontArc,
    text: &str,
    color: u32,
    size_px: f32,
    x_origin: f32,
    baseline_y: f32,
) {
    use ab_glyph::{Font, ScaleFont};
    let scaled = font.as_scaled(size_px);
    let mut x = x_origin;
    for ch in text.chars() {
        let glyph_id = font.glyph_id(ch);
        let glyph = glyph_id.with_scale_and_position(size_px, ab_glyph::point(x, baseline_y));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, coverage| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                if px < 0 || py < 0 {
                    return;
                }
                let (px, py) = (px as u32, py as u32);
                if px >= stride || py >= h {
                    return;
                }
                let idx = (py * stride + px) as usize;
                let Some(slot) = buf.get_mut(idx) else { return };
                let alpha = (coverage.clamp(0.0, 1.0) * 255.0) as u8;
                *slot = blend(*slot, color, alpha);
            });
        }
        x += scaled.h_advance(glyph_id);
    }
}

/// Compute target window height (logical px) that fits `n_lines` of
/// transcript text at `TEXT_FONT_PX`, clamped to [`WIN_MIN_HEIGHT`,
/// `WIN_MAX_HEIGHT`].
fn target_height(n_lines: usize) -> f32 {
    let n = n_lines.max(1) as f32;
    let lines_h = TEXT_FONT_PX * n + LINE_GAP * (n - 1.0).max(0.0);
    (PADDING_TOP + STATUS_FONT_PX + STATUS_TO_TEXT + lines_h + PADDING_BOT)
        .clamp(WIN_MIN_HEIGHT, WIN_MAX_HEIGHT)
}

// ---------------------------------------------------------------------------
//  Event loop
// ---------------------------------------------------------------------------

#[allow(clippy::items_after_statements, clippy::too_many_lines)]
fn run_event_loop(
    rx: std::sync::mpsc::Receiver<OverlayCmd>,
    proxy_tx: std::sync::mpsc::Sender<winit::event_loop::EventLoopProxy<()>>,
) -> Result<(), String> {
    use std::num::NonZeroU32;
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
            let mut builder = EventLoop::<()>::with_user_event();
            {
                use winit::platform::wayland::EventLoopBuilderExtWayland;
                <_ as EventLoopBuilderExtWayland>::with_any_thread(&mut builder, true);
            }
            {
                use winit::platform::x11::EventLoopBuilderExtX11;
                <_ as EventLoopBuilderExtX11>::with_any_thread(&mut builder, true);
            }
            builder
                .build()
                .map_err(|e| format!("EventLoop::with_user_event().build(): {e}"))?
        }
        #[cfg(not(target_os = "linux"))]
        {
            EventLoop::<()>::with_user_event()
                .build()
                .map_err(|e| format!("EventLoop::with_user_event().build(): {e}"))?
        }
    };
    let _ = proxy_tx.send(event_loop.create_proxy());
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);

    struct App {
        window: Option<std::sync::Arc<Window>>,
        surface: Option<softbuffer::Surface<std::sync::Arc<Window>, std::sync::Arc<Window>>>,
        font: Option<ab_glyph::FontArc>,
        state: OverlayState,
        text: String,
        /// Cached wrapping for the current `text`; recomputed when
        /// text changes.
        wrapped: Vec<String>,
        rx: std::sync::mpsc::Receiver<OverlayCmd>,
    }

    impl ApplicationHandler<()> for App {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attrs = Window::default_attributes()
                .with_title("Fono")
                .with_decorations(false)
                .with_resizable(false)
                .with_transparent(true)
                .with_window_level(WindowLevel::AlwaysOnTop)
                .with_inner_size(winit::dpi::LogicalSize::new(WIN_WIDTH, WIN_MIN_HEIGHT))
                // Critical: don't steal focus from the user's
                // currently-focused window. Without this, the moment
                // the overlay maps it grabs keyboard focus and the
                // ensuing inject (which paste-synthesizes into the
                // *focused* window) lands in the overlay itself.
                .with_active(false)
                .with_visible(false);
            // Platform-specific focus-suppression. On X11 we use a
            // belt-and-braces approach because window managers
            // disagree about how aggressively to honour the hints
            // above on subsequent map cycles (the overlay is shown
            // and hidden once per dictation, and many WMs default to
            // "give focus on map" on the second+ map even for
            // notification windows):
            //
            //   * with_x11_window_type(Notification) — declares the
            //     window as a notification toplevel per EWMH. WMs
            //     should skip focus, taskbar, pager, alt-tab.
            //   * with_override_redirect(true) — bypasses the WM
            //     entirely. The X server never asks the WM about
            //     focus, mapping, or stacking for this window. This
            //     is what tooltips, dmenu, rofi all do; it makes
            //     focus theft physically impossible on X11
            //     regardless of WM behaviour. Trade-off: we lose
            //     WM-managed always-on-top, but borderless
            //     override-redirect windows naturally stack above
            //     normal toplevels because the WM doesn't move them.
            //
            // On Wayland the compositor controls focus completely;
            // a proper xdg_activation_v1 / wlr-layer-shell solution
            // lands in Slice B's subprocess overlay refactor.
            #[cfg(all(unix, not(target_os = "macos")))]
            let attrs = {
                use winit::platform::x11::{WindowAttributesExtX11, WindowType};
                attrs
                    .with_x11_window_type(vec![WindowType::Notification])
                    .with_override_redirect(true)
            };
            let win = el.create_window(attrs).map_or_else(
                |e| {
                    tracing::warn!("overlay: create_window failed: {e}");
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
                        self.state = s;
                        if let Some(w) = self.window.as_ref() {
                            w.set_visible(!matches!(s, OverlayState::Hidden));
                            needs_redraw = true;
                        }
                    }
                    OverlayCmd::UpdateText(t) => {
                        if t != self.text {
                            self.text = t;
                            self.wrapped = if let (Some(font), false) =
                                (self.font.as_ref(), self.text.is_empty())
                            {
                                let max_w = WIN_WIDTH - PADDING_X * 2.0 - ACCENT_WIDTH;
                                wrap_text(font, &self.text, TEXT_FONT_PX, max_w)
                            } else {
                                Vec::new()
                            };
                            if self.window.is_some() {
                                needs_redraw = true;
                                needs_resize = true;
                            }
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
                    let h = target_height(self.wrapped.len());
                    let _ = w.request_inner_size(winit::dpi::LogicalSize::new(WIN_WIDTH, h));
                    // Re-position so the overlay still hugs the bottom
                    // when its height changes.
                    if let Some(monitor) = w.current_monitor() {
                        let mon_size = monitor.size();
                        let win_size = w.outer_size();
                        let x = (mon_size.width.saturating_sub(win_size.width)) / 2;
                        let y = mon_size
                            .height
                            .saturating_sub(win_size.height + BOTTOM_OFFSET);
                        w.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
                    }
                }
            }
            if needs_redraw {
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
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
        // Clear to fully transparent — compositor lets the desktop
        // show through outside our rounded panel.
        for px in buf.iter_mut() {
            *px = 0x0000_0000;
        }

        // Convert logical coords to physical using the window's scale.
        let scale = window.scale_factor() as f32;
        let panel = (
            0.0,
            0.0,
            w as f32,
            h as f32,
        );
        // Rounded panel (translucent dark charcoal).
        fill_round_rect(&mut buf, w, h, panel, CORNER_RADIUS * scale, COLOR_BG);

        // Coloured accent stripe down the left edge — same rounded
        // shape but trimmed to the leftmost ACCENT_WIDTH band by
        // drawing a fresh rounded rect over it.
        let accent = accent_color(app.state);
        if (accent >> 24) & 0xFF != 0 {
            let stripe = (
                0.0,
                CORNER_RADIUS * scale * 0.4,
                ACCENT_WIDTH * scale,
                h as f32 - CORNER_RADIUS * scale * 0.4,
            );
            fill_round_rect(
                &mut buf,
                w,
                h,
                stripe,
                ACCENT_WIDTH * scale * 0.5,
                accent,
            );
        }

        // Text content.
        if let Some(font) = app.font.as_ref() {
            let pad_x = (PADDING_X + ACCENT_WIDTH) * scale;
            let pad_top = PADDING_TOP * scale;
            // Status row.
            let label = state_label(app.state);
            if !label.is_empty() {
                let status_baseline = pad_top + STATUS_FONT_PX * scale * 0.85;
                draw_line(
                    &mut buf,
                    w,
                    h,
                    font,
                    label,
                    COLOR_TEXT_DIM,
                    STATUS_FONT_PX * scale,
                    pad_x,
                    status_baseline,
                );
            }
            // Transcript rows.
            if !app.wrapped.is_empty() {
                let text_top = pad_top + STATUS_FONT_PX * scale + STATUS_TO_TEXT * scale;
                let mut baseline = text_top + TEXT_FONT_PX * scale * 0.85;
                let max_visible_lines =
                    ((h as f32 - text_top - PADDING_BOT * scale)
                        / (TEXT_FONT_PX * scale + LINE_GAP * scale)) as usize;
                let total = app.wrapped.len();
                // If content exceeds visible space, show the TAIL —
                // most recent text always visible.
                let skip = total.saturating_sub(max_visible_lines.max(1));
                for line in app.wrapped.iter().skip(skip) {
                    draw_line(
                        &mut buf,
                        w,
                        h,
                        font,
                        line,
                        COLOR_TEXT,
                        TEXT_FONT_PX * scale,
                        pad_x,
                        baseline,
                    );
                    baseline += TEXT_FONT_PX * scale + LINE_GAP * scale;
                    if baseline > h as f32 - PADDING_BOT * scale {
                        break;
                    }
                }
            }
        }

        let _ = buf.present();
    }

    let mut app = App {
        window: None,
        surface: None,
        font: load_system_font(),
        state: OverlayState::Hidden,
        text: String::new(),
        wrapped: Vec::new(),
        rx,
    };
    event_loop
        .run_app(&mut app)
        .map_err(|e| format!("run_app: {e}"))
}
