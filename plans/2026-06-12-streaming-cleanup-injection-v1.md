# Streaming Cleanup-LLM Output Into Live Injection

## Objective

Reduce perceived latency on longer dictations by injecting the cleanup
(polish) LLM's output **incrementally** — as tokens/words arrive — instead of
waiting for the complete cleaned string before typing a single character.
Today the dictation pipeline blocks on the whole cleaned transcript, so the
user stares at an empty cursor for the full LLM duration (multi-second on
local backends, sub-second to a few hundred ms on cloud). Streaming injection
would start filling the cursor almost immediately and finish typing roughly
when generation finishes.

## Current Behaviour (as found)

- `TextFormatter::format(&self, raw, ctx) -> Result<String>`
  (`crates/fono-polish/src/traits.rs:385`) is the single cleanup entry point.
  It returns one complete `String`. There is **no** streaming variant.
- Cloud cleanup (`crates/fono-polish/src/openai_compat.rs:205`) sends
  `stream: false` (`openai_compat.rs:236`) and reads the whole body before
  returning. Anthropic (`crates/fono-polish/src/anthropic.rs`) is the same
  shape.
- Local cleanup (`crates/fono-polish/src/llama_local.rs`) decodes
  token-by-token internally but only returns the assembled string; CPU
  inference is ~5–15 tok/s (`llama_local.rs:7-9`), i.e. 7–20 s for a typical
  output — this is exactly where streaming helps most.
- The dictation orchestrator awaits the full string at
  `crates/fono/src/session.rs:3918` (`run_pipeline`) and the live path at
  `session.rs:3545`, then injects the **whole** `final_text` in one shot
  (`session.rs:3978`, batch; `session.rs:3619`, live).
- Injection backends (`crates/fono-inject/src/inject.rs:131`) are stateless
  per call: `wtype <text>`, `ydotool type <text>`, `xdotool type <text>`,
  XTEST per-character, or enigo. Each call appends at the current cursor — so
  calling them repeatedly with successive deltas naturally produces
  incremental typing. There is no per-call setup cost beyond process spawn
  (subprocess backends) which is relevant to chunk sizing.
- **Safety guards run on the complete output** and fall back to the raw
  transcript when they fire: `looks_like_clarification`
  (`openai_compat.rs:361`, `anthropic.rs:90`, `llama_local.rs:936`),
  `looks_like_degenerate_cleanup` (`llama_local.rs:930`),
  `looks_like_translated_cleanup` (`llama_local.rs:942`). When any fires the
  backend returns the **raw** text instead of the cleaned text. Empty-output
  and trailing-`\n\n` trimming also happen on the full string.
- The assistant (F8) path **already streams** end-to-end:
  `Assistant::reply_stream -> BoxStream<Result<TokenDelta>>`
  (`crates/fono-assistant/src/traits.rs:146`) feeding a sentence splitter into
  TTS. This is the proven in-repo pattern to mirror for cleanup. SSE plumbing
  already exists (`fono-http/src/sse.rs`, `fono-assistant/src/sse.rs`).

## The Core Tension (why this is "medium", not "easy")

Incremental injection is **append-only and irreversible**: once a character is
typed into the user's focused app, Fono cannot retract it. But the existing
cleanup contract depends on inspecting the *entire* output to decide whether to
inject the cleaned text or discard it and inject the raw transcript instead
(clarification refusals, role-token degeneration, accidental translation,
empty output). Naive streaming would type a clarification/translation/garbled
prefix before the guard could fire, leaving the user worse off than today.

Therefore the design must reconcile "start typing early" with "be able to fall
back to raw". The plan below treats that reconciliation as the central design
decision and offers concrete strategies.

## Implementation Plan

- [ ] Task 1. **Add a streaming variant to the `TextFormatter` trait.**
  Introduce `format_stream(&self, raw, ctx) -> Result<BoxStream<'static,
  Result<String>>>` (delta chunks) with a **default implementation** that
  calls the existing `format()` and yields a single final chunk. Rationale:
  keeps all current backends compiling unchanged and lets streaming be opt-in
  per backend. Mirror the assistant `TokenDelta` shape
  (`fono-assistant/src/traits.rs:55`) for consistency; cleanup needs only the
  `text` field (no tool events).

- [ ] Task 2. **Implement real streaming in the cloud backend.**
  In `openai_compat.rs`, add a streaming request path (`stream: true`) that
  parses SSE deltas via the existing `fono-http`/`fono-assistant` SSE
  machinery and yields text chunks. Keep the non-streaming `format()` intact
  for callers that want the whole string. Rationale: cloud is where SSE is
  cheap and well-understood; reuse existing SSE code rather than writing new
  parsing.

- [ ] Task 3. **Implement real streaming in the local backend.**
  In `llama_local.rs`, expose the per-token decode loop as a stream (emit each
  decoded token's text as a chunk) instead of only returning the assembled
  string. This is where the latency win is largest (7–20 s → first words in
  <1 s). Preserve the prompt-state-cache, sampler, stop-sequence, and
  `MAX_NEW_TOKENS` logic. Rationale: local inference is the dominant pain
  point the user is describing.

- [ ] Task 4. **Design and implement the guard-vs-streaming reconciliation
  (central decision).** Choose one of the strategies below and wire it into the
  orchestrator. Recommended default: **bounded-prefix gate** — buffer output
  until either (a) a small safety threshold is crossed (e.g. the first
  sentence / first N characters / first newline) **or** (b) generation
  finishes, run the cheap guards (`looks_like_clarification`,
  `looks_like_degenerate_cleanup`) on that prefix, and only then begin
  streaming the buffered prefix + subsequent deltas. This preserves the
  clarification/degenerate fallback (those fire on the *opening* tokens, which
  the prefix captures) while still starting injection far earlier than
  today. Document that the *translation* guard
  (`looks_like_translated_cleanup`) is inherently whole-output and either (i)
  downgrade it to "best-effort on the buffered prefix" or (ii) keep it only on
  the non-streaming path and disable streaming when a hard source-language
  contract is active. Rationale: the opener-based guards are satisfiable on a
  short prefix; the language guard needs enough text and is the one that must
  be explicitly traded off.

- [ ] Task 5. **Add a stream sink that injects deltas on safe boundaries.**
  Introduce an injection consumer that accumulates deltas and flushes to the
  injector on word/whitespace boundaries (not mid-token), so the user never
  sees half-words flicker. Reuse or generalise the sentence-splitter pattern
  from the assistant/TTS path (`fono-tts/src/sentence_split.rs`) for boundary
  detection. For subprocess backends (`wtype`/`ydotool`/`xdotool`), batch
  deltas into reasonably-sized chunks to avoid spawning a process per token;
  for XTEST/enigo, per-word is fine. Rationale: keeps typing visually smooth
  and bounds process-spawn overhead.

- [ ] Task 6. **Gate streaming behind config + capability checks.**
  Add a `[polish].stream_injection` config flag (default decision documented in
  Task 4 outcome). Disable streaming automatically when: the inject path is the
  **clipboard fallback** (no key-injection backend — streaming to a clipboard
  is meaningless; `session.rs:512` / `inject.rs:166`), the utterance is below
  `skip_if_words_lt` (cleanup already skipped, `session.rs:3874`), or the
  active backend only has the default `format()` (no real stream). Rationale:
  streaming must degrade gracefully to today's one-shot behaviour wherever it
  doesn't apply.

- [ ] Task 7. **Decide history + clipboard semantics for streamed text.**
  The history row and the belt-and-suspenders clipboard copy
  (`also_copy_to_clipboard`, around `session.rs:4004`) currently use the final
  cleaned string. With streaming, accumulate the full streamed output and use
  *that* assembled string for the history write and clipboard copy after the
  stream completes. Rationale: streaming changes only *when* characters reach
  the cursor, not the recorded artefact.

- [ ] Task 8. **Handle mid-stream failure / cancellation.**
  Define behaviour when the stream errors partway (network drop on cloud, user
  presses cancel hotkey, model stalls): text already typed stays (cannot be
  retracted); log + optionally notify; do **not** then also inject the raw
  transcript on top (that would duplicate). Wire into the existing
  cancellation `Notify` used by the live/assistant paths. Rationale: partial
  injection is a new failure mode that must be explicitly specified, not left
  to chance.

- [ ] Task 9. **Update metrics + tracing.**
  Add a time-to-first-injected-char metric alongside the existing `llm_ms` /
  `inject_ms` (`PipelineMetrics`, `session.rs:366`), analogous to the
  assistant's `llm_ttfb_ms` / `tts_ttfa_ms`. Rationale: the whole point is
  responsiveness; we need to measure the TTFB win and confirm it in traces.

- [ ] Task 10. **Tests.** Unit-test the stream sink boundary flushing and the
  bounded-prefix guard gate (clarification/degenerate prefixes must still
  fall back to raw and inject nothing-or-raw, never the refusal). Extend the
  pipeline integration test (`crates/fono/tests/pipeline.rs`) with a fake
  streaming formatter + a recording injector that captures the sequence of
  delta writes, asserting order and that guard-triggering outputs do not leak.
  Rationale: the irreversibility of injection makes regressions
  user-visible and unrecoverable, so guard behaviour under streaming needs
  explicit coverage.

## Verification Criteria

- Time-to-first-injected-character on a long local-backend dictation drops
  from ≈ full `llm_ms` to under ~1 s (measured via the new metric / trace).
- A clarification-style or role-token-degenerate cleanup output never reaches
  the cursor; the pipeline still falls back to raw (or injects nothing)
  exactly as it does today.
- Short utterances (< `skip_if_words_lt`) and clipboard-fallback sessions
  behave identically to current one-shot behaviour.
- Final history row and clipboard copy contain the same assembled text a
  non-streaming run would have produced for a clean output.
- No half-word/partial-token flicker is visible during typing.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
  warnings`, and `cargo test --workspace --tests --lib` all pass.

## Potential Risks and Mitigations

1. **Irreversible early injection of bad output (clarification / translation /
   garbage).**
   Mitigation: bounded-prefix gate (Task 4) — never inject until the opener
   guards have inspected a safety prefix; explicitly trade off the
   whole-output translation guard and document it.
2. **Half-words / token flicker degrading the typing experience.**
   Mitigation: flush only on word/whitespace boundaries via a sentence/word
   splitter (Task 5).
3. **Process-spawn storm for subprocess inject backends (wtype/ydotool/
   xdotool) if flushed per token.**
   Mitigation: batch deltas into chunks for subprocess backends; reserve
   per-word for in-process XTEST/enigo (Task 5).
4. **Partial injection on mid-stream error/cancel leaves orphaned text.**
   Mitigation: specify "keep typed prefix, do not append raw on top, log/notify"
   and wire cancellation (Task 8).
5. **Clipboard fallback path has no meaningful streaming target.**
   Mitigation: auto-disable streaming whenever the injector is the clipboard
   fallback (Task 6).
6. **Reduced benefit for fast cloud + short text** (whole reply already arrives
   in <300 ms), so streaming adds complexity for little gain there.
   Mitigation: config flag + capability gate; streaming primarily targets
   local backends and long dictations.
7. **Prompt-state cache / sampler / stop-sequence regressions when refactoring
   the local decode loop into a stream.**
   Mitigation: keep `format()` as the canonical path that consumes the same
   stream internally; share one decode core so cache/sampler logic isn't
   duplicated (Task 3).

## Alternative Approaches

1. **Chunk-by-sentence (not token-by-token).** Buffer until a sentence
   boundary, run guards per sentence, inject whole sentences. Simpler boundary
   handling and naturally guard-friendly, at the cost of coarser
   responsiveness (first sentence still waits for a full sentence). Good
   middle ground and arguably the lowest-risk first increment.
2. **Optimistic streaming with sentinel-based abort.** Stream everything
   immediately but, if a guard fires after the fact, attempt to select-all /
   delete the typed text and re-type the raw transcript. Rejected as default:
   relies on synthesising destructive editing keystrokes into an arbitrary
   focused app — fragile, app-specific, and risks deleting unrelated user
   content. Violates the project's "no irreversible surprises" posture.
3. **Streaming only for local backends.** Restrict the feature to
   `llama_local` (the real latency pain) and leave cloud non-streaming.
   Smallest surface area, captures most of the user-perceived win, defers SSE
   work. Viable as a phase-1 scope cut.
4. **Status quo + better "polishing" feedback.** Keep one-shot injection but
   improve the overlay animation so the wait feels shorter. Cheapest, no
   injection-correctness risk, but does not deliver the actual responsiveness
   the user asked for.
