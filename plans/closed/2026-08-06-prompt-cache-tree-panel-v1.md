# Prompt-Cache Tree Panel in Web Settings

## Status: Completed (all 15 tasks landed 2026-08-06)

## Objective

Make the live prompt-state (KV) cache visible in the web settings UI as a
branch tree, so that at a glance you can tell:

- **what was last used** — and how long ago, not merely in what order;
- **what is free** — how many slots and bytes remain before the next eviction,
  and which entry dies first;
- **whether the cache is the right shape** — do branches share a pinned root,
  are any pins stranded, are there more conversation heads than slots.

The panel is a *diagnostic*, read-only. It changes no caching behaviour. Its
purpose is to produce the evidence on which the next round of cache work
(pin policy, budgets, per-origin reservation) will be decided, rather than
guessing.

Scope is deliberately pull-based: fetch on view entry plus an explicit
refresh. Server-push (SSE) and any policy change are out of scope here.

---

## Assessment

### What already exists

`crates/fono-core/src/prompt_cache.rs` already holds everything the tree needs
except timestamps:

- Entries store their **exact token sequence** in `prefix_tokens`
  (`crates/fono-core/src/prompt_cache.rs:159-176`), so every parent/child
  prefix relation is derivable. The comparison is already performed twice in
  the file — `prune_dominated_by`
  (`crates/fono-core/src/prompt_cache.rs:282-315`) and `find_longest_prefix`
  (`crates/fono-core/src/prompt_cache.rs:360-377`).
- Occupancy is already public and already `&self`: `bytes()`, `len()`,
  `pinned_len()` (`crates/fono-core/src/prompt_cache.rs:242-258`).
- Pinned membership is queryable (`crates/fono-core/src/prompt_cache.rs:348`),
  and eviction order is the `lru` deque
  (`crates/fono-core/src/prompt_cache.rs:217`).
- Mutations already report churn as `CacheMutationReport`
  (`crates/fono-core/src/prompt_cache.rs:193-207`).

So the cache is **already a forest, stored flattened**. Roots are the pinned
base prefixes; heads are the frontier entries `prune_dominated_by` keeps.
Sibling branches are deliberately preserved — regression test at
`crates/fono-core/src/prompt_cache.rs:622-633`.

On the delivery side, the web settings server needs no new wire machinery: the
doctor hook is a zero-argument closure returning JSON
(`crates/fono-net/src/web_settings/mod.rs:106-110`), and
`web_settings_hooks` **already receives** `Option<&Arc<SessionOrchestrator>>`
(`crates/fono/src/daemon.rs:4082-4089`), which sibling hooks already clone.

### The three real gaps

1. **No timestamps.** Recency is ordinal only — a position in a `VecDeque`.
   "Last used 30s ago" is unavailable without a field on the entry. Since
   recency is half of what the panel exists to show, add `last_used: Instant`
   and touch it in `get`. Sixteen bytes on entries that already hold
   megabytes.

2. **No read-only introspection.** `get` clones the whole multi-MB `state`
   blob, and both `get` and `contains` bump the LRU
   (`crates/fono-core/src/prompt_cache.rs:337-346`). Neither can be used by a
   panel that refreshes — a diagnostic that reorders eviction is worse than no
   diagnostic. A `&self` metadata iterator is required.

3. **No cross-turn aggregation.** Hit/miss facts exist only inside opt-in
   per-turn trace JSON (`crates/fono-core/src/turn_trace.rs:389-410`), keyed
   by a process-local counter, with nothing accumulated. Process-lifetime
   atomics are needed for a hit rate to exist at all.

### Two facts the panel will immediately expose

Both are known from the code, so they are expectations, not discoveries:

- **LLM-server turns never warm.** `pin_prefix` requires empty history
  (`crates/fono-assistant/src/llama_local.rs:2485`) and API clients resend
  theirs from turn 2; the code says so outright
  (`crates/fono-assistant/src/llama_local.rs:2475-2476`).
- **Slots bind before bytes.** Observed live occupancy has been 4–5 entries at
  13–15 MB against a 256 MiB budget; the entry cap is what binds
  (`crates/fono-core/src/prompt_cache.rs:223`). The panel should therefore
  make the *entry* bar at least as prominent as the byte bar.

### Non-goals

- No SSE / live push. Pull on view entry plus refresh.
- No caching-policy change: pin rules, budgets and eviction stay exactly as
  they are. This plan only observes.
- No CLI tree. Terminal output gets a one-row summary so the doctor health
  icon can reflect thrash; ASCII art is not a goal.
- No origin tagging (local voice vs API vs prewarm). The row layout reserves
  the slot; the backend change belongs to the follow-up round.

---

## Design

### Data model

A serde-serialisable `CacheSnapshot`, built daemon-side, carrying no KV blobs:

- **identity** — role (assistant / polish), model label, short runtime hash.
- **budgets** — `max_entries`, `max_bytes`.
- **occupancy** — entries and bytes each split pinned / evictable / free, plus
  resident bytes. Resident is reported separately because both budgets measure
  *evictable* totals only, by deliberate design
  (`crates/fono-core/src/prompt_cache.rs:390-401`), so resident can exceed the
  byte budget legitimately.
- **nodes** — per entry: stable id, layer, absolute token count, delta tokens
  against its parent, bytes, pinned flag, LRU rank, last-used age in seconds,
  parent id, depth, eviction ordinal (`None` for pins).
- **unplaced** — entries with empty `prefix_tokens`; they cannot join the tree
  because prefix matching skips them
  (`crates/fono-core/src/prompt_cache.rs:371`).
- **verdicts** — the shape assessment below, as data rather than prose.
- **counters** — lifetime restores, cold prefills bucketed by reason,
  evictions, prunes, pin releases.

### Parent-edge derivation

Sort nodes by token count ascending, then for each node choose the longest
strictly-shorter node whose `prefix_tokens` is a prefix of its own, restricted
to the same runtime. That is the same rule `find_longest_prefix` uses, so the
rendered edge is exactly the reuse path the cache would actually take. Nodes
with no such ancestor are roots.

Ties are broken deterministically — prefer a pinned candidate, then the lower
LRU rank — because iteration over a `HashMap` is otherwise unordered.

### Shape verdicts

Each one names a real pathology:

| Verdict | Meaning |
|---|---|
| `rooted` / `fragmented` | do all heads descend from a pinned root? Multiple disjoint roots on one runtime means prompts diverge at token 0 and nothing is shared. |
| `stranded_pins` | a pinned node with no descendants — a wasted slot holding wasted memory. |
| `heads_over_slots` | branch heads at or above the evictable cap: mutual eviction is now guaranteed. |
| `orphans` | count of tokenless entries, dead weight to prefix matching. |
| `max_depth` | expected 2–4 (system → tools → chat), since `prune_dominated_by` collapses same-layer spines by design. Deeper is a signal. |
| `duplication` | total node bytes against the bytes that would be needed if shared prefixes were stored once — quantifies the snapshots-not-chains trade-off (`crates/fono-core/src/prompt_cache.rs:12-21`). |

Verdicts are suppressed while the cache is still warming (before the first
completed turn), so startup prewarm does not raise false alarms.

### Panel layout

```
┌ Prompt cache ─────────────────── [assistant] [polish] ─── refresh ┐
│  gemma-4-e2b · runtime a1b2c3d4                                   │
│                                                                   │
│  entries   pinned 3 | evictable 6 | free 1            9 / 10 + 3  │
│  bytes     pinned 63.5 MiB | evictable 148.2 MiB | free 107.8 MiB │
│                                              budget 256.0 MiB     │
│                                                                   │
│  rooted   4 heads   depth 3   0 orphans   0 stranded pins         │
│                                                                   │
│  [lock] f8_system              1512 tok   38.1 MiB  [====]        │
│    |- [lock] assistant_tools    +284 tok   12.4 MiB  [==]         │
│    |    |- f8_chat_prefix       +748 tok   31.2 MiB  [===]  30s   │
│    |    \- history_prefix       +592 tok   24.8 MiB  [==]    2m   │
│    \- f8_chat_prefix            +390 tok   18.0 MiB  [==]   11m   │
│  [lock] f7_system               412 tok    9.8 MiB  [=]           │
│    \- f7_context                +118 tok    3.1 MiB  []      5m   │
│                                                                   │
│  > unplaced — exact-key only, invisible to prefix matching (1)     │
└───────────────────────────────────────────────────────────────────┘
```

Encoding rules:

- **Recency rail** — a narrow left edge per row on a warm-to-cold ramp,
  freshest at accent, oldest dim. Always paired with a relative age string;
  colour never carries meaning alone.
- **Byte bar** — inline, width proportional to the largest node. This is what
  makes "which branch is eating the budget" instant.
- **Delta tokens on children**, absolute on roots. The deltas *are* the branch
  structure; absolutes obscure it.
- **Pinned rows** carry a lock glyph and a distinct border, and no eviction
  ordinal — they sit outside the budget by design.
- **Eviction ordinals** on the LRU tail, with the next-to-go row given a
  warning rail. "What is free" in practice means "how many entries until my
  branch dies".
- **Two caches, two tabs.** Assistant and polish are separate instances
  (`crates/fono-assistant/src/llama_local.rs:156`,
  `crates/fono-polish/src/llama_local.rs:94`); merging them would be a lie.
- **Footer note** that interior ancestors are pruned by design, so the
  frontier-only shape does not read as missing data.

Interaction: hover for the full key and exact figures; re-fetch on view entry
(mirroring the actions view, not the cached doctor view); explicit refresh
button; collapsed unplaced bucket.

---

## Implementation Plan

- [x] Task 1. Add a `last_used` timestamp to `PromptStateCacheEntry` and update
      it on every `get`. Rationale: the cache has ordinal recency only, so
      "what was last used" is otherwise unanswerable.
- [x] Task 2. Add a `&self` introspection method yielding per-entry layer,
      token count, bytes, pinned flag, LRU rank, last-used age and a borrowed
      token slice — cloning neither `state` nor touching the LRU. Rationale:
      `get` and `contains` both bump the LRU and deep-copy multi-MB blobs, so
      neither is usable by a panel that refreshes.
- [x] Task 3. Add a `&self` peek variant of `contains` and document the LRU
      side effect on the existing one, so future introspection cannot silently
      reorder eviction.
- [x] Task 4. Add the serde-serialisable `CacheSnapshot` type: identity,
      budgets, occupancy split pinned/evictable/free for entries and bytes,
      resident bytes, node list, unplaced bucket, verdicts, counters.
- [x] Task 5. Derive parent edges by longest-`starts_with` in a single pass
      over length-sorted nodes, with a deterministic tie-break, and assign
      depth and eviction ordinals.
- [x] Task 6. Compute the shape verdicts as data, suppressed while warming.
- [x] Task 7. Add process-lifetime atomic counters for restores, cold prefills
      bucketed by reason, evictions, prunes and pin releases, incremented at
      the existing trace call sites in both backends. Rationale: these facts
      live only in opt-in per-turn trace JSON today, with no aggregation.
- [x] Task 8. Add a snapshot accessor on `SessionOrchestrator` covering both
      the assistant and polish caches, returning nothing for non-local
      backends, built on the existing `Arc`-cloning accessor pattern so it
      cannot block the recording path.
- [x] Task 9. Add a `GET /api/promptcache` hook via the established five-step
      closure pattern, capturing the orchestrator `web_settings_hooks` already
      receives. Extend the exhaustive web-settings test stub or the build
      breaks.
- [x] Task 10. Build the panel in `app.js`: occupancy bars, verdict chips, the
      tree, the unplaced bucket, hover detail, role tabs, refresh.
- [x] Task 11. Add the panel CSS: recency ramp, byte bars, pinned and doomed
      row treatments, indent guides. Reuse existing severity, chip and meter
      classes; add no chart dependency.
- [x] Task 12. Add empty and degraded states: no local backend, cache empty
      before prewarm, daemon unreachable.
- [x] Task 13. Add a one-row cache summary to the doctor report — occupancy
      and worst verdict, no tree — so the header health icon reflects thrash,
      degrading to an informational row without an orchestrator.
- [x] Task 14. Add unit tests for edge derivation and verdicts: sibling
      branches under one root, disjoint roots flagged fragmented, stranded pin
      detected, tokenless entries in the unplaced bucket, frontier-only shape
      after pruning, eviction ordinals skipping pins.
- [x] Task 15. Add a test proving eviction order is byte-identical with and
      without an interleaved snapshot call.

## Verification Criteria

- Panel occupancy, pin count and byte totals match the daemon's live cache
  exactly.
- Eviction ordinals predict the actual next eviction, verified against a
  forced over-budget insert.
- Two conversations diverging after a shared root render as two heads under one
  root; two unrelated system prompts render as two roots flagged fragmented.
- Snapshot cost is independent of KV blob size — no `state` clone on the path.
- Refreshing triggers no subprocess spawn and no outbound network request.
- Recency ordering in the panel matches LRU order in the cache.
- Panel remains legible at 10 nodes and at 200.
- Every colour-carried meaning is also carried by text or glyph.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`
  and `cargo test --workspace --tests --lib` all pass; the size-budget gate
  passes.

## Potential Risks and Mitigations

1. **Snapshotting under the cache mutex stalls a generation.** Mitigation:
   copy cheap metadata under the lock; derive topology and verdicts outside it.
2. **`last_used` on the hot `get` path adds cost.** Mitigation: a single
   `Instant::now()` beside an operation that already clones megabytes.
3. **Edge derivation is quadratic if budgets later grow.** Mitigation: single
   pass over length-sorted nodes; cap rendered nodes with an overflow row.
4. **Frontier-only tree reads as missing data.** Mitigation: stated in the
   footer.
5. **Verdicts fire misleadingly during startup prewarm.** Mitigation: suppress
   until the first completed turn; show a warming state.
6. **Asset growth against the binary budget**, since the frontend is embedded
   with `include_str!`. Mitigation: no chart library, reuse existing classes,
   run the size gate before pushing.
7. **The panel will immediately show API branches never warming** — a known
   defect, not a rendering bug. Mitigation: expect it; it is the first thing
   the panel is meant to prove, and the fix belongs to the follow-up round.

## Alternative Approaches

1. **SVG tree instead of nested rows.** Prettier edges, true 2-D layout.
   Rejected: harder to keep accessible and legible at 200 nodes, and it
   discards reuse of existing row and chip styling.
2. **Flat table sorted by eviction order with an indent column.** Much less
   code, arguably better for "what dies next" alone. Rejected: branch
   structure becomes something you infer rather than see, which is the part
   being bought.
3. **Counters only, no tree.** A fraction of the work, answers "is it
   thrashing". Rejected: does not answer "which branch", which is what the
   next round of cache work needs.
4. **Include origin tagging now** so branches are coloured local vs API vs
   prewarm. Deferred: touches both backends' insert sites and widens the
   change; the row layout reserves the slot so it drops in cleanly later.
