# Caching Status Assessment + Waterfall Instrumentation Extension

## Objective

Two coupled goals:

1. **Diagnose** the current state of the prompt-state (KV) cache work and
   confirm/refute the suspicion that "things are not working as we'd like."
2. **Extend** the existing lightweight `TurnTrace` instrumentation
   (`crates/fono-core/src/turn_trace.rs`) into a complete waterfall that
   visualises cache creation/consumption, hit/miss decisions, stage
   boundaries, and the keypress→pipeline timeline — across **both** the
   assistant (F8) path and the plain-dictation/polish (F7) path, not just
   assistant turns.

---

## Part A — Status Assessment (what the research found)

### A.1 The cache is always on; there is no kill-switch hiding the win
- Both backends instantiate `PromptStateCache` unconditionally in their
  constructors (`crates/fono-assistant/src/llama_local.rs:190`,
  `crates/fono-polish/src/llama_local.rs:114`). No config flag, no env var,
  no "default off". The only gate is the `llama-local` cargo feature
  (whether the embedded backend is compiled at all). So if caching
  underperforms, it is a **wiring/logic** problem, not a disabled-feature
  problem.

### A.2 The polish (F7) path is wired correctly — this side works
- Pins the `F7System` base **with recorded tokens** via
  `insert_pinned`/`with_tokens` (`crates/fono-polish/src/llama_local.rs:334`).
- On a per-app-context (`F7Context`) miss it falls back to
  `find_longest_prefix` across `F7System` + `F7Context`
  (`crates/fono-polish/src/llama_local.rs:409-420`), restores the pinned base
  and decodes only the delta. This is the intended graceful-degradation
  pattern.
- **Weakness:** the F7 base is built lazily inside the first `format()` call,
  not prewarmed at startup (`crates/fono-polish/src/llama_local.rs:638-643`
  only loads the model). The first cleanup after launch is therefore cold.

### A.3 The assistant (F8) path has a layer/lookup mismatch — this is the bug
This is the most likely root cause of "not working as we'd like":

- The **live reply path** consumes only the `F8ChatPrefix` layer via an
  **exact-key** lookup (`crates/fono-assistant/src/llama_local.rs:301-308`),
  and inserts entries with `PromptStateCacheEntry::new` — i.e. **no recorded
  tokens** (`:349`, `:429`).
- The **startup prewarm** and **hotkey prepare** build `F8System`,
  `AssistantTools`, `F7System`, and the deprecated `WindowContext` layers —
  **never `F8ChatPrefix`** (`:1223-1232`, `:1244-1267`).
- The F8 path **never calls `find_longest_prefix`**, and even if it did, its
  inserted entries record no tokens so they could never be prefix candidates.

**Net effect:** the prewarmed/prepared `F8System` base can never be restored
by the live reply path (wrong layer, no prefix fallback). The F8 cache only
hits when an *identical* `F8ChatPrefix` recurs — but history grows every
turn, so this is rare. Most F8 turns pay a full cold prefill, and the
startup/hotkey prewarm work is effectively dead for the live path. The
benchmarks reported in `docs/status.md` measured the *machinery in isolation*
(replay harnesses), not this live wiring — which is why the numbers looked
good while the lived experience does not.

### A.4 Dead work observed
- The deprecated `WindowContext` checkpoint is rebuilt on every hotkey press
  (`crates/fono-assistant/src/llama_local.rs:1251-1267`) despite the assistant
  no longer injecting window context (`crates/fono-core/src/prompt_cache.rs:50-52`).
- The assistant warming `F7System` (`:1224`) is doubly dead: wrong layer for
  F8 *and* a different backend instance/cache from the polish F7 path.

### A.5 Assumption / open question for the implementation agent
The design intent (`plans/2026-06-07-2026-06-07-runtime-prompt-state-cache-v1.md`)
must be reconciled with A.3. Two candidate fixes (decide before coding):
- **(a)** Make the F8 live path build/consume `F8ChatPrefix` entries **with
  recorded tokens** and use `find_longest_prefix` against the prewarmed
  `F8System` base (mirroring the working F7 design), or
- **(b)** Have the prewarm build the `F8ChatPrefix` layer the live path
  actually looks up.
Assumption for this plan: **(a)** is preferred — it matches the proven F7
pattern and degrades gracefully as history grows. This is a *fix* and is
out of scope for the instrumentation work below, but the instrumentation is
what will *prove* the fix works.

---

## Part B — Current Instrumentation State

- `TurnTrace` writes one Chrome Trace Event JSON per turn, opt-in via
  `FONO_ASSISTANT_TRACE` (`crates/fono-core/src/turn_trace.rs:71-77`); viewable
  in `chrome://tracing` / Perfetto.
- A trace is created **only** inside `run_assistant_turn`
  (`crates/fono/src/assistant.rs:169-170`). **Plain F7 dictation and the
  hotkey/FSM path create no trace at all** — `fono-hotkey`, `fono-stt`,
  `fono-polish` have zero `turn_trace` references.
- Traced today: STT timing, LLM stream open, LLM prefill (tokenize / batch /
  decode), per-token decode, TTS synth + engine internals, playback, splitter.
- **Cache events that exist:** `llm.prompt_cache_restored`
  (`crates/fono-assistant/src/llama_local.rs:320-332`) and
  `llm.prompt_cache_built` (`:351-361`, `:435-445`).
- **Gaps (emit nothing):** the cache lookup hit/miss decision itself
  (`cache.get` at `:301-308`), longest-prefix match attempts/failures (the
  `Ok(None)` cold-prefill fallback at `:286`/`:299`), pinning/eviction, hotkey
  FSM transitions and keypress timing (trace starts well *after* the press),
  and the **entire F7 polish path** (no trace file is ever written for
  dictation).

---

## Implementation Plan

### Phase 1 — Make cache decisions visible (assistant path)

- [ ] Add a `llm.prompt_cache_lookup` instant at the exact-key lookup site
  (`crates/fono-assistant/src/llama_local.rs:301-308`) recording
  `{layer, cache_key, hit: bool, token_count, cache_entries, cache_bytes}`.
  Rationale: today hit/miss is only inferable indirectly from whether
  `restored` or `built` fires; an explicit event makes the waterfall
  unambiguous and lets us count hit-rate.
- [ ] Add `llm.prompt_cache_prefix_match` / `llm.prompt_cache_cold_prefill`
  instants at the longest-prefix fallback branches (`:281`, `:286`, `:292`,
  `:299`) recording `{matched_layer, matched_tokens, total_tokens,
  decoded_suffix_tokens}` on match and `{reason}` on cold fallback.
  Rationale: the `Ok(None)` → full-prefill path is currently invisible, which
  is exactly the path A.3 says dominates F8 turns — we need to see it.
- [ ] Emit a `llm.prompt_cache_evicted` / `llm.prompt_cache_pinned` instant
  from the cache mutation sites. This likely requires a small callback/return
  signal from `PromptStateCache::insert`/`evict_over_budget`
  (`crates/fono-core/src/prompt_cache.rs:211-299`) since the cache itself is
  llama-agnostic and cannot call `turn_trace` directly without a dependency.
  Rationale: pinning/eviction churn is a prime suspect for thrashing; keep the
  cache crate dependency-clean by surfacing eviction facts to the caller.

### Phase 2 — Trace the F7 polish (plain dictation) path

- [ ] Start a `TurnTrace` for the dictation/polish flow. Add
  `TurnTrace::start_from_env()` + `make_current()` in the F7 session branch
  (the dictation path in `crates/fono/src/session.rs`, around the polish
  invocation summarised near `:4096-4108`), mirroring
  `crates/fono/src/assistant.rs:169-170`.
  Rationale: today dictation produces no trace whatsoever; this is half the
  product surface and the cache path the user most often exercises.
- [ ] Add `current_span`/`current_instant` calls inside
  `crates/fono-polish/src/llama_local.rs` for: base-prefix build
  (`ensure_base_prefix_cache`, `:305-337`), exact `F7Context` hit/miss
  (`generate_with_prefix_cache`, `:382-404`), `find_longest_prefix` result
  (`:409-420`), and the per-context checkpoint cache write (`:441-444`).
  Use a `f7-polish` lane and `polish.*` category to keep it distinct from the
  `llm` lane. Rationale: gives F7 the same cache-visibility F8 will have.
- [ ] Decide and document a consistent lane/category taxonomy so both paths
  render cleanly in one viewer:
  lanes `keys`, `stt`, `f7-polish`, `llm`, `tts`, `playback`, `cache`;
  categories prefixed by stage. Rationale: a "nice waterfall" needs stable
  lane ordering across F7 and F8 traces.

### Phase 3 — Capture the keypress → pipeline timeline

- [ ] Begin the trace at **press time**, not at `run_assistant_turn`. Introduce
  a pre-turn ambient trace started when the hotkey FSM enters a recording/tool
  state (`crates/fono-hotkey/src/fsm.rs` transitions, surfaced through
  `crates/fono/src/session.rs` press handlers around `:1932`, `:2183`, `:2258`).
  Rationale: the keypress, the audio-device acquisition, and the
  prepare-prompt-cache fire-and-forget all happen before today's trace exists,
  so the most latency-sensitive head of the waterfall is currently missing.
- [ ] Add a `keys` lane with `key.press` / `key.release` / `fsm.transition`
  instants recording `{trigger: F7|F8|Escape, from_state, to_state}` from the
  FSM. Requires either threading a `TurnTrace` handle into `fono-hotkey` or
  emitting via the ambient `current_instant` after the trace is installed
  early. Rationale: directly answers the user's "when keys are pressed" ask.
- [ ] Trace the hotkey-time `prepare_prompt_cache_for_turn` fire-and-forget
  (`crates/fono/src/session.rs:2631-2651` and the assistant
  `prepare_turn_prompt_caches`, `crates/fono-assistant/src/llama_local.rs:1241-1269`)
  so prewarm work shows on the timeline and its uselessness (per A.3/A.4) is
  visible until the A.5 fix lands. Rationale: makes the dead prewarm work
  self-evident in the waterfall, justifying the fix.

### Phase 4 — Startup prewarm visibility

- [ ] Instrument `spawn_warmups` / `prewarm_prompt_caches`
  (`crates/fono/src/session.rs:1739-1808`,
  `crates/fono-assistant/src/llama_local.rs:1220-1233`) with a `warmup` lane so
  the startup checkpoint builds (and which layers they target) are recorded.
  Since startup is not a "turn", gate this behind the same env var writing to a
  dedicated `startup-*.json` trace file. Rationale: lets us confirm whether the
  prewarmed layers match what the live path consumes (the A.3 mismatch).

### Phase 5 — Documentation & ergonomics

- [ ] Update the `turn_trace.rs` module doc and any developer doc to describe
  the new lanes, the F7 trace file, the startup trace file, and the keypress
  lane. Rationale: the env var and lane taxonomy are the only UX; they must be
  discoverable.
- [ ] (Optional) Add a tiny `args.summary` rollup to `turn.finish` reporting
  `cache_hits`, `cache_misses`, `cold_prefills`, `bytes_restored` so a single
  glance at the finish event states whether caching helped this turn.
  Rationale: turns the waterfall into a scoreboard without opening the file.

---

## Verification Criteria

- With `FONO_ASSISTANT_TRACE` set, a **plain dictation** turn now writes a
  trace file (previously none existed).
- A trace file contains a `keys` lane with the press event preceding all STT
  work, and the gap between press and first STT span is measurable.
- Every cache interaction on both F7 and F8 paths produces exactly one
  unambiguous lookup event tagged hit/miss, plus a prefix-match-or-cold-prefill
  event; no cache decision is inferable only indirectly.
- Loading a trace in `chrome://tracing`/Perfetto shows a single coherent
  waterfall with stable lane ordering across F7-only and F8 traces.
- The A.3 mismatch is now visible: F8 turns show repeated `cold_prefill`
  events and the prewarmed base never appears in a `restored` event (this is
  the evidence that motivates the separate cache-wiring fix).
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets
  --features llama-local -- -D warnings`, and `cargo test --workspace`
  all pass (per AGENTS.md pre-commit gate).
- `fono-core` gains **no** new heavy dependency; eviction/pinning facts reach
  the trace via caller-side signalling, keeping `prompt_cache.rs`
  llama-agnostic.

## Potential Risks and Mitigations

1. **Threading a trace into `fono-hotkey` adds coupling to a low-level crate.**
   Mitigation: use the existing ambient `current_instant` pattern (install the
   trace early via `make_current`) instead of passing a handle, mirroring how
   `fono-tts`/`fono-assistant` already emit without a parameter.
2. **Per-token / per-event instants can bloat trace files on long turns.**
   Mitigation: cache lookup/prefix events are O(1) per turn, not per token; keep
   the high-frequency `llm.decode_token` behaviour unchanged and add only
   coarse cache events.
3. **Instrumenting the cache mutation path risks pulling `turn_trace` into the
   llama-agnostic `fono-core::prompt_cache`.** Mitigation: return eviction/pin
   facts from `insert`/`evict_over_budget` (or accept an optional callback) and
   emit the trace event in the backend caller, not inside the data structure.
4. **Starting the trace at press time changes trace lifetime/ownership.**
   Mitigation: keep the `Weak`-based current-trace design; the press-time trace
   becomes the same `Arc` the turn later finishes, with a clear single
   `finish` owner to avoid double-write.
5. **Instrumentation may be mistaken for the fix.** Mitigation: explicitly scope
   this plan to *visibility*; the A.3/A.5 cache-wiring correction is tracked as
   a separate follow-up that this instrumentation will validate.

## Alternative Approaches

1. **Fix the F8 cache wiring first, instrument second.** Trade-off: faster
   perceived win, but you lose the before/after evidence that proves the fix —
   and you'd be flying blind on the F7 path which has no trace at all. The plan
   above deliberately instruments first so the fix is measurable.
2. **Replace the bespoke Chrome-Trace recorder with `tracing` +
   `tracing-chrome`/`tracing-flame`.** Trade-off: less custom code and richer
   span trees, but adds a dependency (deny.toml + GPL-3.0 license review per
   AGENTS.md) and gives less control over the exact lane layout that makes the
   waterfall readable. The existing recorder is small and already shaped for
   this; extending it is lower-risk.
3. **Emit structured `tracing` logs only (no trace file) and post-process.**
   Trade-off: zero new viewer wiring, but loses the visual waterfall the user
   explicitly asked for; harder to correlate stages by eye.
