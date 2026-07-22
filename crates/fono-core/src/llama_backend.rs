// SPDX-License-Identifier: GPL-3.0-only
//! Process-wide llama.cpp backend singleton.
//!
//! `llama_cpp_2::LlamaBackend::init()` flips global state inside
//! llama.cpp and may be called at most once per process; a second call
//! returns `BackendAlreadyInitialized`. Both `fono-polish` (cleanup)
//! and `fono-assistant` (voice chat) embed llama.cpp, so the backend
//! handle MUST live in one shared place rather than each crate owning
//! its own `OnceLock`. Two independent `OnceLock`s mean two `init()`
//! calls: whichever backend loads second panics inside `get_or_init`
//! while holding its model `state` mutex, poisoning it — observed at
//! runtime as `llama-local mutex poisoned` on the assistant stream
//! after a polish turn (or vice versa). Routing both crates through
//! this single singleton guarantees exactly one init per process.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once, OnceLock, Weak};

use anyhow::{Context, Result};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::LlamaModel;

static LLAMA_LOG_INIT: Once = Once::new();

/// Redirect llama.cpp + ggml's chatty stderr logging through `tracing`
/// so the daemon's normal log filter governs it (the `info` filter pins
/// the `llama-cpp-2` target to `warn`). Idempotent; safe to call from
/// either the polish or assistant path. Mirrors the equivalent
/// `whisper_rs::install_logging_hooks` hook in `fono-stt`.
pub fn init_llama_logging() {
    LLAMA_LOG_INIT.call_once(|| {
        llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default());
    });
}

/// Shared process-wide llama.cpp backend. Initialised exactly once on
/// first use, regardless of whether the polish or assistant path gets
/// there first. Subsequent callers — including a daemon hot-swap into a
/// fresh `LlamaLocal` — reuse the cached handle instead of re-binding.
pub fn backend() -> &'static LlamaBackend {
    static BACKEND: OnceLock<LlamaBackend> = OnceLock::new();
    BACKEND.get_or_init(|| {
        // Install the tracing redirector before the first backend init so
        // backend-init's own log lines (CPU feature detection, etc.) go
        // through tracing rather than straight to stderr.
        init_llama_logging();
        LlamaBackend::init()
            .expect("LlamaBackend::init() failed — llama.cpp could not initialise its backend")
    })
}

/// Load `path` as a shared, process-wide `Arc<LlamaModel>`, deduplicating
/// repeated loads of the same file.
///
/// The polish (F7 cleanup) and assistant (F8 chat) embedded backends both
/// resolve their local GGUF from `polish_models_dir` (see
/// `fono::session` wiring and `fono-assistant`'s `resolve_local_model_path`),
/// so when they are configured to the same model — the default `gemma-4-e2b`
/// for both — they point at the *same* path. Without this registry each
/// backend would `LlamaModel::load_from_file` an independent copy: ~2× the
/// 3.2 GB resident set, two model loads, and (at startup) two prefills
/// fighting for the CPU. Routing both through here means one mmap, one set of
/// weights, half the memory.
///
/// Entries are held **weakly**: each backend keeps the strong `Arc` in its own
/// `state`, so a daemon hot-swap that drops the old backend frees the weights
/// once nothing references them. Keyed by canonicalized path **plus** the
/// load-time knobs that change the resident layout (`n_gpu_layers`, `use_mmap`,
/// `use_mlock`): a caller loading the same file with the same knobs shares one
/// resident copy, while the same file loaded with different per-role params
/// (e.g. polish on `default()` vs assistant on [`streaming_model_params`])
/// loads as separate entries rather than silently reusing the first variant.
///
/// # Errors
/// Propagates `llama.cpp`'s load failure (missing/corrupt GGUF, OOM, …).
pub fn shared_model(path: &Path, params: &LlamaModelParams) -> Result<Arc<LlamaModel>> {
    static MODELS: OnceLock<Mutex<HashMap<ModelKey, Weak<LlamaModel>>>> = OnceLock::new();
    let key = ModelKey::new(path, params);
    let registry = MODELS.get_or_init(|| Mutex::new(HashMap::new()));
    // Held across the (slow) load on purpose: a concurrent request for the
    // same key then waits and reuses the freshly-loaded weights instead of
    // racing into a second load. Lock ordering: this registry mutex is always
    // the innermost lock a backend takes (after its own `state` mutex), so no
    // deadlock. Distinct keys serialise their loads, which is fine — the
    // startup prewarms are already serialised upstream.
    let mut map = registry.lock().expect("llama shared-model registry mutex poisoned");
    if let Some(model) = map.get(&key).and_then(Weak::upgrade) {
        return Ok(model);
    }
    let model = Arc::new(
        LlamaModel::load_from_file(backend(), path, params)
            .with_context(|| format!("loading GGUF model from {path:?}"))?,
    );
    map.insert(key, Arc::downgrade(&model));
    drop(map);
    Ok(model)
}

/// Cache key for [`shared_model`]: the canonicalized path together with the
/// load-time params that materially change the resident layout. Two loads that
/// agree on all of these can safely share one `Arc<LlamaModel>`; any difference
/// must load a separate copy (see the `shared_model` doc and Phase B).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ModelKey {
    path: PathBuf,
    n_gpu_layers: i32,
    use_mmap: bool,
    use_mlock: bool,
}

impl ModelKey {
    fn new(path: &Path, params: &LlamaModelParams) -> Self {
        Self {
            path: path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
            n_gpu_layers: params.n_gpu_layers(),
            use_mmap: params.use_mmap(),
            use_mlock: params.use_mlock(),
        }
    }
}

/// Model-load params for the larger-than-RAM streaming regime: **mmap on**
/// (weights stay file-backed and page in on demand instead of being copied into
/// anonymous RAM) and **mlock off** (never pin — pinning a model bigger than
/// RAM is an OOM). CPU-only (`n_gpu_layers = 0`); GPU offload is a separate,
/// still-unproven effort. This is what the assistant role uses so a selected
/// asym MoE streams from SSD rather than being resident in full; the small
/// dense polish models keep `LlamaModelParams::default()`.
#[must_use]
pub fn streaming_model_params() -> LlamaModelParams {
    LlamaModelParams::default().with_use_mmap(true).with_use_mlock(false).with_n_gpu_layers(0)
}

/// Default llama.cpp decode thread count: all available logical cores
/// (clamped to a sane minimum of 4 when the platform can't report).
///
/// Used by the one-shot (non-streaming) inference paths, which have no
/// concurrent consumer to share the machine with and so want every core.
#[must_use]
pub fn decode_threads() -> i32 {
    std::thread::available_parallelism().map(|n| i32::try_from(n.get()).unwrap_or(4)).unwrap_or(4)
}

/// Decode thread count that **reserves one core** for a concurrent streaming
/// consumer (F7 streaming text injection, F8 streaming TTS synthesis).
///
/// llama.cpp CPU decode is barrier-synchronized across all of its threads on
/// every token. When a streaming consumer runs on the same fully saturated
/// machine — waking roughly once per decoded token to drain the channel, run
/// gate checks, and call the injector / TTS — it preempts a decode thread, and
/// every *other* decode thread then stalls at the per-token barrier waiting
/// for it. Measured on an 8-core host this dragged generation from ~22 tok/s
/// (no concurrent consumer) down to ~13–15 tok/s; reserving one core for the
/// consumer recovered it to ~26 tok/s.
///
/// Falls back to the full count on ≤2-core hosts, where reserving a core would
/// halve decode throughput and hurt more than the contention it avoids.
#[must_use]
pub fn streaming_decode_threads() -> i32 {
    let all = decode_threads();
    if all > 2 {
        all - 1
    } else {
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Phase B regression: the shared-model cache key must fold in the load-time
    // params that change resident layout, so per-role variants of the *same*
    // file don't silently reuse the first-loaded copy. `path.canonicalize()`
    // falls back to the literal path when the file is absent, so these tests
    // need no real GGUF on disk.
    fn key(path: &str, params: &LlamaModelParams) -> ModelKey {
        ModelKey::new(Path::new(path), params)
    }

    #[test]
    fn same_file_same_params_shares_one_key() {
        // Scenario A: identical file + identical params → one resident copy.
        let a = key("/models/gemma.gguf", &LlamaModelParams::default());
        let b = key("/models/gemma.gguf", &LlamaModelParams::default());
        assert_eq!(a, b);
    }

    #[test]
    fn same_file_different_params_load_separately() {
        // Scenario B: same file, but polish `default()` vs assistant streaming
        // params → distinct keys, so the two roles load independent copies
        // rather than the assistant inheriting polish's (or vice versa).
        let polish = key("/models/gemma.gguf", &LlamaModelParams::default());
        let assistant = key("/models/gemma.gguf", &streaming_model_params());
        assert_ne!(
            polish, assistant,
            "streaming params must not collide with default() for the same file"
        );
    }

    #[test]
    fn streaming_params_are_mmap_on_mlock_off_cpu() {
        let p = streaming_model_params();
        assert!(p.use_mmap(), "streaming must mmap (file-backed, page on demand)");
        assert!(!p.use_mlock(), "streaming must not pin — mlock on an over-RAM model OOMs");
        assert_eq!(p.n_gpu_layers(), 0, "streaming path is CPU-only for now");
    }
}
