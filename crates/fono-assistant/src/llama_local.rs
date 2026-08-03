// SPDX-License-Identifier: GPL-3.0-only
//! Embedded llama.cpp assistant backend for local GGUF chat models.
//!
//! This is the default path for the wizard's `local` assistant. The
//! OpenAI-compatible/Ollama client remains available when the user manually
//! configures an explicit local server URL.

#![allow(clippy::significant_drop_tightening)]

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use fono_core::brain_tap::{decode_token_with_tap, BrainTap};
use fono_core::llama_backend::{backend, shared_model, streaming_model_params};
use fono_core::llama_gen::{
    adopt_sampled_token, first_stop_marker, generation_sampler, generation_sampler_with_grammar,
    is_control_token, ruled_out, safe_stream_end, sample_next, turn_markers,
    warn_on_template_vocab_mismatch, TurnMarkers,
};
use fono_core::tool_grammar::trigger_patterns;
use fono_core::turn_trace::{
    current_instant, current_span, generation_span_args, record_cache_mutation, CACHE_LANE,
};
use futures::stream::{BoxStream, StreamExt};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, info, warn};

use crate::history::{ChatRole, ChatTurn, ToolCall};
use crate::local_tools;
use crate::traits::{
    Assistant, AssistantCacheTrigger, AssistantContext, AssistantPromptCacheSnapshot,
    AssistantPromptCacheWarmup, TokenDelta, ToolEvent,
};

const MAX_NEW_TOKENS: i32 = 384;
const MIN_CTX: u32 = 512;

/// Per-request generation knobs threaded into the prefix-cache decode path.
/// Bundled so the cache fns stay under clippy's argument limit and the two
/// flags travel together.
///
/// `Clone` rather than `Copy` since the rails are a shared string: the wording
/// pass after a tool call reuses the same knobs with one field changed.
#[derive(Clone)]
struct GenParams {
    /// Cap on generated tokens (already clamped to [`MAX_NEW_TOKENS`]).
    max_new_tokens: i32,
    /// This request's cached prefix is the *static head* — system prompt, area
    /// and device names, tool catalogue — with no conversation in front of it,
    /// so it is byte-identical on every later turn and in every later
    /// conversation. True for a turn with empty history and for the
    /// `fono.summarize` path; false once a conversation has history, and for
    /// the wording pass after a tool call.
    ///
    /// The prefix is checkpointed either way — see the store site in
    /// [`Self::generate_with_prefix_cache`] — but only the static head is
    /// **pinned**, because only it is worth protecting from eviction forever.
    pin_prefix: bool,
    /// Whether this request's cached prefix is still worth having once the turn
    /// is over.
    ///
    /// True for the prefix that ends where the user's words begin: the next turn
    /// starts with the same system prompt and the same history, so a checkpoint
    /// of it saves that turn the whole read. False for the wording pass after a
    /// tool call, whose prefix contains this turn's own request, call and result
    /// — text no later turn reproduces.
    ///
    /// The distinction has to be made because the two are filed together
    /// otherwise, and the longer one deletes the shorter: an insert drops any
    /// same-layer entry it strictly contains, which is right for prefix
    /// matching and wrong here, because the shorter entry is the one the next
    /// turn asks for by name. Measured cost of getting this wrong: every turn
    /// of a 22-command run re-read 1592 tokens it had read moments before, 40 s
    /// each, while the surviving checkpoint was a mid-turn one nothing could
    /// use.
    prefix_outlives_turn: bool,
    /// Whether this turn may drive the Glas Cortex tap. Carried from
    /// [`AssistantContext::allow_brain_capture`]; `false` for network turns
    /// so a remote client sharing this backend never lights the local
    /// overlay. Opens the backend's `capture_gate` for the duration of the
    /// turn.
    allow_capture: bool,
    /// The rails the model is held to once it starts writing a command, from
    /// [`ActionTools::grammar`]. `None` leaves sampling exactly as it is
    /// without the setting, which is what makes the two comparable.
    ///
    /// Carried per request rather than per backend because it describes the
    /// tools offered *this turn* — and because it must be absent on the turns
    /// where no tools are offered at all.
    grammar: Option<Arc<str>>,
    /// Where the steady part of this prompt ends: the greeting, the areas, the
    /// devices and the tool catalogue, framed for the model and stopping short
    /// of anything that changes from turn to turn.
    ///
    /// A checkpoint is usable only when its whole token list is a prefix of the
    /// new prompt, so a checkpoint of a prompt carrying "Reply in Romanian." is
    /// worthless to the English turn after it and to a turn with a different
    /// speaker. Naming the boundary lets the cold read stop there, keep what it
    /// has, and read on — after which every language, every speaker and every
    /// note costs the handful of tokens it is written in rather than the whole
    /// house. Measured on the command benchmark before this existed: two
    /// languages, 1579 tokens each, 39 s and 35 s, for prompts identical but
    /// for four words at the end.
    ///
    /// `None` where no such boundary is known, which leaves the read exactly as
    /// it was.
    steady_head: Option<Arc<str>>,
}

/// RAII guard that closes the backend's brain-capture gate when a
/// generation ends. Set the gate open (or not) at the start of a turn and
/// hold one of these for the turn's body; on any exit path — success,
/// `?`-propagated error, or panic — the gate falls back to closed so no
/// later prewarm/diagnostic decode accidentally captures.
struct CaptureGateGuard<'a>(&'a AtomicBool);
impl Drop for CaptureGateGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

const DEFAULT_BATCH_SIZE: u32 = 2048;
const DEFAULT_UBATCH_SIZE: u32 = 512;
const STREAM_CHANNEL_CAPACITY: usize = 32;
// Sampler, stop predicate, and textual stop-marker scan are the shared
// generation policy in `fono_core::llama_gen` (one definition for the
// polish + assistant embedded backends — see that module's docs for the
// gemma-4-e2b control-token evidence and the repetition-loop rationale).

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
    /// Glass Cortex capture (opt-in, default off). Created lazily in
    /// [`Self::ensure_loaded`] once the model's layer count is known;
    /// shared via `Arc` so `clone_thin` workers and the overlay-side
    /// consumer see one tap. See `fono_core::brain_tap`.
    brain_tap_enabled: bool,
    brain_tap: Arc<OnceLock<Arc<BrainTap>>>,
    /// Per-generation latch gating whether the tap actually captures.
    /// Set (under the model lock, so it can't race a concurrent turn) at
    /// the top of the reply path from [`AssistantContext::allow_brain_capture`]
    /// and reset when the turn ends. Shared across `clone_thin` workers via
    /// `Arc`. Keeps network-driven turns (the shared LLM server) from
    /// lighting the local overlay while local hotkey turns still do.
    capture_gate: Arc<AtomicBool>,
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
            brain_tap_enabled: false,
            brain_tap: Arc::new(OnceLock::new()),
            capture_gate: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Opt in to Glass Cortex keyframe capture (the brain-visualization
    /// tap; default off — with it off the decode path carries zero tap
    /// code, not even a null-callback install).
    #[must_use]
    pub fn with_brain_tap(mut self, enabled: bool) -> Self {
        self.brain_tap_enabled = enabled;
        self
    }

    /// The shared tap handle, once the model has loaded with capture
    /// enabled. The overlay-side consumer drains keyframes from here.
    #[must_use]
    pub fn brain_tap(&self) -> Option<Arc<BrainTap>> {
        if self.brain_tap_enabled {
            self.brain_tap.get().cloned()
        } else {
            None
        }
    }

    /// Internal accessor used by the decode paths. Returns the tap only
    /// when capture is enabled *and* the per-generation gate is open (a
    /// local, overlay-visible turn) — so network-driven turns sharing this
    /// backend never arm the tap or publish overlay events.
    fn tap(&self) -> Option<&Arc<BrainTap>> {
        if self.brain_tap_enabled && self.capture_gate.load(Ordering::Relaxed) {
            self.brain_tap.get()
        } else {
            None
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
            brain_tap_enabled: self.brain_tap_enabled,
            brain_tap: Arc::clone(&self.brain_tap),
            capture_gate: Arc::clone(&self.capture_gate),
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
        //
        // The assistant role uses the explicit streaming params (mmap on, mlock
        // off, CPU) so a selected larger-than-RAM asym MoE pages in from SSD
        // instead of being copied resident — the mechanism behind Win #1. For
        // the small dense default this is behaviourally identical to
        // `default()`; the differing params also key a *separate* shared-model
        // entry from polish's `default()` load of the same file, which
        // is correct — the two roles want different residency for big MoEs.
        let model = shared_model(&self.model_path, &streaming_model_params())?;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let model_name = self.model_path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
        // Load-time tripwire: warn when the selected hand-rolled template's
        // markers do not resolve to control tokens in this vocabulary (the
        // gemma-4-e2b anomaly) or the name matches no known family.
        warn_on_template_vocab_mismatch(&model, model_name);
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
        if self.brain_tap_enabled {
            let n_layer = guard.as_ref().map_or(0, |m| m.n_layer());
            let (n_expert, n_expert_used) =
                guard.as_ref().map_or((0, 0), |m| fono_core::brain_tap::model_expert_counts(m));
            let tap = self
                .brain_tap
                .get_or_init(|| Arc::new(BrainTap::new(n_layer, n_expert, n_expert_used)));
            debug!("brain tap ready: {} layers, interval {}", tap.n_layer(), tap.interval());
        }
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

    /// Read as far as the end of the steady head, checkpoint there, and report
    /// how many tokens that was.
    ///
    /// A checkpoint is usable only when its whole token list is a prefix of the
    /// new prompt, so one taken of a prompt ending "Reply in Romanian." is worth
    /// nothing to the English turn after it — and the two prompts are otherwise
    /// the same 1579 tokens of greeting, areas, devices and tools. Stopping at
    /// the boundary the note has yet to cross gives every language, every
    /// speaker and every note the same checkpoint to start from.
    ///
    /// The entry goes under `F8System`, pinned, keyed exactly as the startup
    /// warm keys it, so the two agree on one occupant instead of evicting each
    /// other.
    ///
    /// `Ok(None)` where there is no boundary to stop at, where the head is not
    /// a token prefix of this prompt after all, or where the whole prefix *is*
    /// the head and the ordinary store below already pins it. The caller then
    /// reads on exactly as it did before.
    fn checkpoint_steady_head(
        &self,
        model: &LlamaModel,
        ctx: &mut LlamaContext<'_>,
        prefix_tokens: &[llama_cpp_2::token::LlamaToken],
        params: &GenParams,
        start: usize,
    ) -> Result<Option<usize>> {
        let Some(head) = params.steady_head.as_deref().map(str::trim_end).filter(|h| !h.is_empty())
        else {
            return Ok(None);
        };
        let head_tokens =
            model.str_to_token(head, AddBos::Always).context("tokenize steady head")?;
        if head_tokens.is_empty() || head_tokens.len() >= prefix_tokens.len() {
            return Ok(None);
        }
        if !prefix_tokens.starts_with(&head_tokens) {
            debug!(
                head_tokens = head_tokens.len(),
                prefix_tokens = prefix_tokens.len(),
                "steady head is not a token prefix of this prompt; reading straight through"
            );
            return Ok(None);
        }
        // Something deeper is already restored, so the head is behind us and
        // whatever supplied it is at least as good as this checkpoint.
        if start >= head_tokens.len() {
            return Ok(None);
        }
        self.prefill_tokens(
            ctx,
            &prefix_tokens[start..head_tokens.len()],
            start as i32,
            false,
            "llm.prompt_cache_head_prefill",
        )?;
        let Some(key) =
            self.prompt_state_cache_key(PromptStateCacheLayer::F8System, head, &head_tokens).ok()
        else {
            return Ok(Some(head_tokens.len()));
        };
        if let Ok(state) = copy_context_state(ctx) {
            let state_bytes = state.len();
            if let Ok(mut cache) = self.prompt_state_cache.lock() {
                let entry = PromptStateCacheEntry::with_tokens(state, token_ids(&head_tokens[..]));
                record_cache_mutation(&cache.insert_pinned(key.clone(), entry));
            }
            current_instant(
                "llm.prompt_cache_head_stored",
                "cache",
                CACHE_LANE,
                json!({
                    "layer": key.layer().as_str(),
                    "cache_key": key.stable_id(),
                    "pinned": true,
                    "token_count": head_tokens.len(),
                    "prefix_tokens": prefix_tokens.len(),
                    "state_bytes": state_bytes,
                }),
            );
        }
        Ok(Some(head_tokens.len()))
    }

    /// Generation-time prefix cache. Restores a cached prefix checkpoint
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
        params: GenParams,
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
        if full_tokens.len() + params.max_new_tokens.max(0) as usize >= self.context_size as usize {
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
        // Where the pinned static head lives (see `GenParams::pin_prefix`). It is
        // asked for by key as well as by prefix search because a repeat of the
        // *same* head — the first turn of a second conversation — is an
        // equal-length match, and `find_longest_prefix` deliberately ignores
        // those: for a whole prompt an equal-length entry leaves nothing to
        // decode. Here the head is only part of the prompt and the suffix is
        // known to be non-empty, so the match is both legal and exactly the one
        // that saves the most time.
        let head_key = self
            .prompt_state_cache_key(PromptStateCacheLayer::F8System, prefix, &prefix_tokens)
            .ok();
        // And where an *unpinned* checkpoint of the same prefix lives. A prompt
        // carrying a per-turn note is never pinned — the note is the wrong
        // occupant for the one pinned slot — so its checkpoint is filed under
        // `HistoryPrefix`, and it is subject to the identical equal-length
        // blindness. Without this second exemption, a note appended to the
        // system prompt made every turn a cold read of the whole device list:
        // measured on the command benchmark, 22 of 22 turns cold and the middle
        // turn 4.5× slower, for a note that never changed between turns.
        let stored_key = self
            .prompt_state_cache_key(PromptStateCacheLayer::HistoryPrefix, prefix, &prefix_tokens)
            .ok();
        let cached = {
            let mut cache = self
                .prompt_state_cache
                .lock()
                .map_err(|_| anyhow!("llama-local prompt-state cache mutex poisoned"))?;
            let entry = cache
                .get(&key)
                .or_else(|| head_key.as_ref().and_then(|head| cache.get(head)))
                .or_else(|| stored_key.as_ref().and_then(|stored| cache.get(stored)));
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
                    &[
                        PromptStateCacheLayer::F8ChatPrefix,
                        PromptStateCacheLayer::HistoryPrefix,
                        // Where a prefix that dies with the turn is filed. Of
                        // no use to a later turn, but the wording pass after a
                        // tool call has the pass before it as a strict prefix,
                        // so within one turn it is the deepest thing there is.
                        PromptStateCacheLayer::ExactPrompt,
                        PromptStateCacheLayer::F8System,
                    ],
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
            // Stop at the end of the steady head, keep what has been read, then
            // read on. Everything after that boundary — the language note, the
            // speaker, the conversation, the user's words — is what makes one
            // turn's checkpoint useless to the next; everything before it is
            // the same in every turn this house will ever see, and is where
            // nearly all the reading time goes.
            //
            // The pin goes here rather than to the whole prefix because a pin
            // is one entry per layer and this is the occupant every turn can
            // use. See the store site below for the whole-prefix checkpoint,
            // which is still taken and still serves the exact-key path.
            let pinned_head =
                self.checkpoint_steady_head(model, &mut ctx, &prefix_tokens, &params, start)?;
            if let Some(head_tokens) = pinned_head {
                start = head_tokens;
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
            // Checkpoint the prefix whenever we just paid to read it. Measured
            // on gemma-4-e2b: 966 tokens of system prompt, area and device
            // names and tool catalogue cost 13.2 s to read, and were then
            // thrown away — the next conversation paid 16.5 s to read the same
            // thing again, because the only pinned entry was the 72-token bare
            // system prompt the daemon warms at startup, before the device list
            // and the tools exist.
            //
            // Stored on EVERY cold read, not only when the prefix is the static
            // head, and stored under `HistoryPrefix` rather than the chat
            // layer. Both halves matter, and a later pair of traces showed why:
            //
            //  * A turn that calls a tool ends up storing a checkpoint that
            //    contains the tool call and the tool result. The NEXT turn
            //    never sees either — history keeps only the spoken reply — so
            //    that checkpoint diverges and cannot match, however deep it is.
            //    The prefix read at the start of the turn is the thing the next
            //    turn actually shares.
            //  * Pruning is per layer, and drops any shorter same-layer entry
            //    the new one covers. Filed under the chat layer, this
            //    checkpoint was pruned by the completed-turn insert seconds
            //    later, in the same turn, leaving only the 72-token pin again.
            //    Its own layer keeps it out of that fight.
            //
            // Measured cost of not having it: a conversation whose prompt was
            // an exact prefix of the next turn's still re-read 1599 tokens,
            // 37.6 s, because the only survivor was a 1742-token checkpoint too
            // long and too divergent to match.
            //
            // Only the static head is PINNED (`params.pin_prefix`): it is the
            // one entry worth protecting from eviction forever, being identical
            // in every later conversation.
            //
            // And a prefix that does not outlive the turn is filed apart from
            // the ones that do (`params.prefix_outlives_turn`), because the
            // pruning described above happens *within* `HistoryPrefix` too: the
            // wording pass after a tool call has a longer prefix that contains
            // the turn-start one, so filing both together deleted the useful
            // half seconds after it was written.
            //
            // The pin is claimed by the steady head when there is one: it is
            // the same head with the turn's own note still to come, so it
            // matches everything this one would and more. Pinning both would
            // mean the second insert releasing the first.
            let pin_here = params.pin_prefix && pinned_head.is_none();
            if start < prefix_tokens.len() {
                let store_key = if pin_here {
                    head_key
                } else if params.prefix_outlives_turn {
                    stored_key
                } else {
                    self.prompt_state_cache_key(
                        PromptStateCacheLayer::ExactPrompt,
                        prefix,
                        &prefix_tokens,
                    )
                    .ok()
                };
                if let (Ok(prefix_state), Some(store_key)) = (copy_context_state(&ctx), store_key) {
                    let state_bytes = prefix_state.len();
                    if let Ok(mut cache) = self.prompt_state_cache.lock() {
                        let entry = PromptStateCacheEntry::with_tokens(
                            prefix_state,
                            token_ids(&prefix_tokens),
                        );
                        let report = if pin_here {
                            cache.insert_pinned(store_key.clone(), entry)
                        } else {
                            cache.insert(store_key.clone(), entry)
                        };
                        record_cache_mutation(&report);
                    }
                    current_instant(
                        "llm.prompt_cache_prefix_stored",
                        "cache",
                        CACHE_LANE,
                        json!({
                            "layer": store_key.layer().as_str(),
                            "cache_key": store_key.stable_id(),
                            "pinned": pin_here,
                            "token_count": prefix_tokens.len(),
                            "state_bytes": state_bytes,
                        }),
                    );
                }
            }
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
            params.max_new_tokens,
            self.tap().map(Arc::as_ref),
            params.grammar.as_deref(),
            on_delta,
        )?;
        // Option C: checkpoint the POST-generation state so the next turn can
        // restore the completed exchange (system + history + this user + reply)
        // instead of re-prefilling user_N + reply_N.
        //
        // Subtlety (proven empirically): the KV cache holds the
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
        //
        // Stored for every turn, including the first one of a conversation. It
        // used to be skipped when the history was empty, on the grounds that a
        // one-shot summary's checkpoint can never prefix the next, different
        // summary — true, but it also skipped the case that matters most: the
        // first turn of a conversation, whose checkpoint is needed twice within
        // a second or two (by the wording pass after a tool call, and by turn
        // two). A trace paid 23.8 s for that omission. The old objection that
        // this insert would prune the shared system-prompt prefix no longer
        // holds either — that prefix is now pinned, and pruning skips pins.
        if !generation.tokens.is_empty() {
            let mut combined: Vec<llama_cpp_2::token::LlamaToken> =
                Vec::with_capacity(full_tokens.len() + generation.tokens.len());
            combined.extend_from_slice(&full_tokens);
            combined.extend_from_slice(&generation.tokens);
            // Reconstruct THIS turn exactly as the next turn's history will spell
            // it: the trimmed reply plus the model's real close marker. Derive the
            // marker from the model so the gemma-4 line closes with `<turn|>`
            // rather than the literal `<end_of_turn>` it does not register — if it
            // diverged from the live prompt, completed-turn checkpoints would stop
            // matching and every turn would cold-prefill.
            let model_name =
                self.model_path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
            let closer = turn_markers(model_name).close;
            let canonical = format!("{full_prompt}{}{closer}\n", generation.text.trim());
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
        self.run_inference_with_model(model, prompt, MAX_NEW_TOKENS, None, on_delta)
    }

    /// Reply generation with the prefix cache. Only attempts the cached
    /// path when the split reproduces the full prompt byte-for-byte; on any
    /// incompatibility it falls back to a full prefill having emitted nothing.
    fn run_inference_with_prefix_cache<F>(
        &self,
        prompt: &str,
        prefix: &str,
        suffix: &str,
        layer: PromptStateCacheLayer,
        params: GenParams,
        mut on_delta: F,
    ) -> Result<String>
    where
        F: FnMut(String) -> Result<bool>,
    {
        let guard = self.state.lock().map_err(|_| anyhow!("llama-local mutex poisoned"))?;
        let model = guard.as_ref().ok_or_else(|| anyhow!("llama-local model not loaded"))?;
        // Open the capture gate for exactly this turn (we hold the model
        // lock, so generations are serialised and this can't race a
        // concurrent network turn). The RAII guard closes it again on every
        // exit path, so any later prewarm/diagnostic decode stays dark.
        self.capture_gate.store(params.allow_capture, Ordering::Relaxed);
        let _capture_gate = CaptureGateGuard(&self.capture_gate);
        if format!("{prefix}{suffix}") == prompt {
            if let Some(text) = self.generate_with_prefix_cache(
                model,
                prefix,
                suffix,
                layer,
                params.clone(),
                &mut on_delta,
            )? {
                return Ok(text);
            }
        } else {
            cold_prefill(layer.as_str(), "prompt_split_mismatch");
        }
        // The rails follow the fallback: a cache miss must not quietly disarm
        // them, or an A/B run would be measuring cache luck instead of rails.
        self.run_inference_with_model(
            model,
            prompt,
            params.max_new_tokens,
            params.grammar.as_deref(),
            on_delta,
        )
    }

    #[allow(clippy::too_many_lines)]
    fn run_inference_with_model<F>(
        &self,
        model: &LlamaModel,
        prompt: &str,
        max_new_tokens: i32,
        grammar: Option<&str>,
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
        if let Some(tap) = self.tap() {
            // SAFETY: the tap is owned by `self` (shared across thin
            // clones via `Arc`) and therefore outlives this
            // method-local context — the `install` contract.
            unsafe { tap.install(&mut ctx_params) };
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
            max_new_tokens,
            self.tap().map(Arc::as_ref),
            grammar,
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
                            "reply": prompt_for_trace(&text),
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
                let uncached_output = self.run_inference_with_model(
                    model,
                    &full_prompt,
                    MAX_NEW_TOKENS,
                    None,
                    |_| Ok(true),
                )?;
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
                    MAX_NEW_TOKENS,
                    self.tap().map(Arc::as_ref),
                    // Cache-benchmark path: never railed, so the two arms of the
                    // cached/uncached comparison stay identical in sampling.
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
        if let Some(tap) = self.tap() {
            // SAFETY: the tap is owned by `self` (shared across thin
            // clones via `Arc`) and therefore outlives this
            // method-local context — the `install` contract.
            unsafe { tap.install(&mut ctx_params) };
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
        // One spine-sweep pulse on the Glass Cortex per prefill batch.
        #[allow(clippy::cast_possible_truncation)]
        fono_core::brain_tap::publish_prefill(
            self.tap().map(std::convert::AsRef::as_ref),
            tokens.len() as u32,
        );
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
        if let Some(tap) = self.tap() {
            // SAFETY: the tap is owned by `self` (shared across thin
            // clones via `Arc`) and therefore outlives this
            // method-local context — the `install` contract.
            unsafe { tap.install(&mut ctx_params) };
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
                MAX_NEW_TOKENS,
                self.tap().map(Arc::as_ref),
                // Diagnostic replay of a rendered prompt: no tools are offered,
                // so there is nothing to constrain.
                None,
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
            // Warm the head the reply path will actually send — greeting, areas,
            // devices and tool descriptions — rendered by the same code, so the
            // checkpoint is a genuine token prefix of the live prompt. Warming
            // the bare greeting instead pinned 72 tokens in front of 1510 and
            // cost thirteen seconds on the first command of every conversation.
            let head = local_tools::head_with_tools(
                system,
                &warmup.f8_action_descriptors,
                warmup.f8_instructions.as_deref(),
            );
            let base = assistant_base_prefix(&head, model_name);
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
        // Same head as the reply path — see `build_stable_prompt_caches`.
        let head = local_tools::head_with_tools(
            &snapshot.system_prompt,
            &snapshot.action_descriptors,
            snapshot.instructions.as_deref(),
        );
        let base = assistant_base_prefix(&head, model_name);
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
    /// completed exchange instead of re-prefilling it.
    tokens: Vec<llama_cpp_2::token::LlamaToken>,
}

// 9 args: the `tap` observer is optional plumbing for the Glass Cortex
// visualization and `grammar` is the optional tool-call constraint; bundling
// the generation bounds into a struct would churn five call sites for no
// clarity gain.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn generate_from_prefilled_context<F>(
    model: &LlamaModel,
    ctx: &mut LlamaContext<'_>,
    start_pos: i32,
    first_sample_idx: i32,
    first_token_override: Option<llama_cpp_2::token::LlamaToken>,
    max_new_tokens: i32,
    tap: Option<&BrainTap>,
    grammar: Option<&str>,
    mut on_delta: F,
) -> Result<GenerationResult>
where
    F: FnMut(String) -> Result<bool>,
{
    // Shared generation policy: repetition penalty over generated tokens
    // only, feeding greedy. Deterministic; breaks the verbatim-repetition
    // attractor (see `fono_core::llama_gen` for the evidence).
    //
    // With rails supplied, one extra link goes in front of greedy. It is inert
    // until the model starts writing a command, so ordinary talking samples
    // exactly as it does without them — which is what makes the two runs of an
    // A/B comparable.
    let (mut sampler, rails_armed) = grammar.map_or_else(
        || (generation_sampler(), false),
        |g| generation_sampler_with_grammar(model, g, &trigger_patterns()),
    );
    let eos = model.token_eos();
    let mut out = String::new();
    let mut emitted_len = 0_usize;
    let mut sample_idx = first_sample_idx;
    let mut next_token = first_token_override;
    let mut decoder = encoding_rs::UTF_8.new_decoder();
    // Slice the autoregressive decode loop onto the `llm` lane as a single
    // `llm.generate` span carrying the canonical generation schema
    // ([`generation_span_args`]), so the F8 waterfall reports `tok_per_sec` /
    // `ttft_ms` directly — at parity with the F7 `polish.generate` span —
    // instead of forcing the reader to infer them from a separate instant.
    let gen_span = current_span("llm.generate", "assistant.llm", "llm");
    let decode_started = Instant::now();
    let mut generated_tokens = 0_u32;
    let mut deltas = 0_u32;
    let mut ttft_ms = 0_u64;
    let token_trace = token_trace_enabled();
    let mut decoded_tokens: Vec<llama_cpp_2::token::LlamaToken> = Vec::new();
    let mut stop_reason = "max_tokens";
    let mut batch = LlamaBatch::new(1, 1);
    // Glass Cortex: announce the generation on the brain-event bus
    // (no-op unless the tap is installed AND a sink is listening).
    fono_core::brain_tap::publish_reply_begin(tap);
    for n_cur in (start_pos..).take(max_new_tokens.max(1) as usize) {
        // `sample_next` leaves the accepting to llama.cpp, which does it inside
        // the sample call. A token that came from somewhere else — the prompt
        // cache samples the first one from the restored state — is the one the
        // sampler has to be told about by hand. Doing both to the same token
        // fed it twice and cost us the tool-call rails entirely; see
        // `fono_core::llama_gen::sample_next`.
        let token = match next_token.take() {
            Some(first) => {
                adopt_sampled_token(&mut sampler, first);
                first
            }
            None => sample_next(&mut sampler, ctx, sample_idx),
        };
        // Model-agnostic stop: any token this vocabulary tags as Control ends
        // the turn, however the marker is spelled (gemma-4-e2b ships `<turn|>`,
        // not `<end_of_turn>` — literal lookups were dead code; see
        // `fono_core::llama_gen`).
        if token == eos || is_control_token(model, token) {
            stop_reason = if token == eos { "eos" } else { "control_token" };
            break;
        }
        let piece = model.token_to_piece(token, &mut decoder, false, None).unwrap_or_default();
        out.push_str(&piece);
        if generated_tokens == 0 {
            ttft_ms = decode_started.elapsed().as_millis() as u64;
        }
        generated_tokens = generated_tokens.saturating_add(1);
        if let Some((stop_at, marker)) = first_stop_marker(&out) {
            if stop_at > emitted_len {
                let delta = out[emitted_len..stop_at].to_string();
                if !on_delta(delta)? {
                    stop_reason = "receiver_dropped";
                    break;
                }
                deltas += 1;
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
            deltas += 1;
            emitted_len = safe_end;
        }
        // Per-token instant is verbose and costs a mutex+alloc on the hot
        // decode thread for EVERY token, so it is gated behind
        // `FONO_TRACE_TOKENS` (default off). The `llm.generate` span's
        // `deltas`/`ttft_ms` already capture decode cadence for normal traces.
        if token_trace {
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
        }
        batch.clear();
        batch.add(token, n_cur, &[0], true).context("decode batch.add")?;
        sample_idx = 0;
        // Glass Cortex keyframe capture (opt-in): the shared helper arms
        // the eval-callback tap only for governor-chosen tokens and times
        // the whole surcharge (arm + decode + collect + logits stats) so
        // the governor's < 1 % backoff sees the true cost. With no tap
        // this is the plain decode call.
        decode_token_with_tap(ctx, &mut batch, tap, u64::from(generated_tokens.saturating_sub(1)))
            .context("decode loop")?;
        decoded_tokens.push(token);
    }
    if stop_reason != "receiver_dropped" && emitted_len < out.len() {
        let delta = out[emitted_len..].to_string();
        if on_delta(delta)? {
            deltas += 1;
        } else {
            stop_reason = "receiver_dropped";
        }
    }
    let elapsed_ms = decode_started.elapsed().as_millis() as u64;
    // Glass Cortex: close the generation on the brain-event bus with
    // throughput + KV-fill stats (no-op without a tap + sink).
    #[allow(clippy::cast_sign_loss)]
    fono_core::brain_tap::publish_reply_end(
        tap,
        u64::from(generated_tokens),
        elapsed_ms,
        (start_pos.max(0) as u32).saturating_add(generated_tokens),
        ctx.n_ctx(),
    );
    // Stamp whether the rails were armed for this generation, so an
    // A/B pair of traces is self-describing — you can tell from the artefact
    // alone which arm produced it instead of trusting the run log.
    let mut gen_args = generation_span_args(
        generated_tokens,
        out.chars().count(),
        deltas,
        ttft_ms,
        elapsed_ms,
        start_pos,
        stop_reason,
    );
    if let Some(obj) = gen_args.as_object_mut() {
        // Three states, not two. `rejected` is the one that used to hide: a
        // grammar llama.cpp refuses leaves sampling exactly as it is with the
        // setting off, and reporting the request rather than the outcome meant
        // a trace could say `on` while nothing was being held to anything.
        let state = match (grammar.is_some(), rails_armed) {
            (true, true) => "on",
            (true, false) => "rejected",
            _ => "off",
        };
        obj.insert("grammar".into(), state.into());
        // Armed is not the same as effective, and the difference is not
        // academic: for one whole measurement the rails were armed on every
        // generation and held the model to nothing, because the sampler was
        // being fed each token twice and never recognised the opener. So the
        // trace also reports whether the constraint was still holding anything
        // when the model stopped — false on a turn that wrote a command means
        // the rails were bypassed, however green the setting looks.
        if rails_armed {
            obj.insert("rails_bit".into(), (ruled_out(model, &sampler) > 0).into());
        }
    }
    gen_span.finish(gen_args);
    Ok(GenerationResult { text: out, elapsed_ms, tokens: decoded_tokens })
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

fn push_gemma_turn(buf: &mut String, role: &str, content: &str, markers: TurnMarkers) {
    if content.trim().is_empty() {
        return;
    }
    buf.push_str(markers.open);
    buf.push_str(role);
    buf.push('\n');
    buf.push_str(content.trim());
    buf.push_str(markers.close);
    buf.push('\n');
}

/// Split the reply prompt into a stable prefix and a per-turn suffix for the
/// prefix cache. The stable prefix is everything up to (but not
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
        build_gemma_prompt_split(ctx, user_text, turn_markers(model_name))
    } else {
        build_chatml_prompt_split(ctx, user_text)
    }
}

/// Render a completed assistant turn for replay on a later turn.
///
/// A turn where the model invoked a tool carries the call in `tool_calls` and
/// usually has empty `content`. Rendering only `content` therefore erased the
/// action entirely: history replayed the user's request followed by the model
/// apologising, with no evidence a tool had ever run. The model then reasoned
/// from its own confusion — one trace shows it announcing that the lights "did
/// not respond" immediately after a call that succeeded, then calling again.
///
/// The call is spelled with [`local_tools::render_call`], the same wrapper the
/// model is asked to produce, so a replayed call reads back in the syntax it
/// was taught. It is rebuilt from the parsed name and arguments rather than the
/// model's own bytes, because that is all history keeps — so this is a
/// normalised spelling, not a transcript. That is fine here and would not be
/// mid-turn: the *current* turn continues from the raw text precisely so the
/// checkpoint saved moments earlier still matches (see the note in
/// `reply_stream`), whereas a later turn has no such checkpoint to preserve.
fn render_assistant(turn: &ChatTurn) -> String {
    let mut out = String::new();
    let content = turn.content.trim();
    if !content.is_empty() {
        out.push_str(content);
    }
    for call in &turn.tool_calls {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&local_tools::render_call(&call.name, &call.arguments));
    }
    out
}

/// Whether the system block can ride on the first turn Gemma will render.
///
/// Gemma has no system role, so the block is folded into the first user turn —
/// its trained convention. That only holds if a user turn is what comes first,
/// and often it is not. The rolling window drops turns off the front, and turns
/// arrive in pairs, so half the time the survivor at the front is a model
/// reply; an API client, meanwhile, may send any array it likes. Rendering the
/// block behind a model turn buries the instructions the model is meant to
/// follow, and stops the pinned base checkpoint being a token prefix of the
/// prompt — so it can never match, and the turn is prefilled cold from nothing.
///
/// An empty history counts as welding: the block leads the current user turn,
/// which is the same shape.
fn system_welds_onto_first_turn(history: &[ChatTurn]) -> bool {
    history
        .iter()
        .find_map(|turn| match turn.role {
            ChatRole::User | ChatRole::System => (!turn.content.trim().is_empty()).then_some(true),
            ChatRole::Assistant => (!render_assistant(turn).trim().is_empty()).then_some(false),
            ChatRole::Tool => {
                (!local_tools::render_result(&turn.content).trim().is_empty()).then_some(false)
            }
        })
        .unwrap_or(true)
}

fn build_gemma_prompt_split(
    ctx: &AssistantContext,
    user_text: &str,
    markers: TurnMarkers,
) -> (String, String) {
    // Gemma has no dedicated system role, so the system prompt is prepended to
    // the FIRST user turn (Gemma's trained convention). This keeps the rendered
    // prompt strictly append-only: the leading tokens — system, then each
    // completed turn — never change as the conversation grows, so a boot-built
    // system checkpoint and a per-conversation checkpoint both stay valid as
    // token-prefixes turn after turn. Anything volatile (the current user text)
    // lives only in the trailing suffix.
    //
    // When the history does not open on a user turn there is nothing to fold
    // into, so the block leads on a turn of its own instead. Slightly off the
    // trained shape, and far better than the alternative of rendering it after
    // a model reply, which buries it and voids the pinned checkpoint.
    let system = ctx.system_prompt.trim();
    let mut prefix = String::new();

    let leads_alone = !system.is_empty() && !system_welds_onto_first_turn(&ctx.history);
    if leads_alone {
        push_gemma_turn(&mut prefix, "user", system, markers);
    }
    let mut system_emitted = leads_alone;

    for turn in &ctx.history {
        match turn.role {
            ChatRole::User | ChatRole::System => {
                let content = turn.content.trim();
                if content.is_empty() {
                    continue;
                }
                if !system_emitted && !system.is_empty() {
                    push_gemma_turn(
                        &mut prefix,
                        "user",
                        &format!("{system}\n\n{content}"),
                        markers,
                    );
                    system_emitted = true;
                } else {
                    push_gemma_turn(&mut prefix, "user", content, markers);
                }
            }
            ChatRole::Assistant => {
                push_gemma_turn(&mut prefix, "model", &render_assistant(turn), markers);
            }
            ChatRole::Tool => {
                push_gemma_turn(
                    &mut prefix,
                    "user",
                    &local_tools::render_result(&turn.content),
                    markers,
                );
            }
        }
    }

    prefix.push_str(markers.open);
    prefix.push_str("user\n");
    if !system_emitted && !system.is_empty() {
        // No prior user turn carried the system prompt, so it leads the current
        // turn. It stays in the (cacheable) prefix; only the user text varies.
        prefix.push_str(system);
        prefix.push_str("\n\n");
    }
    let suffix = format!("{}{}\n{}model\n", user_text.trim(), markers.close, markers.open);
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
        let (role, content) = match turn.role {
            ChatRole::User => ("user", turn.content.trim().to_string()),
            ChatRole::Assistant => ("assistant", render_assistant(turn)),
            ChatRole::System => ("system", turn.content.trim().to_string()),
            // No tool role exists in the hand-rolled ChatML framing, so the
            // answer comes back on the user channel — the same downgrade the
            // Anthropic backend documents. Dropping it instead taught the model
            // that acting on a request leaves no trace.
            ChatRole::Tool => ("user", local_tools::render_result(&turn.content)),
        };
        if content.trim().is_empty() {
            continue;
        }
        prefix.push_str("<|im_start|>");
        prefix.push_str(role);
        prefix.push('\n');
        prefix.push_str(content.trim());
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
        format!("{}user\n{system}", turn_markers(model_name).open)
    } else {
        format!("<|im_start|>system\n{system}")
    }
}

/// Continues a finished prompt with the model's tool call and the tool's
/// answer, ready for the second generation that words the result.
///
/// The first prompt already ends with the open-model marker, so we close the
/// model's turn, hand the result back as a user turn, and re-open. The tool's
/// answer travels as ordinary text rather than a dedicated tool role: the
/// hand-rolled templates have no such role, and inventing one would put
/// tokens in front of the model that it was never trained on.
fn tool_result_continuation(prompt: &str, call: &str, result: &str, model_name: &str) -> String {
    let m = turn_markers(model_name);
    let reply_role =
        if model_name.to_ascii_lowercase().contains("gemma") { "model" } else { "assistant" };
    format!(
        "{prompt}{call}{close}\n{open}user\n{result}{close}\n{open}{reply_role}\n",
        close = m.close,
        open = m.open,
        result = local_tools::render_result(result),
    )
}

#[async_trait]
impl Assistant for LlamaLocalAssistant {
    #[allow(clippy::too_many_lines)] // one streaming closure; splitting it would
                                     // only move the borrowed state into a struct of arguments
    async fn reply_stream(
        &self,
        user_text: &str,
        ctx: &AssistantContext,
    ) -> Result<BoxStream<'static, Result<TokenDelta>>> {
        let model_name = self.model_path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        // Tools are described in the system prompt because this backend renders
        // its own chat markers and so never sees the GGUF's tool template. The
        // block is *appended*, never prepended, so the pinned system checkpoint
        // stays a genuine token prefix and the turn is not cold-prefilled.
        //
        // Order is load-bearing: greeting, areas, devices, tools — everything
        // that changes only when the house does — and the speaker note last,
        // because it changes every turn. Composed here rather than by the
        // caller so that the head this backend sends is byte-identical to the
        // head `prewarm_prompt_caches` pinned; two renderings that must agree
        // have drifted twice before, and each time the symptom was a checkpoint
        // that could never match.
        let actions = ctx
            .actions
            .clone()
            .filter(|a| !a.descriptors.is_empty())
            .filter(|_| std::env::var_os("FONO_LOCAL_TOOLS_OFF").is_none());
        let head = local_tools::head_with_tools(
            &ctx.system_prompt,
            actions.as_ref().map_or(&[][..], |a| &a.descriptors),
            ctx.instructions.as_deref(),
        );
        let composed = crate::compose_system_prompt(&head, ctx.turn_notes().as_deref());
        let ctx = if composed == ctx.system_prompt {
            std::borrow::Cow::Borrowed(ctx)
        } else {
            let mut c = ctx.clone();
            c.system_prompt = composed;
            std::borrow::Cow::Owned(c)
        };
        let ctx = ctx.as_ref();
        let prompt = build_prompt(ctx, user_text, model_name);

        let (cache_prefix, cache_suffix) = build_prompt_split(ctx, user_text, model_name);
        // The head, framed for the model and stopping short of the language
        // note, the speaker, the conversation and the user's words. Anything
        // after it differs between two turns that are otherwise identical, and
        // a checkpoint is all-or-nothing about its tokens — so this is the
        // deepest point a checkpoint can reach and still serve the next turn,
        // whatever language it is spoken in.
        let steady_head: Option<Arc<str>> = {
            let base = assistant_base_prefix(&head, model_name);
            (!base.is_empty() && cache_prefix.starts_with(&base)).then(|| Arc::from(base.as_str()))
        };
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
        // Per-request generation budget: short-form callers (e.g. notification
        // summaries) cap the reply well below the global default so a
        // degenerate run is bounded by seconds, not the full 384-token budget.
        let max_new_tokens = ctx
            .max_new_tokens
            .and_then(|n| i32::try_from(n).ok())
            .map_or(MAX_NEW_TOKENS, |n| n.clamp(1, MAX_NEW_TOKENS));
        // Pin this request's prefix only when it genuinely IS the static head —
        // system prompt, areas, devices, tool catalogue and nothing else.
        //
        // The warm paths pin that head deliberately and by name, so this is now
        // only a safety net for the window before the warm has finished (or a
        // backend reached through the LLM server, which never warms at all).
        // Narrow, because the pin is one entry per layer and the wrong occupant
        // evicts the right one: a turn carrying a speaker note would pin
        // "…tools, and you are talking to Ana", which the next turn — a
        // different speaker, or none — cannot use, having thrown away the head
        // that every turn could. The same goes for the language note, which
        // changes the moment the user switches language mid-conversation.
        let gen_params = GenParams {
            max_new_tokens,
            pin_prefix: ctx.history.is_empty() && ctx.turn_notes().is_none(),
            // This prefix ends where the user's words begin, so the next turn
            // asks for exactly it.
            prefix_outlives_turn: true,
            allow_capture: ctx.allow_brain_capture,
            // Rails only when tools are offered this turn AND the setting is on.
            // `ActionTools::grammar` is already `None` when the switch is off,
            // so there is nothing to check here beyond "are there tools at all".
            grammar: actions
                .as_ref()
                .and_then(|a| a.grammar.as_ref().map(|g| Arc::from(g.as_str()))),
            // Where the steady part of this prompt ends. Rendered by the same
            // call the startup warm uses, so the checkpoint taken here and the
            // one warmed there are the same entry rather than two that evict
            // each other.
            steady_head,
        };
        let started = Instant::now();
        let model_name_owned = model_name.to_string();
        let (tx, rx) = mpsc::channel::<Result<TokenDelta>>(STREAM_CHANNEL_CAPACITY);
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let total_span = current_span("llm.local_streaming_inference", "assistant.llm", "llm");
            let mut deltas_emitted = 0_u32;
            let result = (|| -> Result<String> {
                me.ensure_loaded()?;
                // While tools are on, a reply that might still turn out to be a
                // call is held back rather than spoken. `could_be_call` releases
                // it the moment it is plainly prose, so an ordinary answer keeps
                // its head start; only a genuine call is ever buffered whole.
                //
                // Watching does not stop there. The model is asked to say what
                // it is doing and then write the command, so the command
                // arrives after prose that has already been spoken — and text
                // released without watching for what follows it gets a
                // perfectly good call read aloud as JSON.
                let watching = actions.is_some();
                let mut buf = String::new();
                let mut spoken = String::new();
                let text = me.run_inference_with_prefix_cache(
                    &prompt,
                    &cache_prefix,
                    &cache_suffix,
                    PromptStateCacheLayer::F8ChatPrefix,
                    gen_params.clone(),
                    |delta| {
                        let delta = delta.trim_start_matches('\u{feff}').to_string();
                        if delta.is_empty() {
                            return Ok(true);
                        }
                        if !watching {
                            deltas_emitted = deltas_emitted.saturating_add(1);
                            return Ok(tx.blocking_send(Ok(TokenDelta::text(delta))).is_ok());
                        }
                        buf.push_str(&delta);
                        // Nothing said yet, so the whole reply may still be a
                        // command — in any of the shapes a model writes one in,
                        // which is more than the one tag the split below knows.
                        if spoken.is_empty() && local_tools::could_be_call(&buf) {
                            return Ok(true);
                        }
                        let (speak, hold) = local_tools::split_speakable(&buf);
                        let flush = speak.to_string();
                        buf = hold.to_string();
                        if flush.is_empty() {
                            return Ok(true);
                        }
                        spoken.push_str(&flush);
                        deltas_emitted = deltas_emitted.saturating_add(1);
                        Ok(tx.blocking_send(Ok(TokenDelta::text(flush))).is_ok())
                    },
                )?;
                if !watching {
                    return Ok(text);
                }
                let held = std::mem::take(&mut buf);
                debug!(
                    spoken_chars = spoken.len(),
                    held_chars = held.len(),
                    text_chars = text.len(),
                    has_open = text.contains(local_tools::OPEN),
                    "first pass finished"
                );
                let Some(actions) = actions else { return Ok(text) };
                // Whether this turn carries a command is decided on the whole
                // reply, not on the part that happened to arrive last. Holding
                // is a streaming convenience: it keeps a command out of the
                // speaker while it is still being written. Treating it as the
                // record of what was written puts one delivery hiccup between
                // a perfectly good command and the house — a run left five
                // commands unrun and read all five out as JSON instead, each
                // one whole and parseable in the text the model had just
                // finished writing.
                let Some((name, arguments)) =
                    local_tools::parse_call(&held).or_else(|| local_tools::parse_call(&text))
                else {
                    if held.trim().is_empty() {
                        return Ok(text);
                    }
                    // Ambiguous to the last token, but prose after all. Say it —
                    // swallowing it would leave the user with silence.
                    deltas_emitted = deltas_emitted.saturating_add(1);
                    let _ = tx.blocking_send(Ok(TokenDelta::text(held)));
                    return Ok(text);
                };
                // Anything the command itself was spelled with is not something
                // the user was told, whatever reached the speaker.
                let spoken = local_tools::split_speakable(&spoken).0.to_string();
                let call = ToolCall {
                    id: format!("local-{}", started.elapsed().as_nanos()),
                    name,
                    arguments,
                };
                let closer = turn_markers(&model_name_owned).close;

                // One pass of the model, holding back anything that might not
                // be prose. Two kinds of thing get held: a channel header some
                // models open with, which must never reach the speaker, and a
                // tool call, which is either run or discarded but never read
                // aloud.
                //
                // The call is watched for *throughout*, not just at the start,
                // and on every pass rather than only where a correction is
                // still allowed. Asking the model to name the devices that
                // failed and then try again is asking for prose followed by a
                // call; judging the reply only by how it opens released the
                // prose and then recited the call. A trace shows two
                // well-formed calls spoken as JSON while the lights stayed off
                // — and, worse, stored in the conversation as something the
                // assistant had said, so the next turn believed a command could
                // be carried out by describing one.
                //
                // Returns the raw text (which the next prompt must reproduce
                // exactly, or the checkpoint saved moments ago cannot match),
                // the part that was spoken, and the part held back.
                //
                // `written` is text the prompt already put in the model's mouth,
                // and it is counted as generated: the buffer starts with it, so
                // an opener written for the model still hides the command from
                // the speaker, and the raw text still reproduces the whole reply
                // for the next prompt. A pass that starts mid-sentence has no
                // channel header to strip either.
                let mut run_pass = |full: &str,
                                    prefix: &str,
                                    suffix: &str,
                                    written: &str|
                 -> Result<(String, String, String)> {
                    let mut buf = written.to_string();
                    let mut spoken = String::new();
                    let mut header_done = !written.is_empty();
                    let text = me.run_inference_with_prefix_cache(
                        full,
                        prefix,
                        suffix,
                        PromptStateCacheLayer::F8ChatPrefix,
                        // Never pinned: this pass's prefix carries this turn's
                        // own words, so pinning it would evict the static head
                        // pin — the one entry every later conversation depends
                        // on. Nor does it outlive the turn, for the same
                        // reason: it contains this turn's request, tool call
                        // and tool result, which no later turn reproduces.
                        GenParams {
                            pin_prefix: false,
                            prefix_outlives_turn: false,
                            ..gen_params.clone()
                        },
                        |delta| {
                            buf.push_str(delta.trim_start_matches('\u{feff}'));
                            if !header_done {
                                if local_tools::maybe_preamble(&buf) {
                                    return Ok(true);
                                }
                                buf = local_tools::strip_preamble(&buf).to_string();
                                header_done = true;
                            }
                            let (speak, hold) = local_tools::split_speakable(&buf);
                            let flush = speak.to_string();
                            buf = hold.to_string();
                            if flush.is_empty() {
                                return Ok(true);
                            }
                            spoken.push_str(&flush);
                            deltas_emitted = deltas_emitted.saturating_add(1);
                            Ok(tx.blocking_send(Ok(TokenDelta::text(flush))).is_ok())
                        },
                    )?;
                    Ok((format!("{written}{text}"), spoken, buf))
                };

                // Run the call, word the result, and — at most once, and only
                // for a failure the executor judged safe to repeat — let the
                // model correct itself instead of ending the turn in an
                // apology the user has to answer by speaking again.
                let mut call = call;
                let mut base = prompt.clone();
                let mut call_text = text.trim().to_string();
                let mut attempt = 0;
                // What the user has already been told this turn. The model
                // announces the command before writing it, so on the ordinary
                // path the reply exists before the tool runs.
                let mut promised = spoken;
                loop {
                    let _ = tx.blocking_send(Ok(TokenDelta::tool(ToolEvent::Called(call.clone()))));
                    let outcome = handle.block_on((actions.execute)(call.clone()));
                    let _ = tx.blocking_send(Ok(TokenDelta::tool(ToolEvent::Result {
                        tool_call_id: call.id.clone(),
                        summary: outcome.summary.clone(),
                        failed: outcome.failed,
                        sent: outcome.sent.clone(),
                    })));

                    // The command was announced, and the world was read
                    // afterwards and agrees. The turn is over: reading the
                    // result and writing a second sentence that says the same
                    // thing costs a whole extra pass — measured at a median of
                    // 2.2 s of generation on top of 2.1 s spent re-reading the
                    // server's answer — and it is the pass that arrives in
                    // English on a Romanian turn, or arrives empty, or claims
                    // something the house never did. Nothing is skipped where
                    // the reading disagreed, or where the model said nothing.
                    if outcome.confirmed && !promised.trim().is_empty() {
                        debug!("the house agrees with what was already said; no second pass");
                        return Ok(promised);
                    }

                    // Word the result. Two things decide whether this pass
                    // costs a fifth of a second or half a minute, and a real
                    // trace paid the half minute — 21.6 s of it re-reading 974
                    // tokens it had just read:
                    //
                    //  * The call has to be spelled the way the model spelled
                    //    it. Re-serialising it as tidy JSON made this prompt
                    //    diverge from the checkpoint saved moments earlier, so
                    //    that checkpoint could never match.
                    //  * The cached prefix has to be THIS turn's completed
                    //    exchange, not the system prefix. The search only
                    //    considers entries shorter than the prefix it is given,
                    //    so offering the system prefix hid the deeper
                    //    checkpoint — which, worse, had just displaced the
                    //    shallower entry that used to match, leaving nothing
                    //    but the system prompt to restore.
                    let cont = tool_result_continuation(
                        &base,
                        &call_text,
                        &outcome.summary,
                        &model_name_owned,
                    );
                    let may_retry = attempt == 0 && outcome.retryable;
                    // The correction is written for the model, not asked of it.
                    // Ending the prompt with the opener leaves it mid-command,
                    // so the only way to continue is to write one — and the
                    // rails arm on its first `{`, which is what makes the second
                    // attempt land inside this house instead of repeating the
                    // guess that just failed. The invitation this replaces was
                    // declined every time: a small model reads "correct it and
                    // call the tool once more; otherwise tell the user plainly
                    // what went wrong" and picks the apology. Home Assistant had
                    // already said the name was the problem.
                    let written = if may_retry { local_tools::OPEN } else { "" };
                    let full = format!("{cont}{written}");
                    let turn_prefix = format!("{base}{call_text}{closer}\n");
                    let (cont_prefix, cont_suffix) = match full.strip_prefix(turn_prefix.as_str()) {
                        Some(rest) if !rest.is_empty() => (turn_prefix.clone(), rest.to_string()),
                        // Belt and braces: an empty suffix would generate from
                        // nothing, so fall back to the old system-prefix split.
                        _ => (
                            cache_prefix.clone(),
                            full.strip_prefix(cache_prefix.as_str()).unwrap_or("").to_string(),
                        ),
                    };
                    let (raw, spoken, held) = run_pass(&full, &cont_prefix, &cont_suffix, written)?;

                    let parsed = local_tools::parse_call(&held);
                    let retry =
                        may_retry.then(|| parsed.clone()).flatten().map(|(name, arguments)| {
                            ToolCall {
                                id: format!("local-{}", started.elapsed().as_nanos()),
                                name,
                                arguments,
                            }
                        });
                    // A repeat of the request that just failed is not a second
                    // attempt; it is a second wait for the same answer. Sending
                    // it is the only way the correction can waste the user's
                    // time, so it is not sent — unless the failure was Fono
                    // refusing on a guess about the request, where writing the
                    // call again is how the model says the guess was wrong.
                    //
                    // Judged against both what the model wrote and what the
                    // executor actually sent, because those differ: fields the
                    // model wrote are dropped or settled on the way out, so a
                    // second attempt that writes exactly what went last time is
                    // a repeat even though the two spellings do not match.
                    let repeat_ok = outcome.repeat_ok;
                    let as_sent = outcome.sent.clone();
                    let retry = retry.filter(|next| {
                        let same_as =
                            |before: &str| local_tools::same_request(&next.arguments, before);
                        let repeat = next.name == call.name
                            && (same_as(&call.arguments)
                                || as_sent.as_deref().is_some_and(same_as));
                        if repeat && !repeat_ok {
                            warn!(
                                "{} was written again unchanged after it failed; not sending it a \
                                 second time",
                                call.name
                            );
                        }
                        !repeat || repeat_ok
                    });
                    let Some(next) = retry else {
                        // Nothing more to run. Whatever was held back is either
                        // prose — say it, swallowing it would leave the user
                        // with silence — or a call we are not allowed to make.
                        // A call is never spoken: reciting JSON tells the user
                        // nothing and teaches the conversation that describing
                        // a command is a way of carrying one out.
                        if let Some((name, _)) = parsed {
                            warn!(
                                "{name} was proposed after the correction was already spent; not \
                                 running it and not reading it out"
                            );
                        } else if !held.trim().is_empty() {
                            deltas_emitted = deltas_emitted.saturating_add(1);
                            let _ = tx.blocking_send(Ok(TokenDelta::text(held.clone())));
                            return Ok(format!("{spoken}{held}"));
                        }
                        if !spoken.trim().is_empty() {
                            return Ok(spoken);
                        }
                        // The turn has run its commands and has no words to show
                        // for them. That is not a rare corner: the corrective
                        // pass ends the prompt mid-command, so prose is not even
                        // reachable, and when the command it forces is then
                        // refused as a repeat the user hears nothing at all and
                        // has no idea whether the house moved. Ask once more with
                        // nothing pre-written and no rails, so the only thing
                        // left to produce is a sentence.
                        let (prose_prefix, prose_suffix) = match cont
                            .strip_prefix(turn_prefix.as_str())
                        {
                            Some(rest) if !rest.is_empty() => {
                                (turn_prefix.clone(), rest.to_string())
                            }
                            _ => (
                                cache_prefix.clone(),
                                cont.strip_prefix(cache_prefix.as_str()).unwrap_or("").to_string(),
                            ),
                        };
                        if let Ok((_, said, rest)) =
                            run_pass(&cont, &prose_prefix, &prose_suffix, "")
                        {
                            if !said.trim().is_empty() {
                                return Ok(said);
                            }
                            // Prose the watcher held back for looking like the
                            // start of a command, and which turned out not to be
                            // one, was never sent to the speaker.
                            if !rest.trim().is_empty() && local_tools::parse_call(&rest).is_none() {
                                deltas_emitted = deltas_emitted.saturating_add(1);
                                let _ = tx.blocking_send(Ok(TokenDelta::text(rest.clone())));
                                return Ok(rest);
                            }
                        }
                        // Even that produced nothing. What the user was told
                        // before the command ran is better than silence.
                        return Ok(promised);
                    };
                    info!(
                        "retrying {} as {} after a failure that changed nothing",
                        call.name, next.name
                    );
                    base = cont;
                    // The next prompt must reproduce what the model actually
                    // wrote, call and all, or the checkpoint saved a moment ago
                    // cannot match and the whole conversation is re-read.
                    call_text = raw.trim().to_string();
                    call = next;
                    // A correction is announced the same way the first attempt
                    // was, so what the user just heard stands as the reply if
                    // the second command lands.
                    promised = spoken;
                    attempt += 1;
                }
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
                            "reply": prompt_for_trace(&text),
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

    fn model(&self) -> Option<String> {
        self.model_path.file_stem().and_then(|s| s.to_str()).map(str::to_owned)
    }

    fn can_run_actions(&self) -> bool {
        true
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
    fono_core::llama_backend::decode_threads()
}

/// Whether the verbose per-token `llm.decode_token` instant is emitted.
///
/// Off by default: that instant fires for every decoded token (a mutex lock +
/// JSON alloc on the hot decode thread) and the `llm.generate` span already
/// carries `deltas`/`ttft_ms`/`tok_per_sec`, so normal traces don't need it.
/// Set `FONO_TRACE_TOKENS=1` to opt into per-token granularity. Cached so the
/// env var is read once, not per token.
fn token_trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("FONO_TRACE_TOKENS").ok().and_then(|v| env_bool(&v)).unwrap_or(false)
    })
}

fn sha256_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

fn prompt_for_trace(prompt: &str) -> Option<&str> {
    fono_core::turn_trace::transcript_enabled().then_some(prompt)
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
    fn capture_gate_guard_closes_on_drop() {
        // The RAII guard must leave the gate closed no matter what it was
        // set to, so a later prewarm/diagnostic decode never captures.
        let gate = AtomicBool::new(false);
        {
            gate.store(true, Ordering::Relaxed);
            let _g = CaptureGateGuard(&gate);
            assert!(gate.load(Ordering::Relaxed), "gate open for the turn body");
        }
        assert!(!gate.load(Ordering::Relaxed), "gate closed after the turn");
    }

    #[test]
    fn tap_stays_dark_until_the_gate_opens() {
        // A capture-enabled backend must not expose the tap to the decode
        // path until a local turn opens the gate — the network-safety
        // invariant, checked without needing a loaded model.
        let a = LlamaLocalAssistant::new("/nonexistent.gguf", MIN_CTX).with_brain_tap(true);
        // Simulate the tap having been created at load time.
        let _ = a.brain_tap.set(Arc::new(BrainTap::new(4, 0, 0)));
        assert!(a.tap().is_none(), "gate closed by default");
        a.capture_gate.store(true, Ordering::Relaxed);
        assert!(a.tap().is_some(), "gate open exposes the tap");
        a.capture_gate.store(false, Ordering::Relaxed);
        assert!(a.tap().is_none(), "gate closed again hides the tap");
    }

    #[test]
    fn gemma_prompt_uses_gemma_turn_markers() {
        let ctx =
            AssistantContext { system_prompt: "Be concise.".into(), ..AssistantContext::default() };
        // The default gemma-4 line ships non-standard control-token markers.
        let p = build_prompt(&ctx, "hello", "gemma-4-e2b-it-Q4_K_M");
        assert!(p.contains("<|turn>user\nBe concise."));
        assert!(p.ends_with("<|turn>model\n"));
        assert!(!p.contains("<|im_start|>"));
        assert!(!p.contains("<start_of_turn>"));
    }

    #[test]
    fn standard_gemma_keeps_classic_turn_markers() {
        let ctx =
            AssistantContext { system_prompt: "Be concise.".into(), ..AssistantContext::default() };
        // Older Gemma builds still spell their markers the classic way.
        let p = build_prompt(&ctx, "hello", "gemma-2-2b-it");
        assert!(p.contains("<start_of_turn>user\nBe concise."));
        assert!(p.ends_with("<start_of_turn>model\n"));
        assert!(!p.contains("<|turn>"));
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
    fn tool_turns_survive_into_the_replayed_prompt() {
        // The defect this guards: `ChatRole::Tool` was dropped and an assistant
        // turn rendered only its `content`, so a turn that switched a light on
        // replayed as the user asking and the model apologising — no call, no
        // result. The model then reasoned from that, announced the device had
        // not responded, and called again.
        let call = ToolCall {
            id: "local-1".into(),
            name: "HassTurnOn".into(),
            arguments: r#"{"area":"Master bedroom","domain":["light"]}"#.into(),
        };
        let mut acted = turn(ChatRole::Assistant, "");
        acted.tool_calls = vec![call];
        let mut answered = turn(ChatRole::Tool, "turned on 3 lights");
        answered.tool_call_id = Some("local-1".into());

        for model in ["gemma-4-e2b-it-Q4_K_M", "qwen3.5-0.8b"] {
            let ctx = AssistantContext {
                system_prompt: "Be concise.".into(),
                history: vec![
                    turn(ChatRole::User, "turn on the light in the master bedroom"),
                    acted.clone(),
                    answered.clone(),
                    turn(ChatRole::Assistant, "Done."),
                ],
                ..AssistantContext::default()
            };
            let p = build_prompt(&ctx, "and the kitchen", model);
            assert!(p.contains("HassTurnOn"), "the call must be replayed for {model:?}");
            assert!(
                p.contains("Master bedroom"),
                "the call's arguments must be replayed for {model:?}"
            );
            assert!(
                p.contains("turned on 3 lights"),
                "the tool's answer must be replayed for {model:?}"
            );
        }
    }

    #[test]
    fn replayed_call_is_spelled_the_way_the_model_is_asked_to_spell_it() {
        // A call read back in a syntax the model was never told to produce
        // teaches it the wrong shape. Both directions go through
        // `local_tools::render_call`, so this pins them together.
        let mut acted = turn(ChatRole::Assistant, "");
        acted.tool_calls = vec![ToolCall {
            id: "local-1".into(),
            name: "HassTurnOff".into(),
            arguments: r#"{"name":"Lampa de sare"}"#.into(),
        }];
        assert_eq!(
            render_assistant(&acted),
            local_tools::render_call("HassTurnOff", r#"{"name":"Lampa de sare"}"#),
        );
    }

    #[test]
    fn narration_and_call_in_one_turn_both_survive() {
        // A model may narrate and then call in the same breath. Keeping only
        // one of the two would leave history describing an action with no
        // action, or an action nobody explained.
        let mut both = turn(ChatRole::Assistant, "Right away.");
        both.tool_calls = vec![ToolCall {
            id: "local-1".into(),
            name: "HassTurnOn".into(),
            arguments: r#"{"area":"Office"}"#.into(),
        }];
        let rendered = render_assistant(&both);
        assert!(rendered.starts_with("Right away."), "narration kept: {rendered:?}");
        assert!(rendered.contains("HassTurnOn"), "call kept: {rendered:?}");
    }

    #[test]
    fn a_cleared_log_rebuilds_the_very_first_prompt() {
        // Why clearing after an action keeps the next command fast. The cache is
        // keyed on content and looked up by token *prefix*, so a head that is no
        // longer at the front is a head that cannot be found — an accumulating
        // log pushes it back and the next command pays a full cold prefill.
        // Emptying the log makes the following command byte-identical to a first
        // one, which is exactly the case the pinned checkpoint was stored for.
        let system = "You are Fono, a terse assistant.";
        for model in ["gemma-4-e2b-it-Q4_K_M", "qwen3.5-0.8b"] {
            let first =
                AssistantContext { system_prompt: system.into(), ..AssistantContext::default() };
            let (fresh_prefix, _) = build_prompt_split(&first, "turn on the office light", model);

            // The same context after a command that actuated something and was
            // then forgotten: history is empty again.
            let after_clearing =
                AssistantContext { system_prompt: system.into(), ..AssistantContext::default() };
            let (next_prefix, _) =
                build_prompt_split(&after_clearing, "turn off the office light", model);
            assert_eq!(
                fresh_prefix, next_prefix,
                "a forgotten command must leave the cacheable prefix untouched for {model:?}"
            );

            // And for contrast: had the exchange been kept, the head would no
            // longer be all there is in front of the new command.
            let mut acted = turn(ChatRole::Assistant, "");
            acted.tool_calls = vec![ToolCall {
                id: "local-1".into(),
                name: "HassTurnOn".into(),
                arguments: r#"{"area":"Office","domain":["light"]}"#.into(),
            }];
            let kept = AssistantContext {
                system_prompt: system.into(),
                history: vec![
                    turn(ChatRole::User, "turn on the office light"),
                    acted,
                    turn(ChatRole::Tool, "turned on 1 light"),
                    turn(ChatRole::Assistant, "Done."),
                ],
                ..AssistantContext::default()
            };
            let (kept_prefix, _) = build_prompt_split(&kept, "turn off the office light", model);
            assert!(
                kept_prefix.len() > next_prefix.len(),
                "keeping the exchange must be the longer prompt for {model:?}"
            );
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
    fn pinned_base_survives_per_turn_system_decoration() {
        // Regression guard for the D2 defect in the voice-actions v3 plan.
        //
        // The daemon pins `F8System` from bare `prompt_main` at startup, but the
        // live turn may decorate the system prompt (today: a speaker-identity
        // note; soon: a tool catalogue). Decoration MUST be appended so
        // `prompt_main` keeps leading. Prepending diverges at roughly token one,
        // which silently turns every decorated turn into a full cold prefill —
        // the pin is never restored and the cache looks like it is working
        // because nothing errors.
        let prompt_main = "You are Fono, a terse assistant.";
        for model in ["gemma-4-e2b-it-Q4_K_M", "qwen3.5-0.8b"] {
            let base = assistant_base_prefix(prompt_main, model);
            assert!(!base.is_empty());

            // APPENDED decoration: base still leads. This is the supported shape.
            let appended = format!("{prompt_main}\n\nThe current speaker is Ana.");
            let ctx = AssistantContext { system_prompt: appended, ..AssistantContext::default() };
            let (prefix, _) = build_prompt_split(&ctx, "turn on the lights", model);
            assert!(
                prefix.starts_with(&base),
                "appended decoration must keep the pinned base leading for {model:?}\n \
                 base: {base:?}\n prefix: {prefix:?}"
            );

            // PREPENDED decoration: base no longer leads. Asserting the negative
            // documents exactly what regressed, so a future refactor that
            // reintroduces prepending fails here instead of silently costing a
            // full prefill on every turn.
            let prepended = format!("The current speaker is Ana.\n\n{prompt_main}");
            let bad_ctx =
                AssistantContext { system_prompt: prepended, ..AssistantContext::default() };
            let (bad_prefix, _) = build_prompt_split(&bad_ctx, "turn on the lights", model);
            assert!(
                !bad_prefix.starts_with(&base),
                "prepended decoration is expected to break the pin for {model:?}; if this \
                 now passes, the cache layout changed and the guard needs revisiting"
            );
        }
    }

    /// The head the warm paths pin must be byte-identical to the head the reply
    /// path sends — including the tool block, and regardless of who is
    /// speaking.
    ///
    /// Three places render it: `build_stable_prompt_caches` (startup),
    /// `prepare_turn_prompt_caches` (hotkey), and `reply_stream` (live). They
    /// all go through `local_tools::head_with_tools`, but nothing in the type
    /// system says they must, and a checkpoint that is not a genuine token
    /// prefix of the live prompt can never be restored — the symptom being a
    /// full cold prefill on every turn while the cache reports a hit on the
    /// bare greeting behind it (F28, F30, F31).
    #[test]
    fn warm_head_leads_every_live_prompt() {
        let prompt_main = "You are Fono, a terse assistant.\n\nAreas: Kitchen, Office.";
        let instructions = "Reply in 1-4 sentences. Match the user's language.";
        let descriptors = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "HassTurnOn",
                "parameters": { "properties": { "area": { "type": "string" } } },
            },
        })];
        // What the two warm paths pin.
        let warm_head =
            crate::local_tools::head_with_tools(prompt_main, &descriptors, Some(instructions));
        // The behavioural rules go last, behind the tool block — a weak model
        // ignored them when they sat fourteen hundred tokens back.
        assert!(
            warm_head.trim_end().ends_with(instructions),
            "instructions must be the tail of the head: {warm_head:?}"
        );

        for model in ["gemma-4-e2b-it-Q4_K_M", "qwen3.5-0.8b"] {
            let base = assistant_base_prefix(&warm_head, model);
            for speaker in [None, Some("Ana"), Some("Bogdan")] {
                // What the reply path composes: the same head, then the
                // volatile speaker note appended behind it.
                let live = crate::compose_system_prompt(
                    &warm_head,
                    speaker.map(|s| format!("The current speaker is {s}.")).as_deref(),
                );
                let ctx = AssistantContext { system_prompt: live, ..AssistantContext::default() };
                let (prefix, _) = build_prompt_split(&ctx, "turn on the lights", model);
                assert!(
                    prefix.starts_with(&base),
                    "warm head must lead the live prompt for {model:?} / {speaker:?}\n \
                     base: {base:?}\n prefix: {prefix:?}"
                );
            }
        }
    }

    /// Two turns spoken in different languages share everything but their last
    /// few words, and a checkpoint is all-or-nothing about its tokens — so a
    /// checkpoint of the whole prompt serves exactly one language and the other
    /// pays to read the house again.
    ///
    /// Asserts the property the head checkpoint depends on: the head leads both
    /// prompts, and the prompts diverge only after it. String level; the token
    /// level is guarded at runtime by `prefix_tokens.starts_with(&head_tokens)`
    /// in [`LlamaLocalAssistant::checkpoint_steady_head`], which needs a real
    /// vocabulary and falls back to reading straight through when it fails.
    #[test]
    fn the_head_leads_a_prompt_in_any_language_and_is_where_they_part() {
        let head = "You are Fono, a terse assistant.\n\nAreas: Kitchen, Office.";
        for model in ["gemma-4-e2b-it-Q4_K_M", "qwen3.5-0.8b"] {
            let base = assistant_base_prefix(head, model);
            let prompt = |code: &str| {
                let ctx = AssistantContext {
                    system_prompt: crate::compose_system_prompt(
                        head,
                        AssistantContext {
                            language: Some(code.into()),
                            ..AssistantContext::default()
                        }
                        .turn_notes()
                        .as_deref(),
                    ),
                    ..AssistantContext::default()
                };
                build_prompt_split(&ctx, "stinge lumina", model).0
            };
            let (en, ro) = (prompt("en"), prompt("ro"));
            assert_ne!(en, ro, "the note must actually differ for {model:?}");
            for p in [&en, &ro] {
                assert!(p.starts_with(&base), "head must lead the prompt for {model:?}: {p:?}");
            }
            let shared =
                en.as_bytes().iter().zip(ro.as_bytes()).take_while(|(a, b)| a == b).count();
            assert!(
                shared >= base.len(),
                "the two languages must share at least the head for {model:?}: \
                 shared {shared} < head {}",
                base.len()
            );
        }
    }

    /// Tools reach this backend through the system prompt, so the two things
    /// that could quietly go wrong are: the block landing *before* the pinned
    /// base (a cold prefill on every turn), and the second pass not continuing
    /// the same prefix (a second cold prefill, on the slowest path there is).
    #[test]
    fn the_tool_block_and_its_follow_up_both_keep_the_pinned_base_leading() {
        let prompt_main = "You are Fono, a terse assistant.";
        let descriptors = vec![serde_json::json!({
            "type": "function",
            "function": {"name": "HassTurnOn", "parameters": {"type": "object",
                "properties": {"area": {"type": "string"}}}}
        })];
        for model in ["gemma-4-e2b-it-Q4_K_M", "qwen3.5-0.8b"] {
            let base = assistant_base_prefix(prompt_main, model);
            let decorated =
                format!("{prompt_main}\n\n{}", crate::local_tools::instructions(&descriptors));
            let ctx = AssistantContext { system_prompt: decorated, ..AssistantContext::default() };
            let (prefix, _) = build_prompt_split(&ctx, "turn on the kitchen lights", model);
            assert!(prefix.starts_with(&base), "tool block must be appended for {model:?}");

            let prompt = build_prompt(&ctx, "turn on the kitchen lights", model);
            let cont = tool_result_continuation(&prompt, "{\"name\":\"x\"}", "done", model);
            assert!(
                cont.starts_with(&prefix),
                "the follow-up turn must continue the cached prefix for {model:?}"
            );
        }
    }

    /// The second pass is only fast if its prompt starts with the exact string
    /// the checkpoint was saved under: the finished prompt, the reply as the
    /// model wrote it, and the model's own turn closer. One trace paid 21.6 s
    /// for a mismatch here, so this pins the two halves together — if the
    /// checkpoint rendering and the continuation rendering ever drift apart,
    /// this test fails instead of the next local turn going slow in silence.
    #[test]
    fn the_wording_pass_starts_where_the_finished_turn_was_saved() {
        for model in ["gemma-4-e2b-it-Q4_K_M", "qwen3.5-0.8b"] {
            let ctx = AssistantContext {
                system_prompt: "You are Fono.".to_string(),
                ..AssistantContext::default()
            };
            let prompt = build_prompt(&ctx, "turn on the office light", model);
            // Exactly what the model emits, wrapper and all — not a tidied
            // re-serialisation of the parsed call.
            let reply = "<tool_call>{\"name\": \"HassTurnOn\", \"arguments\": {\"area\": \
                         \"Office\"}}</tool_call>";
            // How `run_inference_with_prefix_cache` spells the saved checkpoint.
            let saved = format!("{prompt}{reply}{}\n", turn_markers(model).close);
            let cont = tool_result_continuation(&prompt, reply, "it worked", model);
            assert!(
                cont.starts_with(&saved),
                "the wording pass must continue the saved checkpoint for {model:?}"
            );
            assert!(
                cont.len() > saved.len(),
                "the wording pass must add a suffix to generate from for {model:?}"
            );
        }
    }

    /// The whole local tool path against a real model: the block reaches the
    /// prompt, the model answers with a call, the call is parsed and executed,
    /// and the result comes back as spoken words rather than JSON.
    ///
    /// Everything else about this feature is testable without a model. This is
    /// the one thing that is not — whether the model, shown these particular
    /// instructions, actually emits something the parser recognises.
    #[tokio::test]
    #[ignore = "requires FONO_TEST_ASSISTANT_GGUF=/path/to/chat-model.gguf"]
    async fn a_real_model_calls_a_tool_and_words_the_result() {
        use crate::traits::{ActionTools, ToolOutcome};
        use futures::StreamExt;
        use std::sync::{Arc, Mutex};

        let model_path = std::env::var_os("FONO_TEST_ASSISTANT_GGUF")
            .expect("set FONO_TEST_ASSISTANT_GGUF=/path/to/chat-model.gguf");
        let context = std::env::var("FONO_TEST_ASSISTANT_CTX")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(4096);
        let assistant = LlamaLocalAssistant::new(model_path, context);

        let seen: Arc<Mutex<Vec<ToolCall>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        let actions = Arc::new(ActionTools {
            descriptors: vec![serde_json::json!({
                "type": "function",
                "function": {
                    "name": "HassTurnOn",
                    "description": "Turns on lights or devices in an area of the home.",
                    "parameters": {"type": "object", "properties": {
                        "area": {"type": "string"},
                        "domain": {"type": "array", "items": {"type": "string"}}
                    }}
                }
            })],
            hint: Some("Areas in this home: Kitchen, Office.".into()),
            grammar: None,
            said: crate::Said::default(),
            execute: Arc::new(move |call: ToolCall| {
                let recorder = Arc::clone(&recorder);
                Box::pin(async move {
                    recorder.lock().expect("poisoned").push(call);
                    ToolOutcome::worked(
                        "{\"success\": [{\"name\": \"Kitchen light\"}], \"failed\": []}".into(),
                    )
                })
            }),
        });

        let ctx = AssistantContext {
            system_prompt: "You are Fono, a terse voice assistant. Reply in one short sentence."
                .into(),
            actions: Some(actions),
            ..AssistantContext::default()
        };
        let mut stream =
            assistant.reply_stream("turn on the kitchen lights", &ctx).await.expect("stream");
        let mut spoken = String::new();
        let mut called = false;
        while let Some(delta) = stream.next().await {
            let delta = delta.expect("delta");
            match delta.tool_event {
                Some(ToolEvent::Called(_)) => called = true,
                Some(ToolEvent::Result { .. }) => {}
                None => spoken.push_str(&delta.text),
            }
        }

        let calls = seen.lock().expect("poisoned").clone();
        assert!(called, "no tool call was emitted; the model said: {spoken:?}");
        assert_eq!(calls.len(), 1, "expected exactly one call, got {calls:?}");
        assert_eq!(calls[0].name, "HassTurnOn", "wrong tool: {calls:?}");
        assert!(
            calls[0].arguments.to_lowercase().contains("kitchen"),
            "the area did not survive into the arguments: {:?}",
            calls[0].arguments
        );
        // The user must hear words, not the raw tool answer.
        assert!(!spoken.trim().is_empty(), "the turn produced no speech at all");
        assert!(
            !spoken.contains("<tool_call>") && !spoken.contains("\"success\""),
            "raw machinery leaked into speech: {spoken:?}"
        );
        println!("call: {:?}\nspoken: {spoken:?}", calls[0]);
    }

    /// Three places have to agree about how a command opens, and if any two of
    /// them drift the mechanism fails *silently* — which is exactly how the
    /// rails came to be switched on for a year while holding the model to
    /// nothing. The prompt writes the opener for the model on a correction, the
    /// rails watch for it to know when to start constraining, and the parser
    /// looks for it to know a command was written at all. One string, checked
    /// here, because none of the three fails loudly on its own.
    #[test]
    fn the_opener_written_for_the_model_is_the_one_the_rails_and_parser_watch_for() {
        let opener = local_tools::OPEN;
        let patterns = fono_core::tool_grammar::trigger_patterns();
        assert!(
            patterns.iter().any(|p| p.contains(opener)),
            "no rail watches for {opener:?}; a correction would generate unconstrained: {patterns:?}"
        );
        // And the parser reads a command whose opener it did not see generated.
        let written = format!(
            "{opener}{{\"name\": \"HassTurnOn\", \"arguments\": {{\"area\": \"Office\"}}}}"
        );
        let (name, args) = local_tools::parse_call(&written).expect("the parser reads it back");
        assert_eq!(name, "HassTurnOn");
        assert!(args.contains("Office"), "{args}");
    }

    /// A command that failed must be corrected, not apologised for.
    ///
    /// The invitation this replaces was prose — "correct it and call the tool
    /// once more; otherwise tell the user plainly what went wrong" — and a small
    /// model took the second option every time, ending the turn with the lights
    /// still off and the user having to say the whole thing again. So the opener
    /// is now written for it: there is no prose branch to take. This is the only
    /// way to check that, because what is being tested is what the model does
    /// when it is left mid-sentence.
    #[tokio::test]
    #[ignore = "requires FONO_TEST_ASSISTANT_GGUF=/path/to/chat-model.gguf"]
    async fn a_failed_command_is_corrected_rather_than_apologised_for() {
        use crate::traits::{ActionTools, ToolOutcome};
        use futures::StreamExt;
        use std::sync::{Arc, Mutex};

        let model_path = std::env::var_os("FONO_TEST_ASSISTANT_GGUF")
            .expect("set FONO_TEST_ASSISTANT_GGUF=/path/to/chat-model.gguf");
        let context = std::env::var("FONO_TEST_ASSISTANT_CTX")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(4096);
        let assistant = LlamaLocalAssistant::new(model_path, context);

        let seen: Arc<Mutex<Vec<ToolCall>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        let actions = Arc::new(ActionTools {
            descriptors: vec![serde_json::json!({
                "type": "function",
                "function": {
                    "name": "HassTurnOn",
                    "description": "Turns on lights or devices in an area of the home.",
                    "parameters": {"type": "object", "properties": {"area": {"type": "string"}}}
                }
            })],
            hint: Some("Areas in this home: Kitchen, Office.".into()),
            grammar: None,
            said: crate::Said::default(),
            execute: Arc::new(move |call: ToolCall| {
                let recorder = Arc::clone(&recorder);
                Box::pin(async move {
                    let mut log = recorder.lock().expect("poisoned");
                    log.push(call);
                    // The first go fails in a way that changed nothing and names
                    // the problem, which is the shape a real server refuses in.
                    if log.len() == 1 {
                        ToolOutcome {
                            summary: "No such area. Nothing was changed.".into(),
                            failed: true,
                            retryable: true,
                            sent: None,
                            repeat_ok: false,
                            confirmed: false,
                        }
                    } else {
                        ToolOutcome::worked(
                            "{\"success\": [{\"name\": \"Office light\"}], \"failed\": []}".into(),
                        )
                    }
                })
            }),
        });

        let ctx = AssistantContext {
            system_prompt: "You are Fono, a terse voice assistant. Reply in one short sentence."
                .into(),
            actions: Some(actions),
            ..AssistantContext::default()
        };
        let mut stream =
            assistant.reply_stream("turn on the office light", &ctx).await.expect("stream");
        let mut spoken = String::new();
        while let Some(delta) = stream.next().await {
            let delta = delta.expect("delta");
            if delta.tool_event.is_none() {
                spoken.push_str(&delta.text);
            }
        }

        let calls = seen.lock().expect("poisoned").clone();
        assert_eq!(
            calls.len(),
            2,
            "a failure that changed nothing must be corrected, not spoken about: {calls:?} \
             said {spoken:?}"
        );
        assert!(
            !spoken.contains("<tool_call>") && !spoken.contains('{'),
            "the correction leaked into speech: {spoken:?}"
        );
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
        assert!(prefix.starts_with("<|turn>user\nYou are a helpful assistant.\n\n"));
        assert!(prefix.ends_with("\n\n"));
        assert!(!suffix.contains("You are a helpful assistant."));
        assert_eq!(suffix, "what time is it<turn|>\n<|turn>model\n");
    }

    #[test]
    fn gemma_system_leads_prompt_regardless_of_history() {
        // The whole cache scheme rests on the system prompt being a *leading*
        // token prefix. If a refactor ever pushes it back into the per-turn
        // tail (the old, un-cacheable layout), this fails loudly.
        let boot = "<|turn>user\nYou are Fono.\n\n";
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
    fn gemma_system_leads_even_when_history_opens_on_a_reply() {
        // The rolling window drops turns off the front and turns arrive in
        // pairs, so half the time the survivor at the front is a model reply.
        // The system block must still lead: buried behind a model turn it stops
        // being a token prefix, the pinned base can never match, and the turn
        // is prefilled cold from nothing.
        let system = "You are Fono.";
        let base = assistant_base_prefix(system, "gemma-4-e2b-it");
        let histories = [
            vec![turn(ChatRole::Assistant, "reply one"), turn(ChatRole::User, "turn two")],
            vec![turn(ChatRole::Tool, "the light is on"), turn(ChatRole::User, "turn two")],
        ];
        for history in histories {
            let ctx = AssistantContext {
                system_prompt: system.into(),
                history,
                ..AssistantContext::default()
            };
            let full = build_prompt(&ctx, "current question", "gemma-4-e2b-it");
            assert!(full.starts_with(&base), "pinned base must lead; got: {full}");
            assert_eq!(full.matches(system).count(), 1, "system rendered twice: {full}");
        }
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
        // Regression for the current-turn double-count bug: the
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

    /// Live two-turn conversation through the real `reply_stream` prefix-cache
    /// path, asserting the post-generation "completed turn" checkpoint stored
    /// on turn 1 is actually matched and restored on turn 2. Guards the
    /// canonical-closer interplay (`generate_with_prefix_cache` renders the
    /// turn closer textually) against decoding-policy changes — specifically
    /// the shared Control-attribute stop, which on `gemma-4-e2b` stops on a
    /// token the hand-rolled template spells differently.
    #[tokio::test]
    #[ignore = "requires FONO_TEST_ASSISTANT_GGUF=/path/to/chat-model.gguf"]
    async fn completed_turn_checkpoint_is_restored_next_turn() {
        use fono_core::turn_trace::TurnTrace;
        use futures::StreamExt;

        let model_path = std::env::var_os("FONO_TEST_ASSISTANT_GGUF")
            .expect("set FONO_TEST_ASSISTANT_GGUF=/path/to/chat-model.gguf");
        let context = std::env::var("FONO_TEST_ASSISTANT_CTX")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(4096);
        let assistant = LlamaLocalAssistant::new(model_path, context);

        let trace_dir =
            std::env::temp_dir().join(format!("fono-cache-test-{}", std::process::id()));
        let trace = TurnTrace::start_in(&trace_dir);
        let guard = trace.make_current();

        let system = "You are Fono, a terse assistant. Reply with one short sentence.";
        let mut history: Vec<ChatTurn> = Vec::new();
        for user in ["turn the lights on", "now dim them to fifty percent"] {
            let ctx = AssistantContext {
                system_prompt: system.into(),
                history: history.clone(),
                ..AssistantContext::default()
            };
            let mut stream = assistant.reply_stream(user, &ctx).await.expect("reply_stream");
            let mut text = String::new();
            while let Some(delta) = stream.next().await {
                text.push_str(&delta.expect("token delta").text);
            }
            assert!(!text.trim().is_empty(), "empty reply for {user:?}");
            history.push(turn(ChatRole::User, user));
            history.push(turn(ChatRole::Assistant, &text));
        }

        guard.clear();
        trace.finish(serde_json::json!({}));
        let raw = std::fs::read_to_string(trace.path()).expect("read trace file");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse trace JSON");
        let events = parsed["traceEvents"].as_array().expect("traceEvents array");
        // Turn 1 must store a completed-turn checkpoint (the canonical closer
        // rendering must overlap the sampled reply beyond the prompt)...
        assert!(
            events.iter().any(|e| e["name"] == "llm.prompt_cache_completed_turn"),
            "no completed-turn checkpoint stored; canonical closer no longer overlaps the reply"
        );
        // ...and turn 2 must restore it via longest-prefix matching.
        assert!(
            events.iter().any(|e| e["name"] == "llm.prompt_cache_prefix_match"
                && e["args"]["matched_layer"] == "f8_chat_prefix"),
            "turn 2 never matched the completed-turn (f8_chat_prefix) checkpoint"
        );
    }

    /// Live regression for the `fono.summarize` reuse pattern: the SAME
    /// system prompt with empty history but varying user text on every call.
    /// The first call is a cold prefill that must store the pre-suffix prefix
    /// (system prompt) checkpoint; the second must restore it instead of
    /// re-prefilling the whole system prompt. Without the cold-prefill
    /// checkpoint store, every summary pays the full system-prompt prefill.
    ///
    /// Run the live cache tests with `--test-threads=1`: two models loading
    /// concurrently contend on the shared llama backend and skew the trace.
    #[tokio::test]
    #[ignore = "requires FONO_TEST_ASSISTANT_GGUF=/path/to/chat-model.gguf"]
    async fn repeated_prefix_prompt_restores_cached_system_prefix() {
        use fono_core::turn_trace::TurnTrace;
        use futures::StreamExt;

        let model_path = std::env::var_os("FONO_TEST_ASSISTANT_GGUF")
            .expect("set FONO_TEST_ASSISTANT_GGUF=/path/to/chat-model.gguf");
        let context = std::env::var("FONO_TEST_ASSISTANT_CTX")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(4096);
        let assistant = LlamaLocalAssistant::new(model_path, context);

        let trace_dir =
            std::env::temp_dir().join(format!("fono-summarize-cache-{}", std::process::id()));
        let trace = TurnTrace::start_in(&trace_dir);
        let guard = trace.make_current();

        let system = "You summarize notifications. Reply with one short spoken sentence.";
        for user in ["Mihai reports a failed deploy.", "Ana asks about the staging build."] {
            let ctx = AssistantContext {
                system_prompt: system.into(),
                max_new_tokens: Some(48),
                ..AssistantContext::default()
            };
            let mut stream = assistant.reply_stream(user, &ctx).await.expect("reply_stream");
            let mut text = String::new();
            while let Some(delta) = stream.next().await {
                text.push_str(&delta.expect("token delta").text);
            }
            assert!(!text.trim().is_empty(), "empty reply for {user:?}");
        }

        guard.clear();
        trace.finish(serde_json::json!({}));
        let raw = std::fs::read_to_string(trace.path()).expect("read trace file");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse trace JSON");
        let events = parsed["traceEvents"].as_array().expect("traceEvents array");
        // Call 1 (cold) must store the pre-suffix system-prompt checkpoint...
        assert!(
            events.iter().any(|e| e["name"] == "llm.prompt_cache_prefix_stored"),
            "cold call never stored the pre-suffix prefix checkpoint"
        );
        // ...and call 2 must restore it (the varying user text lives only in
        // the suffix, so the system-prompt prefix is identical token-for-token).
        assert!(
            events.iter().any(|e| e["name"] == "llm.prompt_cache_prefix_match"),
            "second summary never restored the cached system-prompt prefix"
        );
    }

    /// Live runtime confirmation of the D2 prompt-cache fix (voice-actions v3).
    ///
    /// The daemon pins `F8System` from bare `prompt_main` at startup, but a
    /// speaker-verified turn decorates the system prompt with an identity note.
    /// The fix APPENDS that note (`assistant_system_prompt`) so `prompt_main`
    /// keeps leading and the pin stays a genuine token prefix. This drives the
    /// real warmup → decorated-turn path and asserts through the trace that the
    /// pinned `F8System` base is actually *restored* rather than cold-prefilled
    /// — the number that says the caching fix is real at runtime, not just as a
    /// string invariant (which `pinned_base_survives_per_turn_system_decoration`
    /// already proves without a model).
    ///
    /// Run with `--test-threads=1` (see the sibling live-cache tests).
    #[tokio::test]
    #[ignore = "requires FONO_TEST_ASSISTANT_GGUF=/path/to/chat-model.gguf"]
    async fn appended_speaker_note_restores_pinned_f8_system() {
        use fono_core::turn_trace::TurnTrace;
        use futures::StreamExt;

        let model_path = std::env::var_os("FONO_TEST_ASSISTANT_GGUF")
            .expect("set FONO_TEST_ASSISTANT_GGUF=/path/to/chat-model.gguf");
        let context = std::env::var("FONO_TEST_ASSISTANT_CTX")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(4096);
        let assistant = LlamaLocalAssistant::new(model_path, context);

        let prompt_main = "You are Fono, a terse voice assistant. Reply with one short sentence.";

        // Pin `F8System` from bare `prompt_main`, exactly as the daemon startup
        // warmup does (`assistant_cache_warmup`).
        assistant
            .prewarm_prompt_caches(AssistantPromptCacheWarmup {
                f8_system_prompt: Some(prompt_main.to_string()),
                ..AssistantPromptCacheWarmup::default()
            })
            .await
            .expect("warmup");

        // Trace only the live turn, so the assertion sees its cache decision and
        // not the warmup's build instants.
        let trace_dir = std::env::temp_dir().join(format!("fono-d2-cache-{}", std::process::id()));
        let trace = TurnTrace::start_in(&trace_dir);
        let guard = trace.make_current();

        // The D2 fix shape: identity note APPENDED, `prompt_main` still leading.
        let decorated = format!("{prompt_main}\n\nThe current speaker is Ana.");
        let ctx = AssistantContext {
            system_prompt: decorated,
            max_new_tokens: Some(24),
            ..AssistantContext::default()
        };
        let mut stream =
            assistant.reply_stream("turn on the kitchen lights", &ctx).await.expect("reply_stream");
        let mut text = String::new();
        while let Some(delta) = stream.next().await {
            text.push_str(&delta.expect("token delta").text);
        }
        assert!(!text.trim().is_empty(), "empty reply");

        guard.clear();
        trace.finish(serde_json::json!({}));
        let raw = std::fs::read_to_string(trace.path()).expect("read trace file");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse trace JSON");
        let events = parsed["traceEvents"].as_array().expect("traceEvents array");

        // The pinned `F8System` base must be restored on the decorated turn.
        assert!(
            events.iter().any(|e| e["name"] == "llm.prompt_cache_prefix_match"
                && e["args"]["matched_layer"] == "f8_system"),
            "appended speaker note did NOT restore the pinned F8System base — D2 regression"
        );
        // And the turn must not have cold-prefilled for lack of a prefix.
        assert!(
            !events.iter().any(|e| e["name"] == "llm.prompt_cache_cold_prefill"
                && e["args"]["reason"] == "no_prefix_match"),
            "turn cold-prefilled despite the pinned F8System base — D2 regression"
        );
    }

    /// A note in the system prompt must not cost a cold read of the whole
    /// device list on every turn.
    ///
    /// A prompt carrying a per-turn note is deliberately never pinned, so its
    /// checkpoint is filed under `history_prefix` — and the longest-prefix
    /// search skips an entry exactly as long as the prefix it is looking for,
    /// because for a whole prompt that would leave nothing to decode. Between
    /// those two rules a repeated note fell through every lookup: turn after
    /// turn re-read a prefix it had already read. Measured on the command
    /// benchmark, every one of 22 turns was cold and the middle turn took 4.5×
    /// as long, for a note identical on all of them.
    ///
    /// Deliberately warms nothing: the pinned base is the path the sibling test
    /// covers, and its presence would mask this one by matching first.
    ///
    /// Run with `--test-threads=1` (see the sibling live-cache tests).
    #[tokio::test]
    #[ignore = "requires FONO_TEST_ASSISTANT_GGUF=/path/to/chat-model.gguf"]
    async fn a_repeated_note_is_read_once_not_once_per_turn() {
        use fono_core::turn_trace::TurnTrace;
        use futures::StreamExt;

        let model_path = std::env::var_os("FONO_TEST_ASSISTANT_GGUF")
            .expect("set FONO_TEST_ASSISTANT_GGUF=/path/to/chat-model.gguf");
        let context = std::env::var("FONO_TEST_ASSISTANT_CTX")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(4096);
        let assistant = LlamaLocalAssistant::new(model_path, context);

        let ctx = AssistantContext {
            system_prompt: "You are Fono, a terse voice assistant. Reply with one short \
                            sentence.\n\nReply in English."
                .into(),
            max_new_tokens: Some(24),
            ..AssistantContext::default()
        };
        let say = |text: &'static str| {
            let assistant = &assistant;
            let ctx = ctx.clone();
            async move {
                let mut stream =
                    assistant.reply_stream(text, &ctx).await.expect("reply_stream").boxed();
                let mut out = String::new();
                while let Some(delta) = stream.next().await {
                    out.push_str(&delta.expect("token delta").text);
                }
                assert!(!out.trim().is_empty(), "empty reply");
            }
        };

        // Turn one pays for the prefix and checkpoints it.
        say("turn on the kitchen lights").await;

        // Only turn two is traced, so the assertion sees its cache decision.
        let trace_dir =
            std::env::temp_dir().join(format!("fono-note-cache-{}", std::process::id()));
        let trace = TurnTrace::start_in(&trace_dir);
        let guard = trace.make_current();
        say("turn off the kitchen lights").await;
        guard.clear();
        trace.finish(serde_json::json!({}));

        let raw = std::fs::read_to_string(trace.path()).expect("read trace file");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse trace JSON");
        let events = parsed["traceEvents"].as_array().expect("traceEvents array");
        assert!(
            !events.iter().any(|e| e["name"] == "llm.prompt_cache_cold_prefill"
                && e["args"]["reason"] == "no_prefix_match"),
            "a turn whose note was unchanged still cold-read its whole prefix"
        );
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
