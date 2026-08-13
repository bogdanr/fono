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
            "{:?}:runtime={}:prompt={}:tokens={}:count={}",
            self.layer,
            self.runtime_sha256,
            self.prompt_sha256,
            self.token_sha256,
            self.token_count
        )
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
}

impl Default for PromptStateCache {
    fn default() -> Self {
        Self::new(10, 256 * 1024 * 1024)
    }
}

impl PromptStateCache {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            max_entries,
            max_bytes,
            bytes: 0,
            entries: HashMap::new(),
            lru: VecDeque::new(),
            pinned: HashSet::new(),
        }
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
                evicted.push(EvictedEntry {
                    layer: key.layer.clone(),
                    token_count: entry.token_count,
                    bytes: entry.state.len(),
                });
            }
        }
        evicted
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
}
