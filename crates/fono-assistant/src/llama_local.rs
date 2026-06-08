// SPDX-License-Identifier: GPL-3.0-only
//! Embedded llama.cpp assistant backend for local GGUF chat models.
//!
//! This is the default path for the wizard's `local` assistant. The
//! OpenAI-compatible/Ollama client remains available when the user manually
//! configures an explicit local server URL.

#![allow(clippy::significant_drop_tightening)]

use std::collections::{HashMap, VecDeque};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use fono_core::turn_trace::{current_instant, current_span};
use futures::stream::{BoxStream, StreamExt};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
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

static LLAMA_LOG_INIT: Once = Once::new();

fn init_llama_logging() {
    LLAMA_LOG_INIT.call_once(|| {
        llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default());
    });
}

fn backend() -> &'static LlamaBackend {
    static BACKEND: OnceLock<LlamaBackend> = OnceLock::new();
    BACKEND.get_or_init(|| {
        init_llama_logging();
        LlamaBackend::init().expect(
            "LlamaBackend::init() failed — another library has already initialised llama.cpp",
        )
    })
}

pub struct LlamaLocalAssistant {
    model_path: PathBuf,
    context_size: u32,
    threads: i32,
    batch_size: Option<u32>,
    ubatch_size: Option<u32>,
    state: Arc<Mutex<Option<LlamaModel>>>,
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PromptStateCacheLayer {
    F7System,
    F8System,
    AssistantTools,
    WindowContext,
    BenchmarkPrefix,
    ExactPrompt,
}

impl PromptStateCacheLayer {
    fn as_str(&self) -> &'static str {
        match self {
            Self::F7System => "f7_system",
            Self::F8System => "f8_system",
            Self::AssistantTools => "assistant_tools",
            Self::WindowContext => "window_context",
            Self::BenchmarkPrefix => "benchmark_prefix",
            Self::ExactPrompt => "exact_prompt",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PromptStateCacheKey {
    layer: PromptStateCacheLayer,
    runtime_sha256: String,
    prompt_sha256: String,
    token_sha256: String,
    token_count: usize,
}

impl PromptStateCacheKey {
    fn stable_id(&self) -> String {
        format!(
            "{:?}:runtime={}:prompt={}:tokens={}:count={}",
            self.layer,
            self.runtime_sha256,
            self.prompt_sha256,
            self.token_sha256,
            self.token_count
        )
    }
}

#[derive(Debug, Clone)]
struct PromptStateCacheEntry {
    state: Vec<u8>,
    token_count: usize,
}

#[derive(Debug)]
struct PromptStateCache {
    max_entries: usize,
    max_bytes: usize,
    bytes: usize,
    entries: HashMap<PromptStateCacheKey, PromptStateCacheEntry>,
    lru: VecDeque<PromptStateCacheKey>,
}

impl Default for PromptStateCache {
    fn default() -> Self {
        Self {
            max_entries: 8,
            max_bytes: 256 * 1024 * 1024,
            bytes: 0,
            entries: HashMap::new(),
            lru: VecDeque::new(),
        }
    }
}

impl PromptStateCache {
    fn insert(&mut self, key: PromptStateCacheKey, entry: PromptStateCacheEntry) {
        if let Some(old) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(old.state.len());
            self.lru.retain(|existing| existing != &key);
        }
        self.bytes = self.bytes.saturating_add(entry.state.len());
        self.lru.push_back(key.clone());
        self.entries.insert(key, entry);
        self.evict_over_budget();
    }

    fn get(&mut self, key: &PromptStateCacheKey) -> Option<PromptStateCacheEntry> {
        let entry = self.entries.get(key).cloned()?;
        self.lru.retain(|existing| existing != key);
        self.lru.push_back(key.clone());
        Some(entry)
    }

    fn contains(&mut self, key: &PromptStateCacheKey) -> bool {
        self.get(key).is_some()
    }

    fn remove_layer(&mut self, layer: PromptStateCacheLayer) {
        let removed: Vec<_> = self.entries.keys().filter(|k| k.layer == layer).cloned().collect();
        for key in removed {
            if let Some(entry) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(entry.state.len());
            }
            self.lru.retain(|existing| existing != &key);
        }
    }

    fn evict_over_budget(&mut self) {
        while self.entries.len() > self.max_entries || self.bytes > self.max_bytes {
            let Some(key) = self.lru.pop_front() else { break };
            if let Some(entry) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(entry.state.len());
            }
        }
    }
}

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
        let model = LlamaModel::load_from_file(
            backend(),
            &self.model_path,
            &LlamaModelParams::default(),
        )
        .with_context(|| format!("loading assistant GGUF model from {:?}", self.model_path))?;
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

    /// Look up a cached stable-prefix checkpoint for the live reply path.
    ///
    /// Part of the staged Task 8 generation-time prefix-cache path. Not yet
    /// wired into [`Self::reply_stream`]: the stable checkpoints are currently
    /// built from raw prompt text, which is not a token-prefix of the
    /// chat-templated reply prompt, so restore-and-suffix would always fall
    /// back. Kept ready for benchmark-driven promotion (plan tasks 8/16).
    #[allow(dead_code)]
    fn prompt_prefix_cache_entry(
        &self,
        model: &LlamaModel,
        layer: PromptStateCacheLayer,
        prefix: &str,
    ) -> Result<Option<(PromptStateCacheEntry, PromptStateCacheKey)>> {
        let prefix_tokens =
            model.str_to_token(prefix, AddBos::Always).context("tokenize prompt prefix")?;
        if prefix_tokens.is_empty() {
            return Ok(None);
        }
        let key = self.prompt_state_cache_key(layer, prefix, &prefix_tokens)?;
        let entry = self
            .prompt_state_cache
            .lock()
            .map_err(|_| anyhow!("llama-local prompt-state cache mutex poisoned"))?
            .get(&key);
        Ok(entry.map(|entry| (entry, key)))
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
        cache.insert(key, PromptStateCacheEntry { state, token_count: prefix_tokens.len() });
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

    /// Staged Task 8 generation-time path (see [`Self::prompt_prefix_cache_entry`]).
    #[allow(dead_code)]
    fn try_run_inference_with_cached_prefix<F>(
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
        let Some((entry, key)) = self.prompt_prefix_cache_entry(model, layer.clone(), prefix)?
        else {
            current_instant(
                "llm.prompt_cache_miss",
                "assistant.llm",
                "llm",
                json!({ "layer": layer.as_str() }),
            );
            return Ok(None);
        };
        let full_prompt = format!("{prefix}{suffix}");
        let full_tokens =
            model.str_to_token(&full_prompt, AddBos::Always).context("tokenize cached prompt")?;
        let prefix_tokens =
            model.str_to_token(prefix, AddBos::Always).context("tokenize cached prefix")?;
        if entry.token_count != prefix_tokens.len() || !full_tokens.starts_with(&prefix_tokens) {
            debug!(
                layer = layer.as_str(),
                cached_tokens = entry.token_count,
                prefix_tokens = prefix_tokens.len(),
                "prompt-state cache token split incompatible; falling back"
            );
            return Ok(None);
        }
        let suffix_tokens = &full_tokens[prefix_tokens.len()..];
        if suffix_tokens.is_empty() {
            return Ok(None);
        }
        let restore_started = Instant::now();
        let mut ctx = self.new_context(model, "llm.prompt_cache_context_created")?;
        let restored_bytes = unsafe { ctx.set_state_data(&entry.state) };
        let restore_ms = restore_started.elapsed().as_millis() as u64;
        if restored_bytes == 0 {
            warn!(layer = layer.as_str(), "llama.cpp failed to restore prompt-state cache");
            return Ok(None);
        }
        current_instant(
            "llm.prompt_cache_restored",
            "assistant.llm",
            "llm",
            json!({
                "layer": layer.as_str(),
                "cache_key": key.stable_id(),
                "state_bytes": entry.state.len(),
                "restored_bytes": restored_bytes,
                "restore_ms": restore_ms,
                "suffix_tokens": suffix_tokens.len(),
            }),
        );
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
        Ok(Some(generation.text.trim().to_string()))
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

    /// Staged Task 8 generation-time path (see [`Self::prompt_prefix_cache_entry`]).
    #[allow(dead_code)]
    fn run_inference_with_prompt_cache<F>(
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
        if let Some(text) =
            self.try_run_inference_with_cached_prefix(model, prefix, suffix, layer, &mut on_delta)?
        {
            return Ok(text);
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
            let entry =
                PromptStateCacheEntry { state: state.clone(), token_count: prefix_tokens.len() };
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
        Ok(PromptStateCacheKey {
            layer,
            runtime_sha256: sha256_text(&runtime_identity),
            prompt_sha256: sha256_text(prompt),
            token_sha256: sha256_tokens(tokens),
            token_count: tokens.len(),
        })
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

    /// Build the stable F7/F8/tool prompt-state checkpoints from a startup/idle
    /// warmup request. Each prompt is prefilled once and its llama.cpp state is
    /// stored in the in-memory LRU so a later hotkey press only pays the cheap
    /// restore. Missing or empty prompts are skipped. Plan task 4.
    fn build_stable_prompt_caches(&self, warmup: &AssistantPromptCacheWarmup) -> Result<()> {
        let guard = self.state.lock().map_err(|_| anyhow!("llama-local mutex poisoned"))?;
        let model = guard.as_ref().ok_or_else(|| anyhow!("llama-local model not loaded"))?;
        for (layer, prompt) in [
            (PromptStateCacheLayer::F7System, warmup.f7_system_prompt.as_deref()),
            (PromptStateCacheLayer::F8System, warmup.f8_system_prompt.as_deref()),
            (PromptStateCacheLayer::AssistantTools, warmup.assistant_tool_prompt.as_deref()),
        ] {
            if let Some(prompt) = prompt.filter(|s| !s.trim().is_empty()) {
                self.build_prompt_prefix_cache(model, layer, prompt)?;
            }
        }
        Ok(())
    }

    /// Hotkey-time cache preparation. Ensures the matching stable F7/F8
    /// checkpoint exists (building it if startup warming hasn't run yet — the
    /// build is a no-op cache hit when it has), then, when active-window context
    /// is available, rebuilds the dynamic window-context checkpoint. The window
    /// layer is invalidated first so a window change can never restore stale
    /// context (plan tasks 5–7/9).
    fn prepare_turn_prompt_caches(&self, snapshot: &AssistantPromptCacheSnapshot) -> Result<()> {
        let guard = self.state.lock().map_err(|_| anyhow!("llama-local mutex poisoned"))?;
        let model = guard.as_ref().ok_or_else(|| anyhow!("llama-local model not loaded"))?;
        let stable_layer = match snapshot.trigger {
            AssistantCacheTrigger::F7 => PromptStateCacheLayer::F7System,
            AssistantCacheTrigger::F8 => PromptStateCacheLayer::F8System,
        };
        if !snapshot.system_prompt.trim().is_empty() {
            self.build_prompt_prefix_cache(model, stable_layer, &snapshot.system_prompt)?;
        }
        if let Some(window) =
            snapshot.active_window_context.as_deref().filter(|s| !s.trim().is_empty())
        {
            let combined = if snapshot.system_prompt.trim().is_empty() {
                window.trim().to_string()
            } else {
                format!("{}\n\n{}", snapshot.system_prompt.trim(), window.trim())
            };
            {
                let mut cache = self
                    .prompt_state_cache
                    .lock()
                    .map_err(|_| anyhow!("llama-local prompt-state cache mutex poisoned"))?;
                cache.remove_layer(PromptStateCacheLayer::WindowContext);
            }
            self.build_prompt_prefix_cache(model, PromptStateCacheLayer::WindowContext, &combined)?;
        }
        Ok(())
    }
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

struct GenerationResult {
    text: String,
    elapsed_ms: u64,
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
    Ok(GenerationResult { text: out, elapsed_ms })
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

fn build_prompt(ctx: &AssistantContext, user_text: &str, model_name: &str) -> String {
    if model_name.to_ascii_lowercase().contains("gemma") {
        build_gemma_prompt(ctx, user_text)
    } else {
        build_chatml_prompt(ctx, user_text)
    }
}

fn build_gemma_prompt(ctx: &AssistantContext, user_text: &str) -> String {
    let mut s = String::new();
    let system = ctx.system_prompt.trim();
    let current_user = if system.is_empty() {
        user_text.to_string()
    } else {
        format!("{system}\n\nUser request: {user_text}")
    };
    for turn in &ctx.history {
        match turn.role {
            ChatRole::User => push_gemma_turn(&mut s, "user", &turn.content),
            ChatRole::Assistant => push_gemma_turn(&mut s, "model", &turn.content),
            ChatRole::System => push_gemma_turn(&mut s, "user", &turn.content),
            ChatRole::Tool => {}
        }
    }
    push_gemma_turn(&mut s, "user", &current_user);
    s.push_str("<start_of_turn>model\n");
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

fn build_chatml_prompt(ctx: &AssistantContext, user_text: &str) -> String {
    let mut s = String::new();
    if !ctx.system_prompt.trim().is_empty() {
        s.push_str("<|im_start|>system\n");
        s.push_str(ctx.system_prompt.trim());
        s.push_str("<|im_end|>\n");
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
        s.push_str("<|im_start|>");
        s.push_str(role);
        s.push('\n');
        s.push_str(turn.content.trim());
        s.push_str("<|im_end|>\n");
    }
    s.push_str("<|im_start|>user\n");
    s.push_str(user_text.trim());
    s.push_str("<|im_end|>\n<|im_start|>assistant\n");
    s
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
