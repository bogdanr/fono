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
/// once nothing references them. Keyed by canonicalized path; a caller loading
/// the same file with materially different [`LlamaModelParams`] would share
/// the first-loaded variant — acceptable today because both embedded backends
/// load with `LlamaModelParams::default()`.
///
/// # Errors
/// Propagates `llama.cpp`'s load failure (missing/corrupt GGUF, OOM, …).
pub fn shared_model(path: &Path, params: &LlamaModelParams) -> Result<Arc<LlamaModel>> {
    static MODELS: OnceLock<Mutex<HashMap<PathBuf, Weak<LlamaModel>>>> = OnceLock::new();
    let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
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
