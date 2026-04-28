# Strip Whisper closer-hallucinations + log LLM cleanup diff

## Objective

Stop "Thank you", "Bye", "Thanks for watching" etc. from leaking into
the cursor at end of dictation, and surface what the LLM cleanup
actually changed so the user can judge whether cleanup is pulling its
weight on their chosen model.

## Background

Whisper hallucinates polite closers on silent tails (training-corpus
artefact, documented in Radford et al. 2022). Fono's
`interactive.hold_release_grace_ms` (300 ms default) makes this
reliably reproducible because the grace period is silence by
construction.

The LLM cleanup, with the hardened "echo verbatim if unclear" prompt,
generally preserves rather than introduces these — but the user has
no visibility into how much the LLM is actually changing the text.
On large models (gpt-4o, claude-3-5-sonnet) cleanup may be redundant;
on small ones (claude-haiku, llama-3.2-1b) it may be doing real work.
Log the diff and let the user decide.

## Implementation Plan

- [ ] Task 1. New `crates/fono-stt/src/hallucinations.rs` module exposing
  `pub fn strip_closer(text: &str) -> (String, Option<String>)`. Returns
  the cleaned text and (if a closer was stripped) the exact removed
  substring so the caller can log it. Matching is case-insensitive,
  ignores trailing punctuation/whitespace, and only fires when the
  closer is preceded by a sentence-terminator (`.`, `!`, `?`, `…`, `,`)
  OR is the entire utterance — protects against false positives like
  "I would like to thank you for the report".

- [ ] Task 2. Curated phrase list (~25 entries) in `HALLUCINATIONS`,
  ordered longest-first so prefix matching does not chop a longer
  phrase into a shorter one. Initial list:
  "thanks for watching [everyone|and see you next time]",
  "thank you for watching", "thanks for listening",
  "thank you for listening", "see you next time",
  "see you in the next [one|video]", "see you guys next time",
  "see you later", "thank you so much", "thank you very much",
  "thanks a lot", "thank you", "thanks", "goodbye", "bye bye", "bye",
  "subscribe", "please subscribe", "like and subscribe",
  "don't forget to subscribe". English-only at first; non-English
  Whisper hallucinations (e.g. "Untertitelung des ZDF" in German,
  "Sous-titrage" in French) tracked as Phase 2.

- [ ] Task 3. Wire `strip_closer` into the pipeline at
  `crates/fono/src/session.rs:1206` immediately after the `let raw =
  trans.text.trim().to_string()` line. When a closer is stripped, log
  at INFO: `stt: stripped Whisper hallucination at tail: "Thank you"`.
  Apply BEFORE the `raw.is_empty()` empty-skip check so a
  closer-only utterance (Whisper transcribed pure silence as just
  "Thank you") becomes empty and falls through to `EmptyOrTooShort`.

- [ ] Task 4. Same wiring in the live-mode finalize lane at
  `crates/fono/src/session.rs:1109` (around the live pipeline result
  injection). Cleanup must apply identically to F8 batch and F8/F9
  live so the user experience matches.

- [ ] Task 5. Add `general.strip_whisper_hallucinations: bool` config
  defaulting to `true`. Power users who legitimately end every
  dictation with "Thanks" can opt out. Doc-comment cites the reason
  for the default and notes the known-failure-mode rationale.

- [ ] Task 6. New LLM cleanup diff log. At
  `crates/fono/src/session.rs:1265` (where `tracing::debug!` already
  logs `llm.output: {trimmed:?}`), add an INFO line right after the
  existing `llm: {} {}ms → {} chars` summary that summarises the diff:
  `llm: cleanup added=N removed=M chars (or "no-op" when input ==
  output)`. Use a small inline `summarise_diff(raw, cleaned) ->
  (added: usize, removed: usize)` helper based on byte-level edit
  distance (Levenshtein) — it's already a transitive dep via
  `fono-bench`'s equivalence harness. If pulling Levenshtein into the
  hot path is undesirable, fall back to character-set difference
  cardinality which is cheaper and good enough for the "is the LLM
  doing anything?" question.

- [ ] Task 7. Bonus DEBUG-level diff dump on `target: "fono::pipeline"`
  showing the actual before/after text when they differ — gated to
  debug because it can leak transcript content and is verbose.
  Operator can opt in with `RUST_LOG=fono::pipeline=debug`.

- [ ] Task 8. Unit tests in `hallucinations.rs`: 9 cases covering
  strips_thank_you_after_sentence, strips_thanks_for_watching,
  strips_bye, keeps_legit_thank_you_mid_utterance,
  keeps_thank_you_when_part_of_sentence, handles_only_hallucination,
  handles_empty, case_insensitive, strips_ellipsis_then_phrase.

- [ ] Task 9. Integration test in `crates/fono/tests/pipeline.rs`:
  feed a mock STT that returns "Hello world. Thank you." and assert
  the injected text is "Hello world." with the INFO log line present
  via tracing-test fixture.

- [ ] Task 10. Docs: CHANGELOG entry under `### Fixed` referencing the
  Whisper paper failure mode; `docs/troubleshooting.md` new section
  "Whisper appends 'Thank you' / 'Bye' to my dictation" explaining
  the mechanism and the new strip + opt-out flag.

- [ ] Task 11. Build / fmt / clippy / test verify. No version bump
  (operator runs smoke tests and tags v0.3.4 or v0.3.5 once the
  notification audit + this fix have both been validated). No tag,
  no push.

## Verification Criteria

- `target/debug/fono` with `RUST_LOG=fono=info` shows
  `stt: stripped Whisper hallucination at tail: "Thank you"` when
  Whisper's output ends with a closer; no log line when the raw text
  is clean.
- The cleaned text injected at the cursor never ends with any of the
  25 catalogued closers (case-insensitive, after stripping
  punctuation).
- "I would like to thank you for everything" survives unchanged
  (mid-utterance match).
- An utterance that is ONLY a hallucination (Whisper transcribed pure
  silence) becomes empty and falls through to EmptyOrTooShort
  instead of injecting the closer.
- `general.strip_whisper_hallucinations = false` in config bypasses
  the strip entirely.
- INFO log shows `llm: cleanup added=N removed=M chars` after every
  cleanup invocation, allowing the user to see at a glance whether
  the LLM is doing real work or operating as a no-op pass-through.
- 9 new unit tests + 1 integration test all pass; existing 196 tests
  still pass.
- clippy `--workspace --all-targets -- -D warnings` clean; fmt clean.

## Potential Risks and Mitigations

1. **False positive: a user actually says "Thank you" at the end of
   their dictation.**
   Mitigation: opt-out config flag `general.strip_whisper_hallucinations`.
   The INFO log line lets the user notice the strip is happening so
   they can disable it if it's biting them. Worst case (flag off),
   behaviour is exactly today's. Dictation that ends with "Thank
   you for the prompt response, regards." survives because the
   match requires the closer to be the *suffix*, not embedded.

2. **Phrase list goes stale: Whisper updates introduce new
   hallucinations.**
   Mitigation: phrase list is a single `const HALLUCINATIONS:
   &[&str]` — easy to extend without touching call sites. Future
   work could load it from a config-defined extra list so users can
   add language-specific closers without a code change.

3. **Levenshtein on every cleanup adds latency on long dictations.**
   Mitigation: the diff summary runs only when log level >= INFO and
   uses byte-level diff with O(n*m) complexity but with `n, m` capped
   by the utterance length (typically <500 chars; Levenshtein at that
   scale is sub-millisecond). If profiling shows it's hot, fall back
   to the cheaper character-set cardinality difference.

4. **Stripped-only utterances being silently dropped confuses
   users.**
   Mitigation: the EmptyOrTooShort path already plays the "nothing
   to inject" sound and logs the reason. The new INFO log line
   above tells the user *why* it became empty.

## Alternative Approaches

1. **VAD-based hard tail trim before STT.** Trim trailing silence
   in `fono-audio/src/trim.rs` more aggressively so Whisper never
   sees the silent tail. Lower-level fix but harder to tune
   (aggressive trim chops legitimate trailing words; conservative
   trim leaves the hallucination window). This complements rather
   than replaces the hallucination strip.

2. **`logprob_threshold` tuning on Whisper local.** Whisper exposes
   per-segment log-probability; segments below a threshold are
   filtered. Documented in whisper.cpp as the canonical knob for
   this exact failure. Doesn't apply to cloud STT (Groq doesn't
   expose this on whisper-large-v3-turbo). Worth doing for the
   local path as a secondary defence.

3. **Prompt the LLM to remove closers in cleanup.** Add to the
   `default_prompt_main`: "Remove any 'thank you' / 'bye' /
   'thanks for watching' that appears at the very end and looks
   like a transcription artefact." Risk: the LLM is then making
   judgement calls about user intent, which conflicts with the
   "echo verbatim if unclear" rule we already shipped. Reject.

4. **Keep the closer but visually mark it for user review.** E.g.
   inject "Hello world. [Thank you]" so the user can manually
   delete the bracketed part. Adds friction; users want clean
   text. Reject.
