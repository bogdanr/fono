// SPDX-License-Identifier: GPL-3.0-only
//! Local `llama-cpp-2` text-formatter backend.
//!
//! Real ggml/llama.cpp inference, opt-in via the `llama-local` cargo feature
//! because it vendors and rebuilds llama.cpp (cmake + cc).
//!
//! Heads up for callers: CPU-only inference of a 1.5B-parameter Q4_K_M model
//! on a 4-core laptop is on the order of 5–15 tok/s. A typical 100-token
//! cleanup output therefore takes 7–20 s — too slow for live dictation flow.
//! For low-tier hardware the wizard defaults to "Skip polish" or to a
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
use fono_core::brain_tap::{decode_token_with_tap, BrainTap};
use fono_core::llama_backend::{backend, shared_model};
use fono_core::llama_gen::{
    first_stop_marker, generation_sampler, is_control_token, safe_stream_end, turn_markers,
    warn_on_template_vocab_mismatch, TurnMarkers,
};
use fono_core::prompt_cache::{
    PromptStateCache, PromptStateCacheEntry, PromptStateCacheKey, PromptStateCacheLayer,
};
use fono_core::turn_trace::{
    current_instant, current_span, generation_span_args, record_cache_mutation, POLISH_LANE,
};
use futures::stream::{BoxStream, StreamExt};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::token::LlamaToken;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{debug, info, warn};

use crate::traits::{
    looks_like_clarification, looks_like_degenerate_cleanup, looks_like_translated_cleanup,
    user_prompt, FormatContext, TextFormatter,
};

/// Hard cap on tokens generated for a cleanup pass. Cleanup outputs are
/// usually shorter than the input; capping bounds runtime on slow hardware
/// to ~ MAX_NEW_TOKENS / tok_per_sec. Cloud backends use 512; we go tighter
/// because CPU inference is the bottleneck.
const MAX_NEW_TOKENS: i32 = 256;

/// Default n_ctx fallback if the caller passes 0 / a sub-512 value.
const MIN_CTX: u32 = 512;

// The sampler (repetition penalty over generated tokens only, feeding
// greedy), the Control-attribute stop predicate, and the textual
// stop-marker scan are the shared generation policy in
// `fono_core::llama_gen` — one definition for the polish + assistant
// embedded backends. The Gemma verbatim-repetition evidence (a cleaned
// sentence repeated ~6× = 23s + garbage) and the gemma-4-e2b control-token
// anomaly are documented there.

// The process-wide `LlamaBackend` singleton lives in
// `fono_core::llama_backend` so the polish (cleanup) and assistant
// (voice chat) embedded-LLM paths share ONE `LlamaBackend::init()`.
// A second init in the same process panics — see that module's docs.
// The `llama_cpp_2 → tracing` log redirector is installed there too,
// so model-load output is demoted to `warn` by the daemon's default
// `info` filter (`crates/fono/src/cli.rs`); re-enable with
// `FONO_LOG=llama-cpp-2=info`.

pub struct LlamaLocal {
    model_path: PathBuf,
    context_size: u32,
    threads: i32,
    state: Arc<Mutex<Option<Arc<LlamaModel>>>>,
    /// Bounded prompt-state (KV) cache shared across `format()` calls. Holds the
    /// pinned context-independent base prefix (`F7System`) and the per-app
    /// full-system prefixes (`F7Context`); the live cleanup path restores the
    /// deepest matching prefix and decodes only the transcript suffix.
    prompt_state_cache: Arc<Mutex<PromptStateCache>>,
    /// Glass Cortex capture (opt-in, default off) — same tap design as
    /// the assistant backend; see `fono_core::brain_tap`.
    brain_tap_enabled: bool,
    brain_tap: Arc<OnceLock<Arc<BrainTap>>>,
}

impl LlamaLocal {
    pub fn new(model_path: impl Into<PathBuf>, context_size: u32) -> Self {
        Self::with_threads(model_path, context_size, fono_core::llama_backend::decode_threads())
    }

    pub fn with_threads(model_path: impl Into<PathBuf>, context_size: u32, threads: i32) -> Self {
        Self {
            model_path: model_path.into(),
            context_size: context_size.max(MIN_CTX),
            threads,
            state: Arc::new(Mutex::new(None)),
            prompt_state_cache: Arc::new(Mutex::new(PromptStateCache::default())),
            brain_tap_enabled: false,
            brain_tap: Arc::new(OnceLock::new()),
        }
    }

    /// Opt in to Glass Cortex keyframe capture (default off; off means
    /// no callback is installed at all).
    #[must_use]
    pub fn with_brain_tap(mut self, enabled: bool) -> Self {
        self.brain_tap_enabled = enabled;
        self
    }

    /// The shared tap handle once the model has loaded with capture
    /// enabled (overlay-side consumer).
    #[must_use]
    pub fn brain_tap(&self) -> Option<Arc<BrainTap>> {
        if self.brain_tap_enabled {
            self.brain_tap.get().cloned()
        } else {
            None
        }
    }

    fn tap(&self) -> Option<&Arc<BrainTap>> {
        if self.brain_tap_enabled {
            self.brain_tap.get()
        } else {
            None
        }
    }

    /// Cheap snapshot for use inside `spawn_blocking`. The actual model
    /// stays behind the shared Arc<Mutex>, not duplicated; the prompt-state
    /// cache is likewise shared so checkpoints persist across calls.
    fn clone_thin(&self) -> Self {
        Self {
            model_path: self.model_path.clone(),
            context_size: self.context_size,
            threads: self.threads,
            state: Arc::clone(&self.state),
            prompt_state_cache: Arc::clone(&self.prompt_state_cache),
            brain_tap_enabled: self.brain_tap_enabled,
            brain_tap: Arc::clone(&self.brain_tap),
        }
    }

    /// Load the GGUF model into memory if it isn't already. Idempotent.
    /// Concurrent format() calls serialise on the state mutex by design —
    /// llama.cpp inference can't safely share a context across threads.
    fn ensure_loaded(&self) -> Result<()> {
        let mut guard = self.state.lock().map_err(|_| anyhow!("llama-local mutex poisoned"))?;
        if guard.is_some() {
            return Ok(());
        }
        if !self.model_path.exists() {
            return Err(anyhow!(
                "llama-local model not found at {:?}; run `fono models install <name>` \
                 or pick a cloud polish backend with `fono use polish groq`",
                self.model_path
            ));
        }
        let t = Instant::now();
        // Shared, process-wide weights: when polish and the assistant are both
        // the same local model (the default `gemma-4-e2b`) they resolve to the
        // same path and share ONE `LlamaModel` instead of loading two ~3.2 GB
        // copies. See `fono_core::llama_backend::shared_model`.
        let model = shared_model(&self.model_path, &LlamaModelParams::default())?;
        // Single, concise INFO line summarising what got loaded — name +
        // on-disk size (≈ resident memory once mapped) + load wall time.
        // Verbose architecture/KV/tensor dumps from llama.cpp itself are
        // routed through `init_llama_logging()` and demoted to warn by
        // the default tracing filter so they don't crowd this line.
        let elapsed_ms = t.elapsed().as_millis() as u64;
        let model_name = self.model_path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
        // Load-time tripwire: warn when the selected hand-rolled template's
        // markers do not resolve to control tokens in this vocabulary (the
        // gemma-4-e2b anomaly) or the name matches no known family.
        warn_on_template_vocab_mismatch(&model, model_name);
        let size_mb =
            std::fs::metadata(&self.model_path).map(|m| m.len() / (1024 * 1024)).unwrap_or(0);
        info!(
            "LLM ready: {model_name} ({size_mb} MB, {threads} threads, ctx={ctx}) in {elapsed_ms} ms",
            threads = self.threads,
            ctx = self.context_size,
        );
        *guard = Some(model);
        if self.brain_tap_enabled {
            let n_layer = guard.as_ref().map_or(0, |m| m.n_layer());
            let (n_expert, n_expert_used) =
                guard.as_ref().map_or((0, 0), |m| fono_core::brain_tap::model_expert_counts(m));
            let tap = self
                .brain_tap
                .get_or_init(|| Arc::new(BrainTap::new(n_layer, n_expert, n_expert_used)));
            debug!("brain tap ready: {} layers, interval {}", tap.n_layer(), tap.interval());
        }
        Ok(())
    }

    fn new_context<'model>(&self, model: &'model LlamaModel) -> Result<LlamaContext<'model>> {
        let n_ctx = NonZeroU32::new(self.context_size).unwrap_or_else(|| {
            NonZeroU32::new(MIN_CTX).expect("MIN_CTX is non-zero by construction")
        });
        let mut ctx_params = LlamaContextParams::default()
            .with_n_ctx(Some(n_ctx))
            .with_n_batch(self.context_size)
            .with_n_threads(self.threads)
            .with_n_threads_batch(self.threads);
        if let Some(tap) = self.tap() {
            // SAFETY: the tap is owned by `self` (shared across thin
            // clones via `Arc`) and therefore outlives this
            // method-local context — the `install` contract.
            unsafe { tap.install(&mut ctx_params) };
        }
        model.new_context(backend(), ctx_params).context("create llama context")
    }

    /// Prefill `tokens` into `ctx` starting at `start_pos`. Only the final
    /// token requests logits when `logits_last` is set (the caller samples from
    /// it); intermediate prefixes are decoded with `logits_last = false`.
    fn prefill_tokens(
        &self,
        ctx: &mut LlamaContext<'_>,
        tokens: &[LlamaToken],
        start_pos: i32,
        logits_last: bool,
    ) -> Result<()> {
        if tokens.is_empty() {
            return Ok(());
        }
        // Slice the per-turn (and startup base) decode onto the f7-polish lane so
        // the waterfall shows where prefill time goes, mirroring the assistant
        // `llm.prefill_decode` span. No-op cost when tracing is disabled.
        let span = current_span("polish.prefill_decode", "polish", POLISH_LANE);
        let mut batch = LlamaBatch::new(self.context_size as usize, 1);
        let last_idx = tokens.len() - 1;
        for (i, t) in tokens.iter().enumerate() {
            batch
                .add(*t, start_pos + i as i32, &[0], logits_last && i == last_idx)
                .context("prefill batch.add")?;
        }
        ctx.decode(&mut batch).context("prefill decode")?;
        // One spine-sweep pulse on the Glass Cortex per prefill batch.
        #[allow(clippy::cast_possible_truncation)]
        fono_core::brain_tap::publish_prefill(
            self.tap().map(std::convert::AsRef::as_ref),
            tokens.len() as u32,
        );
        span.finish(
            json!({ "tokens": tokens.len(), "start_pos": start_pos, "logits_last": logits_last }),
        );
        Ok(())
    }

    /// Greedy generation from an already-prefilled context. `start_pos` is the
    /// absolute position of the first generated token; `first_sample_idx` is the
    /// batch index holding the logits to sample the first token from.
    ///
    /// `on_piece` is invoked with each chunk of decoded text as soon as it is
    /// known not to be part of an incomplete stop marker — see
    /// [`safe_stream_end`]. The full accumulated (trimmed) string is still
    /// returned, so non-streaming callers pass a no-op closure and get
    /// behaviour identical to the pre-streaming implementation. Streaming
    /// callers ([`LlamaLocal::format_stream`]) forward each chunk over a
    /// channel for incremental word injection.
    fn generate_from_prefilled(
        model: &LlamaModel,
        ctx: &mut LlamaContext<'_>,
        start_pos: i32,
        first_sample_idx: i32,
        tap: Option<&BrainTap>,
        on_piece: &mut dyn FnMut(&str),
    ) -> Result<String> {
        // Shared generation policy: repetition penalty over generated tokens
        // only, feeding greedy — deterministic, but escapes the verbatim
        // self-repetition loop bare greedy falls into on cleanup. See
        // `fono_core::llama_gen`.
        let mut sampler = generation_sampler();
        // Slice the autoregressive decode loop onto the f7-polish lane so the
        // generation cost (distinct from prefill) is visible in the waterfall,
        // mirroring the assistant `llm.local_streaming_inference` span.
        let span = current_span("polish.generate", "polish", POLISH_LANE);
        // Stop on ANY control token. A single-shot cleanup must never emit a
        // turn marker, BOS/EOS, or an end-of-generation token — so we stop the
        // moment the model samples a token tagged `LlamaTokenAttr::Control`,
        // regardless of how that marker is spelled in this model's vocabulary
        // (the gemma-4-e2b `<|turn>`/`<turn|>` evidence lives in
        // `fono_core::llama_gen`).
        let mut out = String::new();
        let mut emitted_len = 0_usize;
        let mut sample_idx = first_sample_idx;
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut batch = LlamaBatch::new(1, 1);
        // Generation-throughput counters, mirroring the assistant
        // `llm.local_streaming_inference` span so the waterfall reports real
        // tok/s instead of forcing a chars÷4 estimate. `tokens` counts decoded
        // tokens (greedy = one per step), `deltas` counts `on_piece` flushes,
        // and `ttft_ms` is the latency to the first decoded token.
        let gen_started = Instant::now();
        let mut tokens_generated = 0_u32;
        let mut deltas = 0_u32;
        let mut ttft_ms = 0_u64;
        let mut stop_reason = "max_tokens";
        // Glass Cortex: announce the generation on the brain-event bus
        // (no-op unless the tap is installed AND a sink is listening).
        fono_core::brain_tap::publish_reply_begin(tap);
        for n_cur in (start_pos..).take(MAX_NEW_TOKENS as usize) {
            let token = sampler.sample(ctx, sample_idx);
            sampler.accept(token);
            if is_control_token(model, token) {
                stop_reason = "control_token";
                break;
            }
            if tokens_generated == 0 {
                ttft_ms = gen_started.elapsed().as_millis() as u64;
            }
            tokens_generated += 1;
            // `special = false` keeps any marker that slips through from
            // round-tripping into user-visible output.
            let piece = model.token_to_piece(token, &mut decoder, false, None).unwrap_or_default();
            out.push_str(&piece);
            // Belt-and-braces: if a template marker round-tripped as plain text
            // (rather than a registered control token), emit the prose before
            // it, truncate at the first occurrence, and stop.
            if let Some((idx, _marker)) = first_stop_marker(&out) {
                if idx > emitted_len {
                    on_piece(&out[emitted_len..idx]);
                    deltas += 1;
                }
                out.truncate(idx);
                emitted_len = out.len();
                stop_reason = "stop_marker";
                break;
            }
            // Stream only bytes that cannot be the start of a not-yet-complete
            // stop marker, so a marker split across several token pieces is
            // never partially injected into the cursor.
            let safe_end = safe_stream_end(&out);
            if safe_end > emitted_len {
                on_piece(&out[emitted_len..safe_end]);
                deltas += 1;
                emitted_len = safe_end;
            }
            batch.clear();
            batch.add(token, n_cur, &[0], true).context("decode batch.add")?;
            sample_idx = 0;
            // Glass Cortex keyframe capture (opt-in) — the shared helper
            // (same shape as the assistant loop) times the whole tap
            // surcharge so the governor's < 1 % backoff sees true cost.
            decode_token_with_tap(
                ctx,
                &mut batch,
                tap,
                u64::from(tokens_generated.saturating_sub(1)),
            )
            .context("decode loop")?;
        }
        // Flush any held-back tail that turned out not to be a stop marker.
        if emitted_len < out.len() {
            on_piece(&out[emitted_len..]);
            deltas += 1;
        }
        let out = out.trim().to_string();
        let gen_ms = gen_started.elapsed().as_millis() as u64;
        // Glass Cortex: close the generation on the brain-event bus with
        // throughput + KV-fill stats (no-op without a tap + sink).
        #[allow(clippy::cast_sign_loss)]
        fono_core::brain_tap::publish_reply_end(
            tap,
            u64::from(tokens_generated),
            gen_ms,
            (start_pos.max(0) as u32).saturating_add(tokens_generated),
            ctx.n_ctx(),
        );
        span.finish(generation_span_args(
            tokens_generated,
            out.chars().count(),
            deltas,
            ttft_ms,
            gen_ms,
            start_pos,
            stop_reason,
        ));
        Ok(out)
    }

    fn run_inference_with_model(
        &self,
        model: &LlamaModel,
        prompt: &str,
        on_piece: &mut dyn FnMut(&str),
    ) -> Result<String> {
        let mut ctx = self.new_context(model)?;
        let tokens = model.str_to_token(prompt, AddBos::Always).context("tokenize prompt")?;
        if tokens.len() as u32 + (MAX_NEW_TOKENS as u32) >= self.context_size {
            return Err(anyhow!(
                "prompt is {} tokens, leaving < {} for generation in a context of {}; \
                 raise `[polish.local].context` or shorten the input",
                tokens.len(),
                MAX_NEW_TOKENS,
                self.context_size
            ));
        }
        let last_prefill_idx = tokens.len() as i32 - 1;
        self.prefill_tokens(&mut ctx, &tokens, 0, true)?;
        Self::generate_from_prefilled(
            model,
            &mut ctx,
            tokens.len() as i32,
            last_prefill_idx,
            self.tap().map(Arc::as_ref),
            on_piece,
        )
    }

    /// Build a runtime+content cache key for `prefix`. Mirrors the assistant
    /// backend: the runtime identity (model path, size, mtime, ctx, threads)
    /// keys out cross-model / cross-config reuse, and the prompt + token hashes
    /// key out cross-prompt reuse.
    fn prompt_state_cache_key(
        &self,
        layer: PromptStateCacheLayer,
        prefix: &str,
        tokens: &[LlamaToken],
    ) -> Result<PromptStateCacheKey> {
        let metadata = std::fs::metadata(&self.model_path)
            .with_context(|| format!("read model metadata {}", self.model_path.display()))?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or_else(
                || "unknown".to_string(),
                |d| format!("{}.{:09}", d.as_secs(), d.subsec_nanos()),
            );
        let runtime_identity = format!(
            "llama-cpp-2:{}|model={}|size={}|modified={}|ctx={}|threads={}",
            env!("CARGO_PKG_VERSION"),
            self.model_path.display(),
            metadata.len(),
            modified,
            self.context_size,
            self.threads,
        );
        Ok(PromptStateCacheKey::new(
            layer,
            sha256_text(&runtime_identity),
            sha256_text(prefix),
            sha256_tokens(tokens),
            tokens.len(),
        ))
    }

    /// Ensure the pinned, context-independent base checkpoint exists. `base` is
    /// the partial prompt `<|im_start|>system\n{base_system}` — a genuine token
    /// prefix of every full F7 prompt — so once built it can be restored and
    /// extended for any app context. Idempotent: a cache hit is a no-op.
    fn ensure_base_prefix_cache(&self, model: &LlamaModel, base: &str) -> Result<()> {
        if base.is_empty() {
            return Ok(());
        }
        let base_tokens = model.str_to_token(base, AddBos::Always).context("tokenize base")?;
        if base_tokens.is_empty()
            || base_tokens.len() + MAX_NEW_TOKENS as usize >= self.context_size as usize
        {
            return Ok(());
        }
        let key =
            self.prompt_state_cache_key(PromptStateCacheLayer::F7System, base, &base_tokens)?;
        {
            let mut cache = self
                .prompt_state_cache
                .lock()
                .map_err(|_| anyhow!("llama-local prompt-state cache mutex poisoned"))?;
            if cache.contains(&key) {
                return Ok(());
            }
        }
        let build_span = current_span("polish.base_build", "polish", POLISH_LANE);
        let mut ctx = self.new_context(model)?;
        self.prefill_tokens(&mut ctx, &base_tokens, 0, false)?;
        let state = copy_context_state(&ctx)?;
        let report = {
            let mut cache = self
                .prompt_state_cache
                .lock()
                .map_err(|_| anyhow!("llama-local prompt-state cache mutex poisoned"))?;
            cache.insert_pinned(
                key,
                PromptStateCacheEntry::with_tokens(state, token_ids(&base_tokens)),
            )
        };
        record_cache_mutation(&report);
        build_span.finish(json!({
            "layer": PromptStateCacheLayer::F7System.as_str(),
            "base_tokens": base_tokens.len(),
        }));
        debug!(tokens = base_tokens.len(), "F7 base prefix checkpoint built and pinned");
        Ok(())
    }

    /// Fast path for [`generate_with_prefix_cache`]: emit the per-context
    /// `polish.prompt_cache_lookup` instant and, on an exact hit, restore the
    /// cached state into a fresh context and decode only the suffix. Returns
    /// `Ok(Some(text))` on a hit, `Ok(None)` to fall through to the
    /// longest-prefix miss path (including when a state restore fails).
    #[allow(clippy::too_many_arguments)]
    fn try_exact_context_hit(
        &self,
        model: &LlamaModel,
        ctx_key: &PromptStateCacheKey,
        layer_str: &'static str,
        prefix_tokens: &[LlamaToken],
        suffix_tokens: &[LlamaToken],
        full_tokens: &[LlamaToken],
        on_piece: &mut dyn FnMut(&str),
    ) -> Result<Option<String>> {
        let exact = {
            let mut cache = self
                .prompt_state_cache
                .lock()
                .map_err(|_| anyhow!("llama-local prompt-state cache mutex poisoned"))?;
            let entry = cache.get(ctx_key);
            current_instant(
                "polish.prompt_cache_lookup",
                "polish",
                POLISH_LANE,
                json!({
                    "layer": layer_str,
                    "cache_key": ctx_key.stable_id(),
                    "hit": entry.is_some(),
                    "token_count": prefix_tokens.len(),
                    "cache_entries": cache.len(),
                    "cache_bytes": cache.bytes(),
                }),
            );
            entry
        };
        let Some(entry) = exact else { return Ok(None) };
        let mut ctx = self.new_context(model)?;
        let restored = unsafe { ctx.set_state_data(&entry.state) };
        if restored == 0 {
            warn!("F7 prompt-state restore failed; falling back to full prefill");
            polish_cold_prefill(layer_str, "restore_failed");
            return Ok(None);
        }
        current_instant(
            "polish.prompt_cache_prefix_match",
            "polish",
            POLISH_LANE,
            json!({
                "matched_layer": layer_str,
                "matched_tokens": entry.token_count,
                "total_tokens": full_tokens.len(),
                "decoded_suffix_tokens": suffix_tokens.len(),
            }),
        );
        current_instant(
            "polish.prompt_cache_restored",
            "polish",
            POLISH_LANE,
            json!({
                "layer": layer_str,
                "matched_layer": layer_str,
                "matched_tokens": entry.token_count,
                "restored_bytes": entry.state.len(),
                "suffix_tokens": suffix_tokens.len(),
            }),
        );
        self.prefill_tokens(&mut ctx, suffix_tokens, prefix_tokens.len() as i32, true)?;
        let text = Self::generate_from_prefilled(
            model,
            &mut ctx,
            full_tokens.len() as i32,
            (suffix_tokens.len() - 1) as i32,
            self.tap().map(Arc::as_ref),
            on_piece,
        )?;
        debug!(layer = "f7_context", "F7 prefix cache hit (exact)");
        Ok(Some(text))
    }

    /// Cleanup with the F7 prefix cache. Restores the deepest cached prefix that
    /// is a token-prefix of this prompt (an exact per-context hit, else the
    /// pinned base via longest-prefix matching) and decodes only the remainder.
    /// Returns `Ok(None)` — having produced no output — on any incompatibility
    /// so the caller can fall back to a full prefill. The base + full-context
    /// checkpoints are populated as a side effect so later turns hit warm.
    fn generate_with_prefix_cache(
        &self,
        model: &LlamaModel,
        base: &str,
        full_prefix: &str,
        suffix: &str,
        on_piece: &mut dyn FnMut(&str),
    ) -> Result<Option<String>> {
        let layer_str = PromptStateCacheLayer::F7Context.as_str();
        if full_prefix.is_empty() || suffix.is_empty() {
            polish_cold_prefill(layer_str, "empty_prefix_or_suffix");
            return Ok(None);
        }
        let prefix_tokens =
            model.str_to_token(full_prefix, AddBos::Always).context("tokenize prefix")?;
        let full_prompt = format!("{full_prefix}{suffix}");
        let full_tokens =
            model.str_to_token(&full_prompt, AddBos::Always).context("tokenize prompt")?;
        if prefix_tokens.is_empty() || !full_tokens.starts_with(&prefix_tokens) {
            polish_cold_prefill(layer_str, "token_split_incompatible");
            return Ok(None);
        }
        let suffix_tokens = &full_tokens[prefix_tokens.len()..];
        if suffix_tokens.is_empty()
            || full_tokens.len() + MAX_NEW_TOKENS as usize >= self.context_size as usize
        {
            polish_cold_prefill(layer_str, "empty_suffix_or_too_large");
            return Ok(None);
        }

        // Build the pinned base first so longest-prefix matching has a floor.
        self.ensure_base_prefix_cache(model, base)?;

        let ctx_key = self.prompt_state_cache_key(
            PromptStateCacheLayer::F7Context,
            full_prefix,
            &prefix_tokens,
        )?;
        let runtime = ctx_key.runtime_sha256().to_string();

        // Fast path: an exact per-context hit — restore and decode just the suffix.
        if let Some(text) = self.try_exact_context_hit(
            model,
            &ctx_key,
            layer_str,
            &prefix_tokens,
            suffix_tokens,
            &full_tokens,
            on_piece,
        )? {
            return Ok(Some(text));
        }

        // Miss: build the full-context prefix, accelerating with the deepest
        // cached prefix (the pinned base) when present, then cache it pinned-by-
        // layer for next time. Finally decode the suffix.
        let mut ctx =
            self.build_miss_context(model, ctx_key, &runtime, layer_str, &prefix_tokens)?;
        self.prefill_tokens(&mut ctx, suffix_tokens, prefix_tokens.len() as i32, true)?;
        let text = Self::generate_from_prefilled(
            model,
            &mut ctx,
            full_tokens.len() as i32,
            (suffix_tokens.len() - 1) as i32,
            self.tap().map(Arc::as_ref),
            on_piece,
        )?;
        Ok(Some(text))
    }

    /// Build the full-context prefix for a per-context cache miss: restore the
    /// deepest cached prefix (the pinned base, via longest-prefix matching) when
    /// present, decode the remaining prefix tokens, then cache the result keyed
    /// by `ctx_key` for next time. Emits the `polish.prompt_cache_longest_prefix`
    /// and `polish.prompt_cache_built` instants. Returns a context primed with
    /// the full prefix, ready for the caller to decode the suffix.
    fn build_miss_context<'model>(
        &self,
        model: &'model LlamaModel,
        ctx_key: PromptStateCacheKey,
        runtime: &str,
        layer_str: &'static str,
        prefix_tokens: &[LlamaToken],
    ) -> Result<LlamaContext<'model>> {
        let mut ctx = self.new_context(model)?;
        let longest = {
            let mut cache = self
                .prompt_state_cache
                .lock()
                .map_err(|_| anyhow!("llama-local prompt-state cache mutex poisoned"))?;
            let hit = cache.find_longest_prefix(
                runtime,
                &[PromptStateCacheLayer::F7System, PromptStateCacheLayer::F7Context],
                &token_ids(prefix_tokens),
            );
            hit.map(|k| (k.layer().clone(), cache.get(&k)))
        };
        let mut start = 0_usize;
        let mut matched_layer: Option<&'static str> = None;
        let mut restored_bytes = 0_usize;
        if let Some((hit_layer, Some(entry))) = longest {
            let restored = unsafe { ctx.set_state_data(&entry.state) };
            if restored == 0 {
                // Restore failed; rebuild from scratch in a fresh context.
                ctx = self.new_context(model)?;
            } else {
                start = entry.token_count.min(prefix_tokens.len());
                matched_layer = Some(hit_layer.as_str());
                restored_bytes = entry.state.len();
                debug!(restored_tokens = start, "F7 prefix cache base hit (longest-prefix)");
            }
        }
        current_instant(
            "polish.prompt_cache_longest_prefix",
            "polish",
            POLISH_LANE,
            json!({
                "matched": matched_layer.is_some(),
                "matched_layer": matched_layer,
                "matched_tokens": start,
                "total_tokens": prefix_tokens.len(),
                "decoded_prefix_tokens": prefix_tokens.len().saturating_sub(start),
            }),
        );
        if let Some(layer) = matched_layer {
            // A longest-prefix base restore is a cache hit for scoreboard
            // purposes (mirrors the assistant `llm.prompt_cache_restored`): we
            // reuse `start` cached tokens and only decode the remaining prefix.
            current_instant(
                "polish.prompt_cache_restored",
                "polish",
                POLISH_LANE,
                json!({
                    "layer": layer_str,
                    "matched_layer": layer,
                    "matched_tokens": start,
                    "restored_bytes": restored_bytes,
                }),
            );
        } else {
            polish_cold_prefill(layer_str, "no_prefix_match");
        }
        if start < prefix_tokens.len() {
            self.prefill_tokens(&mut ctx, &prefix_tokens[start..], start as i32, false)?;
        }
        let state = copy_context_state(&ctx)?;
        {
            let mut cache = self
                .prompt_state_cache
                .lock()
                .map_err(|_| anyhow!("llama-local prompt-state cache mutex poisoned"))?;
            let report = cache.insert(
                ctx_key,
                PromptStateCacheEntry::with_tokens(state, token_ids(prefix_tokens)),
            );
            record_cache_mutation(&report);
        }
        current_instant(
            "polish.prompt_cache_built",
            "polish",
            POLISH_LANE,
            json!({
                "layer": layer_str,
                "prefix_tokens": prefix_tokens.len(),
                "restored_tokens": start,
            }),
        );
        Ok(ctx)
    }

    /// Cleanup using the prefix cache when the split reproduces the prompt
    /// byte-for-byte, falling back to a full prefill otherwise. Holds the model
    /// lock for the whole call (llama.cpp contexts are not thread-safe).
    fn run_inference_cached(
        &self,
        prompt: &str,
        base: &str,
        full_prefix: &str,
        suffix: &str,
        on_piece: &mut dyn FnMut(&str),
    ) -> Result<String> {
        let guard = self.state.lock().map_err(|_| anyhow!("llama-local mutex poisoned"))?;
        let model = guard.as_ref().ok_or_else(|| anyhow!("llama-local model not loaded"))?;
        if format!("{full_prefix}{suffix}") == prompt {
            if let Some(text) =
                self.generate_with_prefix_cache(model, base, full_prefix, suffix, on_piece)?
            {
                return Ok(text);
            }
        } else {
            polish_cold_prefill(PromptStateCacheLayer::F7Context.as_str(), "prompt_split_mismatch");
        }
        self.run_inference_with_model(model, prompt, on_piece)
    }

    /// Cleanup via a cold full prefill, bypassing the prefix cache entirely.
    /// Used as a one-shot retry when the cached (state-restore) path produced a
    /// degenerate result (e.g. the bare token `model`): a fresh context + full
    /// prefill recomputes the prompt deterministically without relying on a
    /// restored KV state, which is the path that drifts.
    fn run_inference_uncached(
        &self,
        prompt: &str,
        on_piece: &mut dyn FnMut(&str),
    ) -> Result<String> {
        let guard = self.state.lock().map_err(|_| anyhow!("llama-local mutex poisoned"))?;
        let model = guard.as_ref().ok_or_else(|| anyhow!("llama-local model not loaded"))?;
        self.run_inference_with_model(model, prompt, on_piece)
    }

    /// Speculatively build (and cache) the F7Context checkpoint for
    /// `full_prefix` so the first cleanup into this app + language context is an
    /// exact restore rather than a multi-second prefix decode on the hotkey
    /// path. Reuses [`build_miss_context`], which accelerates off the pinned
    /// base via longest-prefix matching and caches the result keyed by layer.
    /// Idempotent: an exact hit (already warm) is a no-op. Best-effort — called
    /// off the hotkey path, concurrent with capture + STT.
    fn warm_context_prefix(&self, full_prefix: &str) -> Result<()> {
        if full_prefix.is_empty() {
            return Ok(());
        }
        let guard = self.state.lock().map_err(|_| anyhow!("llama-local mutex poisoned"))?;
        let model = guard.as_ref().ok_or_else(|| anyhow!("llama-local model not loaded"))?;
        let prefix_tokens =
            model.str_to_token(full_prefix, AddBos::Always).context("tokenize prefix")?;
        if prefix_tokens.is_empty()
            || prefix_tokens.len() + MAX_NEW_TOKENS as usize >= self.context_size as usize
        {
            return Ok(());
        }
        let ctx_key = self.prompt_state_cache_key(
            PromptStateCacheLayer::F7Context,
            full_prefix,
            &prefix_tokens,
        )?;
        {
            let mut cache = self
                .prompt_state_cache
                .lock()
                .map_err(|_| anyhow!("llama-local prompt-state cache mutex poisoned"))?;
            if cache.contains(&ctx_key) {
                return Ok(());
            }
        }
        let runtime = ctx_key.runtime_sha256().to_string();
        let _ctx = self.build_miss_context(
            model,
            ctx_key,
            &runtime,
            PromptStateCacheLayer::F7Context.as_str(),
            &prefix_tokens,
        )?;
        debug!(tokens = prefix_tokens.len(), "F7 context prefix checkpoint prewarmed");
        Ok(())
    }
}

/// Emit a `polish.prompt_cache_cold_prefill` instant on the `f7-polish` lane
/// recording why the F7 prefix-cache fast path was abandoned for a full prefill.
fn polish_cold_prefill(layer: &str, reason: &str) {
    current_instant(
        "polish.prompt_cache_cold_prefill",
        "polish",
        POLISH_LANE,
        json!({ "layer": layer, "reason": reason }),
    );
}

fn token_ids(tokens: &[LlamaToken]) -> Vec<i32> {
    tokens.iter().map(|t| t.0).collect()
}

fn copy_context_state(ctx: &LlamaContext<'_>) -> Result<Vec<u8>> {
    let state_bytes = ctx.get_state_size();
    let mut state = vec![0_u8; state_bytes];
    let saved = unsafe { ctx.copy_state_data(state.as_mut_ptr()) };
    if saved == 0 || saved > state_bytes {
        return Err(anyhow!(
            "llama.cpp copied an invalid state size: {saved} bytes into {state_bytes} byte buffer"
        ));
    }
    state.truncate(saved);
    Ok(state)
}

fn sha256_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

fn sha256_tokens(tokens: &[LlamaToken]) -> String {
    let mut hasher = Sha256::new();
    for token in tokens {
        hasher.update(token.0.to_le_bytes());
    }
    hex::encode(hasher.finalize())
}

/// ChatML prompt template used by the Apache-2.0 local cleanup models in
/// [`PolishRegistry`](crate::registry::PolishRegistry). Qwen3-family GGUFs use
/// a thinking-capable template; for cleanup we explicitly seed the assistant
/// turn with an empty `<think>` block so generation starts directly in the
/// visible answer channel.
#[cfg(test)]
fn build_chatml_prompt(system: &str, user: &str) -> String {
    build_chatml_prompt_with_options(system, user, false)
}

#[cfg(test)]
fn build_chatml_prompt_for_model(system: &str, user: &str, model_name: &str) -> String {
    build_chatml_prompt_with_options(system, user, model_uses_qwen_thinking_template(model_name))
}

fn model_uses_qwen_thinking_template(model_name: &str) -> bool {
    model_name.to_ascii_lowercase().contains("qwen3")
}

#[cfg(test)]
fn build_chatml_prompt_with_options(system: &str, user: &str, disable_thinking: bool) -> String {
    let (prefix, suffix) = build_chatml_prompt_split_with_options(system, user, disable_thinking);
    format!("{prefix}{suffix}")
}

/// Which chat template a local cleanup model expects, picked by model-name
/// substring. Mirrors the assistant backend's `build_prompt_split` dispatch so
/// the same GGUF gets the same framing in both paths.
#[derive(Clone, Copy)]
enum PromptTemplate {
    /// Qwen / SmolLM family — `<|im_start|>…<|im_end|>`.
    ChatMl { disable_thinking: bool },
    /// Gemma family — `<start_of_turn>…<end_of_turn>`, no dedicated system role.
    Gemma,
}

fn template_for_model(model_name: &str) -> PromptTemplate {
    if model_name.to_ascii_lowercase().contains("gemma") {
        PromptTemplate::Gemma
    } else {
        PromptTemplate::ChatMl { disable_thinking: model_uses_qwen_thinking_template(model_name) }
    }
}

/// Build the (stable prefix, per-utterance suffix) split for `model_name`'s
/// chat template. Gemma models get the Gemma template; everything else uses
/// ChatML (with Qwen3 thinking suppression when applicable). By construction
/// `format!("{prefix}{suffix}")` reproduces the full prompt (asserted in
/// tests), and the cache path re-checks that equality before trusting the
/// split. The stable prefix is what the F7 prompt-state cache restores; only
/// the suffix is decoded per turn.
///
/// The live `format()` path and the prewarmed base prefix MUST agree on the
/// template — otherwise the pinned base is not a token-prefix of the live
/// prompt and every turn cold-prefills, and a Gemma model fed ChatML loops its
/// output to the token cap.
fn build_prompt_split_for_model(system: &str, user: &str, model_name: &str) -> (String, String) {
    match template_for_model(model_name) {
        PromptTemplate::Gemma => build_gemma_prompt_split(system, user, turn_markers(model_name)),
        PromptTemplate::ChatMl { disable_thinking } => {
            build_chatml_prompt_split_with_options(system, user, disable_thinking)
        }
    }
}

/// Gemma cleanup prompt split. Gemma has no system role, so the system prompt
/// leads the single user turn (Gemma's trained convention). The stable prefix
/// is `{open}user\n{system}\n\n`; the suffix carries the transcript plus the
/// model-turn opener. `markers` selects the spelling this model registers as
/// control tokens (the gemma-4 line differs from the rest of the family).
fn build_gemma_prompt_split(system: &str, user: &str, markers: TurnMarkers) -> (String, String) {
    let system = system.trim();
    let mut prefix = String::with_capacity(system.len() + 32);
    prefix.push_str(markers.open);
    prefix.push_str("user\n");
    if !system.is_empty() {
        prefix.push_str(system);
        prefix.push_str("\n\n");
    }
    let suffix = format!("{}{}\n{}model\n", user.trim(), markers.close, markers.open);
    (prefix, suffix)
}

fn build_chatml_prompt_split_with_options(
    system: &str,
    user: &str,
    disable_thinking: bool,
) -> (String, String) {
    let mut prefix = String::with_capacity(system.len() + 64);
    if !system.is_empty() {
        prefix.push_str("<|im_start|>system\n");
        prefix.push_str(system);
        prefix.push_str("<|im_end|>\n");
    }
    prefix.push_str("<|im_start|>user\n");

    let mut suffix = String::with_capacity(user.len() + 48);
    suffix.push_str(user);
    suffix.push_str("<|im_end|>\n<|im_start|>assistant\n");
    if disable_thinking {
        suffix.push_str("<think>\n\n</think>\n\n");
    }
    (prefix, suffix)
}

/// The partial prompt `<|im_start|>system\n{base_system}` — the context- and
/// utterance-independent base. It is a genuine textual (and, modulo tokenizer
/// boundary effects the cache guards catch, token) prefix of the full F7 prompt
/// for any app context, so it is the pinnable floor for longest-prefix
/// matching. Empty when there is no base system prompt.
fn chatml_base_prefix(base_system: &str) -> String {
    if base_system.is_empty() {
        String::new()
    } else {
        format!("<|im_start|>system\n{base_system}")
    }
}

/// The context-independent base prefix for `model_name`'s template — the
/// pinnable floor for longest-prefix matching. Uses the SAME framing as
/// [`build_prompt_split_for_model`] so the pinned checkpoint is a genuine token
/// prefix of the live prompt. Empty when there is no base system prompt.
fn base_prefix_for_model(base_system: &str, model_name: &str) -> String {
    let base = base_system.trim();
    if base.is_empty() {
        return String::new();
    }
    match template_for_model(model_name) {
        PromptTemplate::Gemma => format!("{}user\n{base}", turn_markers(model_name).open),
        PromptTemplate::ChatMl { .. } => chatml_base_prefix(base),
    }
}

#[async_trait]
impl TextFormatter for LlamaLocal {
    async fn format(&self, raw: &str, ctx: &FormatContext) -> Result<String> {
        let model_name = self.model_path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        let user = user_prompt(raw);
        let (prefix, suffix) =
            build_prompt_split_for_model(&ctx.system_prompt(), &user, model_name);
        let prompt = format!("{prefix}{suffix}");
        let base = base_prefix_for_model(&ctx.base_system_prompt(), model_name);
        let me = self.clone_thin();
        let raw_for_retry = raw.to_string();
        let started = Instant::now();
        let text = tokio::task::spawn_blocking(move || -> Result<String> {
            me.ensure_loaded()?;
            let mut noop = |_: &str| {};
            let first = me.run_inference_cached(&prompt, &base, &prefix, &suffix, &mut noop)?;
            // The warm prompt-state-cache restore path occasionally degenerates
            // into a bare chat-role token (e.g. "model"): llama.cpp's KV-state
            // restore is not bit-exact and greedy decoding amplifies the drift.
            // Recover by recomputing once from a cold full prefill (no restore),
            // which reproduces the correct context deterministically. The outer
            // guard still rejects → raw if the retry also degenerates.
            if looks_like_degenerate_cleanup(&raw_for_retry, &first) {
                warn!(output = %first, "llama-local cleanup degenerated on the cached path; retrying with a cold full prefill");
                return me.run_inference_uncached(&prompt, &mut noop);
            }
            Ok(first)
        })
        .await
        .context("llama-local join")??;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        if elapsed_ms > 5_000 {
            warn!(
                elapsed_ms,
                "llama-local cleanup took {} ms; on CPU-only hardware consider \
                 switching to a cloud provider (`fono use polish groq` / `cerebras`) \
                 or a smaller model",
                elapsed_ms
            );
        } else {
            debug!(elapsed_ms, "llama-local cleanup ok");
        }
        if looks_like_degenerate_cleanup(raw, &text) {
            anyhow::bail!(
                "llama-local cleanup degenerated into the bare chat-role token {text:?} \
                 (KV-restore drift); falling back to raw text"
            );
        }
        if looks_like_clarification(&text) {
            anyhow::bail!(
                "llama-local returned a clarification reply instead of a cleaned transcript; \
                 falling back to raw text. response: {text:?}"
            );
        }
        if looks_like_translated_cleanup(raw, &text, ctx) {
            anyhow::bail!(
                "llama-local appears to have translated the transcript instead of cleaning it; \
                 falling back to raw text. response: {text:?}"
            );
        }
        Ok(text)
    }

    fn name(&self) -> &'static str {
        "llama-local"
    }

    /// Stream each decoded token piece over a channel so the orchestrator can
    /// inject words incrementally during the multi-second CPU cleanup. Shares
    /// the exact decode core (`run_inference_cached` → `generate_from_prefilled`)
    /// with [`Self::format`], so the prompt-state cache, sampler, stop-sequence
    /// handling, and `MAX_NEW_TOKENS` cap are identical. The degenerate
    /// cold-reprefill retry that `format()` applies is intentionally skipped
    /// here: a degenerate prefix is caught by the orchestrator's first-sentence
    /// guard gate, which falls back to the raw transcript. The cleanup guards
    /// are NOT applied inside this method — the caller runs them on the
    /// buffered prefix before committing any text to the cursor.
    async fn format_stream(
        &self,
        raw: &str,
        ctx: &FormatContext,
    ) -> Result<BoxStream<'static, Result<String>>> {
        let model_name = self.model_path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        let user = user_prompt(raw);
        let (prefix, suffix) =
            build_prompt_split_for_model(&ctx.system_prompt(), &user, model_name);
        let prompt = format!("{prefix}{suffix}");
        let base = base_prefix_for_model(&ctx.base_system_prompt(), model_name);
        let me = self.clone_thin();
        // Unbounded so the blocking decode thread is NEVER paced by the
        // injector. A bounded channel coupled CPU decode to per-word text
        // injection: once the buffer filled, every `send` blocked the decode
        // loop until the orchestrator typed a word, dragging generation from
        // ~22 tok/s down to ~8 tok/s. With an unbounded channel decode runs
        // flat-out and the orchestrator drains (and batches) at its own pace.
        let (tx, rx) = mpsc::unbounded_channel::<Result<String>>();
        tokio::task::spawn_blocking(move || {
            let result = (|| -> Result<String> {
                me.ensure_loaded()?;
                let tx_pieces = tx.clone();
                let mut on_piece = move |piece: &str| {
                    if !piece.is_empty() {
                        // A closed receiver (orchestrator dropped the stream on a
                        // guard hit / cancel) just means no one is listening; the
                        // decode finishes normally and is discarded.
                        let _ = tx_pieces.send(Ok(piece.to_string()));
                    }
                };
                me.run_inference_cached(&prompt, &base, &prefix, &suffix, &mut on_piece)
            })();
            if let Err(e) = result {
                let _ = tx.send(Err(e));
            }
        });
        Ok(UnboundedReceiverStream::new(rx).boxed())
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn prewarm(&self) -> Result<()> {
        let me = self.clone_thin();
        tokio::task::spawn_blocking(move || me.ensure_loaded())
            .await
            .context("llama-local prewarm join")?
    }

    async fn prewarm_prompt_cache(&self, base_system: &str) -> Result<()> {
        // Build the same base prefix the live `format()` path restores — with
        // the template chosen by model name (see `base_prefix_for_model` +
        // `format`) — so the pinned checkpoint built here is a cache hit at turn
        // time rather than a fresh multi-second prefill on the first dictation.
        let model_name = self.model_path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        let base = base_prefix_for_model(base_system, model_name);
        if base.is_empty() {
            return Ok(());
        }
        let me = self.clone_thin();
        tokio::task::spawn_blocking(move || -> Result<()> {
            me.ensure_loaded()?;
            let guard = me.state.lock().map_err(|_| anyhow!("llama-local mutex poisoned"))?;
            let model = guard.as_ref().ok_or_else(|| anyhow!("llama-local model not loaded"))?;
            me.ensure_base_prefix_cache(model, &base)
        })
        .await
        .context("llama-local prompt-cache prewarm join")?
    }

    async fn prewarm_context_cache(&self, full_system: &str) -> Result<()> {
        // The transcript-independent prefix the live `format()` path restores
        // for this app + language: `build_prompt_split_for_model(...).0`. Build
        // and cache its F7Context checkpoint so the first dictation into this
        // window is an exact restore instead of a multi-second prefix decode.
        let model_name = self.model_path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        let (full_prefix, _suffix) = build_prompt_split_for_model(full_system, "", model_name);
        if full_prefix.is_empty() {
            return Ok(());
        }
        let me = self.clone_thin();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("fono-polish-prewarm".into())
            .spawn(move || {
                lower_current_thread_priority_for_prewarm();
                let result = (|| -> Result<()> {
                    me.ensure_loaded()?;
                    me.warm_context_prefix(&full_prefix)
                })();
                let _ = tx.send(result);
            })
            .context("spawn llama-local context-cache prewarm thread")?;
        rx.await.context("llama-local context-cache prewarm thread exited before reporting")?
    }
}

#[cfg(target_os = "linux")]
fn lower_current_thread_priority_for_prewarm() {
    use std::ffi::c_int;

    unsafe extern "C" {
        fn nice(inc: c_int) -> c_int;
    }

    // Dedicated prewarm threads may run while audio capture and cloud STT are
    // active. Lowering only this short-lived thread keeps speculative cache work
    // from competing too aggressively; the thread exits after the prewarm, so
    // the lower priority cannot leak into later latency-critical cleanup calls.
    unsafe {
        let _ = nice(10);
    }
}

#[cfg(not(target_os = "linux"))]
fn lower_current_thread_priority_for_prewarm() {}

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
    fn qwen3_5_prompt_disables_thinking() {
        let p = build_chatml_prompt_for_model("be terse", "hello world", "qwen3.5-0.8b");
        assert!(p.ends_with("<|im_start|>assistant\n<think>\n\n</think>\n\n"));
    }

    #[test]
    fn gemma_prompt_does_not_seed_qwen_thinking() {
        let p = build_chatml_prompt_for_model("be terse", "hello world", "gemma-4-e2b");
        assert!(p.ends_with("<|im_start|>assistant\n"));
        assert!(!p.contains("<think>"));
    }

    #[test]
    fn gemma_model_uses_gemma_template() {
        // The default gemma-4 line ships non-standard control-token markers.
        let (prefix, suffix) =
            build_prompt_split_for_model("be terse", "hello world", "gemma-4-e2b");
        assert!(prefix.starts_with("<|turn>user\nbe terse\n\n"));
        assert_eq!(suffix, "hello world<turn|>\n<|turn>model\n");
        assert!(!prefix.contains("<|im_start|>"));
    }

    #[test]
    fn standard_gemma_model_keeps_classic_markers() {
        let (prefix, suffix) =
            build_prompt_split_for_model("be terse", "hello world", "gemma-2-2b");
        assert!(prefix.starts_with("<start_of_turn>user\nbe terse\n\n"));
        assert_eq!(suffix, "hello world<end_of_turn>\n<start_of_turn>model\n");
    }

    #[test]
    fn qwen_model_uses_chatml_template() {
        let (prefix, _suffix) = build_prompt_split_for_model("be terse", "hi", "qwen3.5-0.8b");
        assert!(prefix.starts_with("<|im_start|>system\nbe terse"));
        assert!(!prefix.contains("<start_of_turn>"));
    }

    #[test]
    fn gemma_split_reproduces_full_prompt() {
        let (prefix, suffix) =
            build_gemma_prompt_split("be terse", "hello world", TurnMarkers::GEMMA);
        assert_eq!(
            format!("{prefix}{suffix}"),
            "<start_of_turn>user\nbe terse\n\nhello world<end_of_turn>\n<start_of_turn>model\n"
        );
    }

    #[test]
    fn gemma_base_prefix_is_textual_prefix_of_full_prefix() {
        // The pinned Gemma base must be a textual prefix of the full Gemma
        // prefix so the cached base checkpoint is reusable via longest-prefix
        // matching (the fix for the looping / cold-prefill Gemma polish bug).
        let base_system = "Clean up the transcript.";
        let full_system = format!("{base_system}\n\nYou are dictating into a terminal.");
        let base = base_prefix_for_model(base_system, "gemma-4-e2b");
        let (full_prefix, _suffix) =
            build_prompt_split_for_model(&full_system, "hi", "gemma-4-e2b");
        assert_eq!(base, "<|turn>user\nClean up the transcript.");
        assert!(
            full_prefix.starts_with(&base),
            "gemma base must be a textual prefix of the full prefix\n base: {base:?}\n full: {full_prefix:?}"
        );
    }

    #[test]
    fn first_stop_marker_finds_earliest_turn_marker() {
        assert_eq!(first_stop_marker("clean text"), None);
        // The opener mid-stream (model degenerating into a new turn) truncates.
        let s = "Cleaned sentence.<start_of_turn>model";
        assert_eq!(first_stop_marker(s), Some(("Cleaned sentence.".len(), "<start_of_turn>")));
        // Earliest of several wins.
        let s2 = "a<|im_end|>b<end_of_turn>";
        assert_eq!(first_stop_marker(s2), Some((1, "<|im_end|>")));
    }

    #[test]
    fn missing_model_path_errors_clearly() {
        let m = LlamaLocal::new("/this/path/does/not/exist.gguf", 1024);
        let e = m.ensure_loaded().unwrap_err().to_string();
        assert!(e.contains("not found"), "got: {e}");
    }

    #[test]
    fn split_reproduces_full_prompt() {
        // The cache path trusts the split only when prefix+suffix == the full
        // prompt byte-for-byte; assert that invariant across template variants.
        for disable in [false, true] {
            let (prefix, suffix) =
                build_chatml_prompt_split_with_options("be terse", "hello world", disable);
            let full = build_chatml_prompt_with_options("be terse", "hello world", disable);
            assert_eq!(format!("{prefix}{suffix}"), full);
        }
        // Empty system still round-trips.
        let (prefix, suffix) = build_chatml_prompt_split_with_options("", "hi", false);
        assert_eq!(format!("{prefix}{suffix}"), build_chatml_prompt("", "hi"));
    }

    #[test]
    fn base_prefix_is_textual_prefix_of_full_prefix() {
        // The pinnable base `<|im_start|>system\n{base}` must be a textual prefix
        // of the full split prefix so the cached base checkpoint is reusable.
        let base_system = "Clean up the transcript.";
        let full_system = format!("{base_system}\n\nYou are dictating into a terminal.");
        let base = chatml_base_prefix(base_system);
        let (full_prefix, _suffix) =
            build_chatml_prompt_split_with_options(&full_system, "hi", false);
        assert!(!base.is_empty());
        assert!(
            full_prefix.starts_with(&base),
            "base must be a textual prefix of the full prefix\n base: {base:?}\n full: {full_prefix:?}"
        );
    }

    #[test]
    fn empty_base_prefix_when_no_base_system() {
        assert!(chatml_base_prefix("").is_empty());
    }
}
