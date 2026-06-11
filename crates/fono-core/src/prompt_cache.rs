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

use std::collections::{HashMap, HashSet, VecDeque};

/// Logical role of a cached prefix. The layer is part of the cache key, so two
/// prefixes with identical text but different roles never collide, and it
/// drives pinning (`is_pinnable`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PromptStateCacheLayer {
    /// F7 transcription cleanup base prompt (config: main + advanced +
    /// dictionary). Context-independent, pinned.
    F7System,
    /// F8 assistant system prompt. Context-independent, pinned.
    F8System,
    /// F8 assistant tool/function prompt. Context-independent, pinned.
    AssistantTools,
    /// F7 cleanup base + the focused app's `rule_suffix` (CLI / editor /
    /// browser / terminal-agent). Per-context layer, LRU among contexts.
    F7Context,
    /// Deprecated assistant window-context layer (assistant no longer injects
    /// window context). Retained for key stability / migration.
    WindowContext,
    /// F8 chat prefix (system + tools + history), used by the live reply path.
    F8ChatPrefix,
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
            Self::AssistantTools => "assistant_tools",
            Self::F7Context => "f7_context",
            Self::WindowContext => "window_context",
            Self::F8ChatPrefix => "f8_chat_prefix",
            Self::BenchmarkPrefix => "benchmark_prefix",
            Self::ExactPrompt => "exact_prompt",
        }
    }

    /// Context-independent base prefixes that are reused on every turn of every
    /// conversation. These are prewarmed at startup and must never be evicted
    /// by LRU churn. All other layers age out normally.
    pub fn is_pinnable(&self) -> bool {
        matches!(self, Self::F7System | Self::F8System | Self::AssistantTools)
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
}

impl PromptStateCacheEntry {
    pub fn new(state: Vec<u8>, token_count: usize) -> Self {
        Self { state, token_count, prefix_tokens: Vec::new() }
    }

    /// Build an entry that records its token sequence so it can be found by
    /// longest-prefix matching. `token_count` is derived from `prefix_tokens`.
    pub fn with_tokens(state: Vec<u8>, prefix_tokens: Vec<i32>) -> Self {
        Self { state, token_count: prefix_tokens.len(), prefix_tokens }
    }
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
        Self::new(8, 256 * 1024 * 1024)
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

    /// Total bytes currently held across all entries.
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
    /// `layer` and `runtime_sha256`, with recorded `prefix_tokens` that are a
    /// *strict* prefix of `key`'s tokens. Such entries can never beat `key` in
    /// a [`Self::find_longest_prefix`] match, so retaining them only wastes a
    /// slot and a (multi-MB) state blob. No-op when the new entry records no
    /// tokens (exact-key-only entries don't participate in prefix matching).
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
                    && k.layer == key.layer
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
        let entry = self.entries.get(key).cloned()?;
        self.lru.retain(|existing| existing != key);
        self.lru.push_back(key.clone());
        Some(entry)
    }

    pub fn contains(&mut self, key: &PromptStateCacheKey) -> bool {
        self.get(key).is_some()
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

    fn evict_over_budget(&mut self) -> Vec<EvictedEntry> {
        let mut evicted = Vec::new();
        while self.entries.len() > self.max_entries || self.bytes > self.max_bytes {
            // Evict the oldest entry that is not pinned. Pinned base prefixes
            // are skipped; if only pinned entries remain we stop rather than
            // drop a protected checkpoint.
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
}
