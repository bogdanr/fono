# Fono — Automatic Translation

## Objective

Let users dictate in any source language and have Fono inject text in a
chosen target language automatically, without leaving their editor.
Replace the current "STT → cleanup → inject" pipeline with
"STT → (translate) → cleanup → inject", where translation is opt-in,
arbitrary-target (not English-only), and composes cleanly with the
existing language allow-list (`docs/decisions/0016-language-allow-list.md`).

The feature must support:

- arbitrary `(source, target)` BCP-47 pairs (Romanian → French, German → Japanese, etc.);
- a "translate only when source ≠ target" mode, so a single config works for users who dictate primarily in their native tongue but occasionally in others;
- per-app overrides via the existing `[[context_rules]]` shape (e.g. *"always translate to English when the focused app is Slack"*);
- both batch and live (streaming) pipelines at parity;
- fast-path optimisations (Whisper `set_translate`, cloud STT `/audio/translations`) when the target happens to be English, without forcing users to reason about the distinction.

Non-goals (deferred):

- voice-to-voice translation (TTS playback);
- bidirectional translation (mid-utterance code-switching detection);
- per-segment translation in live preview before EOU finalisation.

## Background — what we already have

Source layer (recently landed allow-list work):

- `crates/fono-stt/src/lang.rs:25-37` — `LanguageSelection { Auto, Forced, AllowList }`.
- `crates/fono-stt/src/traits.rs:7-41` — `Transcription { text, language, duration_ms }`. The `language` field is populated by `whisper_local.rs:147`, `groq.rs:163`, and (best-effort) `openai.rs:153-157`.
- `crates/fono-llm/src/traits.rs:7-54` — `FormatContext { …, language }`. The `language` field is **plumbed but unused by `system_prompt()`**; we get to start using it.
- `crates/fono/src/session.rs:1109-1333` — batch `run_pipeline`. Live equivalent at `:909-1106`. Both build a `FormatContext` via `build_format_context` at `:1344-1364`.
- `crates/fono-core/src/config.rs:14-55` — top-level `Config`; legal home for a new `[translate]` section is line 54.
- `crates/fono/src/cli.rs:119-155` — `Record` / `Transcribe` per-call flags; `:262-284` — `UseCmd` provider switching.

Native fast paths (English-only target):

- `whisper-rs` 0.16 `params.set_translate(bool)` — currently hardcoded to `false` at `whisper_local.rs:125,501`.
- Groq `/audio/translations` and OpenAI `/audio/translations` — endpoint shape mirrors `/audio/transcriptions`; output is **always English**.

There is no prior translation work in `docs/decisions/` or `docs/plans/` (the word "translator" in `status.md:131` and `0015-boundary-heuristics.md:122` refers to the streaming token-translator task).

## Strategy choice

Three implementation routes were considered:

| | A. Whisper `set_translate` | B. Cloud STT `/audio/translations` | C. LLM-as-translator |
| --- | --- | --- | --- |
| Target languages | **English only** | **English only** (Groq + OpenAI) | **Arbitrary** |
| Latency added | ~0 (same decoder pass) | 0 (replaces transcription call) | +200–800 ms (one LLM round-trip) |
| Cost added | $0 | $0 (replaces STT call) | One extra LLM call (or merged into cleanup) |
| Quality | Whisper-grade | Whisper-large-v3 on Groq | Frontier LLMs ≥ Whisper; uses dictionary / rule_suffix |
| Allow-list compat | OK (source still detected) | Loses `language` echo → mode-`if-source-not-target` breaks | OK |

**A and B are non-starters as the sole mechanism** — Fono explicitly needs arbitrary target languages. Decision: **C is the default; A and B are opt-in fast paths that activate only when `target = "en"`**. A new `Translator` trait dispatches between them.

The `Translator` is merge-able with cleanup later: a single LLM call carrying both *"translate to X"* and *"apply cleanup rules"* in one system prompt halves latency, at modest quality risk. Ship as separate calls in v1; A/B-test the merged variant in a follow-up.

## Implementation Plan

### Phase 1 — Trait + plumbing scaffold (no behaviour change)

- [ ] Task 1. Create `crates/fono-llm/src/translate.rs` exposing `pub trait Translator: Send + Sync` with `async fn translate(&self, text, source: Option<&str>, target: &str) -> Result<String>`, `name()`, and `prewarm()`. Re-export from `crates/fono-llm/src/lib.rs`. Rationale: a dedicated trait (rather than overloading `TextFormatter`) keeps the responsibility split clean and lets local-only deployments pick a different translator backend than the cleanup LLM.

- [ ] Task 2. Add `crates/fono-llm/src/factory.rs::build_translator(cfg: &Translate, secrets, paths) -> Result<Option<Arc<dyn Translator>>>`, mirroring the existing `build_llm` shape. Returns `Ok(None)` when `!cfg.enabled || cfg.target.is_empty()`. Errors are non-fatal at construction (the caller logs and continues without translation, just like LLM cleanup today — `session.rs:255-261`).

- [ ] Task 3. Wire the factory into `SessionOrchestrator` (`crates/fono/src/session.rs:247-334` and the reload path `:343-405`). Add a `translator: Arc<RwLock<Option<Arc<dyn Translator>>>>` slot alongside the existing `llm` slot. Hot-reload symmetry is mandatory (matches the language-allow-list precedent).

### Phase 2 — Config schema + migration

- [ ] Task 4. Add a `Translate` struct in `crates/fono-core/src/config.rs` (between `Llm` and `ContextRule`) with fields:
  - `enabled: bool` (default `false`);
  - `target: String` (default `""` — BCP-47);
  - `mode: TranslateMode` enum (`Llm` | `WhisperNative` | `CloudStt`, default `Llm`);
  - `backend: String` (default `"auto"` — when `"auto"` and mode is `Llm`, reuse the configured `[llm]` backend);
  - `when: TranslateWhen` enum (`Always` | `IfSourceNotTarget`, default `IfSourceNotTarget`);
  - `before_cleanup: bool` (default `true` — translate, then run cleanup in the target language so dictionary + rule_suffix apply correctly);
  - `cloud: Option<TranslateCloud>` for backend-specific overrides (mirror `SttCloud`).

  Plumb a top-level `pub translate: Translate` on `Config` with `#[serde(default)]`. No version bump needed (additive).

- [ ] Task 5. Extend `ContextRule` (`config.rs:393-406`) with `translate_target: Option<String>` so per-app overrides slot in alongside the existing `prompt_suffix` field. The active rule's `translate_target`, when present, overrides `[translate].target` for that dictation. Rationale: the user can dictate primarily in Romanian but always translate to English when the focused app is Slack — the same affordance the cleanup pipeline already gives via `prompt_suffix`.

- [ ] Task 6. Add migration in `Config::migrate` (`config.rs:666-702`): no legacy field to lift, but emit a one-line `info!` when `translate.enabled` flips on for the first time so users see in the daemon log that the feature engaged.

### Phase 3 — Backends

- [ ] Task 7. **LLM-as-translator backend** (`crates/fono-llm/src/translate_llm.rs`): implement `Translator` for a thin wrapper that owns an `Arc<dyn TextFormatter>` and forwards `translate(text, source, target)` to the underlying LLM with a fixed system prompt template ("Translate the following text from {source_or_detect} to {target}. Output only the translation. Preserve formatting, line breaks, and any code blocks verbatim."). Reuse Anthropic / OpenAI-compat / llama-local via the existing factory; the `Translator` is just a different prompt over the same channel.

- [ ] Task 8. **Whisper-native fast path**: gate `params.set_translate(true)` at `whisper_local.rs:125` and `:501` on `cfg.translate.enabled && cfg.translate.mode == WhisperNative && cfg.translate.target == "en"`. When the user picks this mode but a non-English target, log a warning at startup and fall back to `Llm`. Document in the new ADR.

- [ ] Task 9. **Cloud STT fast path**: add `GROQ_TRANSLATIONS_ENDPOINT` / `OPENAI_TRANSLATIONS_ENDPOINT` constants alongside the existing transcription endpoints (`groq.rs:12`, `openai.rs:13`). Add a `translate_to_english: bool` flag on the cloud STT structs; when set, route to `/audio/translations` (drop the `language` form field, response shape is identical). Same English-only fallback warning as Task 8. **Caveat**: the response no longer echoes a `language` field, so the allow-list post-validation rerun (`groq.rs:130-159`) must be skipped on this path — document this as a known degradation.

### Phase 4 — Pipeline integration

- [ ] Task 10. Refactor `session.rs::run_pipeline` (`:1109-1333`) to insert a translation stage between STT (`:1167`) and LLM cleanup (`:1206`). New helper `apply_translation(translator, raw, source_lang, cfg) -> Result<Option<String>>` returning `Some(translated)` when translation ran, `None` when skipped (mode = `IfSourceNotTarget` and source already matches target, or feature disabled). Set `cleanup_input` to `translated.as_deref().unwrap_or(&raw)`.

- [ ] Task 11. Mirror Task 10 in the live (streaming) finalise path at `session.rs:909-1106` — specifically the cleanup span at `:991-1021`. To avoid duplication, hoist the new helper into a shared `pipeline` submodule that both call sites invoke.

- [ ] Task 12. Extend `FormatContext` (`crates/fono-llm/src/traits.rs:7-16`) with `target_language: Option<String>` and start using `language` + `target_language` inside `system_prompt()` (`:21-41`) so the cleanup LLM knows it should keep output in the target language and not "helpfully" translate back. Tiny prompt addition: *"The user's dictation is in {language}. The output must be in {target_language}."* Without this, Anthropic models in particular love to translate cleaned text back to whatever they think is "natural".

- [ ] Task 13. Extend the history schema to record translation. Add a `translated: Option<String>` column to the `transcriptions` table (`crates/fono-core/src/history.rs`) with a forward-compatible `ALTER TABLE … ADD COLUMN` migration. Insertion happens at `session.rs:1308-1320`. Tray "Recent transcriptions" menu (in `crates/fono-tray`) shows the translated text by default, with the raw available on hover or via `fono history show --raw <id>`.

### Phase 5 — Wizard + CLI

- [ ] Task 14. Add a translation step to both wizard branches:
  - `crates/fono/src/wizard.rs:243-252` (cloud) — after the languages prompt, ask *"Translate dictation to a different language? [y/N]"*; if yes, *"Target language (BCP-47)?"* and persist into `config.translate`.
  - `crates/fono/src/wizard.rs:254-315` (mixed) — same prompts at `:309-315`.

- [ ] Task 15. Add `--translate-to <code>` per-call flags to `Record` (`crates/fono/src/cli.rs:119-140`) and `Transcribe` (`:143-155`). Empty / `"none"` disables for the call; non-empty overrides `config.translate.target` and forces `enabled = true` for the call. Useful for one-off dictation sessions that target a non-default language.

- [ ] Task 16. Add a `fono use translate <provider>` subcommand under `UseCmd` (`cli.rs:262-284`) so users can switch translation backends without editing TOML, matching the existing `fono use stt` / `fono use llm` ergonomics.

- [ ] Task 17. Add a `fono translate <text> --to <code>` one-shot command (no audio capture) that pipes text directly to the configured `Translator`. Useful for piping clipboard content; aligns with the project's *"unioning Tambourine and OpenWhispr"* feature scope.

### Phase 6 — Docs

- [ ] Task 18. Author `docs/decisions/0017-auto-translation.md` — documents the strategy choice (LLM as default, Whisper / cloud STT as English-only fast paths), the allow-list interaction (source detection still uses the allow-list), and the per-app override mechanism (`[[context_rules]].translate_target`).

- [ ] Task 19. Update `docs/providers.md` with a new "Translation" section listing each backend's supported target languages, latency profile, and cost model. Cross-link to the ADR.

- [ ] Task 20. Append a new `docs/status.md` entry describing the feature, the user-visible config knobs, and the known limitation (`mode = WhisperNative` / `CloudStt` only support English target; `mode = Llm` is the default).

- [ ] Task 21. Add an `### Added — automatic translation` block to `CHANGELOG.md` under `## [Unreleased]`, summarising the new `[translate]` section, the `--translate-to` flag, and the per-app override.

### Phase 7 — Tests + verification

- [ ] Task 22. Unit tests: `Translate::default()` round-trip (`config.rs::tests`); `TranslateMode` / `TranslateWhen` parse + serialise; `ContextRule::translate_target` migration (loading a v0 config without the field should default to `None`).

- [ ] Task 23. `Translator` trait test using a fake backend (`crates/fono-llm/tests/translator_fake.rs`): asserts source / target plumbing, `IfSourceNotTarget` skip logic, and that `target_language` lands in the cleanup `FormatContext` after translation.

- [ ] Task 24. Integration test for the pipeline (`crates/fono/tests/translate_pipeline.rs`): full `run_pipeline` with stub STT (returns `Transcription { text: "buna ziua", language: Some("ro"), … }`) → fake `Translator` (`ro` → `en` returns `"good morning"`) → fake `TextFormatter` cleanup → `CapturingInjector`. Asserts the injected text is the cleaned translation, history row carries both `raw` and `translated`.

- [ ] Task 25. Integration test for `IfSourceNotTarget` skip path: same stub STT but `language: Some("en")` and `target = "en"` should bypass the translator entirely (assert it was never called).

- [ ] Task 26. Mocked-HTTP test for the Groq `/audio/translations` fast path (mirrors `crates/fono-stt/tests/groq_*.rs` patterns): asserts the endpoint URL switches when `translate_to_english` is set and that the response is consumed.

- [ ] Task 27. Final verification per `AGENTS.md`: `cargo build --workspace`, `cargo test --workspace --lib`, `cargo test --workspace`, `cargo clippy --workspace --lib --bins -- -D warnings`. All green. SPDX header on every new `.rs` file. DCO sign-off on every commit.

## Verification Criteria

- A user with `[translate]` disabled sees zero behaviour change on the batch and live paths (zero new LLM round-trips, identical history rows).
- A user with `enabled = true, target = "en", mode = "llm"` and source-language Romanian dictation sees the injected text in English; the history row carries both `raw` (Romanian) and `translated` (English).
- A user with `mode = "whisper-native", target = "en"` triggers the Whisper fast path (no extra LLM call) and the injected text is English; same config with `target = "fr"` logs a warning at startup and silently falls back to `mode = "llm"`.
- A user with `when = "if-source-not-target"`, `target = "en"`, dictating in English bypasses the translator entirely (verified via the fake-translator integration test asserting zero invocations).
- A user with a `[[context_rules]]` matching Slack and `translate_target = "en"` sees Slack-bound dictation translated to English regardless of the global `[translate].target`.
- The language allow-list (`docs/decisions/0016`) continues to constrain *source* language detection unchanged; allow-list violations still trigger the existing post-validation rerun (except on the cloud `/audio/translations` fast path, where the response shape forces us to skip it — documented).
- Live (streaming) dictation produces the same translation behaviour as batch, verified by parity tests.
- `fono translate "buna ziua" --to en` (Task 17) returns `"good morning"` (or equivalent) on stdout without touching audio capture.

## Potential Risks and Mitigations

1. **Cleanup LLM "helpfully" translates back to source language.**
   Anthropic models in particular tend to "normalise" output to whatever they think the user's primary language is. Mitigation: Task 12 explicitly extends `system_prompt()` with the `target_language` directive. Add a regression test (Task 24) that asserts cleaned text is in the target language for at least one `(source, target)` pair.

2. **Latency doubling on the cleanup-then-translate default.**
   Translation + cleanup = two LLM round-trips. Mitigation: ship `before_cleanup = true` as default (translate first, then cleanup runs in the target language and benefits from target-language dictionary). Document the merged-prompt optimisation as a v2 follow-up so users know the latency floor will improve.

3. **Whisper / cloud STT fast paths only support English targets, surprising users.**
   Mitigation: wizard prompts the target *first* and only offers `mode = WhisperNative / CloudStt` when the user picked `"en"`. CLI flag `--translate-to` similarly validates. Daemon logs a warning at startup if a non-English target is paired with an English-only mode and falls back to `Llm`.

4. **Cloud STT `/audio/translations` response drops the `language` field, breaking the allow-list post-validation rerun.**
   Mitigation: skip the rerun on this path explicitly (Task 9) and document the degradation in `0017-auto-translation.md`. Users who care about strict allow-list enforcement should pick `mode = Llm` instead.

5. **History schema migration on a populated SQLite database.**
   Mitigation: `ALTER TABLE … ADD COLUMN translated TEXT` is forward-compatible (`NULL` for legacy rows). Wrap in a transaction; idempotent — running it twice is a no-op (catch the "duplicate column" error and continue). Add a `history::tests::translation_column_migrates_idempotently` test.

6. **Live pipeline drift.**
   The live finalise path duplicates the cleanup logic (`session.rs:991-1021`) — easy to forget when adding new stages. Mitigation: Task 11 hoists the helper into a shared submodule consumed by both call sites. Add a parity test (Task 25) that runs the same input through both paths and asserts identical injected text.

7. **`Transcription.language` reliability varies by backend.**
   `whisper-1` doesn't echo language by default (`openai.rs:93-98`); `mode = "if-source-not-target"` would silently degrade to "always translate". Mitigation: force `response_format = verbose_json` on the OpenAI backend whenever translation is enabled, OR document that `IfSourceNotTarget` requires a backend that echoes language and refuse to start with a clear error otherwise.

## Alternative Approaches

1. **Single merged LLM prompt** ("translate then clean in one call"). Halves latency and token cost vs. separate Translator + TextFormatter calls. Trade-off: quality coupling — a bad translation contaminates cleanup, harder to debug, harder to unit-test (one call returning two things). Recommend: ship the two-stage pipeline first; introduce a `[translate].merge_with_cleanup = true` flag in v2 once the per-stage quality baseline is established.

2. **Translation as a `TextFormatter` chain rather than a new trait.** Reuse the existing `TextFormatter` abstraction by treating translation as just another formatter in a chain (`Translator → Cleanup → final`). Trade-off: forces `Translator` and cleanup to share the same `FormatContext` shape, which is fine today but couples them; harder to swap the translator backend independently of the cleanup backend (a real use case: free local translation via NLLB-class models, paid cloud cleanup). Recommend: keep them separate (Task 1) for the architectural seam.

3. **Skip the `Translator` trait, hardcode "translate via the configured cleanup LLM with a different prompt".** Simpler; one less factory, one less config section. Trade-off: cannot mix-and-match (can't use Anthropic for cleanup but Groq Whisper-large for English-only translation); harder to add NLLB / Argos / Marian local translation later. Recommend against for the same architectural-seam reason.

4. **Translation on the streaming preview lane (mid-utterance).** Show translated text live as the user speaks. Trade-off: requires retranslating every preview token rev (high cost), and translation quality on partial utterances is poor (translators need full sentences for context). Recommend: explicitly out of scope for v1; revisit only if user feedback demands it.
