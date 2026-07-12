// SPDX-License-Identifier: GPL-3.0-only
//! Throwaway decision-aid mockups for the "brain" visualisation redesign.
//!
//! Renders THREE candidate concepts (Layer Bars, Neural Current, Deep Scan)
//! across four phase frames each (listening, thinking/prefill, speaking/decode
//! dense, speaking/decode MoE) as static PNGs so the user can pick a direction.
//!
//! This is exploratory scaffolding. It is deliberately isolated from the
//! production scene in `cortex.rs`: all draw code is self-contained here and
//! shares nothing with the live renderer beyond the trivial `blend` primitive.
//! Delete this file once a concept is chosen.
//!
//! Run:
//!   cargo run --release -p fono-overlay --example brain_mockups -- /root/brain_mockups
//!
//! Requires the `magick` (or `convert`) CLI to turn the intermediate PPMs into
//! PNGs; both are checked at startup.

#![allow(
    clippy::suboptimal_flops,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]

use std::f32::consts::TAU;
use std::io::Write;
use std::process::Command;

// ---------------------------------------------------------------------------
// Palette (mirrors the tray/overlay state accents; see ADR 0013 and
// cortex_gallery.rs). Colours are packed 0x00RR_GGBB; alpha is implied opaque
// because the mockup composites onto its own dark panel.
// ---------------------------------------------------------------------------

const PANEL_OUTER: u32 = 0x000C_0C10; // near-black desktop-ish surround
const PANEL_BG: u32 = 0x0016_161C; // dark translucent-feel panel body
const PANEL_BG_TOP: u32 = 0x001C_1C24; // subtle top highlight for depth

const ACCENT_LISTEN: u32 = 0x0038_BDF8; // cyan  — recording / listening
const ACCENT_THINK: u32 = 0x00F0_A030; // amber — thinking / prefill
const ACCENT_SPEAK: u32 = 0x0034_D399; // teal-green — speaking / decode

const EXPERT_WARM: u32 = 0x00FF_B347; // amber — warm expert (in RAM)
const EXPERT_COLD: u32 = 0x0056_8CFF; // blue  — cold expert (on disk)

// ---------------------------------------------------------------------------
// Deterministic RNG (xorshift64*) — no external deps.
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + (self.next_u64() as usize) % (hi - lo)
    }
}

// ---------------------------------------------------------------------------
// One deterministic synthetic signal set — shared by dense and MoE variants so
// they are directly comparable.
// ---------------------------------------------------------------------------

struct Signal {
    n_layers: usize,
    n_experts: usize,
    /// Per-layer activation magnitude, 0..1, generally rising with depth.
    activation: Vec<f32>,
    /// Per-token entropy, in bits (drives spark size / cascade spread).
    entropy: f32,
    /// Per-layer fired experts (sparse top-k routing).
    routing: Vec<Vec<usize>>,
    /// Per-expert residency: true = warm (in RAM), false = cold (on disk).
    warm: Vec<bool>,
    /// Synthetic mic spectrum, 32 bins 0..1, for the listening frame.
    spectrum: Vec<f32>,
}

fn build_signal() -> Signal {
    let mut rng = Rng::new(mockup_seed());
    let n_layers = 40usize;
    let n_experts = 96usize;
    let top_k = 4usize;

    let activation: Vec<f32> = (0..n_layers)
        .map(|l| {
            let depth = l as f32 / (n_layers - 1) as f32;
            let rise = 0.22 + 0.62 * depth;
            let wobble = 0.16 * (l as f32 * 0.8).sin() + 0.10 * (l as f32 * 2.3).cos();
            let noise = (rng.f32() - 0.5) * 0.14;
            (rise + wobble + noise).clamp(0.05, 1.0)
        })
        .collect();

    let entropy = 2.7; // bits — a moderately uncertain token

    let warm: Vec<bool> = (0..n_experts).map(|_| rng.f32() < 0.42).collect();

    let routing: Vec<Vec<usize>> = (0..n_layers)
        .map(|_| {
            let mut fired = Vec::with_capacity(top_k);
            while fired.len() < top_k {
                let e = rng.range(0, n_experts);
                if !fired.contains(&e) {
                    fired.push(e);
                }
            }
            fired
        })
        .collect();

    // Voice-ish spectrum: a couple of formant bumps plus roll-off.
    let spectrum: Vec<f32> = (0..32)
        .map(|b| {
            let f = b as f32;
            let formant1 = (-((f - 4.0).powi(2)) / 10.0).exp();
            let formant2 = (-((f - 11.0).powi(2)) / 26.0).exp() * 0.7;
            let air = (-((f - 22.0).powi(2)) / 90.0).exp() * 0.35;
            let jitter = 0.12 * (f * 1.7).sin().abs();
            ((formant1 + formant2 + air) * 0.85 + jitter).clamp(0.0, 1.0)
        })
        .collect();

    Signal { n_layers, n_experts, activation, entropy, routing, warm, spectrum }
}

// Fixed seed so every render is byte-for-byte reproducible.
fn mockup_seed() -> u64 {
    0x00B7_A11E_5EED_1234
}

// ---------------------------------------------------------------------------
// Framebuffer + additive-glow primitives (self-contained).
// The buffer is opaque 0x00RR_GGBB accumulation; alpha is unused.
// ---------------------------------------------------------------------------

struct Canvas {
    buf: Vec<u32>,
    w: i32,
    h: i32,
}

impl Canvas {
    fn new(w: u32, h: u32, fill: u32) -> Self {
        Self { buf: vec![fill; (w * h) as usize], w: w as i32, h: h as i32 }
    }

    #[inline]
    fn get(&self, x: i32, y: i32) -> u32 {
        self.buf[(y * self.w + x) as usize]
    }

    #[inline]
    fn set(&mut self, x: i32, y: i32, c: u32) {
        self.buf[(y * self.w + x) as usize] = c;
    }

    /// Alpha-blend `color` (0x00RRGGBB) at coverage `a` in 0..1.
    fn blend_px(&mut self, x: i32, y: i32, color: u32, a: f32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h || a <= 0.0 {
            return;
        }
        let a = a.clamp(0.0, 1.0);
        let bg = self.get(x, y);
        let (br, bgc, bb) = split(bg);
        let (fr, fg, fb) = split(color);
        let r = fr * a + br * (1.0 - a);
        let g = fg * a + bgc * (1.0 - a);
        let b = fb * a + bb * (1.0 - a);
        self.set(x, y, pack(r, g, b));
    }

    /// Additive-blend (bloom): saturating add of `color * intensity`.
    fn add_px(&mut self, x: i32, y: i32, color: u32, intensity: f32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h || intensity <= 0.0 {
            return;
        }
        let bg = self.get(x, y);
        let (br, bgc, bb) = split(bg);
        let (fr, fg, fb) = split(color);
        let r = (br + fr * intensity).min(255.0);
        let g = (bgc + fg * intensity).min(255.0);
        let b = (bb + fb * intensity).min(255.0);
        self.set(x, y, pack(r, g, b));
    }

    /// Soft additive dot with quadratic falloff — the glow workhorse.
    fn glow_dot(&mut self, cx: f32, cy: f32, radius: f32, color: u32, intensity: f32) {
        let r = radius.max(0.5);
        let x0 = (cx - r).floor() as i32;
        let x1 = (cx + r).ceil() as i32;
        let y0 = (cy - r).floor() as i32;
        let y1 = (cy + r).ceil() as i32;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let d = dx.hypot(dy) / r;
                if d >= 1.0 {
                    continue;
                }
                let fall = 1.0 - d;
                self.add_px(x, y, color, intensity * fall * fall);
            }
        }
    }

    /// Additive glow along a segment (samples soft dots).
    fn glow_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, radius: f32, color: u32, i: f32) {
        let len = (x1 - x0).hypot(y1 - y0);
        let steps = (len.max(1.0)).ceil() as i32;
        for s in 0..=steps {
            let t = s as f32 / steps as f32;
            self.glow_dot(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t, radius, color, i);
        }
    }

    /// Additive-glow arrow: a shaft from tail->head plus two arrowhead barbs.
    /// `(cx, cy)` is the arrow centre; `angle` in radians (screen coords: +x
    /// right, +y down); `len` is the tip-to-tip shaft length.
    fn arrow(&mut self, cx: f32, cy: f32, angle: f32, len: f32, radius: f32, color: u32, i: f32) {
        let (s, co) = angle.sin_cos();
        let hx = cx + co * len * 0.5;
        let hy = cy + s * len * 0.5;
        let tx = cx - co * len * 0.5;
        let ty = cy - s * len * 0.5;
        self.glow_line(tx, ty, hx, hy, radius, color, i);
        // Two barbs sweeping back from the head at +/- spread.
        let barb = (len * 0.42).clamp(2.2, 6.5);
        let spread = 0.62;
        let a1 = angle + std::f32::consts::PI - spread;
        let a2 = angle + std::f32::consts::PI + spread;
        self.glow_line(hx, hy, hx + a1.cos() * barb, hy + a1.sin() * barb, radius, color, i);
        self.glow_line(hx, hy, hx + a2.cos() * barb, hy + a2.sin() * barb, radius, color, i);
    }

    /// Solid (alpha) filled rect.
    fn fill_rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: u32, a: f32) {
        for y in y0.floor() as i32..y1.ceil() as i32 {
            for x in x0.floor() as i32..x1.ceil() as i32 {
                self.blend_px(x, y, color, a);
            }
        }
    }
}

#[inline]
fn split(c: u32) -> (f32, f32, f32) {
    (((c >> 16) & 0xFF) as f32, ((c >> 8) & 0xFF) as f32, (c & 0xFF) as f32)
}
#[inline]
fn pack(r: f32, g: f32, b: f32) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}
#[inline]
fn lerp_color(a: u32, b: u32, t: f32) -> u32 {
    let (ar, ag, ab) = split(a);
    let (br, bg, bb) = split(b);
    pack(ar + (br - ar) * t, ag + (bg - ag) * t, ab + (bb - ab) * t)
}
/// Smoothstep on 0..1.
#[inline]
fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ---------------------------------------------------------------------------
// Panel chrome shared by every concept: dark body + left state-accent bar.
// Returns the inner content rect (x0, y0, x1, y1) to the right of the accent.
// ---------------------------------------------------------------------------

fn draw_panel(c: &mut Canvas, accent: u32) -> (f32, f32, f32, f32) {
    let w = c.w as f32;
    let h = c.h as f32;
    // Panel body with a faint vertical gradient for depth.
    for y in 0..c.h {
        let t = y as f32 / h;
        let row = lerp_color(PANEL_BG_TOP, PANEL_BG, smooth(t));
        for x in 0..c.w {
            c.set(x, y, row);
        }
    }
    // Rounded-ish inset border glow.
    let m = (h * 0.06).max(3.0);
    // Left state-accent bar (bright, the product's signature strip).
    let bar_x0 = m;
    let bar_x1 = m + (h * 0.09).max(5.0);
    c.fill_rect(bar_x0, m, bar_x1, h - m, accent, 0.95);
    // Bloom off the accent bar so it reads as emissive.
    for y in (m as i32)..(h - m) as i32 {
        c.glow_dot((bar_x0 + bar_x1) * 0.5, y as f32 + 0.5, (bar_x1 - bar_x0) * 1.6, accent, 0.5);
    }
    let inner_x0 = bar_x1 + (w * 0.02).max(10.0);
    let inner_x1 = w - m - (w * 0.01).max(6.0);
    let inner_y0 = m + 2.0;
    let inner_y1 = h - m - 2.0;
    (inner_x0, inner_y0, inner_x1, inner_y1)
}

// ---------------------------------------------------------------------------
// Phase description passed to each concept draw fn.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Phase {
    /// Sound-reactive; no model activity.
    Listening,
    /// Prefill sweep, `progress` 0..1 through the pass.
    Prefill { progress: f32 },
    /// Decode mid-word: `sweep` 0..1 spark position, `moe` selects variant.
    Decode { sweep: f32, moe: bool },
}

fn accent_for(phase: Phase) -> u32 {
    match phase {
        Phase::Listening => ACCENT_LISTEN,
        Phase::Prefill { .. } => ACCENT_THINK,
        Phase::Decode { .. } => ACCENT_SPEAK,
    }
}

/// Interpolate the 32-bin spectrum to `n` samples.
fn spectrum_at(sig: &Signal, n: usize, i: usize) -> f32 {
    let f = i as f32 / (n - 1).max(1) as f32 * (sig.spectrum.len() - 1) as f32;
    let a = f.floor() as usize;
    let b = (a + 1).min(sig.spectrum.len() - 1);
    let t = f - a as f32;
    sig.spectrum[a] * (1.0 - t) + sig.spectrum[b] * t
}

// ===========================================================================
// CONCEPT 1 — LAYER BARS (music-equalizer)
// ===========================================================================

fn draw_layer_bars(c: &mut Canvas, sig: &Signal, phase: Phase) {
    let accent = accent_for(phase);
    let (x0, y0, x1, y1) = draw_panel(c, accent);
    let n = sig.n_layers;
    let area_w = x1 - x0;
    let area_h = y1 - y0;
    let slot = area_w / n as f32;
    let bar_w = (slot * 0.62).max(2.0);

    for l in 0..n {
        let cx = x0 + slot * (l as f32 + 0.5);
        // Per-bar magnitude by phase.
        let mag = match phase {
            Phase::Listening => spectrum_at(sig, n, l).powf(0.8),
            Phase::Prefill { progress } => {
                // A single wave fills ALL bars at once: broad envelope that has
                // already swept most of the strip, every bar lifted.
                let base = 0.55 + 0.4 * sig.activation[l];
                let wave = 0.5 + 0.5 * ((l as f32 / n as f32) * TAU - progress * TAU).sin();
                (base * (0.7 + 0.3 * wave)).min(1.0)
            }
            Phase::Decode { sweep, .. } => {
                // Baseline from activation, plus a bright spark envelope.
                let base = 0.30 + 0.55 * sig.activation[l];
                let pos = l as f32 / (n - 1) as f32;
                let d = (pos - sweep).abs();
                let spark = (-(d * d) / 0.004).exp(); // narrow moving peak
                (base * 0.7 + spark * 0.9).min(1.0)
            }
        };
        let bh = (mag * area_h).clamp(2.0, area_h);
        let top = y1 - bh;

        let moe = matches!(phase, Phase::Decode { moe: true, .. });
        if moe {
            // Stack of expert cells; only routed experts lit, warm/cold tint.
            let cells = 12usize;
            let cell_h = area_h / cells as f32;
            let fired = &sig.routing[l];
            for ci in 0..cells {
                let cy1 = y1 - cell_h * ci as f32;
                let cy0 = cy1 - cell_h + 1.0;
                // Map cell index -> a candidate expert for this layer.
                let lit = ci < fired.len() + 2 && (ci % 2 == 0 || ci < fired.len());
                if ci < fired.len() {
                    let e = fired[ci];
                    let col = if sig.warm[e] { EXPERT_WARM } else { EXPERT_COLD };
                    let inten = 0.55 + 0.45 * sig.activation[l];
                    c.fill_rect(cx - bar_w * 0.5, cy0, cx + bar_w * 0.5, cy1, col, 0.9);
                    c.glow_dot(cx, (cy0 + cy1) * 0.5, bar_w * 0.9, col, inten);
                } else if lit {
                    // dim inactive cell outline for structure
                    c.fill_rect(cx - bar_w * 0.5, cy0, cx + bar_w * 0.5, cy1, 0x0030_3644, 0.35);
                }
            }
        } else {
            // Dense: solid bar with a bright hot cap + bloom.
            let body = lerp_color(0x0020_5A50, accent, smooth(mag));
            c.fill_rect(cx - bar_w * 0.5, top, cx + bar_w * 0.5, y1, body, 0.85);
            // Hot cap.
            let cap = lerp_color(accent, 0x00FF_FFFF, 0.55 * mag);
            c.glow_dot(cx, top, bar_w * 1.1, cap, 0.7 + 0.6 * mag);
            c.fill_rect(cx - bar_w * 0.5, top, cx + bar_w * 0.5, top + 2.5, cap, 0.95);
        }
    }

    // Extra emphasis pass for the decode spark (a travelling comet head).
    if let Phase::Decode { sweep, moe } = phase {
        let cx = x0 + area_w * sweep;
        let _ = moe;
        let head = 0x00FF_FFFF;
        c.glow_dot(cx, y0 + area_h * 0.2, area_h * 0.34, head, 1.1);
        c.glow_line(cx, y0, cx, y1, 2.0, accent, 0.5);
    }
    // Prefill: a bright full-width crossbar to say "all at once".
    if let Phase::Prefill { progress } = phase {
        let yline = y0 + area_h * (0.25 + 0.1 * (progress * TAU).sin());
        c.glow_line(x0, yline, x1, yline, 2.2, 0x00FF_FFFF, 0.35);
    }
}

// ===========================================================================
// CONCEPT 2 — NEURAL CURRENT (flow field)
// ===========================================================================

fn draw_neural_current(c: &mut Canvas, sig: &Signal, phase: Phase) {
    let accent = accent_for(phase);
    let (x0, y0, x1, y1) = draw_panel(c, accent);
    let n = sig.n_layers;
    let area_w = x1 - x0;
    let area_h = y1 - y0;
    let midy = (y0 + y1) * 0.5;

    // The layer spine (dim baseline).
    c.glow_line(x0, midy, x1, midy, 1.4, lerp_color(accent, 0x0020_2830, 0.4), 0.25);

    let moe = matches!(phase, Phase::Decode { moe: true, .. });

    for l in 0..n {
        let cx = x0 + area_w * (l as f32 + 0.5) / n as f32;
        let act = sig.activation[l];
        let (mag, bright) = match phase {
            Phase::Listening => {
                let s = spectrum_at(sig, n, l);
                (s, 0.5 + 0.7 * s)
            }
            Phase::Prefill { progress } => {
                // Broad tide: a wide soft band already crossing, all filaments up.
                let tide = (-(((l as f32 / n as f32) - progress).powi(2)) / 0.10).exp();
                (0.6 + 0.4 * act, 0.5 + 0.9 * tide)
            }
            Phase::Decode { sweep, .. } => {
                let pos = l as f32 / (n - 1) as f32;
                let pulse = (-((pos - sweep).powi(2)) / 0.010).exp();
                (0.35 + 0.5 * act + 0.4 * pulse, 0.35 + 0.9 * pulse + 0.3 * act)
            }
        };

        if moe {
            // Filaments clump into expert cells: a small vertical cluster of
            // dots, warm/cold tinted, near the layer position.
            let fired = &sig.routing[l];
            for (k, &e) in fired.iter().enumerate() {
                let col = if sig.warm[e] { EXPERT_WARM } else { EXPERT_COLD };
                let off = (k as f32 - (fired.len() as f32 - 1.0) * 0.5) * (area_h * 0.16);
                let cy = midy + off;
                c.glow_dot(cx, cy, 3.0 + 2.0 * act, col, 0.7 + 0.5 * act);
            }
            // A short connecting filament through the cluster.
            c.glow_line(cx, midy - area_h * 0.22, cx, midy + area_h * 0.22, 1.2, accent, 0.18);
        } else {
            // Oriented filament: length ~ magnitude, angle tilts toward the
            // next-firing (higher-activation) neighbour.
            let next = if l + 1 < n { sig.activation[l + 1] } else { act };
            let angle = (next - act) * 1.6; // radians-ish tilt
            let len = (mag * area_h * 0.7).clamp(3.0, area_h * 0.9);
            let dx = angle.sin() * len * 0.5;
            let dy = angle.cos() * len * 0.5;
            let col = lerp_color(accent, 0x00FF_FFFF, 0.4 * mag);
            c.glow_line(cx - dx, midy - dy, cx + dx, midy + dy, 1.7, col, bright);
            // Bright node at the leading tip.
            c.glow_dot(cx + dx, midy + dy, 2.6, 0x00FF_FFFF, 0.5 * bright);
        }
    }

    if let Phase::Decode { sweep, moe } = phase {
        // A comet pulse travelling along the spine.
        let cx = x0 + area_w * sweep;
        c.glow_dot(cx, midy, area_h * 0.4, 0x00FF_FFFF, if moe { 0.6 } else { 1.1 });
        c.glow_dot(cx, midy, area_h * 0.7, accent, 0.5);
    }
    if let Phase::Prefill { progress } = phase {
        // The broad tide as a soft moving vertical band.
        let bx = x0 + area_w * progress;
        for dy in -(area_h as i32 / 2)..(area_h as i32 / 2) {
            c.glow_dot(bx, midy + dy as f32, area_w * 0.05, accent, 0.10);
        }
    }
}

// ===========================================================================
// CONCEPT 2b — NEURAL CURRENT v2 (proper VECTOR / FLOW FIELD of arrows)
// A dense grid of small arrows whose orientation rotates smoothly across the
// panel (a curl / flow-noise field), driven by the synthetic signal set.
// Rendered ONLY at the real overlay strip size (~810x96).
// ===========================================================================

const ACCENT_REC: u32 = 0x00E8_3A3A; // red — recording / listening (reference)

/// Deterministic per-cell hash in 0..1 (no state; for unit-level variation).
fn hash2(a: usize, b: usize) -> f32 {
    let mut h = (a as u64).wrapping_mul(0x9E37_79B1) ^ (b as u64).wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545_F491_4F6C_DD1D);
    h ^= h >> 13;
    (h & 0x00FF_FFFF) as f32 / (0x0100_0000_u32 as f32)
}

/// Smooth low-frequency curl-noise angle field (radians). Neighbouring cells
/// differ only slightly, so the arrows read as a flowing swirl rather than a
/// grid of dashes. Biased up-right with several octaves of gentle rotation.
fn curl_flow(nx: f32, ny: f32) -> f32 {
    // Dominant low-frequency sweep: a grand, coherent undulation across the
    // width, gently tilted by row so the vertical bands curve into a vortex
    // (like the reference). High spatial coherence = reads as flow, not noise.
    let sweep = (nx * 9.0 + ny * 1.8).sin() * 1.35;
    // A soft secondary term for organic curl without breaking smoothness.
    let secondary = (nx * 3.0 - ny * 1.6 + 0.4).sin() * 0.25;
    -0.2 + sweep + secondary
}

/// Luminosity/saturation ramp: t=0 -> a dim tint of `accent`, t=1 -> the
/// bright `hi` colour. Ramping toward a *coloured* highlight (not pure white)
/// keeps arrows saturated the way the reference does, instead of blooming out
/// to white where activation is high.
fn ramp(accent: u32, hi: u32, t: f32) -> u32 {
    let dim = lerp_color(0x000B_0A0E, accent, 0.8);
    lerp_color(dim, hi, t.clamp(0.0, 1.0).powf(0.9))
}

fn draw_neural_current_v2(c: &mut Canvas, sig: &Signal, phase: Phase) {
    let accent = match phase {
        Phase::Listening => ACCENT_REC,
        Phase::Prefill { .. } => ACCENT_THINK,
        Phase::Decode { .. } => ACCENT_SPEAK,
    };
    let (x0, y0, x1, y1) = draw_panel(c, accent);
    let area_w = x1 - x0;
    let area_h = y1 - y0;
    let n = sig.n_layers;

    // Grid: many columns (depth) x a few rows (units). At 96px tall this lands
    // ~4 rows; columns track the ~40 transformer layers one-to-one.
    let ncols = 40usize;
    let nrows = 5usize;
    let cell_w = area_w / ncols as f32;
    let cell_h = area_h / nrows as f32;
    let base_len = (cell_w.min(cell_h) * 0.82).min(14.0);
    let radius = 0.82;

    // Bright highlight colour per phase — a warm/coloured tint, never pure white
    // (white is reserved for the travelling focal pulse in decode).
    let hi = match phase {
        Phase::Listening => 0x00FF_B070, // orange head, like the reference
        Phase::Prefill { .. } => 0x00FF_E2A6, // warm amber
        Phase::Decode { .. } => 0x00E8_FFF4, // near-white teal (pops at pulse)
    };

    let moe = matches!(phase, Phase::Decode { moe: true, .. });

    for row in 0..nrows {
        for col in 0..ncols {
            let layer = (col * n / ncols).min(n - 1);
            let nx = col as f32 / (ncols - 1) as f32;
            let ny = row as f32 / (nrows - 1).max(1) as f32;
            let cx = x0 + cell_w * (col as f32 + 0.5);
            let cy = y0 + cell_h * (row as f32 + 0.5);
            let act = sig.activation[layer];
            // Modest per-unit length variance keeps the field legible (a big
            // random spread reads as noise, not flow — the reference is fairly
            // uniform). Length still maps to activation magnitude below.
            let unit = 0.74 + 0.26 * hash2(layer, row);
            let grad = if layer + 1 < n { sig.activation[layer + 1] - act } else { 0.0 };
            // Keep the per-layer signal a *subtle* modulation of the smooth
            // curl — a large coefficient here injects random jitter that reads
            // as noise and destroys the flow coherence.
            let base_angle = curl_flow(nx, ny) + grad * 0.25;

            if moe {
                // Expert regions: map each row to one of the layer's fired
                // experts; tint warm (amber, in RAM) / cold (blue, on disk).
                let fired = &sig.routing[layer];
                let e = fired[row % fired.len()];
                let warm = sig.warm[e];
                let base_col = if warm { EXPERT_WARM } else { EXPERT_COLD };
                // Sparse firing: only a minority of cells light up strongly.
                let roll = hash2(layer * 7 + row, e * 3 + 1);
                let pos = layer as f32 / (n - 1) as f32;
                let sweep = if let Phase::Decode { sweep, .. } = phase { sweep } else { 0.55 };
                let pulse = (-((pos - sweep).powi(2)) / 0.010).exp();
                let active = roll > 0.70 || pulse > 0.5;
                let (mag, bright, angle) = if active {
                    let b = 0.55 + 0.4 * act + 0.7 * pulse;
                    let a = base_angle * (1.0 - pulse) + 0.0 * pulse; // align right at pulse
                    ((0.62 + 0.38 * act + 0.4 * pulse) * unit, b, a)
                } else {
                    (0.20 * unit, 0.14, base_angle)
                };
                let len = (base_len * mag).clamp(base_len * 0.3, base_len);
                let warm_hi = 0x00FF_D08A; // bright amber head
                let cold_hi = 0x00BC_D6FF; // bright blue head
                let hi_c = if warm { warm_hi } else { cold_hi };
                let col_c = ramp(base_col, hi_c, (bright - 0.15).max(0.0));
                c.arrow(cx, cy, angle, len, radius, col_c, 0.4 + 0.65 * bright.min(1.2));
                if active && bright > 0.9 {
                    let hx = cx + angle.cos() * len * 0.5;
                    let hy = cy + angle.sin() * len * 0.5;
                    c.glow_dot(hx, hy, 2.1, hi_c, 0.45 * bright);
                }
                continue;
            }

            let (mag, bright, angle) = match phase {
                Phase::Listening => {
                    let s = spectrum_at(sig, n, layer);
                    let ripple = 0.5 + 0.5 * (nx * 9.0 - 1.1).sin();
                    let m = (0.5 + 0.55 * s) * unit;
                    let b = 0.45 + 0.75 * s * ripple + 0.2 * act;
                    let a = base_angle + 0.16 * (ny * TAU + nx * 3.0).sin();
                    (m, b, a)
                }
                Phase::Prefill { progress } => {
                    // Single wave energising & COMBING the whole field: arrows
                    // align to a common direction, a bright band sweeps across.
                    let wave = (-((nx - progress).powi(2)) / 0.13).exp();
                    let target = -0.5;
                    let align = 0.74;
                    let a = base_angle * (1.0 - align) + target * align;
                    let m = (0.62 + 0.4 * act) * (0.82 + 0.55 * wave) * unit;
                    let b = 0.52 + 0.85 * wave + 0.28 * act;
                    (m, b, a)
                }
                Phase::Decode { sweep, .. } => {
                    // Swirl preserved; a bright focal pulse at the current layer
                    // with nearby arrows lengthening/aligning right (travel).
                    let pos = layer as f32 / (n - 1) as f32;
                    let d = pos - sweep;
                    let pulse = (-(d * d) / 0.006).exp();
                    let m = (0.36 + 0.4 * act + 0.55 * pulse) * unit;
                    let b = 0.34 + 0.28 * act + 1.0 * pulse;
                    let a = base_angle * (1.0 - pulse) + 0.0 * pulse;
                    (m, b, a)
                }
            };

            let len = (base_len * mag).clamp(base_len * 0.42, base_len * 1.05);
            let col_c = ramp(accent, hi, bright);
            c.arrow(cx, cy, angle, len, radius, col_c, 0.4 + 0.65 * bright.min(1.2));
            if bright > 0.92 {
                let hx = cx + angle.cos() * len * 0.5;
                let hy = cy + angle.sin() * len * 0.5;
                c.glow_dot(hx, hy, 2.4, hi, 0.5 * (bright - 0.5));
            }
        }
    }

    // Focal bloom for decode: a soft travelling halo over the pulse column.
    if let Phase::Decode { sweep, moe } = phase {
        let cx = x0 + area_w * sweep;
        let midy = (y0 + y1) * 0.5;
        c.glow_dot(cx, midy, area_h * 0.55, 0x00FF_FFFF, if moe { 0.35 } else { 0.7 });
        c.glow_dot(cx, midy, area_h * 0.9, accent, 0.4);
    }
}

// ===========================================================================
// CONCEPT 4 — ACTIVATION HEATMAP (full-panel chunky grid)
// A single visualisation that fills the ENTIRE strip with a chunky heat grid,
// like the red cells on the left of the reference screenshot but expanded to
// occupy the whole panel. Columns = transformer layers (depth), rows = sampled
// units/channels. Cell colour = activation magnitude on a heat ramp; hottest
// cells get additive glow. No arrows, no HUD text — just the grid + accent bar.
// Rendered ONLY at the real overlay strip size (~810x96).
// ===========================================================================

/// Multi-stop heat ramp: t in 0..1 walks near-black -> deep -> mid -> hot.
/// `stops` are colour anchors from cold to hot; the near-black floor is
/// prepended so t=0 reads as an almost-off cell (grid structure without light).
fn heat_ramp(deep: u32, mid: u32, hot: u32, t: f32) -> u32 {
    // Four segments: floor -> deep -> mid -> hot.
    const FLOOR: u32 = 0x0012_0A0C; // barely-lit warm charcoal
    let t = t.clamp(0.0, 1.0);
    let seg = t * 3.0;
    if seg < 1.0 {
        lerp_color(FLOOR, deep, smooth(seg))
    } else if seg < 2.0 {
        lerp_color(deep, mid, smooth(seg - 1.0))
    } else {
        lerp_color(mid, hot, smooth(seg - 2.0))
    }
}

/// Per-phase heat palette (deep, mid, hot) for the dense ramp.
fn heat_palette(phase: Phase) -> (u32, u32, u32) {
    match phase {
        // Reference red heat: maroon -> red -> hot orange.
        Phase::Listening => (0x0050_1012, 0x00C8_2A20, 0x00FF_8A4A),
        // Amber prefill bloom: golden brown -> bright amber -> pale gold.
        Phase::Prefill { .. } => (0x006E_3E06, 0x00FF_B22A, 0x00FF_F0C4),
        // Teal decode: deep teal -> green -> near-white mint.
        Phase::Decode { .. } => (0x000E_3A30, 0x0022_C088, 0x00D6_FFEC),
    }
}

fn draw_activation_heatmap(c: &mut Canvas, sig: &Signal, phase: Phase) {
    let accent = match phase {
        Phase::Listening => ACCENT_REC, // red, like the reference
        Phase::Prefill { .. } => ACCENT_THINK,
        Phase::Decode { .. } => ACCENT_SPEAK,
    };
    let (x0, y0, x1, y1) = draw_panel(c, accent);
    let area_w = x1 - x0;
    let area_h = y1 - y0;
    let n = sig.n_layers;

    // Chunky grid: ~one column per transformer layer, few rows so cells stay
    // large and legible at 96px tall (like the reference squares).
    let ncols = 38usize;
    let nrows = 6usize;
    let gap = 2.0f32; // subtle dark gap between cells
    let cell_w = area_w / ncols as f32;
    let cell_h = area_h / nrows as f32;

    let (deep, mid, hot) = heat_palette(phase);
    let moe = matches!(phase, Phase::Decode { moe: true, .. });

    for col in 0..ncols {
        let layer = (col * n / ncols).min(n - 1);
        let act = sig.activation[layer];
        let nx = col as f32 / (ncols - 1) as f32;
        // Per-column shimmer from the mic spectrum (listening) or sweep energy.
        let shimmer = spectrum_at(sig, ncols, col);
        let cxr0 = x0 + cell_w * col as f32 + gap * 0.5;
        let cxr1 = x0 + cell_w * (col as f32 + 1.0) - gap * 0.5;
        let cxc = (cxr0 + cxr1) * 0.5;

        for row in 0..nrows {
            let ny = row as f32 / (nrows - 1) as f32;
            let cyr0 = y0 + cell_h * row as f32 + gap * 0.5;
            let cyr1 = y0 + cell_h * (row as f32 + 1.0) - gap * 0.5;
            let cyc = (cyr0 + cyr1) * 0.5;

            // Deterministic per-cell variation so rows aren't identical.
            let jitter = hash2(layer * 13 + row, row * 7 + col);

            if moe {
                // Experts: sparse firing + warm/cold residency tint. Most cells
                // dark; a few bright per column. Warm = amber (RAM), cold = blue.
                let fired = &sig.routing[layer];
                // Map this row to a candidate expert; only "fire" a minority.
                let e = fired[row % fired.len()];
                let roll = hash2(layer * 17 + row * 3, e * 5 + 1);
                let pos = layer as f32 / (n - 1) as f32;
                let sweep = if let Phase::Decode { sweep, .. } = phase { sweep } else { 0.55 };
                let near = (-((pos - sweep).powi(2)) / 0.012).exp();
                let active = roll > 0.74 || (near > 0.4 && roll > 0.5);
                if active {
                    let warm = sig.warm[e];
                    let v = 0.6 + 0.4 * act;
                    let (base_c, hot_c) = if warm {
                        (0x0053_2A08u32, 0x00FF_C878u32)
                    } else {
                        (0x000E_2A5Au32, 0x00A8_D0FF)
                    };
                    let col_c = lerp_color(base_c, hot_c, (v * near.max(0.55)).clamp(0.0, 1.0));
                    c.fill_rect(cxr0, cyr0, cxr1, cyr1, col_c, 0.96);
                    let tint = if warm { EXPERT_WARM } else { EXPERT_COLD };
                    c.glow_dot(cxc, cyc, cell_w * 0.7, tint, 0.35 + 0.7 * v * near.max(0.4));
                } else {
                    // Dim empty expert slot — just enough to show grid structure.
                    let dim = if sig.warm[e] { 0x0022_1408 } else { 0x0009_1424 };
                    c.fill_rect(cxr0, cyr0, cxr1, cyr1, dim, 0.9);
                }
                continue;
            }

            // Dense value per phase (all cells carry activation).
            let v = match phase {
                Phase::Listening => {
                    // Grid reacts to the mic spectrum: columns shimmer/pulse.
                    let base = 0.20 + 0.55 * act;
                    let rowfall = 0.75 + 0.25 * (1.0 - (ny - 0.5).abs() * 2.0);
                    (base * (0.45 + 0.75 * shimmer) * rowfall + 0.12 * jitter).clamp(0.0, 1.0)
                }
                Phase::Prefill { progress } => {
                    // WHOLE grid energises at once: a bright bloom fills every
                    // cell together, gently domed toward the centre.
                    let bloom = 0.72 + 0.28 * (progress * TAU).sin().abs();
                    let dome = 1.0 - 0.22 * ((nx - 0.5).powi(2) + (ny - 0.5).powi(2));
                    (0.5 + 0.5 * act).mul_add(bloom, 0.10 * jitter) * dome
                }
                Phase::Decode { sweep, .. } => {
                    // Travelling bright COLUMN sweep left->right with a trailing
                    // glow; all rows carry a solid activation gradient.
                    let pos = layer as f32 / (n - 1) as f32;
                    let d = pos - sweep;
                    let head = (-(d * d) / 0.004).exp();
                    // Asymmetric trail: cells just passed keep some heat.
                    let trail = if d < 0.0 { (-(d * d) / 0.05).exp() * 0.5 } else { 0.0 };
                    let base = 0.22 + 0.4 * act;
                    (base + 0.85 * head + trail + 0.06 * jitter).clamp(0.0, 1.0)
                }
            }
            .clamp(0.0, 1.0);

            let col_c = heat_ramp(deep, mid, hot, v);
            c.fill_rect(cxr0, cyr0, cxr1, cyr1, col_c, 0.97);
            // Additive glow on the hottest cells for a subtle bloom.
            if v > 0.62 {
                c.glow_dot(cxc, cyc, cell_w * 0.62, hot, 0.35 * (v - 0.5));
            }
        }
    }

    // Phase-specific accents that reinforce the read.
    match phase {
        Phase::Prefill { .. } => {
            // Soft full-panel wash to say "all at once".
            let midy = (y0 + y1) * 0.5;
            c.glow_dot((x0 + x1) * 0.5, midy, area_h * 1.2, hot, 0.10);
        }
        Phase::Decode { sweep, moe } => {
            // Travelling column halo over the current layer.
            let cx = x0 + area_w * sweep;
            let midy = (y0 + y1) * 0.5;
            c.glow_dot(cx, midy, area_h * 0.6, if moe { hot } else { 0x00FF_FFFF }, 0.4);
            c.glow_line(cx, y0, cx, y1, 1.6, hot, 0.35);
        }
        Phase::Listening => {}
    }
    let _ = (sig.entropy, sig.n_experts);
}

// ===========================================================================
// CONCEPT 3 — DEEP SCAN (telemetry river)
// time scrolls right->left; y = layer depth (bottom=first, top=last).
// ===========================================================================

fn draw_deep_scan(c: &mut Canvas, sig: &Signal, phase: Phase) {
    let accent = accent_for(phase);
    let (x0, y0, x1, y1) = draw_panel(c, accent);
    let n = sig.n_layers;
    let area_w = x1 - x0;
    let area_h = y1 - y0;
    // Row (layer) -> y. bottom = layer 0, top = last layer.
    let row_y = |l: usize| -> f32 { y1 - (l as f32 + 0.5) / n as f32 * area_h };
    let moe = matches!(phase, Phase::Decode { moe: true, .. });

    // River bed: draw every layer as a dim horizontal glow line so the panel
    // reads as a filled depth field rather than empty black. Brighter rows =
    // more active layers.
    for l in 0..n {
        let a = sig.activation[l];
        let col = lerp_color(0x0018_2636, accent, 0.24 + 0.34 * a);
        c.glow_line(x0, row_y(l), x1, row_y(l), 1.3, col, 0.16 + 0.24 * a);
    }

    match phase {
        Phase::Listening => {
            // A scrolling spectral river: each time-column is the mic spectrum
            // shifted, newest at the right and fading to the left.
            let n_cols = 42usize;
            for col in 0..n_cols {
                let age = col as f32 / (n_cols - 1) as f32;
                let xpix = x1 - age * area_w;
                let fade = (1.0 - age).powf(1.1);
                for l in 0..n {
                    let sc = spectrum_at(sig, n, (l + col) % n);
                    if sc > 0.14 {
                        let cc = lerp_color(accent, 0x00FF_FFFF, 0.5 * sc);
                        c.glow_dot(xpix, row_y(l), 2.2 + 2.2 * sc, cc, (0.30 + 1.0 * sc) * fade);
                    }
                }
            }
        }
        Phase::Prefill { .. } => {
            // Prefill = a bold FULL-HEIGHT flare across every layer at once near
            // the right edge, as a wide bright band, with trailing echoes left.
            for (k, off) in [(0usize, 0.0f32), (1, 0.15), (2, 0.31)] {
                let fx = x1 - off * area_w - 6.0;
                let inten = 1.0 - k as f32 * 0.38;
                for dx in -4i32..=4 {
                    let bandfade = 1.0 - (dx.abs() as f32 / 5.0);
                    let cx = fx + dx as f32 * 2.2;
                    for l in 0..n {
                        let v = 0.35 + 0.30 * sig.activation[l];
                        let cc = lerp_color(accent, 0x00FF_FFFF, 0.55);
                        c.glow_dot(cx, row_y(l), 2.6, cc, v * inten * bandfade);
                    }
                }
            }
        }
        Phase::Decode { .. } => {
            // Decode = repeating DIAGONAL cascades that climb bottom->top and
            // scroll left. The freshest cascade is brightest at the right.
            let lean = area_w * 0.14;
            for k in 0..6 {
                let age = k as f32 / 6.0;
                let xr = x1 - age * area_w * 0.92;
                let bright = (1.0 - age).powf(1.15);
                if bright < 0.05 {
                    continue;
                }
                for l in 0..n {
                    let t = l as f32 / (n - 1) as f32;
                    let x = xr - t * lean;
                    let y = row_y(l);
                    let a = sig.activation[l];
                    if moe {
                        // Sparse lit expert cells within the cascade, warm/cold.
                        let fired = &sig.routing[l];
                        let e = fired[k % fired.len()];
                        let cc = if sig.warm[e] { EXPERT_WARM } else { EXPERT_COLD };
                        c.glow_dot(x, y, 2.8, cc, (0.35 + 0.6 * a) * bright);
                    } else {
                        // Solid glowing cascade node.
                        let cc = lerp_color(accent, 0x00FF_FFFF, 0.5 * bright);
                        c.glow_dot(x, y, 2.8, cc, (0.35 + 0.7 * a) * bright);
                    }
                }
            }
        }
    }

    // Bright leading edge marker at the right (the "now" line).
    c.glow_line(x1, y0, x1, y1, 1.8, accent, 0.4);
    let _ = (sig.entropy, sig.n_experts);
}

// ---------------------------------------------------------------------------
// Output: write PPM then shell out to magick/convert for PNG.
// ---------------------------------------------------------------------------

fn write_ppm(path: &str, c: &Canvas) {
    let mut out = Vec::with_capacity(c.buf.len() * 3 + 32);
    out.extend_from_slice(format!("P6\n{} {}\n255\n", c.w, c.h).as_bytes());
    for &px in &c.buf {
        out.push(((px >> 16) & 0xFF) as u8);
        out.push(((px >> 8) & 0xFF) as u8);
        out.push((px & 0xFF) as u8);
    }
    std::fs::File::create(path).and_then(|mut f| f.write_all(&out)).expect("write ppm");
}

fn ppm_to_png(magick: &str, ppm: &str, png: &str) {
    let status = if magick == "magick" {
        Command::new("magick").arg(ppm).arg(png).status()
    } else {
        Command::new("convert").arg(ppm).arg(png).status()
    };
    match status {
        Ok(s) if s.success() => {}
        other => panic!("PNG conversion failed for {png}: {other:?}"),
    }
    let _ = std::fs::remove_file(ppm);
}

fn detect_magick() -> &'static str {
    if Command::new("magick").arg("-version").output().map(|o| o.status.success()).unwrap_or(false)
    {
        "magick"
    } else if Command::new("convert")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        "convert"
    } else {
        panic!("neither `magick` nor `convert` found on PATH; cannot produce PNGs");
    }
}

// ---------------------------------------------------------------------------

type DrawFn = fn(&mut Canvas, &Signal, Phase);

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "/root/brain_mockups".into());
    std::fs::create_dir_all(&dir).expect("mkdir");
    let magick = detect_magick();
    let sig = build_signal();

    let concepts: [(&str, DrawFn); 3] = [
        ("layer_bars", draw_layer_bars),
        ("neural_current", draw_neural_current),
        ("deep_scan", draw_deep_scan),
    ];
    let phases: [(&str, Phase); 4] = [
        ("listening", Phase::Listening),
        ("thinking_prefill", Phase::Prefill { progress: 0.55 }),
        ("speaking_decode_dense", Phase::Decode { sweep: 0.55, moe: false }),
        ("speaking_decode_moe", Phase::Decode { sweep: 0.55, moe: true }),
    ];
    // (label, w, h, suffix)
    let sizes: [(u32, u32, &str); 2] = [(810, 96, ""), (640, 240, "_tall")];

    let mut produced: Vec<String> = Vec::new();
    for (cname, draw) in concepts {
        for (pname, phase) in phases {
            for (w, h, suffix) in sizes {
                let mut canvas = Canvas::new(w, h, PANEL_OUTER);
                draw(&mut canvas, &sig, phase);
                let base = format!("{dir}/{cname}_{pname}{suffix}");
                let ppm = format!("{base}.ppm");
                let png = format!("{base}.png");
                write_ppm(&ppm, &canvas);
                ppm_to_png(magick, &ppm, &png);
                produced.push(png);
            }
        }
    }

    // -------------------------------------------------------------------
    // Neural Current v2 — proper flow-field of arrows, at the REAL overlay
    // strip size ONLY (810x96). Rendered into a sibling `_v2` directory.
    // -------------------------------------------------------------------
    let dir_v2 = std::env::args().nth(2).unwrap_or_else(|| "/root/brain_mockups_v2".into());
    std::fs::create_dir_all(&dir_v2).expect("mkdir v2");
    let mut produced_v2: Vec<String> = Vec::new();
    for (pname, phase) in phases {
        let mut canvas = Canvas::new(810, 96, PANEL_OUTER);
        draw_neural_current_v2(&mut canvas, &sig, phase);
        let base = format!("{dir_v2}/neural_current_{pname}");
        let ppm = format!("{base}.ppm");
        let png = format!("{base}.png");
        write_ppm(&ppm, &canvas);
        ppm_to_png(magick, &ppm, &png);
        produced_v2.push(png);
    }

    // -------------------------------------------------------------------
    // Activation Heatmap — full-panel chunky heat grid, at the REAL overlay
    // strip size ONLY (810x96). Rendered into a `_v3` directory.
    // -------------------------------------------------------------------
    let dir_v3 = std::env::args().nth(3).unwrap_or_else(|| "/root/brain_mockups_v3".into());
    std::fs::create_dir_all(&dir_v3).expect("mkdir v3");
    let mut produced_v3: Vec<String> = Vec::new();
    for (pname, phase) in phases {
        let mut canvas = Canvas::new(810, 96, PANEL_OUTER);
        draw_activation_heatmap(&mut canvas, &sig, phase);
        let base = format!("{dir_v3}/heatmap_{pname}");
        let ppm = format!("{base}.ppm");
        let png = format!("{base}.png");
        write_ppm(&ppm, &canvas);
        ppm_to_png(magick, &ppm, &png);
        produced_v3.push(png);
    }

    println!("Rendered {} PNGs into {dir}", produced.len());
    for p in &produced {
        println!("  {p}");
    }
    println!("Rendered {} v2 flow-field PNGs into {dir_v2}", produced_v2.len());
    for p in &produced_v2 {
        println!("  {p}");
    }
    println!("Rendered {} v3 heatmap PNGs into {dir_v3}", produced_v3.len());
    for p in &produced_v3 {
        println!("  {p}");
    }
}
