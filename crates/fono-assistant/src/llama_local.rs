// SPDX-License-Identifier: GPL-3.0-only
//! Embedded llama.cpp assistant backend for local GGUF chat models.
//!
//! This is the default path for the wizard's `local` assistant. The
//! OpenAI-compatible/Ollama client remains available when the user manually
//! configures an explicit local server URL.

#![allow(clippy::significant_drop_tightening)]

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use fono_core::llama_backend::{backend, shared_model};
use fono_core::turn_trace::{current_instant, current_span, record_cache_mutation, CACHE_LANE};
use futures::stream::{BoxStream, StreamExt};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, info, warn};

use crate::history::ChatRole;
use crate::traits::{
    Assistant, AssistantCacheTrigger, AssistantContext, AssistantPromptCacheSnapshot,
    AssistantPromptCacheWarmup, TokenDelta,
};

const MAX_NEW_TOKENS: i32 = 384;
const MIN_CTX: u32 = 512;
const DEFAULT_BATCH_SIZE: u32 = 2048;
const DEFAULT_UBATCH_SIZE: u32 = 512;
const STREAM_CHANNEL_CAPACITY: usize = 32;
const STOP_MARKERS: &[&str] = &[
    "<end_of_turn>",
    "<start_of_turn>",
    "<|im_end|>",
    "<|end|>",
    "<|eot_id|>",
    "<|endoftext|>",
    "</s>",
];

// The process-wide `LlamaBackend` singleton and the `llama_cpp_2 →
// tracing` log redirector both live in `fono_core::llama_backend` so
// the assistant (voice chat) and polish (cleanup) embedded-LLM paths
// share ONE `LlamaBackend::init()`. A second init in the same process
// panics — see that module's docs.

pub struct LlamaLocalAssistant {
    model_path: PathBuf,
    context_size: u32,
    threads: i32,
    batch_size: Option<u32>,
    ubatch_size: Option<u32>,
    state: Arc<Mutex<Option<Arc<LlamaModel>>>>,
    prompt_state_cache: Arc<Mutex<PromptStateCache>>,
}

#[derive(Debug, Clone)]
pub struct RawPromptStateCacheRun {
    pub iteration: usize,
    pub latency_ms: u64,
    pub time_to_first_token_ms: Option<u64>,
    pub delta_count: usize,
    pub output_chars: usize,
    pub output: String,
    pub state_restore_ms: u64,
    pub decode_elapsed_ms: u64,
}

#[derive(Debug, Clone)]
pub struct RawPromptStateCacheReport {
    pub prompt_tokens: usize,
    pub state_bytes: usize,
    pub setup_prefill_ms: u64,
    pub runs: Vec<RawPromptStateCacheRun>,
}

#[derive(Debug, Clone)]
pub struct RawPromptPrefixCacheRun {
    pub iteration: usize,
    pub suffix_index: usize,
    pub suffix_chars: usize,
    pub suffix_tokens: usize,
    pub uncached_latency_ms: u64,
    pub cached_latency_ms: u64,
    pub cached_time_to_first_token_ms: Option<u64>,
    pub state_restore_ms: u64,
    pub suffix_prefill_ms: u64,
    pub cached_decode_elapsed_ms: u64,
    pub cached_delta_count: usize,
    pub uncached_output_chars: usize,
    pub cached_output_chars: usize,
    pub outputs_match: bool,
    pub uncached_output: String,
    pub cached_output: String,
}

#[derive(Debug, Clone)]
pub struct RawPromptPrefixCacheReport {
    pub cache_key: String,
    pub prefix_tokens: usize,
    pub state_bytes: usize,
    pub setup_prefill_ms: u64,
    pub runs: Vec<RawPromptPrefixCacheRun>,
}

/// Per-turn result of a simulated multi-turn conversation replay. Captures how
/// the cached prefix grows (and the per-turn cached cost stays flat) as history
/// accumulates.
#[derive(Debug, Clone)]
pub struct ConversationTurnReport {
    pub turn_index: usize,
    pub history_turns: usize,
    pub prefix_tokens: usize,
    pub suffix_tokens: usize,
    pub state_bytes: usize,
    pub setup_prefill_ms: u64,
    pub runs: Vec<RawPromptPrefixCacheRun>,
}

#[derive(Debug, Clone)]
pub struct ConversationPrefixCacheReport {
    pub model_name: String,
    pub turns: Vec<ConversationTurnReport>,
}

// The bounded prompt-state cache (LRU + byte budget + pinning) lives in
// `fono-core` so both the assistant (F8) and polish (F7) embedded backends can
// share it. This crate keeps only the llama.cpp-specific glue: building a
// checkpoint by prefilling tokens into a context, restoring one, and computing
// the content-fingerprint key.
use fono_core::prompt_cache::{
    PromptStateCache, PromptStateCacheEntry, PromptStateCacheKey, PromptStateCacheLayer,
};

impl LlamaLocalAssistant {
    pub fn new(model_path: impl Into<PathBuf>, context_size: u32) -> Self {
        Self::with_threads(model_path, context_size, num_threads())
    }

    pub fn with_threads(model_path: impl Into<PathBuf>, context_size: u32, threads: i32) -> Self {
        let tuned_batch = DEFAULT_BATCH_SIZE.min(context_size.max(MIN_CTX));
        let tuned_ubatch = DEFAULT_UBATCH_SIZE.min(tuned_batch);
        Self::with_runtime_options(
            model_path,
            context_size,
            threads,
            Some(tuned_batch),
            Some(tuned_ubatch),
        )
    }

    pub fn with_runtime_options(
        model_path: impl Into<PathBuf>,
        context_size: u32,
        threads: i32,
        batch_size: Option<u32>,
        ubatch_size: Option<u32>,
    ) -> Self {
        Self {
            model_path: model_path.into(),
            context_size: context_size.max(MIN_CTX),
            threads,
            batch_size,
            ubatch_size,
            state: Arc::new(Mutex::new(None)),
            prompt_state_cache: Arc::new(Mutex::new(PromptStateCache::default())),
        }
    }

    fn clone_thin(&self) -> Self {
        Self {
            model_path: self.model_path.clone(),
            context_size: self.context_size,
            threads: self.threads,
            batch_size: self.batch_size,
            ubatch_size: self.ubatch_size,
            state: Arc::clone(&self.state),
            prompt_state_cache: Arc::clone(&self.prompt_state_cache),
        }
    }

    fn ensure_loaded(&self) -> Result<()> {
        let span = current_span("llm.model_ensure_loaded", "assistant.llm", "llm");
        let mut guard = self.state.lock().map_err(|_| anyhow!("llama-local mutex poisoned"))?;
        if guard.is_some() {
            span.finish(json!({ "cache_hit": true }));
            return Ok(());
        }
        if !self.model_path.exists() {
            return Err(anyhow!(
                "local assistant model not found at {:?}; run `fono models install {}` or choose a cloud assistant backend",
                self.model_path,
                self.model_path.file_stem().and_then(|s| s.to_str()).unwrap_or("<model>")
            ));
        }
        let started = Instant::now();
        // Shared, process-wide weights: polish (F7) and the assistant (F8)
        // resolve their local GGUF from the same directory, so when both use
        // the same model (the default `gemma-4-e2b`) they share ONE
        // `LlamaModel` rather than each loading a ~3.2 GB copy. See
        // `fono_core::llama_backend::shared_model`.
        let model = shared_model(&self.model_path, &LlamaModelParams::default())?;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let model_name = self.model_path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
        let size_mb =
            std::fs::metadata(&self.model_path).map(|m| m.len() / (1024 * 1024)).unwrap_or(0);
        info!(
            "Assistant LLM ready: {model_name} ({size_mb} MB, {threads} threads, ctx={ctx}, batch={batch}, ubatch={ubatch}) in {elapsed_ms} ms",
            threads = self.threads,
            ctx = self.context_size,
            batch = self.batch_size.unwrap_or(self.context_size),
            ubatch = self.ubatch_size.map_or_else(|| "auto".to_string(), |v| v.to_string()),
        );
        *guard = Some(model);
        span.finish(json!({
            "cache_hit": false,
            "model": model_name,
            "size_mb": size_mb,
            "threads": self.threads,
            "ctx": self.context_size,
            "batch": self.batch_size.unwrap_or(self.context_size),
            "ubatch": self.ubatch_size,
            "elapsed_ms": elapsed_ms,
        }));
        Ok(())
    }

    /// Task 8 generation-time prefix cache. Restores a cached prefix checkpoint
    /// when one exists (building it on first use), then prefills only the
    /// per-turn suffix before generating. Returns `Ok(None)` — having emitted
    /// nothing — whenever the split cannot be reused safely (empty prefix/suffix,
    /// token-boundary mismatch, oversized prompt, or a failed restore) so the
    /// caller can fall back to a full prefill.
    #[allow(clippy::too_many_lines)]
    fn generate_with_prefix_cache<F>(
        &self,
        model: &LlamaModel,
        prefix: &str,
        suffix: &str,
        layer: PromptStateCacheLayer,
        on_delta: F,
    ) -> Result<Option<String>>
    where
        F: FnMut(String) -> Result<bool>,
    {
        if prefix.is_empty() || suffix.is_empty() {
            cold_prefill(layer.as_str(), "empty_prefix_or_suffix");
            return Ok(None);
        }
        let prefix_tokens =
            model.str_to_token(prefix, AddBos::Always).context("tokenize cached prefix")?;
        if prefix_tokens.is_empty() {
            cold_prefill(layer.as_str(), "empty_prefix_tokens");
            return Ok(None);
        }
        let full_prompt = format!("{prefix}{suffix}");
        let full_tokens =
            model.str_to_token(&full_prompt, AddBos::Always).context("tokenize cached prompt")?;
        if !full_tokens.starts_with(&prefix_tokens) {
            debug!(
                layer = layer.as_str(),
                "prompt-state cache token split incompatible; falling back"
            );
            cold_prefill(layer.as_str(), "token_split_incompatible");
            return Ok(None);
        }
        let suffix_tokens = &full_tokens[prefix_tokens.len()..];
        if suffix_tokens.is_empty() {
            cold_prefill(layer.as_str(), "empty_suffix_tokens");
            return Ok(None);
        }
        if full_tokens.len() + MAX_NEW_TOKENS as usize >= self.context_size as usize {
            debug!(
                layer = layer.as_str(),
                tokens = full_tokens.len(),
                ctx = self.context_size,
                "prompt-state cache prompt too large; falling back"
            );
            cold_prefill(layer.as_str(), "prompt_too_large");
            return Ok(None);
        }
        let key = self.prompt_state_cache_key(layer.clone(), prefix, &prefix_tokens)?;
        let cached = {
            let mut cache = self
                .prompt_state_cache
                .lock()
                .map_err(|_| anyhow!("llama-local prompt-state cache mutex poisoned"))?;
            let entry = cache.get(&key);
            current_instant(
                "llm.prompt_cache_lookup",
                "cache",
                CACHE_LANE,
                json!({
                    "layer": layer.as_str(),
                    "cache_key": key.stable_id(),
                    "hit": entry.is_some(),
                    "token_count": prefix_tokens.len(),
                    "cache_entries": cache.len(),
                    "cache_bytes": cache.bytes(),
                }),
            );
            entry
        };
        let mut ctx = self.new_context(model, "llm.prompt_cache_context_created")?;
        if let Some(entry) = cached {
            let restore_started = Instant::now();
            let restored_bytes = unsafe { ctx.set_state_data(&entry.state) };
            if restored_bytes == 0 {
                warn!(
                    layer = layer.as_str(),
                    "llama.cpp failed to restore prompt-state cache; falling back"
                );
                cold_prefill(layer.as_str(), "restore_failed");
                return Ok(None);
            }
            current_instant(
                "llm.prompt_cache_prefix_match",
                "cache",
                CACHE_LANE,
                json!({
                    "matched_layer": layer.as_str(),
                    "matched_tokens": entry.token_count,
                    "total_tokens": full_tokens.len(),
                    "decoded_suffix_tokens": suffix_tokens.len(),
                }),
            );
            current_instant(
                "llm.prompt_cache_restored",
                "assistant.llm",
                "llm",
                json!({
                    "layer": layer.as_str(),
                    "cache_key": key.stable_id(),
                    "state_bytes": entry.state.len(),
                    "restored_bytes": restored_bytes,
                    "restore_ms": restore_started.elapsed().as_millis() as u64,
                    "suffix_tokens": suffix_tokens.len(),
                }),
            );
        } else {
            // Exact-key miss. Before paying a full cold prefill from scratch,
            // restore the deepest cached prefix that is a token-prefix of this
            // prompt — a prior turn's `F8ChatPrefix` checkpoint (the prompt is
            // append-only, so turn N's prefix is a prefix of turn N+1's) or the
            // pinned `F8System` base — and prefill only the remaining prefix
            // tokens. Mirrors the F7 polish longest-prefix path.
            // (`AssistantTools` is prewarmed but is not a prompt prefix, so it
            // is intentionally excluded from the candidate layers.)
            let runtime = key.runtime_sha256().to_string();
            let longest = {
                let mut cache = self
                    .prompt_state_cache
                    .lock()
                    .map_err(|_| anyhow!("llama-local prompt-state cache mutex poisoned"))?;
                let hit_key = cache.find_longest_prefix(
                    &runtime,
                    &[PromptStateCacheLayer::F8ChatPrefix, PromptStateCacheLayer::F8System],
                    &token_ids(&prefix_tokens),
                );
                hit_key.and_then(|hk| cache.get(&hk).map(|entry| (hk, entry)))
            };
            let mut start = 0_usize;
            let mut matched = false;
            if let Some((hit_key, entry)) = longest {
                let restore_started = Instant::now();
                let restored_bytes = unsafe { ctx.set_state_data(&entry.state) };
                if restored_bytes == 0 {
                    warn!(
                        layer = layer.as_str(),
                        "llama.cpp failed to restore longest-prefix state; cold-prefilling"
                    );
                    ctx = self.new_context(model, "llm.prompt_cache_context_created")?;
                } else {
                    start = entry.token_count.min(prefix_tokens.len());
                    matched = true;
                    current_instant(
                        "llm.prompt_cache_prefix_match",
                        "cache",
                        CACHE_LANE,
                        json!({
                            "matched_layer": hit_key.layer().as_str(),
                            "matched_tokens": entry.token_count,
                            "total_tokens": full_tokens.len(),
                            "decoded_prefix_tokens": prefix_tokens.len().saturating_sub(start),
                            "decoded_suffix_tokens": suffix_tokens.len(),
                        }),
                    );
                    current_instant(
                        "llm.prompt_cache_restored",
                        "assistant.llm",
                        "llm",
                        json!({
                            "layer": hit_key.layer().as_str(),
                            "cache_key": hit_key.stable_id(),
                            "state_bytes": entry.state.len(),
                            "restored_bytes": restored_bytes,
                            "restore_ms": restore_started.elapsed().as_millis() as u64,
                            "suffix_tokens": suffix_tokens.len(),
                        }),
                    );
                }
            }
            if !matched {
                cold_prefill(layer.as_str(), "no_prefix_match");
            }
            if start < prefix_tokens.len() {
                self.prefill_tokens(
                    &mut ctx,
                    &prefix_tokens[start..],
                    start as i32,
                    false,
                    "llm.prompt_cache_build_prefill",
                )?;
            }
            // NOTE: we intentionally do NOT checkpoint the pre-suffix prefix
            // here. The post-generation "completed turn" checkpoint below is
            // always a deeper superset of it (system + history + this user +
            // reply vs. just system + history + this user), and the traces
            // confirm `find_longest_prefix` always selects that deeper entry —
            // the pre-suffix checkpoint was never the winner. Storing it would
            // burn a cache slot and an O(history) `copy_context_state` every
            // turn for nothing. On the rare degenerate turn where no
            // completed-turn checkpoint is stored (empty/near-empty reply), the
            // next turn gracefully falls back to the prior turn's completed-turn
            // checkpoint or the pinned base.
        }
        self.prefill_tokens(
            &mut ctx,
            suffix_tokens,
            prefix_tokens.len() as i32,
            true,
            "llm.prompt_cache_suffix_prefill",
        )?;
        let generation = generate_from_prefilled_context(
            model,
            &mut ctx,
            full_tokens.len() as i32,
            (suffix_tokens.len() - 1) as i32,
            None,
            on_delta,
        )?;
        // Option C: checkpoint the POST-generation state so the next turn can
        // restore the completed exchange (system + history + this user + reply)
        // instead of re-prefilling user_N + reply_N.
        //
        // Subtlety (proven empirically, 2026-06-09): the KV cache holds the
        // *sampled* token ids, but next turn the same reply text re-tokenizes as
        // part of a longer prompt. BPE merges the final reply token with the
        // following turn-closer (`<end_of_turn>` / `<|im_end|>`), so the raw
        // generated sequence is NOT a token-prefix of the next turn's prompt —
        // it misses by the trailing token(s) and `find_longest_prefix` rejects
        // the whole entry (the bug we observed: completed-turn checkpoints never
        // matched). The same hazard covers leading-space and any mid-reply
        // divergence between sampled and canonical tokenization.
        //
        // Fix: store only the longest prefix of the generated sequence that the
        // next turn reproduces verbatim — the common prefix with the canonical
        // "completed turn" rendering (reply trimmed + the template closer, i.e.
        // exactly how this turn appears in the next turn's history). Truncate
        // the KV cache to that length so the saved state's position count equals
        // the recorded token count — the invariant every other checkpoint holds
        // (restore sets n_past to the token count, then prefills the new turn
        // into free cells). Cells past the boundary would be stale anyway.
        if !generation.tokens.is_empty() {
            let mut combined: Vec<llama_cpp_2::token::LlamaToken> =
                Vec::with_capacity(full_tokens.len() + generation.tokens.len());
            combined.extend_from_slice(&full_tokens);
            combined.extend_from_slice(&generation.tokens);
            let closer = if full_prompt.contains("<start_of_turn>") {
                "<end_of_turn>\n"
            } else {
                "<|im_end|>\n"
            };
            let canonical = format!("{full_prompt}{}{closer}", generation.text.trim());
            let reusable_len = model
                .str_to_token(&canonical, AddBos::Always)
                .ok()
                .map_or(0, |canon| common_prefix_len(&combined, &canon))
                .min(combined.len());
            // Only worth storing if it covers reply tokens beyond the
            // pre-generation prefix (already checkpointed above), and leaves
            // room for the next turn's framing + generation budget.
            if reusable_len > full_tokens.len()
                && reusable_len + MAX_NEW_TOKENS as usize <= self.context_size as usize
            {
                // Drop KV cells at positions >= reusable_len so the serialized
                // state covers exactly `reusable_len` positions.
                let truncated = reusable_len == combined.len()
                    || ctx.clear_kv_cache_seq(Some(0), Some(reusable_len as u32), None).is_ok();
                if truncated {
                    if let Ok(post_state) = copy_context_state(&ctx) {
                        let reusable = &combined[..reusable_len];
                        if let Ok(post_key) = self.prompt_state_cache_key(
                            PromptStateCacheLayer::F8ChatPrefix,
                            &canonical,
                            reusable,
                        ) {
                            let post_bytes = post_state.len();
                            if let Ok(mut cache) = self.prompt_state_cache.lock() {
                                let report = cache.insert(
                                    post_key,
                                    PromptStateCacheEntry::with_tokens(
                                        post_state,
                                        token_ids(reusable),
                                    ),
                                );
                                record_cache_mutation(&report);
                            }
                            current_instant(
                                "llm.prompt_cache_completed_turn",
                                "cache",
                                CACHE_LANE,
                                json!({
                                    "layer": layer.as_str(),
                                    "prefix_tokens": full_tokens.len(),
                                    "reply_tokens": generation.tokens.len(),
                                    "reusable_tokens": reusable_len,
                                    "dropped_tail_tokens": combined.len() - reusable_len,
                                    "total_tokens": combined.len(),
                                    "state_bytes": post_bytes,
                                }),
                            );
                        }
                    }
                }
            }
        }
        Ok(Some(generation.text.trim().to_string()))
    }

    fn build_prompt_prefix_cache(
        &self,
        model: &LlamaModel,
        layer: PromptStateCacheLayer,
        prefix: &str,
    ) -> Result<()> {
        let prefix = prefix.trim_end();
        if prefix.is_empty() {
            return Ok(());
        }
        let prefix_tokens =
            model.str_to_token(prefix, AddBos::Always).context("tokenize prompt prefix")?;
        if prefix_tokens.is_empty() {
            return Ok(());
        }
        if prefix_tokens.len() + MAX_NEW_TOKENS as usize >= self.context_size as usize {
            debug!(
                layer = layer.as_str(),
                tokens = prefix_tokens.len(),
                ctx = self.context_size,
                "prompt-state cache prefix too large; skipping"
            );
            return Ok(());
        }
        let key = self.prompt_state_cache_key(layer.clone(), prefix, &prefix_tokens)?;
        {
            let mut cache = self
                .prompt_state_cache
                .lock()
                .map_err(|_| anyhow!("llama-local prompt-state cache mutex poisoned"))?;
            if cache.contains(&key) {
                debug!(
                    layer = layer.as_str(),
                    tokens = prefix_tokens.len(),
                    "prompt-state cache hit"
                );
                return Ok(());
            }
        }
        let started = Instant::now();
        let mut ctx = self.new_context(model, "llm.prompt_cache_build_context_created")?;
        self.prefill_tokens(&mut ctx, &prefix_tokens, 0, false, "llm.prompt_cache_build_prefill")?;
        let state = copy_context_state(&ctx)?;
        let state_bytes = state.len();
        let mut cache = self
            .prompt_state_cache
            .lock()
            .map_err(|_| anyhow!("llama-local prompt-state cache mutex poisoned"))?;
        let entry = PromptStateCacheEntry::with_tokens(state, token_ids(&prefix_tokens));
        let report = if layer.is_pinnable() {
            cache.insert_pinned(key, entry)
        } else {
            cache.insert(key, entry)
        };
        record_cache_mutation(&report);
        current_instant(
            "llm.prompt_cache_built",
            "assistant.llm",
            "llm",
            json!({
                "layer": layer.as_str(),
                "prefix_tokens": prefix_tokens.len(),
                "state_bytes": state_bytes,
                "elapsed_ms": started.elapsed().as_millis() as u64,
            }),
        );
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn run_inference<F>(&self, prompt: &str, on_delta: F) -> Result<String>
    where
        F: FnMut(String) -> Result<bool>,
    {
        let guard = self.state.lock().map_err(|_| anyhow!("llama-local mutex poisoned"))?;
        let model = guard.as_ref().ok_or_else(|| anyhow!("llama-local model not loaded"))?;
        self.run_inference_with_model(model, prompt, on_delta)
    }

    /// Reply generation with the Task 8 prefix cache. Only attempts the cached
    /// path when the split reproduces the full prompt byte-for-byte; on any
    /// incompatibility it falls back to a full prefill having emitted nothing.
    fn run_inference_with_prefix_cache<F>(
        &self,
        prompt: &str,
        prefix: &str,
        suffix: &str,
        layer: PromptStateCacheLayer,
        mut on_delta: F,
    ) -> Result<String>
    where
        F: FnMut(String) -> Result<bool>,
    {
        let guard = self.state.lock().map_err(|_| anyhow!("llama-local mutex poisoned"))?;
        let model = guard.as_ref().ok_or_else(|| anyhow!("llama-local model not loaded"))?;
        if format!("{prefix}{suffix}") == prompt {
            if let Some(text) =
                self.generate_with_prefix_cache(model, prefix, suffix, layer, &mut on_delta)?
            {
                return Ok(text);
            }
        } else {
            cold_prefill(layer.as_str(), "prompt_split_mismatch");
        }
        self.run_inference_with_model(model, prompt, on_delta)
    }

    #[allow(clippy::too_many_lines)]
    fn run_inference_with_model<F>(
        &self,
        model: &LlamaModel,
        prompt: &str,
        on_delta: F,
    ) -> Result<String>
    where
        F: FnMut(String) -> Result<bool>,
    {
        let n_ctx =
            NonZeroU32::new(self.context_size).unwrap_or_else(|| NonZeroU32::new(MIN_CTX).unwrap());
        let batch_size = self.batch_size.unwrap_or(self.context_size).max(1);
        let mut ctx_params = LlamaContextParams::default()
            .with_n_ctx(Some(n_ctx))
            .with_n_batch(batch_size)
            .with_n_threads(self.threads)
            .with_n_threads_batch(self.threads);
        if let Some(ubatch_size) = self.ubatch_size {
            ctx_params = ctx_params.with_n_ubatch(ubatch_size.max(1));
        }
        let ctx_started = Instant::now();
        let mut ctx = model.new_context(backend(), ctx_params).context("create llama context")?;
        current_instant(
            "llm.context_created",
            "assistant.llm",
            "llm",
            json!({
                "ctx": self.context_size,
                "batch": batch_size,
                "ubatch": self.ubatch_size,
                "threads": self.threads,
                "elapsed_ms": ctx_started.elapsed().as_millis() as u64,
            }),
        );

        let tokenize_span = current_span("llm.tokenize_prompt", "assistant.llm", "llm");
        let tokens =
            model.str_to_token(prompt, AddBos::Always).context("tokenize assistant prompt")?;
        tokenize_span.finish(
            json!({ "prompt_chars": prompt.chars().count(), "prompt_tokens": tokens.len() }),
        );
        if tokens.len() as u32 + (MAX_NEW_TOKENS as u32) >= self.context_size {
            return Err(anyhow!(
                "assistant prompt is {} tokens, leaving < {} for generation in a context of {}; raise `[assistant.local].context` or shorten the conversation",
                tokens.len(),
                MAX_NEW_TOKENS,
                self.context_size
            ));
        }

        let batch_span = current_span("llm.prefill_batch_build", "assistant.llm", "llm");
        let prefill_batch_capacity = self.context_size as usize;
        let mut batch = LlamaBatch::new(prefill_batch_capacity, 1);
        let last_prefill_idx = tokens.len() as i32 - 1;
        for (i, token) in tokens.iter().enumerate() {
            batch
                .add(*token, i as i32, &[0], i as i32 == last_prefill_idx)
                .context("prefill batch.add")?;
        }
        batch_span.finish(json!({
            "prompt_tokens": tokens.len(),
            "batch_capacity": prefill_batch_capacity,
        }));
        let prefill_span = current_span("llm.prefill_decode", "assistant.llm", "llm");
        ctx.decode(&mut batch).context("prefill decode")?;
        prefill_span.finish(json!({ "prompt_tokens": tokens.len() }));

        let generation = generate_from_prefilled_context(
            model,
            &mut ctx,
            tokens.len() as i32,
            last_prefill_idx,
            None,
            on_delta,
        )?;
        Ok(generation.text.trim().to_string())
    }
    /// Run an already-rendered llama prompt and stream deltas. Intended for diagnostics and benchmark replay.
    pub async fn reply_raw_prompt_stream(
        &self,
        prompt: String,
    ) -> Result<BoxStream<'static, Result<TokenDelta>>> {
        let me = self.clone_thin();
        let started = Instant::now();
        let (tx, rx) = mpsc::channel::<Result<TokenDelta>>(STREAM_CHANNEL_CAPACITY);
        tokio::task::spawn_blocking(move || {
            let total_span =
                current_span("llm.local_raw_prompt_streaming_inference", "assistant.llm", "llm");
            let mut deltas_emitted = 0_u32;
            let result = (|| -> Result<String> {
                me.ensure_loaded()?;
                me.run_inference(&prompt, |delta| {
                    let delta = delta.trim_start_matches('\u{feff}').to_string();
                    if delta.is_empty() {
                        return Ok(true);
                    }
                    deltas_emitted = deltas_emitted.saturating_add(1);
                    Ok(tx.blocking_send(Ok(TokenDelta::text(delta))).is_ok())
                })
            })();
            let elapsed_ms = started.elapsed().as_millis() as u64;
            match result {
                Ok(text) => {
                    total_span.finish(json!({
                        "reply_chars": text.chars().count(),
                        "deltas": deltas_emitted,
                        "elapsed_ms": elapsed_ms,
                    }));
                    current_instant(
                        "llm.local_raw_prompt_stream_finished",
                        "assistant.llm",
                        "llm",
                        json!({
                            "elapsed_ms": elapsed_ms,
                            "reply_chars": text.chars().count(),
                            "deltas": deltas_emitted,
                        }),
                    );
                }
                Err(e) => {
                    total_span.finish(json!({
                        "error": e.to_string(),
                        "deltas": deltas_emitted,
                        "elapsed_ms": elapsed_ms,
                    }));
                    let _ = tx.blocking_send(Err(e));
                }
            }
        });
        current_instant(
            "llm.local_raw_prompt_stream_started",
            "assistant.llm",
            "llm",
            json!({ "channel_capacity": STREAM_CHANNEL_CAPACITY }),
        );
        Ok(ReceiverStream::new(rx).boxed())
    }

    /// Run an already-rendered llama prompt. Intended for diagnostics and benchmark replay.
    pub async fn reply_raw_prompt(&self, prompt: String) -> Result<String> {
        let me = self.clone_thin();
        tokio::task::spawn_blocking(move || -> Result<String> {
            let total_span = current_span("llm.local_raw_prompt_inference", "assistant.llm", "llm");
            me.ensure_loaded()?;
            let text = me.run_inference(&prompt, |_| Ok(true))?;
            total_span.finish(json!({ "reply_chars": text.chars().count() }));
            Ok(text)
        })
        .await
        .context("local assistant raw prompt join")?
    }

    /// Replay one raw prompt repeatedly from an in-memory llama.cpp state snapshot taken after prompt prefill.
    pub async fn replay_raw_prompt_with_state_cache(
        &self,
        prompt: String,
        iterations: usize,
    ) -> Result<RawPromptStateCacheReport> {
        let me = self.clone_thin();
        tokio::task::spawn_blocking(move || -> Result<RawPromptStateCacheReport> {
            me.ensure_loaded()?;
            me.run_state_cache_replay(&prompt, iterations.max(1))
        })
        .await
        .context("local assistant state-cache replay join")?
    }

    /// Replay multiple raw prompt suffixes from one cached prefix checkpoint.
    ///
    /// This benchmark path models real assistant usage better than exact full-prompt replay: the stable
    /// system/tool/window prefix is prefetched once, while the current user request suffix changes.
    pub async fn replay_raw_prompt_prefix_cache(
        &self,
        prefix: String,
        suffixes: Vec<String>,
        iterations: usize,
    ) -> Result<RawPromptPrefixCacheReport> {
        let me = self.clone_thin();
        tokio::task::spawn_blocking(move || -> Result<RawPromptPrefixCacheReport> {
            me.ensure_loaded()?;
            me.run_prefix_cache_replay(&prefix, &suffixes, iterations.max(1))
        })
        .await
        .context("local assistant prefix-cache replay join")?
    }

    #[allow(clippy::too_many_lines)]
    fn run_prefix_cache_replay(
        &self,
        prefix: &str,
        suffixes: &[String],
        iterations: usize,
    ) -> Result<RawPromptPrefixCacheReport> {
        if suffixes.is_empty() {
            return Err(anyhow!("prefix-cache replay requires at least one suffix"));
        }
        let guard = self.state.lock().map_err(|_| anyhow!("llama-local mutex poisoned"))?;
        let model = guard.as_ref().ok_or_else(|| anyhow!("llama-local model not loaded"))?;

        let prefix_tokens =
            model.str_to_token(prefix, AddBos::Always).context("tokenize prefix")?;
        if prefix_tokens.is_empty() {
            return Err(anyhow!("prefix-cache replay produced an empty prefix token list"));
        }
        let cache_key = self.prompt_state_cache_key(
            PromptStateCacheLayer::BenchmarkPrefix,
            prefix,
            &prefix_tokens,
        )?;
        let stable_key = cache_key.stable_id();
        let cache_hit = {
            let mut cache = self
                .prompt_state_cache
                .lock()
                .map_err(|_| anyhow!("llama-local prompt-state cache mutex poisoned"))?;
            cache.get(&cache_key)
        };

        let (state, setup_prefill_ms) = if let Some(entry) = cache_hit {
            if entry.token_count != prefix_tokens.len() {
                return Err(anyhow!(
                    "cached prefix token count mismatch: cached={}, current={}",
                    entry.token_count,
                    prefix_tokens.len()
                ));
            }
            (entry.state, 0)
        } else {
            let mut setup_ctx =
                self.new_context(model, "llm.prefix_cache_setup_context_created")?;
            let prefill_started = Instant::now();
            self.prefill_tokens(
                &mut setup_ctx,
                &prefix_tokens,
                0,
                false,
                "llm.prefix_cache_setup_prefill",
            )?;
            let setup_prefill_ms = prefill_started.elapsed().as_millis() as u64;
            let state = copy_context_state(&setup_ctx)?;
            let entry = PromptStateCacheEntry::new(state.clone(), prefix_tokens.len());
            let mut cache = self
                .prompt_state_cache
                .lock()
                .map_err(|_| anyhow!("llama-local prompt-state cache mutex poisoned"))?;
            cache.insert(cache_key, entry);
            current_instant(
                "llm.prefix_cache_inserted",
                "assistant.llm",
                "llm",
                json!({
                    "cache_key": stable_key,
                    "prefix_tokens": prefix_tokens.len(),
                    "state_bytes": state.len(),
                    "setup_prefill_ms": setup_prefill_ms,
                }),
            );
            (state, setup_prefill_ms)
        };

        let mut runs = Vec::with_capacity(iterations.saturating_mul(suffixes.len()));
        for iteration in 0..iterations {
            for (suffix_index, suffix) in suffixes.iter().enumerate() {
                let full_prompt = format!("{prefix}{suffix}");
                let full_tokens = model
                    .str_to_token(&full_prompt, AddBos::Always)
                    .context("tokenize prefix-cache full prompt")?;
                if !full_tokens.starts_with(&prefix_tokens) {
                    return Err(anyhow!(
                        "prefix-cache split is not token-boundary compatible for suffix {}; choose a prefix that ends on a stable token boundary",
                        suffix_index + 1
                    ));
                }
                let suffix_tokens = full_tokens[prefix_tokens.len()..].to_vec();
                if prefix_tokens.len() + suffix_tokens.len() + MAX_NEW_TOKENS as usize
                    >= self.context_size as usize
                {
                    return Err(anyhow!(
                        "cached prefix plus suffix is {} tokens, leaving < {} for generation in context {}; shorten the prompt or raise context size",
                        prefix_tokens.len() + suffix_tokens.len(),
                        MAX_NEW_TOKENS,
                        self.context_size
                    ));
                }
                if suffix_tokens.is_empty() {
                    return Err(anyhow!(
                        "prefix-cache suffix {} tokenized to zero tokens",
                        suffix_index + 1
                    ));
                }

                let uncached_started = Instant::now();
                let uncached_output =
                    self.run_inference_with_model(model, &full_prompt, |_| Ok(true))?;
                let uncached_latency_ms = uncached_started.elapsed().as_millis() as u64;

                let restore_started = Instant::now();
                let mut cached_ctx = self.new_context(model, "llm.prefix_cache_context_created")?;
                let restored_bytes = unsafe { cached_ctx.set_state_data(&state) };
                let state_restore_ms = restore_started.elapsed().as_millis() as u64;
                if restored_bytes == 0 {
                    return Err(anyhow!("llama.cpp failed to restore cached prefix state"));
                }
                current_instant(
                    "llm.prefix_cache_restored",
                    "assistant.llm",
                    "llm",
                    json!({
                        "iteration": iteration + 1,
                        "suffix_index": suffix_index + 1,
                        "cache_key": stable_key,
                        "state_bytes": state.len(),
                        "restored_bytes": restored_bytes,
                        "elapsed_ms": state_restore_ms,
                    }),
                );

                let cached_started = Instant::now();
                let suffix_prefill_started = Instant::now();
                self.prefill_tokens(
                    &mut cached_ctx,
                    &suffix_tokens,
                    prefix_tokens.len() as i32,
                    true,
                    "llm.prefix_cache_suffix_prefill",
                )?;
                let suffix_prefill_ms = suffix_prefill_started.elapsed().as_millis() as u64;
                let mut first_token_ms = None;
                let mut delta_count = 0_usize;
                let generation = generate_from_prefilled_context(
                    model,
                    &mut cached_ctx,
                    (prefix_tokens.len() + suffix_tokens.len()) as i32,
                    (suffix_tokens.len() - 1) as i32,
                    None,
                    |delta| {
                        if first_token_ms.is_none() {
                            first_token_ms = Some(cached_started.elapsed().as_millis() as u64);
                        }
                        if delta.is_empty() {
                            return Ok(true);
                        }
                        delta_count = delta_count.saturating_add(1);
                        Ok(true)
                    },
                )?;
                let cached_output = generation.text.trim().to_string();
                let cached_latency_ms = cached_started.elapsed().as_millis() as u64;
                let uncached_output = uncached_output.trim().to_string();
                current_instant(
                    "llm.prefix_cache_iteration_finished",
                    "assistant.llm",
                    "llm",
                    json!({
                        "iteration": iteration + 1,
                        "suffix_index": suffix_index + 1,
                        "suffix_tokens": suffix_tokens.len(),
                        "uncached_latency_ms": uncached_latency_ms,
                        "cached_latency_ms": cached_latency_ms,
                        "cached_time_to_first_token_ms": first_token_ms,
                        "state_restore_ms": state_restore_ms,
                        "suffix_prefill_ms": suffix_prefill_ms,
                        "cached_decode_elapsed_ms": generation.elapsed_ms,
                        "outputs_match": uncached_output == cached_output,
                    }),
                );
                runs.push(RawPromptPrefixCacheRun {
                    iteration: iteration + 1,
                    suffix_index: suffix_index + 1,
                    suffix_chars: suffix.chars().count(),
                    suffix_tokens: suffix_tokens.len(),
                    uncached_latency_ms,
                    cached_latency_ms,
                    cached_time_to_first_token_ms: first_token_ms,
                    state_restore_ms,
                    suffix_prefill_ms,
                    cached_decode_elapsed_ms: generation.elapsed_ms,
                    cached_delta_count: delta_count,
                    uncached_output_chars: uncached_output.chars().count(),
                    cached_output_chars: cached_output.chars().count(),
                    outputs_match: uncached_output == cached_output,
                    uncached_output,
                    cached_output,
                });
            }
        }

        Ok(RawPromptPrefixCacheReport {
            cache_key: stable_key,
            prefix_tokens: prefix_tokens.len(),
            state_bytes: state.len(),
            setup_prefill_ms,
            runs,
        })
    }

    /// Benchmark the multi-turn prefix cache through the *real* reply-prompt
    /// splitter ([`build_prompt_split`]). Simulates a conversation that grows by
    /// one `(user, assistant)` exchange per turn: at turn `t` the history holds
    /// the first `t` exchanges, the cache prefix is the system block plus that
    /// history, and the suffix is the current user text. Each turn replays
    /// uncached-vs-cached generation. This is the end-to-end evidence that the
    /// system-first, append-only Gemma layout keeps per-turn cached cost flat
    /// (restore + small suffix prefill) while the uncached path scales with the
    /// whole growing prefix.
    pub async fn replay_conversation_prefix_cache(
        &self,
        system_prompt: String,
        user_turns: Vec<String>,
        assistant_reply: String,
        iterations: usize,
    ) -> Result<ConversationPrefixCacheReport> {
        let me = self.clone_thin();
        let model_name =
            self.model_path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
        tokio::task::spawn_blocking(move || -> Result<ConversationPrefixCacheReport> {
            me.ensure_loaded()?;
            if user_turns.is_empty() {
                return Err(anyhow!("conversation replay requires at least one user turn"));
            }
            let mut turns = Vec::with_capacity(user_turns.len());
            for (t, user) in user_turns.iter().enumerate() {
                let mut history: Vec<crate::history::ChatTurn> = Vec::with_capacity(t * 2);
                for prior in user_turns.iter().take(t) {
                    history.push(crate::history::ChatTurn {
                        role: ChatRole::User,
                        content: prior.clone(),
                        at: Instant::now(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                    });
                    history.push(crate::history::ChatTurn {
                        role: ChatRole::Assistant,
                        content: assistant_reply.clone(),
                        at: Instant::now(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                    });
                }
                let ctx = AssistantContext {
                    system_prompt: system_prompt.clone(),
                    history,
                    ..AssistantContext::default()
                };
                let (prefix, suffix) = build_prompt_split(&ctx, user, &model_name);
                let report = me.run_prefix_cache_replay(
                    &prefix,
                    std::slice::from_ref(&suffix),
                    iterations.max(1),
                )?;
                let suffix_tokens = report.runs.first().map_or(0, |r| r.suffix_tokens);
                turns.push(ConversationTurnReport {
                    turn_index: t + 1,
                    history_turns: t * 2,
                    prefix_tokens: report.prefix_tokens,
                    suffix_tokens,
                    state_bytes: report.state_bytes,
                    setup_prefill_ms: report.setup_prefill_ms,
                    runs: report.runs,
                });
            }
            Ok(ConversationPrefixCacheReport { model_name, turns })
        })
        .await
        .context("local assistant conversation prefix-cache replay join")?
    }

    fn new_context<'model>(
        &self,
        model: &'model LlamaModel,
        event_name: &'static str,
    ) -> Result<LlamaContext<'model>> {
        let n_ctx =
            NonZeroU32::new(self.context_size).unwrap_or_else(|| NonZeroU32::new(MIN_CTX).unwrap());
        let batch_size = self.batch_size.unwrap_or(self.context_size).max(1);
        let mut ctx_params = LlamaContextParams::default()
            .with_n_ctx(Some(n_ctx))
            .with_n_batch(batch_size)
            .with_n_threads(self.threads)
            .with_n_threads_batch(self.threads);
        if let Some(ubatch_size) = self.ubatch_size {
            ctx_params = ctx_params.with_n_ubatch(ubatch_size.max(1));
        }
        let ctx_started = Instant::now();
        let ctx = model.new_context(backend(), ctx_params).context("create llama context")?;
        current_instant(
            event_name,
            "assistant.llm",
            "llm",
            json!({
                "ctx": self.context_size,
                "batch": batch_size,
                "ubatch": self.ubatch_size,
                "threads": self.threads,
                "elapsed_ms": ctx_started.elapsed().as_millis() as u64,
            }),
        );
        Ok(ctx)
    }

    fn prefill_tokens(
        &self,
        ctx: &mut LlamaContext<'_>,
        tokens: &[llama_cpp_2::token::LlamaToken],
        start_pos: i32,
        logits_last: bool,
        event_name: &'static str,
    ) -> Result<()> {
        if tokens.is_empty() {
            return Err(anyhow!("cannot prefill an empty token list"));
        }
        let batch_span = current_span(event_name, "assistant.llm", "llm");
        let prefill_batch_capacity = self.context_size as usize;
        let mut batch = LlamaBatch::new(prefill_batch_capacity, 1);
        let last_idx = tokens.len() - 1;
        for (i, token) in tokens.iter().enumerate() {
            batch
                .add(*token, start_pos + i as i32, &[0], logits_last && i == last_idx)
                .context("prefill batch.add")?;
        }
        ctx.decode(&mut batch).context("prefill decode")?;
        batch_span.finish(json!({
            "prompt_tokens": tokens.len(),
            "start_pos": start_pos,
            "batch_capacity": prefill_batch_capacity,
            "logits_last": logits_last,
        }));
        Ok(())
    }

    fn prompt_state_cache_key(
        &self,
        layer: PromptStateCacheLayer,
        prompt: &str,
        tokens: &[llama_cpp_2::token::LlamaToken],
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
            "llama-cpp-2:{}|model={}|size={}|modified={}|ctx={}|threads={}|batch={}|ubatch={}",
            env!("CARGO_PKG_VERSION"),
            self.model_path.display(),
            metadata.len(),
            modified,
            self.context_size,
            self.threads,
            self.batch_size.unwrap_or(self.context_size),
            self.ubatch_size.map_or_else(|| "auto".to_string(), |v| v.to_string())
        );
        Ok(PromptStateCacheKey::new(
            layer,
            sha256_text(&runtime_identity),
            sha256_text(prompt),
            sha256_tokens(tokens),
            tokens.len(),
        ))
    }

    #[allow(clippy::too_many_lines)]
    fn run_state_cache_replay(
        &self,
        prompt: &str,
        iterations: usize,
    ) -> Result<RawPromptStateCacheReport> {
        let guard = self.state.lock().map_err(|_| anyhow!("llama-local mutex poisoned"))?;
        let model = guard.as_ref().ok_or_else(|| anyhow!("llama-local model not loaded"))?;

        let n_ctx =
            NonZeroU32::new(self.context_size).unwrap_or_else(|| NonZeroU32::new(MIN_CTX).unwrap());
        let batch_size = self.batch_size.unwrap_or(self.context_size).max(1);
        let mut ctx_params = LlamaContextParams::default()
            .with_n_ctx(Some(n_ctx))
            .with_n_batch(batch_size)
            .with_n_threads(self.threads)
            .with_n_threads_batch(self.threads);
        if let Some(ubatch_size) = self.ubatch_size {
            ctx_params = ctx_params.with_n_ubatch(ubatch_size.max(1));
        }
        let ctx_started = Instant::now();
        let mut ctx = model.new_context(backend(), ctx_params).context("create llama context")?;
        current_instant(
            "llm.state_cache_context_created",
            "assistant.llm",
            "llm",
            json!({
                "ctx": self.context_size,
                "batch": batch_size,
                "ubatch": self.ubatch_size,
                "threads": self.threads,
                "elapsed_ms": ctx_started.elapsed().as_millis() as u64,
            }),
        );

        let tokenize_span = current_span("llm.state_cache_tokenize_prompt", "assistant.llm", "llm");
        let tokens =
            model.str_to_token(prompt, AddBos::Always).context("tokenize assistant prompt")?;
        tokenize_span.finish(
            json!({ "prompt_chars": prompt.chars().count(), "prompt_tokens": tokens.len() }),
        );
        if tokens.len() as u32 + (MAX_NEW_TOKENS as u32) >= self.context_size {
            return Err(anyhow!(
                "assistant prompt is {} tokens, leaving < {} for generation in a context of {}; raise `[assistant.local].context` or shorten the conversation",
                tokens.len(),
                MAX_NEW_TOKENS,
                self.context_size
            ));
        }

        let batch_span =
            current_span("llm.state_cache_prefill_batch_build", "assistant.llm", "llm");
        let prefill_batch_capacity = self.context_size as usize;
        let mut batch = LlamaBatch::new(prefill_batch_capacity, 1);
        let last_prefill_idx = tokens.len() as i32 - 1;
        for (i, token) in tokens.iter().enumerate() {
            batch
                .add(*token, i as i32, &[0], i as i32 == last_prefill_idx)
                .context("prefill batch.add")?;
        }
        batch_span.finish(json!({
            "prompt_tokens": tokens.len(),
            "batch_capacity": prefill_batch_capacity,
        }));
        let setup_prefill_started = Instant::now();
        let prefill_span = current_span("llm.state_cache_prefill_decode", "assistant.llm", "llm");
        ctx.decode(&mut batch).context("prefill decode")?;
        let setup_prefill_ms = setup_prefill_started.elapsed().as_millis() as u64;
        prefill_span
            .finish(json!({ "prompt_tokens": tokens.len(), "elapsed_ms": setup_prefill_ms }));

        let first_token = LlamaSampler::greedy().sample(&ctx, last_prefill_idx);
        current_instant(
            "llm.state_cache_first_token_sampled",
            "assistant.llm",
            "llm",
            json!({ "token": first_token.0 }),
        );

        let state_bytes = ctx.get_state_size();
        let mut state = vec![0_u8; state_bytes];
        let save_span = current_span("llm.state_cache_save", "assistant.llm", "llm");
        let saved_bytes = unsafe { ctx.copy_state_data(state.as_mut_ptr()) };
        save_span.finish(json!({ "state_bytes": state_bytes, "saved_bytes": saved_bytes }));
        if saved_bytes == 0 || saved_bytes > state_bytes {
            return Err(anyhow!(
                "llama.cpp copied an invalid state size: {saved_bytes} bytes into {state_bytes} byte buffer"
            ));
        }
        state.truncate(saved_bytes);

        let mut runs = Vec::with_capacity(iterations);
        for iteration in 0..iterations {
            let restore_started = Instant::now();
            let restore_span = current_span("llm.state_cache_restore", "assistant.llm", "llm");
            ctx.clear_kv_cache();
            let restored_bytes = unsafe { ctx.set_state_data(&state) };
            let state_restore_ms = restore_started.elapsed().as_millis() as u64;
            restore_span.finish(json!({
                "iteration": iteration + 1,
                "state_bytes": state.len(),
                "restored_bytes": restored_bytes,
                "elapsed_ms": state_restore_ms,
            }));
            if restored_bytes == 0 {
                return Err(anyhow!("llama.cpp failed to restore cached prompt state"));
            }

            let started = Instant::now();
            let mut first_token_ms = None;
            let mut delta_count = 0_usize;
            let generation = generate_from_prefilled_context(
                model,
                &mut ctx,
                tokens.len() as i32,
                last_prefill_idx,
                Some(first_token),
                |delta| {
                    if first_token_ms.is_none() {
                        first_token_ms = Some(started.elapsed().as_millis() as u64);
                    }
                    if delta.is_empty() {
                        return Ok(true);
                    }
                    delta_count = delta_count.saturating_add(1);
                    Ok(true)
                },
            )?;
            let output = generation.text.trim().to_string();
            let latency_ms = started.elapsed().as_millis() as u64;
            current_instant(
                "llm.state_cache_iteration_finished",
                "assistant.llm",
                "llm",
                json!({
                    "iteration": iteration + 1,
                    "latency_ms": latency_ms,
                    "time_to_first_token_ms": first_token_ms,
                    "deltas": delta_count,
                    "reply_chars": output.chars().count(),
                    "state_restore_ms": state_restore_ms,
                    "decode_elapsed_ms": generation.elapsed_ms,
                }),
            );
            runs.push(RawPromptStateCacheRun {
                iteration: iteration + 1,
                latency_ms,
                time_to_first_token_ms: first_token_ms,
                delta_count,
                output_chars: output.chars().count(),
                output,
                state_restore_ms,
                decode_elapsed_ms: generation.elapsed_ms,
            });
        }

        Ok(RawPromptStateCacheReport {
            prompt_tokens: tokens.len(),
            state_bytes: state.len(),
            setup_prefill_ms,
            runs,
        })
    }

    /// Build the stable F8 system + tool prompt-state checkpoints from a
    /// startup/idle warmup request. Each prompt is prefilled once and its
    /// llama.cpp state is stored in the in-memory LRU so a later hotkey press
    /// only pays the cheap restore. Missing or empty prompts are skipped.
    ///
    /// Only the F8 family is warmed: the live reply path restores the F8 base
    /// (via longest-prefix matching) and never the F7 cleanup base — F7 polish
    /// runs on a separate backend with its own cache, so the old `F7System`
    /// warmup on this backend was dead work and has been removed.
    fn build_stable_prompt_caches(&self, warmup: &AssistantPromptCacheWarmup) -> Result<()> {
        let guard = self.state.lock().map_err(|_| anyhow!("llama-local mutex poisoned"))?;
        let model = guard.as_ref().ok_or_else(|| anyhow!("llama-local model not loaded"))?;
        let model_name = self.model_path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        // The F8 system base is framed into the chat template so it is a true
        // token prefix of the live F8ChatPrefix prompt (mirrors the F7 base).
        if let Some(system) = warmup.f8_system_prompt.as_deref().filter(|s| !s.trim().is_empty()) {
            let base = assistant_base_prefix(system, model_name);
            self.build_prompt_prefix_cache(model, PromptStateCacheLayer::F8System, &base)?;
        }
        if let Some(tools) =
            warmup.assistant_tool_prompt.as_deref().filter(|s| !s.trim().is_empty())
        {
            self.build_prompt_prefix_cache(model, PromptStateCacheLayer::AssistantTools, tools)?;
        }
        Ok(())
    }

    /// Hotkey-time cache preparation. Ensures the stable F8 base checkpoint
    /// exists (building it framed into the chat template if startup warming
    /// hasn't run yet — a no-op cache hit when it has) so the live reply path
    /// can restore it via longest-prefix matching.
    ///
    /// Only the F8 trigger warms anything here: F7 polish runs on a separate
    /// backend, and the deprecated dynamic `WindowContext` checkpoint (the
    /// assistant no longer injects window context) was never restored, so both
    /// have been removed as confirmed-dead work.
    fn prepare_turn_prompt_caches(&self, snapshot: &AssistantPromptCacheSnapshot) -> Result<()> {
        if snapshot.trigger != AssistantCacheTrigger::F8 {
            return Ok(());
        }
        if snapshot.system_prompt.trim().is_empty() {
            return Ok(());
        }
        let guard = self.state.lock().map_err(|_| anyhow!("llama-local mutex poisoned"))?;
        let model = guard.as_ref().ok_or_else(|| anyhow!("llama-local model not loaded"))?;
        let model_name = self.model_path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        let base = assistant_base_prefix(&snapshot.system_prompt, model_name);
        self.build_prompt_prefix_cache(model, PromptStateCacheLayer::F8System, &base)?;
        Ok(())
    }
}

/// Emit a `llm.prompt_cache_cold_prefill` instant on the `cache` lane recording
/// why the prefix-cache fast path was abandoned in favour of a full prefill. The
/// dominance of these events on the F8 path is the evidence for the A.3 wiring
/// mismatch (the live reply path never restores the prewarmed base).
fn cold_prefill(layer: &str, reason: &str) {
    current_instant(
        "llm.prompt_cache_cold_prefill",
        "cache",
        CACHE_LANE,
        json!({ "layer": layer, "reason": reason }),
    );
}

fn copy_context_state(ctx: &LlamaContext<'_>) -> Result<Vec<u8>> {
    let state_bytes = ctx.get_state_size();
    let mut state = vec![0_u8; state_bytes];
    let saved_bytes = unsafe { ctx.copy_state_data(state.as_mut_ptr()) };
    if saved_bytes == 0 || saved_bytes > state_bytes {
        return Err(anyhow!(
            "llama.cpp copied an invalid state size: {saved_bytes} bytes into {state_bytes} byte buffer"
        ));
    }
    state.truncate(saved_bytes);
    Ok(state)
}

fn sha256_tokens(tokens: &[llama_cpp_2::token::LlamaToken]) -> String {
    let mut hasher = Sha256::new();
    for token in tokens {
        hasher.update(token.0.to_le_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Flatten llama tokens to their raw i32 ids for `PromptStateCacheEntry::with_tokens`
/// so cached checkpoints can participate in longest-prefix matching.
fn token_ids(tokens: &[llama_cpp_2::token::LlamaToken]) -> Vec<i32> {
    tokens.iter().map(|t| t.0).collect()
}

/// Length of the leading run where two token sequences agree by id. Used to
/// trim a post-generation checkpoint to the prefix the next turn reproduces
/// verbatim (sampled tokens can diverge from the canonical re-tokenization at
/// the reply/turn-closer boundary).
fn common_prefix_len(
    a: &[llama_cpp_2::token::LlamaToken],
    b: &[llama_cpp_2::token::LlamaToken],
) -> usize {
    a.iter().zip(b.iter()).take_while(|(x, y)| x.0 == y.0).count()
}

struct GenerationResult {
    text: String,
    elapsed_ms: u64,
    /// Content tokens actually decoded into the context KV, in order (the
    /// stop token is excluded — it breaks before being decoded). Lets the
    /// prefix-cache path checkpoint the *post-generation* state (system +
    /// history + this turn's user + reply) so the next turn can restore the
    /// completed exchange instead of re-prefilling it. See Option C in
    /// `plans/2026-06-09-f8-current-turn-double-count-cache-fix-v1.md`.
    tokens: Vec<llama_cpp_2::token::LlamaToken>,
}

fn generate_from_prefilled_context<F>(
    model: &LlamaModel,
    ctx: &mut LlamaContext<'_>,
    start_pos: i32,
    first_sample_idx: i32,
    first_token_override: Option<llama_cpp_2::token::LlamaToken>,
    mut on_delta: F,
) -> Result<GenerationResult>
where
    F: FnMut(String) -> Result<bool>,
{
    let mut sampler = LlamaSampler::greedy();
    let eos = model.token_eos();
    let end_of_turn = single_token(model, "<end_of_turn>");
    let im_end = single_token(model, "<|im_end|>");
    let mut out = String::new();
    let mut emitted_len = 0_usize;
    let mut sample_idx = first_sample_idx;
    let mut next_token = first_token_override;
    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let decode_started = Instant::now();
    let mut generated_tokens = 0_u32;
    let mut decoded_tokens: Vec<llama_cpp_2::token::LlamaToken> = Vec::new();
    let mut stop_reason = "max_tokens";
    let mut batch = LlamaBatch::new(1, 1);
    for n_cur in (start_pos..).take(MAX_NEW_TOKENS as usize) {
        let token = next_token.take().unwrap_or_else(|| sampler.sample(ctx, sample_idx));
        sampler.accept(token);
        if token == eos || Some(token) == end_of_turn || Some(token) == im_end {
            stop_reason = "eos";
            break;
        }
        let piece = model.token_to_piece(token, &mut decoder, false, None).unwrap_or_default();
        out.push_str(&piece);
        generated_tokens = generated_tokens.saturating_add(1);
        if let Some((stop_at, marker)) = first_stop_marker(&out) {
            if stop_at > emitted_len {
                let delta = out[emitted_len..stop_at].to_string();
                if !on_delta(delta)? {
                    stop_reason = "receiver_dropped";
                    break;
                }
            }
            out.truncate(stop_at);
            stop_reason = marker;
            break;
        }
        let safe_end = safe_stream_end(&out);
        if safe_end > emitted_len {
            let delta = out[emitted_len..safe_end].to_string();
            if !on_delta(delta)? {
                stop_reason = "receiver_dropped";
                break;
            }
            emitted_len = safe_end;
        }
        current_instant(
            "llm.decode_token",
            "assistant.llm",
            "llm",
            json!({
                "index": generated_tokens,
                "piece_chars": piece.chars().count(),
                "cumulative_chars": out.chars().count(),
            }),
        );
        batch.clear();
        batch.add(token, n_cur, &[0], true).context("decode batch.add")?;
        sample_idx = 0;
        ctx.decode(&mut batch).context("decode loop")?;
        decoded_tokens.push(token);
    }
    if stop_reason != "receiver_dropped" && emitted_len < out.len() {
        let delta = out[emitted_len..].to_string();
        if !on_delta(delta)? {
            stop_reason = "receiver_dropped";
        }
    }
    let elapsed_ms = decode_started.elapsed().as_millis() as u64;
    current_instant(
        "llm.decode_done",
        "assistant.llm",
        "llm",
        json!({
            "generated_tokens": generated_tokens,
            "reply_chars": out.chars().count(),
            "elapsed_ms": elapsed_ms,
            "tokens_per_second": if elapsed_ms == 0 {
                0.0
            } else {
                f64::from(generated_tokens) / (elapsed_ms as f64 / 1000.0)
            },
            "stop_reason": stop_reason,
        }),
    );
    Ok(GenerationResult { text: out, elapsed_ms, tokens: decoded_tokens })
}

fn single_token(model: &LlamaModel, text: &str) -> Option<llama_cpp_2::token::LlamaToken> {
    model
        .str_to_token(text, AddBos::Never)
        .ok()
        .filter(|v| v.len() == 1)
        .and_then(|v| v.into_iter().next())
}

fn first_stop_marker(text: &str) -> Option<(usize, &'static str)> {
    STOP_MARKERS
        .iter()
        .filter_map(|marker| text.find(marker).map(|idx| (idx, *marker)))
        .min_by_key(|(idx, _)| *idx)
}

fn safe_stream_end(text: &str) -> usize {
    let keep = STOP_MARKERS
        .iter()
        .filter_map(|marker| longest_marker_prefix_suffix(text, marker))
        .max()
        .unwrap_or(0);
    text.len().saturating_sub(keep)
}

fn longest_marker_prefix_suffix(text: &str, marker: &str) -> Option<usize> {
    let max = text.len().min(marker.len().saturating_sub(1));
    (1..=max).rev().find(|&len| text.ends_with(&marker[..len]))
}

/// Render the full reply prompt. Defined as the concatenation of the cache
/// split (`prefix + suffix`) so the two can never diverge — the prefix/suffix
/// split is the single source of truth for prompt layout.
fn build_prompt(ctx: &AssistantContext, user_text: &str, model_name: &str) -> String {
    let (prefix, suffix) = build_prompt_split(ctx, user_text, model_name);
    let mut s = prefix;
    s.push_str(&suffix);
    s
}

fn push_gemma_turn(buf: &mut String, role: &str, content: &str) {
    if content.trim().is_empty() {
        return;
    }
    buf.push_str("<start_of_turn>");
    buf.push_str(role);
    buf.push('\n');
    buf.push_str(content.trim());
    buf.push_str("<end_of_turn>\n");
}

/// Split the reply prompt into a stable prefix and a per-turn suffix for the
/// Task 8 prefix cache. The stable prefix is everything up to (but not
/// including) the variable user text; the suffix carries the user text plus the
/// closing template. By construction `format!("{prefix}{suffix}")` reproduces
/// [`build_prompt`] for the same inputs (asserted in tests), and the runtime
/// cache path re-checks that equality before trusting the split.
fn build_prompt_split(
    ctx: &AssistantContext,
    user_text: &str,
    model_name: &str,
) -> (String, String) {
    if model_name.to_ascii_lowercase().contains("gemma") {
        build_gemma_prompt_split(ctx, user_text)
    } else {
        build_chatml_prompt_split(ctx, user_text)
    }
}

fn build_gemma_prompt_split(ctx: &AssistantContext, user_text: &str) -> (String, String) {
    // Gemma has no dedicated system role, so the system prompt is prepended to
    // the FIRST user turn (Gemma's trained convention). This keeps the rendered
    // prompt strictly append-only: the leading tokens — system, then each
    // completed turn — never change as the conversation grows, so a boot-built
    // system checkpoint and a per-conversation checkpoint both stay valid as
    // token-prefixes turn after turn. Anything volatile (the current user text)
    // lives only in the trailing suffix.
    let system = ctx.system_prompt.trim();
    let mut prefix = String::new();
    let mut system_emitted = false;

    for turn in &ctx.history {
        match turn.role {
            ChatRole::User | ChatRole::System => {
                let content = turn.content.trim();
                if content.is_empty() {
                    continue;
                }
                if !system_emitted && !system.is_empty() {
                    push_gemma_turn(&mut prefix, "user", &format!("{system}\n\n{content}"));
                    system_emitted = true;
                } else {
                    push_gemma_turn(&mut prefix, "user", content);
                }
            }
            ChatRole::Assistant => push_gemma_turn(&mut prefix, "model", &turn.content),
            ChatRole::Tool => {}
        }
    }

    prefix.push_str("<start_of_turn>user\n");
    if !system_emitted && !system.is_empty() {
        // No prior user turn carried the system prompt, so it leads the current
        // turn. It stays in the (cacheable) prefix; only the user text varies.
        prefix.push_str(system);
        prefix.push_str("\n\n");
    }
    let suffix = format!("{}<end_of_turn>\n<start_of_turn>model\n", user_text.trim());
    (prefix, suffix)
}

fn build_chatml_prompt_split(ctx: &AssistantContext, user_text: &str) -> (String, String) {
    let mut prefix = String::new();
    if !ctx.system_prompt.trim().is_empty() {
        prefix.push_str("<|im_start|>system\n");
        prefix.push_str(ctx.system_prompt.trim());
        prefix.push_str("<|im_end|>\n");
    }
    for turn in &ctx.history {
        let role = match turn.role {
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
            ChatRole::System => "system",
            ChatRole::Tool => continue,
        };
        if turn.content.trim().is_empty() {
            continue;
        }
        prefix.push_str("<|im_start|>");
        prefix.push_str(role);
        prefix.push('\n');
        prefix.push_str(turn.content.trim());
        prefix.push_str("<|im_end|>\n");
    }
    prefix.push_str("<|im_start|>user\n");
    let suffix = format!("{}<|im_end|>\n<|im_start|>assistant\n", user_text.trim());
    (prefix, suffix)
}

/// The context-independent F8 base: the system prompt wrapped in the model's
/// chat framing up to (but not including) any variable content. By construction
/// it is a genuine textual prefix of the live [`build_prompt_split`] prefix for
/// any history (the system block always leads — asserted in tests), so the
/// pinned base checkpoint can be restored via longest-prefix matching and only
/// the per-turn remainder decoded instead of cold-prefilling from scratch.
/// Empty when there is no system prompt. Mirrors the F7 polish `chatml_base_prefix`.
fn assistant_base_prefix(system: &str, model_name: &str) -> String {
    let system = system.trim();
    if system.is_empty() {
        return String::new();
    }
    if model_name.to_ascii_lowercase().contains("gemma") {
        format!("<start_of_turn>user\n{system}")
    } else {
        format!("<|im_start|>system\n{system}")
    }
}

#[async_trait]
impl Assistant for LlamaLocalAssistant {
    async fn reply_stream(
        &self,
        user_text: &str,
        ctx: &AssistantContext,
    ) -> Result<BoxStream<'static, Result<TokenDelta>>> {
        let model_name = self.model_path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        let prompt = build_prompt(ctx, user_text, model_name);
        let (cache_prefix, cache_suffix) = build_prompt_split(ctx, user_text, model_name);
        current_instant(
            "llm.prompt_built",
            "assistant.llm",
            "llm",
            json!({
                "model": model_name,
                "prompt_chars": prompt.chars().count(),
                "history_turns": ctx.history.len(),
                "system_prompt_chars": ctx.system_prompt.chars().count(),
                "user_chars": user_text.chars().count(),
                "history_chars": ctx.history.iter().map(|t| t.content.chars().count()).sum::<usize>(),
                "prompt_sha256": sha256_text(&prompt),
                "prompt": prompt_for_trace(&prompt),
            }),
        );
        let me = self.clone_thin();
        let started = Instant::now();
        let (tx, rx) = mpsc::channel::<Result<TokenDelta>>(STREAM_CHANNEL_CAPACITY);
        tokio::task::spawn_blocking(move || {
            let total_span = current_span("llm.local_streaming_inference", "assistant.llm", "llm");
            let mut deltas_emitted = 0_u32;
            let result = (|| -> Result<String> {
                me.ensure_loaded()?;
                me.run_inference_with_prefix_cache(
                    &prompt,
                    &cache_prefix,
                    &cache_suffix,
                    PromptStateCacheLayer::F8ChatPrefix,
                    |delta| {
                        let delta = delta.trim_start_matches('\u{feff}').to_string();
                        if delta.is_empty() {
                            return Ok(true);
                        }
                        deltas_emitted = deltas_emitted.saturating_add(1);
                        Ok(tx.blocking_send(Ok(TokenDelta::text(delta))).is_ok())
                    },
                )
            })();
            let elapsed_ms = started.elapsed().as_millis() as u64;
            match result {
                Ok(text) => {
                    total_span.finish(json!({
                        "reply_chars": text.chars().count(),
                        "deltas": deltas_emitted,
                        "elapsed_ms": elapsed_ms,
                    }));
                    if elapsed_ms > 5_000 {
                        warn!(
                            elapsed_ms,
                            deltas = deltas_emitted,
                            "local assistant took {} ms",
                            elapsed_ms
                        );
                    } else {
                        debug!(elapsed_ms, deltas = deltas_emitted, "local assistant ok");
                    }
                    current_instant(
                        "llm.local_stream_finished",
                        "assistant.llm",
                        "llm",
                        json!({
                            "elapsed_ms": elapsed_ms,
                            "reply_chars": text.chars().count(),
                            "deltas": deltas_emitted,
                        }),
                    );
                }
                Err(e) => {
                    total_span.finish(json!({
                        "error": e.to_string(),
                        "deltas": deltas_emitted,
                        "elapsed_ms": elapsed_ms,
                    }));
                    let _ = tx.blocking_send(Err(e));
                }
            }
        });
        current_instant(
            "llm.local_stream_started",
            "assistant.llm",
            "llm",
            json!({ "channel_capacity": STREAM_CHANNEL_CAPACITY }),
        );
        Ok(ReceiverStream::new(rx).boxed())
    }

    fn name(&self) -> &'static str {
        "llama-local-assistant"
    }

    async fn prewarm(&self) -> Result<()> {
        let me = self.clone_thin();
        tokio::task::spawn_blocking(move || me.ensure_loaded())
            .await
            .context("local assistant prewarm join")?
    }

    /// Startup/idle warmup of the stable F7/F8/tool prompt checkpoints. Runs on
    /// a blocking thread so the prefill never blocks the async runtime; the
    /// daemon already defers this call so it doesn't compete with first-launch
    /// work (plan task 4).
    async fn prewarm_prompt_caches(&self, warmup: AssistantPromptCacheWarmup) -> Result<()> {
        let me = self.clone_thin();
        tokio::task::spawn_blocking(move || -> Result<()> {
            me.ensure_loaded()?;
            me.build_stable_prompt_caches(&warmup)
        })
        .await
        .context("local assistant prompt-cache prewarm join")?
    }

    /// Hotkey-time prompt-state cache preparation (plan tasks 5–7/9). Runs on a
    /// blocking thread; the stable checkpoint restore is cheap and the dynamic
    /// window-context checkpoint is rebuilt only when context is present.
    async fn prepare_prompt_cache_for_turn(
        &self,
        snapshot: AssistantPromptCacheSnapshot,
    ) -> Result<()> {
        let me = self.clone_thin();
        tokio::task::spawn_blocking(move || -> Result<()> {
            me.ensure_loaded()?;
            me.prepare_turn_prompt_caches(&snapshot)
        })
        .await
        .context("local assistant prompt-cache prepare join")?
    }
}

fn num_threads() -> i32 {
    std::thread::available_parallelism().map(|n| i32::try_from(n.get()).unwrap_or(4)).unwrap_or(4)
}

fn sha256_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

fn prompt_for_trace(prompt: &str) -> Option<&str> {
    match std::env::var("FONO_ASSISTANT_TRACE_PROMPT") {
        Ok(v) if env_bool(&v) == Some(false) => return None,
        Ok(v) if env_bool(&v) == Some(true) => return Some(prompt),
        _ => {}
    }
    match std::env::var("FONO_ASSISTANT_TRACE") {
        Ok(v) if env_bool(&v) != Some(false) && !v.trim().is_empty() => Some(prompt),
        _ => None,
    }
}

fn env_bool(value: &str) -> Option<bool> {
    let value = value.trim();
    if value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes") {
        Some(true)
    } else if value.is_empty()
        || value == "0"
        || value.eq_ignore_ascii_case("false")
        || value.eq_ignore_ascii_case("no")
    {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::ChatTurn;

    #[test]
    fn gemma_prompt_uses_gemma_turn_markers() {
        let ctx =
            AssistantContext { system_prompt: "Be concise.".into(), ..AssistantContext::default() };
        let p = build_prompt(&ctx, "hello", "gemma-4-e2b-it-Q4_K_M");
        assert!(p.contains("<start_of_turn>user\nBe concise."));
        assert!(p.ends_with("<start_of_turn>model\n"));
        assert!(!p.contains("<|im_start|>"));
    }

    #[test]
    fn chatml_prompt_keeps_non_gemma_fallback() {
        let ctx =
            AssistantContext { system_prompt: "Be concise.".into(), ..AssistantContext::default() };
        let p = build_prompt(&ctx, "hello", "qwen3.5-0.8b");
        assert!(p.contains("<|im_start|>system\nBe concise.<|im_end|>"));
        assert!(p.ends_with("<|im_start|>assistant\n"));
    }

    fn turn(role: ChatRole, content: &str) -> ChatTurn {
        ChatTurn {
            role,
            content: content.to_string(),
            at: Instant::now(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    #[test]
    fn prompt_split_reproduces_full_prompt() {
        let cases = [
            ("gemma-4-e2b-it-Q4_K_M", "Be concise.", " spaced user text "),
            ("gemma-4-e2b-it-Q4_K_M", "", "no system here"),
            ("qwen3.5-0.8b", "Be concise.", " hello world "),
            ("qwen3.5-0.8b", "", "no system chatml"),
        ];
        for (model, system, user) in cases {
            let ctx = AssistantContext {
                system_prompt: system.into(),
                history: vec![
                    turn(ChatRole::User, "first question"),
                    turn(ChatRole::Assistant, "first answer"),
                ],
                ..AssistantContext::default()
            };
            let full = build_prompt(&ctx, user, model);
            let (prefix, suffix) = build_prompt_split(&ctx, user, model);
            assert_eq!(
                format!("{prefix}{suffix}"),
                full,
                "split must reproduce the full prompt for model={model:?} system={system:?}"
            );
            assert!(!prefix.is_empty(), "prefix should be non-empty for {model:?}");
            assert!(!suffix.is_empty(), "suffix should be non-empty for {model:?}");
        }
    }

    #[test]
    fn assistant_base_prefix_leads_chat_prefix() {
        // Workstream C: the prewarmed F8System base must be a textual prefix of
        // the live F8ChatPrefix prompt prefix for any history, otherwise
        // `find_longest_prefix` can never restore it and every turn cold-prefills
        // from scratch. (Token-level matching is guarded at runtime by
        // `full_tokens.starts_with(prefix)`; this asserts the string-level
        // invariant a prompt-layout change would break.) Mirrors the F7 polish
        // `base_prefix_is_textual_prefix_of_full_prefix` test.
        let system = "You are Fono, a terse assistant.";
        for model in ["gemma-4-e2b-it-Q4_K_M", "qwen3.5-0.8b"] {
            let base = assistant_base_prefix(system, model);
            assert!(!base.is_empty(), "base should be non-empty for {model:?}");

            // No history: the base leads the current-turn prefix.
            let no_history =
                AssistantContext { system_prompt: system.into(), ..AssistantContext::default() };
            let (prefix, _) = build_prompt_split(&no_history, "hello", model);
            assert!(
                prefix.starts_with(&base),
                "base must lead the chat prefix (no history) for {model:?}\n base: {base:?}\n prefix: {prefix:?}"
            );

            // With history: the system block still leads, so the base is still a
            // prefix and remains restorable as the conversation grows.
            let with_history = AssistantContext {
                system_prompt: system.into(),
                history: vec![
                    turn(ChatRole::User, "first question"),
                    turn(ChatRole::Assistant, "first answer"),
                ],
                ..AssistantContext::default()
            };
            let (prefix2, _) = build_prompt_split(&with_history, "again", model);
            assert!(
                prefix2.starts_with(&base),
                "base must lead the chat prefix (with history) for {model:?}\n base: {base:?}\n prefix: {prefix2:?}"
            );
        }

        // Empty system prompt -> empty base (nothing to pin).
        assert!(assistant_base_prefix("   ", "gemma-4-e2b-it").is_empty());
    }

    #[test]
    fn gemma_split_keeps_system_in_prefix() {
        let ctx = AssistantContext {
            system_prompt: "You are a helpful assistant.".into(),
            ..AssistantContext::default()
        };
        let (prefix, suffix) = build_prompt_split(&ctx, "what time is it", "gemma-4-e2b-it");
        // System leads the prompt and stays entirely in the cacheable prefix;
        // only the variable user text lands in the suffix.
        assert!(prefix.starts_with("<start_of_turn>user\nYou are a helpful assistant.\n\n"));
        assert!(prefix.ends_with("\n\n"));
        assert!(!suffix.contains("You are a helpful assistant."));
        assert_eq!(suffix, "what time is it<end_of_turn>\n<start_of_turn>model\n");
    }

    #[test]
    fn gemma_system_leads_prompt_regardless_of_history() {
        // The whole cache scheme rests on the system prompt being a *leading*
        // token prefix. If a refactor ever pushes it back into the per-turn
        // tail (the old, un-cacheable layout), this fails loudly.
        let boot = "<start_of_turn>user\nYou are Fono.\n\n";
        let no_history = AssistantContext {
            system_prompt: "You are Fono.".into(),
            ..AssistantContext::default()
        };
        let with_history = AssistantContext {
            system_prompt: "You are Fono.".into(),
            history: vec![
                turn(ChatRole::User, "turn one"),
                turn(ChatRole::Assistant, "reply one"),
                turn(ChatRole::User, "turn two"),
                turn(ChatRole::Assistant, "reply two"),
            ],
            ..AssistantContext::default()
        };
        for ctx in [&no_history, &with_history] {
            let full = build_prompt(ctx, "current question", "gemma-4-e2b-it");
            assert!(
                full.starts_with(boot),
                "boot system prefix must lead every gemma prompt; got: {}",
                &full[..boot.len().min(full.len())]
            );
        }
        // The system prompt must appear exactly once even with history.
        let full = build_prompt(&with_history, "current question", "gemma-4-e2b-it");
        assert_eq!(full.matches("You are Fono.").count(), 1);
    }

    #[test]
    fn gemma_conversation_is_append_only() {
        // Append-only invariant: each turn's full prompt must be an exact string
        // prefix of the next turn's prompt. The only thing the model appends
        // between turns is its own reply plus the next turn's framing — nothing
        // earlier is ever rewritten. This is the property that makes the KV
        // prefix cache reusable across a multi-turn Gemma conversation; if it
        // breaks, the cache silently degrades to full prefills every turn.
        let model = "gemma-4-e2b-it";
        let system = "You are Fono, a terse assistant.";
        let exchanges = [
            ("turn the lights on", "Done."),
            ("now dim them to fifty percent", "Dimmed to 50%."),
            ("what's the time", "It is 4pm."),
        ];

        let mut history: Vec<ChatTurn> = Vec::new();
        let mut prev_prompt: Option<String> = None;
        for (user, assistant) in exchanges {
            let ctx = AssistantContext {
                system_prompt: system.into(),
                history: history.clone(),
                ..AssistantContext::default()
            };
            let prompt = build_prompt(&ctx, user, model);
            if let Some(prev) = &prev_prompt {
                assert!(
                    prompt.starts_with(prev),
                    "turn prompt must extend the previous turn's prompt verbatim.\nprev:\n{prev}\nnext:\n{prompt}"
                );
            }
            // Advance the rolling history exactly as the daemon would.
            history.push(turn(ChatRole::User, user));
            history.push(turn(ChatRole::Assistant, assistant));
            prev_prompt = Some(prompt);
        }
    }

    #[test]
    fn chatml_conversation_is_append_only() {
        let model = "qwen3.5-0.8b";
        let system = "You are Fono.";
        let exchanges = [("first", "one"), ("second", "two"), ("third", "three")];
        let mut history: Vec<ChatTurn> = Vec::new();
        let mut prev_prompt: Option<String> = None;
        for (user, assistant) in exchanges {
            let ctx = AssistantContext {
                system_prompt: system.into(),
                history: history.clone(),
                ..AssistantContext::default()
            };
            let prompt = build_prompt(&ctx, user, model);
            if let Some(prev) = &prev_prompt {
                assert!(prompt.starts_with(prev), "chatml prompt must be append-only");
            }
            history.push(turn(ChatRole::User, user));
            history.push(turn(ChatRole::Assistant, assistant));
            prev_prompt = Some(prompt);
        }
    }

    #[test]
    fn common_prefix_len_stops_at_first_divergent_token() {
        use llama_cpp_2::token::LlamaToken as T;
        let tok = |ids: &[i32]| ids.iter().map(|i| T(*i)).collect::<Vec<_>>();
        // Identical sequences: full overlap.
        assert_eq!(common_prefix_len(&tok(&[1, 2, 3]), &tok(&[1, 2, 3])), 3);
        // Divergence at the last token (the reply/closer merge case): the
        // shared run is everything up to the divergent tail, which is exactly
        // what the completed-turn checkpoint must store.
        assert_eq!(common_prefix_len(&tok(&[1, 2, 3, 99]), &tok(&[1, 2, 3, 4, 5])), 3);
        // One is a strict prefix of the other.
        assert_eq!(common_prefix_len(&tok(&[1, 2]), &tok(&[1, 2, 3, 4])), 2);
        // Immediate divergence / empty input.
        assert_eq!(common_prefix_len(&tok(&[9]), &tok(&[1])), 0);
        assert_eq!(common_prefix_len(&tok(&[]), &tok(&[1, 2])), 0);
    }

    #[test]
    fn gemma_history_render_is_stable_across_turns() {
        // A turn that has scrolled into history must render byte-for-byte the
        // same as it did when it was the current turn (modulo the appended
        // model reply). This is what guarantees the cached KV for turn N is a
        // valid prefix for turn N+1.
        let model = "gemma-4-e2b-it";
        let system = "You are Fono.";

        // Turn 1: empty history, "hello" is the current user text.
        let ctx1 = AssistantContext { system_prompt: system.into(), ..AssistantContext::default() };
        let prompt1 = build_prompt(&ctx1, "hello", model);

        // Turn 2: "hello"/"hi" now in history.
        let ctx2 = AssistantContext {
            system_prompt: system.into(),
            history: vec![turn(ChatRole::User, "hello"), turn(ChatRole::Assistant, "hi")],
            ..AssistantContext::default()
        };
        let prompt2 = build_prompt(&ctx2, "again", model);

        // Everything prompt1 emitted up to the model-open tag is reproduced
        // verbatim at the head of prompt2.
        assert!(prompt2.starts_with(&prompt1), "history render drifted between turns");
    }

    #[test]
    fn cached_prefix_nests_across_turns_under_daemon_flow() {
        // Regression for the current-turn double-count bug (2026-06-09): the
        // daemon snapshots COMPLETED history (excluding the in-flight user turn)
        // and passes the current turn as `user_text`
        // (`crates/fono/src/assistant.rs`). Under that contract every turn's
        // cache prefix must be a string-prefix of the next turn's prefix, so
        // `find_longest_prefix` can restore the prior checkpoint and prefill
        // only the new exchange. The old bug pushed the user turn into
        // `ctx.history` *before* snapshotting, so the prefix ended in a volatile
        // `<start_of_turn>user\n` marker that the next turn overwrote with the
        // model reply (`<start_of_turn>model\n...`) — nesting broke and only the
        // static system base could ever be restored. This test reproduces the
        // exact push/snapshot ordering and is model-free (the divergence is
        // structural, visible at the string level).
        use crate::history::ConversationHistory;
        let exchanges = [
            ("what's the weather", "I can't check live weather."),
            ("set a timer for ten minutes", "Timer set."),
            ("make it fifteen", "Updated to fifteen minutes."),
        ];
        for model in ["gemma-4-e2b-it", "qwen3.5-0.8b"] {
            let system = "You are a concise voice assistant.";
            let mut hist = ConversationHistory::default();
            let mut prev_prefix: Option<String> = None;
            for (user, assistant) in exchanges {
                // Daemon order: snapshot COMPLETED history, then record the user.
                let snapshot = hist.snapshot();
                hist.push_user((*user).to_string());
                let ctx = AssistantContext {
                    system_prompt: system.into(),
                    history: snapshot,
                    ..AssistantContext::default()
                };
                let (prefix, _suffix) = build_prompt_split(&ctx, user, model);
                if let Some(prev) = &prev_prefix {
                    assert!(
                        prefix.starts_with(prev),
                        "cache prefix must extend the previous turn's prefix for {model:?}.\nprev: {prev:?}\nnext: {prefix:?}"
                    );
                }
                // The in-flight user text lives only in the suffix, never the
                // cached prefix — asserts the double-count is gone.
                assert!(
                    !prefix.contains(user),
                    "current user text leaked into the cached prefix for {model:?}: {prefix:?}"
                );
                prev_prefix = Some(prefix);
                hist.push_assistant((*assistant).to_string());
            }
        }
    }

    #[test]
    fn stop_marker_helpers_strip_control_text() {
        let text = "Paris<end_of_turn>\n<start_of_turn>user\nAgain";
        assert_eq!(first_stop_marker(text), Some((5, "<end_of_turn>")));
        assert_eq!(safe_stream_end("Paris<end"), 5);
        assert_eq!(safe_stream_end("Paris"), 5);
    }

    #[test]
    fn missing_model_path_errors_clearly() {
        let m = LlamaLocalAssistant::new("/this/path/does/not/exist.gguf", 1024);
        let e = m.ensure_loaded().unwrap_err().to_string();
        assert!(e.contains("local assistant model not found"), "got: {e}");
    }

    #[tokio::test]
    #[ignore = "requires FONO_TEST_ASSISTANT_GGUF=/path/to/chat-model.gguf"]
    async fn local_assistant_smoke_generates_reply() {
        use futures::StreamExt;

        let model_path = std::env::var_os("FONO_TEST_ASSISTANT_GGUF")
            .expect("set FONO_TEST_ASSISTANT_GGUF=/path/to/chat-model.gguf");
        let context = std::env::var("FONO_TEST_ASSISTANT_CTX")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(4096);
        let assistant = LlamaLocalAssistant::with_threads(model_path, context, 2);
        let ctx = AssistantContext {
            system_prompt: "Reply with exactly one short sentence.".into(),
            ..AssistantContext::default()
        };
        let mut stream = assistant
            .reply_stream("Say hello from the local assistant.", &ctx)
            .await
            .expect("local assistant reply_stream");
        let mut text = String::new();
        while let Some(delta) = stream.next().await {
            text.push_str(&delta.expect("local assistant token delta").text);
        }
        assert!(!text.trim().is_empty(), "local assistant returned an empty reply");
    }
}
