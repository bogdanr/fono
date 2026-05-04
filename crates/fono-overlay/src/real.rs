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

#![allow(
    clippy::suboptimal_flops,
    clippy::branches_sharing_code,
    clippy::cognitive_complexity,
    clippy::many_single_char_names
)]

use std::collections::VecDeque;
use std::io;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use fono_core::config::WaveformStyle;

use crate::OverlayState;

/// How the overlay window renders the `Recording` state. `Text` keeps
/// the existing transcript-style panel; `Waveform(style)` swaps the
/// transcript area for an audio visualisation.
#[derive(Debug, Clone, Copy)]
enum OverlayMode {
    Text,
    Waveform(WaveformStyle),
}

/// Ring-buffer caps. 60 levels = 2 s at the 33 ms ticker cadence;
/// 5000 samples ≈ 312 ms at 16 kHz — wide enough for the oscilloscope
/// to scroll slowly across the panel, so individual voice cycles are
/// visible rather than blurring into a uniform band. 120 FFT frames
/// at 30 fps ≈ 4 s of spectrogram history.
const LEVELS_CAP: usize = 60;
const OSC_SAMPLES_CAP: usize = 5000;
const FFT_FRAMES_CAP: usize = 120;

/// Commands sent from the main thread to the overlay's winit thread.
#[derive(Debug)]
enum OverlayCmd {
    SetState(OverlayState),
    UpdateText(String),
    /// Normalised RMS amplitude in `[0.0, 1.0]`. Drives the bars
    /// ring buffer and the live-dictation VU bar.
    AudioLevel(f32),
    /// Raw f32 PCM samples (16 kHz mono). Only consumed by the
    /// `Oscilloscope` style; kept separate from `AudioLevel` so the
    /// higher-cadence path doesn't bloat the bar path.
    AudioSamples(Vec<f32>),
    /// One frame of normalised FFT magnitude bins in `[0.0, 1.0]`.
    /// Consumed by the `Fft` style (latest frame only) and the
    /// `Heatmap` style (rolling history).
    FftBins(Vec<f32>),
    /// Toggle the right-side VU bar on the live-dictation panel.
    /// Pushed at startup once `Config.overlay.volume_bar` is known.
    SetVolumeBar(bool),
    /// Swap the waveform style at runtime so a `[overlay].style`
    /// config edit (or tray "Waveform style" pick) takes effect
    /// without a daemon restart. No-op when the overlay was spawned
    /// in `Text` mode; the visualisation overlay only switches
    /// between waveform variants.
    SetWaveformStyle(WaveformStyle),
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

    /// Push a normalised RMS amplitude in `[0.0, 1.0]`. Used by both
    /// the standalone waveform overlay (bars/pulse) and the live-
    /// dictation VU bar.
    pub fn push_level(&self, amplitude: f32) {
        self.send(OverlayCmd::AudioLevel(amplitude));
    }

    /// Push a batch of raw f32 PCM samples (16 kHz mono). Consumed
    /// only by the `Oscilloscope` waveform style.
    pub fn push_samples(&self, samples: Vec<f32>) {
        self.send(OverlayCmd::AudioSamples(samples));
    }

    /// Push one frame of normalised FFT magnitude bins. Each entry
    /// is in `[0.0, 1.0]` (post-dB-mapping); the slice length is
    /// fixed by the producer (typically 64). Consumed by the `Fft`
    /// and `Heatmap` waveform styles.
    pub fn push_fft_bins(&self, bins: Vec<f32>) {
        self.send(OverlayCmd::FftBins(bins));
    }

    /// Enable or disable the right-side VU bar on the live-dictation
    /// panel. Set once at orchestrator startup based on
    /// `[overlay].volume_bar`.
    pub fn set_volume_bar(&self, enabled: bool) {
        self.send(OverlayCmd::SetVolumeBar(enabled));
    }

    /// Swap the active waveform style at runtime. No-op when the
    /// overlay was spawned in `Text` mode (live-dictation overlay).
    /// Pushed by the orchestrator's `reload()` after a
    /// `[overlay].style` config change so the visualisation
    /// switches without a daemon restart.
    pub fn set_waveform_style(&self, style: WaveformStyle) {
        self.send(OverlayCmd::SetWaveformStyle(style));
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
    /// Spawn the standard text-based overlay used by live dictation.
    /// `Recording` state shows the existing wrapped-transcript panel.
    pub fn spawn() -> std::io::Result<OverlayHandle> {
        Self::spawn_with_mode(OverlayMode::Text)
    }

    /// Spawn the standalone audio-visualisation overlay used during
    /// batch recording. `Recording` state renders the requested
    /// waveform style instead of transcript text.
    pub fn spawn_waveform(style: WaveformStyle) -> std::io::Result<OverlayHandle> {
        Self::spawn_with_mode(OverlayMode::Waveform(style))
    }

    fn spawn_with_mode(mode: OverlayMode) -> std::io::Result<OverlayHandle> {
        let (tx, rx) = channel::<OverlayCmd>();
        let (proxy_tx, proxy_rx) =
            std::sync::mpsc::channel::<Result<winit::event_loop::EventLoopProxy<()>, String>>();
        let join = std::thread::Builder::new()
            .name("fono-overlay".into())
            .spawn(move || {
                if let Err(e) = run_event_loop(rx, proxy_tx, mode) {
                    tracing::warn!("overlay: event loop ended with error: {e:#}");
                }
            })?;
        let proxy = match proxy_rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(Ok(proxy)) => proxy,
            Ok(Err(msg)) => {
                let _ = join.join();
                return Err(io::Error::other(msg));
            }
            Err(e) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("overlay event loop did not become ready within 2s: {e}"),
                ));
            }
        };
        Ok(OverlayHandle {
            inner: Arc::new(HandleInner {
                tx,
                proxy: Mutex::new(Some(proxy)),
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
/// Fixed height for the standalone waveform overlay. Status row + a
/// roomy ~56 px visualisation area; smaller than the live-dictation
/// max so the panel doesn't dominate the screen during batch
/// recording.
const WIN_WAVEFORM_HEIGHT: f32 = 100.0;
/// Inset from the bottom edge.
const BOTTOM_OFFSET: u32 = 48;

const PADDING_X: f32 = 24.0;
const PADDING_TOP: f32 = 14.0;
const PADDING_BOT: f32 = 16.0;
const ACCENT_WIDTH: f32 = 4.0;
/// Right-side vertical VU meter on the live-dictation panel. Logical
/// pixels — multiply by `scale` in renderers.
const VU_BAR_WIDTH: f32 = 8.0;
const VU_BAR_GAP: f32 = 6.0;
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

/// Accent stripe colour per state. Mirrored by the tray icon so the
/// user sees the same colour story in both places: red = dictation,
/// green = assistant recording, amber = thinking / processing,
/// indigo = live dictation.
fn accent_color(state: OverlayState) -> u32 {
    match state {
        OverlayState::Hidden => 0x0000_0000,
        // Soft red (dictation recording).
        OverlayState::Recording { .. } => 0xFFE0_5454,
        // Saturated green (assistant recording).
        OverlayState::AssistantRecording { .. } => 0xFF22_C55E,
        // Warm amber — assistant is thinking / TTS warming up.
        // Matches the tray's `Processing` state so both surfaces
        // tell the same colour story.
        OverlayState::AssistantThinking => 0xFFF5_9E0B,
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
        OverlayState::AssistantRecording { .. } => "ASSISTANT",
        OverlayState::AssistantThinking => "THINKING",
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
        let Ok(bytes) = std::fs::read(p) else {
            continue;
        };
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
///
/// Hot path: ~99 % of pixels in a typical panel BG / accent-stripe
/// fill sit in the rectilinear interior where the corner-distance
/// computation always returns full coverage. Splitting the iteration
/// into "AA bands" (corners + edges that touch them) and an
/// "interior band" (a row range that's guaranteed dx == dy == 0)
/// keeps the blend in the inner loop and skips the per-pixel sqrt
/// for everything else.
fn fill_round_rect(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    rect: (f32, f32, f32, f32),
    radius: f32,
    color: u32,
) {
    let (x0, y0, x1, y1) = rect;
    let r = radius.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0);
    let yi0 = y0.max(0.0) as i32;
    let yi1 = (y1.min(h as f32) as i32).max(yi0);
    let xi0 = x0.max(0.0) as i32;
    let xi1 = (x1.min(stride as f32) as i32).max(xi0);
    if yi1 <= yi0 || xi1 <= xi0 {
        return;
    }
    // Rectilinear interior bounds — pixels strictly inside this box
    // satisfy `xf >= x0+r && xf <= x1-r && yf >= y0+r && yf <= y1-r`,
    // which means dx == dy == 0 in the AA formula.
    let inner_x0 = ((x0 + r).ceil() as i32).max(xi0);
    let inner_x1 = ((x1 - r).floor() as i32).min(xi1);
    let inner_y0 = ((y0 + r).ceil() as i32).max(yi0);
    let inner_y1 = ((y1 - r).floor() as i32).min(yi1);
    let r_outer_sq = (r + 0.5) * (r + 0.5);
    for yi in yi0..yi1 {
        let in_inner_band = yi >= inner_y0 && yi < inner_y1;
        if in_inner_band && inner_x1 > inner_x0 {
            // Left AA edge.
            for xi in xi0..inner_x0 {
                fill_round_rect_aa_pixel(buf, stride, xi, yi, x0, y0, x1, y1, r, r_outer_sq, color);
            }
            // Interior fast path — full coverage, no distance math
            // and no blend. Direct assignment because either:
            //   * the pixel was just cleared to 0 (panel BG fill),
            //     and `blend(0, color, 255)` collapses to `color`
            //     exactly; or
            //   * the pixel currently holds the panel BG colour
            //     (accent stripe), and full-coverage write is the
            //     intended "draw stripe over background" semantic.
            let row_off = (yi as u32 * stride) as usize;
            for xi in inner_x0..inner_x1 {
                let idx = row_off + xi as usize;
                if let Some(slot) = buf.get_mut(idx) {
                    *slot = color;
                }
            }
            // Right AA edge.
            for xi in inner_x1..xi1 {
                fill_round_rect_aa_pixel(buf, stride, xi, yi, x0, y0, x1, y1, r, r_outer_sq, color);
            }
        } else {
            // Top / bottom AA band — full-width AA (corners + edge
            // strip share this path).
            for xi in xi0..xi1 {
                fill_round_rect_aa_pixel(buf, stride, xi, yi, x0, y0, x1, y1, r, r_outer_sq, color);
            }
        }
    }
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn fill_round_rect_aa_pixel(
    buf: &mut [u32],
    stride: u32,
    xi: i32,
    yi: i32,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    r: f32,
    r_outer_sq: f32,
    color: u32,
) {
    let xf = xi as f32 + 0.5;
    let yf = yi as f32 + 0.5;
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
        if d2 >= r_outer_sq {
            return;
        }
        let d = d2.sqrt();
        let cov = (r + 0.5 - d).clamp(0.0, 1.0);
        (cov * 255.0) as u8
    };
    let idx = (yi as u32 * stride + xi as u32) as usize;
    if let Some(slot) = buf.get_mut(idx) {
        *slot = blend(*slot, color, coverage);
    }
}

// ---------------------------------------------------------------------------
//  Audio visualisation primitives
// ---------------------------------------------------------------------------

/// Replace the alpha byte of an ARGB colour.
#[inline]
fn with_alpha(color: u32, alpha: u8) -> u32 {
    (u32::from(alpha) << 24) | (color & 0x00FF_FFFF)
}

/// Single-pixel-wide line via Bresenham, clipped to the buffer. Blends
/// via `blend()` at the supplied coverage_alpha.
#[allow(clippy::too_many_arguments, clippy::cast_possible_wrap)]
fn draw_line_segment(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    color: u32,
    coverage_alpha: u8,
) {
    let mut x0 = x0.round() as i32;
    let mut y0 = y0.round() as i32;
    let x1 = x1.round() as i32;
    let y1 = y1.round() as i32;
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        if x0 >= 0 && y0 >= 0 && (x0 as u32) < stride && (y0 as u32) < h {
            let idx = (y0 as u32 * stride + x0 as u32) as usize;
            if let Some(slot) = buf.get_mut(idx) {
                *slot = blend(*slot, color, coverage_alpha);
            }
        }
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

/// Scrolling amplitude bars. Bars fill the content area, glowing
/// brighter as RMS rises; a 1-pixel floor line marks the baseline.
#[allow(clippy::too_many_arguments)]
fn draw_waveform_bars(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    levels: &VecDeque<f32>,
    x0: f32,
    x1: f32,
    y_top: f32,
    y_bot: f32,
    accent: u32,
    scale: f32,
) {
    let n = levels.len();
    if n == 0 {
        return;
    }
    let area_w = (x1 - x0).max(0.0);
    let slot_w = area_w / n as f32;
    let bar_w = (slot_w - 1.0 * scale).max(1.0);
    let area_h = (y_bot - y_top).max(0.0);
    for (i, &v) in levels.iter().enumerate() {
        let v = v.clamp(0.0, 1.0);
        let bar_h = (v * area_h).max(1.0 * scale);
        let bx0 = x0 + i as f32 * slot_w;
        let bx1 = bx0 + bar_w;
        let alpha = 0x33 + ((0xFF - 0x33) as f32 * v) as u32;
        let color = with_alpha(accent, alpha as u8);
        fill_round_rect(
            buf,
            stride,
            h,
            (bx0, y_bot - bar_h, bx1, y_bot),
            2.0 * scale,
            color,
        );
    }
    // Floor line so silence still looks alive.
    let floor_y = y_bot.round() as i32;
    if floor_y >= 0 && (floor_y as u32) < h {
        let xi0 = x0.max(0.0).round() as i32;
        let xi1 = (x1.min(stride as f32)).round() as i32;
        for xi in xi0..xi1 {
            if xi < 0 {
                continue;
            }
            let idx = (floor_y as u32 * stride + xi as u32) as usize;
            if let Some(slot) = buf.get_mut(idx) {
                *slot = blend(*slot, COLOR_TEXT_DIM, 0x33);
            }
        }
    }
}

/// Bars draw that takes a per-bar profile (newest at index 0..N,
/// fully replaced each tick) instead of a time-series ring. Used
/// during the assistant's "thinking" phase so the orchestrator
/// can push a full symmetric centre-out shape via `push_fft_bins`
/// and have the renderer paint each bin as one bar without
/// reinterpreting it as time history. Floor line is drawn the
/// same way so silence still reads as "alive".
#[allow(clippy::too_many_arguments)]
fn draw_waveform_bars_from_profile(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    profile: &[f32],
    x0: f32,
    x1: f32,
    y_top: f32,
    y_bot: f32,
    accent: u32,
    scale: f32,
) {
    let n = profile.len();
    if n == 0 {
        return;
    }
    let area_w = (x1 - x0).max(0.0);
    let slot_w = area_w / n as f32;
    let bar_w = (slot_w - 1.0 * scale).max(1.0);
    let area_h = (y_bot - y_top).max(0.0);
    for (i, &v) in profile.iter().enumerate() {
        let v = v.clamp(0.0, 1.0);
        let bar_h = (v * area_h).max(1.0 * scale);
        let bx0 = x0 + i as f32 * slot_w;
        let bx1 = bx0 + bar_w;
        let alpha = 0x33 + ((0xFF - 0x33) as f32 * v) as u32;
        let color = with_alpha(accent, alpha as u8);
        fill_round_rect(
            buf,
            stride,
            h,
            (bx0, y_bot - bar_h, bx1, y_bot),
            2.0 * scale,
            color,
        );
    }
    let floor_y = y_bot.round() as i32;
    if floor_y >= 0 && (floor_y as u32) < h {
        let xi0 = x0.max(0.0).round() as i32;
        let xi1 = (x1.min(stride as f32)).round() as i32;
        for xi in xi0..xi1 {
            if xi < 0 {
                continue;
            }
            let idx = (floor_y as u32 * stride + xi as u32) as usize;
            if let Some(slot) = buf.get_mut(idx) {
                *slot = blend(*slot, COLOR_TEXT_DIM, 0x33);
            }
        }
    }
}

/// Connected-line waveform centred on the content area. Subsamples
/// the sample ring buffer to one column per logical pixel, drawing a
/// 2-physical-pixel stroke.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn draw_oscilloscope(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    osc_samples: &VecDeque<f32>,
    x0: f32,
    x1: f32,
    y_top: f32,
    y_bot: f32,
    accent: u32,
    scale: f32,
    headroom: f32,
) {
    let cols = ((x1 - x0).max(1.0) as usize).max(1);
    let n = osc_samples.len();
    let y_mid = (y_top + y_bot) * 0.5;
    // `headroom` shrinks the vertical mapping so loud PCM peaks
    // don't slam into the panel edges. Recording paths pass a
    // small reduction (≈0.88); the synthetic thinking path
    // controls its own amplitude and passes 1.0 to use the full
    // panel.
    let half_h = (y_bot - y_top) * 0.5 * headroom;
    // Subtle centre guide.
    let guide_y = y_mid.round() as i32;
    if guide_y >= 0 && (guide_y as u32) < h {
        let xi0 = x0.max(0.0).round() as i32;
        let xi1 = (x1.min(stride as f32)).round() as i32;
        for xi in xi0..xi1 {
            if xi < 0 {
                continue;
            }
            let idx = (guide_y as u32 * stride + xi as u32) as usize;
            if let Some(slot) = buf.get_mut(idx) {
                *slot = blend(*slot, COLOR_TEXT_DIM, 0x22);
            }
        }
    }
    if n == 0 {
        return;
    }
    let map_y = |amp: f32| -> f32 {
        let amp = amp.clamp(-1.0, 1.0);
        (y_mid - amp * half_h).clamp(y_top, y_bot)
    };
    // Walk one pixel column at a time and decimate the ring buffer
    // across the whole viewport. With ~5000 samples in the buffer and
    // ~600 columns, this picks roughly every 8th sample — enough to
    // preserve voice fundamentals while letting the visible window
    // span ~300 ms of audio (a "slow" sweep that exposes individual
    // cycles instead of a uniform blur).
    let mut prev: Option<(f32, f32)> = None;
    for px in 0..cols {
        let frac = if cols <= 1 {
            0.0
        } else {
            px as f32 / (cols - 1) as f32
        };
        let xf = x0 + frac * (x1 - x0);
        let sample_idx = (frac * (n.saturating_sub(1)) as f32) as usize;
        let amp = osc_samples[sample_idx.min(n - 1)];
        let yf = map_y(amp);
        if let Some((px0, py0)) = prev {
            draw_line_segment(buf, stride, h, px0, py0, xf, yf, accent, 0xFF);
            // Cheap 2-pixel stroke: shadow line one physical px down.
            let off = scale.max(1.0);
            draw_line_segment(buf, stride, h, px0, py0 + off, xf, yf + off, accent, 0x80);
        }
        prev = Some((xf, yf));
    }
}

/// Vertical VU meter anchored to the right edge of the panel during
/// live dictation. Filled from the bottom up; the unfilled portion
/// stays as a faint ghost track so the bar's bounds are always
/// visible.
#[allow(clippy::too_many_arguments)]
fn draw_vu_bar(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    level: f32,
    x_right: f32,
    y_top: f32,
    y_bot: f32,
    accent: u32,
    scale: f32,
) {
    let level = level.clamp(0.0, 1.0);
    let bar_w = VU_BAR_WIDTH * scale;
    let radius = bar_w * 0.5;
    let x0 = x_right - bar_w;
    let x1 = x_right;
    if x0 >= x1 || y_top >= y_bot {
        return;
    }
    // Ghost track full-height.
    let ghost = with_alpha(accent, 0x22);
    fill_round_rect(buf, stride, h, (x0, y_top, x1, y_bot), radius, ghost);
    // Filled portion, anchored to bottom.
    let fill_h = level * (y_bot - y_top);
    if fill_h > 0.0 {
        fill_round_rect(
            buf,
            stride,
            h,
            (x0, y_bot - fill_h, x1, y_bot),
            radius,
            with_alpha(accent, 0xFF),
        );
    }
}

/// Spectrum bars (left-to-right = low → high frequency). Each bin's
/// height tracks the magnitude pushed by the FFT producer; alpha
/// glows with magnitude so quiet bins look subdued. Bars are drawn
/// as pixel-aligned solid rects (no rounded corners, no AA) so
/// adjacent bars share an exact pixel boundary — at 300 bins ÷
/// 588 px content area the slots are fractional (≈1.96 px) and any
/// AA between bars would leak through as a sub-pixel sliver.
#[allow(clippy::too_many_arguments)]
fn draw_fft(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    bins: &[f32],
    x0: f32,
    x1: f32,
    y_top: f32,
    y_bot: f32,
    accent: u32,
    scale: f32,
) {
    if bins.is_empty() {
        return;
    }
    let area_w = (x1 - x0).max(0.0);
    let area_h = (y_bot - y_top).max(0.0);
    let bin_count = bins.len() as f32;
    for (i, &v) in bins.iter().enumerate() {
        let v = v.clamp(0.0, 1.0);
        let bar_h = (v * area_h).max(1.0 * scale);
        // Float bar bounds: bx1 of bar i equals bx0 of bar i+1, so
        // the rounded pixel boundary is identical for both — no gap,
        // no overlap, no fractional-pixel AA at the seam.
        let bxf0 = x0 + (i as f32 / bin_count) * area_w;
        let bxf1 = x0 + ((i + 1) as f32 / bin_count) * area_w;
        let xi0 = bxf0.round() as i32;
        let xi1 = bxf1.round() as i32;
        let yi0 = (y_bot - bar_h).round() as i32;
        let yi1 = y_bot.round() as i32;
        if xi1 <= xi0 || yi1 <= yi0 {
            continue;
        }
        let alpha = 0x33 + ((0xFF - 0x33) as f32 * v) as u32;
        let color = with_alpha(accent, alpha as u8);
        let cov = (color >> 24) as u8;
        for yi in yi0..yi1 {
            if yi < 0 || (yi as u32) >= h {
                continue;
            }
            for xi in xi0..xi1 {
                if xi < 0 || (xi as u32) >= stride {
                    continue;
                }
                let idx = (yi as u32 * stride + xi as u32) as usize;
                if let Some(slot) = buf.get_mut(idx) {
                    *slot = blend(*slot, color, cov);
                }
            }
        }
    }
    let _ = scale; // silence unused (kept for API symmetry)
                   // Floor line so silence still looks alive.
    let floor_y = y_bot.round() as i32;
    if floor_y >= 0 && (floor_y as u32) < h {
        let xi0 = x0.max(0.0).round() as i32;
        let xi1 = (x1.min(stride as f32)).round() as i32;
        for xi in xi0..xi1 {
            if xi < 0 {
                continue;
            }
            let idx = (floor_y as u32 * stride + xi as u32) as usize;
            if let Some(slot) = buf.get_mut(idx) {
                *slot = blend(*slot, COLOR_TEXT_DIM, 0x33);
            }
        }
    }
}

/// Like [`draw_fft`] but leaves a 1-px gap between bars. Used during
/// the assistant's "thinking" phase where the gapped layout reads
/// as a discrete spectrum rather than a continuous wall.
///
/// Slot widths are integer-aligned so every bar / gap pair is
/// rendered with exactly the same pixel count. Computing the
/// per-slot extents in float space (`i / n` × area_w) and then
/// rounding to the nearest pixel — as `draw_fft` does — leaves
/// some slots one pixel wider than others when the slot width is
/// non-integer (e.g. 588 px / 100 bins = 5.88 px per slot rounds
/// inconsistently); the gap then alternates 0 / 1 / 0 / 1 across
/// the panel, which the user notices as "lines at unequal
/// distances".
#[allow(clippy::too_many_arguments)]
fn draw_fft_gapped(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    bins: &[f32],
    x0: f32,
    x1: f32,
    y_top: f32,
    y_bot: f32,
    accent: u32,
    scale: f32,
) {
    if bins.is_empty() {
        return;
    }
    let area_w = (x1 - x0).max(0.0);
    let area_h = (y_bot - y_top).max(0.0);
    let n = bins.len();
    // Floor the slot to an integer pixel count and use that for
    // every bar; any leftover area is centred so the bars sit in
    // the middle of the content area. The gap is integer too
    // (1 px at default DPI, scaled up for HiDPI) — clamped to at
    // most half the slot so very dense bin counts still produce a
    // visible bar.
    let slot_px = (area_w / n as f32).floor().max(3.0) as i32;
    // 2-px gap for crisper separation; clamped so dense panels
    // never collapse to zero-pixel bars.
    let gap_px = ((2.0 * scale).round() as i32).max(1).min(slot_px / 2);
    let bar_px = (slot_px - gap_px).max(1);
    let total_px = slot_px * n as i32;
    let leftover = area_w as i32 - total_px;
    let x_start = x0.round() as i32 + leftover.max(0) / 2;
    for (i, &v) in bins.iter().enumerate() {
        let v = v.clamp(0.0, 1.0);
        let bar_h = (v * area_h).max(1.0 * scale);
        let xi0 = x_start + i as i32 * slot_px;
        let xi1 = xi0 + bar_px;
        let yi0 = (y_bot - bar_h).round() as i32;
        let yi1 = y_bot.round() as i32;
        if xi1 <= xi0 || yi1 <= yi0 {
            continue;
        }
        let alpha = 0x33 + ((0xFF - 0x33) as f32 * v) as u32;
        let color = with_alpha(accent, alpha as u8);
        let cov = (color >> 24) as u8;
        for yi in yi0..yi1 {
            if yi < 0 || (yi as u32) >= h {
                continue;
            }
            for xi in xi0..xi1 {
                if xi < 0 || (xi as u32) >= stride {
                    continue;
                }
                let idx = (yi as u32 * stride + xi as u32) as usize;
                if let Some(slot) = buf.get_mut(idx) {
                    *slot = blend(*slot, color, cov);
                }
            }
        }
    }
    let floor_y = y_bot.round() as i32;
    if floor_y >= 0 && (floor_y as u32) < h {
        let xi0 = x0.max(0.0).round() as i32;
        let xi1 = (x1.min(stride as f32)).round() as i32;
        for xi in xi0..xi1 {
            if xi < 0 {
                continue;
            }
            let idx = (floor_y as u32 * stride + xi as u32) as usize;
            if let Some(slot) = buf.get_mut(idx) {
                *slot = blend(*slot, COLOR_TEXT_DIM, 0x33);
            }
        }
    }
}

/// Resize and reset the heatmap cache to all-`COLOR_BG`. Returns
/// `true` if the cache was reinitialised (so the caller knows to
/// skip the leftward scroll for this push — the freshly-cleared
/// buffer has no history to scroll).
fn heatmap_cache_resize(
    cache: &mut Vec<u32>,
    cache_dim: &mut (u32, u32),
    cols: u32,
    rows: u32,
) -> bool {
    let needed = (cols as usize) * (rows as usize);
    if *cache_dim == (cols, rows) && cache.len() == needed {
        return false;
    }
    cache.clear();
    cache.resize(needed, COLOR_BG);
    *cache_dim = (cols, rows);
    true
}

/// Render one column's worth of heatmap pixels (the full vertical
/// frequency strip for `frame`) into `cache` at `[col_x, col_x +
/// width)`. Pixels are stored as `blend(COLOR_BG, heatmap_color,
/// alpha)` so `redraw` can blit them straight into the framebuffer.
#[allow(clippy::too_many_arguments)]
fn heatmap_render_column(
    cache: &mut [u32],
    cols: u32,
    rows: u32,
    col_x: u32,
    width: u32,
    frame: &[f32],
    accent: u32,
) {
    if frame.is_empty() || width == 0 || rows == 0 {
        return;
    }
    let bins_len = frame.len();
    let row_span = rows.saturating_sub(1).max(1);
    for ry in 0..rows {
        let bin_frac = 1.0 - (ry as f32 / row_span as f32);
        let bin_idx = (bin_frac * (bins_len - 1) as f32).round() as usize;
        let v = *frame.get(bin_idx.min(bins_len - 1)).unwrap_or(&0.0);
        let color = heatmap_color(v, accent);
        let cov = (color >> 24) as u8;
        let pre = blend(COLOR_BG, color, cov);
        let row_off = (ry * cols) as usize;
        for cx in col_x..(col_x + width).min(cols) {
            if let Some(slot) = cache.get_mut(row_off + cx as usize) {
                *slot = pre;
            }
        }
    }
}

/// Apply a single FFT frame to the heatmap cache: shift everything
/// leftward by one frame-width, then render the new frame into the
/// rightmost columns. The cache is reinitialised to `COLOR_BG` if
/// the panel content area has resized since the last push.
fn heatmap_cache_push(
    cache: &mut Vec<u32>,
    cache_dim: &mut (u32, u32),
    cols: u32,
    rows: u32,
    frame: &[f32],
    accent: u32,
) {
    let was_resized = heatmap_cache_resize(cache, cache_dim, cols, rows);
    if cols == 0 || rows == 0 {
        return;
    }
    let step = (cols / FFT_FRAMES_CAP as u32).max(1);
    if !was_resized && cols > step {
        // Shift the whole cache leftward by `step` columns, row by
        // row. `copy_within` handles overlapping source / destination.
        let shift = step as usize;
        let span = cols as usize;
        for ry in 0..rows {
            let row_start = (ry * cols) as usize;
            cache.copy_within(row_start + shift..row_start + span, row_start);
        }
    }
    let new_col_x = cols.saturating_sub(step);
    heatmap_render_column(cache, cols, rows, new_col_x, step, frame, accent);
}

/// Map a normalised magnitude `[0, 1]` to a colour suitable for the
/// spectrogram. Below 0.5 we ramp up alpha on the accent base; above
/// 0.5 we shift towards white to highlight peaks.
#[inline]
fn heatmap_color(v: f32, accent: u32) -> u32 {
    let v = v.clamp(0.0, 1.0);
    if v < 0.5 {
        let t = v * 2.0;
        let alpha = (t * 255.0) as u8;
        with_alpha(accent, alpha)
    } else {
        let t = (v - 0.5) * 2.0;
        let lerp = |c: u32, target: u32| -> u32 {
            let cf = c as f32;
            let tf = target as f32;
            (cf + (tf - cf) * t).round() as u32
        };
        let r = lerp((accent >> 16) & 0xFF, 0xFF);
        let g = lerp((accent >> 8) & 0xFF, 0xFF);
        let b = lerp(accent & 0xFF, 0xFF);
        0xFF00_0000 | (r << 16) | (g << 8) | b
    }
}

/// Rolling spectrogram. Time on X (oldest left, newest right);
/// frequency on Y (low at the bottom, high at the top); magnitude as
/// colour intensity.
#[allow(clippy::too_many_arguments)]
fn draw_heatmap(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    frames: &VecDeque<Vec<f32>>,
    x0: f32,
    x1: f32,
    y_top: f32,
    y_bot: f32,
    accent: u32,
) {
    if frames.is_empty() {
        return;
    }
    let frame_count = frames.len();
    let cols = ((x1 - x0).max(1.0) as i32).max(1);
    let rows = ((y_bot - y_top).max(1.0) as i32).max(1);
    let bins_len = frames.iter().map(Vec::len).max().unwrap_or(0).max(1);
    let xi0 = x0.round() as i32;
    let yi0 = y_top.round() as i32;
    for cx in 0..cols {
        let frame_frac = cx as f32 / (cols.max(1) - 1).max(1) as f32;
        let frame_idx = (frame_frac * (frame_count - 1) as f32).round() as usize;
        let frame = &frames[frame_idx.min(frame_count - 1)];
        if frame.is_empty() {
            continue;
        }
        for ry in 0..rows {
            // y inverts so low frequencies sit at the bottom.
            let bin_frac = 1.0 - (ry as f32 / (rows.max(1) - 1).max(1) as f32);
            let bin_idx = (bin_frac * (bins_len - 1) as f32).round() as usize;
            let v = *frame.get(bin_idx.min(frame.len() - 1)).unwrap_or(&0.0);
            let color = heatmap_color(v, accent);
            let px = xi0 + cx;
            let py = yi0 + ry;
            if px < 0 || py < 0 || (px as u32) >= stride || (py as u32) >= h {
                continue;
            }
            let idx = (py as u32 * stride + px as u32) as usize;
            if let Some(slot) = buf.get_mut(idx) {
                *slot = blend(*slot, color, (color >> 24) as u8);
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
        let candidate_width =
            advance(&current) + scaled.h_advance(font.glyph_id(' ')) + advance(word);
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
            while !line.is_empty()
                && advance(line) + scaled.h_advance(font.glyph_id('…')) > max_width
            {
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
    proxy_tx: std::sync::mpsc::Sender<Result<winit::event_loop::EventLoopProxy<()>, String>>,
    mode: OverlayMode,
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
                .map_err(|e| format!("EventLoop::with_user_event().build(): {e}"))
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
        font: Option<ab_glyph::FontArc>,
        state: OverlayState,
        text: String,
        /// Cached wrapping for the current `text`; recomputed when
        /// text changes.
        wrapped: Vec<String>,
        /// Whether this overlay was spawned as a standalone waveform
        /// (and which style) or the regular text panel.
        mode: OverlayMode,
        /// Show the right-side VU bar during `LiveDictating`. Toggled
        /// at startup via `set_volume_bar`; default off so the slim
        /// path (and pre-config-load window) matches existing layout.
        volume_bar: bool,
        /// Ring buffer of normalised RMS amplitudes. Used by `Bars`
        /// and `Pulse` standalone styles, and the live-dictation VU
        /// bar in `Text` mode.
        levels: VecDeque<f32>,
        /// Ring buffer of raw PCM samples. Only consumed by the
        /// `Oscilloscope` standalone style.
        osc_samples: VecDeque<f32>,
        /// Ring buffer of FFT magnitude frames. The `Fft` style only
        /// reads the most recent frame; the `Heatmap` style scrolls
        /// the whole buffer left-to-right as time advances.
        fft_frames: VecDeque<Vec<f32>>,
        /// Pre-blended heatmap pixel buffer covering the panel
        /// content area. Each cell holds `blend(COLOR_BG,
        /// heatmap_color, alpha)` for the frame-and-bin that ended
        /// up there, so `redraw` can blit it into the panel
        /// framebuffer with a straight `copy_from_slice` instead of
        /// re-walking `cols × rows` per frame.
        heatmap_cache: Vec<u32>,
        /// `(cols, rows)` the cache is sized for, in physical
        /// pixels. Reset on any geometry change so the next
        /// `FftBins` push triggers a clean reinit.
        heatmap_cache_dim: (u32, u32),
        rx: std::sync::mpsc::Receiver<OverlayCmd>,
    }

    impl ApplicationHandler<()> for App {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let initial_h = match self.mode {
                OverlayMode::Text => WIN_MIN_HEIGHT,
                OverlayMode::Waveform(_) => WIN_WAVEFORM_HEIGHT,
            };
            let attrs = Window::default_attributes()
                .with_title("Fono")
                .with_decorations(false)
                .with_resizable(false)
                .with_transparent(true)
                .with_window_level(WindowLevel::AlwaysOnTop)
                .with_inner_size(winit::dpi::LogicalSize::new(WIN_WIDTH, initial_h))
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
                    let y = mon_size
                        .height
                        .saturating_sub(win_size.height + BOTTOM_OFFSET);
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
                                let mut max_w = WIN_WIDTH - PADDING_X * 2.0 - ACCENT_WIDTH;
                                if self.volume_bar
                                    && matches!(self.state, OverlayState::LiveDictating)
                                {
                                    max_w -= VU_BAR_WIDTH + VU_BAR_GAP;
                                }
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
                    OverlayCmd::AudioLevel(v) => {
                        let v = v.clamp(0.0, 1.0);
                        if self.levels.len() == LEVELS_CAP {
                            self.levels.pop_front();
                        }
                        self.levels.push_back(v);
                        if self.window.is_some() && !matches!(self.state, OverlayState::Hidden) {
                            needs_redraw = true;
                        }
                    }
                    OverlayCmd::AudioSamples(s) => {
                        self.osc_samples.extend(s);
                        while self.osc_samples.len() > OSC_SAMPLES_CAP {
                            self.osc_samples.pop_front();
                        }
                        if matches!(
                            self.mode,
                            OverlayMode::Waveform(WaveformStyle::Oscilloscope)
                        ) && self.window.is_some()
                            && !matches!(self.state, OverlayState::Hidden)
                        {
                            needs_redraw = true;
                        }
                    }
                    OverlayCmd::FftBins(bins) => {
                        if self.fft_frames.len() == FFT_FRAMES_CAP {
                            self.fft_frames.pop_front();
                        }
                        self.fft_frames.push_back(bins);
                        // Heatmap mode: incrementally update the
                        // pre-blended pixel cache so `redraw` can
                        // memcpy it straight into the framebuffer
                        // instead of re-walking `cols × rows` per
                        // frame.
                        if matches!(self.mode, OverlayMode::Waveform(WaveformStyle::Heatmap),) {
                            if let Some(window) = self.window.as_ref() {
                                let scale = window.scale_factor() as f32;
                                let size = window.inner_size();
                                let cx0 = ((PADDING_X + ACCENT_WIDTH) * scale).round() as i32;
                                let cx1 = (size.width as f32 - PADDING_X * scale).round() as i32;
                                let pad_top = PADDING_TOP * scale;
                                let cy0 =
                                    (pad_top + STATUS_FONT_PX * scale + STATUS_TO_TEXT * scale)
                                        .round() as i32;
                                let cy1 = (size.height as f32 - PADDING_BOT * scale).round() as i32;
                                let cols = (cx1 - cx0).max(0) as u32;
                                let rows = (cy1 - cy0).max(0) as u32;
                                if let Some(latest) = self.fft_frames.back() {
                                    let accent = accent_color(self.state);
                                    heatmap_cache_push(
                                        &mut self.heatmap_cache,
                                        &mut self.heatmap_cache_dim,
                                        cols,
                                        rows,
                                        latest,
                                        accent,
                                    );
                                }
                            }
                        }
                        // Trigger a redraw on every fft_frames push
                        // for the waveform modes that consume them:
                        // FFT and Heatmap always (real-audio path),
                        // and Bars when in AssistantThinking (the
                        // orchestrator pushes per-bar profiles via
                        // fft_frames during thinking — without this
                        // arm the bars sat on a stale frame after
                        // F10 release).
                        let consumes_fft = matches!(
                            self.mode,
                            OverlayMode::Waveform(WaveformStyle::Fft | WaveformStyle::Heatmap)
                        ) || matches!(
                            (self.mode, self.state),
                            (
                                OverlayMode::Waveform(WaveformStyle::Bars),
                                OverlayState::AssistantThinking
                            )
                        );
                        if consumes_fft
                            && self.window.is_some()
                            && !matches!(self.state, OverlayState::Hidden)
                        {
                            needs_redraw = true;
                        }
                    }
                    OverlayCmd::SetWaveformStyle(style) => {
                        // Only swap when this overlay is in
                        // `Waveform` mode and the style actually
                        // differs. Don't promote a `Text` overlay
                        // (live-dictation) into `Waveform` — the
                        // window geometry / text wrapping was sized
                        // for one or the other at spawn time.
                        if let OverlayMode::Waveform(current) = self.mode {
                            if current != style {
                                self.mode = OverlayMode::Waveform(style);
                                if self.window.is_some()
                                    && !matches!(self.state, OverlayState::Hidden)
                                {
                                    needs_redraw = true;
                                }
                                tracing::debug!("overlay: waveform style -> {style:?}");
                            }
                        }
                    }
                    OverlayCmd::SetVolumeBar(enabled) => {
                        if self.volume_bar != enabled {
                            self.volume_bar = enabled;
                            // Re-wrap so the text width matches the new
                            // bar visibility.
                            if let (Some(font), false) = (self.font.as_ref(), self.text.is_empty())
                            {
                                let mut max_w = WIN_WIDTH - PADDING_X * 2.0 - ACCENT_WIDTH;
                                if self.volume_bar
                                    && matches!(self.state, OverlayState::LiveDictating)
                                {
                                    max_w -= VU_BAR_WIDTH + VU_BAR_GAP;
                                }
                                self.wrapped = wrap_text(font, &self.text, TEXT_FONT_PX, max_w);
                            }
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
            if needs_resize && matches!(self.mode, OverlayMode::Text) {
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
            if needs_redraw && self.window.is_some() {
                // Render synchronously rather than going through
                // `request_redraw` → `RedrawRequested`. winit can
                // coalesce / delay queued redraw events on Linux
                // (especially for transparent override-redirect
                // windows), which made 30-Hz audio-level pushes appear
                // to update only every few seconds. Drawing inline
                // bypasses that round-trip and lets the panel keep
                // pace with the ticker.
                redraw(self);
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
        // show through outside our rounded panel. `slice::fill`
        // compiles to memset (or its SIMD equivalent), several × the
        // throughput of the explicit per-pixel loop we used before.
        buf.fill(0x0000_0000);

        // Convert logical coords to physical using the window's scale.
        let scale = window.scale_factor() as f32;
        let panel = (0.0, 0.0, w as f32, h as f32);
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
            fill_round_rect(&mut buf, w, h, stripe, ACCENT_WIDTH * scale * 0.5, accent);
        }

        // Status row + content (text or waveform).
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
            // Content area.
            let text_top = pad_top + STATUS_FONT_PX * scale + STATUS_TO_TEXT * scale;
            match app.mode {
                OverlayMode::Waveform(style)
                    if matches!(
                        app.state,
                        OverlayState::Recording { .. }
                            | OverlayState::AssistantRecording { .. }
                            | OverlayState::AssistantThinking
                    ) =>
                {
                    let x0 = (PADDING_X + ACCENT_WIDTH) * scale;
                    let x1 = w as f32 - PADDING_X * scale;
                    let y_top = text_top;
                    let y_bot = h as f32 - PADDING_BOT * scale;
                    let thinking = matches!(app.state, OverlayState::AssistantThinking);
                    match style {
                        // During AssistantThinking the orchestrator
                        // pushes a per-bar profile via fft_frames
                        // (Symmetric Centre-Out shape); render the
                        // latest profile directly rather than the
                        // levels ring.
                        WaveformStyle::Bars if thinking => {
                            if let Some(profile) = app.fft_frames.back() {
                                draw_waveform_bars_from_profile(
                                    &mut buf, w, h, profile, x0, x1, y_top, y_bot, accent, scale,
                                );
                            }
                        }
                        WaveformStyle::Bars => draw_waveform_bars(
                            &mut buf,
                            w,
                            h,
                            &app.levels,
                            x0,
                            x1,
                            y_top,
                            y_bot,
                            accent,
                            scale,
                        ),
                        WaveformStyle::Oscilloscope => {
                            // Headroom: leave a small margin on
                            // recording so loud PCM peaks don't
                            // clip the panel edges; the thinking
                            // generator emits samples already in
                            // [-1, 1] so it gets the full height.
                            let headroom = if thinking { 1.0 } else { 0.88 };
                            draw_oscilloscope(
                                &mut buf,
                                w,
                                h,
                                &app.osc_samples,
                                x0,
                                x1,
                                y_top,
                                y_bot,
                                accent,
                                scale,
                                headroom,
                            );
                        }
                        WaveformStyle::Fft => {
                            if let Some(latest) = app.fft_frames.back() {
                                if thinking {
                                    draw_fft_gapped(
                                        &mut buf, w, h, latest, x0, x1, y_top, y_bot, accent, scale,
                                    );
                                } else {
                                    draw_fft(
                                        &mut buf, w, h, latest, x0, x1, y_top, y_bot, accent, scale,
                                    );
                                }
                            }
                        }
                        WaveformStyle::Heatmap => {
                            // Fast path: blit the pre-blended cache
                            // built incrementally by the `FftBins`
                            // handler. The cache already has the
                            // panel BG composited under each pixel,
                            // so a straight `copy_from_slice` per row
                            // matches what the full `draw_heatmap`
                            // walk would produce. Falls back to the
                            // full walk when the cache hasn't caught
                            // up to the current panel size yet
                            // (first frame after spawn / resize).
                            let cache_cols = (x1 - x0).round() as i32;
                            let cache_rows = (y_bot - y_top).round() as i32;
                            let cache_ok = cache_cols > 0
                                && cache_rows > 0
                                && app.heatmap_cache_dim == (cache_cols as u32, cache_rows as u32)
                                && app.heatmap_cache.len() == (cache_cols * cache_rows) as usize;
                            if cache_ok {
                                let cx0 = x0.round() as i32;
                                let cy0 = y_top.round() as i32;
                                let cols_u = cache_cols as u32;
                                let rows_u = cache_rows as u32;
                                for ry in 0..rows_u {
                                    let dst_y = cy0 + ry as i32;
                                    if dst_y < 0 || (dst_y as u32) >= h {
                                        continue;
                                    }
                                    let dst_x = cx0.max(0);
                                    let skip = (dst_x - cx0).max(0) as u32;
                                    if skip >= cols_u {
                                        continue;
                                    }
                                    let dst_off = (dst_y as u32 * w + dst_x as u32) as usize;
                                    let src_off = (ry * cols_u + skip) as usize;
                                    let avail_w = w.saturating_sub(dst_x as u32);
                                    let copy_len = (cols_u - skip).min(avail_w) as usize;
                                    if copy_len == 0 {
                                        continue;
                                    }
                                    if dst_off + copy_len <= buf.len()
                                        && src_off + copy_len <= app.heatmap_cache.len()
                                    {
                                        buf[dst_off..dst_off + copy_len].copy_from_slice(
                                            &app.heatmap_cache[src_off..src_off + copy_len],
                                        );
                                    }
                                }
                            } else {
                                draw_heatmap(
                                    &mut buf,
                                    w,
                                    h,
                                    &app.fft_frames,
                                    x0,
                                    x1,
                                    y_top,
                                    y_bot,
                                    accent,
                                );
                            }
                        }
                    }
                }
                _ => {
                    // VU bar on the live-dictation panel — drawn before
                    // text so that even very long lines (clipped at the
                    // edge) don't cover it.
                    if matches!(app.mode, OverlayMode::Text)
                        && matches!(app.state, OverlayState::LiveDictating)
                        && app.volume_bar
                        && !app.levels.is_empty()
                    {
                        let level = app.levels.back().copied().unwrap_or(0.0);
                        let x_right = w as f32 - PADDING_X * scale;
                        let y_top = text_top;
                        let y_bot = h as f32 - PADDING_BOT * scale;
                        draw_vu_bar(&mut buf, w, h, level, x_right, y_top, y_bot, accent, scale);
                    }
                    // Transcript rows (text mode, or waveform mode in
                    // non-Recording states which currently render
                    // status-only).
                    if !app.wrapped.is_empty() {
                        let mut baseline = text_top + TEXT_FONT_PX * scale * 0.85;
                        let max_visible_lines = ((h as f32 - text_top - PADDING_BOT * scale)
                            / (TEXT_FONT_PX * scale + LINE_GAP * scale))
                            as usize;
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
        mode,
        volume_bar: false,
        levels: VecDeque::with_capacity(LEVELS_CAP),
        osc_samples: VecDeque::with_capacity(OSC_SAMPLES_CAP),
        fft_frames: VecDeque::with_capacity(FFT_FRAMES_CAP),
        heatmap_cache: Vec::new(),
        heatmap_cache_dim: (0, 0),
        rx,
    };
    event_loop
        .run_app(&mut app)
        .map_err(|e| format!("run_app: {e}"))
}
