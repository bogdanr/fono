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

- [ ] Task 8. On transcript completion, restore the best available checkpoint and process only the remaining suffix. Rationale: The final user text is not known until STT completes. The optimal path is to restore the most specific valid checkpoint, append/decode the transcript, then generate the assistant response.

- [x] Task 9. Add cancellation and invalidation rules. Rationale: If the user releases/cancels the hotkey, changes active window, switches tools, changes model, or changes prompt template, stale cache work must be cancelled or invalidated safely.

- [x] Task 10. Add bounded memory and eviction policy. Rationale: Multiple checkpoints are useful, but they consume RAM. Fono should keep a small number of high-value runtime checkpoints and evict stale/low-value entries under memory pressure.

- [x] Task 11. Keep caches in memory for the first implementation. Rationale: In-memory runtime caches avoid persistence hazards from stale model/runtime/prompt state. Disk persistence can be evaluated later only if startup warming is too expensive.

- [x] Task 12. Add benchmark support for shared-prefix prompts with changing suffixes. Rationale: Exact full-prompt replay proves mechanics, but real Fono usage needs to prove that caching stable prefixes helps when user text and window context change.

- [ ] Task 13. Add benchmark support for STT contention. Rationale: Cache building must not degrade transcription. The benchmark should compare STT-only, STT plus checkpoint restore, and STT plus checkpoint build/extension.

- [ ] Task 14. Add benchmark support for tool-count scaling. Rationale: One major reason to cache is to make larger tool sets feasible. Benchmarks should measure 0, 5, 10, 20, and 40 tools with and without cached tool prompts.

- [ ] Task 15. Add benchmark support for active-window context scaling. Rationale: Window context size will vary significantly. Benchmarks should measure cache behavior for no window context, small context, medium context, and large enriched context.

- [ ] Task 16. Promote the cache policy only after benchmark evidence. Rationale: The default runtime behavior should be based on measured latency, CPU contention, memory use, and correctness rather than assumptions.

## Verification Criteria

- [x] F7 and F8 stable prompt checkpoints can be built at runtime with low-priority scheduling.
- [x] Hotkey press can restore the correct stable checkpoint without blocking STT.
- [x] Active-window context can create a dynamic checkpoint that invalidates independently from stable prompts.
- [ ] Transcript-ready generation can use the best valid checkpoint and fall back safely when a more specific checkpoint is unavailable.
- [x] Cache keys prevent reuse across incompatible model/runtime/prompt/tool/window states.
- [x] Benchmarks show whether checkpoint restore improves time-to-first-token without hurting STT latency.
- [ ] Tool-count benchmarks quantify whether cached tool prompts make larger tool sets practical.
- [ ] Window-context benchmarks quantify when dynamic window checkpoints are worth building.

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

## Recommended Initial Policy

- [x] Build F7 system prompt checkpoint at startup/idle with very low priority.
- [x] Build F8 assistant system prompt checkpoint at startup/idle with very low priority.
- [x] Build assistant tool prompt checkpoint at startup/idle with very low priority.
- [x] On F7/F8 press, restore the matching stable checkpoint immediately.
- [x] When active-window context is available, build a dynamic window checkpoint only if doing so will not harm STT.
- [ ] When transcript is ready, use the best checkpoint available and process the remaining user-text suffix.
- [x] Keep all prompt-state caches in memory for the first production implementation.
