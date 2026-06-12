# Streaming Cleanup Injection — Local-Only, Guard-Preserving (v2)

## Objective

Narrowed scope per user decision: streaming injection applies to the **local
cleanup backend only** (`crates/fono-polish/src/llama_local.rs`), and **all
three safety guards must be preserved**. Deliver a first-words-typed latency of
~1–3 s on long dictations (vs. 7–20 s today at 5–15 tok/s) without weakening
the raw-transcript fallback.

## The Chosen Compromise: First-Sentence Gate + Sentence-Cadence Streaming

**Why this is the best trade-off:** all three guard failure modes manifest in
the *opening* tokens of the output, so a buffered first sentence is enough to
run every guard at (effectively) full strength:

1. `looks_like_clarification` (`crates/fono-polish/src/traits.rs:147`) keys on
   telltale **openers** ("It seems like you…", "Could you provide…") — by
   construction it is decidable on the first sentence.
2. `looks_like_degenerate_cleanup` (`traits.rs:213`) fires only when the
   **entire** output is a single role token ("model"). If the stream produces
   more than one word, the guard could never fire on the full output either —
   so it is fully decided by the time the first sentence (or stream end,
   whichever comes first) arrives.
3. `looks_like_translated_cleanup` (`traits.rs:232`) needs ≥24 alphabetic
   chars / ≥4 words (`traits.rs:289-295`). A model that translates does so
   from the first word, not midway — so detection on the first sentence
   catches the realistic failure mode. If the first sentence is shorter than
   the guard's minimum-text threshold, **extend the buffer** until the
   threshold is met or the stream ends.

After the gate passes, deltas flow to the injector at **sentence cadence**
(flush on sentence boundary, falling back to word boundary for very long
sentences), with a cheap per-sentence language re-check as a drift tripwire:
if a later sentence reliably detects as a different language, **stop typing**
(keep what's typed, log + notify, do not append raw on top).

**Latency math:** a typical first sentence is ~10–20 tokens. At 5–15 tok/s
local decode that is ~1–3 s to first injected words, versus waiting the full
7–20 s today. Subsequent sentences appear as they decode, so the user reads
along while the model works.

## Implementation Plan

- [ ] Task 1. **Expose the local decode loop as a delta stream.** Refactor
  `llama_local.rs`'s token loop so it yields decoded text chunks through a
  channel/stream while preserving the prompt-state cache, sampler,
  stop-sequence, retry-on-degenerate, and `MAX_NEW_TOKENS` logic. Keep
  `format()` as a consumer of the same core so non-streaming callers and the
  cold-retry path share one implementation. Rationale: one decode core, no
  duplicated cache/sampler logic.

- [ ] Task 2. **Add `format_stream` to `TextFormatter` with a one-shot default.**
  Default impl calls `format()` and yields a single chunk; only `LlamaLocal`
  overrides it. Cloud backends are untouched (they stay one-shot — their
  sub-second latency doesn't need streaming). Rationale: minimal trait surface,
  zero churn for cloud/anthropic backends.

- [ ] Task 3. **Implement the first-sentence guard gate in the orchestrator.**
  In `run_pipeline` (`crates/fono/src/session.rs:3918` region) and the live
  path (`session.rs:3545`), when the active backend is local and streaming is
  enabled: buffer deltas until (a) a sentence boundary AND the
  translation-guard minimum-text threshold are both reached, or (b) the stream
  ends. Run all three guards on the buffered prefix. On any guard hit: discard
  the stream, fall back to raw exactly as today (the existing in-backend guard
  handling for the non-streaming path remains for `format()` callers).
  Rationale: nothing reaches the cursor until the prefix is vetted.

- [ ] Task 4. **Sentence-cadence injection sink.** After the gate passes,
  inject the buffered prefix, then flush each subsequent completed sentence
  (reuse the boundary logic pattern from
  `crates/fono-tts/src/sentence_split.rs`). Word-boundary fallback flush for
  run-on sentences (e.g. every ~80 chars without a terminator). Sentence-sized
  chunks also keep subprocess injector spawns (wtype/ydotool/xdotool) to a
  handful per dictation. Rationale: smooth visual output, bounded spawn
  overhead, natural units for the drift tripwire.

- [ ] Task 5. **Per-sentence language drift tripwire.** Before flushing each
  post-gate sentence, run the same whatlang reliable-detection check used by
  `looks_like_translated_cleanup` against the expected source language; on a
  reliable mismatch, stop the stream: keep typed text, skip the rest, log and
  surface a one-shot notification. Never inject raw on top of partial typed
  text. Rationale: closes the (rare) mid-output drift hole the prefix gate
  cannot see, at trivial cost.

- [ ] Task 6. **Gating and config.** New `[polish].stream_injection` flag
  (suggested default: `true` for local backend, ignored elsewhere). Auto-fall
  back to one-shot behaviour when: backend is not local, injector resolves to
  the clipboard fallback (`session.rs:512`), the utterance is below
  `skip_if_words_lt` (`session.rs:3874`), or the live-preview/Transcript path
  semantics conflict. Rationale: graceful degradation everywhere streaming
  doesn't apply.

- [ ] Task 7. **History, clipboard, metrics, cancellation.** Accumulate the
  full streamed text for the history row and `also_copy_to_clipboard`
  (`session.rs:4004` region) so artefacts match a non-streaming run. Add a
  time-to-first-injected-char metric beside `llm_ms`/`inject_ms`
  (`PipelineMetrics`, `session.rs:366`). On cancel/error mid-stream: keep
  typed prefix, do not retype, mark the outcome. Rationale: streaming changes
  *when* characters land, not what Fono records.

- [ ] Task 8. **Tests.** Unit tests: gate holds until sentence+threshold; each
  guard-triggering prefix (clarification opener, bare role token, translated
  opening) yields raw-fallback with zero injected deltas; drift tripwire stops
  mid-stream. Integration test in `crates/fono/tests/pipeline.rs` with a fake
  streaming formatter + recording injector asserting delta order and
  non-leakage. Rationale: injection is irreversible; guard regressions must be
  caught in CI, not at the cursor.

## Verification Criteria

- First injected characters within ~1–3 s on a long local-cleanup dictation
  (new TTFI metric in traces), vs. ≈`llm_ms` today.
- All three guards behave identically to the non-streaming path for outputs
  that fail in the opening tokens: nothing typed, raw fallback injected.
- Mid-stream language drift stops typing without appending raw text.
- Cloud backends, short utterances, and clipboard-fallback sessions are
  byte-for-byte identical to current behaviour.
- History row and clipboard contents match what a non-streaming run produces.
- fmt / clippy / test pre-commit gate passes.

## Residual Risks (accepted, with rationale)

1. **Mid-output drift after a clean first sentence** — mitigated by the
   per-sentence tripwire (Task 5); worst case is a truncated-but-correct
   prefix at the cursor, never a translated tail.
2. **A guard hit after partial typing is impossible by construction** for the
   opener guards (decided at the gate) and degenerate guard (decided at the
   gate); only drift remains, covered above.
3. **Slightly later first words on very short outputs** (buffer must reach the
   translation guard's text minimum) — acceptable: short outputs finish fast
   anyway, and `skip_if_words_lt` already bypasses cleanup for the shortest.

## Alternatives Considered and Set Aside

1. **Token/word-cadence streaming with prefix gate** — marginally snappier
   visuals, but more injector spawns and a noisier drift surface; sentence
   cadence is the better default. Could be a later config option.
2. **Optimistic stream + delete/retype on guard hit** — rejected: synthesising
   destructive edits into arbitrary apps is fragile and violates the
   no-irreversible-surprises posture.
3. **Cloud streaming too** — deferred: sub-second cloud latency makes the
   complexity unjustified; the trait default keeps the door open.
