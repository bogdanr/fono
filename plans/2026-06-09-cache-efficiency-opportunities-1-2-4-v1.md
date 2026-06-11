# Cache efficiency follow-ups (opportunities #1, #2, #4)

Date: 2026-06-09
Status: implemented (gate green; pending real-model trace verification)

## Context

The F8 prompt-state cache now works end-to-end (Option A double-count fix +
Option C completed-turn checkpoint with common-prefix truncation). The clean
trace run (`/tmp/fono-traces`, ~10:36) confirmed the flat-prefill win:
`matched_tokens` grows turn-over-turn (78 → 117 → 162 → 226) while
`decoded_prefix_tokens` stays flat at 17. The same traces exposed three
remaining inefficiencies, all addressed here.

Evidence (per assistant turn):
- Two `f8_chat_prefix` entries stored per turn — a pre-suffix "build"
  checkpoint (token counts 79 / 134 / 179 / 243) AND the completed-turn
  checkpoint (117 / 162 / 226 / 275). `find_longest_prefix` always selects the
  completed-turn one; the build checkpoint is never the winner.
- `cache_entries` climbs 2 → 4 → 6 → 8; default cap is `max_entries = 8`
  (`prompt_cache.rs:201`), so a few turns in, LRU starts evicting useful state.
- `turn.finish` summary reads `cache_hits: 0, cache_misses: 1` on turns that
  fully restored via longest-prefix — only `cold_prefills: 0` is honest.

## Objectives

1. **#1 — Stop storing the redundant pre-suffix checkpoint.** It is always a
   strict token-prefix of (and therefore dominated by) the same turn's
   completed-turn checkpoint, so it is never selected. Removing its store drops
   one O(n) `copy_context_state` + insert per turn and halves cache-entry
   growth.
2. **#2 — Prune dominated prefix entries on insert.** When a new
   longest-prefix-capable entry is inserted, drop any existing **non-pinned**
   entry of the **same layer + runtime** whose recorded `prefix_tokens` is a
   strict prefix of the new entry's tokens. Keeps the cache at ~1 frontier
   entry per conversation regardless of length; eliminates LRU thrash.
3. **#4 — Honest scoreboard.** Count a successful restore (exact or
   longest-prefix) as a hit and only a genuine cold prefill as a miss.

## Implementation

### #1 — `crates/fono-assistant/src/llama_local.rs`, `generate_with_prefix_cache`
- In the exact-miss branch, keep the `build_prefill` of `prefix_tokens[start..]`
  (still needed to position the KV) but **remove** the subsequent
  `copy_context_state` + `cache.insert` + `llm.prompt_cache_built` store
  (currently ~`:440-464`) and the now-unused `build_started` timer.
- Add a comment explaining the completed-turn checkpoint supersedes it, and
  that the rare degenerate-reply turn (no completed-turn store) gracefully
  falls back to the prior turn's completed-turn / the pinned base.

### #2 — `crates/fono-core/src/prompt_cache.rs`
- Add `pruned: Vec<EvictedEntry>` to `CacheMutationReport`.
- In `insert`, after inserting the new entry, if `entry.prefix_tokens` is
  non-empty, remove every other **non-pinned** entry with the same `layer` and
  `runtime_sha256` whose `prefix_tokens` is a strict prefix
  (`new.starts_with(old) && old.len() < new.len()`) of the new tokens; collect
  them into `report.pruned` and decrement `bytes` / `lru`.
- Pinned base layers are a different layer than `F8ChatPrefix`, and the
  same-layer guard plus the explicit pinned skip keep them safe.
- `record_cache_mutation` (`turn_trace.rs`): emit a `llm.prompt_cache_pruned`
  instant for each pruned entry (mirror the evicted handler) and include
  `report.pruned` in the early-return guard.
- Tests: dominated same-layer entry is pruned on insert; sibling
  (non-prefix) entries of the same layer are kept; pinned/different-layer
  bases are never pruned.

### #4 — `crates/fono-core/src/turn_trace.rs`, `cache_scoreboard`
- Redefine: `cache_hits` = count of `*prompt_cache_restored` events (exact +
  longest-prefix both emit it); `cache_misses` = `cold_prefills` = count of
  `*prompt_cache_cold_prefill`; `bytes_restored` unchanged (sum of
  `restored_bytes`). Stop treating an exact-key `lookup` miss as a cache miss.
- Update the doc comment. Add a test asserting a lookup-miss-then-prefix-match
  turn scores as 1 hit / 0 misses, and a cold-prefill turn as 0 hits / 1 miss.

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --features llama-local -- -D warnings`
- `cargo test --workspace --tests --lib --features llama-local`
- Empirical (user, real model): a multi-turn trace run should now show **one**
  `f8_chat_prefix` entry retained per conversation (`cache_entries` flat, not
  climbing by 2/turn), `llm.prompt_cache_pruned` events, and `turn.finish`
  summaries reading `cache_hits: 1, cache_misses: 0` on restored turns.

## Out of scope
- #3 (full-KV-state memory growth / delta storage) — largely mitigated by #1+#2.
- #5 (suffix-prefill + first-token latency floor) — inherent.
