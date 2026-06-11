# Cache Waterfall — Trace Gaps Fix + F8 Cold-Prefill Fix

## Objective

Act on the evidence from the first real trace run (six files in `/tmp/fono-traces`,
2026-06-09 ~08:39–08:41). The instrumentation works and **proved the F8 cache is
missing on every assistant turn**. This plan closes the two trace-completeness gaps
the run exposed, then fixes the underlying F8 cold-prefill bug those traces caught.

Empirical baseline from the run (the "before" we must beat):

| Turn | lookup result | prewarmed entries present | outcome | cost |
|---|---|---|---|---|
| `assistant-…0003` | `F8ChatPrefix` **miss**, 110 prefix tok | 4 entries / 13.2 MB | full prefill `start_pos=0` | `built` **974 ms** |
| `assistant-…0004` | `F8ChatPrefix` **miss**, 194 prefix tok | 5 entries / 15.3 MB | full prefill `start_pos=0` | `built` **1714 ms** |

No `llm.prompt_cache_prefix_match` or `llm.prompt_cache_restored` fired on either
turn; the pinned bases built+pinned at startup (`startup-…0001`,
`llm.prompt_cache_pinned: 3`) sat unused. The cold prefill ≈ the entire
`llm_ttfb_ms` (1422 ms, 2093 ms) and grows with history.

---

## Workstream A — Gap #1: assistant `turn.finish` has no cache scoreboard

**Evidence:** the dictation and startup `turn.finish` events carry
`summary: {cache_hits, cache_misses, cold_prefills, bytes_restored}`
(`crates/fono/src/session.rs:1856`, `:2926`,
`crates/fono-core/src/turn_trace.rs:275`), but the assistant pump's `turn.finish`
(`crates/fono/src/assistant.rs`, the final `finish(...)` around the end-of-turn
summary, ~`:795-804`) does not. So the most important path lacks the at-a-glance
scoreboard.

- [ ] In `crates/fono/src/assistant.rs`, fold `trace.cache_scoreboard()` into the
  `summary` key of the assistant `turn.finish` args, matching the dictation path.
  The pump owns the `TurnTrace` (`assistant.rs:169-170`), so call
  `cache_scoreboard()` on that handle. Apply it to **all** `finish(...)` exits on
  this path (the normal completion and the early `aborted` returns) so every
  assistant trace ends with a scoreboard.
  Rationale: the scoreboard is the headline metric for whether caching helped;
  it must be present on the path it matters most.

## Workstream B — Gap #2: dictation STT/polish stages emit nothing

**Evidence:** dictation traces contain only
`turn.start → key.press → fsm.transition → cache.prepare_for_turn → key.release →
fsm.transition → turn.finish` (`dictation-…0002`, `…0006`). There is **no STT span
and no `polish.*` event**, even though ~0.4–0.7 s elapses between `key.release` and
`turn.finish` where transcription + polish + injection run. The polish-cache
instrumentation (`crates/fono-polish/src/llama_local.rs:328`, `:508`, `:531`,
`:561`, `:600`) is present but never fires because the ambient trace
(`TurnTrace::current()`, set via `make_current`) is not installed on the
thread/task that runs the post-release pipeline — `current_span`/`current_instant`
silently no-op when there is no current trace.

- [ ] Determine where the dictation pipeline runs after `key.release` in
  `crates/fono/src/session.rs` (the STT → polish → inject sequence reached from the
  release handler near `:2165`). Confirm whether it runs on the same task that
  started the trace at `:2027` or is dispatched to a worker.
  Rationale: the fix differs depending on whether it's a thread hop.
- [ ] Hold the dictation `TurnTrace` current across the whole post-release pipeline.
  Either keep the `make_current` guard alive until after polish+inject complete on
  the same task, or re-install it (`trace.make_current()`) at the start of the
  worker that does STT/polish. Mirror how the assistant pump keeps its trace current
  for the full turn.
  Rationale: this is the single change that makes the existing polish-cache spans
  actually record.
- [ ] Add an `stt` lane span on the dictation path wrapping the transcribe call
  (mirroring the assistant `stt.transcribe` at `assistant.rs:249-260`), so the
  dictation waterfall shows STT timing, not just a gap.
  Rationale: STT is the largest dictation stage; without it the waterfall is mostly
  empty between release and finish.
- [ ] Re-run a dictation turn with `FONO_ASSISTANT_TRACE=/tmp/fono-traces` and
  confirm the trace now contains an `stt` span and `polish.*` cache events (or,
  if polish is configured to a non-embedded backend in the test config, document
  that and verify with the embedded polish backend selected).
  Rationale: closes the loop — the gap was found by reading a trace, so verify by
  reading a trace.

## Workstream C — The real fix: F8 cold-prefill via longest-prefix restore

**Evidence/diagnosis:** the assistant live path
(`generate_with_prefix_cache`, `crates/fono-assistant/src/llama_local.rs:259-412`)
does an **exact-key** `cache.get(&key)` only (`:312`). On miss it cold-prefills the
entire prefix from `start_pos=0` (`:364-395`) and inserts the checkpoint with
`PromptStateCacheEntry::new` — **no recorded tokens** (`:381`). The startup prewarm
(`build_prompt_prefix_cache`, `:414-468`) also inserts bases with `::new`
(no tokens, `:462`). Because `find_longest_prefix` only matches entries that
recorded their tokens (`crates/fono-core/src/prompt_cache.rs:294`), and the
assistant path never calls it anyway, the pinned bases are unreachable. This is
why every turn is a full cold prefill.

The polish F7 path already does this correctly and is the template to copy:
`ensure_base_prefix_cache` pins the base **with tokens**
(`crates/fono-polish/src/llama_local.rs` `with_tokens`), and on an exact miss it
calls `find_longest_prefix` across `F7System`+`F7Context`
(`crates/fono-polish/src/llama_local.rs:508`) to restore the deepest base and decode
only the delta.

- [ ] **Record tokens on every assistant checkpoint insert.** Change the two
  inserts to `PromptStateCacheEntry::with_tokens(state, prefix_tokens)`:
  the live build at `crates/fono-assistant/src/llama_local.rs:381` and the prewarm
  build at `:462`. Without recorded tokens, nothing the assistant caches can ever
  be a longest-prefix candidate.
  Rationale: precondition for any prefix matching.
- [ ] **Add a longest-prefix fallback before cold prefill.** In
  `generate_with_prefix_cache`, on exact-key miss (`:364` `else` branch), first call
  `cache.find_longest_prefix(runtime, &[F8ChatPrefix, F8System, AssistantTools],
  full_tokens)` (or the appropriate layer set) and, if a hit is returned, restore
  that checkpoint (`set_state_data`), emit `llm.prompt_cache_prefix_match` +
  `llm.prompt_cache_restored`, and prefill only the tokens after the matched prefix
  length — instead of prefilling from `start_pos=0`. Only when no prefix matches do
  the existing full cold prefill + `cold_prefill(...)` event.
  Mirror the structure of `crates/fono-polish/src/llama_local.rs:508-561`.
  Rationale: this converts the ~1–1.7 s cold prefill into a base restore (~tens of
  ms per the F7 benchmarks) + a small suffix prefill.
- [ ] **Verify the F8 prompt is append-only so the base is a true token-prefix.**
  The Gemma system-first reordering already makes the system prompt lead the prompt
  (`docs/status.md` 2026-06-08 entries); confirm `build_prompt_split` /
  `build_gemma_prompt_split` still emit a prompt whose tokens start with the pinned
  `F8System` base tokens, otherwise `find_longest_prefix` will never match. Add or
  extend a unit test asserting the prewarmed base token sequence is a prefix of a
  representative `F8ChatPrefix` prompt's tokens.
  Rationale: the matching is only as good as the append-only invariant; lock it
  with a test so a future prompt-layout change fails loud.
- [ ] **Decide and document the layer the prewarm should target.** Either keep
  prewarming `F8System` (and let the new fallback match it as a prefix of
  `F8ChatPrefix`), or additionally prewarm an `F8ChatPrefix`-shaped base. Pick the
  `F8System`-as-prefix approach (matches F7, no second checkpoint to maintain) and
  remove the now-confirmed-dead `WindowContext` prewarm
  (`crates/fono-assistant/src/llama_local.rs` hotkey prepare path) and the dead
  `F7System` prewarm on the assistant backend.
  Rationale: stop paying for checkpoints the live path will never use; reduce cache
  pressure so the useful bases stay pinned.

## Verification Criteria

- **A:** every assistant `turn.finish` carries a `summary` scoreboard.
- **B:** a fresh dictation trace contains an `stt` span and (with embedded polish)
  `polish.*` cache events between `key.release` and `turn.finish`.
- **C (the win):** re-running two consecutive assistant turns with
  `FONO_ASSISTANT_TRACE` set shows, on turn 2+, `llm.prompt_cache_prefix_match`
  and `llm.prompt_cache_restored` (restore in tens of ms) instead of
  `llm.prompt_cache_built @ start_pos=0`; `llm_ttfb_ms` drops substantially versus
  the 1422/2093 ms baseline; the `turn.finish` scoreboard shows a prefix-restore
  rather than `cold_prefills` every turn.
- AGENTS.md pre-commit gate green, in order: `cargo fmt --all -- --check`;
  `cargo clippy --workspace --all-targets --features llama-local -- -D warnings`;
  `cargo test --workspace` (doctests may be skipped locally if `rustdoc` is absent,
  per AGENTS.md).
- `crates/fono-core/src/prompt_cache.rs` stays llama-agnostic (no new deps);
  every touched `.rs` keeps `// SPDX-License-Identifier: GPL-3.0-only` on line 1.
- A `docs/status.md` session entry is added describing this work (the prior session
  omitted one — fix that here).

## Risks and Mitigations

1. **Restoring a base that is not actually a token-prefix corrupts output.**
   Mitigation: keep both existing guards before trusting a restore — exact
   `prefix+suffix == prompt` string check and token-level `starts_with` — and on any
   mismatch fall through to a clean cold prefill having emitted nothing (the F7 path
   already does this). The new prefix-test (Workstream C) catches layout drift in
   CI.
2. **`find_longest_prefix` is O(entries) per turn.** Mitigation: the cache is bounded
   to 8 entries; the scan is trivial. No change needed.
3. **Holding the dictation trace current across a thread hop could keep the `Arc`
   alive or double-finish.** Mitigation: use the `Weak`-based current-trace design as
   is; ensure exactly one `finish` owner (the release handler) and re-install
   `make_current` on the worker rather than moving ownership.
4. **Removing the `WindowContext`/`F7System` assistant prewarm could regress an
   unforeseen consumer.** Mitigation: confirm via grep there are no other readers of
   those layers on the assistant backend before deleting; the trace run already shows
   they are never restored.

## Alternatives Considered

1. **Prewarm an `F8ChatPrefix`-shaped base instead of matching `F8System` as a
   prefix.** Rejected as the primary approach: history makes a full chat-prefix base
   stale immediately, so it would still miss; the `F8System`-prefix approach matches
   the proven F7 design and degrades gracefully as history grows.
2. **Split Workstreams A/B (trace gaps) into a separate change from C (the fix).**
   Viable, but the gaps are cheap and A/B make C's win measurable in the same trace,
   so shipping them together gives an immediate before/after.
