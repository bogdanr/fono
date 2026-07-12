// SPDX-License-Identifier: GPL-3.0-only
//! Brain-trace dump — captures one real local generation's full
//! [`BrainEvent`] stream to JSON, for the Glas Cortex redesign
//! (`plans/2026-07-11-glas-cortex-restart-v1.md`, step 1).
//!
//! This is the "is the instrumented data the right shape?" tool: it
//! drives a real GGUF model exactly the way the assistant backend does
//! (reply-begin → prefill → governor-paced per-token frames →
//! reply-end), installs a process-wide event sink that records every
//! event in decode order, and writes the whole trace to a JSON file the
//! Claude design simulation can load verbatim. No synthetic data — the
//! file is precisely what the live overlay would receive.
//!
//! Run (release build is mandatory — debug decode speed is meaningless):
//!
//! ```text
//! cargo run --release -p fono-core --features llama-local \
//!     --example brain_trace_dump -- \
//!     tmp/models/polish/gemma-4-e2b.gguf 200 /tmp/cortex-trace-gemma.json
//! ```
//!
//! Args: `<model.gguf> [n_tokens=200] [out.json=/tmp/cortex-trace.json]`.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use fono_core::brain_tap::{
    decode_token_with_tap, publish_prefill, publish_reply_begin, publish_reply_end, set_event_sink,
    BrainEvent, BrainTap, LAYER_STRIDE,
};
use fono_core::llama_backend::{backend, decode_threads, shared_model};
use fono_core::llama_gen::generation_sampler;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use serde_json::{json, Value};

/// A coding-agent-flavoured prompt: this is the domain the first Cortex
/// preset targets, and it produces a realistically long, varied-entropy
/// reply (prose + a little structure) rather than a flat monologue.
const PROMPT: &str = "You are a senior Rust engineer. Explain, in flowing prose, how you would \
     design a bounded lock-free ring buffer for single-producer single-consumer use, covering \
     the memory ordering you would pick for the head and tail indices, how you avoid false \
     sharing, and how the consumer detects an empty buffer:";

/// Prefill the prompt, then greedily decode `n_tokens`, publishing the
/// full brain-event lifecycle exactly as `fono-assistant` does.
fn generate(model: &LlamaModel, tap: &BrainTap, n_tokens: u32) -> Result<()> {
    let threads = decode_threads();
    let mut params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(4096))
        .with_n_threads(threads)
        .with_n_threads_batch(threads);
    // SAFETY: `tap` outlives the method-local context created below.
    unsafe { tap.install(&mut params) };
    let mut ctx = model.new_context(backend(), params).context("create context")?;

    let tokens = model.str_to_token(PROMPT, AddBos::Always).context("tokenize prompt")?;
    let mut batch = LlamaBatch::new(tokens.len().max(1), 1);
    let last = tokens.len() - 1;
    for (i, t) in tokens.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        batch.add(*t, i as i32, &[0], i == last).context("prefill batch.add")?;
    }

    // Mirror the assistant decode path's event grammar.
    publish_reply_begin(Some(tap));
    ctx.decode(&mut batch).context("prefill decode")?;
    #[allow(clippy::cast_possible_truncation)]
    publish_prefill(Some(tap), tokens.len() as u32);

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
        decode_token_with_tap(&mut ctx, &mut single, Some(tap), u64::from(step))
            .context("decode loop")?;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let gen_ms = started.elapsed().as_millis() as u64;
    let ctx_capacity = ctx.n_ctx();
    let ctx_used = start_pos as u32 + n_tokens;
    publish_reply_end(Some(tap), u64::from(n_tokens), gen_ms, ctx_used, ctx_capacity);
    Ok(())
}

/// Serialise one recorded [`BrainEvent`] to the trace JSON shape the
/// Claude simulation loads. Field names mirror the Rust struct so the
/// design contract and the source stay legible against each other.
fn event_to_json(ev: &BrainEvent) -> Value {
    match ev {
        BrainEvent::ReplyBegin { n_layer } => json!({ "type": "reply_begin", "n_layer": n_layer }),
        BrainEvent::Prefill { n_tokens } => json!({ "type": "prefill", "n_tokens": n_tokens }),
        BrainEvent::Frame(f) => json!({
            "type": "frame",
            "token_index": f.token_index,
            "layer_norms": f.layer_norms,
            "experts": f.experts.iter().map(|e| json!({
                "layer": e.layer,
                "ids": e.ids,
                "weights": e.weights,
            })).collect::<Vec<_>>(),
            "token_prob": f.token_prob,
            "entropy_bits": f.entropy_bits,
        }),
        BrainEvent::ReplyEnd { total_tokens, gen_ms, ctx_used, ctx_capacity } => json!({
            "type": "reply_end",
            "total_tokens": total_tokens,
            "gen_ms": gen_ms,
            "ctx_used": ctx_used,
            "ctx_capacity": ctx_capacity,
        }),
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let model_path = PathBuf::from(
        args.next().unwrap_or_else(|| "tmp/models/polish/gemma-4-e2b.gguf".to_string()),
    );
    let n_tokens: u32 = args.next().map_or(Ok(200), |s| s.parse()).context("bad n_tokens")?;
    let out_path =
        PathBuf::from(args.next().unwrap_or_else(|| "/tmp/cortex-trace.json".to_string()));
    anyhow::ensure!(model_path.exists(), "model not found: {model_path:?}");

    println!("model:   {}", model_path.display());
    println!("tokens:  {n_tokens} (greedy decode)");
    println!("threads: {}", decode_threads());
    println!("out:     {}", out_path.display());

    let model = shared_model(&model_path, &LlamaModelParams::default())?;
    let n_layer = model.n_layer();
    println!("layers:  {n_layer}");

    // Record every published event in decode order.
    let log: Arc<Mutex<Vec<BrainEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_log = Arc::clone(&log);
    set_event_sink(Some(Arc::new(move |ev: BrainEvent| {
        sink_log.lock().expect("trace log mutex poisoned").push(ev);
    })));

    let tap = BrainTap::new(n_layer);
    // Warm-up so weights are faulted in before the recorded run (keeps
    // the governor from paying the page-cache cost mid-trace). Cleared
    // afterwards so only the real run lands in the log.
    generate(&model, &tap, 8.min(n_tokens))?;
    log.lock().expect("trace log mutex poisoned").clear();
    let _ = tap.take_frames();

    generate(&model, &tap, n_tokens)?;
    set_event_sink(None);

    let events = log.lock().expect("trace log mutex poisoned");
    let frame_count = events.iter().filter(|e| matches!(e, BrainEvent::Frame(_))).count();

    // Coverage over a full stride rotation (each frame observes one
    // rotating residue class of layers): how many layers ever produced a
    // norm, and whether any expert (MoE) tensors showed up at all.
    let mut ever_seen = vec![false; n_layer as usize];
    let mut moe_layers = 0usize;
    for ev in events.iter() {
        if let BrainEvent::Frame(f) = ev {
            for (slot, &n) in ever_seen.iter_mut().zip(&f.layer_norms) {
                if n != 0.0 {
                    *slot = true;
                }
            }
            moe_layers = moe_layers.max(f.experts.len());
        }
    }
    let covered = ever_seen.iter().filter(|&&s| s).count();

    let doc = json!({
        "model": model_path.file_name().and_then(|s| s.to_str()).unwrap_or_default(),
        "n_layer": n_layer,
        "layer_stride": LAYER_STRIDE,
        "kind": if moe_layers > 0 { "moe" } else { "dense" },
        "frame_count": frame_count,
        "events": events.iter().map(event_to_json).collect::<Vec<_>>(),
    });
    drop(events);
    std::fs::write(&out_path, serde_json::to_string_pretty(&doc)?)
        .with_context(|| format!("write {}", out_path.display()))?;

    println!(
        "\ncaptured {frame_count} frames | layer coverage {covered}/{n_layer} | \
         max MoE layers/frame {moe_layers} ({})",
        if moe_layers > 0 { "MoE model" } else { "dense model — experts empty by design" }
    );
    println!("wrote {}", out_path.display());
    Ok(())
}
