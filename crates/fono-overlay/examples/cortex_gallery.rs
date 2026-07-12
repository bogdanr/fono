// SPDX-License-Identifier: GPL-3.0-only
//! Offline visual gallery for the Activation Heatmap renderer.
//!
//! Feeds a synthetic trace through every phase (listening, thinking /
//! prefill, dense decode, MoE decode) and dumps frames as PPM images
//! so the look can be iterated on without restarting the daemon. All
//! frames are rendered at the REAL overlay strip size (810×96).
//!
//! Run: `cargo run --release -p fono-overlay --example cortex_gallery -- /tmp/cortex_gallery`
//! then `magick <f>.ppm <f>.png` (or the harness prints the list).

#![allow(clippy::suboptimal_flops, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use fono_overlay::cortex::{draw_cortex, CortexState};
use fono_overlay::{CortexCmd, CortexExperts, CortexFrame, OverlayState};
use std::io::Write;

// Match the real live overlay panel: a wide, short 8:1 strip.
const W: u32 = 810;
const H: u32 = 96;
const PANEL_BG: u32 = 0xCC17_171B;
// Real per-state accents the app assigns via
// `fono_overlay::renderer::accent_color` — using the *actual* values
// (not hand-picked gallery colours) so this harness renders exactly
// what ships, including the Thinking/Synthesising/Speaking palette
// unification.
const ACCENT_LISTEN: u32 = 0xFFE0_5454;
const ACCENT_THINK: u32 = 0xFFF5_9E0B;
const ACCENT_SPEAK: u32 = 0xFF38_BDF8;

fn write_ppm(path: &str, buf: &[u32]) {
    let mut out = Vec::with_capacity(buf.len() * 3 + 32);
    out.extend_from_slice(format!("P6\n{W} {H}\n255\n").as_bytes());
    for &px in buf {
        // Composite the premultiplied panel over a desktop-ish dark
        // grey so the PPM approximates what the user sees.
        let a = ((px >> 24) & 0xFF) as f32 / 255.0;
        for shift in [16u32, 8, 0] {
            let c = ((px >> shift) & 0xFF) as f32;
            let bg = 40.0;
            out.push((c + bg * (1.0 - a)).min(255.0) as u8);
        }
    }
    std::fs::File::create(path).and_then(|mut f| f.write_all(&out)).expect("write ppm");
}

fn shot(c: &CortexState, accent: u32, t: f32, dir: &str, name: &str) {
    let mut buf = vec![PANEL_BG; (W * H) as usize];
    draw_cortex(&mut buf, W, H, c, 4.0, W as f32 - 4.0, 4.0, H as f32 - 4.0, accent, 1.0, t);
    write_ppm(&format!("{dir}/{name}.ppm"), &buf);
}

/// Dense keyframes: rising per-layer activation with a little wobble.
fn dense_keyframes(c: &mut CortexState, n_layer: u32) {
    for i in 0..40u64 {
        let norms: Vec<f32> = (0..n_layer)
            .map(|l| 1.0 + ((l as f32 * 0.7 + i as f32) * 0.9).sin().abs() * (1.0 + l as f32 * 0.1))
            .collect();
        c.apply(CortexCmd::Frame(CortexFrame {
            token_index: i * 4,
            layer_norms: norms,
            experts: Vec::new(),
            token_prob: Some(0.6),
            entropy_bits: Some(1.2 + 1.6 * ((i as f32 * 0.9).sin().abs())),
        }));
    }
    c.apply(CortexCmd::ReplyEnd {
        total_tokens: 160,
        gen_ms: 8_000,
        ctx_used: 900,
        ctx_capacity: 4096,
    });
    c.apply(CortexCmd::AudioTotal { secs: 12.0 });
}

/// MoE keyframes: same activation profile, but each layer also reports
/// a sparse top-k expert routing so the scene shows the expert-cell
/// look (warm/cold residency is synthesised inside the renderer).
fn moe_keyframes(c: &mut CortexState, n_layer: u32) {
    let n_experts = 96i32;
    let top_k = 4usize;
    for i in 0..40u64 {
        let norms: Vec<f32> = (0..n_layer)
            .map(|l| 1.0 + ((l as f32 * 0.7 + i as f32) * 0.9).sin().abs() * (1.0 + l as f32 * 0.1))
            .collect();
        let experts: Vec<CortexExperts> = (0..n_layer)
            .map(|l| {
                let mut ids = Vec::with_capacity(top_k);
                let mut seed = (l as u64) * 2_654_435_761 + i * 40503;
                while ids.len() < top_k {
                    seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                    let e = ((seed >> 33) as i32) % n_experts;
                    let e = e.abs();
                    if !ids.contains(&e) {
                        ids.push(e);
                    }
                }
                CortexExperts { layer: l, ids, weights: vec![0.4, 0.3, 0.2, 0.1] }
            })
            .collect();
        c.apply(CortexCmd::Frame(CortexFrame {
            token_index: i * 4,
            layer_norms: norms,
            experts,
            token_prob: Some(0.55),
            entropy_bits: Some(2.0),
        }));
    }
    c.apply(CortexCmd::ReplyEnd {
        total_tokens: 160,
        gen_ms: 6_000,
        ctx_used: 900,
        ctx_capacity: 4096,
    });
    c.apply(CortexCmd::AudioTotal { secs: 12.0 });
}

/// Advance the speaking replay until a token pulse sits near the strip
/// centre, then shoot — so the travelling hot column / MoE focal
/// hotspot lands mid-strip in the still.
fn shoot_decode(c: &mut CortexState, accent: u32, dir: &str, name: &str) {
    for _ in 0..400 {
        c.tick(&[0.0; 32]);
        std::thread::sleep(std::time::Duration::from_millis(15));
        let front = c.beads().iter().fold(f32::NEG_INFINITY, |m, b| m.max(b.x));
        if (0.44..=0.6).contains(&front) {
            shot(c, accent, 3.0, dir, name);
            return;
        }
    }
    // Fallback: shoot whatever we have so the harness never silently
    // skips a frame.
    shot(c, accent, 3.0, dir, name);
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "/tmp/cortex_gallery".into());
    std::fs::create_dir_all(&dir).expect("mkdir");

    // --- Listening: live mic spectrum drives the heatmap + level bar.
    let mut c = CortexState::default();
    c.on_state(OverlayState::Recording { db: -20 });
    for f in 0..30 {
        let bins: Vec<f32> =
            (0..32).map(|b| ((f * 5 + b * 7) as f32 * 0.31).sin().abs() * 0.85).collect();
        c.tick(&bins);
    }
    let bins: Vec<f32> = (0..32).map(|b| ((b * 7) as f32 * 0.31).sin().abs() * 0.85).collect();
    c.tick(&bins);
    shot(&c, ACCENT_LISTEN, 3.0, &dir, "1_listening");

    // --- Thinking / prefill: whole grid floods with a fill-wave.
    // t chosen so the wave crest sits mid-strip in the still.
    let mut c = CortexState::default();
    c.on_state(OverlayState::AssistantThinking);
    c.apply(CortexCmd::Prefill { n_tokens: 512 });
    for _ in 0..4 {
        c.tick(&[0.35; 32]);
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    shot(&c, ACCENT_THINK, 5.625, &dir, "2_thinking_prefill");

    // --- Thinking / decode: once real token keyframes start arriving
    // a single one-shot wipe snaps the scene over to the decode flare
    // grammar used later while speaking — distinguishable from
    // prefill via motion/shape, not a clashing colour.
    let mut c = CortexState::default();
    c.on_state(OverlayState::AssistantThinking);
    c.apply(CortexCmd::ReplyBegin { n_layer: 35 });
    c.apply(CortexCmd::Prefill { n_tokens: 512 });
    dense_keyframes(&mut c, 35);
    for _ in 0..60 {
        c.tick(&[0.0; 32]);
        std::thread::sleep(std::time::Duration::from_millis(15));
        let (latched, _snap_t) = c.decode_snap();
        let front = c.beads().iter().fold(f32::NEG_INFINITY, |m, b| m.max(b.x));
        if latched && (0.4..=0.6).contains(&front) {
            break;
        }
    }
    shot(&c, ACCENT_THINK, 6.0, &dir, "2b_thinking_decode");

    // --- Synthesising: decode has finished (every captured keyframe
    // already live-applied) but the TTS round-trip hasn't produced
    // audio yet. The grid must keep replaying the real decode trace —
    // looped/held — instead of fading to black or drifting onto
    // fabricated motion.
    c.on_state(OverlayState::AssistantSynthesising);
    for _ in 0..80 {
        c.tick(&[0.0; 32]);
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    shot(&c, ACCENT_THINK, 7.5, &dir, "2c_synthesising_hold");

    // --- Speaking (dense decode): travelling hot column.
    let mut c = CortexState::default();
    c.apply(CortexCmd::ReplyBegin { n_layer: 35 });
    dense_keyframes(&mut c, 35);
    c.on_state(OverlayState::AssistantSpeaking);
    shoot_decode(&mut c, ACCENT_SPEAK, &dir, "3_speaking_decode_dense");

    // --- Speaking (MoE decode): sparse warm/cold expert cells.
    let mut c = CortexState::default();
    c.apply(CortexCmd::ReplyBegin { n_layer: 35 });
    moe_keyframes(&mut c, 35);
    c.on_state(OverlayState::AssistantSpeaking);
    shoot_decode(&mut c, ACCENT_SPEAK, &dir, "4_speaking_decode_moe");

    // --- Speaking (cadence, no keyframes — the ollama/brain_capture
    // =false degraded path): dense look driven purely by TTS cadence.
    let mut c = CortexState::default();
    c.apply(CortexCmd::ReplyBegin { n_layer: 35 });
    c.apply(CortexCmd::AudioTotal { secs: 8.0 });
    c.on_state(OverlayState::AssistantSpeaking);
    shoot_decode(&mut c, ACCENT_SPEAK, &dir, "5_speaking_cadence_degraded");

    // --- Speaking (dense decode) with strong reply-audio modulation:
    // the real TTS spectrum (Goertzel bands) should only decorate
    // brightness on top of the decode trace, never replace it or
    // dominate the read.
    let mut c = CortexState::default();
    c.apply(CortexCmd::ReplyBegin { n_layer: 35 });
    dense_keyframes(&mut c, 35);
    for i in 0..60u32 {
        let at = i as f32 * 0.2;
        let bands: Vec<f32> =
            (0..8).map(|b| (0.5 + 0.5 * (at * 3.0 + b as f32).sin()).abs()).collect();
        let amp = (0.6 + 0.4 * (at * 2.0).sin()).abs();
        c.apply(CortexCmd::AudioBands { at_secs: at, bands, amp });
    }
    c.on_state(OverlayState::AssistantSpeaking);
    shoot_decode(&mut c, ACCENT_SPEAK, &dir, "6_speaking_audio_bands");

    println!("gallery written to {dir}");
    println!("convert with: for f in {dir}/*.ppm; do magick \"$f\" \"${{f%.ppm}}.png\"; done");
}
