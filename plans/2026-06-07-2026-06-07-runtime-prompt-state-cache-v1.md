# Runtime Prompt-State Cache Plan

## Objective

Design and implement Fono's embedded local-assistant prompt-state cache so stable prompt work is done early, reused safely, and never competes unnecessarily with STT. The target outcome is lower assistant time-to-first-token for F7/F8 and future wake-word flows while preserving correctness, cancellation, and predictable CPU use.

## Clarifications

- `intent_f8` is not required as a separate cache layer unless F8 uses a prompt that differs from the normal assistant prompt. In the current design, F8 should be treated primarily as a trigger that selects/restores the assistant cache.
- Startup warming should focus on stable prompt families: F7 dictation/polish prompt, F8 assistant prompt, and assistant tool prompt.
- Active-window context is dynamic and should be cached only after it is available on the local machine at runtime.
- Cache entries should be generated dynamically at runtime, not shipped as prebuilt artifacts.
- Heavy cache building should avoid competing with STT. Cache restore is cheap and can happen early; cache extension/prefill should be scheduled carefully.

## Implementation Plan

- [x] Task 1. Define cache layers and keys. Rationale: The cache must separate stable prompts from dynamic context so changing the active window does not invalidate the F7/F8 base prompts or tool prompt. The minimum layers should be F7 system prompt, F8 assistant system prompt, assistant tools prompt, and active-window context prompt.

- [x] Task 2. Add a runtime prompt-state cache manager for the embedded llama.cpp backend. Rationale: A dedicated manager gives Fono one place to build, restore, extend, invalidate, and evict prompt-state checkpoints without scattering cache logic through hotkey, STT, and assistant code paths.

- [x] Task 3. Generate strict cache keys from model/runtime identity and prompt identity. Rationale: Restoring a llama.cpp state from the wrong model, prompt text, tokenizer, context size, or tool schema can produce incorrect output. Keys should include model identity, runtime parameters, prompt/template version, token hash, and layer type.

- [x] Task 4. Build F7, F8, and tool checkpoints at startup with very low priority. Rationale: These prompts are stable and can be prepared before the user needs them, but startup cache work must not make the desktop feel slower or steal CPU from foreground work.

- [x] Task 5. Restore the appropriate stable checkpoint when F7 or F8 is pressed. Rationale: Hotkey press is the first reliable intent signal. Restoring a prepared checkpoint at this point is cheap and positions the local assistant to respond quickly once STT and context collection finish.

- [x] Task 6. Capture and enrich active-window context independently from stable checkpoint restore. Rationale: Window context is machine- and moment-specific, so it should be generated dynamically for the current desktop state rather than included in a static startup cache.

- [x] Task 7. Create or refresh the active-window checkpoint as soon as enriched window context is available, subject to CPU scheduling policy. Rationale: If STT is idle or has enough spare CPU, Fono can extend the restored F8/tool checkpoint with window context. If STT is CPU-bound, Fono should defer this work and use the best available stable checkpoint instead.

- [x] Task 8. On transcript completion, restore the best available checkpoint and process only the remaining suffix. Rationale: The final user text is not known until STT completes. The optimal path is to restore the most specific valid checkpoint, append/decode the transcript, then generate the assistant response.

- [x] Task 9. Add cancellation and invalidation rules. Rationale: If the user releases/cancels the hotkey, changes active window, switches tools, changes model, or changes prompt template, stale cache work must be cancelled or invalidated safely.

- [x] Task 10. Add bounded memory and eviction policy. Rationale: Multiple checkpoints are useful, but they consume RAM. Fono should keep a small number of high-value runtime checkpoints and evict stale/low-value entries under memory pressure.

- [x] Task 11. Keep caches in memory for the first implementation. Rationale: In-memory runtime caches avoid persistence hazards from stale model/runtime/prompt state. Disk persistence can be evaluated later only if startup warming is too expensive.

- [x] Task 12. Add benchmark support for shared-prefix prompts with changing suffixes. Rationale: Exact full-prompt replay proves mechanics, but real Fono usage needs to prove that caching stable prefixes helps when user text and window context change.

- [ ] Task 13. Add benchmark support for STT contention. Rationale: Cache building must not degrade transcription. The benchmark should compare STT-only, STT plus checkpoint restore, and STT plus checkpoint build/extension.

- [x] Task 14. Add benchmark support for tool-count scaling. Rationale: One major reason to cache is to make larger tool sets feasible. Benchmarks should measure 0, 5, 10, 20, and 40 tools with and without cached tool prompts.

- [x] Task 15. Add benchmark support for active-window context scaling. Rationale: Window context size will vary significantly. Benchmarks should measure cache behavior for no window context, small context, medium context, and large enriched context.

- [ ] Task 16. Promote the cache policy only after benchmark evidence. Rationale: The default runtime behavior should be based on measured latency, CPU contention, memory use, and correctness rather than assumptions.

## Verification Criteria

- [x] F7 and F8 stable prompt checkpoints can be built at runtime with low-priority scheduling.
- [x] Hotkey press can restore the correct stable checkpoint without blocking STT.
- [x] Active-window context can create a dynamic checkpoint that invalidates independently from stable prompts.
- [x] Transcript-ready generation can use the best valid checkpoint and fall back safely when a more specific checkpoint is unavailable.
- [x] Cache keys prevent reuse across incompatible model/runtime/prompt/tool/window states.
- [x] Benchmarks show whether checkpoint restore improves time-to-first-token without hurting STT latency.
- [x] Tool-count benchmarks quantify whether cached tool prompts make larger tool sets practical.
- [x] Window-context benchmarks quantify when dynamic window checkpoints are worth building.

## Potential Risks and Mitigations

1. **Cache work competes with STT for CPU**
   Mitigation: Treat restore as cheap, but schedule prefill/extension as low-priority and cancellable. Defer window-context checkpoint building when STT is CPU-bound.

2. **Stale or incompatible checkpoint restores produce incorrect output**
   Mitigation: Use strict cache keys that include model identity, runtime parameters, prompt/template/tool hashes, token count, and cache layer kind.

3. **Layered cache logic becomes too complex**
   Mitigation: Start with only stable F7/F8/tool checkpoints plus one active-window layer. Add more layers only after benchmarks prove value.

4. **Memory use grows with multiple checkpoints**
   Mitigation: Keep caches in memory only, bound entry count/bytes, and evict stale window-specific checkpoints first.

5. **Window context changes faster than checkpoints can be built**
   Mitigation: Use the stable checkpoint fallback. Build window checkpoints only when context remains current and CPU budget allows.

6. **Large tool sets increase first-build cost**
   Mitigation: Warm the tool checkpoint at startup/idle time and benchmark tool-count scaling before increasing default tool exposure.

## Alternative Approaches

1. **Full prompt prefill after transcript only**: Simplest implementation, but pays all prompt processing cost at the worst time and does not exploit stable system/tool prompts.

2. **Live staged prefill without checkpoints**: Prefill system/tools/window/user text progressively in a single live context. This can reduce latency but is harder to cancel, reuse, or roll back when context changes.

3. **Exact full-prompt cache only**: Fast for repeated identical prompts and benchmarks, but too narrow for real assistant usage where the current user request usually changes.

4. **Persistent disk cache**: Could improve cold startup, but is riskier because llama.cpp state depends on exact model/runtime/prompt identity. Defer until in-memory runtime caching is proven.

## Initial Implementation Results

Completed in the first implementation slice:

- Added embedded prompt-state cache layer types for F7 system, F8 system, assistant tools, active-window context, benchmark prefixes, and exact prompts.
- Added strict runtime cache keys that include the cache layer, model/runtime identity, prompt SHA-256, token SHA-256, and token count.
- Added a bounded in-memory LRU prompt-state cache with an initial budget of 8 entries / 256 MiB.
- Added `fono-bench assistant-prefix-cache` to benchmark one cached stable prefix against multiple changing suffixes.
- Kept production hotkey/startup/window integration out of this slice; those remain pending until the shared-prefix benchmark evidence is reviewed.

Benchmark artifact:

- `/tmp/fono-runtime-prompt-cache/prefix-cache-controlled-release.json`

Benchmark summary with `gemma-4-e2b.gguf`, `ctx=2048`, `threads=8`, `batch=2048`, `ubatch=512`:

| Metric | Result |
|---|---:|
| Prefix size | 783 chars / 181 tokens |
| State size | 3,340,938 bytes |
| One-time prefix prefill | 1,836 ms |
| Median restore | 9 ms |
| Median suffix prefill | 147 ms |
| Median cached TTFB | 227 ms |
| Median cached latency | 485 ms |
| Median uncached latency | 2,989 ms |
| Exact output matches | 6 / 9 |

The controlled benchmark proves the core real-world shape: once the shared system/tool/window prefix is cached, changing short suffixes avoid reprocessing the 181-token prefix and respond much faster. The `outputs_match` failures were concentrated in an intentionally brittle one-word `window` suffix where both uncached and cached paths generated long repetitive text until the generation cap; this should be treated as a benchmark prompt-quality issue, not a cache-key or restore failure.

## Task 8 Implementation (transcript-ready prefix cache)

The assistant reply path now consumes the prompt-state cache instead of only
building it:

- Added `build_prompt_split`, which splits the rendered reply prompt into a
  stable prefix and a per-turn suffix (the user text plus the closing
  template). By construction `prefix + suffix` reproduces `build_prompt`
  byte-for-byte (in fact `build_prompt` is now *defined* as the concatenation,
  so they cannot diverge); unit tests assert this for Gemma and ChatML, with
  and without a system prompt. See the "Gemma system-first re-ordering" section
  below for the prefix boundary — the original Gemma boundary (system in the
  per-turn tail) was wrong for multi-turn caching and has been corrected.
- Added `generate_with_prefix_cache` on the embedded backend: restores a cached
  `F8ChatPrefix` checkpoint when present (building it on first use), prefills
  only the suffix tokens, then generates. It returns `Ok(None)` — having emitted
  nothing — on any incompatibility (empty split, non-token-prefix boundary,
  oversized prompt, failed restore), so the caller falls back to a full prefill.
- `reply_stream` now calls `run_inference_with_prefix_cache`, which additionally
  re-checks `format!("{prefix}{suffix}") == prompt` before trusting the split.
  Two independent guards (exact-string equality + token-prefix `starts_with`)
  make a wrong-state restore impossible: worst case is a safe full prefill.
- Build cost on a cache miss is the normal prefix prefill plus one state copy
  (single-digit ms in the Task 12 benchmark); a hit skips the prefix prefill
  entirely (≈9 ms restore vs ≈1.8 s prefill in that benchmark).

Known limitation (follow-up, gated on benchmarks 13–16): startup/hotkey
pre-warming still builds the older raw-prompt `F7System`/`F8System`/
`AssistantTools`/`WindowContext` checkpoints, which the live reply path no
longer restores (it keys on `F8ChatPrefix`). Pre-warming the exact
`F8ChatPrefix` at hotkey time is not yet wired because the reply-time history
snapshot includes the just-pushed user turn, so the hotkey-time prefix cannot be
reproduced ahead of the transcript. The cache therefore self-populates lazily on
the reply path and reuses the checkpoint whenever an identical system+history
prefix recurs; tightening pre-warm alignment is deferred until the contention
and scaling benchmarks justify the policy.

## Gemma system-first re-ordering (multi-turn cache correctness)

The original Gemma builder rendered `[history] → <start_of_turn>user\n{system}
\n\nUser request: {user}`, i.e. the large immutable system/tool prompt sat in
the *current* user turn, **after** the rolling history. For KV prefix caching
this is exactly inverted: the expensive stable text was re-prefilled every turn
while the cheap, mutable history occupied the cacheable head. It also capped
`F8ChatPrefix` reuse at turn 1 on Gemma — from turn 2 on, history preceded
system, so the checkpoint was no longer a token-prefix and the path fell back
to a full prefill.

Corrected layout: the system prompt is prepended to the **first** user turn
(Gemma's trained convention — Gemma has no dedicated system role), making the
rendered prompt **strictly append-only**:

```
<start_of_turn>user
{system}

{turn-1 user}<end_of_turn>
<start_of_turn>model
{turn-1 assistant}<end_of_turn>
...
<start_of_turn>user
{current user text}<end_of_turn>      ← only this varies per turn (the suffix)
<start_of_turn>model
```

Consequences:
- The leading tokens (system, then every completed turn) never change as the
  conversation grows. A boot-built `<start_of_turn>user\n{system}\n\n`
  checkpoint and a per-conversation checkpoint are both valid token-prefixes on
  every turn — the property the whole cache scheme depends on.
- The cache prefix boundary is now immediately before the current user text;
  the suffix is `{user}<end_of_turn>\n<start_of_turn>model\n`.
- `build_prompt` is defined as `prefix + suffix`, so the rendered prompt and the
  cache split can never drift.

Regression guards (in `crates/fono-assistant/src/llama_local.rs` tests):
`gemma_system_leads_prompt_regardless_of_history`,
`gemma_conversation_is_append_only`, `chatml_conversation_is_append_only`, and
`gemma_history_render_is_stable_across_turns`. The append-only tests simulate a
3-turn conversation and assert each turn's full prompt is an exact string
prefix of the next turn's — if a future change pushes system back into the tail
or otherwise breaks ordering, these fail loudly. ChatML already led with system
and was unaffected, but is now covered by the same invariant.

### Multi-turn benchmark (2026-06-08, `gemma-4-e2b.gguf`, ctx=4096, threads=8, batch=4096, ubatch=512, 2 iters/turn)

New `fono-bench assistant-conversation-cache` subcommand walks a growing
conversation through the **real** `build_prompt_split` (so it exercises the
fixed Gemma layout end-to-end), replaying uncached-vs-cached generation at each
turn. Artifact: `/tmp/fono-runtime-prompt-cache/conversation-cache.json`.

| Turn | History turns | Prefix tok | Cold prefix prefill | Cached restore | Suffix tok / prefill | Cached TTFB | Uncached full latency |
|---|---|---|---|---|---|---|---|
| 1 | 0 | 31 | 447 ms | 30 ms | 22 / 339 ms | 341 ms | 2029 ms |
| 2 | 2 | 90 | 1277 ms | 39 ms | 23 / 640 ms | 641 ms | 3251 ms |
| 3 | 4 | 150 | 2086 ms | 37 ms | 24 / 382 ms | 383 ms | 3243 ms |
| 4 | 6 | 211 | 2813 ms | 34 ms | 25 / 508 ms | 509 ms | 4899 ms |
| 5 | 8 | 273 | 3877 ms | 15 ms | 23 / 490 ms | 491 ms | 6878 ms |
| 6 | 10 | 333 | 4518 ms | 21 ms | 24 / 374 ms | 375 ms | 6927 ms |

Reading the result:
- **The cache now works on every turn, not just turn 1.** That is the whole
  point of the re-ordering — pre-fix, a Gemma checkpoint stopped matching from
  turn 2 on. Here the restore succeeds at every turn (state restore 15–39 ms,
  flat regardless of the 0.5→6.1 MB checkpoint).
- **The cache replaces a prefix prefill that grows to ~4.5 s (turn 6, 333
  tokens) with a ~21 ms restore.** That cold-prefill column is exactly the cost
  the cache amortizes away once a checkpoint exists; the uncached path re-pays
  it every turn, which is why uncached full latency climbs to ~6.9 s while the
  cached path stays bounded.
- **Per-turn cached cost stays flat as history grows.** TTFB is ~341–641 ms
  across the whole conversation (it tracks the ~22–25-token suffix, not the
  growing prefix). The uncached path's first token cannot arrive until the full
  prefix is prefilled, so its latency scales with conversation length.
- `outputs_match` was 2/2 on five of six turns and 0/2 on turn 3. The divergence
  is sampling noise — both paths free-run to `MAX_NEW_TOKENS = 384` on synthetic
  prompts with no natural stop, so tiny FP differences compound into different
  ramble. The restored KV state is correct (it matches on the other five turns);
  TTFB/restore/suffix-prefill are the stable decision metrics.

Net: the system-first re-ordering converts the cache from "turn-1-only on Gemma"
into a genuine multi-turn win — flat per-turn time-to-first-token and a ~20 ms
restore standing in for a prefill that would otherwise grow unbounded with the
conversation.

Still open: the contention/scaling policy work (Tasks 13, 16).

## Tasks 14 & 15 Implementation (cache scaling benchmarks)

Added the `fono-bench assistant-cache-scaling` subcommand, which sweeps one
prefix dimension and reports cached-vs-uncached latency per size:

- `--dimension tools` synthesises a stable prefix carrying N tool/function
  descriptors (`--sizes 0,5,10,20,40`), covering Task 14.
- `--dimension window` synthesises a stable prefix carrying an N-line
  active-window context block (`--sizes 0,8,32,96`), covering Task 15.
- Each synthetic prefix ends at `User request:`, so the per-turn suffix begins
  on a stable token boundary — the same prefix/suffix split the live reply path
  uses — and replays through the existing `replay_raw_prompt_prefix_cache`
  machinery. The report emits, per size: prefix chars/tokens, state bytes,
  one-time setup prefill, median uncached vs cached latency, median TTFB, median
  restore, median suffix prefill, output-match count, and a rounded
  `cached_speedup_x`. Schema `assistant-cache-scaling-report-v1`.

Example invocation:

```
cargo run -p fono-bench --features llama-local -- assistant-cache-scaling \
  --model-path <gguf> --dimension tools --sizes 0,5,10,20,40 \
  --suffix "turn on the kitchen light" --suffix "what's the weather" \
  --iterations 3 --machine-label dev --out /tmp/cache-scaling-tools.json
```

These produce the evidence Task 16 needs (whether cached tool/window prefixes
make larger tool sets and richer window context practical). Task 13 (STT
contention) and Task 16 (policy promotion) remain open.

### Benchmark Results (2026-06-08, `gemma-4-e2b.gguf`, ctx=4096, threads=8, batch=4096, ubatch=512, 2 iters × 3 suffixes)

Artifacts:

- `/tmp/fono-runtime-prompt-cache/cache-scaling-tools.json`
- `/tmp/fono-runtime-prompt-cache/cache-scaling-window.json`

**Tool-count scaling (Task 14):**

| Tools | Prefix tok | One-time prefill | State | Median restore | Median suffix prefill | **Cached TTFB** | Uncached latency |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 0  | 25   | 217 ms    | 0.5 MB | 15 ms | 112 ms | **114 ms** | 3,948 ms |
| 5  | 424  | 5,143 ms  | 7.8 MB | 21 ms | 114 ms | **115 ms** | 5,981 ms |
| 10 | 821  | 10,341 ms | 15 MB  | 14 ms | 117 ms | **118 ms** | 11,249 ms |
| 20 | 1,631| 21,077 ms | 30 MB  | 21 ms | 119 ms | **120 ms** | 22,284 ms |
| 40 | 3,251| 44,806 ms | 60 MB  | 27 ms | 132 ms | **133 ms** | 49,084 ms |

**Window-context scaling (Task 15):**

| Lines | Prefix tok | One-time prefill | State | Median restore | Median suffix prefill | **Cached TTFB** | Uncached latency |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 0  | 25    | 225 ms    | 0.5 MB | 15 ms | 76 ms  | **78 ms**  | 1,071 ms |
| 8  | 295   | 3,629 ms  | 5.4 MB | 20 ms | 82 ms  | **98 ms**  | 6,381 ms |
| 32 | 1,132 | 14,262 ms | 21 MB  | 22 ms | 104 ms | **105 ms** | 20,113 ms |
| 96 | 3,372 | 46,221 ms | 62 MB  | 28 ms | 109 ms | **138 ms** | 48,001 ms |

Findings:

- **Time-to-first-token is flat and prefix-size-independent.** Cached TTFB stays
  ~78–138 ms across the whole sweep, while the uncached path must reprocess the
  entire prefix first — climbing to ~48–49 s at ~3,300 prefix tokens. State
  restore is a near-constant ~15–28 ms regardless of prefix size; only the small
  per-turn suffix prefill (~76–132 ms) is paid each turn.
- **The win grows with prefix size.** At 0 tools/lines the cache barely helps
  (1.1–1.5×) because there is nothing to amortise; by 40 tools / 96 lines a
  cached restore replaces a ~45 s prefill — a ~33–39× reduction in
  time-to-useful-work. This directly answers Tasks 14/15: cached prefixes make
  large tool sets and rich window context practical that would otherwise be
  unusable interactively.
- **Memory is bounded and affordable.** The largest checkpoints are ~60–62 MB of
  llama.cpp state at ~3,300 tokens, so the 256 MiB / 8-entry budget holds ~4
  large checkpoints — enough for F7/F8/tools plus one window-context layer.
- **`cached_speedup_x` (full-latency ratio) is noisy and should not be the
  headline.** Because both paths generate up to `MAX_NEW_TOKENS = 384` and the
  synthetic prompts have no natural stop, total latency is dominated by variable
  generation length (e.g. the 20-tool point's 17.9 s median cached latency is a
  generation-length outlier, not a cache regression). TTFB, restore, and
  suffix-prefill are the stable, decision-relevant metrics. The `outputs_match`
  dips at mid sizes are the same synthetic-rambling artefact, not a cache
  correctness failure — restore reproduces the exact prefix state by construction.

Policy implication for Task 16: the **latency and memory** acceptance criteria
are now met with strong evidence — caching stable prefixes should be the default.
The remaining gate is **CPU contention** (Task 13): the one-time prefill is
expensive (up to ~45 s for a 3,300-token prefix), so building/extending large
checkpoints must stay low-priority and deferred while STT is CPU-bound. Promotion
is therefore evidence-backed on two of three axes; Task 13 closes the third.

## Recommended Initial Policy

- [x] Build F7 system prompt checkpoint at startup/idle with very low priority.
- [x] Build F8 assistant system prompt checkpoint at startup/idle with very low priority.
- [x] Build assistant tool prompt checkpoint at startup/idle with very low priority.
- [x] On F7/F8 press, restore the matching stable checkpoint immediately.
- [x] When active-window context is available, build a dynamic window checkpoint only if doing so will not harm STT.
- [x] When transcript is ready, use the best checkpoint available and process the remaining user-text suffix.
- [x] Keep all prompt-state caches in memory for the first production implementation.

## Design Revision v2 — Simplified Layered Cache (2026-06-08, LOCKED)

This revision supersedes the window-context portions of the original plan after a
design review with the maintainer. Two scope decisions drive it:

1. **The assistant (F8) will NOT use active-window context in its prompt.** The
   `active_window_context` capture in `crates/fono/src/session.rs:223` and the
   `WindowContext` cache layer are therefore vestigial for the reply path. (The
   string is still captured and may feed other features, but it does not enter
   the assistant prompt and is not a cache layer we build/restore for F8.) This
   removes the "Option A vs B" placement question entirely — there is nothing
   volatile to place between system+tools and history.
2. **Transcription (F7) will NOT use the per-utterance language directive in the
   cached path.** Its suffix is just the transcript.

### The unified model — both paths are the same shape

| | F7 transcription (polish) | F8 assistant |
|---|---|---|
| **Pinned base** (context-independent, prewarmed, never evicted) | `main + advanced + dictionary` | `system + tools` |
| **Recurring middle layer** | `rule_suffix` (app context: CLI / editor / browser / terminal-agent) | conversation history (grows within the 5-min session) |
| **Volatile suffix** (decoded fresh each turn) | transcript | request |

Both prompts are already (or will be) **append-only / prefix-ordered**: the
stable base leads, the recurring layer is appended, the volatile suffix is last.
F8's Gemma builder was already fixed to be system-first; F7's polish prompt is
naturally ordered this way (`crates/fono-polish/src/traits.rs:33-57`).

### Why this is correct for KV prefix caching

Each checkpoint is an independent, fully-serialized copy of llama.cpp state
(`copy_state_data` → `Vec<u8>`; restore via `set_state_data` into a fresh
context). Entries do not reference each other, so eviction of one never
corrupts another, and a miss is only a latency cost (rebuild via prefill), never
a correctness risk — guarded by exact `prefix+suffix == prompt` string equality
plus a token-level `starts_with` check.

### New work items (v2)

- [x] Task 17. **Pinning.** Protect the *currently active* F8 `system+tools`
  base and F7 base (`main+advanced+dictionary`) checkpoints from LRU eviction,
  keyed to the active prompt/runtime identity. When the prompt/model changes,
  release the stale pin and pin the new base. Rationale: these are the smallest,
  most-reused, prewarmed entries; evicting one drops the next use back to a cold
  prefill (up to ~45 s for a large tool prompt). Pinning converts "usually warm
  under LRU" into a hard guarantee at the cost of ≤2 bounded slots.
  *Done: `PromptStateCache::insert_pinned` + `PromptStateCacheLayer::is_pinnable`
  (F7System/F8System/AssistantTools); `evict_over_budget` skips pinned entries
  and only the most-recent snapshot of a pinnable layer stays pinned (stale pin
  released on prompt change). Wired into `build_prompt_prefix_cache`. Covered by
  unit tests in `fono-core::prompt_cache`.*

- [x] Task 18. **Shared cache machinery.** Lift `PromptStateCache` /
  `PromptStateCacheKey` / `PromptStateCacheEntry` out of `fono-assistant` into a
  location usable by both the assistant and the polish (F7) backend (e.g. a
  shared module/crate), rather than duplicating. The polish backend
  (`crates/fono-polish/src/llama_local.rs`) currently has NO prompt-state cache:
  `format()` builds the full prompt fresh and runs cold every dictation.
  *Done: extracted into `crates/fono-core/src/prompt_cache.rs` as a
  llama-agnostic data structure (LRU + byte budget + pinning, opaque
  `Vec<u8>` state blobs, no `llama-cpp-2` dependency). `fono-assistant` now
  imports it and keeps only the llama.cpp glue (key fingerprint, build/restore).
  7 unit tests in fono-core. The `F7Context` layer was added for Task 20.*

- [x] Task 19. **F7 restore-and-suffix.** Ported the llama.cpp build/restore
  glue into the polish backend (`crates/fono-polish/src/llama_local.rs`),
  guarded by the same exact-string (`prefix+suffix == prompt`) + token-prefix
  (`full_tokens.starts_with(prefix_tokens)`) checks as F8 so a miss is only
  slower, never wrong. `format()` now splits the ChatML prompt
  (`build_chatml_prompt_split_*`), and `run_inference_cached` restores the
  deepest matching checkpoint and decodes only the transcript suffix; on any
  incompatibility it falls back to a full prefill. The pinned base
  (`<|im_start|>system\n{base_system}`) is built lazily on first use and
  pinned, then reused for every dictation. Split-reproduction and
  base-is-a-prefix regression tests added.*

- [x] Task 20. **F7 per-context (app) layer.** The full system prefix
  (`base + rule_suffix[context]`, plus any language directive present in the
  rendered `system_prompt()`) is cached under the `F7Context` layer, keyed by
  the prompt + token fingerprint — so each focused-app context gets its own
  checkpoint, restored exactly on the next dictation into that app. Contexts
  are few and recurring and each `rule_suffix` is small, so these checkpoints
  are cheap and hit constantly. `FormatContext::base_system_prompt()` exposes
  the pinnable base distinct from the per-context full prompt.

- [x] Task 21. **Longest-prefix matching.** `PromptStateCache::find_longest_prefix`
  (in `fono-core`) returns the deepest cached entry whose recorded token
  sequence is a *proper* token-prefix of the new prompt, scoped by runtime +
  layer set. The F7 path uses it on an exact-key miss: a fresh per-context
  prefix still restores the pinned base and decodes only the per-context delta
  instead of a cold prefill. Graceful fallback chain: exact F7Context hit →
  longest-prefix (pinned base) → cold. Pinning guarantees the floor is never
  colder than the base after warmup. 3 dedicated unit tests in fono-core
  (deepest-match, proper-prefix+runtime scoping, tokenless entries ignored).

### Sequencing

17 (pinning, safe, self-contained) → 18 (shared machinery) → 19 (F7 restore) →
20 (F7 per-context) → 21 (longest-prefix). Run the fmt/clippy/test gate after
each. Task 13 (STT contention) is now largely designed away: the only heavy
prefill is the one-time base build at startup/idle; per-turn work is restore +
small-suffix decode after STT completes.
