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

use anyhow::{anyhow, Context, Result};
use llama_cpp_2::context::params::{KvCacheType, LlamaContextParams};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::LlamaModel;
use tracing::{debug, info, warn};

static LLAMA_LOG_INIT: Once = Once::new();

/// Version of the llama.cpp bindings whose serialization format a saved
/// checkpoint is written in.
///
/// A saved state is llama.cpp's own byte format, and that format is not
/// promised to be stable across versions, so a checkpoint written by one and
/// read by another can restore into nonsense. It therefore belongs in the key
/// that decides whether a stored checkpoint may be used.
///
/// Kept by hand rather than read from the crate, because a dependency's version
/// is not available to `env!`. The test below fails the build if the pin in
/// `Cargo.lock` moves without this following it.
pub const LLAMA_BINDING_VERSION: &str = "0.1.154";

/// Version of Fono's own reading of a saved state — how a checkpoint's tokens,
/// positions and payload relate to each other.
///
/// Separate from [`LLAMA_BINDING_VERSION`] because the two go stale for
/// different reasons: llama.cpp can change its bytes while Fono's handling is
/// unchanged, and Fono can change how it trims or restores a state while the
/// bytes are identical. Bump this when the latter happens.
pub const STATE_FORMAT_VERSION: u32 = 1;

/// Budget a prompt-state cache for the model and context that just loaded.
///
/// The cache is built before the model is known, so its opening budget is a
/// share of free RAM — a number that says what may be spent, not what is worth
/// spending. Once the model is loaded the useful unit is available: one saved
/// checkpoint is one copy of the KV cache, and a cache smaller than that
/// retains nothing at all.
///
/// Best-effort. A model whose metadata will not parse keeps the opening budget,
/// which is the behaviour that predates this and costs only efficiency.
pub fn budget_prompt_cache(
    cache: &Mutex<crate::prompt_cache::PromptStateCache>,
    model_path: &Path,
    n_ctx: u32,
    type_k: KvCacheType,
    type_v: KvCacheType,
    disk: Option<DiskTierRequest<'_>>,
) {
    let Some(checkpoint) = crate::gpu_offload::kv_cache_bytes(model_path, n_ctx, type_k, type_v)
    else {
        debug!("prompt cache: model metadata unreadable, keeping the opening budget");
        return;
    };
    let Ok(mut cache) = cache.lock() else { return };
    // Disk first: whether it is attached decides how much RAM the memory tier
    // needs. With somewhere to put what it drops, memory only has to hold the
    // conversation being served; without, it has to hold the predecessors a
    // longest-prefix match needs too.
    if let Some(request) = disk {
        attach_checkpoint_tier(&mut cache, checkpoint, request);
    }
    let budget = cache.resize_for_checkpoint(checkpoint, crate::hwcheck::available_ram_bytes());
    let mb = |b: u64| b / (1024 * 1024);
    if cache.holds_a_checkpoint(checkpoint) {
        debug!(
            "prompt cache: {} MB budget, {} MB a checkpoint at ctx={n_ctx}",
            mb(budget as u64),
            mb(checkpoint)
        );
    } else {
        // Not a tuning note. Below one checkpoint every entry is admitted and
        // dropped by the same pass, so the cache is inert and every turn pays a
        // full cold prefill. The remedy is a shorter context or more free
        // memory; a larger cache is not on offer, because the ceiling is what
        // the machine has.
        warn!(
            "prompt cache: {} MB budget cannot hold one {} MB checkpoint at ctx={n_ctx}; \
             prompt reuse is off until the context is shorter or memory frees up",
            mb(budget as u64),
            mb(checkpoint)
        );
    }
}

/// Where a caller wants checkpoints kept, and any size it was told to use.
#[derive(Debug, Clone, Copy)]
pub struct DiskTierRequest<'a> {
    /// Directory the checkpoints live in. Created on demand.
    pub dir: &'a Path,
    /// Size from configuration, in whole GiB. `None` derives one; `Some(0)`
    /// refuses.
    pub configured_gb: Option<u32>,
    /// Hash of everything that decides whether a stored checkpoint can be
    /// restored into this context. Checkpoints stored under any other value are
    /// swept on attach: they can never match again.
    pub runtime_sha256: &'a str,
}

/// Give the in-memory cache somewhere to put the checkpoints it drops.
///
/// Every outcome is reported, including the refusals, because a user who
/// expects checkpoints to survive a restart has no other way to tell whether
/// they do.
fn attach_checkpoint_tier(
    cache: &mut crate::prompt_cache::PromptStateCache,
    checkpoint_bytes: u64,
    request: DiskTierRequest<'_>,
) {
    let mb = |b: u64| b / (1024 * 1024);
    let free = crate::hwcheck::free_disk_bytes(request.dir);
    let max_bytes =
        match size_checkpoint_tier(request.dir, checkpoint_bytes, request.configured_gb, free) {
            CheckpointTier::On { max_bytes } => max_bytes,
            CheckpointTier::OffByChoice => {
                debug!("prompt checkpoints: turned off in configuration");
                return;
            }
            CheckpointTier::OffNoRoom { checkpoint_bytes, free_bytes } => {
                warn!(
                    "prompt checkpoints: {} MB free where one checkpoint is {} MB; \
                     they will not survive a restart until there is more room",
                    mb(free_bytes),
                    mb(checkpoint_bytes)
                );
                return;
            }
        };
    // The store has the last word on whether it can run at all, so that the
    // reason a user is shown comes from the code that decided it.
    let Some(store) =
        crate::prompt_cache_disk::CheckpointStore::open(request.dir.to_path_buf(), max_bytes)
    else {
        let why = crate::prompt_cache_disk::CheckpointStore::refusal(request.dir, max_bytes)
            .unwrap_or_else(|| format!("{} could not be created", request.dir.display()));
        warn!("prompt checkpoints: {why}; they will not survive a restart");
        return;
    };
    let stale = cache.attach_disk(store, request.runtime_sha256);
    let (held_bytes, held) =
        cache.disk().map(super::prompt_cache_disk::CheckpointStore::usage).unwrap_or((0, 0));
    info!(
        "prompt checkpoints: keeping up to {} MB in {} ({held} there now, {} MB, {stale} dropped as \
         stored for a different model or setting)",
        mb(max_bytes),
        request.dir.display(),
        mb(held_bytes)
    );
}

/// Where checkpoints are kept, and how much room they get.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointTier {
    /// Keeping checkpoints, with this many bytes to spend.
    On { max_bytes: u64 },
    /// Turned off in configuration. No directory is created.
    OffByChoice,
    /// Not enough free disk for even one checkpoint.
    OffNoRoom { checkpoint_bytes: u64, free_bytes: u64 },
}

/// Fraction of free disk the tier may claim when no size is configured.
///
/// Deliberately modest: this is a cache on a disk the user has other plans for,
/// and the tier's value is already realised by holding a handful of checkpoints.
const DISK_SHARE_PERCENT: u64 = 20;

/// Checkpoints the automatic size aims to hold. Past a handful the returns fall
/// away — a session revisits recent conversations, not distant ones.
const DISK_CHECKPOINTS: u64 = 8;

/// Ceiling on the automatic size, whatever the arithmetic says.
///
/// Eight checkpoints is the right shape for a mixture model, whose checkpoints
/// are around 200 MB, and eight of those is 1.7 GiB. It is the wrong shape for a
/// dense 26B at a 20k context, whose checkpoints are 2.3 GB each: eight of those
/// is 18 GiB of cache, which no user asked for. A cache should not be the largest
/// thing Fono puts on the disk.
///
/// This is a cap on a derived number, not a budget in its own right — the
/// distinction that matters, because a fixed budget holds fifteen checkpoints of
/// one model and one of another. Under the cap a large dense model gets one or
/// two checkpoints, which is enough for the case that motivated the tier:
/// resuming the conversation in progress after a restart.
const DISK_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Decide how much disk the checkpoint tier gets.
///
/// Sized in checkpoints rather than gigabytes, for the reason the memory budget
/// already learned: the same constant means wildly different things per model,
/// and a tier that cannot hold one checkpoint holds none. A configured `0` is an
/// explicit refusal and is honoured before anything else is looked at.
#[must_use]
pub fn size_checkpoint_tier(
    dir: &Path,
    checkpoint_bytes: u64,
    configured_gb: Option<u32>,
    free_disk_bytes: Option<u64>,
) -> CheckpointTier {
    if configured_gb == Some(0) {
        return CheckpointTier::OffByChoice;
    }
    if crate::prompt_cache_disk::path_is_memory_backed(dir) {
        // Not decided here — `CheckpointStore::open` refuses a memory-backed
        // directory and says why. Sizing it as if it would work keeps one owner
        // of that reason.
        return CheckpointTier::On { max_bytes: checkpoint_bytes };
    }
    if let Some(gb) = configured_gb {
        return CheckpointTier::On { max_bytes: u64::from(gb) * 1024 * 1024 * 1024 };
    }
    let free = free_disk_bytes.unwrap_or(0);
    let share = free / 100 * DISK_SHARE_PERCENT;
    let budget = checkpoint_bytes.saturating_mul(DISK_CHECKPOINTS).min(share).min(DISK_MAX_BYTES);
    if budget < checkpoint_bytes {
        return CheckpointTier::OffNoRoom { checkpoint_bytes, free_bytes: free };
    }
    CheckpointTier::On { max_bytes: budget }
}

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
/// resident copy, while the same file loaded with different params (e.g. one
/// role on the device and another on the host) loads as separate entries rather
/// than silently reusing the first variant.
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
/// must load a separate copy (see the `shared_model` doc).
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
/// RAM is an OOM), with the model kept entirely on the host.
///
/// This is what a load falls back to when the device refuses it, and what
/// [`shared_model_sized`] uses whenever the model does not fit an accelerator.
#[must_use]
pub fn host_only_model_params() -> LlamaModelParams {
    LlamaModelParams::default().with_use_mmap(true).with_use_mlock(false).with_n_gpu_layers(0)
}

/// Load `path` with the layers on an accelerator when the whole model fits one,
/// and on the host when it does not — falling back to the host if the device
/// refuses the load anyway.
///
/// One entry point for both roles, because the question that decides the answer
/// is the same for both: do these weights, this cache and its working memory fit
/// the device? Only the inputs differ, which is why they are arguments. A shared
/// *constant* was the previous arrangement and it was wrong in both directions —
/// the assistant was pinned to the host even on a machine that could hold it,
/// while cleanup asked for everything and so failed the load outright on a small
/// card instead of quietly running on the CPU.
///
/// `n_ctx` and the two cache types are needed because the cache is not a
/// rounding error: at full context it can rival the weights, and the same model
/// costs a different amount at `f16` than at `q8_0`.
///
/// # Errors
/// Propagates the load failure when the host attempt fails too.
pub fn shared_model_sized(
    path: &Path,
    n_ctx: u32,
    type_k: KvCacheType,
    type_v: KvCacheType,
) -> Result<Arc<LlamaModel>> {
    let decision = crate::gpu_offload::decide(path, n_ctx, type_k, type_v);
    if decision.n_gpu_layers == 0 {
        debug!("offload: {}", decision.explanation);
        return shared_model(path, &host_only_model_params());
    }
    let params = host_only_model_params().with_n_gpu_layers(decision.n_gpu_layers);
    match shared_model(path, &params) {
        Ok(model) => {
            info!("offload: {}", decision.explanation);
            if let Some(device) = &decision.device {
                crate::gpu_offload::note_accelerator_in_use(device);
            }
            Ok(model)
        }
        // The estimate said it fits and the device disagreed. One retry on the
        // host, and no search for a layer count in between: a partial offload
        // generates slower than none at all, so there is nothing to find.
        Err(e) => {
            warn!(
                "offload: the device refused the model ({e:#}); loading it on the CPU instead. \
                 The reply will be slower but correct"
            );
            shared_model(path, &host_only_model_params())
        }
    }
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

/// Check that the context params are not asking for a quantized value cache
/// with flash attention switched off, and name the policy in force so callers
/// can record it.
///
/// Quantizing the V half of the KV cache *requires* flash attention. Without
/// it llama.cpp hands back a null context, which surfaces as an opaque
/// "create llama context" failure rather than as the configuration mistake it
/// is. The policy defaults to `AUTO` and llama.cpp resolves it on for the
/// shapes we run, so the pairing is correct today — this turns a future
/// upstream default flip, or a local edit, into a message that says what went
/// wrong instead of a null pointer.
///
/// `AUTO` is reported rather than resolved: llama.cpp decides at context
/// creation and does not publish the answer, so the honest thing to record is
/// what was asked for. A successful load with a quantized V cache is itself the
/// evidence that `AUTO` resolved to enabled.
///
/// # Errors
/// When `type_v` is a quantized type and flash attention is explicitly
/// disabled.
pub fn flash_attention_policy(params: &LlamaContextParams) -> Result<&'static str> {
    let policy = match params.flash_attention_policy() {
        llama_cpp_sys_2::LLAMA_FLASH_ATTN_TYPE_DISABLED => "disabled",
        llama_cpp_sys_2::LLAMA_FLASH_ATTN_TYPE_ENABLED => "enabled",
        _ => "auto",
    };
    if policy == "disabled" && is_quantized(params.type_v()) {
        return Err(anyhow!(
            "a quantized value cache ({:?}) needs flash attention, and it is switched off; \
             llama.cpp would refuse to create the context",
            params.type_v()
        ));
    }
    Ok(policy)
}

/// Whether a cache type stores fewer than a whole float per value, which is the
/// property flash attention is required for.
fn is_quantized(ty: KvCacheType) -> bool {
    !matches!(ty, KvCacheType::F32 | KvCacheType::F16 | KvCacheType::BF16 | KvCacheType::F64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A saved checkpoint is llama.cpp's own byte format, and the version that
    /// wrote it decides whether it can be read. The pin here has to follow the
    /// pin in `Cargo.lock`, and nothing but a test notices when it stops
    /// following it — so a bump that forgets this fails the build rather than
    /// letting stale checkpoints restore into nonsense.
    #[test]
    fn the_binding_version_matches_the_locked_dependency() {
        let lock =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"))
                .expect("read Cargo.lock");
        let locked = lock
            .split("name = \"llama-cpp-2\"")
            .nth(1)
            .and_then(|rest| rest.split("version = \"").nth(1))
            .and_then(|rest| rest.split('"').next())
            .expect("llama-cpp-2 version in Cargo.lock");
        assert_eq!(
            locked, LLAMA_BINDING_VERSION,
            "llama-cpp-2 moved to {locked}; update LLAMA_BINDING_VERSION so stored \
             checkpoints written by {LLAMA_BINDING_VERSION} are not read back by it"
        );
    }

    #[test]
    fn a_configured_zero_refuses_before_anything_else_is_looked_at() {
        // The opt-out has to hold even where the automatic size would have
        // said yes, and it must not create the directory to find out.
        let dir = Path::new("/nonexistent/fono-test-checkpoints");
        let tier = size_checkpoint_tier(dir, 200 * 1024 * 1024, Some(0), Some(u64::MAX));
        assert_eq!(tier, CheckpointTier::OffByChoice);
        assert!(!dir.exists());
    }

    #[test]
    fn a_configured_size_is_taken_as_given() {
        let tier = size_checkpoint_tier(Path::new("/var/tmp"), 200 * 1024 * 1024, Some(2), None);
        assert_eq!(tier, CheckpointTier::On { max_bytes: 2 * 1024 * 1024 * 1024 });
    }

    #[test]
    fn the_automatic_size_is_counted_in_checkpoints_and_capped_by_free_disk() {
        let checkpoint = 200 * 1024 * 1024;
        let dir = Path::new("/var/tmp");

        // Plenty of room: the number of checkpoints decides, not the disk.
        let roomy = size_checkpoint_tier(dir, checkpoint, None, Some(500 * 1024 * 1024 * 1024));
        assert_eq!(roomy, CheckpointTier::On { max_bytes: checkpoint * DISK_CHECKPOINTS });

        // A nearly full disk: the share decides, and it still holds several.
        // The share divides before multiplying so it cannot overflow, which
        // costs a few bytes of rounding.
        let free = 5 * 1024 * 1024 * 1024_u64;
        let tight = size_checkpoint_tier(dir, checkpoint, None, Some(free));
        assert_eq!(tight, CheckpointTier::On { max_bytes: free / 100 * DISK_SHARE_PERCENT });

        // Below one checkpoint the tier holds nothing at all, so say so rather
        // than write a file the next sweep deletes. This is the same cliff the
        // memory budget has, one storey down.
        let starved = size_checkpoint_tier(dir, checkpoint, None, Some(512 * 1024 * 1024));
        assert_eq!(
            starved,
            CheckpointTier::OffNoRoom {
                checkpoint_bytes: checkpoint,
                free_bytes: 512 * 1024 * 1024
            }
        );

        // An unreadable disk reads as no room, not as unlimited room.
        assert!(matches!(
            size_checkpoint_tier(dir, checkpoint, None, None),
            CheckpointTier::OffNoRoom { .. }
        ));
    }

    /// A dense 26B checkpoint is 2.3 GB, so eight of them is 18 GiB. The cap
    /// keeps the automatic size to something a user would not be startled to
    /// find on their disk, and still leaves room for the checkpoint in front of
    /// it — which is the case the tier exists for.
    #[test]
    fn a_huge_checkpoint_does_not_produce_a_huge_cache() {
        let checkpoint = 2337 * 1024 * 1024_u64;
        let tier = size_checkpoint_tier(
            Path::new("/var/tmp"),
            checkpoint,
            None,
            Some(500 * 1024 * 1024 * 1024),
        );
        assert_eq!(tier, CheckpointTier::On { max_bytes: DISK_MAX_BYTES });
        let CheckpointTier::On { max_bytes } = tier else { unreachable!() };
        assert!(max_bytes >= checkpoint, "the cap must still admit one checkpoint");
    }

    // Regression: the shared-model cache key must fold in the load-time
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
        // Scenario B: the same file, one role on the host and one on the
        // device → distinct keys, so each gets the residency it asked for
        // instead of inheriting whichever role loaded first.
        let host = key("/models/gemma.gguf", &host_only_model_params());
        let device = key("/models/gemma.gguf", &host_only_model_params().with_n_gpu_layers(31));
        assert_ne!(host, device, "an offloaded load must not reuse the host copy");
    }

    #[test]
    fn host_params_are_mmap_on_mlock_off_cpu() {
        let p = host_only_model_params();
        assert!(p.use_mmap(), "weights must stay file-backed and page in on demand");
        assert!(!p.use_mlock(), "pinning a model larger than RAM is an OOM");
        assert_eq!(p.n_gpu_layers(), 0, "the host fallback keeps every layer on the CPU");
    }

    #[test]
    fn the_shipped_cache_types_pass_the_flash_attention_check() {
        // What every inference path actually asks for: a q8_0 cache on the
        // default AUTO policy. It must not trip the guard.
        let params = LlamaContextParams::default()
            .with_type_k(KvCacheType::Q8_0)
            .with_type_v(KvCacheType::Q8_0);
        assert_eq!(flash_attention_policy(&params).unwrap(), "auto");
    }

    #[test]
    fn a_quantized_value_cache_with_flash_attention_off_is_refused() {
        // Forcing the policy off is the failure this guard exists to name.
        // Without it llama.cpp returns a null context and the caller reports
        // an opaque "create llama context" error.
        let params = LlamaContextParams::default()
            .with_type_v(KvCacheType::Q8_0)
            .with_flash_attention_policy(llama_cpp_sys_2::LLAMA_FLASH_ATTN_TYPE_DISABLED);
        assert!(flash_attention_policy(&params).is_err());
    }

    #[test]
    fn an_unquantized_value_cache_may_switch_flash_attention_off() {
        // f16 has no such requirement, so turning the policy off is a legal
        // configuration and the guard must not invent a failure.
        let params = LlamaContextParams::default()
            .with_type_v(KvCacheType::F16)
            .with_flash_attention_policy(llama_cpp_sys_2::LLAMA_FLASH_ATTN_TYPE_DISABLED);
        assert_eq!(flash_attention_policy(&params).unwrap(), "disabled");
    }
}
