// SPDX-License-Identifier: GPL-3.0-only
//! Brain-tap overhead benchmark — Task 1.4 of
//! `plans/2026-07-05-brain-visualization-v1.md`.
//!
//! Measures per-token decode cost on a real GGUF model in three
//! configurations, interleaved round-robin to cancel thermal drift:
//!
//! * **baseline** — no eval callback installed at all;
//! * **dormant**  — tap installed but never armed (the steady state every
//!   user pays once `[overlay].brain_capture = true`);
//! * **active**   — tap installed and governor-paced (real keyframe
//!   capture, the state during TTS playback).
//!
//! The **primary gate** is the governor's within-run overhead estimate
//! (sampled-vs-plain EMA from the *same* run — immune to the ±20 %
//! round-to-round thermal drift a throttling laptop shows), checked
//! against the < 1 % budget (`OVERHEAD_BUDGET`). The wall-clock A/B
//! medians are printed as a secondary sanity signal only. Also
//! validates that active keyframes carry a nonzero norm for **every**
//! model layer (the Task 1.3 tensor-name desk-check, on real data).
//!
//! Run (release build is mandatory — debug decode speed is meaningless):
//!
//! ```text
//! cargo run --release -p fono-core --features llama-local \
//!     --example brain_tap_bench -- tmp/models/polish/gemma-4-e2b.gguf 128 3
//! ```

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use fono_core::brain_tap::{decode_token_with_tap, BrainTap, OVERHEAD_BUDGET};
use fono_core::llama_backend::{backend, decode_threads, shared_model};
use fono_core::llama_gen::generation_sampler;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};

/// Long enough to exercise a realistic KV, short enough to prefill fast.
const PROMPT: &str = "Write a detailed, flowing description of how a bicycle works, \
     covering the frame, the wheels and spokes, the chain drivetrain, the brakes, \
     and the steering geometry, in plain continuous prose without lists:";

struct RunStats {
    per_token_ms: f64,
}

/// Prefill the prompt and decode `n_tokens` greedily, timing only the
/// decode loop. `tap = Some(..)` installs the callback; `arm = true`
/// additionally lets the governor pace real captures.
fn run(model: &LlamaModel, tap: Option<&BrainTap>, arm: bool, n_tokens: u32) -> Result<RunStats> {
    let threads = decode_threads();
    let mut params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(2048))
        .with_n_threads(threads)
        .with_n_threads_batch(threads);
    if let Some(tap) = tap {
        // SAFETY: `tap` outlives the method-local context created below.
        unsafe { tap.install(&mut params) };
    }
    let mut ctx = model.new_context(backend(), params).context("create context")?;

    let tokens = model.str_to_token(PROMPT, AddBos::Always).context("tokenize prompt")?;
    let mut batch = LlamaBatch::new(tokens.len().max(1), 1);
    let last = tokens.len() - 1;
    for (i, t) in tokens.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        batch.add(*t, i as i32, &[0], i == last).context("prefill batch.add")?;
    }
    ctx.decode(&mut batch).context("prefill decode")?;

    let mut sampler = generation_sampler();
    let mut sample_idx = i32::try_from(last).context("prompt too long")?;
    let start_pos = i32::try_from(tokens.len()).context("prompt too long")?;
    let mut single = LlamaBatch::new(1, 1);

    let started = Instant::now();
    for (step, pos) in (0..n_tokens).zip(start_pos..) {
        let token = sampler.sample(&ctx, sample_idx);
        sampler.accept(token);
        single.clear();
        single.add(token, pos, &[0], true).context("decode batch.add")?;
        sample_idx = 0;
        // No stop handling on purpose: every run decodes exactly
        // `n_tokens` so the three configurations stay comparable.
        let effective_tap = if arm { tap } else { None };
        decode_token_with_tap(&mut ctx, &mut single, effective_tap, u64::from(step))
            .context("decode loop")?;
    }
    let elapsed = started.elapsed().as_secs_f64();
    Ok(RunStats { per_token_ms: elapsed * 1000.0 / f64::from(n_tokens) })
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let model_path = PathBuf::from(
        args.next().unwrap_or_else(|| "tmp/models/polish/gemma-4-e2b.gguf".to_string()),
    );
    let n_tokens: u32 = args.next().map_or(Ok(128), |s| s.parse()).context("bad n_tokens")?;
    let rounds: u32 = args.next().map_or(Ok(3), |s| s.parse()).context("bad rounds")?;
    anyhow::ensure!(model_path.exists(), "model not found: {model_path:?}");

    println!("model:   {}", model_path.display());
    println!("tokens:  {n_tokens} per run, {rounds} interleaved rounds per config");
    println!("threads: {}", decode_threads());

    let model = shared_model(&model_path, &LlamaModelParams::default())?;
    let n_layer = model.n_layer();
    println!("layers:  {n_layer}\n");

    let tap = BrainTap::new(n_layer);

    // Warm-up: fault the weights in so round 1 isn't a page-cache benchmark.
    run(&model, None, false, 8.min(n_tokens))?;
    // Governor calibration: an unmeasured active run so the interval
    // reaches its steady state before we start timing (a fresh governor
    // begins at the aggressive base interval and pays the widening cost
    // in-run, which is start-up transient, not steady-state overhead).
    run(&model, Some(&tap), true, n_tokens)?;
    println!("governor steady-state interval after calibration: {}\n", tap.interval());

    let mut overheads_dormant = Vec::new();
    let mut overheads_active = Vec::new();
    let mut best_baseline = f64::INFINITY;
    for round in 0..rounds {
        let b = run(&model, None, false, n_tokens)?;
        let d = run(&model, Some(&tap), false, n_tokens)?;
        let a = run(&model, Some(&tap), true, n_tokens)?;
        println!(
            "round {}: baseline {:.2} ms/tok | dormant {:.2} ms/tok ({:+.2} %) | \
             active {:.2} ms/tok ({:+.2} %, interval {})",
            round + 1,
            b.per_token_ms,
            d.per_token_ms,
            (d.per_token_ms - b.per_token_ms) / b.per_token_ms * 100.0,
            a.per_token_ms,
            (a.per_token_ms - b.per_token_ms) / b.per_token_ms * 100.0,
            tap.interval()
        );
        // Within-round ratios: thermal drift moves all three configs of
        // a round together, so the ratio is far more stable than
        // cross-round absolute times on a throttling laptop.
        overheads_dormant.push((d.per_token_ms - b.per_token_ms) / b.per_token_ms);
        overheads_active.push((a.per_token_ms - b.per_token_ms) / b.per_token_ms);
        best_baseline = best_baseline.min(b.per_token_ms);
    }

    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let d_med = median(&mut overheads_dormant);
    let a_med = median(&mut overheads_active);
    println!("\nbest baseline: {best_baseline:.3} ms/tok ({:.1} tok/s)", 1000.0 / best_baseline);
    println!("median dormant overhead (wall-clock, informational): {:+.2} %", d_med * 100.0);
    println!("median active overhead  (wall-clock, informational): {:+.2} %", a_med * 100.0);

    let est = tap
        .overhead_estimate()
        .context("governor produced no overhead estimate — no sampled tokens?")?;
    println!(
        "governor estimate: plain {:.2} ms/tok | sampled {:.2} ms/tok | \
         amortized {:+.3} % at interval {}",
        est.plain_s * 1000.0,
        est.sampled_s * 1000.0,
        est.amortized * 100.0,
        tap.interval()
    );

    // Keyframe validation (Task 1.3 on real data): merged across the
    // trailing LAYER_STRIDE keyframes (each frame observes one rotating
    // residue class of layers), every layer of a dense model must have
    // produced a nonzero l_out norm, and the sampler-side stats must be
    // present on every frame.
    let frames = tap.take_frames();
    anyhow::ensure!(!frames.is_empty(), "active runs captured no keyframes");
    let mut merged = vec![0.0_f32; n_layer as usize];
    for f in frames.iter().rev().take(fono_core::brain_tap::LAYER_STRIDE as usize) {
        for (m, &n) in merged.iter_mut().zip(&f.layer_norms) {
            if n != 0.0 {
                *m = n;
            }
        }
    }
    let zero_layers = merged.iter().filter(|&&n| n == 0.0).count();
    let f = frames.last().expect("nonempty");
    println!(
        "\nkeyframes: {} captured | merged coverage: {} layers ({} zero) | last frame: \
         {} MoE layers, prob {:?}, entropy {:?} bits",
        frames.len(),
        merged.len(),
        zero_layers,
        f.experts.len(),
        f.token_prob,
        f.entropy_bits
    );
    anyhow::ensure!(
        zero_layers == 0,
        "{zero_layers}/{} layers produced no norm across a full stride rotation — \
         tensor-name matching is incomplete for this architecture",
        merged.len()
    );
    anyhow::ensure!(
        f.token_prob.is_some() && f.entropy_bits.is_some(),
        "sampler-side stats missing from keyframe"
    );

    let budget_pct = OVERHEAD_BUDGET * 100.0;
    if est.amortized <= OVERHEAD_BUDGET {
        println!(
            "\nPASS: governor-estimated amortized overhead {:.3} % <= {budget_pct} % budget",
            est.amortized * 100.0
        );
        if a_med > OVERHEAD_BUDGET * 3.0 {
            println!(
                "note: wall-clock median ({:+.2} %) disagrees strongly — check for a \
                 systemic cost the per-token EMAs cannot see (e.g. allocator churn)",
                a_med * 100.0
            );
        }
        Ok(())
    } else {
        anyhow::bail!(
            "FAIL: governor-estimated amortized overhead {:.3} % exceeds the {budget_pct} % \
             budget",
            est.amortized * 100.0
        )
    }
}
