# Romanian (and any-language) Dictation: Make LLM Cleanup Repair Garbled Transcripts — Zero-Config

## Objective

Make dictation cleanup **just work** for non-English speakers, with no
documentation reading and no manual configuration, across **every** STT
engine (local Whisper, Groq, OpenAI, Deepgram, Cartesia, Cohere, Parakeet,
Wyoming, …), including the common case where the user **switches languages
between utterances** (English now, native language next, back to English).

The user's symptom: dictating Romanian yields raw text that looks "written
with the feet" — nonsense words, **missing diacritics** (ă, â, î, ș, ț) —
and the LLM cleanup step does nothing to fix it.

Design constraint from feedback: **easiest approach that works ~99% of the
time.** Assume the user never reads docs. Do not depend on any single
engine reporting a per-utterance language. Reuse the existing
language-subset signal Fono already computes.

This is a planning document only. No code is changed here.

## What I confirmed in the codebase (updated)

### Confirmed bug 1 — the cleanup prompt forbids reconstruction
`crates/fono-core/src/config.rs:570-583` (`default_prompt_main`) restricts
the model to filler removal + punctuation + stutter collapse and says
"do not … add content" / "Do not invent missing content." It is *designed*
not to repair garbled words. This is the primary reason cleanup "does
nothing" on broken Romanian. (Origin: the clarification-refusal fix,
`plans/closed/2026-04-28-llm-cleanup-clarification-refusal-fix-v1.md`.)

### Confirmed bug 2 — the language hint never reaches the model
`FormatContext` has a `language` field
(`crates/fono-polish/src/traits.rs:7-16`), populated from the STT result in
`crates/fono/src/session.rs:3412-3420`, but
`FormatContext::system_prompt()` (`crates/fono-polish/src/traits.rs:18-42`)
**never reads it**. The model gets Romanian with no language signal and no
instruction to restore diacritics.

### NEW finding A — a language *subset* already exists, zero-config
`crates/fono/src/daemon.rs:100-112` **auto-populates `general.languages`**
from OS-locale signals (`detect_user_languages_ranked`) whenever the list
is empty — even if the user skips the wizard. The detector
(`crates/fono-core/src/locale.rs:88-139`) fuses system locale, formatting
locale, keyboard layout, and timezone, so a Romanian user on a typical box
ends up with `["ro", "en"]` automatically. **This is the "subset of
languages based on signals" the feedback refers to, and it is already
wired.** It is the ideal source of *candidate* languages to feed the LLM.

### NEW finding B — per-utterance language reporting is NOT universal
`Transcription.language` (`crates/fono-stt/src/traits.rs:8-12`) is best-
effort and engine-dependent: Whisper local fills it
(`crates/fono-stt/src/whisper_local.rs:268`), Groq/Cartesia echo it when
present (`crates/fono-stt/src/groq.rs:451`,
`crates/fono-stt/src/cartesia.rs:216`), but it is often `None` and a future
Cohere/Parakeet backend may never set it. **Therefore the cleanup fix must
not rely on `Transcription.language` being present.** It must instead lean
on the *configured candidate set* (finding A), which is always available.

### Confirmed context — short-skip and English-only refusal guard
`crates/fono/src/session.rs:3223-3236` skips cleanup below
`skip_if_words_lt` (default 3). `looks_like_clarification`
(`crates/fono-polish/src/traits.rs:72-120`) is English-only. Both matter
once the prompt is loosened.

## Core idea (the easy 99% approach)

Stop trying to *tell* the LLM exactly which language each utterance is —
that signal is unreliable and engine-specific. Instead:

1. **Give the LLM the user's candidate language set** (from
   `general.languages`, which is auto-populated — finding A) on every
   cleanup call.
2. **Instruct the LLM to (a) decide which of those languages the transcript
   is in, (b) keep the output in that language, and (c) fully repair it —
   restoring diacritics and fixing phonetically-mangled words.**

This single change is:
- **Engine-agnostic** — works for Cohere/Parakeet/anything because it does
  not need the STT engine to report a language.
- **Switch-proof** — the set is `{ro, en}`, so an English utterance is
  cleaned as English and the next Romanian one as Romanian; the LLM
  re-decides every call.
- **Zero-config** — the candidate set already populates itself from OS
  signals.
- **Robust** — when `Transcription.language` *is* present and inside the
  set, we pass it as a soft hint to make the LLM's choice even easier, but
  correctness does not depend on it.

## Implementation Plan

### A. Plumb the candidate language set into cleanup

- [ ] Task A1. Extend `FormatContext`
  (`crates/fono-polish/src/traits.rs:7-16`) with the candidate set, e.g.
  `candidate_languages: Vec<String>` (BCP-47 codes), alongside the existing
  best-effort `language: Option<String>` hint. Rationale: the set is the
  reliable signal (finding A/B); the singular hint stays as an optional
  accelerator.

- [ ] Task A2. Populate the set in `build_format_context`
  (`crates/fono/src/session.rs:3395-3426`) from `config.general.languages`
  (fall back to `config.stt.local.languages` when that per-backend override
  is set, mirroring `lang_for`). Keep passing `trans.language` into the
  existing `language` field as the soft hint. Rationale: reuses the
  already-populated subset; no new detection code.

- [ ] Task A3. Map BCP-47 codes to human names for the prompt using
  `fono_core::languages::display_name`
  (`crates/fono-core/src/languages.rs:42-45`). Rationale: "Romanian, English"
  reads better to an LLM than "ro, en" and improves selection accuracy.

### B. Teach `system_prompt()` to emit the language directive

- [ ] Task B1. In `FormatContext::system_prompt()`
  (`crates/fono-polish/src/traits.rs:18-42`), when `candidate_languages` is
  non-empty, append a directive of the form: "This transcript is in one of:
  <Name list>. Detect which one, keep your output entirely in that
  language, and restore its correct orthography — including all diacritics
  (e.g. for Romanian: ă, â, î, ș, ț). Do not translate between these
  languages." When a single-element soft `language` hint is present and in
  the set, add "It is most likely <Name>." Rationale: closes confirmed
  bug 2 with the reliable set; the hint is additive, not load-bearing.

- [ ] Task B2. Unit-test the directive: present for `{ro, en}`, names both,
  mentions diacritics; absent when the set is empty; the soft hint sentence
  appears only when the hint is inside the set. Rationale: locks behaviour
  and prevents the field going unused again (the bug we just found).

### C. Reword the cleanup prompt to allow reconstruction (carefully)

- [ ] Task C1. Revise `default_prompt_main`
  (`crates/fono-core/src/config.rs:570-583`) so the model is **permitted and
  instructed** to reconstruct garbled / phonetically-mangled words into the
  most plausible intended words *in the detected language* and to restore
  diacritics — while re-scoping "do not invent content" to "do not add new
  ideas, sentences, or facts the speaker did not say." Rationale: primary
  fix for confirmed bug 1; the re-scoping keeps the anti-hallucination
  intent intact.

- [ ] Task C2. Keep every anti-clarification hard rule verbatim (no
  questions, no preamble, output only cleaned text, no `<<<`/`>>>` markers)
  so the closed clarification-refusal fix does not regress. Rationale:
  reconstruction and "no chatty refusals" are compatible if stated
  precisely.

- [ ] Task C3. Adjust `default_prompt_advanced`
  (`crates/fono-core/src/config.rs:586-591`) to say that for low-confidence
  tokens the model should prefer the most likely in-language reconstruction
  over a literal pass-through. Rationale: reinforces C1.

- [ ] Task C4. Find and update every test/snapshot that asserts the exact
  default prompt text (`crates/fono/tests/pipeline.rs`, polish-crate tests,
  any prompt fixtures). Rationale: prompt text is asserted; CI pre-commit
  gate will fail otherwise.

### D. Keep the guards safe under the looser prompt

- [ ] Task D1. Re-run / extend `looks_like_clarification`
  (`crates/fono-polish/src/traits.rs:72-120`) checks to confirm reconstructed
  non-English output is never falsely rejected and chatty refusals are still
  caught. The guard stays English-only (the refusal failure mode is an
  English chat-tuning artifact); just verify no regression. Rationale:
  finding from the original analysis; low effort, prevents surprises.

- [ ] Task D2. Confirm the short-utterance skip
  (`crates/fono/src/session.rs:3223-3236`) still bypasses cleanup by design
  for sub-`skip_if_words_lt` captures, and that this is acceptable (a
  one-word Romanian capture won't be repaired). Document the trade-off in
  the plan rather than changing behaviour. Rationale: scope control.

### E. Validation

- [ ] Task E1. Pipeline test (extend `crates/fono/tests/pipeline.rs`): stub
  STT returns a garbled, diacritic-stripped Romanian string with
  `language: None` (to prove engine-independence) while
  `general.languages = ["ro","en"]`; a fake `TextFormatter` asserts the
  `FormatContext` it receives carries `candidate_languages = ["ro","en"]`
  and that `system_prompt()` contains the Romanian diacritic directive.
  Rationale: pins finding-A reliance and both confirmed-bug fixes together,
  independent of `Transcription.language`.

- [ ] Task E2. Add a second case with `language: Some("ro")` to prove the
  soft-hint sentence is added when the engine *does* report it. Rationale:
  covers the Whisper/Groq path without making it load-bearing.

- [ ] Task E3. Run the AGENTS.md pre-commit gate (`cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`) before any commit. Rationale:
  mandatory project rule.

### F. Optional upstream quality boost (only if reconstruction proves insufficient)

- [ ] Task F1. Investigate whether the *streaming/batch local Whisper* path
  should pass the candidate set so STT itself decodes Romanian better
  (already supported via `LanguageSelection::AllowList`,
  `crates/fono-stt/src/whisper_local.rs:492-562`, and fed from
  `general.languages`). Only pursue if cleaned output is still weak because
  the raw text has too little signal. Rationale: keeps the change minimal;
  the cleanup-side fix should carry the 99% on its own.

## Verification Criteria

- With `general.languages = ["ro","en"]` (auto-populated, no manual edit),
  a garbled diacritic-free Romanian capture is returned as readable,
  correctly-accented Romanian — **even when the STT engine reports no
  language** (`Transcription.language = None`).
- An English capture in the same session is cleaned as English (no
  spurious translation), proving the switch-proof behaviour.
- `system_prompt()` names every candidate language and explicitly requests
  diacritic restoration when the set is non-empty; emits nothing extra when
  the set is empty (unit-tested).
- Anti-clarification behaviour holds: existing `looks_like_clarification`
  tests pass; no questions/preamble are emitted.
- `cargo fmt --check`, `cargo clippy -D warnings`, and the workspace tests
  all pass.

## Potential Risks and Mitigations

1. **LLM picks the wrong language from the set** (e.g. cleans Romanian as
   if English). Mitigation: the soft `Transcription.language` hint (when
   present) biases the choice; low `temperature` (0.2,
   `crates/fono-polish/src/openai_compat.rs:148`); the set is usually small
   (2-3) so the choice is easy.
2. **Reconstruction re-opens hallucination.** Mitigation: keep "no new
   ideas/sentences" rule (C1), keep all hard rules (C2), validate short
   ambiguous captures.
3. **Regressing the clarification-refusal fix.** Mitigation: retain hard
   rules verbatim and re-run the guard's test suite (D1).
4. **Prompt-text test breakage.** Mitigation: C4 updates all assertions in
   the same change.
5. **Empty candidate set on exotic systems** (locale detection returns
   nothing, user skipped wizard). Mitigation: behaviour degrades to the
   soft `language` hint if present, else to today's conservative cleanup —
   no worse than current. Consider a last-resort single-language fallback
   only if field reports show empty sets are common.
6. **Cross-backend consistency.** The directive lives in
   `FormatContext::system_prompt()`, which Anthropic, OpenAI-compat, and
   local llama backends all call — centralised. Mitigation: verify each
   backend routes through `system_prompt()`.

## Alternative Approaches

1. **Set-based, chosen here.** Feed the candidate set + "detect-and-repair"
   instruction. Engine-agnostic, switch-proof, zero-config. Trade-off:
   relies on the LLM to pick correctly within the set (mitigated by small
   sets + soft hint).
2. **Per-utterance language only.** Pass just `Transcription.language`.
   Rejected: not all engines report it (finding B), and it is brittle on
   accented speech (`docs/troubleshooting.md:213-226`).
3. **Detect language with a separate model/library before cleanup.** Adds a
   dependency and latency, and `deny.toml`/license review, for marginal gain
   over letting the cleanup LLM choose from a 2-3 item set. Rejected as
   over-engineering for the "easy 99%" goal.
4. **Force STT to the candidate set only (upstream-only fix).** Improves raw
   text but doesn't satisfy "the LLM should reconstruct," and still passes
   broken words through on mis-decodes. Kept as optional Task F1, not the
   primary path.
