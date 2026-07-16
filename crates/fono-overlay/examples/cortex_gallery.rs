// SPDX-License-Identifier: GPL-3.0-only
//! Offline visual gallery for the Glas Cortex LED grid.
//!
//! Feeds synthetic-but-real-shaped traces through every phase (idle,
//! listening, prefill flood, dense decode, MoE decode, speaking
//! replay) and dumps frames as PPM images so the look can be iterated
//! on without restarting the daemon. All frames render at the REAL
//! overlay strip size (810×96) and use the deterministic-dt tick, so
//! the gallery is instant and reproducible.
//!
//! Run: `cargo run --release -p fono-overlay --example cortex_gallery -- /tmp/cortex_gallery`
//! then `magick <f>.ppm <f>.png` (or the harness prints the list).

#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::suboptimal_flops)]

use fono_overlay::cortex::{draw_cortex, CortexState};
use fono_overlay::{CortexCmd, CortexExperts, CortexFrame, CortexModelKind, OverlayState};
use std::io::Write;

// Match the real live overlay panel: a wide, short 8:1 strip.
const W: u32 = 810;
const H: u32 = 96;
const PANEL_BG: u32 = 0xCC17_171B;
// Accent is deliberately unused by the new design (two fixed ramps),
// but the draw signature still takes it — pass the real Speaking one.
const ACCENT: u32 = 0xFF38_BDF8;

/// Desktop grey the dark composites approximate.
const BG_DARK: f32 = 40.0;
/// A bright desktop (light theme / white webpage behind the strip) —
/// the LED grid must still read as *off tiles on a dark device*.
const BG_BRIGHT: f32 = 225.0;

fn write_ppm_sized(path: &str, buf: &[u32], w: u32, h: u32, bg: f32) {
    let mut out = Vec::with_capacity(buf.len() * 3 + 32);
    out.extend_from_slice(format!("P6\n{w} {h}\n255\n").as_bytes());
    for &px in buf {
        // Composite the premultiplied panel over a desktop grey so
        // the PPM approximates what the user sees.
        let a = ((px >> 24) & 0xFF) as f32 / 255.0;
        for shift in [16u32, 8, 0] {
            let c = ((px >> shift) & 0xFF) as f32;
            out.push((c + bg * (1.0 - a)).min(255.0) as u8);
        }
    }
    std::fs::File::create(path).and_then(|mut f| f.write_all(&out)).expect("write ppm");
}

/// One draw, two PPMs (dark + bright desktop) so background-polarity
/// regressions are caught offline.
fn shot(c: &CortexState, dir: &str, name: &str) {
    // Cortex is a transparent-panel style: the real renderer skips the
    // opaque charcoal backing, so start from a fully transparent buffer
    // and let the composite show the desktop through unlit tiles.
    let mut buf = vec![0u32; (W * H) as usize];
    draw_cortex(&mut buf, W, H, c, 4.0, W as f32 - 4.0, 4.0, H as f32 - 4.0, ACCENT, 1.0, 0.0);
    write_ppm_sized(&format!("{dir}/{name}.ppm"), &buf, W, H, BG_DARK);
    write_ppm_sized(&format!("{dir}/{name}_bright.ppm"), &buf, W, H, BG_BRIGHT);
}

/// Advance the deterministic clock by `secs` (50 ms steps, no sleep).
fn advance(c: &mut CortexState, secs: f32) {
    let steps = (secs / 0.05).ceil() as u32;
    for _ in 0..steps {
        c.tick_dt(&[], 0.05);
    }
}

fn begin(c: &mut CortexState, n_layer: u32, kind: CortexModelKind) {
    let (total, active) =
        if kind == CortexModelKind::Moe { (Some(96), Some(8)) } else { (None, None) };
    c.apply(CortexCmd::ReplyBegin {
        n_layer,
        kind,
        n_experts_total: total,
        n_experts_active: active,
    });
}

/// Real-shaped keyframe: transformer per-layer L2 norms grow with
/// depth and barely vary token-to-token (~±1 %). `boost` scales the
/// whole frame (the BOS attention-sink token can run 10–100× hot).
fn real_frame(i: u64, n_layer: u32, boost: f32) -> CortexCmd {
    let norms: Vec<f32> = (0..n_layer)
        .map(|l| {
            let base = 5.0 + l as f32 * 3.0;
            base * boost * (1.0 + 0.01 * ((i as f32 * 0.7 + l as f32) * 1.3).sin())
        })
        .collect();
    CortexCmd::Frame(CortexFrame {
        token_index: i * 4,
        layer_norms: norms,
        experts: Vec::new(),
        token_prob: Some(0.6 + 0.3 * (i as f32 * 0.5).sin()),
        entropy_bits: Some(1.0 + 0.8 * (i as f32 * 0.4).sin().abs()),
    })
}

/// MoE keyframe: sparse top-k expert routing per (strided) layer.
fn moe_frame(i: u64, n_layer: u32) -> CortexCmd {
    let norms: Vec<f32> = (0..n_layer).map(|l| 5.0 + l as f32 * 3.0).collect();
    let experts: Vec<CortexExperts> = (0..n_layer)
        .map(|l| {
            let mut ids = Vec::with_capacity(4);
            let mut seed = u64::from(l) * 2_654_435_761 + i * 40_503;
            while ids.len() < 4 {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let e = ((seed >> 33) % 96) as i32;
                if !ids.contains(&e) {
                    ids.push(e);
                }
            }
            CortexExperts { layer: l, ids, weights: vec![0.4, 0.3, 0.2, 0.1] }
        })
        .collect();
    CortexCmd::Frame(CortexFrame {
        token_index: i * 4,
        layer_norms: norms,
        experts,
        token_prob: Some(0.55),
        entropy_bits: Some(2.0),
    })
}

/// `n` keyframes closed by `ReplyEnd`/`AudioTotal`.
fn real_reply(c: &mut CortexState, n: u64, n_layer: u32, bos_outlier: bool) {
    for i in 0..n {
        let boost = if bos_outlier && i == 0 { 20.0 } else { 1.0 };
        c.apply(real_frame(i, n_layer, boost));
    }
    c.apply(CortexCmd::ReplyEnd {
        total_tokens: n * 4,
        gen_ms: 30_000,
        ctx_used: 900,
        ctx_capacity: 4096,
    });
    c.apply(CortexCmd::AudioTotal { secs: 8.0 });
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "/tmp/cortex_gallery".into());
    std::fs::create_dir_all(&dir).expect("mkdir");

    // --- 0. Idle: slow cool breath, never dead.
    let mut c = CortexState::default();
    c.on_state(OverlayState::Hidden);
    advance(&mut c, 2.3);
    shot(&c, &dir, "0_idle_breath");

    // --- 1. Listening: live mic spectrum as a cool equalizer.
    let mut c = CortexState::default();
    c.on_state(OverlayState::Recording { db: -20 });
    for f in 0..30 {
        let bins: Vec<f32> =
            (0..32).map(|b| ((f * 5 + b * 7) as f32 * 0.31).sin().abs() * 0.85).collect();
        c.tick_dt(&bins, 0.05);
    }
    shot(&c, &dir, "1_listening");

    // --- 2. Prefill: wide cool flood mid-sweep (reading the prompt).
    let mut c = CortexState::default();
    c.on_state(OverlayState::AssistantThinking);
    c.apply(CortexCmd::Prefill { n_tokens: 512 });
    advance(&mut c, 0.30); // head ≈ mid-strip of the 0.62 s pass
    shot(&c, &dir, "2_prefill_flood");

    // --- 2b. Thinking, first decode token: warm pulse over the
    // cooling prefill wash — the read→write handoff.
    let mut c = CortexState::default();
    c.on_state(OverlayState::AssistantThinking);
    c.apply(CortexCmd::Prefill { n_tokens: 512 });
    begin(&mut c, 24, CortexModelKind::Dense);
    advance(&mut c, 0.7);
    c.apply(real_frame(0, 24, 1.0));
    advance(&mut c, 0.15);
    shot(&c, &dir, "2b_thinking_first_token");

    // --- 3. Speaking, dense replay: warm equalizer sweeps, early and
    // late in an 8 s playback of a 200-frame real-shaped reply.
    let mut c = CortexState::default();
    c.on_state(OverlayState::AssistantThinking);
    begin(&mut c, 24, CortexModelKind::Dense);
    real_reply(&mut c, 200, 24, false);
    c.on_state(OverlayState::AssistantSpeaking);
    advance(&mut c, 1.62);
    shot(&c, &dir, "3a_speaking_dense_early");
    advance(&mut c, 3.6);
    shot(&c, &dir, "3b_speaking_dense_late");

    // --- 4. Speaking, MoE replay: sparse expert lanes.
    let mut c = CortexState::default();
    c.on_state(OverlayState::AssistantThinking);
    begin(&mut c, 24, CortexModelKind::Moe);
    for i in 0..200u64 {
        c.apply(moe_frame(i, 24));
    }
    c.apply(CortexCmd::ReplyEnd {
        total_tokens: 800,
        gen_ms: 30_000,
        ctx_used: 900,
        ctx_capacity: 4096,
    });
    c.apply(CortexCmd::AudioTotal { secs: 8.0 });
    c.on_state(OverlayState::AssistantSpeaking);
    advance(&mut c, 1.62);
    shot(&c, &dir, "4_speaking_moe");

    // --- 5. Resting field: mid-reply capture gap — the panel must
    // stay alive on the dim breathing floor of last-known state.
    let mut c = CortexState::default();
    c.on_state(OverlayState::AssistantThinking);
    begin(&mut c, 24, CortexModelKind::Dense);
    c.apply(real_frame(0, 24, 1.0));
    advance(&mut c, 4.0); // long gap: pulse deposits fully decayed
    shot(&c, &dir, "5_resting_field_gap");

    // --- 6. BOS outlier: a 20× first frame must not crush contrast.
    let mut c = CortexState::default();
    c.on_state(OverlayState::AssistantThinking);
    begin(&mut c, 24, CortexModelKind::Dense);
    real_reply(&mut c, 200, 24, true);
    c.on_state(OverlayState::AssistantSpeaking);
    advance(&mut c, 3.2);
    shot(&c, &dir, "6_speaking_bos_outlier");

    // --- 6b. Traceless (cloud) turn: no real keyframes ever arrive, so
    // after the grace window the simulated-MoE fallback keeps the bar
    // alive with sparse drifting expert lanes.
    let mut c = CortexState::default();
    c.on_state(OverlayState::AssistantThinking);
    advance(&mut c, 2.0); // past SIM_GRACE — simulation running
    shot(&c, &dir, "6b_traceless_moe_sim");

    // --- 7. Fractional HiDPI scale (1.25×): pixel-aligned cells, no
    // sub-pixel seams.
    {
        const W2: u32 = 1013;
        const H2: u32 = 120;
        let mut c = CortexState::default();
        c.on_state(OverlayState::AssistantThinking);
        begin(&mut c, 24, CortexModelKind::Dense);
        real_reply(&mut c, 200, 24, false);
        c.on_state(OverlayState::AssistantSpeaking);
        advance(&mut c, 1.62);
        let mut buf = vec![PANEL_BG; (W2 * H2) as usize];
        draw_cortex(
            &mut buf,
            W2,
            H2,
            &c,
            5.0,
            W2 as f32 - 5.0,
            5.0,
            H2 as f32 - 5.0,
            ACCENT,
            1.25,
            0.0,
        );
        write_ppm_sized(&format!("{dir}/7_speaking_scale125.ppm"), &buf, W2, H2, BG_DARK);
    }

    println!("gallery written to {dir}");
    println!("convert with: for f in {dir}/*.ppm; do magick \"$f\" \"${{f%.ppm}}.png\"; done");
}
