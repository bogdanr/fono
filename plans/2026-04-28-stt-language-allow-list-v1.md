# STT Language Allow-List (Multi-Language Constrained Auto-Detect)

## Objective

Let users dictate freely in **multiple** languages while preventing
Whisper from drifting into unrelated ones. Replace today's
single-string `language = "auto" | <BCP-47>` knob with a list:

- empty list → unconstrained Whisper auto-detect (today's `"auto"`),
- one entry → forced single language (today's `"en"` etc.),
- multiple entries → **constrained auto-detect**: Whisper still picks,
  but only from the allowed set; everything else is banned.

The allow-list applies uniformly to local Whisper and to cloud STT
backends (Groq / OpenAI / etc.), with a behavior fallback path for
providers whose API exposes only a single `language` field.

## Background and Constraints

- whisper-rs 0.16 (already on the workspace, see
  `crates/fono-stt/src/whisper_local.rs:18`) exposes
  `WhisperState::lang_detect(offset_ms, n_threads) -> Result<Vec<f32>>`
  returning a probability vector indexed by Whisper's internal language
  IDs, plus `whisper_rs::get_lang_str(id)` / `get_lang_id(code)` for
  ID ↔ BCP-47 mapping. This is the supported way to do constrained
  language detection without monkey-patching token suppression.
- whisper.cpp's `set_language(Some(code))` accepts exactly one code;
  there is no native multi-language constraint, so the allow-list must
  be enforced at the wrapper layer.
- Cloud STT providers (`groq`, `openai`, `deepgram`, `azure`, etc.)
  accept one `language=` form field (see
  `crates/fono-stt/src/groq.rs:75-77` and
  `crates/fono-stt/src/openai.rs:54-56`). They cannot natively honour an
  allow-list; we need a degrade-gracefully strategy.
- The `language` field is referenced in many places: history
  (`crates/fono-core/src/history.rs:30`), pipeline
  (`crates/fono/src/session.rs:812-818`), CLI
  (`crates/fono/src/cli.rs:646-649`, `crates/fono/src/cli.rs:1466-1469`),
  wizard (`crates/fono/src/wizard.rs:246-249`, `:306-309`), bench
  (`crates/fono-bench/src/runner.rs:123`,
  `crates/fono-bench/src/equivalence.rs:440-443`). Keeping a single
  scalar in `Transcription` (the *resolved* language) is fine; only the
  *input* selector grows.
- Backwards compatibility is mandatory — Phase 0–10 has shipped and
  users have configs on disk with `language = "ro"` etc.

## Design Sketch

### 1. New core type: `LanguageSelection`

In `fono-stt::traits` (or a new `fono-stt::lang` module):

```text
enum LanguageSelection {
    Auto,                       // unconstrained
    Forced(String),             // exactly one allowed
    AllowList(Vec<String>),     // multi-language, ban everything else
}
```

Helpers:
- `LanguageSelection::from_config(general.languages: &[String])` —
  `[]` → `Auto`, `[x]` → `Forced(x)`, `[..]` → `AllowList(..)`.
- `LanguageSelection::contains(code) -> bool` — used to validate
  cloud-detected language post-hoc.
- `LanguageSelection::primary() -> Option<&str>` — first entry; used
  as the "best single guess" we pass to cloud providers and as the
  fallback when forced re-run is required.

The `SpeechToText::transcribe` signature changes from
`Option<&str>` to `&LanguageSelection`. `StreamingStt::stream_transcribe`
takes `LanguageSelection` by value (mirrors today's `Option<String>`).

### 2. Local Whisper: constrained auto-detect

Inside `WhisperLocal::transcribe` (`crates/fono-stt/src/whisper_local.rs:76-120`)
and `decode_blocking` (`crates/fono-stt/src/whisper_local.rs:295-330`):

- `Auto` → today's behaviour (no `set_language` call).
- `Forced(code)` → today's behaviour (`params.set_language(Some(code))`).
- `AllowList(codes)`:
  1. After `state.full(...)` would normally run, instead first call
     `state.lang_detect(0, threads)` on the same prepared state to get
     a `Vec<f32>` of language probabilities (encoder pass on first
     ~30 s of audio; cheap relative to full decode).
  2. Translate each allow-listed BCP-47 code to a Whisper language ID
     via `whisper_rs::get_lang_id`. Skip unknown codes with a
     `warn!` (typo in config).
  3. argmax over the masked subset → pick a single BCP-47 code.
  4. Reset / recreate the state (whisper.cpp requires a fresh state
     between an encoder-only pass and a decoding pass — verify against
     0.16 API; if `state.full` after `lang_detect` is allowed, keep
     the existing state) and run `state.full(...)` with
     `params.set_language(Some(picked))`.

Note on cost: `lang_detect` does only the encoder + a single softmax
on the language-token row, ~50–200 ms for `tiny`/`base`/`small`. For
the streaming path (preview lane) we **do not** re-run lang_detect on
every preview frame — detect once on the first qualifying chunk,
cache the picked code for the rest of the segment, reset on
`SegmentBoundary` (`crates/fono-stt/src/whisper_local.rs:236`).

### 3. Cloud STT: best-effort enforcement

In `Groq::transcribe` (`crates/fono-stt/src/groq.rs:58-97`),
`OpenAI::transcribe` (`crates/fono-stt/src/openai.rs:54-75`), and
analogous spots in any other cloud backend:

- `Auto` → omit `language` field (today's behaviour for `None`).
- `Forced(code)` → set `language = code` (today's behaviour).
- `AllowList(codes)`:
  - **First pass:** send `language = primary()` if
    `general.cloud_force_primary_language = true` (default `false`),
    otherwise omit and let the provider auto-detect.
  - **Post-validate:** if the provider returns a `language` field
    and it is **not** in the allow-list:
    - If `general.cloud_rerun_on_language_mismatch = true` (default
      `false` — costs an extra round-trip), re-issue the same audio
      with `language = primary()` and use that result.
    - Else log a `warn!` and accept the transcript as-is, but
      surface the mismatch in `Transcription.language` so the
      pipeline can flag it.

Document this clearly: the **only** way to get hard multi-language
banning today is local Whisper. Cloud is best-effort.

### 4. Config schema and migration

Replace single `language: String` semantics with a list while keeping
the old field readable for one version cycle.

In `crates/fono-core/src/config.rs:80-117` (`General`):

```text
pub struct General {
    /// Allowed languages (BCP-47). Empty = unconstrained auto-detect.
    /// One = forced. Two or more = constrained allow-list.
    #[serde(default)]
    pub languages: Vec<String>,
    /// DEPRECATED — kept for one cycle for migration. If set and
    /// `languages` is empty, populate `languages` from this.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub language: String,
    // … other fields unchanged …
    /// Cloud-only knob: when allow-list has > 1 entry, force the
    /// primary code on the first request. Default false.
    pub cloud_force_primary_language: bool,
    /// Cloud-only knob: re-run the request with the primary code if
    /// the provider returned a language outside the allow-list.
    /// Default false (one extra roundtrip per mismatch).
    pub cloud_rerun_on_language_mismatch: bool,
}
```

Same dual scheme on `SttLocal` (`crates/fono-core/src/config.rs:195-215`):
add `pub languages: Vec<String>`, deprecate `pub language: String`, and
fall through `general.languages` when both are empty.

`Config::load` runs a migration step:
- if `languages.is_empty()` and `language` is set:
  - `language == "auto"` or empty → leave `languages = []`,
  - otherwise → `languages = vec![language.clone()]`.
- on save, write only `languages`; the deprecated `language` field is
  pruned. Provide a one-liner `info!` log on first migration.

Round-tripping a v1 config without the new keys must keep working —
the existing v1 round-trip test in `crates/fono-core/src/config.rs:792`
becomes the migration test fixture.

### 5. Wizard prompt

`configure_cloud` (`crates/fono/src/wizard.rs:246-249`) and
`configure_mixed` (`crates/fono/src/wizard.rs:306-309`) currently ask
for one BCP-47 code. Replace with:

> "Languages you'll dictate in (comma-separated BCP-47, or `auto`).
>  Examples: `auto` · `en` · `en, ro` · `en, fr, de`."

Validate each entry against `whisper_rs::get_lang_id` (warn on
unknown). Persist as `Vec<String>` into `config.general.languages`.

Add a brief inline note: cloud STT is best-effort for multi-language
allow-lists; see the link to `docs/providers.md`.

### 6. CLI overrides

- `fono record --language en` already exists conceptually
  (`crates/fono/src/cli.rs:646-649`). Add a parallel `--languages
  en,ro,fr` flag that, when present, overrides
  `config.general.languages` for that single invocation. Mutually
  exclusive with `--language` (which becomes shorthand for a one-entry
  allow-list).
- `fono transcribe` (`crates/fono/src/cli.rs:1466-1469`) gets the
  same treatment.
- `fono use language en,ro,fr` — new subcommand under the existing
  `fono use` tree (`crates/fono/src/cli.rs`, alongside `use stt|llm`)
  to atomically rewrite `general.languages` and trigger the existing
  `Request::Reload` IPC so the daemon hot-applies it.

### 7. Pipeline plumbing

`crates/fono/src/session.rs:812-818` builds the `language: Option<String>`
the streaming session takes today. Replace with a
`LanguageSelection::from_config(&cfg.general.languages)` and thread it
through `LiveSession::with_language` (which becomes
`with_language_selection`) at `crates/fono/src/live.rs:167-168` and
`:318`. The detected language fed back into `Transcription.language`
is still a single resolved code (post-detect), unchanged.

History row (`crates/fono-core/src/history.rs:30, 102, 145, 208, 237`)
keeps its single `language` column (the *resolved* language for that
dictation).

### 8. Documentation

- `docs/providers.md` — new "Multi-language dictation" section
  explaining: empty list = auto, single = forced, multiple = banned
  outside the list; cloud caveats; the two cloud knobs.
- `README.md` — one-paragraph mention in the configuration / quick
  reference area.
- `CHANGELOG.md` — entry under unreleased: "STT language allow-list
  (`general.languages`) replaces single `general.language`. Backwards
  compatible; existing configs auto-migrate."

### 9. ADR

`docs/decisions/0016-language-allow-list.md` capturing:
- why detect-then-constrain over token suppression,
- why `LanguageSelection` enum vs. always-`Vec<String>`,
- cloud best-effort trade-off,
- the cost ceiling on `lang_detect` (encoder pass; mitigations).

## Implementation Plan

- [ ] Task 1. **Verify whisper-rs 0.16 `lang_detect` API.** Read
      `~/.cargo/registry/src/.../whisper-rs-0.16.*/src/whisper_state.rs`
      to confirm exact signature, return type, error semantics, and
      whether `state.full()` can run on the same state after
      `lang_detect` or whether a fresh state is required. Also
      confirm the BCP-47 ↔ ID helpers (`get_lang_id`, `get_lang_str`)
      and decide whether unknown codes return `Option` or panic.
      Document findings in the ADR.

- [ ] Task 2. **Introduce `LanguageSelection` in `fono-stt`.** New
      module `crates/fono-stt/src/lang.rs` exposing the enum,
      `from_config(&[String])`, `primary()`, `contains(&str)`,
      `is_auto()`, plus a normalisation helper that lowercases and
      strips whitespace. Unit-test all variants and edge cases (empty
      strings, duplicates, case folding, `"auto"` aliasing to empty).

- [ ] Task 3. **Migrate `SpeechToText` trait.** Change
      `transcribe(&self, pcm, sr, lang: Option<&str>)` to take
      `&LanguageSelection`. Update every implementor (`WhisperLocal`,
      `Groq`, `OpenAI`, fakes in `fono-bench`, test stubs in
      `crates/fono/tests/pipeline.rs`). The `Transcription.language`
      output stays `Option<String>` (the *resolved* code). Mirror on
      `StreamingStt` (`crates/fono-stt/src/streaming.rs:44-88` and
      `crates/fono-stt/src/whisper_local.rs:184-189`).

- [ ] Task 4. **Implement constrained auto-detect in `WhisperLocal`.**
      In both batch (`crates/fono-stt/src/whisper_local.rs:76-120`) and
      streaming `decode_blocking`
      (`crates/fono-stt/src/whisper_local.rs:295-330`): branch on the
      enum, run `state.lang_detect(...)` for `AllowList`, mask the
      probability vector, argmax, then run `state.full(...)` with the
      picked code. For streaming, cache the picked code per segment
      (re-detect on `SegmentBoundary`,
      `crates/fono-stt/src/whisper_local.rs:236`). Emit a `debug!`
      with the picked code and the runner-up score so users can
      diagnose mis-picks via `FONO_LOG`.

- [ ] Task 5. **Implement best-effort enforcement in cloud STT.**
      Update `Groq::transcribe` (`crates/fono-stt/src/groq.rs:58-97`)
      and `OpenAI::transcribe`
      (`crates/fono-stt/src/openai.rs:54-75`). Honour the new config
      knobs `cloud_force_primary_language` and
      `cloud_rerun_on_language_mismatch`. Emit a `warn!` on every
      mismatch, including the case where the rerun knob is off.
      Audit any other cloud backends present in `crates/fono-stt/src/`
      and apply the same pattern.

- [ ] Task 6. **Schema + migration in `fono-core::config`.** Add
      `languages: Vec<String>` and the two cloud knobs to `General`
      (`crates/fono-core/src/config.rs:80-117`); add `languages` to
      `SttLocal` (`crates/fono-core/src/config.rs:195-215`). Implement
      migration in `Config::load` (or `post_load`): if `languages` is
      empty and the legacy `language` is non-empty + non-`"auto"`,
      lift to `languages = vec![language]`. Stop serializing the
      legacy field. Add `assert_compat_v1` round-trip test plus a new
      test fixture for the multi-language case.

- [ ] Task 7. **Update wizard.** Replace the single-language `Input`
      prompts in `crates/fono/src/wizard.rs:246-249` and `:306-309`
      with a comma-list prompt, validate each token via
      `whisper_rs::get_lang_id`, and write into
      `config.general.languages`. Print a one-line preview of the
      effective behaviour (auto / forced / allow-list of N).

- [ ] Task 8. **Add CLI surfaces.** Extend `fono record` and
      `fono transcribe` (`crates/fono/src/cli.rs:646-649`,
      `crates/fono/src/cli.rs:1466-1469`) with `--languages` (comma-
      separated). Add `fono use language en,ro,fr` subcommand that
      atomically rewrites the config and triggers `Request::Reload`,
      mirroring the existing `fono use stt` flow at
      `crates/fono-core/src/providers.rs:178-218`.

- [ ] Task 9. **Pipeline plumbing.** Replace
      `Option<String>`-style language handoff in
      `crates/fono/src/session.rs:812-818` and
      `crates/fono/src/live.rs:141-318` with `LanguageSelection`.
      Keep the resolved language scalar in
      `Transcription.language` so history
      (`crates/fono-core/src/history.rs`) is unchanged.

- [ ] Task 10. **Tray hint (small).** When the active selection is
      an allow-list with > 1 entry, surface the resolved language of
      the most recent dictation in the tray's "Recent transcriptions"
      tooltip (already wired via `RecentProvider`,
      `crates/fono-tray/src/lib.rs`). No new menu items, just enrich
      the existing label format.

- [ ] Task 11. **Tests.**
      - Unit: `LanguageSelection::from_config` (Task 2).
      - Unit: language-mask argmax helper given a synthetic prob
        vector + allow-list → expected pick (no whisper context
        required; isolate the masking logic into a free function).
      - Migration: legacy `language = "ro"` config → `languages =
        ["ro"]` after load + save round-trip.
      - Integration (gated `whisper-local`): tiny model + a recorded
        non-English sample + allow-list `["en"]` → resolved language
        is `"en"` (the ban worked) — fixture under
        `tests/fixtures/lang/`.
      - Cloud (mocked HTTP): provider returns `language = "cy"`,
        allow-list is `["en", "ro"]`, rerun knob off → warn logged,
        accepted; rerun knob on → second request issued with
        `language = "en"`.

- [ ] Task 12. **Docs + ADR + CHANGELOG.** Write
      `docs/decisions/0016-language-allow-list.md`, add the
      "Multi-language dictation" section to `docs/providers.md`, drop
      a one-paragraph mention into `README.md`, and add the entry to
      `CHANGELOG.md` under unreleased. Update `docs/status.md` with a
      session-log entry per the project's hard rules in `AGENTS.md`.

## Verification Criteria

- Loading a v1 config with `[general] language = "ro"` and no
  `languages` field produces, after save, a config containing
  `languages = ["ro"]` and no `language` key, with no other field
  altered.
- `fono record --languages en,ro` on local whisper, fed an English
  sample, produces a transcription whose `language == "en"`. Same
  command fed a French sample produces a transcription whose
  `language ∈ {"en", "ro"}` (whichever is closer; never `"fr"`).
- `fono record` with `general.languages = []` reproduces the current
  unconstrained-auto-detect behaviour byte-for-byte (regression
  guard).
- Cloud STT with `general.languages = ["en", "ro"]`,
  `cloud_force_primary_language = false`,
  `cloud_rerun_on_language_mismatch = true`, fed a French sample:
  the request log shows two HTTP calls (auto then forced `en`) and
  the final `Transcription.language == "en"`.
- `cargo build --workspace`,
  `cargo build --workspace --features fono/interactive`,
  `cargo clippy --workspace --no-deps -- -D warnings`,
  `cargo clippy --workspace --no-deps --features fono/interactive
   -- -D warnings`, and `cargo test --workspace --lib --tests` all
  pass cleanly. No new clippy allows.
- All Rust files added in this plan begin with the SPDX header
  required by `AGENTS.md`. All commits are DCO-signed.

## Potential Risks and Mitigations

1. **`lang_detect` cost on streaming preview lane.** Re-detecting on
   every preview chunk would tank time-to-first-frame.
   Mitigation: detect once per segment, cache the picked code, reset
   on `SegmentBoundary`. Document in the ADR.
2. **whisper-rs 0.16 API drift.** If `state.full()` cannot legally
   follow `state.lang_detect()` on the same state, we must allocate
   a second state per inference, doubling KV-cache memory.
   Mitigation: Task 1 verifies the API up front; if a second state
   is required, document the memory delta and switch the streaming
   path to allocate states from a `parking_lot::Mutex<Pool>` rather
   than a fresh `create_state()` per chunk.
3. **Cloud rerun doubles latency and cost.** Users on metered cloud
   plans could get surprised.
   Mitigation: rerun knob defaults `false`, clearly documented; the
   warn-on-mismatch path is always on so users see the issue and can
   opt in deliberately.
4. **Unknown BCP-47 codes from typos in config or wizard.**
   Mitigation: validate at load + at wizard-input time via
   `whisper_rs::get_lang_id`; surface a single warn-level log line
   per unknown code; never crash.
5. **Config-migration data loss.** A botched migration could nuke
   `language = "en"` without populating `languages`.
   Mitigation: explicit unit test on a known v1 fixture, plus
   write-then-read round-trip assertion. Migration is idempotent.
6. **Allow-list ⊃ all-languages edge case.** A user lists every
   language whisper supports; behaviourally equivalent to `Auto` but
   pays the `lang_detect` cost.
   Mitigation: detect during normalisation: if the allow-list covers
   ≥ N languages (threshold ~50), collapse to `Auto` and emit
   `info!`.

## Alternative Approaches

1. **Token suppression via `whisper_full_params.suppress_tokens`.**
   Build the suppress list from the language tokens not in the
   allow-list. Lower per-call cost (no extra encoder pass) but
   whisper-rs 0.16 does not expose a clean setter and undefined
   behaviour ensues if every language token is suppressed. Higher
   maintenance risk; rejected for v1, revisit if `lang_detect`
   measurably slows steady-state streaming.
2. **Always force the primary language.** Simplest possible change:
   `general.languages[0]` is always passed to whisper / cloud, ignore
   the rest. Loses the multi-language UX entirely; equivalent to a
   one-entry allow-list. Rejected — that is the *current* behaviour
   when users set `language = "en"`, and the issue explicitly calls
   for multi-language support.
3. **External lid (language identification) model.** Ship a small
   dedicated LID model (e.g. SpeechBrain ECAPA-TDNN, ~30 MB) and run
   it before STT. More accurate than whisper's internal LID,
   especially for short clips. But it adds a dependency, a model
   download, and a build-time switch — over-engineered for the
   current pain point. Park as a v0.3 idea if real-world reports
   show whisper LID losing to allow-listed languages on short audio.
