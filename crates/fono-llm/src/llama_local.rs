// SPDX-License-Identifier: GPL-3.0-only
//! Local `llama-cpp-2` text-formatter backend.
//!
//! Real ggml/llama.cpp inference, opt-in via the `llama-local` cargo feature
//! because it vendors and rebuilds llama.cpp (cmake + cc).
//!
//! Heads up for callers: CPU-only inference of a 1.5B-parameter Q4_K_M model
//! on a 4-core laptop is on the order of 5–15 tok/s. A typical 100-token
//! cleanup output therefore takes 7–20 s — too slow for live dictation flow.
//! For low-tier hardware the wizard defaults to "Skip LLM cleanup" or to a
//! fast cloud provider (Cerebras / Groq); local LLM is best for users who
//! have either a GPU build of llama.cpp or are intentionally trading
//! latency for offline operation.
//
// We hold the state mutex for the whole `format()` call (and likewise
// inside `prewarm`/`ensure_loaded`) by design: llama.cpp inference can't
// safely share a context across threads, and serialising callers is the
// simplest way to get correctness with a single backing model. Mirrors
// the same trade-off `WhisperLocal` makes; silence the same clippy lint.
#![allow(clippy::significant_drop_tightening)]

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use tracing::{debug, warn};

use crate::traits::{FormatContext, TextFormatter};

/// Hard cap on tokens generated for a cleanup pass. Cleanup outputs are
/// usually shorter than the input; capping bounds runtime on slow hardware
/// to ~ MAX_NEW_TOKENS / tok_per_sec. Cloud backends use 512; we go tighter
/// because CPU inference is the bottleneck.
const MAX_NEW_TOKENS: i32 = 256;

/// Default n_ctx fallback if the caller passes 0 / a sub-512 value.
const MIN_CTX: u32 = 512;

/// Process-wide llama.cpp backend. `LlamaBackend::init()` flips global
/// state inside llama.cpp; multiple inits return BackendAlreadyInitialized.
/// We cache the handle so the second daemon hot-swap into LlamaLocal
/// doesn't try to re-bind everything.
fn backend() -> &'static LlamaBackend {
    static BACKEND: OnceLock<LlamaBackend> = OnceLock::new();
    BACKEND.get_or_init(|| {
        LlamaBackend::init().expect(
            "LlamaBackend::init() failed — another library has already \
             initialised llama.cpp in this process",
        )
    })
}

pub struct LlamaLocal {
    model_path: PathBuf,
    context_size: u32,
    threads: i32,
    state: Arc<Mutex<Option<LlamaModel>>>,
}

impl LlamaLocal {
    pub fn new(model_path: impl Into<PathBuf>, context_size: u32) -> Self {
        Self::with_threads(model_path, context_size, num_threads())
    }

    pub fn with_threads(model_path: impl Into<PathBuf>, context_size: u32, threads: i32) -> Self {
        Self {
            model_path: model_path.into(),
            context_size: context_size.max(MIN_CTX),
            threads,
            state: Arc::new(Mutex::new(None)),
        }
    }

    /// Cheap snapshot for use inside `spawn_blocking`. The actual model
    /// stays behind the shared Arc<Mutex>, not duplicated.
    fn clone_thin(&self) -> Self {
        Self {
            model_path: self.model_path.clone(),
            context_size: self.context_size,
            threads: self.threads,
            state: Arc::clone(&self.state),
        }
    }

    /// Load the GGUF model into memory if it isn't already. Idempotent.
    /// Concurrent format() calls serialise on the state mutex by design —
    /// llama.cpp inference can't safely share a context across threads.
    fn ensure_loaded(&self) -> Result<()> {
        let mut guard = self
            .state
            .lock()
            .map_err(|_| anyhow!("llama-local mutex poisoned"))?;
        if guard.is_some() {
            return Ok(());
        }
        if !self.model_path.exists() {
            return Err(anyhow!(
                "llama-local model not found at {:?}; run `fono models install <name>` \
                 or pick a cloud LLM backend with `fono use llm groq`",
                self.model_path
            ));
        }
        let t = Instant::now();
        let params = LlamaModelParams::default();
        let model = LlamaModel::load_from_file(backend(), &self.model_path, &params)
            .with_context(|| format!("loading GGUF model from {:?}", self.model_path))?;
        debug!(
            elapsed_ms = t.elapsed().as_millis() as u64,
            path = %self.model_path.display(),
            "llama-local model loaded"
        );
        *guard = Some(model);
        Ok(())
    }

    fn run_inference(&self, prompt: &str) -> Result<String> {
        let guard = self
            .state
            .lock()
            .map_err(|_| anyhow!("llama-local mutex poisoned"))?;
        let model = guard
            .as_ref()
            .ok_or_else(|| anyhow!("llama-local model not loaded"))?;

        let n_ctx = NonZeroU32::new(self.context_size).unwrap_or_else(|| {
            NonZeroU32::new(MIN_CTX).expect("MIN_CTX is non-zero by construction")
        });
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(Some(n_ctx))
            .with_n_batch(self.context_size)
            .with_n_threads(self.threads)
            .with_n_threads_batch(self.threads);
        let mut ctx = model
            .new_context(backend(), ctx_params)
            .context("create llama context")?;

        let tokens = model
            .str_to_token(prompt, AddBos::Always)
            .context("tokenize prompt")?;
        if tokens.len() as u32 + (MAX_NEW_TOKENS as u32) >= self.context_size {
            return Err(anyhow!(
                "prompt is {} tokens, leaving < {} for generation in a context of {}; \
                 raise `[llm.local].context` or shorten the input",
                tokens.len(),
                MAX_NEW_TOKENS,
                self.context_size
            ));
        }

        // Prefill: only the final token requests logits (we sample from it).
        let mut batch = LlamaBatch::new(self.context_size as usize, 1);
        let last_prefill_idx = tokens.len() as i32 - 1;
        for (i, t) in tokens.iter().enumerate() {
            batch
                .add(*t, i as i32, &[0], i as i32 == last_prefill_idx)
                .context("prefill batch.add")?;
        }
        ctx.decode(&mut batch).context("prefill decode")?;

        // Sample loop.
        let mut sampler = LlamaSampler::greedy();
        let eos = model.token_eos();
        // Qwen2.5 / SmolLM2 stop token. If the tokenizer doesn't round-trip
        // the literal we fall back to EOS only — generation still terminates
        // by MAX_NEW_TOKENS in the worst case.
        let im_end = model
            .str_to_token("<|im_end|>", AddBos::Never)
            .ok()
            .and_then(|v| v.into_iter().next());
        let mut out = String::new();
        let mut sample_idx = last_prefill_idx;
        let mut n_cur = tokens.len() as i32;
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        for _ in 0..MAX_NEW_TOKENS {
            let token = sampler.sample(&ctx, sample_idx);
            sampler.accept(token);
            if token == eos || Some(token) == im_end {
                break;
            }
            // `special = false` keeps role markers like `<|im_end|>` from
            // round-tripping into user-visible output if greedy chose them
            // (we already break above; this is belt-and-braces).
            let piece = model
                .token_to_piece(token, &mut decoder, false, None)
                .unwrap_or_default();
            out.push_str(&piece);
            batch.clear();
            batch
                .add(token, n_cur, &[0], true)
                .context("decode batch.add")?;
            n_cur += 1;
            sample_idx = 0;
            ctx.decode(&mut batch).context("decode loop")?;
        }
        Ok(out.trim().to_string())
    }
}

/// ChatML prompt template — used by Qwen2.5 and SmolLM2, the only models
/// in our LlmRegistry today. A future model with a different chat template
/// would need a per-model dispatch here.
fn build_chatml_prompt(system: &str, user: &str) -> String {
    let mut s = String::with_capacity(system.len() + user.len() + 64);
    if !system.is_empty() {
        s.push_str("<|im_start|>system\n");
        s.push_str(system);
        s.push_str("<|im_end|>\n");
    }
    s.push_str("<|im_start|>user\n");
    s.push_str(user);
    s.push_str("<|im_end|>\n<|im_start|>assistant\n");
    s
}

#[async_trait]
impl TextFormatter for LlamaLocal {
    async fn format(&self, raw: &str, ctx: &FormatContext) -> Result<String> {
        let prompt = build_chatml_prompt(&ctx.system_prompt(), raw);
        let me = self.clone_thin();
        let started = Instant::now();
        let text = tokio::task::spawn_blocking(move || -> Result<String> {
            me.ensure_loaded()?;
            me.run_inference(&prompt)
        })
        .await
        .context("llama-local join")??;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        if elapsed_ms > 5_000 {
            warn!(
                elapsed_ms,
                "llama-local cleanup took {} ms; on CPU-only hardware consider \
                 switching to a cloud provider (`fono use llm groq` / `cerebras`) \
                 or a smaller model",
                elapsed_ms
            );
        } else {
            debug!(elapsed_ms, "llama-local cleanup ok");
        }
        Ok(text)
    }

    fn name(&self) -> &'static str {
        "llama-local"
    }

    async fn prewarm(&self) -> Result<()> {
        let me = self.clone_thin();
        tokio::task::spawn_blocking(move || me.ensure_loaded())
            .await
            .context("llama-local prewarm join")?
    }
}

fn num_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| i32::try_from(n.get()).unwrap_or(4))
        .unwrap_or(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chatml_prompt_includes_both_messages() {
        let p = build_chatml_prompt("be terse", "hello world");
        assert!(p.contains("<|im_start|>system\nbe terse<|im_end|>"));
        assert!(p.contains("<|im_start|>user\nhello world<|im_end|>"));
        assert!(p.ends_with("<|im_start|>assistant\n"));
    }

    #[test]
    fn chatml_prompt_skips_empty_system() {
        let p = build_chatml_prompt("", "hi");
        assert!(!p.contains("<|im_start|>system"));
        assert!(p.contains("<|im_start|>user\nhi<|im_end|>"));
    }

    #[test]
    fn missing_model_path_errors_clearly() {
        let m = LlamaLocal::new("/this/path/does/not/exist.gguf", 1024);
        let e = m.ensure_loaded().unwrap_err().to_string();
        assert!(e.contains("not found"), "got: {e}");
    }
}
