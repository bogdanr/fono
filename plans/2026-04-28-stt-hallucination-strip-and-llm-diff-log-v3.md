# Fix Whisper closer-hallucinations at the source — layered defence (v3)

## Objective

Stop "Thank you", "Bye", "Thanks for watching" from leaking into the
cursor, and surface what the LLM cleanup actually changed so users
can judge whether cleanup is pulling its weight on their model.
Attack the root cause (Whisper inference parameters + per-language
initial prompts + tighter VAD), not the symptom (regex strip).

## Diagnosis

(See plan v2 — three contributing factors: missing whisper-rs
hallucination guards, missing initial_prompt, generous VAD trim.
v3 supersedes v2 with a critical correction to Layer B: the prompt
must be language-aware, otherwise it actively breaks multilingual
users.)

## Why v3 supersedes v2

The v2 plan's single `[stt] prompt = "..."` field is dangerous for
multilingual users. Whisper's `initial_prompt` is **language-biasing**:
an English prompt sent with Romanian audio can cause:

1. **Language misdetection** — the language classifier sees English
   context tokens and biases the verdict toward English.
2. **Partial translation** — the model "completes" the prompt rather
   than transcribing, rendering Romanian phonemes as English
   approximations.
3. **Vocabulary bleed** — English filler words get inserted into
   otherwise-Romanian output.

Documented in the OpenAI Whisper cookbook and confirmed across
multiple whisper.cpp issue threads. Fix: per-language prompts,
keyed by BCP-47 code, gated by the language stickiness cache we
already shipped in v0.3.1.

## Implementation Plan

### Layer A — Wire Whisper local hallucination guards (unchanged from v2)

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

- [ ] Task A2. Make the four guards configurable via a new
  `[stt.local.hallucination_guards]` config section. Range-validate
  at config load.

### Layer B — Per-language initial prompts (REVISED for multilingual safety)

- [ ] Task B1. Add new `[stt.prompts]` config section as a
  `HashMap<String, String>` keyed by BCP-47 alpha-2 code:
  ```toml
  [stt.prompts]
  en = "Professional dictation. Output exactly what the speaker
       says with proper punctuation and capitalization."
  # Add your languages here, e.g.:
  # ro = "Dictare profesională. Redă cu precizie..."
  # es = "Dictado profesional. Reproduce con precisión..."
  ```
  Default ships with **English only** + doc-comment example for
  Romanian. Less surface area for "your default Romanian prompt
  is grammatically off" issues than shipping eight starter
  translations. Users add languages as they need them.

- [ ] Task B2. Selection logic at request time, in
  `effective_selection`/`transcribe` for each STT backend:
  - **Cold start** (LanguageCache empty, no force language):
    send NO prompt. Let Whisper auto-detect unbiased.
  - **Cache hit** (cache holds a code): look up
    `prompts[cached_code]`. If present, send. If absent, send
    no prompt.
  - **Forced** (`LanguageSelection::Forced(code)`, e.g. legacy
    config or rerun): look up `prompts[forced_code]`. If
    present, send. If absent, send no prompt.
  - **Confidence rerun** (banned-language detection): each
    candidate rerun forces a peer; use `prompts[peer]` for
    each candidate.

- [ ] Task B3. Plumb the resolved prompt through
  `SpeechToText::transcribe` via a new field on the existing
  request context (`TranscriptionRequest::initial_prompt:
  Option<String>` or thread through `FormatContext`-style).

- [ ] Task B4. Wire prompt to `whisper-rs::FullParams::set_initial_prompt`
  in `whisper_local.rs:128` and `:501`. Skip the call when the
  prompt is None.

- [ ] Task B5. Wire prompt to Groq's `prompt` form-data field in
  `crates/fono-stt/src/groq.rs::groq_post_wav` and
  `groq_post_wav_verbose`. Same wiring in
  `crates/fono-stt/src/groq_streaming.rs` for the preview and
  finalize POSTs. Skip the field when prompt is None.

- [ ] Task B6. Wire prompt to OpenAI's `prompt` form-data field in
  `crates/fono-stt/src/openai.rs`.

- [ ] Task B7. Validate prompts at config load: warn if a prompt
  exceeds 256 tokens (approximate via `prompt.split_whitespace()
  .count() * 1.3`) — that's the documented Whisper cap. Truncate
  with a warning rather than erroring; legacy configs with
  oversized prompts shouldn't break startup.

- [ ] Task B8. (Follow-up, NOT in this PR.) Per-app-context override
  via existing `[[context_rules]]` so users can add `stt.prompt =
  "..."` for code editors, etc. Track separately.

### Layer C — Reduce silent-tail window (unchanged from v2)

- [ ] Task C1. Lower default `interactive.hold_release_grace_ms`
  from `300` to `150`. Smoke-test that trailing-word truncation
  does not regress.

- [ ] Task C2. Lower default `pad_ms` in `fono-audio/src/trim.rs`
  from `60` to `20` and add hysteresis (require N silent frames
  in a row to mark end-of-speech, default `5`).

- [ ] Task C3. (Optional, later PR.) Replace static RMS detection
  with webrtc-vad. Track separately.

### Layer D — LLM cleanup observability (unchanged from v2)

- [ ] Task D1. After the existing `llm: {} {}ms → {} chars` INFO
  line at `session.rs:1265`, add a one-line diff summary:
  `llm: cleanup +N -M chars (or "no-op" when raw == cleaned)`.
  Levenshtein on bounded utterance length is sub-ms.

- [ ] Task D2. Bonus DEBUG log on `target: "fono::pipeline"` showing
  the actual before / after when they differ. Gated to debug
  because of transcript content. Operator opt-in via
  `RUST_LOG=fono::pipeline=debug`.

### Layer E — Strip-list band-aid (unchanged from v2)

- [ ] Task E1. New `crates/fono-stt/src/hallucinations.rs` with
  `strip_closer(text) -> (cleaned, removed)` and a curated list
  of ~25 well-known Whisper closers (English-only at first;
  non-English tracked as later phase).

- [ ] Task E2. Wire into both batch (`session.rs:1206`) and live
  (`session.rs:1109`) pipelines.

- [ ] Task E3. New `general.strip_whisper_hallucinations` config
  defaulting to `false` — opt-in only.

- [ ] Task E4. INFO log when strip fires, so the user sees what
  was cut and can disable the strip if it's biting them.

### Verification & docs

- [ ] Task F1. New unit tests:
  - 9 in `hallucinations.rs`
  - 4 in `whisper_local.rs` mocking `FullParams` setter calls
  - 6 in `groq.rs` / `openai.rs` confirming prompt is included
    in form-data **only when expected**:
      - cold start → no prompt
      - cache hit on `en` with `prompts.en` set → prompt = en string
      - cache hit on `ro` with no `prompts.ro` configured → no prompt
      - forced rerun → uses forced peer's prompt
      - cache hit on `en` but `prompts` empty → no prompt
      - over-256-token prompt → truncated + warning logged

- [ ] Task F2. Integration test in `crates/fono/tests/pipeline.rs`:
  feed mock STT returning "Hello world. Thank you." and assert
  cleaned output behaves correctly under each config combination.

- [ ] Task F3. Multi-language integration test: simulate a
  Romanian dictation and confirm the English `prompts.en`
  default is **not** sent (no prompt at all, since `prompts.ro`
  is unset by default).

- [ ] Task F4. Update `docs/troubleshooting.md` with new section
  "Whisper appends 'Thank you' / 'Bye' to my dictation" walking
  through the layered defence.

- [ ] Task F5. Update `docs/providers.md` with a new
  "Per-language transcription prompts" subsection explaining
  the `[stt.prompts]` map and the cache-driven selection logic.

- [ ] Task F6. CHANGELOG entries.

- [ ] Task F7. Build / fmt / clippy / test verify. No version
  bump. No tag.

## Verification Criteria

- After Layer A only: hallucinations drop substantially on local
  Whisper.
- After A + B: cloud Groq sees ~50 % reduction on the dominant
  language because the initial prompt biases away from closers.
  Other languages unaffected (no prompt sent), preserving
  unbiased detection.
- Multi-language smoke test: a Romanian user with default config
  (`prompts.en` set, `prompts.ro` not set) speaks Romanian.
  Expected: no prompt sent → Whisper auto-detects → text correct.
  Speaks English. Expected: cache=en → `prompts.en` sent →
  Whisper biased correctly → text correct + closer hallucination
  suppressed.
- After A + B + C1: hallucinations on local Whisper drop to
  near-zero in normal conditions.
- After A + B + C1 + C2: hallucinations on cloud Groq drop
  substantially.
- LLM cleanup INFO log makes it obvious when the LLM is doing
  real work vs operating as a no-op pass-through.
- Existing 196 unit tests + new tests all pass; clippy and fmt
  clean.

## Potential Risks and Mitigations

1. **`no_speech_thold = 0.6` aggressively drops legitimate
   short/quiet speech.** Mitigation: configurable; default
   matches whisper.cpp.

2. **English `prompts.en` bleeds prompt-flavour into
   transcriptions.** Mitigation: short, neutral default;
   doc-comment warns about cap; `prompts.en = ""` disables.

3. **Multilingual user has cache=en stale, switches to
   Romanian, English prompt biases detection toward English.**
   Mitigation: existing post-validation gate + confidence
   rerun catch the case in one round-trip. Cache then
   updates to `ro`. Next Romanian call: no prompt sent
   (since `prompts.ro` is unset by default), unbiased
   detection. Self-heals in two calls.

4. **User configures `prompts.ro` but it's grammatically off,
   degrading their Romanian transcriptions.** Mitigation:
   their config, their choice; doc-comment recommends short
   prompts and notes the bleed risk. The default ships only
   `prompts.en`, so out-of-the-box behaviour for Romanian is
   "no prompt" — same as today.

5. **Tighter `pad_ms = 20` clips consonants.** Mitigation:
   hysteresis (5 silent frames in a row required).

6. **Reducing `hold_release_grace_ms = 300 → 150`
   reintroduces trailing-word truncation.** Mitigation:
   smoke-test before committing the default change. If
   regression hits, leave at 300 and rely on layers A + B + C2.

## Alternative Approaches

1. **Ship per-language prompt translations for top 8 dictation
   languages out of the box.** Pro: every user benefits from
   the start. Con: we can't quality-check translations into
   languages we don't speak; risk of grammatically-off prompts
   degrading transcriptions. Reject in favour of "English only
   + doc example for adding others".

2. **Ship the regex strip-list only (skip Layers A + B
   entirely).** Simpler. But it's a band-aid that doesn't
   reduce inference cost. Reject as primary; keep as Layer E
   safety net.

3. **Use language-neutral prompts** — just a list of proper
   nouns / domain terms, no full sentence. E.g.
   `"Fono, NimbleX, BCP-47, whisper.cpp, llama.cpp"`.
   Pro: no language bias. Con: doesn't suppress
   "Thank you / Bye" hallucinations because the model has no
   style anchor. Could ship both: a neutral vocabulary anchor
   ALWAYS sent, plus per-language style anchor sent only when
   language is known. Two-prompt design adds complexity;
   defer to follow-up.

4. **Make the prompt config a list per language with priority
   ordering** (so multiple prompts can compose). Overkill;
   single prompt per language is sufficient for v1.
