// SPDX-License-Identifier: GPL-3.0-only
//! Pure software-rasterised drawing for the overlay.
//!
//! Everything in this module operates on a `&mut [u32]` ARGB
//! premultiplied framebuffer plus a `RendererState`. It is intentionally
//! decoupled from windowing — no `winit`, no `softbuffer`, no
//! `wayland-client`. Each backend in [`crate::backends`] hands the
//! renderer a framebuffer and gets a fully painted frame back.
//!
//! Extracted from the original `real.rs` event-loop file as part of
//! the 2026-05-19 overlay backend split. No drawing logic changed in
//! the move — the per-style branches, the heatmap cache, the
//! `fill_round_rect` AA path, the wrap-and-tail text rendering, all
//! preserved verbatim.

#![allow(
    clippy::suboptimal_flops,
    clippy::branches_sharing_code,
    clippy::cognitive_complexity,
    clippy::many_single_char_names,
    clippy::too_many_arguments
)]

use std::collections::VecDeque;

use fono_core::config::WaveformStyle;

use crate::{OverlayState, PolishingPhase};

/// True when the chosen [`WaveformStyle`] renders the transcript
/// text panel rather than an audio visualisation. Only `Transcript`
/// is a text-mode style; the other four are passive visualisations.
pub fn is_text_style(style: WaveformStyle) -> bool {
    style.requires_streaming()
}

/// Ring-buffer caps. 60 levels = 2 s at the 33 ms ticker cadence;
/// 5000 samples ≈ 312 ms at 16 kHz — wide enough for the oscilloscope
/// to scroll slowly across the panel, so individual voice cycles are
/// visible rather than blurring into a uniform band. 120 FFT frames
/// at 30 fps ≈ 4 s of spectrogram history.
pub const LEVELS_CAP: usize = 60;
pub const OSC_SAMPLES_CAP: usize = 5000;
pub const FFT_FRAMES_CAP: usize = 120;

// ---------------------------------------------------------------------------
//  Layout constants — tuned by eye against a 1080p display.
// ---------------------------------------------------------------------------

/// Logical pixels.
pub const WIN_WIDTH: f32 = 640.0;
/// Minimum and maximum logical heights. Min fits status row + one line
/// of text; max prevents the overlay from dominating the screen.
pub const WIN_MIN_HEIGHT: f32 = 80.0;
pub const WIN_MAX_HEIGHT: f32 = 240.0;
/// Fixed height for the standalone waveform overlay. Status row + a
/// roomy ~56 px visualisation area; smaller than the live-dictation
/// max so the panel doesn't dominate the screen during batch
/// recording.
pub const WIN_WAVEFORM_HEIGHT: f32 = 100.0;
/// Inset from the bottom edge.
pub const BOTTOM_OFFSET: u32 = 48;

pub const PADDING_X: f32 = 16.0;
pub const PADDING_TOP: f32 = 8.0;
pub const PADDING_BOT: f32 = 8.0;
pub const ACCENT_WIDTH: f32 = 4.0;
pub const CORNER_RADIUS: f32 = 12.0;
pub const STATUS_FONT_PX: f32 = 13.0;
pub const TEXT_FONT_PX: f32 = 20.0;
pub const STATUS_TO_TEXT: f32 = 8.0;
pub const LINE_GAP: f32 = 6.0;

/// `0xAARRGGBB`. The compositor honours the alpha byte when the
/// surface is created with an ARGB format. 80 % opaque dark
/// charcoal — translucent enough to read as a modern overlay,
/// opaque enough for legibility.
pub const COLOR_BG: u32 = 0xCC17_171B;
/// `COLOR_BG` in premultiplied-alpha form. The framebuffer holds
/// premultiplied pixels, so any path that writes the panel BG
/// without going through [`blend`] (interior fast path of
/// [`fill_round_rect`], heatmap cache fill / blit) must use this.
pub const COLOR_BG_PRE: u32 = pre_multiply(COLOR_BG);
pub const COLOR_TEXT: u32 = 0xFFEC_ECF1;
pub const COLOR_TEXT_DIM: u32 = 0xCCAA_AAB2;

/// Accent stripe colour per state. Mirrored by the tray icon so the
/// user sees the same colour story in both places.
pub fn accent_color(state: OverlayState) -> u32 {
    match state {
        OverlayState::Hidden => 0x0000_0000,
        OverlayState::Recording { .. } | OverlayState::Pondering { .. } => 0xFFE0_5454,
        OverlayState::AssistantRecording { .. } | OverlayState::AssistantPondering { .. } => {
            0xFF22_C55E
        }
        OverlayState::AssistantThinking | OverlayState::AssistantSynthesising => 0xFFF5_9E0B,
        OverlayState::AssistantSpeaking => 0xFF38_BDF8,
        OverlayState::Processing | OverlayState::Polishing { .. } => 0xFFE0_A040,
        OverlayState::LiveDictating => 0xFF63_7AFF,
        // Neutral grey — deliberately unsaturated so it reads as
        // "Fono paused, not actively listening" rather than fighting
        // the Recording red for visual attention. Mirrors the
        // tray's `Idle` palette intent.
        OverlayState::Ignoring { .. } => 0xFF6B_7280,
    }
}

pub fn state_label(state: OverlayState) -> &'static str {
    match state {
        OverlayState::Hidden => "",
        OverlayState::Recording { .. } => "RECORDING",
        OverlayState::Pondering { .. } => "PONDERING",
        OverlayState::AssistantRecording { .. } => "ASSISTANT",
        OverlayState::AssistantPondering { .. } => "PONDERING",
        OverlayState::AssistantThinking => "THINKING",
        OverlayState::AssistantSynthesising => "SYNTHESISING",
        OverlayState::AssistantSpeaking => "SPEAKING",
        OverlayState::Processing | OverlayState::Polishing { .. } => "POLISHING",
        OverlayState::LiveDictating => "LIVE",
        OverlayState::Ignoring { .. } => "IGNORED",
    }
}

/// States whose panel is fed `push_level` from the live capture pump
/// and should render the right-side VU bar. Slice 3 expansion
/// (2026-05-22): `Recording` and `Pondering` join `LiveDictating` /
/// `AssistantRecording` so the VU bar is visible during plain
/// toggle / push-to-talk dictation, not only during the live-
/// transcript style.
pub fn state_has_vu_bar(state: OverlayState) -> bool {
    matches!(
        state,
        OverlayState::LiveDictating
            | OverlayState::AssistantRecording { .. }
            | OverlayState::AssistantPondering { .. }
            | OverlayState::Recording { .. }
            | OverlayState::Pondering { .. }
    )
}

// ---------------------------------------------------------------------------
//  Font discovery
// ---------------------------------------------------------------------------

pub fn load_system_font() -> Option<ab_glyph::FontArc> {
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

/// Premultiply the RGB channels of an ARGB colour by its alpha.
#[inline]
pub const fn pre_multiply(color: u32) -> u32 {
    let a = (color >> 24) & 0xFF;
    if a == 0xFF {
        return color;
    }
    let r = (((color >> 16) & 0xFF) * a) / 255;
    let g = (((color >> 8) & 0xFF) * a) / 255;
    let b = ((color & 0xFF) * a) / 255;
    (a << 24) | (r << 16) | (g << 8) | b
}

/// Premultiplied alpha-over composite of `fg` (ARGB) onto `bg` (ARGB).
#[inline]
pub fn blend(bg: u32, fg: u32, coverage_alpha: u8) -> u32 {
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

/// Blend a translucent dark rectangle over `(x0..x1, y0..y1)` — the
/// label scrim (plan Task B3). Darkens whatever is underneath (the
/// bright grid) without erasing it, so the status label stays legible
/// while the visualisation still shows through faintly. Preserves the
/// premultiplied invariant (black source only scales existing
/// channels down).
fn darken_rect(buf: &mut [u32], stride: u32, h: u32, x0: f32, y0: f32, x1: f32, y1: f32) {
    let xi0 = x0.max(0.0) as i32;
    let xi1 = (x1.min(stride as f32)) as i32;
    let yi0 = y0.max(0.0) as i32;
    let yi1 = (y1.min(h as f32)) as i32;
    for y in yi0..yi1 {
        let row = y as u32 * stride;
        for x in xi0..xi1 {
            let idx = (row + x as u32) as usize;
            if let Some(slot) = buf.get_mut(idx) {
                *slot = blend(*slot, 0xB000_0000, 255);
            }
        }
    }
}

/// Draw a filled rounded rectangle with anti-aliased corners.
pub fn fill_round_rect(
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
    let inner_x0 = ((x0 + r).ceil() as i32).max(xi0);
    let inner_x1 = ((x1 - r).floor() as i32).min(xi1);
    let inner_y0 = ((y0 + r).ceil() as i32).max(yi0);
    let inner_y1 = ((y1 - r).floor() as i32).min(yi1);
    let r_outer_sq = (r + 0.5) * (r + 0.5);
    let pre_color = pre_multiply(color);
    for yi in yi0..yi1 {
        let in_inner_band = yi >= inner_y0 && yi < inner_y1;
        if in_inner_band && inner_x1 > inner_x0 {
            for xi in xi0..inner_x0 {
                fill_round_rect_aa_pixel(buf, stride, xi, yi, x0, y0, x1, y1, r, r_outer_sq, color);
            }
            let row_off = (yi as u32 * stride) as usize;
            for xi in inner_x0..inner_x1 {
                let idx = row_off + xi as usize;
                if let Some(slot) = buf.get_mut(idx) {
                    *slot = pre_color;
                }
            }
            for xi in inner_x1..xi1 {
                fill_round_rect_aa_pixel(buf, stride, xi, yi, x0, y0, x1, y1, r, r_outer_sq, color);
            }
        } else {
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

/// Replace the alpha byte of an ARGB colour.
#[inline]
pub fn with_alpha(color: u32, alpha: u8) -> u32 {
    (u32::from(alpha) << 24) | (color & 0x00FF_FFFF)
}

#[allow(clippy::cast_possible_wrap)]
pub fn draw_line_segment(
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

pub fn draw_waveform_bars(
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
        fill_round_rect(buf, stride, h, (bx0, y_bot - bar_h, bx1, y_bot), 2.0 * scale, color);
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

pub fn draw_waveform_bars_from_profile(
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
        fill_round_rect(buf, stride, h, (bx0, y_bot - bar_h, bx1, y_bot), 2.0 * scale, color);
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

pub fn draw_oscilloscope(
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
    let half_h = (y_bot - y_top) * 0.5 * headroom;
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
    let mut prev: Option<(f32, f32)> = None;
    for px in 0..cols {
        let frac = if cols <= 1 { 0.0 } else { px as f32 / (cols - 1) as f32 };
        let xf = x0 + frac * (x1 - x0);
        let sample_idx = (frac * (n.saturating_sub(1)) as f32) as usize;
        let amp = osc_samples[sample_idx.min(n - 1)];
        let yf = map_y(amp);
        if let Some((px0, py0)) = prev {
            draw_line_segment(buf, stride, h, px0, py0, xf, yf, accent, 0xFF);
            let off = scale.max(1.0);
            draw_line_segment(buf, stride, h, px0, py0 + off, xf, yf + off, accent, 0x80);
        }
        prev = Some((xf, yf));
    }
}

pub fn draw_vu_bar(
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
    let bar_w = ACCENT_WIDTH * scale;
    let radius = bar_w * 0.5;
    let x0 = x_right - bar_w;
    let x1 = x_right;
    if x0 >= x1 || y_top >= y_bot {
        return;
    }
    let ghost = with_alpha(accent, 0x22);
    fill_round_rect(buf, stride, h, (x0, y_top, x1, y_bot), radius, ghost);
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

/// Snapshot of the silence-watch envelope follower used by the
/// `Advanced` VU-bar flavour to overlay reference ticks. Stored as
/// raw linear RMS values (0..1, i.e. the same units the envelope
/// follower produces) — the renderer converts them to dBFS for its
/// log-scaled axis.
#[derive(Debug, Clone, Copy, Default)]
pub struct GateMetrics {
    pub inst_rms: f32,
    pub voiced_rms: f32,
    pub silence_rms: f32,
}

const VOICED_TICK_COLOR: u32 = 0xFF7C_FF7C; // soft green
const SILENCE_TICK_COLOR: u32 = 0xFFFF_AA33; // amber

/// dBFS range covered by the `Advanced` bar's vertical axis. The
/// top is 0 dBFS (full-scale digital max), the bottom is -60 dBFS.
const ADV_DBFS_TOP: f32 = 0.0;
const ADV_DBFS_BOT: f32 = -60.0;

/// Map a linear RMS value (0..1) onto the 0..1 vertical position on
/// the `Advanced` bar's dBFS axis.
fn adv_rms_to_pos(rms: f32) -> f32 {
    if rms <= 1.0e-7 {
        return 0.0;
    }
    let dbfs = 20.0 * rms.log10();
    ((dbfs - ADV_DBFS_BOT) / (ADV_DBFS_TOP - ADV_DBFS_BOT)).clamp(0.0, 1.0)
}

/// `Advanced`-mode bar: log-scaled dBFS fill plus two annotation
/// ticks (green = voiced reference, amber = silence threshold).
/// Unlike `draw_vu_bar` (linear), the level fill here uses the same
/// dBFS mapping as the ticks so they live on the same axis. Ticks
/// are drawn 2 px thick.
#[allow(clippy::too_many_arguments)]
pub fn draw_vu_bar_advanced(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    _level: f32,
    x_right: f32,
    y_top: f32,
    y_bot: f32,
    accent: u32,
    scale: f32,
    metrics: GateMetrics,
) {
    let bar_w = ACCENT_WIDTH * scale;
    let radius = bar_w * 0.5;
    let x0 = x_right - bar_w;
    let x1 = x_right;
    if x0 >= x1 || y_top >= y_bot {
        return;
    }
    let ghost = with_alpha(accent, 0x22);
    fill_round_rect(buf, stride, h, (x0, y_top, x1, y_bot), radius, ghost);
    let band_h = y_bot - y_top;
    let y_for = |rms: f32| -> f32 { y_bot - adv_rms_to_pos(rms) * band_h };
    let inst_pos = adv_rms_to_pos(metrics.inst_rms);
    if inst_pos > 0.0 {
        let fill_h = inst_pos * band_h;
        fill_round_rect(
            buf,
            stride,
            h,
            (x0, y_bot - fill_h, x1, y_bot),
            radius,
            with_alpha(accent, 0xFF),
        );
    }
    let tick_pad = 3.0 * scale;
    let xl = x0 - tick_pad;
    let xr = x1 + tick_pad;
    let draw_thick = |buf: &mut [u32], y: f32, color: u32| {
        draw_line_segment(buf, stride, h, xl, y, xr, y, color, 0xFF);
        draw_line_segment(buf, stride, h, xl, y + 1.0, xr, y + 1.0, color, 0xFF);
    };
    if metrics.silence_rms > 0.0 {
        draw_thick(buf, y_for(metrics.silence_rms), SILENCE_TICK_COLOR);
    }
    if metrics.voiced_rms > 0.0 {
        draw_thick(buf, y_for(metrics.voiced_rms), VOICED_TICK_COLOR);
    }
}

pub fn draw_fft(
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
    let bin_count = bins.len();
    let bin_last = bin_count.saturating_sub(1) as f32;
    let xi_l = x0.floor() as i32;
    let xi_r = x1.ceil() as i32;
    let yi_bot = y_bot.round() as i32;
    let min_h = 1.0_f32 * scale;
    // Horizontal smoothstep fade at the left and right edges so the
    // visualisation dissolves into the panel background rather than hitting
    // a hard vertical wall. `fade_w` is the ramp width in logical pixels.
    let fade_w = 14.0_f32 * scale;
    // One continuous silhouette: per pixel column, linearly interpolate the
    // bin envelope and fill from y_bot up to the interpolated height. The
    // topmost row of each column receives fractional coverage so the silhouette
    // edge is sub-pixel smooth rather than stair-stepped.
    for xi in xi_l..xi_r {
        if xi < 0 || (xi as u32) >= stride {
            continue;
        }
        // Sample at the column's centre, in bin-index space, with the
        // bin-centre convention (bin i is centered at i + 0.5).
        let col_centre = xi as f32 + 0.5;
        let pos = ((col_centre - x0) / area_w * bin_count as f32 - 0.5).clamp(0.0, bin_last);
        let lo = pos.floor() as usize;
        let hi = (lo + 1).min(bin_count - 1);
        let t = pos - lo as f32;
        let v = (bins[lo] + (bins[hi] - bins[lo]) * t).clamp(0.0, 1.0);
        let bar_h = (v * area_h).max(min_h);
        let yi0_f = y_bot - bar_h;
        let yi0 = yi0_f.floor() as i32;
        if yi_bot <= yi0 {
            continue;
        }
        // Edge fade: linear ramp from 0..1 over `fade_w` pixels, shaped by
        // a smoothstep S-curve so the transition is gentle at both ends.
        let dist_l = (col_centre - x0) / fade_w;
        let dist_r = (x1 - col_centre) / fade_w;
        let e = dist_l.min(dist_r).clamp(0.0, 1.0);
        let edge_cov = e * e * (3.0 - 2.0 * e);
        let alpha = 0x33 + ((0xFF - 0x33) as f32 * v) as u32;
        let color = with_alpha(accent, alpha as u8);
        let cov = (color >> 24) as u8;
        let top_f = 1.0_f32 - yi0_f.fract();
        for yi in yi0..yi_bot {
            if yi < 0 || (yi as u32) >= h {
                continue;
            }
            let v_cov = if yi == yi0 { top_f } else { 1.0_f32 };
            let final_cov = (cov as f32 * v_cov * edge_cov) as u8;
            let idx = (yi as u32 * stride + xi as u32) as usize;
            if let Some(slot) = buf.get_mut(idx) {
                *slot = blend(*slot, color, final_cov);
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

pub fn draw_fft_gapped(
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
    let bin_count = n as f32;
    let gap_px = ((2.0 * scale).round() as i32).max(1);
    for (i, &v) in bins.iter().enumerate() {
        let v = v.clamp(0.0, 1.0);
        let bar_h = (v * area_h).max(1.0 * scale);
        let bxf0 = x0 + (i as f32 / bin_count) * area_w;
        let bxf1 = x0 + ((i + 1) as f32 / bin_count) * area_w;
        let slot_xi0 = bxf0.round() as i32;
        let slot_xi1 = bxf1.round() as i32;
        let xi0 = slot_xi0;
        let xi1 = (slot_xi1 - gap_px).max(slot_xi0 + 1);
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

pub fn heatmap_cache_resize(
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
    cache.resize(needed, COLOR_BG_PRE);
    *cache_dim = (cols, rows);
    true
}

pub fn heatmap_render_column(
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
    let scale = (bins_len - 1) as f32;
    let rspan_f = row_span as f32;
    for ry in 0..rows {
        // Compute the bin-space range this row covers, then average it so
        // that all ~(bins/rows) bins in the range contribute equally.
        let pos_hi = ((1.0 - ry as f32 / rspan_f) * scale).clamp(0.0, scale);
        let pos_lo = ((1.0 - (ry + 1) as f32 / rspan_f) * scale).clamp(0.0, scale);
        let idx_lo = pos_lo.floor() as usize;
        let idx_hi = (pos_hi.ceil() as usize).min(bins_len - 1);
        let count = (idx_hi - idx_lo + 1) as f32;
        let v: f32 = frame[idx_lo..=idx_hi].iter().sum::<f32>() / count;
        let color = heatmap_color(v, accent);
        let cov = (color >> 24) as u8;
        let pre = blend(COLOR_BG_PRE, color, cov);
        let row_off = (ry * cols) as usize;
        for cx in col_x..(col_x + width).min(cols) {
            if let Some(slot) = cache.get_mut(row_off + cx as usize) {
                *slot = pre;
            }
        }
    }
}

pub fn heatmap_cache_push(
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

#[inline]
pub fn heatmap_color(v: f32, accent: u32) -> u32 {
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

pub fn draw_heatmap(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    frames: &VecDeque<Vec<f32>>,
    x0: f32,
    x1: f32,
    y_top: f32,
    y_bot: f32,
    accent: u32,
    scale: f32,
) {
    if frames.is_empty() {
        return;
    }
    let frame_count = frames.len();
    let cols = ((x1 - x0).max(1.0) as i32).max(1);
    let rows = ((y_bot - y_top).max(1.0) as i32).max(1);
    let xi0 = x0.round() as i32;
    let yi0 = y_top.round() as i32;
    let fade_w = 14.0_f32 * scale;
    for cx in 0..cols {
        let frame_frac = cx as f32 / (cols.max(1) - 1).max(1) as f32;
        let frame_idx = (frame_frac * (frame_count - 1) as f32).round() as usize;
        let frame = &frames[frame_idx.min(frame_count - 1)];
        if frame.is_empty() {
            continue;
        }
        // Horizontal smoothstep edge fade — identical maths to the FFT path.
        let col_centre = (xi0 + cx) as f32 + 0.5;
        let dl = (col_centre - x0) / fade_w;
        let dr = (x1 - col_centre) / fade_w;
        let e = dl.min(dr).clamp(0.0, 1.0);
        let edge_cov = e * e * (3.0 - 2.0 * e);
        let frame_bins = frame.len();
        let fscale = (frame_bins - 1) as f32;
        let rspan_f = (rows.max(1) - 1).max(1) as f32;
        for ry in 0..rows {
            let pos_hi = ((1.0 - ry as f32 / rspan_f) * fscale).clamp(0.0, fscale);
            let pos_lo = ((1.0 - (ry + 1) as f32 / rspan_f) * fscale).clamp(0.0, fscale);
            let idx_lo = pos_lo.floor() as usize;
            let idx_hi = (pos_hi.ceil() as usize).min(frame_bins - 1);
            let count = (idx_hi - idx_lo + 1) as f32;
            let v: f32 = frame[idx_lo..=idx_hi].iter().sum::<f32>() / count;
            let color = heatmap_color(v, accent);
            let px = xi0 + cx;
            let py = yi0 + ry;
            if px < 0 || py < 0 || (px as u32) >= stride || (py as u32) >= h {
                continue;
            }
            let idx = (py as u32 * stride + px as u32) as usize;
            if let Some(slot) = buf.get_mut(idx) {
                let alpha = ((color >> 24) as f32 * edge_cov) as u8;
                *slot = blend(*slot, color, alpha);
            }
        }
    }
}

// ---------------------------------------------------------------------------
//  3D Spectrogram terrain (`WaveformStyle::Terrain3d`)
// ---------------------------------------------------------------------------

/// Mesh resolution along the frequency axis. Wider grid → smoother
/// ridges; the spatial 3-tap blur below softens isolated spikes.
const TERRAIN_FREQ_BINS: usize = 64;
/// Mesh resolution along the time axis.
const TERRAIN_TIME_SLICES: usize = 128;
/// Vertical scale on magnitude. High enough that peaks read as
/// clear ridges against the baseline grid.
const TERRAIN_HEIGHT: f32 = 1.10;
/// Noise floor subtracted from every FFT magnitude before lifting
/// the grid. Without this, the constant low-level energy that
/// sits in any room (HVAC, computer fans, mic self-noise) keeps
/// the front row permanently elevated and washes out real peaks.
const TERRAIN_NOISE_FLOOR: f32 = 0.20;
/// Compression curve. We use a `log10(1 + k*m) / log10(1 + k)`
/// shape — perceptually similar to dB scaling but with smoother
/// behaviour at zero. Lower `k` = more linear; higher `k` = more
/// aggressive compression of the high end.
const TERRAIN_LOG_K: f32 = 18.0;
/// Temporal smoothing coefficient. Low so syllable onsets land
/// almost immediately on the front row.
const TERRAIN_EMA: f32 = 0.22;
/// World-space X half-extent. Sized so the projected front
/// (newest) row reaches the bottom-left and bottom-right
/// corners of the panel. Wider than strictly needed by ~10% so
/// the corners are reliably covered across slightly different
/// panel aspects.
const TERRAIN_X_HALF: f32 = 11.0;
/// Input gain applied after noise-floor subtraction and before
/// log compression. At a normal microphone level (≈ -12 dBFS
/// peaks) the FFT magnitudes saturate the log curve and the
/// grid pegs at full height; this scales the input so the
/// usable dynamic range sits in the middle of the curve.
/// Empirically tuned at ~0.30 — equivalent to the user dropping
/// their mic volume to a third, which they already confirmed
/// makes the visualisation read clearly.
const TERRAIN_INPUT_GAIN: f32 = 0.30;
/// World-space Y baseline. The grid sits below the origin so the
/// projected baseline row lands flush with the panel bottom.
const TERRAIN_Y_BASE: f32 = -1.60;
/// World-space Z of the back (oldest) row. More negative = back
/// row sits closer to the top of the panel because perspective
/// pushes it further into the distance.
const TERRAIN_Z_BACK: f32 = -6.5;
/// World-space Z of the front (newest) row.
const TERRAIN_Z_FRONT: f32 = 0.55;
/// Recent-history window. Only the most recent N frames of the
/// FFT ring are sampled across the mesh, so a syllable that hits
/// the front row reaches the back row in ~N×50 ms — fast enough
/// that the user sees the time flow.
const TERRAIN_RECENT_FRAMES: usize = 50;
/// Silence-floor colour mix. Each accent channel is multiplied by
/// this fraction to produce the colour used for flat (silent)
/// segments; peak segments use the full accent. Segments in
/// between lerp linearly based on their vertex height. Lower
/// values give more contrast between peaks and valleys; 1.0
/// disables the effect (every segment renders in full accent).
const TERRAIN_DIM_FLOOR: f32 = 0.8;
/// Height threshold (as a fraction of `TERRAIN_HEIGHT`) above
/// which a segment is treated as "lifted" and renders at the
/// peak-end colour. Below the threshold the segment renders at
/// the baseline (dim) colour. A small non-zero value
/// (`~0.02–0.05`) is enough to ignore floating-point noise from
/// the EMA smoothing while still snapping to peak colour on any
/// real audio energy.
const TERRAIN_LIFT_THRESHOLD: f32 = 0.03;

/// 3D spectrogram terrain. Renders the FFT history as a Tron-style
/// wireframe grid — horizontal "time" rows and vertical "frequency"
/// columns drawn in the accent colour, with depth-modulated
/// brightness so the front rows pop and the back rows fade calmly
/// into the panel. Audio magnitude lifts vertices on the Y axis so
/// the grid ripples gently when the user speaks; at silence it
/// reads as a flat perspective plane.
///
/// Compared to the filled-triangle surface in `bfa211b`, this
/// wireframe version:
///   - reads as a clean grid even at silence (the filled version
///     had no visible structure once magnitudes dropped);
///   - costs less to render (≈100 line draws instead of ≈2400
///     triangle fills) so it stays at 60 fps on a Kaby Lake CPU;
///   - matches the synthwave / Tron mood the user picked when
///     reviewing the visualisation.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn draw_terrain_3d(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    frames: &VecDeque<Vec<f32>>,
    x0: f32,
    x1: f32,
    y_top: f32,
    y_bot: f32,
    accent: u32,
    _scale: f32,
    elapsed_secs: f32,
) {
    use crate::r3d::{Mat4, Vec3};

    let panel_w = (x1 - x0).max(1.0);
    let panel_h = (y_bot - y_top).max(1.0);
    let aspect = panel_w / panel_h;
    let viewport = (x0, y_top, panel_w, panel_h);

    // Higher, more tilted camera so the back row sits closer to
    // the top of the panel and the front row sits closer to the
    // bottom — more inclined plane, more dramatic perspective.
    let eye = Vec3::new(0.0, 1.35, 1.35);
    let target = Vec3::new(0.0, -0.90, -0.8);
    let view = Mat4::look_at(eye, target, Vec3::new(0.0, 1.0, 0.0));
    let proj = Mat4::perspective(62.0_f32.to_radians(), aspect, 0.1, 100.0);
    let view_proj = proj.mul(view);

    // Step 1: sample raw magnitudes into a (TIME × FREQ) grid.
    // Only the most recent `TERRAIN_RECENT_FRAMES` frames are
    // sampled so audio flows from front to back in ~2.5 s.
    let frame_count = frames.len();
    let window = frame_count.min(TERRAIN_RECENT_FRAMES);
    let window_start = frame_count.saturating_sub(window);
    let mut raw: Vec<f32> = vec![0.0; TERRAIN_FREQ_BINS * TERRAIN_TIME_SLICES];
    for t in 0..TERRAIN_TIME_SLICES {
        let frame_idx = if window == 0 {
            None
        } else {
            let frac = t as f32 / (TERRAIN_TIME_SLICES.max(2) - 1) as f32;
            let offset = (frac * (window - 1) as f32).round() as usize;
            Some((window_start + offset).min(frame_count - 1))
        };
        for f in 0..TERRAIN_FREQ_BINS {
            let mag = frame_idx.map_or_else(
                || {
                    // Synthetic idle ripple — quite subtle so the
                    // grid stays mostly flat when nobody's talking.
                    let fu = f as f32 / TERRAIN_FREQ_BINS as f32;
                    let tu = t as f32 / TERRAIN_TIME_SLICES as f32;
                    let phase = elapsed_secs * 0.8 + fu * 4.0 + tu * 2.0;
                    (phase.sin() * 0.5 + 0.5) * 0.10
                },
                |idx| {
                    let frame = &frames[idx];
                    if frame.is_empty() {
                        0.0
                    } else {
                        let frac = f as f32 / (TERRAIN_FREQ_BINS.max(2) - 1) as f32;
                        let bin = (frac * (frame.len() - 1) as f32).round() as usize;
                        frame[bin.min(frame.len() - 1)]
                    }
                },
            );
            raw[t * TERRAIN_FREQ_BINS + f] = mag;
        }
    }

    // Step 2: spatial 1-2-1 blur across frequency, two passes.
    let mut smoothed: Vec<f32> = raw.clone();
    for _pass in 0..2 {
        let src = smoothed.clone();
        for t in 0..TERRAIN_TIME_SLICES {
            let row = t * TERRAIN_FREQ_BINS;
            for f in 0..TERRAIN_FREQ_BINS {
                let lf = if f == 0 { 0 } else { f - 1 };
                let rf = if f + 1 >= TERRAIN_FREQ_BINS { f } else { f + 1 };
                smoothed[row + f] = (src[row + lf] + src[row + f] * 2.0 + src[row + rf]) * 0.25;
            }
        }
    }
    // Step 3: temporal EMA front-to-back.
    for t in 1..TERRAIN_TIME_SLICES {
        let prev_row = (t - 1) * TERRAIN_FREQ_BINS;
        let row = t * TERRAIN_FREQ_BINS;
        for f in 0..TERRAIN_FREQ_BINS {
            smoothed[row + f] =
                smoothed[row + f] * (1.0 - TERRAIN_EMA) + smoothed[prev_row + f] * TERRAIN_EMA;
        }
    }

    // Step 4: build the vertex grid in world space.
    let log_denom = TERRAIN_LOG_K.ln_1p();
    let mut verts: Vec<Vec3> = Vec::with_capacity(TERRAIN_FREQ_BINS * TERRAIN_TIME_SLICES);
    for t in 0..TERRAIN_TIME_SLICES {
        let tf = t as f32 / (TERRAIN_TIME_SLICES.max(2) - 1) as f32;
        let z = TERRAIN_Z_BACK + tf * (TERRAIN_Z_FRONT - TERRAIN_Z_BACK);
        for f in 0..TERRAIN_FREQ_BINS {
            let ff = f as f32 / (TERRAIN_FREQ_BINS.max(2) - 1) as f32;
            let x = (ff - 0.5) * (TERRAIN_X_HALF * 2.0);
            // Subtract a noise floor (so the front row sits flat at
            // silence) then log-compress so quiet syllables still
            // lift the grid visibly while loud peaks don't blow out.
            let mag = smoothed[t * TERRAIN_FREQ_BINS + f].clamp(0.0, 1.0);
            let denoised = ((mag - TERRAIN_NOISE_FLOOR) / (1.0 - TERRAIN_NOISE_FLOOR)).max(0.0);
            let scaled = (denoised * TERRAIN_INPUT_GAIN).min(1.0);
            let boosted = (TERRAIN_LOG_K * scaled).ln_1p() / log_denom;
            let y = TERRAIN_Y_BASE + boosted * TERRAIN_HEIGHT;
            verts.push(Vec3::new(x, y, z));
        }
    }

    // Per-row depth alpha — front rows bright, back rows dim.
    let row_alpha = |t: usize| -> u8 {
        let tf = t as f32 / (TERRAIN_TIME_SLICES.max(2) - 1) as f32;
        (48.0 + tf * 207.0).clamp(48.0, 255.0) as u8
    };

    // Per-segment colour. Each segment's colour lerps from the
    // dim floor (`accent × TERRAIN_DIM_FLOOR`) at the baseline to
    // the full accent at peak height. Height is read straight off
    // the vertex Y so we reuse the same value that lifts the
    // mesh — no parallel intensity array, no second normalisation.
    let accent_r = ((accent >> 16) & 0xFF) as f32;
    let accent_g = ((accent >> 8) & 0xFF) as f32;
    let accent_b = (accent & 0xFF) as f32;
    let segment_color = |y_a: f32, y_b: f32, alpha: u8| -> u32 {
        let mean_y = (y_a + y_b) * 0.5;
        let h = ((mean_y - TERRAIN_Y_BASE) / TERRAIN_HEIGHT).clamp(0.0, 1.0);
        // Binary step: any lift above the threshold snaps the
        // segment to the full accent; everything below renders at
        // the dim floor. The threshold ignores EMA noise around
        // silence so the flat valleys stay calmly dim.
        let k = if h > TERRAIN_LIFT_THRESHOLD { 1.0 } else { TERRAIN_DIM_FLOOR };
        let r = (accent_r * k) as u32;
        let g = (accent_g * k) as u32;
        let b = (accent_b * k) as u32;
        ((u32::from(alpha)) << 24) | (r << 16) | (g << 8) | b
    };

    let visible_rows: usize = 32;
    let visible_columns: usize = 64;

    // Horizontal lines — drawn as per-segment strokes so each
    // segment can take its own colour based on the mean height of
    // its two endpoints.
    let last_row = TERRAIN_TIME_SLICES - 1;
    let row_indices: Vec<usize> =
        (0..visible_rows).map(|i| (i * last_row) / (visible_rows - 1)).collect();
    for &t in &row_indices {
        let row_start = t * TERRAIN_FREQ_BINS;
        let row = &verts[row_start..row_start + TERRAIN_FREQ_BINS];
        let alpha = row_alpha(t);
        for pair in row.windows(2) {
            let a = pair[0];
            let b = pair[1];
            crate::r3d::draw_line_3d(
                buf,
                stride,
                h,
                a,
                b,
                segment_color(a.y, b.y, alpha),
                0xFF,
                &view_proj,
                viewport,
            );
        }
    }

    // Vertical lines (one per frequency column) — same uniform-
    // distribution trick as the rows, so the rightmost cell is
    // the same width as every other cell instead of being a
    // narrow leftover from `step_by + push last`.
    let last_col = TERRAIN_FREQ_BINS - 1;
    let col_indices: Vec<usize> =
        (0..visible_columns).map(|i| (i * last_col) / (visible_columns - 1)).collect();
    let mut column: Vec<Vec3> = Vec::with_capacity(TERRAIN_TIME_SLICES);
    for &f in &col_indices {
        column.clear();
        for t in 0..TERRAIN_TIME_SLICES {
            column.push(verts[t * TERRAIN_FREQ_BINS + f]);
        }
        // Each column segment uses the mean of the two row alphas
        // it touches for depth fade, and the segment colour ramp
        // for height.
        for t in 0..(TERRAIN_TIME_SLICES - 1) {
            let a = column[t];
            let b = column[t + 1];
            let mean_alpha = ((u16::from(row_alpha(t)) + u16::from(row_alpha(t + 1))) / 2) as u8;
            crate::r3d::draw_line_3d(
                buf,
                stride,
                h,
                a,
                b,
                segment_color(a.y, b.y, mean_alpha),
                0xFF,
                &view_proj,
                viewport,
            );
        }
    }
}

//  System/360 (`WaveformStyle::System360`)
// ---------------------------------------------------------------------------

/// System/360-style FFT visualisation rendered natively as a grid
/// of round dots — evoking the rows of operator-console status
/// lamps on a 1960s mainframe. Each FFT bin maps to a column of
/// dots; magnitude controls how many dots in that column are lit,
/// counting from the bottom up.
#[allow(clippy::too_many_arguments)]
pub fn draw_system_360(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    frames: &VecDeque<Vec<f32>>,
    x0: f32,
    x1: f32,
    y_top: f32,
    y_bot: f32,
    accent: u32,
    _scale: f32,
) {
    let panel_w = (x1 - x0).max(1.0);
    let panel_h = (y_bot - y_top).max(1.0);
    if panel_w < 8.0 || panel_h < 8.0 {
        return;
    }

    // Geometry: 7 dot-rows tall, one row of lamps per FFT-magnitude
    // bucket. Row pitch derived from panel height with vertical
    // padding so the grid doesn't touch the top / bottom margins.
    let dot_rows: usize = 7;
    let row_pitch = (panel_h / (dot_rows as f32 + 0.5)).clamp(2.0, 12.0);
    let dot_diameter = (row_pitch * 0.75).clamp(1.5, 8.0);
    // 60 dot columns across the panel — chunky enough to read as
    // discrete lamps, dense enough to show spectral structure.
    let target_dot_cols: usize = 50;
    let col_pitch = panel_w / target_dot_cols as f32;
    let n_dot_cols = target_dot_cols;

    // Latest FFT frame: pick one magnitude per dot column by
    // linear sampling. Each value lands in [0, 1].
    let mags: Vec<f32> = frames.back().map_or_else(
        || vec![0.0; n_dot_cols],
        |latest| {
            let n = latest.len();
            (0..n_dot_cols)
                .map(|i| {
                    if n == 0 {
                        0.0
                    } else if n == 1 {
                        latest[0].clamp(0.0, 1.0)
                    } else {
                        let f = (i as f32 / (n_dot_cols.saturating_sub(1).max(1)) as f32)
                            * (n - 1) as f32;
                        let idx = (f as usize).min(n - 1);
                        latest[idx].clamp(0.0, 1.0)
                    }
                })
                .collect()
        },
    );
    // Total grid height: dot_rows × row_pitch.
    // panel bottom so the grid sits flush with the status bar.
    let baseline_y = y_bot - row_pitch * 0.5;

    // Accent components for the lit-lamp colour and a much dimmer
    // "off" lamp colour for visual structure (the empty grid stays
    // faintly visible like an idle status panel).
    let ar = ((accent >> 16) & 0xFF) as f32;
    let ag = ((accent >> 8) & 0xFF) as f32;
    let ab = (accent & 0xFF) as f32;
    let off_alpha: u8 = 0x18;
    let on_alpha: u8 = 0xFF;

    let radius = dot_diameter * 0.5;
    for (i, &mag) in mags.iter().enumerate() {
        let lit = (mag * (dot_rows as f32 + 0.001)).clamp(0.0, dot_rows as f32);
        let lit_floor = lit.floor() as usize;
        let partial = lit - lit_floor as f32;
        let cx = x0 + (i as f32 + 0.5) * col_pitch;
        for r in 0..dot_rows {
            let cy = baseline_y - r as f32 * row_pitch;
            let intensity = match r.cmp(&lit_floor) {
                std::cmp::Ordering::Less => 1.0,
                std::cmp::Ordering::Equal => partial,
                std::cmp::Ordering::Greater => 0.0,
            };
            let alpha = if intensity > 0.0 {
                // Lerp between off_alpha and on_alpha by intensity.
                let mix = u16::from(off_alpha)
                    + ((u16::from(on_alpha) - u16::from(off_alpha)) as f32 * intensity) as u16;
                mix.clamp(0, 255) as u8
            } else {
                off_alpha
            };
            // Lit dots blend toward white the more they're lit so
            // peak columns crisp up rather than just changing
            // alpha.
            let mix = intensity * 0.45;
            let r_c = (ar + (255.0 - ar) * mix) as u32;
            let g_c = (ag + (255.0 - ag) * mix) as u32;
            let b_c = (ab + (255.0 - ab) * mix) as u32;
            let color = (0xFF << 24) | (r_c << 16) | (g_c << 8) | b_c;
            fill_disc(buf, stride, h, cx, cy, radius, color, alpha);
        }
    }
}

/// Small filled-disc primitive for the System/360 dot grid. Uses
/// box-coverage anti-aliasing (sample at each pixel centre and
/// compute distance to the disc centre) so dots render cleanly
/// at non-integer pitches.
fn fill_disc(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    cx: f32,
    cy: f32,
    radius: f32,
    color: u32,
    alpha: u8,
) {
    if radius <= 0.0 || alpha == 0 {
        return;
    }
    let x_min = (cx - radius - 1.0).floor() as i32;
    let x_max = (cx + radius + 1.0).ceil() as i32;
    let y_min = (cy - radius - 1.0).floor() as i32;
    let y_max = (cy + radius + 1.0).ceil() as i32;
    for yi in y_min..=y_max {
        if yi < 0 || (yi as u32) >= h {
            continue;
        }
        let dy = yi as f32 + 0.5 - cy;
        for xi in x_min..=x_max {
            if xi < 0 || (xi as u32) >= stride {
                continue;
            }
            let dx = xi as f32 + 0.5 - cx;
            let d2 = dx * dx + dy * dy;
            if d2 >= (radius + 0.5) * (radius + 0.5) {
                continue;
            }
            // Smooth edge over the outermost pixel ring.
            let cov = if d2 <= (radius - 0.5).max(0.0) * (radius - 0.5).max(0.0) {
                1.0
            } else {
                let d = d2.sqrt();
                (radius + 0.5 - d).clamp(0.0, 1.0)
            };
            let a = ((u16::from(alpha) as f32) * cov) as u8;
            if a == 0 {
                continue;
            }
            let idx = (yi as u32 * stride + xi as u32) as usize;
            if let Some(slot) = buf.get_mut(idx) {
                *slot = blend(*slot, color, a);
            }
        }
    }
}

pub fn wrap_text(
    font: &ab_glyph::FontArc,
    text: &str,
    size_px: f32,
    max_width: f32,
) -> Vec<String> {
    use ab_glyph::{Font, ScaleFont};
    let scaled = font.as_scaled(size_px);
    let advance =
        |s: &str| -> f32 { s.chars().map(|c| scaled.h_advance(font.glyph_id(c))).sum::<f32>() };
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
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
    for line in &mut lines {
        if advance(line) > max_width {
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

pub fn draw_line(
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

/// `draw_line` variant that paints one character (`highlight_char_idx`)
/// in `highlight_color` instead of `base_color`. Used for the
/// walking-letter highlight on the "Pondering…" status label —
/// see `OverlayState::Pondering` in `crates/fono-overlay/src/lib.rs`.
/// Pass `None` to render the whole string in `base_color` (identical
/// to `draw_line`).
pub fn draw_line_with_highlight(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    font: &ab_glyph::FontArc,
    text: &str,
    base_color: u32,
    highlight_color: u32,
    highlight_char_idx: Option<usize>,
    size_px: f32,
    x_origin: f32,
    baseline_y: f32,
) {
    draw_line_with_highlight_alpha(
        buf,
        stride,
        h,
        font,
        text,
        base_color,
        highlight_color,
        highlight_char_idx,
        0xFF,
        size_px,
        x_origin,
        baseline_y,
    );
}

/// Same as [`draw_line_with_highlight`], but lets the caller attenuate
/// the highlighted character. Used by the terminal polishing pulse so
/// an overrun reads as "still working" rather than a frozen end-stop.
pub fn draw_line_with_highlight_alpha(
    buf: &mut [u32],
    stride: u32,
    h: u32,
    font: &ab_glyph::FontArc,
    text: &str,
    base_color: u32,
    highlight_color: u32,
    highlight_char_idx: Option<usize>,
    highlight_alpha: u8,
    size_px: f32,
    x_origin: f32,
    baseline_y: f32,
) {
    use ab_glyph::{Font, ScaleFont};
    let scaled = font.as_scaled(size_px);
    let highlight_color = with_alpha(highlight_color, highlight_alpha);
    let mut x = x_origin;
    for (i, ch) in text.chars().enumerate() {
        let color = if Some(i) == highlight_char_idx { highlight_color } else { base_color };
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

/// Pondering label highlight color: warm peach/amber, distinct from
/// the polishing accent so the two states stay visually separable.
/// Locked at `+45° hue shift` per
/// `plans/2026-05-22-fono-auto-stop-silence-v1.md`.
pub const COLOR_PONDER_HIGHLIGHT: u32 = 0xCCE6_B073;

/// Convert fixed-point walk progress into a character index for a
/// `letter_count`-wide status label. Progress is clamped instead of
/// wrapped: once a phase reaches the end it stays there until the
/// orchestrator changes phase.
#[must_use]
pub fn walking_highlight_idx(walk_progress: u16, letter_count: usize) -> Option<usize> {
    if walk_progress == 0 || letter_count == 0 {
        return None;
    }
    let p = u32::from(walk_progress).saturating_sub(1);
    Some(((p * letter_count as u32) / 10_000).min(letter_count as u32 - 1) as usize)
}

/// Convert an `OverlayState::Pondering { walk_progress }` value into
/// the character index of "Pondering" (the 9-letter prefix of
/// "Pondering...") that should be drawn in the highlight color.
/// Returns `None` when `walk_progress == 0` — the 1-second plain
/// grace before the walk begins.
#[must_use]
pub fn pondering_highlight_idx(walk_progress: u16) -> Option<usize> {
    walking_highlight_idx(walk_progress, 9)
}

/// Convert polishing phase + fixed-point progress into the highlighted
/// character of `"POLISHING"`. STT walks left-to-right; LLM cleanup
/// walks the same label right-to-left.
#[must_use]
pub fn polishing_highlight_idx(phase: PolishingPhase, walk_progress: u16) -> Option<usize> {
    let idx = walking_highlight_idx(walk_progress, 9)?;
    Some(match phase {
        PolishingPhase::Transcribing => idx,
        PolishingPhase::Cleanup => 8 - idx,
    })
}

#[must_use]
pub fn polishing_terminal_pulse_alpha(walk_progress: u16, elapsed_secs: f32) -> u8 {
    if walk_progress < 10_000 {
        return 0xFF;
    }
    let wave = (elapsed_secs * std::f32::consts::TAU).sin();
    let t = (wave + 1.0) * 0.5;
    (0x66 as f32 + t * (0xFF - 0x66) as f32).round() as u8
}

/// Compute target window height (logical px) that fits `n_lines` of
/// transcript text at `TEXT_FONT_PX`, clamped to [`WIN_MIN_HEIGHT`,
/// `WIN_MAX_HEIGHT`].
pub fn target_height(n_lines: usize) -> f32 {
    let n = n_lines.max(1) as f32;
    let lines_h = TEXT_FONT_PX * n + LINE_GAP * (n - 1.0).max(0.0);
    (PADDING_TOP + STATUS_FONT_PX + STATUS_TO_TEXT + lines_h + PADDING_BOT)
        .clamp(WIN_MIN_HEIGHT, WIN_MAX_HEIGHT)
}

// ---------------------------------------------------------------------------
//  RendererState — pure data + redraw entry point
// ---------------------------------------------------------------------------

/// Mutable renderer state shared by every backend. Holds the loaded
/// system font, the latest overlay state, the wrapped transcript
/// text, and the ring buffers feeding the four passive
/// visualisations. Backends own one of these and drive it via the
/// `set_*` / `push_*` methods + the [`Self::redraw`] entry point.
pub struct RendererState {
    pub font: Option<ab_glyph::FontArc>,
    pub state: OverlayState,
    pub text: String,
    pub wrapped: Vec<String>,
    pub style: WaveformStyle,
    pub volume_bar: fono_core::config::VolumeBarMode,
    pub gate_metrics: GateMetrics,
    pub levels: VecDeque<f32>,
    pub osc_samples: VecDeque<f32>,
    pub fft_frames: VecDeque<Vec<f32>>,
    pub heatmap_cache: Vec<u32>,
    pub heatmap_cache_dim: (u32, u32),
    /// Animated state for the Glass Cortex style (heat trace +
    /// smoothed per-layer activation). Advanced from the FFT push
    /// path, read by the redraw dispatch — same update/read split
    /// as the heatmap cache.
    pub cortex: crate::cortex::CortexState,
    /// Reference instant captured at renderer construction. The 3D
    /// styles (Lissajous, terrain, blob) derive their auto-rotation
    /// phase and synthetic-idle ripple from
    /// `start_instant.elapsed()` so motion is continuous across
    /// state transitions and independent of redraw cadence.
    pub start_instant: std::time::Instant,
}

impl RendererState {
    pub fn new(style: WaveformStyle) -> Self {
        Self {
            font: load_system_font(),
            state: OverlayState::Hidden,
            text: String::new(),
            wrapped: Vec::new(),
            style,
            volume_bar: fono_core::config::VolumeBarMode::Off,
            gate_metrics: GateMetrics::default(),
            levels: VecDeque::with_capacity(LEVELS_CAP),
            osc_samples: VecDeque::with_capacity(OSC_SAMPLES_CAP),
            fft_frames: VecDeque::with_capacity(FFT_FRAMES_CAP),
            heatmap_cache: Vec::new(),
            heatmap_cache_dim: (0, 0),
            cortex: crate::cortex::CortexState::default(),
            start_instant: std::time::Instant::now(),
        }
    }

    /// Recompute wrapped lines for the current text based on the
    /// current style + volume-bar configuration. No-op when no font
    /// is loaded or text is empty.
    pub fn rewrap(&mut self) {
        self.wrapped = if let (Some(font), false) = (self.font.as_ref(), self.text.is_empty()) {
            let mut max_w = WIN_WIDTH - PADDING_X * 2.0 - ACCENT_WIDTH;
            if self.volume_bar.is_on() && state_has_vu_bar(self.state) {
                max_w -= ACCENT_WIDTH;
            }
            wrap_text(font, &self.text, TEXT_FONT_PX, max_w)
        } else {
            Vec::new()
        };
    }

    pub fn set_state(&mut self, state: OverlayState) {
        self.state = state;
        // The cortex replay engine keys its phase machine (listening /
        // thinking / answering) off the overlay state — notify it on
        // every transition (cheap; a no-op for other styles' data).
        self.cortex.on_state(state);
    }

    /// Update transcript text. Returns true if the text changed
    /// (caller may use this to schedule a redraw / resize).
    pub fn update_text(&mut self, text: String) -> bool {
        if text == self.text {
            return false;
        }
        self.text = text;
        self.rewrap();
        true
    }

    pub fn push_level(&mut self, v: f32) {
        let v = v.clamp(0.0, 1.0);
        if self.levels.len() == LEVELS_CAP {
            self.levels.pop_front();
        }
        self.levels.push_back(v);
    }

    pub fn push_samples(&mut self, s: Vec<f32>) {
        self.osc_samples.extend(s);
        while self.osc_samples.len() > OSC_SAMPLES_CAP {
            self.osc_samples.pop_front();
        }
    }

    pub fn push_fft_bins(&mut self, bins: Vec<f32>) {
        if matches!(self.style, WaveformStyle::Cortex) {
            self.cortex.tick(&bins);
        }
        if self.fft_frames.len() == FFT_FRAMES_CAP {
            self.fft_frames.pop_front();
        }
        self.fft_frames.push_back(bins);
    }

    /// Apply a Glass Cortex replay command. Returns `true` when the
    /// active style is `Cortex` (caller may schedule a redraw).
    pub fn push_cortex_cmd(&mut self, cmd: crate::CortexCmd) -> bool {
        self.cortex.apply(cmd);
        matches!(self.style, WaveformStyle::Cortex)
    }

    /// Whether the active style needs the backend to pump frames on a
    /// timer (no external data push will otherwise trigger repaints).
    /// True only for the Glass Cortex thinking/speaking phases while
    /// visible — listening self-drives from mic FFT and Idle is static.
    pub fn wants_animation_frame(&self) -> bool {
        matches!(self.style, WaveformStyle::Cortex)
            && self.is_visible()
            && self.cortex.needs_animation_frames()
    }

    /// Advance the Glass Cortex animation clock one frame (timer-driven
    /// tick for the thinking/speaking phases). No-op for other styles.
    pub fn animation_tick(&mut self) {
        if matches!(self.style, WaveformStyle::Cortex) {
            self.cortex.tick(&[]);
        }
    }

    /// Idempotent style swap. Returns `(changed, crossed_text_boundary)`.
    /// Caller clears ring buffers / text via [`Self::clear_for_style_swap`]
    /// when changed.
    pub fn set_waveform_style(&mut self, style: WaveformStyle) -> (bool, bool) {
        if self.style == style {
            return (false, false);
        }
        let crossed = is_text_style(self.style) != is_text_style(style);
        self.style = style;
        (true, crossed)
    }

    /// Drop cross-style stale data so a swap doesn't briefly flash
    /// stale content from the previous style.
    pub fn clear_for_style_swap(&mut self) {
        self.text.clear();
        self.wrapped.clear();
        self.levels.clear();
        self.osc_samples.clear();
        self.fft_frames.clear();
        self.heatmap_cache.clear();
        self.heatmap_cache_dim = (0, 0);
        self.cortex.clear();
    }

    pub fn set_volume_bar(&mut self, mode: fono_core::config::VolumeBarMode) -> bool {
        if self.volume_bar == mode {
            return false;
        }
        self.volume_bar = mode;
        self.rewrap();
        true
    }

    pub fn set_gate_metrics(&mut self, metrics: GateMetrics) {
        self.gate_metrics = metrics;
    }

    /// Window height target for the current state (logical px).
    /// Text-mode panels grow to fit the wrapped transcript; waveform
    /// panels use a fixed height.
    pub fn target_logical_height(&self) -> f32 {
        if is_text_style(self.style) {
            target_height(self.wrapped.len())
        } else {
            WIN_WAVEFORM_HEIGHT
        }
    }

    /// Apply an FFT push to the heatmap cache when in
    /// `WaveformStyle::Heatmap` and the panel size is known.
    /// `(cx0, cx1, cy0, cy1)` are physical-pixel bounds of the
    /// panel content area. Backends compute these from their current
    /// surface size + scale and call this from the FFT push path.
    pub fn update_heatmap_cache(&mut self, cx0: i32, cx1: i32, cy0: i32, cy1: i32) {
        if !matches!(self.style, WaveformStyle::Heatmap) {
            return;
        }
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

    /// Whether the renderer should currently be drawing at all. A
    /// `Hidden` state means the backend should present an empty /
    /// transparent frame.
    pub fn is_visible(&self) -> bool {
        !matches!(self.state, OverlayState::Hidden)
    }

    /// Whether the latest FFT push should trigger a redraw given the
    /// current style + state. Bars also consumes FFT during the
    /// assistant-thinking / polishing phases (the orchestrator
    /// pushes per-bar profiles via `push_fft_bins`).
    pub fn fft_push_needs_redraw(&self) -> bool {
        matches!(
            self.style,
            WaveformStyle::Fft
                | WaveformStyle::Heatmap
                | WaveformStyle::Terrain3d
                | WaveformStyle::System360
                | WaveformStyle::Cortex
        ) || matches!(
            (self.style, self.state),
            (
                WaveformStyle::Bars,
                OverlayState::AssistantThinking
                    | OverlayState::AssistantSynthesising
                    | OverlayState::AssistantSpeaking
                    | OverlayState::Polishing { .. },
            )
        )
    }

    /// Whether a fresh PCM sample push should trigger a redraw.
    pub fn samples_push_needs_redraw(&self) -> bool {
        matches!(self.style, WaveformStyle::Oscilloscope)
    }

    /// Draw the status label (top-left). `backing = true` paints a
    /// soft scrim + drop shadow under the text first, so it stays
    /// readable when a full-panel visualisation (Cortex) has already
    /// painted a bright grid across the label row (plan Task B3).
    fn draw_status_label(
        &self,
        buf: &mut [u32],
        w: u32,
        h: u32,
        scale: f32,
        font: &ab_glyph::FontArc,
        accent: u32,
        backing: bool,
    ) {
        let label = state_label(self.state);
        if label.is_empty() {
            return;
        }
        let pad_x = (PADDING_X + ACCENT_WIDTH) * scale;
        let pad_top = PADDING_TOP * scale;
        let status_baseline = pad_top + STATUS_FONT_PX * scale * 0.85;
        let size = STATUS_FONT_PX * scale;
        if backing {
            use ab_glyph::{Font, ScaleFont};
            let scaled = font.as_scaled(size);
            let text_w: f32 = label.chars().map(|c| scaled.h_advance(font.glyph_id(c))).sum();
            // Subtle darkened backing behind just the label so the
            // bright grid underneath doesn't bury it.
            darken_rect(
                buf,
                w,
                h,
                pad_x - 5.0 * scale,
                pad_top - 3.0 * scale,
                pad_x + text_w + 6.0 * scale,
                status_baseline + STATUS_FONT_PX * scale * 0.35,
            );
            // Soft drop shadow: an offset dark copy under the glyphs.
            draw_line(
                buf,
                w,
                h,
                font,
                label,
                0xCC00_0000,
                size,
                pad_x + scale,
                status_baseline + scale,
            );
        }
        if let OverlayState::Pondering { walk_progress, .. }
        | OverlayState::AssistantPondering { walk_progress, .. } = self.state
        {
            draw_line_with_highlight(
                buf,
                w,
                h,
                font,
                label,
                COLOR_TEXT_DIM,
                COLOR_PONDER_HIGHLIGHT,
                pondering_highlight_idx(walk_progress),
                size,
                pad_x,
                status_baseline,
            );
        } else if let OverlayState::Polishing { phase, walk_progress } = self.state {
            draw_line_with_highlight_alpha(
                buf,
                w,
                h,
                font,
                label,
                COLOR_TEXT_DIM,
                accent,
                polishing_highlight_idx(phase, walk_progress),
                polishing_terminal_pulse_alpha(
                    walk_progress,
                    self.start_instant.elapsed().as_secs_f32(),
                ),
                size,
                pad_x,
                status_baseline,
            );
        } else {
            // On a scrim the dim text tone loses contrast; lift it to
            // the full-brightness text colour when backed.
            let color = if backing { COLOR_TEXT } else { COLOR_TEXT_DIM };
            draw_line(buf, w, h, font, label, color, size, pad_x, status_baseline);
        }
    }

    /// Synchronous full-frame redraw into `buf` at `(w, h)` physical
    /// pixels with HiDPI `scale`. `buf.len()` must be `>= w * h`.
    /// Caller is responsible for clearing the buffer first (different
    /// backends have different opacity requirements for the
    /// "transparent" pixels around the rounded panel).
    #[allow(clippy::too_many_lines)]
    pub fn redraw(&self, buf: &mut [u32], w: u32, h: u32, scale: f32) {
        if matches!(self.state, OverlayState::Hidden) {
            return;
        }
        let panel = (0.0, 0.0, w as f32, h as f32);
        fill_round_rect(buf, w, h, panel, CORNER_RADIUS * scale, COLOR_BG);
        let accent = accent_color(self.state);
        if (accent >> 24) & 0xFF != 0 {
            let stripe = (
                0.0,
                CORNER_RADIUS * scale * 0.4,
                ACCENT_WIDTH * scale,
                h as f32 - CORNER_RADIUS * scale * 0.4,
            );
            fill_round_rect(buf, w, h, stripe, ACCENT_WIDTH * scale * 0.5, accent);
        }
        let Some(font) = self.font.as_ref() else {
            return;
        };
        let pad_x = (PADDING_X + ACCENT_WIDTH) * scale;
        let pad_top = PADDING_TOP * scale;
        let text_top = pad_top + STATUS_FONT_PX * scale + STATUS_TO_TEXT * scale;
        let waveform_active = !is_text_style(self.style)
            && matches!(
                self.state,
                OverlayState::Recording { .. }
                    | OverlayState::Pondering { .. }
                    | OverlayState::AssistantRecording { .. }
                    | OverlayState::AssistantPondering { .. }
                    | OverlayState::AssistantThinking
                    | OverlayState::AssistantSynthesising
                    | OverlayState::AssistantSpeaking
                    | OverlayState::Polishing { .. }
            );
        // Full-panel visualisations (Cortex) paint the whole strip
        // including the label row, so the status label is drawn LAST
        // (on top) with a scrim + soft shadow. Every other style keeps
        // the label first, underneath the visualisation (plan Task B3).
        let label_last = waveform_active && matches!(self.style, WaveformStyle::Cortex);
        if !label_last {
            self.draw_status_label(buf, w, h, scale, font, accent, false);
        }
        if waveform_active {
            let x0 = (PADDING_X + ACCENT_WIDTH) * scale;
            let x1 = w as f32 - PADDING_X * scale;
            let y_top = pad_top;
            let y_bot = h as f32 - PADDING_BOT * scale;
            // Treat AssistantSpeaking the same way as AssistantThinking
            // and Polishing for the renderer: synthetic animation
            // frames pushed by the orchestrator at 20 fps via
            // `push_fft_bins` / `push_samples`. The only visible
            // difference between Thinking and Speaking is the title
            // label + accent stripe; everything else stays put so the
            // animation continues seamlessly when the LLM starts
            // streaming.
            let thinking = matches!(
                self.state,
                OverlayState::AssistantThinking
                    | OverlayState::AssistantSynthesising
                    | OverlayState::AssistantSpeaking
                    | OverlayState::Polishing { .. }
            );
            match self.style {
                WaveformStyle::Bars if thinking => {
                    if let Some(profile) = self.fft_frames.back() {
                        draw_waveform_bars_from_profile(
                            buf, w, h, profile, x0, x1, y_top, y_bot, accent, scale,
                        );
                    }
                }
                WaveformStyle::Bars => {
                    draw_waveform_bars(
                        buf,
                        w,
                        h,
                        &self.levels,
                        x0,
                        x1,
                        y_top,
                        y_bot,
                        accent,
                        scale,
                    );
                }
                WaveformStyle::Oscilloscope => {
                    let headroom = if thinking { 1.0 } else { 0.88 };
                    draw_oscilloscope(
                        buf,
                        w,
                        h,
                        &self.osc_samples,
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
                    if let Some(latest) = self.fft_frames.back() {
                        if thinking {
                            draw_fft_gapped(buf, w, h, latest, x0, x1, y_top, y_bot, accent, scale);
                        } else {
                            draw_fft(buf, w, h, latest, x0, x1, y_top, y_bot, accent, scale);
                        }
                    }
                }
                WaveformStyle::Heatmap => {
                    let cache_cols = (x1 - x0).round() as i32;
                    let cache_rows = (y_bot - y_top).round() as i32;
                    let cache_ok = cache_cols > 0
                        && cache_rows > 0
                        && self.heatmap_cache_dim == (cache_cols as u32, cache_rows as u32)
                        && self.heatmap_cache.len() == (cache_cols * cache_rows) as usize;
                    if cache_ok {
                        let cx0 = x0.round() as i32;
                        let cy0 = y_top.round() as i32;
                        let cols_u = cache_cols as u32;
                        let rows_u = cache_rows as u32;
                        // Precompute the horizontal smoothstep edge fade once
                        // per visible column. Each entry is the lerp weight
                        // from COLOR_BG_PRE (0) toward the cached pixel (255).
                        let fade_w = 14.0_f32 * scale;
                        let edge: Vec<u8> = (0..cols_u)
                            .map(|cx| {
                                let col_centre = (cx0 + cx as i32) as f32 + 0.5;
                                let dl = (col_centre - x0) / fade_w;
                                let dr = (x1 - col_centre) / fade_w;
                                let e = dl.min(dr).clamp(0.0, 1.0);
                                let s = e * e * (3.0 - 2.0 * e);
                                (s * 255.0) as u8
                            })
                            .collect();
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
                                && src_off + copy_len <= self.heatmap_cache.len()
                            {
                                // Per-pixel lerp from COLOR_BG_PRE toward the
                                // cached colour by the edge weight. Centre
                                // columns pass through unchanged (weight 255);
                                // edge columns dissolve into the panel BG.
                                for k in 0..copy_len {
                                    let src_px = self.heatmap_cache[src_off + k];
                                    let w8 = edge[skip as usize + k] as u32;
                                    if w8 == 255 {
                                        buf[dst_off + k] = src_px;
                                        continue;
                                    }
                                    let iw = 255 - w8;
                                    let lerp_ch = |sh: u32| -> u32 {
                                        let av = (COLOR_BG_PRE >> sh) & 0xFF;
                                        let bv = (src_px >> sh) & 0xFF;
                                        ((av * iw + bv * w8) / 255) & 0xFF
                                    };
                                    buf[dst_off + k] = (lerp_ch(24) << 24)
                                        | (lerp_ch(16) << 16)
                                        | (lerp_ch(8) << 8)
                                        | lerp_ch(0);
                                }
                            }
                        }
                    } else {
                        draw_heatmap(
                            buf,
                            w,
                            h,
                            &self.fft_frames,
                            x0,
                            x1,
                            y_top,
                            y_bot,
                            accent,
                            scale,
                        );
                    }
                }
                WaveformStyle::Transcript => {}
                WaveformStyle::Terrain3d => {
                    let elapsed_secs = self.start_instant.elapsed().as_secs_f32();
                    draw_terrain_3d(
                        buf,
                        w,
                        h,
                        &self.fft_frames,
                        x0,
                        x1,
                        y_top,
                        y_bot,
                        accent,
                        scale,
                        elapsed_secs,
                    );
                }
                WaveformStyle::System360 => {
                    draw_system_360(
                        buf,
                        w,
                        h,
                        &self.fft_frames,
                        x0,
                        x1,
                        y_top,
                        y_bot,
                        accent,
                        scale,
                    );
                }
                WaveformStyle::Cortex => {
                    let elapsed_secs = self.start_instant.elapsed().as_secs_f32();
                    crate::cortex::draw_cortex(
                        buf,
                        w,
                        h,
                        &self.cortex,
                        x0,
                        x1,
                        y_top,
                        y_bot,
                        accent,
                        scale,
                        elapsed_secs,
                    );
                }
            }
            // Full-panel styles: label on top of the visualisation.
            if label_last {
                self.draw_status_label(buf, w, h, scale, font, accent, true);
            }
        } else if !self.wrapped.is_empty() {
            let mut baseline = text_top + TEXT_FONT_PX * scale * 0.85;
            let max_visible_lines = ((h as f32 - text_top - PADDING_BOT * scale)
                / (TEXT_FONT_PX * scale + LINE_GAP * scale))
                as usize;
            let total = self.wrapped.len();
            let skip = total.saturating_sub(max_visible_lines.max(1));
            for line in self.wrapped.iter().skip(skip) {
                draw_line(buf, w, h, font, line, COLOR_TEXT, TEXT_FONT_PX * scale, pad_x, baseline);
                baseline += TEXT_FONT_PX * scale + LINE_GAP * scale;
                if baseline > h as f32 - PADDING_BOT * scale {
                    break;
                }
            }
        }
        // VU bar draws on top of either waveform or text content. The
        // bar lives in the panel's right margin (between the waveform
        // area's right edge and the panel edge) so it never visually
        // overlaps either branch. Defaults pair `volume_bar` with the
        // visualisation style via the tray, but a manual config edit
        // (`volume_bar = "simple" | "advanced"` with any waveform
        // style) is honoured here.
        if state_has_vu_bar(self.state) && self.volume_bar.is_on() && !self.levels.is_empty() {
            let level = self.levels.back().copied().unwrap_or(0.0);
            let x_right = w as f32;
            let y_top = CORNER_RADIUS * scale * 0.4;
            let y_bot = h as f32 - CORNER_RADIUS * scale * 0.4;
            if matches!(self.volume_bar, fono_core::config::VolumeBarMode::Advanced) {
                draw_vu_bar_advanced(
                    buf,
                    w,
                    h,
                    level,
                    x_right,
                    y_top,
                    y_bot,
                    accent,
                    scale,
                    self.gate_metrics,
                );
            } else {
                draw_vu_bar(buf, w, h, level, x_right, y_top, y_bot, accent, scale);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IgnoreReason;

    #[test]
    fn highlight_idx_zero_is_none() {
        assert_eq!(pondering_highlight_idx(0), None);
    }

    #[test]
    fn highlight_idx_one_is_first_letter() {
        assert_eq!(pondering_highlight_idx(1), Some(0));
    }

    #[test]
    fn highlight_idx_max_is_last_letter() {
        assert_eq!(pondering_highlight_idx(10_000), Some(8));
    }

    #[test]
    fn highlight_idx_is_monotone() {
        let mut last = 0;
        for p in (1..=10_000).step_by(100) {
            let idx = pondering_highlight_idx(p).unwrap();
            assert!(idx >= last);
            assert!(idx < 9);
            last = idx;
        }
    }

    #[test]
    fn pondering_state_label_is_walkable() {
        let lbl = state_label(OverlayState::Pondering { db: 0, walk_progress: 0 });
        assert_eq!(lbl, "PONDERING");
        assert_eq!(lbl.chars().take(9).count(), 9);
    }

    #[test]
    fn state_has_vu_bar_covers_live_audio_states() {
        assert!(state_has_vu_bar(OverlayState::LiveDictating));
        assert!(state_has_vu_bar(OverlayState::AssistantRecording { db: 0 }));
        assert!(state_has_vu_bar(OverlayState::Recording { db: 0 }));
        assert!(state_has_vu_bar(OverlayState::Pondering { db: 0, walk_progress: 0 }));
        assert!(!state_has_vu_bar(OverlayState::Hidden));
        assert!(!state_has_vu_bar(OverlayState::Processing));
        assert!(!state_has_vu_bar(OverlayState::Polishing {
            phase: PolishingPhase::Transcribing,
            walk_progress: 0,
        }));
        assert!(!state_has_vu_bar(OverlayState::AssistantThinking));
    }

    #[test]
    fn polishing_highlight_transcribing_walks_left_to_right() {
        assert_eq!(polishing_highlight_idx(PolishingPhase::Transcribing, 0), None);
        assert_eq!(polishing_highlight_idx(PolishingPhase::Transcribing, 1), Some(0));
        assert_eq!(polishing_highlight_idx(PolishingPhase::Transcribing, 10_000), Some(8));
    }

    #[test]
    fn polishing_highlight_cleanup_walks_right_to_left() {
        assert_eq!(polishing_highlight_idx(PolishingPhase::Cleanup, 0), None);
        assert_eq!(polishing_highlight_idx(PolishingPhase::Cleanup, 1), Some(8));
        assert_eq!(polishing_highlight_idx(PolishingPhase::Cleanup, 10_000), Some(0));
    }

    #[test]
    fn polishing_state_label_stays_polishing() {
        for phase in [PolishingPhase::Transcribing, PolishingPhase::Cleanup] {
            let lbl = state_label(OverlayState::Polishing { phase, walk_progress: 5_000 });
            assert_eq!(lbl, "POLISHING");
        }
    }

    #[test]
    fn polishing_terminal_pulse_only_after_walk_finishes() {
        assert_eq!(polishing_terminal_pulse_alpha(9_999, 0.25), 0xFF);
        assert_eq!(polishing_terminal_pulse_alpha(10_000, 0.25), 0xFF);
        assert_eq!(polishing_terminal_pulse_alpha(10_000, 0.75), 0x66);
    }

    #[test]
    fn set_volume_bar_returns_change_flag() {
        use fono_core::config::VolumeBarMode;
        let mut s = RendererState::new(fono_core::config::WaveformStyle::default());
        assert_eq!(s.volume_bar, VolumeBarMode::Off);
        assert!(s.set_volume_bar(VolumeBarMode::Simple));
        assert!(!s.set_volume_bar(VolumeBarMode::Simple));
        assert!(s.set_volume_bar(VolumeBarMode::Advanced));
    }

    #[test]
    fn config_vu_bar_still_draws_when_enabled() {
        // Regression guard (plan Task B2): the config-driven VU bar in
        // the right margin must keep drawing. The cortex redesign must
        // never touch this path — assert both the simple bar and the
        // advanced bar (with its green voiced / amber silence ticks)
        // paint pixels.
        const W: u32 = 200;
        const H: u32 = 60;
        let x_right = W as f32;
        let y_top = 4.0;
        let y_bot = H as f32 - 4.0;
        let accent = 0xFF38_BDF8;

        // Simple bar with a mid level.
        let mut buf = vec![0u32; (W * H) as usize];
        draw_vu_bar(&mut buf, W, H, 0.6, x_right, y_top, y_bot, accent, 1.0);
        assert!(buf.iter().any(|&p| p != 0), "simple VU bar must paint pixels");

        // Advanced bar with a live level + reference ticks.
        let mut buf = vec![0u32; (W * H) as usize];
        let metrics = GateMetrics { inst_rms: 0.3, voiced_rms: 0.2, silence_rms: 0.02 };
        draw_vu_bar_advanced(&mut buf, W, H, 0.6, x_right, y_top, y_bot, accent, 1.0, metrics);
        assert!(buf.iter().any(|&p| p != 0), "advanced VU bar must paint pixels");
        assert!(buf.contains(&VOICED_TICK_COLOR), "green voiced tick must render");
        assert!(buf.contains(&SILENCE_TICK_COLOR), "amber silence tick must render");
    }

    #[test]
    fn gate_metrics_default_is_zero() {
        let m = GateMetrics::default();
        assert!(m.inst_rms.abs() < f32::EPSILON);
        assert!(m.voiced_rms.abs() < f32::EPSILON);
        assert!(m.silence_rms.abs() < f32::EPSILON);
    }

    #[test]
    fn ignoring_state_paints_neutral_grey() {
        // Slice 5 of plan v7 — the relevance gate's "ignored"
        // flash must read as muted / paused so it doesn't fight
        // the Recording red for visual attention.
        let state = OverlayState::Ignoring { reason: IgnoreReason::BackgroundSpeech };
        assert_eq!(accent_color(state), 0xFF6B_7280);
    }

    #[test]
    fn ignoring_state_label_is_ignored() {
        for reason in [
            IgnoreReason::BackgroundSpeech,
            IgnoreReason::LowConfidence,
            IgnoreReason::EchoFromPrompt,
        ] {
            let state = OverlayState::Ignoring { reason };
            assert_eq!(state_label(state), "IGNORED", "reason={reason:?}");
        }
    }

    #[test]
    fn ignoring_state_hides_vu_bar() {
        // The mic is being re-armed during the flash, so the VU
        // bar would be meaningless. Keep it hidden.
        let state = OverlayState::Ignoring { reason: IgnoreReason::BackgroundSpeech };
        assert!(!state_has_vu_bar(state));
    }
}
