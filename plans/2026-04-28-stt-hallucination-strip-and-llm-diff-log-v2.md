# Fix Whisper closer-hallucinations at the source — layered defence

## Objective

Stop "Thank you", "Bye", "Thanks for watching" from leaking into the
cursor, and surface what the LLM cleanup actually changed so users
can judge whether cleanup is pulling its weight on their model.
Attack the root cause (Whisper inference parameters + initial prompt
+ tighter VAD), not the symptom (regex strip).

## Diagnosis

Whisper hallucinates polite closings on silent / low-volume tails
(training-corpus artefact: YouTube + podcasts dominate the closer
distribution; documented in Radford et al. 2022 and discussed at
length in the whisper.cpp issue tracker).

Audit of the current code base reveals **three separate
contributing factors**, none of which are mitigated today:

1. `whisper-rs::FullParams::new()` initialises `no_speech_threshold`,
   `logprob_threshold`, and `compression_ratio_threshold` to disabled
   sentinels. We never call the setters, so Whisper local runs
   without any of the canonical hallucination guards.
   See `crates/fono-stt/src/whisper_local.rs:123-135` and `:497-509`.

2. We never pass an `initial_prompt` to Whisper (local or cloud).
   The model has no style or vocabulary anchor, so it falls back on
   the dominant training-distribution prior — which for silent tails
   is YouTube-style closers.

3. `crates/fono-audio/src/trim.rs` uses static RMS-based silence
   detection with `pad_ms = 60`. Background noise (fan, breath,
   distant typing) often registers above the RMS threshold, so the
   trim stops earlier than the true word boundary; whatever silence
   remains becomes Whisper input. Plus the `interactive
   .hold_release_grace_ms = 300` adds a deliberate silent buffer
   so cpal's audio callback can drain — necessary, but it lengthens
   the silent tail.

The LLM cleanup is at most a *secondary* offender — with the
hardened "echo verbatim if unclear" prompt it generally preserves
rather than introduces closers, but it dutifully keeps whatever
Whisper invented.

## Implementation Plan

### Layer A — Wire Whisper local hallucination guards (root cause, biggest win)

- [ ] Task A1. In `crates/fono-stt/src/whisper_local.rs`, in **both**
  the streaming and non-streaming `FullParams` configurations
  (around lines 123 and 497), add:
  ```
  params.set_no_speech_thold(0.6);
  params.set_logprob_thold(-1.0);
  params.set_compress_thold(2.4);
  params.set_temperature(0.0);
  params.set_temperature_inc(0.2);
  ```
  These are whisper.cpp's canonical defaults, not magic numbers.
  Doc-comment block above each setter explaining what each guard
  catches (no_speech: pure-silence segments, logprob: low-confidence
  garbage, compress: "you you you you" infinite-loop hallucinations,
  temperature_inc: fallback retry with more variance instead of
  immediate drop).

- [ ] Task A2. Make the four guards configurable via a new
  `[stt.local.hallucination_guards]` config section with sensible
  whisper.cpp defaults. Power users with quiet mics may want
  `no_speech_thold = 0.4`; users with noisy mics may want 0.8.
  Range-validate at config load.

### Layer B — Initial prompt support (root cause, biggest cross-backend win)

- [ ] Task B1. Add new top-level `[stt] prompt = "..."` config field
  with a default of:
  ```
  Professional dictation. Output exactly what the speaker says
  with proper punctuation and capitalization.
  ```
  Doc-comment cites the Whisper paper's initial-prompt mechanism
  and warns about the 256-token max.

- [ ] Task B2. Plumb the prompt through `SpeechToText::transcribe`
  (extend `Transcription` request signature, or thread via a
  `TranscriptionContext` that already exists for language). The
  prompt is request-scoped, not backend-scoped, so it can vary
  per dictation in future (app-context-aware prompts when focused
  on a code editor vs an email client).

- [ ] Task B3. Wire prompt to `whisper-rs::FullParams::set_initial_prompt`
  in `whisper_local.rs:128` and `:501`.

- [ ] Task B4. Wire prompt to Groq's `prompt` form-data field in
  `crates/fono-stt/src/groq.rs::groq_post_wav` and
  `groq_post_wav_verbose`. Same wiring in
  `crates/fono-stt/src/groq_streaming.rs` for the preview and
  finalize POSTs.

- [ ] Task B5. Wire prompt to OpenAI's `prompt` form-data field in
  `crates/fono-stt/src/openai.rs`.

- [ ] Task B6. App-context-aware prompts (optional follow-up, NOT in
  this PR): inject a code-flavoured prompt when `app_class` matches
  a configured editor list (`code`, `nvim`, `Cursor`, `kate`, etc.).
  Tracked separately.

### Layer C — Reduce silent-tail window (secondary defence)

- [ ] Task C1. In `crates/fono-core/src/config.rs`, lower the default
  `interactive.hold_release_grace_ms` from `300` to `150`. Halves
  the deliberate silent buffer. Test that trailing-word truncation
  does not regress (your earlier bug from a few sessions back).

- [ ] Task C2. In `crates/fono-audio/src/trim.rs`, lower the default
  `pad_ms` from `60` to `20` and add a hysteresis option (`require
  N silent frames in a row to mark end-of-speech`, default `5`).
  Hysteresis prevents one stray loud frame in a sea of silence
  from extending the speech region.

- [ ] Task C3. (Optional, later PR.) Replace the static RMS
  detection in `trim.rs` with a real VAD (webrtc-vad has Rust
  bindings, ~10 KB binary impact, zero dependencies). Properly
  distinguishes speech from background noise. Track as a separate
  plan.

### Layer D — LLM cleanup observability (independent of hallucination fix)

- [ ] Task D1. At `crates/fono/src/session.rs:1265`, after the
  existing `llm: {} {}ms → {} chars` INFO line, add a one-line
  diff summary: `llm: cleanup +N -M chars (or "no-op" when raw
  == cleaned)`. Use a simple character-set or Levenshtein
  comparison; capped utterance length means O(n*m) is sub-ms.

- [ ] Task D2. Bonus DEBUG log on `target: "fono::pipeline"` showing
  the actual before / after when they differ. Gated to debug
  because of transcript content. Operator opt-in via
  `RUST_LOG=fono::pipeline=debug`.

### Layer E — Strip-list band-aid (last-resort, opt-in only)

- [ ] Task E1. New `crates/fono-stt/src/hallucinations.rs` with
  `strip_closer(text) -> (cleaned, removed)` and a curated list of
  ~25 well-known Whisper closers (English-only at first;
  non-English tracked as later phase). Match is suffix-only,
  case-insensitive, requires sentence-terminator-or-start-of-string
  before the match (no false positive on "I would like to thank
  you for the report").

- [ ] Task E2. Wire `strip_closer` into both batch
  (`session.rs:1206`) and live (`session.rs:1109`) pipelines.

- [ ] Task E3. New `general.strip_whisper_hallucinations` config
  defaulting to `false` — opt-in only. Safety net for users where
  layers A-C don't fully suppress hallucinations on their
  particular hardware / mic / room.

- [ ] Task E4. INFO log `stt: stripped Whisper hallucination at
  tail: "..."` when strip fires, so the user sees what was cut and
  can disable the strip if it's biting them.

### Verification & docs

- [ ] Task F1. New unit tests:
  - 9 in `hallucinations.rs` (covered in earlier plan)
  - 4 in `whisper_local.rs` mocking `FullParams` setter calls to
    confirm the four guards are applied
  - 4 in `groq.rs` / `openai.rs` confirming prompt is included in
    the form-data when configured

- [ ] Task F2. Integration test in `crates/fono/tests/pipeline.rs`:
  feed a mock STT that returns "Hello world. Thank you." and assert
  the cleaned output behaves correctly under each config.

- [ ] Task F3. Update `docs/troubleshooting.md` with a new section
  "Whisper appends 'Thank you' / 'Bye' to my dictation" walking
  through the layered defence (configurable guards, initial prompt,
  trim tightening, opt-in strip list).

- [ ] Task F4. CHANGELOG `### Fixed` entry and `### Added` entry for
  the new config fields.

- [ ] Task F5. Build / fmt / clippy / test verify. No version bump.
  No tag. Operator runs smoke tests.

## Verification Criteria

- After Layer A only: hallucinations drop substantially on local
  Whisper. Verified by repeating the F8-with-silent-tail scenario
  10 times and counting closer-leaks.
- After Layer A + B + C1: hallucinations on local Whisper drop to
  near-zero in normal conditions. Cloud Groq sees ~50 % reduction
  from the initial prompt alone.
- After Layer A + B + C1 + C2: hallucinations on cloud Groq drop
  substantially because the silent tail is shorter and the
  initial prompt biases away from closers.
- LLM cleanup INFO log makes it obvious when the LLM is doing real
  work (e.g. small models doing meaningful punctuation) vs when
  it's a no-op pass-through (large models on already-clean STT
  output).
- Existing 196 unit tests + new tests all pass; clippy and fmt
  clean.

## Potential Risks and Mitigations

1. **`no_speech_thold = 0.6` aggressively drops legitimate
   short/quiet speech** ("yes", "okay", whispered confirmations).
   Mitigation: configurable via [stt.local.hallucination_guards];
   default matches whisper.cpp's documented default; users with
   quiet-speaker / noisy-room can tune.

2. **`initial_prompt` bleeds prompt-flavour into transcriptions**
   (over-long or over-creative prompts can cause Whisper to
   "complete" the prompt rather than transcribe).
   Mitigation: ship a short, neutral default ("Professional
   dictation. Output exactly..."). Doc-comment warns about the
   256-token cap and the bleed risk. Easy to disable
   (`prompt = ""`).

3. **Tighter `pad_ms = 20` clips consonants** (/t/, /k/, /p/) at
   word ends.
   Mitigation: hysteresis (5 silent frames in a row required) gives
   us back the same effective margin as 60ms-pad without the bias.
   Configurable. Existing test `trims_leading_and_trailing_silence`
   in `fono-audio/src/trim.rs` covers this, just needs new fixtures.

4. **Reducing `hold_release_grace_ms = 300 → 150` reintroduces the
   trailing-word truncation bug.**
   Mitigation: this is exactly the value the user reported as a
   problem before. We may need to keep 300 as a safe default and
   address tail-truncation a different way (maybe by signalling
   capture-stop via the cpal callback itself rather than via a
   timer). Smoke-test before committing the default change. If
   regression hits, leave at 300 and rely on layers A + B + C2.

## Alternative Approaches

1. **Just ship the regex strip-list** (the original plan v1).
   Simple, no new APIs, narrow risk surface. But it's a band-aid
   that doesn't reduce inference cost (Whisper still computes the
   hallucinated tokens, we just hide them). Reject as primary
   solution; keep as opt-in safety net (Layer E).

2. **Switch to a non-Whisper STT family** (e.g. Deepgram, Speechmatics).
   Different models have different hallucination behaviour.
   Out-of-scope: would mean abandoning the Whisper ecosystem we've
   built around.

3. **Send each utterance through a lightweight classifier** ("does
   the tail of this transcription look like a generative artefact?").
   Overkill; Whisper's own no_speech_thold and logprob_thold are
   already the model's self-classification of confidence. Use those.

4. **Ship the LLM cleanup diff log first** as a smaller standalone
   PR, then come back for the hallucination work.
   Reasonable if you want quick observability before the bigger
   surgery; trade-off is two PRs to review instead of one.
