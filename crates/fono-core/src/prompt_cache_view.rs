// SPDX-License-Identifier: GPL-3.0-only
//! Topology view over a [`PromptStateCache`], for diagnostics.
//!
//! The cache stores a *forest* in flattened form: the pinned base prefixes are
//! roots, and every other entry hangs off the longest cached entry whose token
//! sequence is a prefix of its own. Nothing in the cache records those edges,
//! because it never needs them — `find_longest_prefix` recomputes the one edge
//! it cares about on demand. This module recovers the whole shape so it can be
//! shown, and derives the handful of facts that say whether the shape is any
//! good.
//!
//! Everything here is read-only and allocation-cheap: it consumes
//! [`PromptStateCache::nodes`], which borrows metadata and copies no KV blob,
//! and it never touches eviction order.
//!
//! One thing the shape will *not* show is a chain of ancestors between a root
//! and a head. Inserting an entry prunes every non-pinned entry in the same
//! layer whose tokens it strictly extends, so what survives is the frontier —
//! roots, plus the deepest checkpoint of each live branch.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::prompt_cache::{CacheCountersSnapshot, CacheNodeView, PromptStateCache};

/// One entry, placed in the tree.
#[derive(Debug, Clone, Serialize)]
pub struct CacheNode {
    pub id: String,
    pub layer: String,
    /// Absolute token count of the cached prefix.
    pub tokens: usize,
    /// Tokens this node adds beyond its parent. Equals `tokens` at a root.
    pub delta_tokens: usize,
    pub bytes: u64,
    pub pinned: bool,
    /// Rank among the entries of this cache, 0 = least recently used and
    /// `nodes.len() + unplaced.len() - 1` = most. Dense and gap-free, unlike the
    /// cache's own queue position, so a client can scale it straight onto a
    /// colour ramp without first having to work out the range.
    pub lru_rank: usize,
    pub idle_secs: u64,
    pub parent: Option<String>,
    pub depth: usize,
    /// How many entries have to go before this one does: 1 means it is next.
    /// `None` for pinned entries, which eviction refuses to touch.
    pub evicts_in: Option<usize>,
    /// An abridged copy of the prompt behind this entry, when the backend
    /// recorded one. The alternative handle on an entry is the hash of that
    /// prompt, which tells a reader nothing.
    pub preview: Option<String>,
}

/// Whether the cache is holding a useful shape, as data rather than prose.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CacheVerdicts {
    /// Entries with no cached ancestor.
    pub roots: usize,
    /// Entries nothing descends from — the live branch tips.
    pub heads: usize,
    pub max_depth: usize,
    /// Entries that recorded no tokens, so prefix matching cannot see them and
    /// they are reachable by exact key only.
    pub orphans: usize,
    /// Pinned entries nothing descends from. A pin is held precisely so
    /// branches can grow off it, so one with no children is a slot and a blob
    /// spent on nothing.
    pub stranded_pins: usize,
    /// At least one branch does not descend from a pinned base, so it pays a
    /// cold prefill however warm the pins are.
    pub fragmented: bool,
    /// More live branches than the cache has evictable slots, so they are now
    /// guaranteed to evict each other.
    pub heads_over_slots: bool,
    /// Bytes held more than once because each entry is a standalone snapshot
    /// that contains its ancestor's KV state. This is the price of being able
    /// to evict any entry without invalidating another.
    pub duplication_bytes: u64,
    /// No turn has consulted the cache yet, so the shape above reflects startup
    /// prewarm only and should not be judged.
    pub warming: bool,
}

/// Everything the UI needs about one cache instance. Carries no KV state.
#[derive(Debug, Clone, Serialize)]
pub struct CacheSnapshot {
    /// Which cache this is: `assistant` or `polish`.
    pub role: String,
    /// Human label for the model behind it.
    pub model: String,
    /// Short form of the runtime fingerprint the keys are scoped to.
    pub runtime: String,
    pub max_entries: usize,
    pub max_bytes: u64,
    pub entries_pinned: usize,
    pub entries_evictable: usize,
    pub entries_free: usize,
    pub bytes_pinned: u64,
    pub bytes_evictable: u64,
    pub bytes_free: u64,
    /// Real memory held, pins included. Both budgets deliberately measure only
    /// what eviction can reclaim, so this can exceed `max_bytes` without the
    /// cache being over budget.
    pub bytes_resident: u64,
    pub nodes: Vec<CacheNode>,
    pub unplaced: Vec<CacheNode>,
    pub verdicts: CacheVerdicts,
    pub counters: CacheCountersSnapshot,
}

/// Intermediate: a placed entry plus the index of its parent.
struct Placed {
    node: CacheNode,
    parent_idx: Option<usize>,
    children: usize,
}

/// Read a cache and describe its shape. `role`, `model` and `counters` are
/// supplied because the cache itself is deliberately ignorant of all three.
pub fn snapshot(
    role: &str,
    model: &str,
    cache: &PromptStateCache,
    counters: CacheCountersSnapshot,
) -> CacheSnapshot {
    // `nodes` hands these back coldest-first, so the position in that order is
    // the dense recency rank. Deriving it here rather than forwarding the queue
    // index keeps two problems out of the client: the index is sparse if the
    // queue and the entry map ever disagree, and its miss value is `usize::MAX`,
    // which would arrive as a 20-digit number and read as the freshest entry in
    // the cache.
    let views = cache.nodes();
    let dense_rank: BTreeMap<&str, usize> =
        views.iter().enumerate().map(|(i, v)| (v.id.as_str(), i)).collect();
    let runtime = views.first().map(|v| short_runtime(v.runtime_sha256)).unwrap_or_default();

    // Eviction order among the entries eviction is allowed to touch. The LRU
    // queue includes pins, so their ranks have to be skipped rather than
    // counted, or every ordinal after a pin would be wrong.
    let ordinal: BTreeMap<&str, usize> = views
        .iter()
        .filter(|v| !v.pinned)
        .enumerate()
        .map(|(i, v)| (v.id.as_str(), i + 1))
        .collect();

    let mut placed: Vec<Placed> = Vec::new();
    let mut unplaced: Vec<CacheNode> = Vec::new();
    for view in &views {
        let node = CacheNode {
            id: view.id.clone(),
            layer: view.layer.as_str().to_string(),
            tokens: view.token_count,
            delta_tokens: view.token_count,
            bytes: view.bytes as u64,
            pinned: view.pinned,
            lru_rank: dense_rank.get(view.id.as_str()).copied().unwrap_or(0),
            idle_secs: view.idle.as_secs(),
            parent: None,
            depth: 0,
            evicts_in: ordinal.get(view.id.as_str()).copied(),
            preview: view.preview.map(ToString::to_string),
        };
        if view.prefix_tokens.is_empty() {
            unplaced.push(node);
        } else {
            placed.push(Placed { node, parent_idx: None, children: 0 });
        }
    }

    let duplication_bytes = link_parents(&mut placed, &views);

    let roots = placed.iter().filter(|p| p.parent_idx.is_none()).count();
    let heads = placed.iter().filter(|p| p.children == 0).count();
    let max_depth = placed.iter().map(|p| p.node.depth).max().unwrap_or(0);
    let stranded_pins = placed.iter().filter(|p| p.node.pinned && p.children == 0).count();
    let fragmented = placed.iter().any(|p| p.parent_idx.is_none() && !p.node.pinned);

    let (entries_evictable, bytes_evictable) = cache.evictable();
    let entries_pinned = cache.pinned_len();
    let bytes_resident = cache.bytes() as u64;
    let bytes_pinned = bytes_resident.saturating_sub(bytes_evictable as u64);
    let max_bytes = cache.max_bytes() as u64;
    let max_entries = cache.max_entries();

    // Sort for display: roots first, then depth-first through children, so the
    // client can render the tree by indenting on `depth` alone.
    let mut nodes: Vec<CacheNode> = placed.into_iter().map(|p| p.node).collect();
    nodes = depth_first(nodes);

    CacheSnapshot {
        role: role.to_string(),
        model: model.to_string(),
        runtime,
        max_entries,
        max_bytes,
        entries_pinned,
        entries_evictable,
        entries_free: max_entries.saturating_sub(entries_evictable),
        bytes_pinned,
        bytes_evictable: bytes_evictable as u64,
        bytes_free: max_bytes.saturating_sub(bytes_evictable as u64),
        bytes_resident,
        verdicts: CacheVerdicts {
            roots,
            heads,
            max_depth,
            orphans: unplaced.len(),
            stranded_pins,
            fragmented,
            heads_over_slots: heads >= max_entries,
            duplication_bytes,
            warming: counters.warming(),
        },
        nodes,
        unplaced,
        counters,
    }
}

/// Recover the forest by finding, for every entry, the longest other entry that
/// is a strict token prefix of it. Fills in each node's parent, depth and delta,
/// and returns the bytes duplicated across the edges — the cost of storing whole
/// snapshots rather than deltas.
fn link_parents(placed: &mut [Placed], views: &[CacheNodeView<'_>]) -> u64 {
    let by_id: BTreeMap<&str, (&[i32], &str)> =
        views.iter().map(|v| (v.id.as_str(), (v.prefix_tokens, v.runtime_sha256))).collect();
    let facts: Vec<(&[i32], &str)> = placed
        .iter()
        .map(|p| by_id.get(p.node.id.as_str()).copied().unwrap_or((&[], "")))
        .collect();

    // Shortest first, so a node's parent is always already resolved by the time
    // we reach it and depth needs no second pass.
    let mut order: Vec<usize> = (0..placed.len()).collect();
    order.sort_by_key(|&i| facts[i].0.len());

    for (position, &i) in order.iter().enumerate() {
        let (mine, my_runtime) = facts[i];
        let mut best: Option<usize> = None;
        for &j in &order[..position] {
            let (theirs, their_runtime) = facts[j];
            if their_runtime != my_runtime || theirs.len() >= mine.len() {
                continue;
            }
            if !mine.starts_with(theirs) {
                continue;
            }
            // Deepest wins — that is the entry `find_longest_prefix` would pick,
            // so the drawn edge is the reuse path the cache would really take.
            // Ties break towards a pin, then towards the fresher entry, because
            // iteration order over the cache's map is otherwise arbitrary.
            let better = best.is_none_or(|b| {
                (theirs.len(), placed[j].node.pinned, placed[b].node.lru_rank)
                    > (facts[b].0.len(), placed[b].node.pinned, placed[j].node.lru_rank)
            });
            if better {
                best = Some(j);
            }
        }
        if let Some(parent) = best {
            placed[i].parent_idx = Some(parent);
            placed[i].node.parent = Some(placed[parent].node.id.clone());
            placed[i].node.depth = placed[parent].node.depth + 1;
            placed[i].node.delta_tokens = mine.len().saturating_sub(facts[parent].0.len());
            placed[parent].children += 1;
        }
    }

    let mut duplication_bytes = 0_u64;
    for entry in placed.iter() {
        if let Some(parent) = entry.parent_idx {
            duplication_bytes =
                duplication_bytes.saturating_add(placed[parent].node.bytes.min(entry.node.bytes));
        }
    }
    duplication_bytes
}

/// Order nodes so each parent is immediately followed by its subtree. Lets the
/// client draw the tree from `depth` without building its own index.
fn depth_first(nodes: Vec<CacheNode>) -> Vec<CacheNode> {
    let mut children: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (i, node) in nodes.iter().enumerate() {
        match &node.parent {
            Some(parent) => children.entry(parent.clone()).or_default().push(i),
            None => roots.push(i),
        }
    }
    // Freshest branch first at every level, so the entry just used is at the top
    // and the one about to be evicted at the bottom.
    let by_recency = |a: &usize, b: &usize| nodes[*b].lru_rank.cmp(&nodes[*a].lru_rank);
    roots.sort_by(by_recency);
    for kids in children.values_mut() {
        kids.sort_by(by_recency);
    }

    let mut out = Vec::with_capacity(nodes.len());
    let mut stack: Vec<usize> = roots.into_iter().rev().collect();
    let mut seen = vec![false; nodes.len()];
    while let Some(i) = stack.pop() {
        if seen[i] {
            continue;
        }
        seen[i] = true;
        out.push(nodes[i].clone());
        if let Some(kids) = children.get(&nodes[i].id) {
            for &k in kids.iter().rev() {
                stack.push(k);
            }
        }
    }
    // Anything unreachable (a parent id that is somehow absent) still ships, so
    // the panel can never silently drop an entry.
    for (i, node) in nodes.iter().enumerate() {
        if !seen[i] {
            out.push(node.clone());
        }
    }
    out
}

fn short_runtime(full: &str) -> String {
    full.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt_cache::{
        PromptStateCacheEntry, PromptStateCacheKey, PromptStateCacheLayer as L,
    };

    fn key(layer: L, id: &str, tokens: usize) -> PromptStateCacheKey {
        PromptStateCacheKey::new(layer, "runtimefingerprint", id, id, tokens)
    }

    /// An entry whose token sequence is `0, 1, 2, …, len-1`, so any shorter one
    /// built the same way is a genuine prefix of it.
    fn chain(len: usize) -> PromptStateCacheEntry {
        PromptStateCacheEntry::with_tokens(vec![0_u8; 1024], (0..len as i32).collect())
    }

    fn shot(cache: &PromptStateCache) -> CacheSnapshot {
        snapshot("assistant", "test-model", cache, CacheCountersSnapshot::default())
    }

    fn node<'a>(s: &'a CacheSnapshot, layer: &str) -> &'a CacheNode {
        s.nodes.iter().find(|n| n.layer == layer).expect("node present")
    }

    /// The base prompt is the root and the chat prefix that extends it hangs off
    /// it, carrying only the tokens it added.
    #[test]
    fn a_longer_prefix_hangs_off_the_base_it_extends() {
        let mut cache = PromptStateCache::new(10, usize::MAX);
        cache.insert_pinned(key(L::F8System, "sys", 10), chain(10));
        cache.insert(key(L::F8ChatPrefix, "chat", 30), chain(30));

        let s = shot(&cache);
        let base = node(&s, "f8_system");
        let chat = node(&s, "f8_chat_prefix");
        assert!(base.parent.is_none(), "the pinned base is a root");
        assert_eq!(chat.parent.as_deref(), Some(base.id.as_str()));
        assert_eq!(chat.depth, 1);
        assert_eq!(chat.delta_tokens, 20, "a child reports what it added, not its total");
        assert_eq!(s.verdicts.roots, 1);
        assert_eq!(s.verdicts.heads, 1);
        assert!(!s.verdicts.fragmented);
    }

    /// Two conversations that diverge after a shared base stay two branches off
    /// one root — the cache keeps diverging siblings on purpose.
    #[test]
    fn diverging_conversations_are_two_branches_of_one_root() {
        let mut cache = PromptStateCache::new(10, usize::MAX);
        cache.insert_pinned(key(L::F8System, "sys", 10), chain(10));
        let mut left = chain(20);
        left.prefix_tokens.push(900);
        let mut right = chain(20);
        right.prefix_tokens.push(901);
        cache.insert(key(L::F8ChatPrefix, "left", 21), left);
        cache.insert(key(L::HistoryPrefix, "right", 21), right);

        let s = shot(&cache);
        let root = node(&s, "f8_system");
        assert_eq!(s.verdicts.roots, 1);
        assert_eq!(s.verdicts.heads, 2, "both conversations survive");
        for layer in ["f8_chat_prefix", "history_prefix"] {
            assert_eq!(node(&s, layer).parent.as_deref(), Some(root.id.as_str()));
        }
    }

    /// A branch that shares no cached ancestor is its own root, and that is the
    /// thing worth telling the user about: it pays a full prefill however warm
    /// the pins are.
    #[test]
    fn an_unrelated_branch_reads_as_fragmented() {
        let mut cache = PromptStateCache::new(10, usize::MAX);
        cache.insert_pinned(key(L::F8System, "sys", 10), chain(10));
        cache.insert(
            key(L::ExactPrompt, "other", 3),
            PromptStateCacheEntry::with_tokens(vec![0; 8], vec![777, 778, 779]),
        );

        let s = shot(&cache);
        assert_eq!(s.verdicts.roots, 2);
        assert!(s.verdicts.fragmented);
    }

    /// A pin with nothing growing off it is a slot and a blob spent on nothing.
    #[test]
    fn a_pin_with_no_children_is_stranded() {
        let mut cache = PromptStateCache::new(10, usize::MAX);
        cache.insert_pinned(key(L::F7System, "polish", 10), chain(10));

        let s = shot(&cache);
        assert_eq!(s.verdicts.stranded_pins, 1);
        assert_eq!(s.entries_pinned, 1);
    }

    /// Entries that recorded no tokens cannot be placed in the tree at all —
    /// prefix matching is blind to them — so they are reported separately
    /// rather than silently dropped.
    #[test]
    fn tokenless_entries_are_reported_as_unplaced() {
        let mut cache = PromptStateCache::new(10, usize::MAX);
        cache.insert(key(L::ExactPrompt, "exact", 5), PromptStateCacheEntry::new(vec![0; 16], 5));

        let s = shot(&cache);
        assert!(s.nodes.is_empty());
        assert_eq!(s.unplaced.len(), 1);
        assert_eq!(s.verdicts.orphans, 1);
    }

    /// Eviction order is the panel's answer to "what is about to be lost", so
    /// the ordinals must count only what eviction can take: pins carry none,
    /// and numbering does not skip over them.
    #[test]
    fn eviction_ordinals_count_only_evictable_entries() {
        let mut cache = PromptStateCache::new(10, usize::MAX);
        cache.insert(key(L::ExactPrompt, "oldest", 4), chain(4));
        cache.insert_pinned(key(L::F8System, "sys", 6), chain(6));
        cache.insert(key(L::F8ChatPrefix, "newest", 8), chain(8));

        let s = shot(&cache);
        let all: Vec<&CacheNode> = s.nodes.iter().chain(s.unplaced.iter()).collect();
        let find = |layer: &str| *all.iter().find(|n| n.layer == layer).expect("present");
        assert_eq!(find("f8_system").evicts_in, None, "a pin is never in line");
        assert_eq!(find("exact_prompt").evicts_in, Some(1), "least recently used goes first");
        assert_eq!(find("f8_chat_prefix").evicts_in, Some(2));
    }

    /// Reading the shape must not disturb what the cache would evict next.
    /// `nodes` takes `&self` and `peek` exists precisely so a diagnostic cannot
    /// reorder the queue by looking at it.
    #[test]
    fn reading_the_shape_does_not_reorder_eviction() {
        // Unrelated token sequences, so nothing is pruned for extending
        // anything else and eviction order alone decides who survives.
        let lone = |t: i32| PromptStateCacheEntry::with_tokens(vec![0_u8; 64], vec![t]);
        let build = || {
            let mut cache = PromptStateCache::new(2, usize::MAX);
            cache.insert(key(L::ExactPrompt, "a", 1), lone(1));
            cache.insert(key(L::ExactPrompt, "b", 1), lone(2));
            cache
        };

        let mut untouched = build();
        let mut inspected = build();
        let _ = shot(&inspected);
        assert!(inspected.peek(&key(L::ExactPrompt, "a", 1)), "peek must not evict either");

        untouched.insert(key(L::ExactPrompt, "c", 1), lone(3));
        inspected.insert(key(L::ExactPrompt, "c", 1), lone(3));
        for id in ["a", "b", "c"] {
            let k = key(L::ExactPrompt, id, 1);
            assert_eq!(
                untouched.peek(&k),
                inspected.peek(&k),
                "{id} survived differently after being inspected"
            );
        }
        assert!(!inspected.peek(&key(L::ExactPrompt, "a", 1)), "the oldest still goes first");
    }
}
