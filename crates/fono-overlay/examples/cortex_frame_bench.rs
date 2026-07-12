// SPDX-License-Identifier: GPL-3.0-only
//! Frame-budget microbenchmark for the Glass Cortex renderer
//! (plan `plans/2026-07-05-brain-visualization-v1.md`, Task 2.6).
//!
//! Measures `draw_cortex` wall-clock per frame in the busiest phase
//! (speaking replay: beads + sparks + ribbon + HUD live) at the
//! overlay's panel sizes, including a 2× HiDPI surface. Budget: the
//! terrain-style envelope, ~4 ms/frame at 30 fps on the Kaby Lake
//! reference machine.
//!
//! Run: `cargo run --release -p fono-overlay --example cortex_frame_bench`

// Same lint posture as the viz modules: readable bench math beats
// `mul_add` chains.
#![allow(clippy::suboptimal_flops)]

use fono_overlay::cortex::{draw_cortex, CortexState};
use fono_overlay::renderer::{draw_system_360, draw_terrain_3d};
use fono_overlay::{CortexCmd, CortexFrame, OverlayState};

fn make_busy_state(n_layer: u32) -> CortexState {
    let mut c = CortexState::default();
    c.apply(CortexCmd::ReplyBegin { n_layer });
    c.apply(CortexCmd::Prefill { n_tokens: 512 });
    // A realistic keyframe train: 40 frames over a 160-token reply.
    for i in 0..40u64 {
        let norms: Vec<f32> =
            (0..n_layer).map(|l| 1.0 + ((l as f32 * 0.7 + i as f32) * 0.9).sin().abs()).collect();
        c.apply(CortexCmd::Frame(CortexFrame {
            token_index: i * 4,
            layer_norms: norms,
            experts: Vec::new(),
            token_prob: Some(0.6),
            entropy_bits: Some(1.5 + (i as f32 * 0.4).sin()),
        }));
    }
    c.apply(CortexCmd::ReplyEnd {
        total_tokens: 160,
        gen_ms: 8_000,
        ctx_used: 900,
        ctx_capacity: 4096,
    });
    c.apply(CortexCmd::AudioTotal { secs: 12.0 });
    c.on_state(OverlayState::AssistantSpeaking);
    c
}

fn bench(label: &str, w: u32, h: u32, scale: f32) {
    bench_with(label, w, h, scale, None);
}

/// When `baseline` is `Some(style)`, draw an existing 3D style
/// instead of the cortex — the like-for-like envelope the plan's
/// frame gate references.
fn bench_with(label: &str, w: u32, h: u32, scale: f32, baseline: Option<&str>) {
    const WARMUP: usize = 30;
    const FRAMES: usize = 300;
    let mut c = make_busy_state(35);
    // A realistic FFT history for the baseline styles (heatmap /
    // terrain window depth).
    let frames: std::collections::VecDeque<Vec<f32>> = (0..64)
        .map(|f| (0..32).map(|b| ((f * 7 + b * 3) as f32 * 0.37).sin().abs() * 0.7).collect())
        .collect();
    let mut buf = vec![0xCC17_171Bu32; (w * h) as usize];
    let mut worst = 0.0f64;
    let mut total = 0.0f64;
    for frame in 0..WARMUP + FRAMES {
        // Advance the replay clock the way the renderer does (~20 fps
        // FFT push cadence), then draw.
        c.tick(&[0.4; 32]);
        buf.fill(0xCC17_171B);
        let (px0, px1) = (4.0 * scale, w as f32 - 4.0 * scale);
        let (py0, py1) = (4.0 * scale, h as f32 - 4.0 * scale);
        let t = frame as f32 / 20.0;
        let t0 = std::time::Instant::now();
        match baseline {
            None => draw_cortex(&mut buf, w, h, &c, px0, px1, py0, py1, 0xFF38_BDF8, scale, t),
            Some("terrain") => {
                draw_terrain_3d(&mut buf, w, h, &frames, px0, px1, py0, py1, 0xFF38_BDF8, scale, t);
            }
            Some(_) => {
                draw_system_360(&mut buf, w, h, &frames, px0, px1, py0, py1, 0xFF38_BDF8, scale);
            }
        }
        let dt = t0.elapsed().as_secs_f64() * 1e3;
        if frame >= WARMUP {
            total += dt;
            worst = worst.max(dt);
        }
        // Keep the replay inside the speaking window.
        if frame % 200 == 0 {
            c = make_busy_state(35);
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let mean = total / FRAMES as f64;
    println!("{label:28} {w:5}x{h:<4} scale {scale}: mean {mean:6.3} ms | worst {worst:6.3} ms");
}

fn main() {
    println!("Glass Cortex draw_cortex frame budget (speaking replay, busiest phase)\n");
    bench("panel min", 640, 80, 1.0);
    bench("panel max", 640, 240, 1.0);
    bench("panel max 2x HiDPI", 1280, 480, 2.0);
    println!("\nexisting-style baselines (same surfaces):");
    bench_with("terrain3d panel max", 640, 240, 1.0, Some("terrain"));
    bench_with("terrain3d 2x HiDPI", 1280, 480, 2.0, Some("terrain"));
    bench_with("system360 panel max", 640, 240, 1.0, Some("system360"));
    bench_with("system360 2x HiDPI", 1280, 480, 2.0, Some("system360"));
    println!("\nbudget: ~4 ms/frame (terrain-style envelope, 30 fps Kaby Lake reference)");
}
