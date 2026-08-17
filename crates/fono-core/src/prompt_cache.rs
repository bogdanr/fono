// SPDX-License-Identifier: GPL-3.0-only
//! Shared prompt-state (KV) cache data structure for the embedded llama.cpp
//! backends (assistant F8 and polish F7).
//!
//! This module is deliberately *llama-agnostic*: it stores opaque serialized
//! KV-state blobs (`Vec<u8>`) keyed by a content fingerprint and manages
//! bounded retention (LRU + byte budget) plus pinning of context-independent
//! base prefixes. The actual building and restoring of llama.cpp state lives in
//! each backend, so this crate carries no `llama-cpp-2` dependency and is cheap
//! for every workspace consumer to compile.
//!
//! ## Why snapshots, not one growing context
//!
//! Each entry is a *complete, standalone copy* of the model's KV state, not a
//! link in a shared chain. That is what makes arbitrary LRU eviction safe:
//! dropping one entry can never invalidate another, because no entry references
//! any other. The cost is redundancy (a system-prompt prefix is duplicated
//! inside every conversation snapshot that extends it), which the byte budget
//! caps. The alternative — a single append-only context per conversation —
//! would remove the duplication but only permit tail truncation, never
//! middle eviction.
//!
//! ## Pinning
//!
//! Context-independent base prefixes (the F7 cleanup base, the F8 system
//! prompt, the tool prompt) are reused on every turn of every conversation and
//! are prewarmed at startup. Losing one to LRU churn regresses the next use to
//! a cold prefill (up to a multi-second cliff for large prompts), so they are
//! pinned and skipped by eviction. Only the most recent snapshot of a given
//! pinnable layer stays pinned; when the active prompt changes the stale pin is
//! released so it can age out.
//!
//! Both budgets measure only what eviction can actually reclaim — see
//! [`PromptStateCache::evictable_totals`].

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::debug;

/// Size and modification time of every file that makes up a model, for the
/// runtime half of a cache key.
///
/// A GGUF over a few tens of gigabytes is published as numbered shards
/// (`…-00001-of-00004.gguf`) and llama.cpp is handed only the first one; it
/// resolves its siblings itself. Fingerprinting just the named file therefore
/// misses every byte of weight data in the remaining shards — for a 104 GB
/// four-shard model that is 99 GB of weights invisible to the key. Swapping
/// those shards for a different quantization would leave saved states looking
/// current and restore them into a model that never produced them. Shards are
/// fingerprinted together so any of them changing invalidates the saved states.
pub fn model_files_fingerprint(path: &Path) -> std::io::Result<String> {
    let mut parts = Vec::new();
    for file in model_shard_paths(path) {
        let metadata = std::fs::metadata(&file)?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or_else(
                || "unknown".to_string(),
                |d| format!("{}.{:09}", d.as_secs(), d.subsec_nanos()),
            );
        parts.push(format!(
            "{}:{}:{}",
            file.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            metadata.len(),
            modified
        ));
    }
    Ok(parts.join(","))
}

/// Total bytes of every shard that makes up the model at `path`, which is what
/// the weights occupy once resident. `None` when a shard cannot be read.
pub fn model_files_size(path: &Path) -> Option<u64> {
    model_shard_paths(path)
        .iter()
        .try_fold(0u64, |sum, p| Some(sum.saturating_add(std::fs::metadata(p).ok()?.len())))
}

/// A stable identity for the model at `path`, in the `sha256:<hex>` shape
/// Ollama clients expect of a served model.
///
/// Hashed over the same names, sizes and modification times as
/// [`model_files_fingerprint`], not over the weights: it has to answer a
/// metadata request, and digesting the bytes of a hundred-gigabyte model to do
/// so would take minutes. It therefore identifies *which* model is served and
/// notices it being replaced, which is what a client wants it for — it is not a
/// checksum of the file's contents and must not be used as one.
pub fn model_digest(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let fingerprint = model_files_fingerprint(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(fingerprint.as_bytes());
    Some(format!("sha256:{:x}", hasher.finalize()))
}

/// Every shard of a sharded GGUF, or just `path` when it is a single file.
///
/// Derives the sibling names from the `-<index>-of-<total>` suffix rather than
/// reading the directory, so unrelated GGUFs sitting alongside cannot creep in.
/// Missing siblings stay in the list: their absence must fail loudly in
/// [`model_files_fingerprint`] rather than silently shrink the fingerprint.
pub fn model_shard_paths(path: &Path) -> Vec<PathBuf> {
    let single = || vec![path.to_path_buf()];
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return single() };
    let Some(stem) = name.strip_suffix(".gguf") else { return single() };
    // Trailing `-<index>-of-<total>`, zero-padded to the width of `<total>`.
    let Some((rest, total_field)) = stem.rsplit_once('-') else { return single() };
    let Some((rest, of)) = rest.rsplit_once('-') else { return single() };
    let Some((prefix, index_field)) = rest.rsplit_once('-') else { return single() };
    let Ok(total) = total_field.parse::<u32>() else { return single() };
    if of != "of" || total == 0 || index_field.len() != total_field.len() {
        return single();
    }
    if index_field.parse::<u32>().is_err() {
        return single();
    }
    let width = total_field.len();
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    (1..=total)
        .map(|i| parent.join(format!("{prefix}-{i:0width$}-of-{total:0width$}.gguf")))
        .collect()
}

/// Logical role of a cached prefix. The layer is part of the cache key, so two
/// prefixes with identical text but different roles never collide, and it
/// drives pinning (`is_pinnable`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PromptStateCacheLayer {
    /// F7 transcription cleanup base prompt (config: main + advanced +
    /// dictionary). Context-independent, pinned.
    F7System,
    /// F8 assistant system prompt. Context-independent, pinned.
    ///
    /// The tool descriptions are part of this prompt rather than a layer of
    /// their own: the embedded backend renders its own chat markers, so it
    /// describes tools inside the system prompt and warms the head it will
    /// really send. A separately warmed tool prompt could never match, because
    /// prefix matching starts at token 0 and the descriptions sit mid-prompt.
    F8System,
    /// F7 cleanup base + the focused app's `rule_suffix` (CLI / editor /
    /// browser / terminal-agent). Per-context layer, LRU among contexts.
    F7Context,
    /// Deprecated assistant window-context layer (assistant no longer injects
    /// window context). Retained for key stability / migration.
    WindowContext,
    /// F8 chat prefix (system + tools + history), used by the live reply path.
    F8ChatPrefix,
    /// The prefix a turn actually read before generating: system + tools +
    /// history, with nothing of this turn's own reply in it.
    ///
    /// Distinct from [`Self::F8ChatPrefix`] purely so it survives that layer's
    /// pruning. A turn stores two checkpoints — this one at the start, and a
    /// completed-turn one at the end that also covers the reply. Under one
    /// layer the second prunes the first within the same turn, and the second
    /// is the one the *next* turn cannot use when a tool was called, because a
    /// completed turn carries the tool call and its result while history keeps
    /// only the spoken reply. Keeping them apart means the next turn still has
    /// something to stand on.
    HistoryPrefix,
    /// Synthetic benchmark prefix.
    BenchmarkPrefix,
    /// Exact full-prompt snapshot.
    ExactPrompt,
}

impl PromptStateCacheLayer {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::F7System => "f7_system",
            Self::F8System => "f8_system",
            Self::F7Context => "f7_context",
            Self::WindowContext => "window_context",
            Self::F8ChatPrefix => "f8_chat_prefix",
            Self::HistoryPrefix => "history_prefix",
            Self::BenchmarkPrefix => "benchmark_prefix",
            Self::ExactPrompt => "exact_prompt",
        }
    }

    /// Inverse of [`Self::as_str`], for reading a key back off disk.
    #[must_use]
    pub fn parse_name(name: &str) -> Option<Self> {
        Some(match name {
            "f7_system" => Self::F7System,
            "f8_system" => Self::F8System,
            "f7_context" => Self::F7Context,
            "window_context" => Self::WindowContext,
            "f8_chat_prefix" => Self::F8ChatPrefix,
            "history_prefix" => Self::HistoryPrefix,
            "benchmark_prefix" => Self::BenchmarkPrefix,
            "exact_prompt" => Self::ExactPrompt,
            _ => return None,
        })
    }

    /// Context-independent base prefixes that are reused on every turn of every
    /// conversation. These are prewarmed at startup and must never be evicted
    /// by LRU churn. All other layers age out normally.
    pub fn is_pinnable(&self) -> bool {
        matches!(self, Self::F7System | Self::F8System)
    }
}

/// Content fingerprint of a cached prefix: layer + runtime hash + prompt-text
/// hash + token hash + token count. Strict enough to prevent cross-model and
/// cross-prompt reuse.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PromptStateCacheKey {
    layer: PromptStateCacheLayer,
    runtime_sha256: String,
    prompt_sha256: String,
    token_sha256: String,
    token_count: usize,
}

impl PromptStateCacheKey {
    pub fn new(
        layer: PromptStateCacheLayer,
        runtime_sha256: impl Into<String>,
        prompt_sha256: impl Into<String>,
        token_sha256: impl Into<String>,
        token_count: usize,
    ) -> Self {
        Self {
            layer,
            runtime_sha256: runtime_sha256.into(),
            prompt_sha256: prompt_sha256.into(),
            token_sha256: token_sha256.into(),
            token_count,
        }
    }

    pub fn layer(&self) -> &PromptStateCacheLayer {
        &self.layer
    }

    pub fn runtime_sha256(&self) -> &str {
        &self.runtime_sha256
    }

    pub fn token_count(&self) -> usize {
        self.token_count
    }

    pub fn stable_id(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.layer.as_str(),
            self.runtime_sha256,
            self.prompt_sha256,
            self.token_sha256,
            self.token_count
        )
    }

    /// Rebuild a key from [`Self::stable_id`].
    ///
    /// The disk tier stores this line beside each checkpoint so a file can be
    /// admitted straight to the in-memory cache without the caller re-deriving
    /// anything. `|` is the separator because the three hashes are hex and no
    /// layer name contains one.
    ///
    /// `None` for anything that does not parse, which the disk tier treats the
    /// same as any other corruption: delete the file.
    #[must_use]
    pub fn parse_stable_id(encoded: &str) -> Option<Self> {
        let mut fields = encoded.split('|');
        let layer = PromptStateCacheLayer::parse_name(fields.next()?)?;
        let runtime_sha256 = fields.next()?.to_string();
        let prompt_sha256 = fields.next()?.to_string();
        let token_sha256 = fields.next()?.to_string();
        let token_count = fields.next()?.parse().ok()?;
        if fields.next().is_some() {
            return None;
        }
        Some(Self { layer, runtime_sha256, prompt_sha256, token_sha256, token_count })
    }
}

/// A serialized llama.cpp KV-state blob plus the token count it represents.
///
/// `prefix_tokens` optionally records the exact token sequence the snapshot was
/// built from. Entries that carry it can participate in longest-prefix matching
/// (restore the deepest cached prefix that is a token-prefix of a new prompt and
/// decode only the remainder); entries built with [`Self::new`] leave it empty
/// and are reachable by exact key only.
#[derive(Debug, Clone)]
pub struct PromptStateCacheEntry {
    pub state: Vec<u8>,
    pub token_count: usize,
    pub prefix_tokens: Vec<i32>,
    /// When this entry was last created or read. The LRU deque records the
    /// *order* entries were touched in; this records *when*, which is what a
    /// diagnostic needs to tell a stale branch from one merely second in a
    /// burst. Private so only the cache can move it.
    last_used: Instant,
    /// Set when the caller knows this snapshot cannot recur once the turn that
    /// made it is over — an intra-turn checkpoint whose prefix contains this
    /// turn's tool call and result, which the stored history does not keep.
    ///
    /// Such an entry is scratch: useful to the wording pass moments later,
    /// useless to every turn after. Marking it lets a longer checkpoint
    /// supersede it across layer boundaries (see [`Self::dies_with_turn`] and
    /// `prune_dominated_by`), instead of leaving ~28 MiB of dead weight to age
    /// out through LRU while live branches compete for the same budget.
    ///
    /// Off by default, which is the safe answer: an OpenAI-protocol client
    /// resends tool calls and results verbatim, so for a network caller the
    /// same prefix may well recur and is worth keeping.
    dies_with_turn: bool,
    /// An abridged copy of the prompt this snapshot was taken over, kept so a
    /// diagnostic can say what an entry holds. Without it the only handle on an
    /// entry is the hash of that prompt, which tells a reader nothing.
    ///
    /// The end is kept in full and the beginning trimmed, because every entry
    /// on a branch shares the same opening: what distinguishes a child from its
    /// parent is only ever at the tail. A few hundred bytes beside a
    /// multi-megabyte blob.
    preview: Option<String>,
}

impl PromptStateCacheEntry {
    pub fn new(state: Vec<u8>, token_count: usize) -> Self {
        Self {
            state,
            token_count,
            prefix_tokens: Vec::new(),
            last_used: Instant::now(),
            dies_with_turn: false,
            preview: None,
        }
    }

    /// Build an entry that records its token sequence so it can be found by
    /// longest-prefix matching. `token_count` is derived from `prefix_tokens`.
    pub fn with_tokens(state: Vec<u8>, prefix_tokens: Vec<i32>) -> Self {
        Self {
            state,
            token_count: prefix_tokens.len(),
            prefix_tokens,
            last_used: Instant::now(),
            dies_with_turn: false,
            preview: None,
        }
    }

    /// Record what prompt this snapshot covers, abridged for display.
    #[must_use]
    pub fn describing(mut self, prompt: &str) -> Self {
        self.preview = Some(abridge_prompt(prompt));
        self
    }

    /// The abridged prompt, when the caller supplied one.
    pub fn preview(&self) -> Option<&str> {
        self.preview.as_deref()
    }

    /// Declare this snapshot intra-turn scratch, so a longer checkpoint may
    /// supersede it whatever layer that checkpoint is filed under. Only the
    /// caller can know this; see the field docs.
    #[must_use]
    pub fn dying_with_turn(mut self) -> Self {
        self.dies_with_turn = true;
        self
    }

    /// Whether this snapshot is intra-turn scratch.
    pub fn dies_with_turn(&self) -> bool {
        self.dies_with_turn
    }

    /// How long since this entry was created or last read.
    pub fn idle(&self) -> Duration {
        self.last_used.elapsed()
    }
}

/// Read-only view of one cached entry, borrowed from the cache for diagnostics.
///
/// Deliberately carries no `state`: [`PromptStateCache::get`] clones the whole
/// multi-megabyte blob, which makes it unusable for a panel that refreshes.
/// Everything here is either a copy of a small field or a borrow.
#[derive(Debug, Clone)]
pub struct CacheNodeView<'a> {
    pub id: String,
    pub layer: &'a PromptStateCacheLayer,
    pub runtime_sha256: &'a str,
    pub token_count: usize,
    pub bytes: usize,
    pub pinned: bool,
    /// Position in the eviction queue, 0 = least recently used. Pinned entries
    /// carry a rank too, but eviction skips them.
    pub lru_rank: usize,
    pub idle: Duration,
    pub prefix_tokens: &'a [i32],
    /// The abridged prompt this snapshot covers, when the caller recorded one.
    pub preview: Option<&'a str>,
}

/// Trim a prompt to something a tooltip can hold.
///
/// The tail is kept and the head trimmed: entries on one branch all begin with
/// the same system prompt, so the only part that identifies an entry is what it
/// added at the end. Enough of the opening survives for a root — which has no
/// parent and is all opening — to stay recognisable.
fn abridge_prompt(prompt: &str) -> String {
    const HEAD: usize = 180;
    const TAIL: usize = 620;
    let trimmed = prompt.trim();
    if trimmed.chars().count() <= HEAD + TAIL {
        return trimmed.to_string();
    }
    let chars: Vec<char> = trimmed.chars().collect();
    let head: String = chars[..HEAD].iter().collect();
    let tail: String = chars[chars.len() - TAIL..].iter().collect();
    format!("{head}\n\n […]\n\n{tail}")
}

/// One entry dropped by LRU / byte-budget enforcement, surfaced to the caller
/// so the llama backend can emit a diagnostic (e.g. a `cache.evicted` trace
/// instant) without this llama-agnostic crate depending on the tracing layer.
#[derive(Debug, Clone)]
pub struct EvictedEntry {
    pub layer: PromptStateCacheLayer,
    pub token_count: usize,
    pub bytes: usize,
}

/// Facts about what an `insert`/`insert_pinned` mutation changed. Returned so
/// the backend caller can record eviction/pinning churn on the turn trace; the
/// cache itself stays free of any tracing dependency. Callers that do not need
/// the diagnostics may ignore the value.
#[derive(Debug, Clone, Default)]
pub struct CacheMutationReport {
    /// Entries dropped by eviction during this mutation, oldest first.
    pub evicted: Vec<EvictedEntry>,
    /// Entries dropped because the freshly inserted entry dominates them — its
    /// recorded `prefix_tokens` is a strict superset, so they could never win a
    /// longest-prefix match again (e.g. turn N's completed-turn checkpoint
    /// supersedes turn N-1's). Pruned eagerly so the cache stays at the
    /// frontier instead of growing one dead entry per turn.
    pub pruned: Vec<EvictedEntry>,
    /// Layer newly pinned by this mutation (`insert_pinned` only).
    pub pinned: Option<PromptStateCacheLayer>,
    /// Stale pin of the same pinnable layer released because this insert
    /// replaced it (`insert_pinned` only).
    pub pin_released: Option<PromptStateCacheLayer>,
}

/// Running totals for one cache instance, since the daemon started.
///
/// Per-instance rather than process-global so the assistant and polish caches
/// never blend into one meaningless average, and held beside the cache rather
/// than inside it so recording a fact never has to wait on the cache lock —
/// several of these are reported from paths that have just released it, and one
/// is reported from a path that never takes it at all.
///
/// The per-turn trace already records all of this in far more detail, but only
/// when tracing is switched on and only one turn at a time, so it can never
/// answer "is the cache working today".
#[derive(Debug, Default)]
pub struct CacheCounters {
    restores: AtomicU64,
    cold_prefills: AtomicU64,
    evictions: AtomicU64,
    prunes: AtomicU64,
    pin_releases: AtomicU64,
    /// Cold prefills bucketed by reason. A leaf lock: nothing else is ever
    /// acquired while it is held, so it cannot take part in a deadlock.
    reasons: Mutex<BTreeMap<String, u64>>,
    /// Prefix tokens read again, bucketed by what stopped a deeper checkpoint
    /// from covering them. Tokens rather than lookups because one miss on a
    /// 16k prompt costs more than a hundred on short ones, and it is the cost
    /// that decides whether checkpoints are worth storing.
    rereads: Mutex<BTreeMap<String, u64>>,
    /// Prefix tokens a checkpoint read back from disk covered, so they were not
    /// read again. The counterpart to `rereads`: together they say what the
    /// disk tier saved and what nothing could have saved.
    disk_covered_tokens: AtomicU64,
}

impl CacheCounters {
    /// A turn restored a cached checkpoint instead of reading its prompt again.
    pub fn record_restore(&self) {
        self.restores.fetch_add(1, Ordering::Relaxed);
    }

    /// A turn read its whole prompt from scratch, and why.
    pub fn record_cold_prefill(&self, reason: &str) {
        self.cold_prefills.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut reasons) = self.reasons.lock() {
            *reasons.entry(reason.to_string()).or_insert(0) += 1;
        }
    }

    /// A lookup re-read `tokens` prefix tokens, and what stopped a deeper
    /// checkpoint from covering them. Recorded on hits too: a shallow hit and
    /// a total miss cost the same thing.
    pub fn record_prefix_reread(&self, cause: &str, tokens: u64) {
        if tokens == 0 {
            return;
        }
        if let Ok(mut rereads) = self.rereads.lock() {
            *rereads.entry(cause.to_string()).or_insert(0) += tokens;
        }
    }

    /// A checkpoint read back from disk covered `tokens` prefix tokens.
    pub fn record_disk_coverage(&self, tokens: u64) {
        self.disk_covered_tokens.fetch_add(tokens, Ordering::Relaxed);
    }

    /// Fold in the churn one insert caused.
    pub fn apply(&self, report: &CacheMutationReport) {
        self.evictions.fetch_add(report.evicted.len() as u64, Ordering::Relaxed);
        self.prunes.fetch_add(report.pruned.len() as u64, Ordering::Relaxed);
        if report.pin_released.is_some() {
            self.pin_releases.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn read(&self) -> CacheCountersSnapshot {
        CacheCountersSnapshot {
            restores: self.restores.load(Ordering::Relaxed),
            cold_prefills: self.cold_prefills.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            prunes: self.prunes.load(Ordering::Relaxed),
            pin_releases: self.pin_releases.load(Ordering::Relaxed),
            cold_prefill_reasons: self.reasons.lock().map(|r| r.clone()).unwrap_or_default(),
            reread_prefix_tokens: self.rereads.lock().map(|r| r.clone()).unwrap_or_default(),
            disk_covered_tokens: self.disk_covered_tokens.load(Ordering::Relaxed),
        }
    }
}

/// Point-in-time copy of [`CacheCounters`], for reporting.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CacheCountersSnapshot {
    pub restores: u64,
    pub cold_prefills: u64,
    /// Ordered so the serialized form is stable.
    pub cold_prefill_reasons: BTreeMap<String, u64>,
    pub evictions: u64,
    pub prunes: u64,
    pub pin_releases: u64,
    /// Prefix tokens read again, by cause — the taxonomy's headline. Ordered so
    /// the serialized form is stable.
    pub reread_prefix_tokens: BTreeMap<String, u64>,
    /// Prefix tokens a checkpoint read back from disk covered.
    pub disk_covered_tokens: u64,
}

impl CacheCountersSnapshot {
    /// True until some turn has either restored a checkpoint or paid a cold
    /// prefill, i.e. while the cache holds only startup prewarm and nothing has
    /// consulted it yet. Shape verdicts are meaningless before that.
    pub fn warming(&self) -> bool {
        self.restores == 0 && self.cold_prefills == 0
    }
}

/// Bounded in-memory cache of prompt-state checkpoints with LRU eviction, a
/// byte budget, and pinning of base prefixes.
#[derive(Debug)]
pub struct PromptStateCache {
    max_entries: usize,
    max_bytes: usize,
    bytes: usize,
    entries: HashMap<PromptStateCacheKey, PromptStateCacheEntry>,
    lru: VecDeque<PromptStateCacheKey>,
    pinned: HashSet<PromptStateCacheKey>,
    /// Token sequences of entries eviction dropped, kept so a later lookup can
    /// tell "this prompt was rewritten" from "we had this and threw it away".
    ///
    /// Only the second of those is worth storing checkpoints on disk for, and
    /// the two are indistinguishable from a live cache alone: both look like a
    /// shallow match. Holding the tokens of what was dropped — not the blob,
    /// which is what costs — makes the difference measurable.
    ///
    /// Pruned entries are deliberately not recorded. A prune drops an entry a
    /// longer live one already covers, so it can never be the deeper thing a
    /// lookup lost.
    tombstones: VecDeque<Tombstone>,
    /// What one checkpoint costs for the loaded model, once known. The budget
    /// is a multiple of it, so reporting the budget without it says nothing
    /// about how many conversations stay warm.
    checkpoint_bytes: Option<u64>,
    /// Checkpoints kept on disk behind this cache, when the tier is on.
    ///
    /// Holding a checkpoint here rather than there saves the 170–218 ms a read
    /// costs, against the 70–114 seconds of re-reading the checkpoint itself
    /// saves — a fifth of one percent. So this cache is a working set, not the
    /// store: memory buys the turn in progress, and disk keeps everything else.
    disk: Option<crate::prompt_cache_disk::CheckpointStore>,
}

/// What eviction dropped, minus the blob: enough to re-run prefix matching
/// against, and nothing else.
#[derive(Debug, Clone)]
struct Tombstone {
    runtime_sha256: String,
    layer: PromptStateCacheLayer,
    prefix_tokens: Vec<i32>,
    bytes: usize,
}

/// Total tokens the tombstone ring may hold before the oldest are dropped.
///
/// Four bytes a token, so this is a 1 MiB ceiling on a diagnostic that sits
/// beside a cache budgeted in hundreds of megabytes. Sized in tokens rather
/// than entries because entries differ by three orders of magnitude — a
/// 72-token system prompt and a 16k conversation are both one entry.
const TOMBSTONE_TOKEN_BUDGET: usize = 256 * 1024;

/// Why a lookup did not restore a deeper checkpoint than it did.
///
/// The whole point of the disk tier is to convert one of these into a hit, and
/// only one of them: a checkpoint the cache had and dropped is recoverable from
/// disk, while a prompt whose front was rewritten has nothing to recover. So
/// this is the measurement that decides whether the tier is worth building.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefixMissCause {
    /// The deepest checkpoint that could match did match. Nothing was lost.
    Deepest,
    /// Nothing is cached under this runtime key at all — the model, its
    /// settings or the process changed since anything was stored.
    RuntimeKeyChange,
    /// A checkpoint that would have matched was evicted to stay inside the
    /// budget. This is the case a disk tier turns into a hit.
    Eviction {
        /// Tokens that evicted checkpoint covered, i.e. what a disk tier would
        /// have saved this lookup from reading again.
        recoverable_tokens: usize,
        /// Bytes the tier would have had to read back.
        bytes: usize,
        layer: PromptStateCacheLayer,
    },
    /// Checkpoints exist for this runtime and none is a token prefix of this
    /// prompt: the prompt was rewritten in front of where they end. No amount
    /// of storage recovers this.
    Divergence {
        /// Token position where the closest candidate stopped agreeing with
        /// this prompt.
        at: usize,
        /// Tokens that candidate held, so the gap between it and `at` says how
        /// much was thrown away by the rewrite.
        candidate_tokens: usize,
    },
}

impl PrefixMissCause {
    /// Short stable name, for a trace field or a counter bucket.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Deepest => "deepest",
            Self::RuntimeKeyChange => "runtime_key_change",
            Self::Eviction { .. } => "eviction",
            Self::Divergence { .. } => "divergence",
        }
    }
}

/// The outcome of a longest-prefix lookup, with the reason it was not deeper.
#[derive(Debug, Clone)]
pub struct PrefixLookup {
    /// The live entry to restore, when one matched.
    pub hit: Option<PromptStateCacheKey>,
    /// Tokens that entry covers — the prefix this lookup does not have to read
    /// again.
    pub matched_tokens: usize,
    /// Tokens this lookup must read, having matched what it did.
    pub decoded_prefix_tokens: usize,
    pub cause: PrefixMissCause,
}

impl Default for PromptStateCache {
    fn default() -> Self {
        Self::new(10, DEFAULT_MAX_BYTES)
    }
}

/// Floor for the byte budget, and the whole budget on a machine with no memory
/// to spare. Small models keep several conversations inside it.
const DEFAULT_MAX_BYTES: usize = 256 * 1024 * 1024;

/// Ceiling for the byte budget. A checkpoint is pure cache: past this the
/// return is not worth the resident set, however much RAM the machine has.
const MAX_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Share of free RAM the cache may claim, after the desktop's reserve.
const HOST_RAM_SHARE: u64 = 4;

/// Reserve left to the desktop before any of free RAM is counted as the
/// cache's to claim. Matches the offload planner's figure.
const DESKTOP_RESERVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Checkpoints the budget aims to hold when nothing is kept on disk: the
/// conversation in progress, the predecessor a longest-prefix match needs, and
/// one spare.
const CHECKPOINTS_HELD: u64 = 3;

/// Checkpoints the budget aims to hold when dropped ones go to disk. The
/// conversation being served has its KV resident in the live context already —
/// that is not a cache copy — so the memory tier's whole value is *other*
/// conversations, and a read costs ~200 ms against the minute or more it saves.
/// One is enough, and the rest of the reservation goes back to the machine
/// (ADR 0042).
const CHECKPOINTS_HELD_WITH_DISK: u64 = 1;

/// On a clean exit, write whatever memory still holds to disk, so the
/// conversation in progress does not cost a full cold prefill after a restart.
///
/// A restart throwing everything away was the single strongest finding behind
/// the disk tier, and this is the trigger that answers it. Nothing else can:
/// eviction writes only what memory drops, and the entry being served is never
/// dropped. Runs once — the cache lives behind one `Arc`, so thin clones of the
/// backend that share it do not each write.
impl Drop for PromptStateCache {
    fn drop(&mut self) {
        if self.disk.is_none() {
            return;
        }
        let written = self.persist_all();
        if written > 0 {
            debug!("prompt checkpoints: kept {written} for the next start");
        }
    }
}

/// The most this cache may claim on a machine with `host_available` bytes
/// free — a share of what is left after the desktop's reserve, capped.
fn host_ceiling(host_available: u64) -> u64 {
    (host_available.saturating_sub(DESKTOP_RESERVE_BYTES) / HOST_RAM_SHARE).min(MAX_MAX_BYTES)
}

impl PromptStateCache {
    /// A cache budgeted from the memory this machine actually has free.
    ///
    /// The fixed 256 MiB this replaced was set when the largest thing anyone
    /// cached was a system prompt. One checkpoint of a 26B model at a 16k
    /// context is 1.8 GB — seven times that budget — so on a large model the
    /// cache retained nothing at all: every entry was admitted and dropped by
    /// the same enforcement pass, and every turn paid a full cold prefill.
    /// Deriving the number from free RAM keeps small models where they were
    /// (the floor is the old constant) and lets a large one hold the
    /// conversation it is having.
    #[must_use]
    pub fn sized_for_host() -> Self {
        Self::sized_from(crate::hwcheck::available_ram_bytes())
    }

    /// [`Self::sized_for_host`] with the memory figure passed in, so the
    /// arithmetic is testable away from the machine running the test.
    #[must_use]
    pub fn sized_from(host_available: u64) -> Self {
        let capped = host_ceiling(host_available);
        let bytes = usize::try_from(capped).unwrap_or(DEFAULT_MAX_BYTES).max(DEFAULT_MAX_BYTES);
        Self::new(10, bytes)
    }

    /// Re-budget for the model that actually loaded, in units of the thing
    /// being stored.
    ///
    /// One checkpoint is one copy of the KV cache, so `checkpoint_bytes` is the
    /// only figure that says whether this cache can hold anything at all —
    /// share-of-RAM does not, and a fixed byte count cannot, because per-token
    /// cost varies more than tenfold across the models Fono ships. Below one
    /// checkpoint the cache retains **nothing**: the entry is admitted and
    /// dropped by the same enforcement pass. That is a cliff, not a gradient,
    /// which is why the target is a small multiple rather than a fraction.
    ///
    /// Three is the multiple: the conversation in progress, the predecessor a
    /// longest-prefix match needs, and one spare so a second conversation does
    /// not evict the first on sight. Bounded above by free RAM, which says what
    /// may be spent and never what should be.
    ///
    /// Returns the budget set. When free RAM cannot cover even one checkpoint
    /// the caller is told, because the cache then cannot do its job and the
    /// remedy is a shorter context, not a larger cache.
    pub fn resize_for_checkpoint(&mut self, checkpoint_bytes: u64, host_available: u64) -> usize {
        let ceiling = host_ceiling(host_available);
        let held = if self.disk.is_some() { CHECKPOINTS_HELD_WITH_DISK } else { CHECKPOINTS_HELD };
        let want = checkpoint_bytes.saturating_mul(held);
        let bytes = usize::try_from(
            want.min(ceiling).max(u64::try_from(DEFAULT_MAX_BYTES).unwrap_or(u64::MAX)),
        )
        .unwrap_or(DEFAULT_MAX_BYTES);
        self.max_bytes = bytes;
        self.checkpoint_bytes = Some(checkpoint_bytes);
        self.evict_over_budget();
        bytes
    }

    /// What one checkpoint costs for the loaded model, if a model has loaded.
    #[must_use]
    pub fn checkpoint_bytes(&self) -> Option<u64> {
        self.checkpoint_bytes
    }

    /// Whether the budget can hold a checkpoint of the given size at all.
    #[must_use]
    pub fn holds_a_checkpoint(&self, checkpoint_bytes: u64) -> bool {
        u64::try_from(self.max_bytes).unwrap_or(u64::MAX) >= checkpoint_bytes
    }

    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            max_entries,
            max_bytes,
            bytes: 0,
            entries: HashMap::new(),
            lru: VecDeque::new(),
            pinned: HashSet::new(),
            tombstones: VecDeque::new(),
            checkpoint_bytes: None,
            disk: None,
        }
    }

    /// Keep checkpoints this cache drops, and read them back when a later
    /// prompt extends one.
    ///
    /// Attaching a store immediately sweeps it for checkpoints stored under a
    /// runtime key that is no longer current — a different model, or a setting
    /// that changes what a saved state means — because a startup is when that
    /// is most likely to have happened, and those files can never match again.
    pub fn attach_disk(
        &mut self,
        store: crate::prompt_cache_disk::CheckpointStore,
        current_runtime: &str,
    ) -> usize {
        let dropped = store.sweep(current_runtime);
        self.disk = Some(store);
        dropped
    }

    /// The disk tier, if one is attached.
    pub fn disk(&self) -> Option<&crate::prompt_cache_disk::CheckpointStore> {
        self.disk.as_ref()
    }

    /// Read the deepest checkpoint on disk that extends nothing memory already
    /// has, and admit it to memory so the caller can restore it as an ordinary
    /// hit.
    ///
    /// Consulted only after a memory lookup came back shallower than
    /// [`PrefixMissCause::Deepest`], so a warm turn never touches the disk. The
    /// returned key is a hit in every sense — the entry is now resident — which
    /// is what keeps every caller's restore path single.
    pub fn promote_from_disk(
        &mut self,
        runtime: &str,
        layers: &[PromptStateCacheLayer],
        tokens: &[i32],
        already_matched: usize,
    ) -> Option<PromptStateCacheKey> {
        let found = self.disk.as_ref()?.lookup(runtime, layers, tokens)?;
        // A checkpoint no deeper than what memory already offers would cost a
        // read and save nothing.
        if found.prefix_tokens.len() <= already_matched {
            return None;
        }
        let key = found.key.clone();
        let entry = PromptStateCacheEntry::with_tokens(found.state, found.prefix_tokens);
        self.insert(key.clone(), entry);
        Some(key)
    }

    /// Write every resident checkpoint to disk.
    ///
    /// Called on a clean exit. Without it a restart throws away everything the
    /// cache holds — measured as the largest single loss in the whole tier,
    /// because an empty cache leaves no trace of what it used to have and the
    /// cost is invisible rather than merely large.
    ///
    /// Returns how many files were written. Already-stored checkpoints write
    /// nothing, so calling this twice is free.
    pub fn persist_all(&self) -> usize {
        let Some(disk) = self.disk.as_ref() else { return 0 };
        self.entries
            .iter()
            .filter(|(_, e)| !e.prefix_tokens.is_empty())
            .filter(|(k, e)| disk.store(k, &e.prefix_tokens, &e.state).unwrap_or(false))
            .count()
    }

    /// Total bytes currently held across all entries, pinned included. This is
    /// real memory, so it is what diagnostics report; it is deliberately not
    /// the quantity the byte budget bounds (see [`Self::evictable_totals`]).
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of currently pinned entries.
    pub fn pinned_len(&self) -> usize {
        self.pinned.len()
    }

    pub fn insert(
        &mut self,
        key: PromptStateCacheKey,
        entry: PromptStateCacheEntry,
    ) -> CacheMutationReport {
        if let Some(old) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(old.state.len());
            self.lru.retain(|existing| existing != &key);
        }
        self.bytes = self.bytes.saturating_add(entry.state.len());
        self.lru.push_back(key.clone());
        self.entries.insert(key.clone(), entry);
        let pruned = self.prune_dominated_by(&key);
        CacheMutationReport { evicted: self.evict_over_budget(), pruned, ..Default::default() }
    }

    /// Drop every non-pinned entry that the entry at `key` dominates: same
    /// `runtime_sha256`, with recorded `prefix_tokens` that are a *strict*
    /// prefix of `key`'s tokens. Such entries can never beat `key` in a
    /// [`Self::find_longest_prefix`] match, so retaining them only wastes a slot
    /// and a (multi-MB) state blob. No-op when the new entry records no tokens
    /// (exact-key-only entries don't participate in prefix matching).
    ///
    /// Normally confined to one `layer`, because layers exist precisely to keep
    /// checkpoints out of each other's way: a turn stores one at the start and
    /// one at the end, and letting the second delete the first left the next
    /// turn nothing to stand on. The exception is an entry the caller marked
    /// [`PromptStateCacheEntry::dies_with_turn`] — intra-turn scratch that
    /// nothing later can match anyway, so the checkpoint that supersedes it is
    /// free to reclaim it whatever layer it belongs to.
    fn prune_dominated_by(&mut self, key: &PromptStateCacheKey) -> Vec<EvictedEntry> {
        let Some(new_entry) = self.entries.get(key) else { return Vec::new() };
        if new_entry.prefix_tokens.is_empty() {
            return Vec::new();
        }
        let new_tokens = new_entry.prefix_tokens.clone();
        let dominated: Vec<PromptStateCacheKey> = self
            .entries
            .iter()
            .filter(|(k, e)| {
                *k != key
                    && !self.pinned.contains(k)
                    && (k.layer == key.layer || e.dies_with_turn)
                    && k.runtime_sha256 == key.runtime_sha256
                    && !e.prefix_tokens.is_empty()
                    && e.prefix_tokens.len() < new_tokens.len()
                    && new_tokens.starts_with(&e.prefix_tokens)
            })
            .map(|(k, _)| k.clone())
            .collect();
        let mut pruned = Vec::with_capacity(dominated.len());
        for k in dominated {
            if let Some(entry) = self.entries.remove(&k) {
                self.bytes = self.bytes.saturating_sub(entry.state.len());
                self.lru.retain(|existing| existing != &k);
                pruned.push(EvictedEntry {
                    layer: k.layer.clone(),
                    token_count: entry.token_count,
                    bytes: entry.state.len(),
                });
            }
        }
        pruned
    }

    /// Insert a base prefix and protect it from eviction. Only the most recent
    /// snapshot of a given pinnable layer stays pinned — when the active prompt
    /// (and therefore the key) changes, the stale pin is released so it can age
    /// out normally.
    pub fn insert_pinned(
        &mut self,
        key: PromptStateCacheKey,
        entry: PromptStateCacheEntry,
    ) -> CacheMutationReport {
        let layer = key.layer.clone();
        let pin_released =
            self.pinned.iter().any(|existing| existing.layer == layer).then(|| layer.clone());
        self.pinned.retain(|existing| existing.layer != layer);
        self.pinned.insert(key.clone());
        let mut report = self.insert(key, entry);
        report.pinned = Some(layer);
        report.pin_released = pin_released;
        report
    }

    pub fn get(&mut self, key: &PromptStateCacheKey) -> Option<PromptStateCacheEntry> {
        let stored = self.entries.get_mut(key)?;
        stored.last_used = Instant::now();
        let entry = stored.clone();
        self.lru.retain(|existing| existing != key);
        self.lru.push_back(key.clone());
        Some(entry)
    }

    /// Membership test that **also marks the entry as most-recently-used**, and
    /// clones its state blob on the way through [`Self::get`]. Use
    /// [`Self::peek`] from anything that must not disturb eviction order.
    pub fn contains(&mut self, key: &PromptStateCacheKey) -> bool {
        self.get(key).is_some()
    }

    /// Membership test with no side effects and no blob copy.
    pub fn peek(&self, key: &PromptStateCacheKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Borrowed metadata for every entry, cheapest-possible: no state blob is
    /// cloned and eviction order is untouched. Ordered by eviction queue
    /// position so the caller sees which entry dies first.
    pub fn nodes(&self) -> Vec<CacheNodeView<'_>> {
        let rank: HashMap<&PromptStateCacheKey, usize> =
            self.lru.iter().enumerate().map(|(i, k)| (k, i)).collect();
        let mut views: Vec<CacheNodeView<'_>> = self
            .entries
            .iter()
            .map(|(key, entry)| CacheNodeView {
                id: key.stable_id(),
                layer: &key.layer,
                runtime_sha256: &key.runtime_sha256,
                token_count: entry.token_count,
                bytes: entry.state.len(),
                pinned: self.pinned.contains(key),
                lru_rank: rank.get(key).copied().unwrap_or(usize::MAX),
                idle: entry.idle(),
                prefix_tokens: &entry.prefix_tokens,
                preview: entry.preview(),
            })
            .collect();
        views.sort_by_key(|v| v.lru_rank);
        views
    }

    /// Budget the cache enforces: maximum evictable entries.
    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    /// Budget the cache enforces: maximum evictable bytes.
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Count and byte total eviction is allowed to reclaim, i.e. everything
    /// except the pins. This is what both budgets are measured against; see
    /// [`Self::evictable_totals`].
    pub fn evictable(&self) -> (usize, usize) {
        self.evictable_totals()
    }

    pub fn is_pinned(&self, key: &PromptStateCacheKey) -> bool {
        self.pinned.contains(key)
    }

    /// Find the cached entry whose recorded `prefix_tokens` is the longest
    /// *proper* token-prefix of `tokens`, restricted to the given `runtime` and
    /// `layers`. Returns the matching key so the caller can restore it and
    /// decode only the remaining tokens. Entries built without recorded tokens
    /// (via [`PromptStateCacheEntry::new`]) never match. This is the graceful
    /// fallback used when an exact-key lookup misses: e.g. a fresh app-context
    /// prefix can still restore the pinned base prefix and decode just the
    /// per-context delta instead of paying a full cold prefill.
    pub fn find_longest_prefix(
        &self,
        runtime: &str,
        layers: &[PromptStateCacheLayer],
        tokens: &[i32],
    ) -> Option<PromptStateCacheKey> {
        self.entries
            .iter()
            .filter(|(k, e)| {
                k.runtime_sha256 == runtime
                    && layers.contains(&k.layer)
                    && !e.prefix_tokens.is_empty()
                    && e.prefix_tokens.len() < tokens.len()
                    && tokens.starts_with(&e.prefix_tokens)
            })
            .max_by_key(|(_, e)| e.prefix_tokens.len())
            .map(|(k, _)| k.clone())
    }

    /// [`Self::find_longest_prefix`], plus the reason the match is not deeper.
    ///
    /// Same search, so a caller can use this everywhere the plain lookup was
    /// used and pay only a second scan of the same entries. The extra work is
    /// entirely in the miss case: when nothing matched, it asks *why*, which is
    /// the question a live cache cannot otherwise answer. A shallow match looks
    /// identical whether the prompt was rewritten or the checkpoint was
    /// evicted, and only the second of those is a case more storage would fix.
    pub fn explain_longest_prefix(
        &self,
        runtime: &str,
        layers: &[PromptStateCacheLayer],
        tokens: &[i32],
    ) -> PrefixLookup {
        let hit = self.find_longest_prefix(runtime, layers, tokens);
        let matched_tokens =
            hit.as_ref().and_then(|k| self.entries.get(k)).map_or(0, |e| e.prefix_tokens.len());
        let decoded_prefix_tokens = tokens.len().saturating_sub(matched_tokens);

        // A tombstone that reaches past the live match is the disk tier's whole
        // case, so it is asked first: it is the only cause that names bytes a
        // tier could have read back.
        let recoverable = self
            .tombstones
            .iter()
            .filter(|t| {
                t.runtime_sha256 == runtime
                    && layers.contains(&t.layer)
                    && t.prefix_tokens.len() > matched_tokens
                    && t.prefix_tokens.len() < tokens.len()
                    && tokens.starts_with(&t.prefix_tokens)
            })
            .max_by_key(|t| t.prefix_tokens.len());
        if let Some(t) = recoverable {
            return PrefixLookup {
                hit,
                matched_tokens,
                decoded_prefix_tokens,
                cause: PrefixMissCause::Eviction {
                    recoverable_tokens: t.prefix_tokens.len(),
                    bytes: t.bytes,
                    layer: t.layer.clone(),
                },
            };
        }
        if matched_tokens > 0 {
            return PrefixLookup {
                hit,
                matched_tokens,
                decoded_prefix_tokens,
                cause: PrefixMissCause::Deepest,
            };
        }

        // Nothing matched and nothing recoverable was dropped. Either this
        // runtime has never stored anything, or it has and the prompt moved
        // away from all of it — a difference the tier cannot close, but which
        // decides whether it is worth trying.
        let candidate = self
            .entries
            .iter()
            .filter(|(k, e)| {
                k.runtime_sha256 == runtime
                    && layers.contains(&k.layer)
                    && !e.prefix_tokens.is_empty()
            })
            .map(|(_, e)| {
                let at = e
                    .prefix_tokens
                    .iter()
                    .zip(tokens)
                    .position(|(a, b)| a != b)
                    .unwrap_or_else(|| e.prefix_tokens.len().min(tokens.len()));
                (at, e.prefix_tokens.len())
            })
            .max_by_key(|(at, _)| *at);
        let cause = match candidate {
            Some((at, candidate_tokens)) => PrefixMissCause::Divergence { at, candidate_tokens },
            None => PrefixMissCause::RuntimeKeyChange,
        };
        PrefixLookup { hit, matched_tokens, decoded_prefix_tokens, cause }
    }

    pub fn remove_layer(&mut self, layer: &PromptStateCacheLayer) {
        let removed: Vec<_> = self.entries.keys().filter(|k| &k.layer == layer).cloned().collect();
        for key in removed {
            if let Some(entry) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(entry.state.len());
            }
            self.lru.retain(|existing| existing != &key);
            self.pinned.remove(&key);
        }
    }

    /// Count and byte total of the entries eviction is allowed to touch, i.e.
    /// everything except the pins.
    ///
    /// Both budgets are measured against this rather than against the whole
    /// cache, because a pin is by definition unreclaimable: counting pins would
    /// only shrink the space left for the conversation checkpoints eviction can
    /// manage. Under the entry cap that costs one slot per pinnable layer —
    /// three of ten — before a single conversation is cached. Under the byte
    /// budget it is worse than proportional: on a model with a large KV
    /// footprint one pinned tool catalogue can exceed the budget by itself, and
    /// then every unpinned entry is evicted on every insert and the cache stops
    /// working at all.
    fn evictable_totals(&self) -> (usize, usize) {
        self.entries
            .iter()
            .filter(|(key, _)| !self.pinned.contains(*key))
            .fold((0, 0), |(count, bytes), (_, entry)| (count + 1, bytes + entry.state.len()))
    }

    fn evict_over_budget(&mut self) -> Vec<EvictedEntry> {
        let mut evicted = Vec::new();
        loop {
            let (count, bytes) = self.evictable_totals();
            if count <= self.max_entries && bytes <= self.max_bytes {
                break;
            }
            // Evict the oldest entry that is not pinned. If only pinned entries
            // remain we stop rather than drop a protected checkpoint.
            let Some(pos) = self.lru.iter().position(|k| !self.pinned.contains(k)) else {
                break;
            };
            let Some(key) = self.lru.remove(pos) else { break };
            if let Some(entry) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(entry.state.len());
                // Hand it to disk on the way out. This is the case the whole
                // tier exists for: the checkpoint is still wanted, memory just
                // has no room for it.
                if let Some(disk) = self.disk.as_ref() {
                    let _ = disk.store(&key, &entry.prefix_tokens, &entry.state);
                }
                self.remember_evicted(&key, &entry);
                evicted.push(EvictedEntry {
                    layer: key.layer.clone(),
                    token_count: entry.token_count,
                    bytes: entry.state.len(),
                });
            }
        }
        evicted
    }

    /// Keep the token sequence of an evicted entry so a later lookup can say
    /// what the budget cost it. Drops the oldest tombstones once the ring is
    /// over its token budget.
    fn remember_evicted(&mut self, key: &PromptStateCacheKey, entry: &PromptStateCacheEntry) {
        // An entry with no recorded tokens could never win a longest-prefix
        // match while it was alive, so losing it costs a lookup nothing.
        if entry.prefix_tokens.is_empty() {
            return;
        }
        self.tombstones.push_back(Tombstone {
            runtime_sha256: key.runtime_sha256.clone(),
            layer: key.layer.clone(),
            prefix_tokens: entry.prefix_tokens.clone(),
            bytes: entry.state.len(),
        });
        let mut held: usize = self.tombstones.iter().map(|t| t.prefix_tokens.len()).sum();
        while held > TOMBSTONE_TOKEN_BUDGET {
            let Some(dropped) = self.tombstones.pop_front() else { break };
            held -= dropped.prefix_tokens.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(layer: PromptStateCacheLayer, id: &str) -> PromptStateCacheKey {
        PromptStateCacheKey::new(layer, "runtime", id, id, 1)
    }

    fn entry(bytes: usize) -> PromptStateCacheEntry {
        PromptStateCacheEntry::new(vec![0_u8; bytes], 1)
    }

    // The miss taxonomy exists to answer one question: of the prefix tokens a
    // turn reads again, how many would a checkpoint on disk have saved? Only
    // one of the four causes is recoverable that way, so each is pinned here.

    fn tokened(
        id: &str,
        tokens: &[i32],
        bytes: usize,
    ) -> (PromptStateCacheKey, PromptStateCacheEntry) {
        (
            PromptStateCacheKey::new(
                PromptStateCacheLayer::F8ChatPrefix,
                "runtime",
                id,
                id,
                tokens.len(),
            ),
            PromptStateCacheEntry::with_tokens(vec![0_u8; bytes], tokens.to_vec()),
        )
    }

    const CHAT: [PromptStateCacheLayer; 1] = [PromptStateCacheLayer::F8ChatPrefix];

    #[test]
    fn an_empty_cache_blames_the_runtime_key() {
        let cache = PromptStateCache::new(10, 1024 * 1024);
        let miss = cache.explain_longest_prefix("runtime", &CHAT, &[1, 2, 3]);
        assert_eq!(miss.cause, PrefixMissCause::RuntimeKeyChange);
        assert_eq!(miss.decoded_prefix_tokens, 3);
    }

    #[test]
    fn a_deepest_possible_match_reports_nothing_lost() {
        let mut cache = PromptStateCache::new(10, 1024 * 1024);
        let (k, e) = tokened("turn-1", &[1, 2, 3], 64);
        cache.insert(k, e);
        let hit = cache.explain_longest_prefix("runtime", &CHAT, &[1, 2, 3, 4, 5]);
        assert_eq!(hit.cause, PrefixMissCause::Deepest);
        assert_eq!(hit.matched_tokens, 3);
        assert_eq!(hit.decoded_prefix_tokens, 2);
    }

    #[test]
    fn a_rewritten_prompt_is_divergence_and_names_where() {
        // The failure a disk tier cannot fix: the client changed the front of
        // the prompt, so the stored checkpoint describes a different sentence.
        let mut cache = PromptStateCache::new(10, 1024 * 1024);
        let (k, e) = tokened("turn-1", &[1, 2, 3, 4], 64);
        cache.insert(k, e);
        let miss = cache.explain_longest_prefix("runtime", &CHAT, &[1, 2, 99, 4, 5]);
        assert_eq!(miss.matched_tokens, 0);
        assert_eq!(
            miss.cause,
            PrefixMissCause::Divergence { at: 2, candidate_tokens: 4 },
            "the prompt agrees for two tokens, and the candidate held four"
        );
    }

    #[test]
    fn a_checkpoint_larger_than_the_whole_budget_never_survives_its_own_insert() {
        // Not a corner case: a checkpoint costs context × model, so a large
        // model at a long context can exceed any fixed budget on its own. Such
        // an entry is admitted and then dropped by the very same enforcement
        // pass, so the cache holds nothing and every later turn re-reads the
        // whole prompt. Sizing the budget from free RAM makes this rarer, not
        // impossible.
        //
        // The taxonomy has to call that eviction rather than a cold cache,
        // because it is the case a disk tier is for.
        let mut cache = PromptStateCache::new(10, 1024);
        let (k, e) = tokened("oversize", &[1, 2, 3, 4], 4096);
        let report = cache.insert(k.clone(), e);
        assert!(!cache.contains(&k), "an entry over the whole budget cannot be kept");
        assert_eq!(report.evicted.len(), 1, "and it is dropped as an eviction, not refused");
        assert_eq!(report.evicted[0].bytes, 4096);

        let miss = cache.explain_longest_prefix("runtime", &CHAT, &[1, 2, 3, 4, 5]);
        assert_eq!(
            miss.cause,
            PrefixMissCause::Eviction {
                recoverable_tokens: 4,
                bytes: 4096,
                layer: PromptStateCacheLayer::F8ChatPrefix
            },
            "the budget, not the prompt, is what cost this lookup its match"
        );
    }

    #[test]
    fn the_budget_follows_free_ram_between_a_floor_and_a_ceiling() {
        // A machine with nothing spare keeps the old fixed budget.
        assert_eq!(PromptStateCache::sized_from(2 << 30).max_bytes(), DEFAULT_MAX_BYTES);
        // 36 GiB free: 32 GiB past the desktop's reserve, a quarter of it ours.
        assert_eq!(PromptStateCache::sized_from(36 << 30).max_bytes(), 8 << 30);
        // And no machine, however large, gives the cache more than the cap.
        assert_eq!(
            PromptStateCache::sized_from(512 << 30).max_bytes(),
            usize::try_from(MAX_MAX_BYTES).expect("64-bit target")
        );
    }

    #[test]
    fn the_budget_is_counted_in_checkpoints_once_the_model_is_known() {
        const GIB: u64 = 1 << 30;
        // gemma-4-26B at 16k: 1.79 GiB a checkpoint, on a 36 GiB machine.
        // Three of them fit under the 8 GiB ceiling, so three is what it takes.
        let mut cache = PromptStateCache::sized_from(36 * GIB);
        let checkpoint = 1_833_700_000_u64;
        let budget = cache.resize_for_checkpoint(checkpoint, 36 * GIB);
        assert_eq!(budget, usize::try_from(checkpoint * CHECKPOINTS_HELD).expect("64-bit"));
        assert!(cache.holds_a_checkpoint(checkpoint));

        // A cheap model does not shrink below the floor just because its
        // checkpoints are small — a short conversation should keep several.
        let budget = cache.resize_for_checkpoint(4 * 1024 * 1024, 36 * GIB);
        assert_eq!(budget, DEFAULT_MAX_BYTES, "the floor still applies from below");

        // And free RAM remains the ceiling: a machine with 6 GiB free has
        // 512 MiB to give, which will not hold this checkpoint. The caller has
        // to be able to see that, because the cache is then inert.
        let mut tight = PromptStateCache::sized_from(6 * GIB);
        let budget = tight.resize_for_checkpoint(checkpoint, 6 * GIB);
        assert_eq!(budget, 512 * 1024 * 1024, "clamped to what the machine has");
        assert!(!tight.holds_a_checkpoint(checkpoint), "and it cannot hold one");
    }

    #[test]
    fn re_budgeting_downward_evicts_what_no_longer_fits() {
        let mut cache = PromptStateCache::new(10, 8192);
        // Two unrelated conversations, so neither dominates the other and both
        // survive on their own merits.
        let (k1, e1) = tokened("turn-1", &[1, 2], 2048);
        let (k2, e2) = tokened("turn-2", &[7, 8, 9], 2048);
        cache.insert(k1.clone(), e1);
        cache.insert(k2.clone(), e2);
        assert!(cache.contains(&k1) && cache.contains(&k2));

        // Free RAM collapsed; the new budget holds one checkpoint, not two.
        // The oldest goes, and it goes through the normal eviction path so the
        // taxonomy can still say what it cost.
        cache.max_bytes = 2048;
        cache.evict_over_budget();
        assert!(!cache.contains(&k1), "the oldest entry is dropped");
        assert!(cache.contains(&k2), "the newest survives");
    }

    #[test]
    fn a_checkpoint_the_budget_dropped_is_reported_as_recoverable() {
        // The failure a disk tier exists for. A byte budget too small for two
        // checkpoints evicts the first; the next prompt extends it and would
        // have matched. Without the tombstone this is indistinguishable from a
        // prompt that was rewritten, and the two lead to opposite decisions.
        let mut cache = PromptStateCache::new(10, 1024);
        let (k1, e1) = tokened("turn-1", &[1, 2, 3, 4], 900);
        cache.insert(k1.clone(), e1);
        let (k2, e2) = tokened("other", &[7, 7, 7], 900);
        let report = cache.insert(k2, e2);
        assert_eq!(report.evicted.len(), 1, "the budget must have dropped turn-1");
        assert!(!cache.contains(&k1));

        let miss = cache.explain_longest_prefix("runtime", &CHAT, &[1, 2, 3, 4, 5, 6]);
        assert_eq!(miss.matched_tokens, 0, "nothing live matches");
        assert_eq!(
            miss.cause,
            PrefixMissCause::Eviction {
                recoverable_tokens: 4,
                bytes: 900,
                layer: PromptStateCacheLayer::F8ChatPrefix,
            }
        );
    }

    /// A scratch directory under `target/`, never under `/tmp`: that is `tmpfs`
    /// on many systems and the store refuses a directory held in memory, so a
    /// test using it would exercise the refusal instead of the tier.
    fn disk_dir(name: &str) -> std::path::PathBuf {
        let dir = std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/ckpt-tests"
        ))
        .join(format!("wire-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_checkpoint_the_budget_drops_lands_on_disk_and_comes_back() {
        // The whole tier in one turn: memory runs out, the checkpoint goes to
        // disk on the way out, and a later prompt extending it reads back as an
        // ordinary resident hit.
        let dir = disk_dir("evict-roundtrip");
        let store =
            crate::prompt_cache_disk::CheckpointStore::open(dir.clone(), 10 * 1024 * 1024).unwrap();
        let mut cache = PromptStateCache::new(10, 1024);
        cache.attach_disk(store, "runtime");

        let (k1, e1) = tokened("turn-1", &[1, 2, 3, 4], 900);
        cache.insert(k1.clone(), e1);
        let (k2, e2) = tokened("other", &[7, 7, 7], 900);
        cache.insert(k2, e2);
        assert!(!cache.contains(&k1), "the budget must have dropped turn-1");

        let promoted = cache
            .promote_from_disk("runtime", &CHAT, &[1, 2, 3, 4, 5], 0)
            .expect("the evicted checkpoint is readable again");
        assert_eq!(promoted, k1);
        assert!(cache.contains(&k1), "promotion makes it resident, so restore is one path");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_shallower_checkpoint_on_disk_is_not_worth_reading() {
        // Reading a checkpoint no deeper than what memory already offers costs
        // a read and saves nothing.
        let dir = disk_dir("no-gain");
        let store =
            crate::prompt_cache_disk::CheckpointStore::open(dir.clone(), 10 * 1024 * 1024).unwrap();
        let mut cache = PromptStateCache::new(10, 1024);
        cache.attach_disk(store, "runtime");
        let (k1, e1) = tokened("turn-1", &[1, 2, 3, 4], 900);
        cache.insert(k1, e1);
        assert_eq!(cache.persist_all(), 1);

        assert!(
            cache.promote_from_disk("runtime", &CHAT, &[1, 2, 3, 4, 5], 4).is_none(),
            "a four-token checkpoint adds nothing to a four-token match"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_clean_exit_writes_what_a_restart_would_otherwise_lose() {
        // The largest measured loss in the tier: a restart empties the cache
        // and leaves no trace of what it held, so the cost is invisible rather
        // than merely large. Writing on exit is the whole remedy.
        let dir = disk_dir("persist-all");
        let store =
            crate::prompt_cache_disk::CheckpointStore::open(dir.clone(), 10 * 1024 * 1024).unwrap();
        let mut cache = PromptStateCache::new(10, 1024 * 1024);
        cache.attach_disk(store, "runtime");
        for i in 0..3 {
            let (k, e) = tokened(&format!("turn-{i}"), &[1, 2, i], 64);
            cache.insert(k, e);
        }
        assert_eq!(cache.persist_all(), 3, "every resident checkpoint is written");
        assert_eq!(cache.persist_all(), 0, "a second call rewrites nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tombstone_ring_stays_inside_its_token_budget() {
        // The taxonomy must not become a second cache. Evict far more than the
        // ring can hold and it still bounds what it keeps.
        let mut cache = PromptStateCache::new(1, 1024);
        let chunk: Vec<i32> = (0..8192).collect();
        for i in 0..64 {
            let (k, e) = tokened(&format!("turn-{i}"), &chunk, 900);
            cache.insert(k, e);
        }
        let held: usize = cache.tombstones.iter().map(|t| t.prefix_tokens.len()).sum();
        assert!(held <= TOMBSTONE_TOKEN_BUDGET, "tombstone ring held {held} tokens");
    }

    #[test]
    fn a_large_checkpoint_is_stored_like_any_other() {
        // Checkpoints of a couple of gigabytes were once refused, on the
        // belief that llama.cpp reloaded them wrongly. Measurement showed the
        // reload is exact and the suspect replies came from an ambiguous test
        // prompt, so size alone must not keep a checkpoint out; only the
        // cache's own byte budget decides what fits.
        let mut cache = PromptStateCache::new(10, 1024 * 1024);
        let big = key(PromptStateCacheLayer::F8ChatPrefix, "turn-1");
        let report = cache.insert(big.clone(), entry(512 * 1024));
        assert!(report.evicted.is_empty());
        assert!(cache.contains(&big));
    }

    #[test]
    fn shard_paths_enumerate_every_gguf_shard() {
        // llama.cpp is handed shard 1 and finds the rest; the fingerprint has
        // to see all of them or the bulk of the weights stays invisible to the
        // key, and a swapped quantization restores stale states silently.
        let first = PathBuf::from("/models/DeepSeek-V4-Flash-UD-IQ3_XXS-00001-of-00004.gguf");
        let shards = model_shard_paths(&first);
        assert_eq!(shards.len(), 4);
        assert_eq!(shards[0], first);
        assert_eq!(
            shards[3],
            PathBuf::from("/models/DeepSeek-V4-Flash-UD-IQ3_XXS-00004-of-00004.gguf")
        );
        // Naming a later shard yields the same set, so the key does not depend
        // on which shard the config happens to point at.
        assert_eq!(model_shard_paths(&shards[2]), shards);
    }

    #[test]
    fn shard_paths_leave_unsharded_models_alone() {
        for name in ["gemma-4-26B-it-Q8_0.gguf", "weird-of-4.gguf", "no-extension", "a-1-of-0.gguf"]
        {
            let p = PathBuf::from("/models").join(name);
            assert_eq!(model_shard_paths(&p), vec![p.clone()], "{name}");
        }
    }

    #[test]
    fn missing_shard_fails_the_fingerprint() {
        // A half-downloaded model must not produce a fingerprint at all: a
        // shorter one would silently collide with the complete download.
        let dir = std::env::temp_dir().join(format!("fono-shard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let first = dir.join("m-00001-of-00002.gguf");
        std::fs::write(&first, b"x").expect("write shard");
        assert!(model_files_fingerprint(&first).is_err(), "missing shard 2");
        std::fs::write(dir.join("m-00002-of-00002.gguf"), b"yy").expect("write shard");
        let fingerprint = model_files_fingerprint(&first).expect("complete model");
        assert!(fingerprint.contains("m-00001-of-00002.gguf:1:"), "{fingerprint}");
        assert!(fingerprint.contains("m-00002-of-00002.gguf:2:"), "{fingerprint}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lru_evicts_oldest_first() {
        let mut cache = PromptStateCache::new(2, usize::MAX);
        cache.insert(key(PromptStateCacheLayer::ExactPrompt, "a"), entry(1));
        cache.insert(key(PromptStateCacheLayer::ExactPrompt, "b"), entry(1));
        cache.insert(key(PromptStateCacheLayer::ExactPrompt, "c"), entry(1));
        assert!(!cache.contains(&key(PromptStateCacheLayer::ExactPrompt, "a")));
        assert!(cache.contains(&key(PromptStateCacheLayer::ExactPrompt, "b")));
        assert!(cache.contains(&key(PromptStateCacheLayer::ExactPrompt, "c")));
    }

    #[test]
    fn touching_an_entry_makes_it_most_recently_used() {
        let mut cache = PromptStateCache::new(2, usize::MAX);
        cache.insert(key(PromptStateCacheLayer::ExactPrompt, "a"), entry(1));
        cache.insert(key(PromptStateCacheLayer::ExactPrompt, "b"), entry(1));
        let _ = cache.get(&key(PromptStateCacheLayer::ExactPrompt, "a")); // bump a
        cache.insert(key(PromptStateCacheLayer::ExactPrompt, "c"), entry(1));
        // b was least-recently-used and should be evicted, not a.
        assert!(cache.contains(&key(PromptStateCacheLayer::ExactPrompt, "a")));
        assert!(!cache.contains(&key(PromptStateCacheLayer::ExactPrompt, "b")));
    }

    #[test]
    fn byte_budget_is_tracked_and_enforced() {
        let mut cache = PromptStateCache::new(usize::MAX, 64);
        cache.insert(key(PromptStateCacheLayer::ExactPrompt, "a"), entry(48));
        cache.insert(key(PromptStateCacheLayer::ExactPrompt, "b"), entry(48));
        assert!(cache.bytes() <= 64);
        assert!(!cache.contains(&key(PromptStateCacheLayer::ExactPrompt, "a")));
        assert!(cache.contains(&key(PromptStateCacheLayer::ExactPrompt, "b")));
    }

    #[test]
    fn pinned_base_survives_entry_count_eviction() {
        let mut cache = PromptStateCache::new(2, usize::MAX);
        cache.insert_pinned(key(PromptStateCacheLayer::F8System, "sys"), entry(8));
        for i in 0..6 {
            cache.insert(key(PromptStateCacheLayer::F8ChatPrefix, &format!("turn{i}")), entry(8));
        }
        assert!(cache.contains(&key(PromptStateCacheLayer::F8System, "sys")));
        assert!(cache.len() <= cache.max_entries + cache.pinned_len());
    }

    #[test]
    fn pinned_base_survives_byte_budget_eviction() {
        let mut cache = PromptStateCache::new(usize::MAX, 64);
        cache.insert_pinned(key(PromptStateCacheLayer::F7System, "base"), entry(32));
        for i in 0..4 {
            cache.insert(key(PromptStateCacheLayer::ExactPrompt, &format!("p{i}")), entry(48));
        }
        assert!(cache.contains(&key(PromptStateCacheLayer::F7System, "base")));
    }

    #[test]
    fn pins_do_not_consume_the_entry_cap() {
        // Both pinnable layers can be occupied at once. If pins counted against
        // the cap they would take two of its slots before a single conversation
        // is cached.
        let mut cache = PromptStateCache::new(2, usize::MAX);
        cache.insert_pinned(key(PromptStateCacheLayer::F7System, "f7"), entry(8));
        cache.insert_pinned(key(PromptStateCacheLayer::F8System, "f8"), entry(8));
        cache.insert(key(PromptStateCacheLayer::F8ChatPrefix, "a"), entry(8));
        cache.insert(key(PromptStateCacheLayer::F8ChatPrefix, "b"), entry(8));
        // The cap is two *evictable* entries, so both conversations survive
        // alongside both pins.
        assert!(cache.contains(&key(PromptStateCacheLayer::F8ChatPrefix, "a")));
        assert!(cache.contains(&key(PromptStateCacheLayer::F8ChatPrefix, "b")));
        assert_eq!(cache.len(), 4);
    }

    #[test]
    fn a_pin_larger_than_the_byte_budget_does_not_empty_the_cache() {
        // On a model with a large KV footprint one pinned system prompt can
        // exceed the whole budget. Counting it would evict every unpinned entry
        // on every insert, leaving the cache permanently empty.
        let mut cache = PromptStateCache::new(usize::MAX, 64);
        cache.insert_pinned(key(PromptStateCacheLayer::F8System, "huge"), entry(4096));
        cache.insert(key(PromptStateCacheLayer::F8ChatPrefix, "a"), entry(32));
        cache.insert(key(PromptStateCacheLayer::F8ChatPrefix, "b"), entry(32));
        assert!(cache.contains(&key(PromptStateCacheLayer::F8ChatPrefix, "a")));
        assert!(cache.contains(&key(PromptStateCacheLayer::F8ChatPrefix, "b")));
        // `bytes` still reports true memory, pin included.
        assert_eq!(cache.bytes(), 4096 + 64);
    }

    #[test]
    fn releasing_a_pin_returns_it_to_the_budget() {
        let mut cache = PromptStateCache::new(1, usize::MAX);
        cache.insert_pinned(key(PromptStateCacheLayer::F8System, "old"), entry(8));
        cache.insert(key(PromptStateCacheLayer::F8ChatPrefix, "chat"), entry(8));
        // Repinning the layer unpins "old", which now competes for the single
        // evictable slot and loses to the more recent chat checkpoint.
        cache.insert_pinned(key(PromptStateCacheLayer::F8System, "new"), entry(8));
        assert!(!cache.contains(&key(PromptStateCacheLayer::F8System, "old")));
        assert!(cache.contains(&key(PromptStateCacheLayer::F8System, "new")));
        assert!(cache.contains(&key(PromptStateCacheLayer::F8ChatPrefix, "chat")));
    }

    #[test]
    fn repinning_same_layer_releases_stale_pin() {
        let mut cache = PromptStateCache::new(2, usize::MAX);
        cache.insert_pinned(key(PromptStateCacheLayer::F8System, "old"), entry(8));
        cache.insert_pinned(key(PromptStateCacheLayer::F8System, "new"), entry(8));
        assert!(!cache.is_pinned(&key(PromptStateCacheLayer::F8System, "old")));
        assert!(cache.is_pinned(&key(PromptStateCacheLayer::F8System, "new")));
        for i in 0..6 {
            cache.insert(key(PromptStateCacheLayer::F8ChatPrefix, &format!("t{i}")), entry(8));
        }
        assert!(cache.contains(&key(PromptStateCacheLayer::F8System, "new")));
        assert!(!cache.contains(&key(PromptStateCacheLayer::F8System, "old")));
    }

    #[test]
    fn remove_layer_clears_pin() {
        let mut cache = PromptStateCache::default();
        cache.insert_pinned(key(PromptStateCacheLayer::F8System, "sys"), entry(8));
        cache.remove_layer(&PromptStateCacheLayer::F8System);
        assert_eq!(cache.pinned_len(), 0);
        assert!(!cache.contains(&key(PromptStateCacheLayer::F8System, "sys")));
    }

    fn token_entry(tokens: &[i32]) -> PromptStateCacheEntry {
        PromptStateCacheEntry::with_tokens(vec![0_u8; 8], tokens.to_vec())
    }

    #[test]
    fn longest_prefix_picks_deepest_match() {
        let mut cache = PromptStateCache::new(8, usize::MAX);
        cache.insert(key(PromptStateCacheLayer::F7System, "base"), token_entry(&[1, 2, 3]));
        cache.insert(key(PromptStateCacheLayer::F7Context, "ctx"), token_entry(&[1, 2, 3, 4, 5]));
        let layers = [PromptStateCacheLayer::F7System, PromptStateCacheLayer::F7Context];
        let hit = cache.find_longest_prefix("runtime", &layers, &[1, 2, 3, 4, 5, 6, 7]).unwrap();
        // The 5-token context prefix is deeper than the 3-token base.
        assert_eq!(hit.layer(), &PromptStateCacheLayer::F7Context);
    }

    #[test]
    fn longest_prefix_requires_true_prefix_and_runtime() {
        let mut cache = PromptStateCache::new(8, usize::MAX);
        cache.insert(key(PromptStateCacheLayer::F7System, "base"), token_entry(&[1, 2, 3]));
        let layers = [PromptStateCacheLayer::F7System];
        // Diverging tokens -> no match.
        assert!(cache.find_longest_prefix("runtime", &layers, &[1, 2, 9, 9]).is_none());
        // Wrong runtime -> no match.
        assert!(cache.find_longest_prefix("other", &layers, &[1, 2, 3, 4]).is_none());
        // Equal length (not a *proper* prefix, nothing left to decode) -> no match.
        assert!(cache.find_longest_prefix("runtime", &layers, &[1, 2, 3]).is_none());
        // Proper prefix, right runtime -> match.
        assert!(cache.find_longest_prefix("runtime", &layers, &[1, 2, 3, 4]).is_some());
    }

    #[test]
    fn longest_prefix_ignores_tokenless_entries() {
        let mut cache = PromptStateCache::new(8, usize::MAX);
        // Built via `new` -> no recorded tokens -> never a longest-prefix candidate.
        cache.insert(key(PromptStateCacheLayer::F7System, "base"), entry(8));
        let layers = [PromptStateCacheLayer::F7System];
        assert!(cache.find_longest_prefix("runtime", &layers, &[1, 2, 3, 4]).is_none());
    }

    #[test]
    fn insert_prunes_dominated_same_layer_prefix() {
        // Turn N's completed-turn checkpoint supersedes turn N-1's: inserting a
        // deeper entry of the same layer drops the strict-prefix one.
        let mut cache = PromptStateCache::new(8, usize::MAX);
        cache.insert(key(PromptStateCacheLayer::F8ChatPrefix, "shallow"), token_entry(&[1, 2]));
        let report = cache
            .insert(key(PromptStateCacheLayer::F8ChatPrefix, "deep"), token_entry(&[1, 2, 3, 4]));
        assert_eq!(report.pruned.len(), 1);
        assert!(!cache.contains(&key(PromptStateCacheLayer::F8ChatPrefix, "shallow")));
        assert!(cache.contains(&key(PromptStateCacheLayer::F8ChatPrefix, "deep")));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn insert_keeps_non_prefix_sibling() {
        // Two conversations that diverge early are not prefixes of one another;
        // neither dominates, so both are retained.
        let mut cache = PromptStateCache::new(8, usize::MAX);
        cache.insert(key(PromptStateCacheLayer::F8ChatPrefix, "a"), token_entry(&[1, 2, 3]));
        let report =
            cache.insert(key(PromptStateCacheLayer::F8ChatPrefix, "b"), token_entry(&[1, 9, 9]));
        assert!(report.pruned.is_empty());
        assert!(cache.contains(&key(PromptStateCacheLayer::F8ChatPrefix, "a")));
        assert!(cache.contains(&key(PromptStateCacheLayer::F8ChatPrefix, "b")));
    }

    #[test]
    fn prune_never_touches_pinned_base_or_other_layers() {
        // A pinned base of a different layer is a token-prefix of the chat
        // checkpoint, but must survive: pruning is same-layer and skips pins.
        let mut cache = PromptStateCache::new(8, usize::MAX);
        cache.insert_pinned(key(PromptStateCacheLayer::F8System, "base"), token_entry(&[1, 2]));
        let report = cache
            .insert(key(PromptStateCacheLayer::F8ChatPrefix, "deep"), token_entry(&[1, 2, 3, 4]));
        assert!(report.pruned.is_empty());
        assert!(cache.contains(&key(PromptStateCacheLayer::F8System, "base")));
        assert!(cache.is_pinned(&key(PromptStateCacheLayer::F8System, "base")));
    }

    #[test]
    fn a_completed_turn_reclaims_its_own_scratch_but_spares_the_turn_start_prefix() {
        // A tool-calling turn writes three checkpoints. Only two are worth
        // keeping, and the middle one is the largest kind of waste there is: a
        // whole KV snapshot that nothing can ever match again.
        let mut cache = PromptStateCache::new(8, usize::MAX);
        // Turn start — the prefix the NEXT turn shares. Must survive.
        cache.insert(key(PromptStateCacheLayer::HistoryPrefix, "start"), token_entry(&[1, 2]));
        // Mid-turn — carries this turn's tool call and result. Scratch.
        cache.insert(
            key(PromptStateCacheLayer::ExactPrompt, "scratch"),
            token_entry(&[1, 2, 3]).dying_with_turn(),
        );
        // Turn end — extends both.
        let report = cache
            .insert(key(PromptStateCacheLayer::F8ChatPrefix, "done"), token_entry(&[1, 2, 3, 4]));

        assert_eq!(report.pruned.len(), 1, "only the scratch should be reclaimed");
        assert!(
            !cache.contains(&key(PromptStateCacheLayer::ExactPrompt, "scratch")),
            "intra-turn scratch outlived the turn"
        );
        assert!(
            cache.contains(&key(PromptStateCacheLayer::HistoryPrefix, "start")),
            "turn-start prefix was reclaimed — the next turn has nothing to stand on"
        );
        assert!(cache.contains(&key(PromptStateCacheLayer::F8ChatPrefix, "done")));
    }

    #[test]
    fn an_unmarked_entry_of_another_layer_is_never_reclaimed() {
        // The cross-layer reach is granted by the entry, not taken by the
        // inserter: an ordinary checkpoint of a different layer stays put even
        // when it is a strict prefix. This is what keeps a network caller's
        // reusable prefix safe.
        let mut cache = PromptStateCache::new(8, usize::MAX);
        cache.insert(key(PromptStateCacheLayer::ExactPrompt, "keep"), token_entry(&[1, 2, 3]));
        let report = cache
            .insert(key(PromptStateCacheLayer::F8ChatPrefix, "done"), token_entry(&[1, 2, 3, 4]));
        assert!(report.pruned.is_empty());
        assert!(cache.contains(&key(PromptStateCacheLayer::ExactPrompt, "keep")));
    }

    #[test]
    fn prune_keeps_cache_flat_across_a_growing_conversation() {
        // Simulate the append-only F8 turn loop: each turn inserts a deeper
        // completed-turn checkpoint. With pruning the cache holds exactly one
        // frontier entry rather than growing one dead entry per turn.
        let mut cache = PromptStateCache::new(8, usize::MAX);
        let mut tokens = vec![1, 2];
        for turn in 0..6 {
            tokens.push(10 + turn);
            cache.insert(
                key(PromptStateCacheLayer::F8ChatPrefix, &format!("turn{turn}")),
                token_entry(&tokens),
            );
        }
        assert_eq!(cache.len(), 1);
    }

    /// The trap that cost a real turn 37.6 s of re-reading. A turn that calls a
    /// tool saves a completed-turn checkpoint covering the call and its result,
    /// but the next turn's history keeps only the spoken reply — so that
    /// checkpoint diverges and can never match. What the next turn *does* share
    /// is the prefix this turn read before generating; saving it under its own
    /// layer is what keeps it alive, because filed under the chat layer the
    /// completed-turn insert prunes it seconds later, in the same turn.
    #[test]
    fn the_prefix_a_turn_read_outlives_its_own_completed_turn_checkpoint() {
        let mut cache = PromptStateCache::new(8, usize::MAX);
        cache.insert_pinned(key(PromptStateCacheLayer::F8System, "head"), token_entry(&[1, 2]));
        // Start of the turn: system + tools + history, before anything is said.
        cache.insert(
            key(PromptStateCacheLayer::HistoryPrefix, "read"),
            token_entry(&[1, 2, 3, 4, 5]),
        );
        // End of the same turn: also covers the tool call and the tool result,
        // neither of which the next turn will ever see.
        let report = cache.insert(
            key(PromptStateCacheLayer::F8ChatPrefix, "completed"),
            token_entry(&[1, 2, 3, 4, 5, 90, 91, 92]),
        );
        assert!(report.pruned.is_empty(), "the prefix was pruned inside its own turn");

        // Next turn: history gained the spoken reply (6), never the tool
        // traffic, so the completed-turn checkpoint diverges at token 90.
        let layers = [
            PromptStateCacheLayer::F8ChatPrefix,
            PromptStateCacheLayer::HistoryPrefix,
            PromptStateCacheLayer::F8System,
        ];
        assert_eq!(
            cache.find_longest_prefix("runtime", &layers, &[1, 2, 3, 4, 5, 6, 7]),
            Some(key(PromptStateCacheLayer::HistoryPrefix, "read")),
            "fell back to the 2-token pin and re-read everything else"
        );
    }

    /// The RAM the tier hands back. With nowhere to put what it drops, memory
    /// has to hold the predecessors a longest-prefix match needs; with disk
    /// attached it only has to hold the conversation being served, and the rest
    /// of the reservation goes back to the machine (ADR 0042).
    #[test]
    fn attaching_disk_shrinks_the_memory_reservation() {
        let checkpoint = 2 * 1024 * 1024 * 1024_u64; // 2 GB, so the floor cannot bind
        let free = 64 * 1024 * 1024 * 1024_u64;

        let mut memory_only = PromptStateCache::new(8, usize::MAX);
        let without = memory_only.resize_for_checkpoint(checkpoint, free);

        let dir = disk_dir("shrink");
        let mut with_disk = PromptStateCache::new(8, usize::MAX);
        let store =
            crate::prompt_cache_disk::CheckpointStore::open(dir.clone(), 8 * checkpoint).unwrap();
        with_disk.attach_disk(store, "runtime");
        let with = with_disk.resize_for_checkpoint(checkpoint, free);

        assert_eq!(without as u64, 3 * checkpoint);
        assert_eq!(with as u64, checkpoint, "disk attached, so memory keeps one");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A restart throwing everything away was the strongest single argument for
    /// the tier, and eviction cannot answer it: the entry being served is never
    /// dropped. A clean exit has to write it.
    #[test]
    fn a_clean_exit_keeps_what_memory_still_holds() {
        let dir = disk_dir("exit");
        let store = crate::prompt_cache_disk::CheckpointStore::open(dir.clone(), 1 << 30).unwrap();
        {
            let mut cache = PromptStateCache::new(8, usize::MAX);
            cache.attach_disk(store, "runtime");
            let (k, e) = tokened("live", &[1, 2, 3], 64);
            cache.insert(k, e);
            assert_eq!(cache.disk().unwrap().usage().1, 0, "nothing written while it is held");
        }
        let reopened =
            crate::prompt_cache_disk::CheckpointStore::open(dir.clone(), 1 << 30).unwrap();
        assert_eq!(reopened.usage().1, 1, "the conversation in progress survived the exit");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
