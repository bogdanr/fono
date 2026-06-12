# Streaming Cleanup Injection — Local-Only, First-Sentence Gate, Word Streaming (v3)

## Objective

Final simplified design per user decisions: streaming injection for the
**local cleanup backend only**, all three guards preserved via a
**first-sentence gate**, and after the gate **word-by-word streaming** with
**no per-sentence language drift tripwire** (dropped — mid-output language
flips do not occur in practice; the realistic translation failure starts at
the first word and is caught by the gate).

Target: first injected words in ~1–3 s on long dictations (vs. 7–20 s today at
local 5–15 tok/s), then continuous word-by-word typing while the model decodes.

## Design Summary

1. **Gate (unchanged from v2):** buffer deltas until a sentence boundary AND
   the translation guard's minimum-text threshold (≥24 alpha chars / ≥4 words,
   `crates/fono-polish/src/traits.rs:289-295`) are both reached — or the
   stream ends, whichever first. Run all three guards on the buffered prefix:
   - `looks_like_clarification` (`traits.rs:147`) — opener-based, decidable on
     the prefix by construction.
   - `looks_like_degenerate_cleanup` (`traits.rs:213`) — whole-output
     single-role-token; decided as soon as a second word exists.
   - `looks_like_translated_cleanup` (`traits.rs:232`) — a translating model
     translates from the first word; prefix detection covers the real case.
   Guard hit ⇒ discard stream, fall back to raw exactly as today, zero
   characters typed.
2. **Post-gate flow (simplified):** inject the buffered prefix, then flush
   each completed **word** (whitespace boundary) as it decodes. No further
   guard checks. Word cadence at 5–15 tok/s is a few injector calls per
   second — negligible even for subprocess backends (wtype/ydotool/xdotool
   spawn cost ~1–5 ms). Never flush partial words/tokens.

## Implementation Plan

- [x] Task 1. **Expose the local decode loop as a delta stream.** Refactor
  `crates/fono-polish/src/llama_local.rs`'s token loop to yield decoded text
  chunks while preserving prompt-state cache, sampler, stop-sequence,
  degenerate-retry, and `MAX_NEW_TOKENS` logic. `format()` consumes the same
  core so non-streaming callers share one implementation.

- [x] Task 2. **Add `format_stream` to `TextFormatter` with a one-shot
  default.** Default impl wraps `format()` as a single chunk; only
  `LlamaLocal` overrides. Cloud backends untouched.

- [x] Task 3. **First-sentence guard gate in the orchestrator.** In
  `run_pipeline` (`crates/fono/src/session.rs:3918` region) and the live path
  (`session.rs:3545`): when backend is local and streaming enabled, buffer to
  the gate condition, run the three guards, and on a hit fall back to raw with
  nothing typed.

- [x] Task 4. **Word-boundary injection sink.** After the gate: inject the
  prefix, then flush on each whitespace boundary. Carry trailing partial-word
  bytes until completed or stream end. No sentence splitter needed post-gate.

- [x] Task 5. **Gating and config.** `[polish].stream_injection` flag
  (default `true`, meaningful only for the local backend). Auto-fallback to
  one-shot when: backend not local, injector resolved to clipboard fallback
  (`session.rs:512`), utterance below `skip_if_words_lt` (`session.rs:3874`),
  or no real stream available.

- [x] Task 6. **History, clipboard, metrics, cancellation.** Accumulate full
  streamed text for the history row and `also_copy_to_clipboard`
  (`session.rs:4004` region). Add time-to-first-injected-char metric beside
  `llm_ms`/`inject_ms` (`PipelineMetrics`, `session.rs:366`). On mid-stream
  cancel/error: keep typed prefix, never retype or append raw on top.

- [x] Task 7. **Tests.** Unit: gate hold conditions; each guard-triggering
  prefix yields raw fallback with zero injected deltas; word-boundary sink
  never emits partial words. Integration (`crates/fono/tests/pipeline.rs`):
  fake streaming formatter + recording injector asserting delta order and
  non-leakage.

## Verification Criteria

- First injected characters within ~1–3 s on long local-cleanup dictations
  (new TTFI metric).
- Guard-failing outputs type nothing and fall back to raw, identical to the
  non-streaming path.
- Post-gate typing is whole words only, continuous, no flicker.
- Cloud backends, short utterances, clipboard-fallback sessions, history row,
  and clipboard copy are byte-identical to current behaviour.
- fmt / clippy / test pre-commit gate passes.

## Residual Risks (accepted)

1. **Mid-output language drift after a clean first sentence** — accepted
   without mitigation per user decision; never observed in practice, and the
   whole-output translation failure is caught at the gate. Worst hypothetical
   case: a few drifted words at the cursor, recoverable by the user.
2. **Injector-call overhead at word cadence** — measured-negligible at local
   decode speeds; if a pathological backend/host shows cost, batching words is
   a one-line chunk-size change in the sink.
3. **Decode-loop refactor regressions (cache/sampler/stop)** — mitigated by
   sharing one decode core between `format()` and `format_stream` (Task 1).

## Dropped from v2

- Per-sentence language drift tripwire (Task 5 in v2) — removed.
- Sentence-cadence post-gate flushing — replaced by word-boundary flushing.
