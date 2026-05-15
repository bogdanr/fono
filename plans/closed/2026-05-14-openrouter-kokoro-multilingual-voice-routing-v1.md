# OpenRouter Kokoro: Multilingual Voice Routing (and TTS-model-swap triage)

## Status: Superseded

## Diagnosis (this is the answer to the user's question)

What you are hearing is **not** a Kokoro defect and **not** an OpenRouter-proxy
defect — it is a misconfiguration on our side. Two facts establish this:

1. **Kokoro v1.0 voices are monolingual, prefixed by language code.**
   Per `huggingface.co/hexgrad/Kokoro-82M/VOICES.md`, there are 54 voices across
   9 locales: `a` American English (e.g. `af_heart`, `am_michael`), `b` British
   English (`bf_emma`…), `e` Spanish (`ef_dora`, `em_alex`), `f` French
   (`ff_siwis`), `h` Hindi, `i` Italian (`if_sara`, `im_nicola`), `j` Japanese,
   `p` Brazilian Portuguese, `z` Mandarin. Each voice was trained on one
   language. Feeding French text into a voice whose first letter is `a`
   (American English) is exactly the "speaking French with a heavy English
   accent" symptom — the model phonemizes via the voice's home-language G2P.

2. **Our OpenRouter TTS client hard-codes one voice and ignores the language
   the caller passes.** Catalogue pins `default_voice = "af_heart"` at
   `crates/fono-core/src/provider_catalog.rs:378`. The shared OpenAI-compat
   client signature already accepts a `_lang: Option<&str>` at
   `crates/fono-tts/src/openai_compat.rs:178` but throws it away — there is no
   language→voice mapping for Kokoro anywhere in the codebase. So every
   synthesis request goes out with `voice=af_heart` regardless of the language
   of the text the assistant just produced or the language the STT layer
   detected.

Cross-checking the user's two suggested replacements against
`https://openrouter.ai/api/v1/models?output_modalities=audio` (live, today):

- `google/gemini-3.1-flash-tts-preview` — **does not exist** on OpenRouter.
- `openai/gpt-4o-mini-tts-2025-12-15` — **does not exist** on OpenRouter.
  The closest OpenRouter offerings are `openai/gpt-audio`, `openai/gpt-audio-mini`,
  and `openai/gpt-4o-audio-preview`, but those are *chat-completions audio*
  (modality `text+audio→text+audio`, reached via `/v1/chat/completions` with
  `modalities: ["text","audio"]` and base64 audio inlined in
  `message.audio.data`). They are **not** OpenAI-compatible
  `/v1/audio/speech` endpoints, so our current `OpenAiCompatTtsClient` cannot
  drive them without a separate chat-shaped client.

Therefore: **fix Kokoro by wiring language→voice routing**. Stop suggesting
the user swap models — the swap candidates aren't real on OpenRouter, and
even if they were, Kokoro stays the right OSI-licensed default per
`docs/decisions/0004-default-models.md`. As a follow-up, opt-in support for
OpenAI's `gpt-audio-mini` (via the chat-completions audio shape) gives a
multilingual cloud fallback for users who want it.

## Objective

Make Kokoro-via-OpenRouter speak each language with a voice native to that
language, driven by the language already detected by STT and/or selected by
the user, while keeping the existing default-voice behaviour for explicitly
overridden voices and English-only flows. Avoid model-swap detours and avoid
churn in the OpenAI/Groq TTS code paths.

## Implementation Plan

### Phase 1 — Kokoro voice catalogue + language router (core fix)

- [ ] Task 1. **Add a `kokoro_voice_map` module under `crates/fono-tts/src/`** that
  exposes (a) a const list of supported `lang_code → default voice` pairs
  drawn directly from the published VOICES.md grading
  (`a→af_heart`, `b→bf_emma`, `e→ef_dora`, `f→ff_siwis`, `h→hf_alpha`,
  `i→if_sara`, `j→jf_alpha`, `p→pf_dora`, `z→zf_xiaoxiao`) and
  (b) a `pick_voice(lang: &str) -> Option<&'static str>` helper that
  canonicalises BCP-47 / ISO 639-1 inputs (`en`, `en-US`, `en-GB`, `es`,
  `es-MX`, `fr`, `fr-FR`, `pt-BR`, `pt-PT`, `zh`, `zh-CN`, `zh-TW`, `ja`,
  `hi`, `it`) to a Kokoro lang_code letter, with `en` mapping to `a` (American
  English) and `en-GB`/`en-AU` mapping to `b`. Rationale: keeps the mapping
  data in one auditable place and lets us extend coverage when Kokoro adds
  voices without touching the client.

- [ ] Task 2. **Teach `OpenAiCompatTtsClient::synthesize` to apply the map only
  for Kokoro models**, gated on `self.default_model.starts_with("hexgrad/kokoro")`
  so OpenAI's `tts-1`, Groq's Orpheus, and any future OpenAI-compat provider
  are completely unaffected. Logic: if (a) the caller did not pass an
  explicit `voice` override, (b) the caller passed a non-empty `lang`, and
  (c) the model is Kokoro, look up `pick_voice(lang)` and substitute it for
  `self.default_voice` in the wire request. Fall back to `self.default_voice`
  on miss. Rationale: keeps the change behind both a model-id guard and a
  caller-opt-in (passing `lang`), preserving the principle of least surprise
  for non-Kokoro paths and explicit voice overrides.

- [ ] Task 3. **Propagate the language from the assistant/dictation pipeline
  into the `synthesize` call.** Audit every call site of `TextToSpeech::synthesize`
  (assistant turn driver, manual /tts CLI path, prewarm/test harness) and pass
  the language Fono already knows: STT's detected language for read-back
  flows, the assistant's reply-language for assistant turns (sniff from
  Anthropic/OpenRouter response metadata if present, else fall back to the
  STT language for the same turn, else the configured UI language). Rationale:
  the trait already takes `lang: Option<&str>` but nothing fills it in; that
  is the missing wire that makes Task 2 effective.

- [ ] Task 4. **Add a fast-path language sniffer for the assistant pipeline.**
  When the assistant reply has no explicit language metadata, run a tiny
  heuristic (Unicode script majority — Latin vs CJK vs Devanagari — plus a
  short stopword bag for `en`/`es`/`fr`/`it`/`pt`) on the first 200 chars of
  the text before sending it to TTS. Rationale: assistant replies are
  generally in the user's language, but the assistant pipeline does not
  currently track this; a cheap classifier avoids a per-request round-trip
  to detect language and prevents fallback to `af_heart` when STT metadata
  is missing (e.g. dictation came from the UI rather than from voice).

- [ ] Task 5. **Surface the resolved voice in logs and in history rows.**
  Extend the existing TTS log line (currently emits provider + model) to also
  emit the resolved voice and the language that produced it; same for the
  history row that captures the synthesis. Rationale: when a user files a
  bug like the one that opened this thread, the log alone should tell us
  whether the router picked correctly without asking the user to repro.

### Phase 2 — Wizard UX: voice + language pinning for Kokoro

- [ ] Task 6. **In the wizard's TTS picker, when OpenRouter (Kokoro) is the
  selected backend, add a language step** after the provider choice. Show
  the same 9 locales the voice map supports, defaulting to the OS locale
  (already detected for the assistant copy). Persist the choice as a new
  `[tts.cloud]` field, e.g. `default_lang = "fr"`. Rationale: most users
  dictate in one language; a single front-loaded choice obviates per-request
  routing and gives a deterministic baseline even when language detection
  silently fails. Routing logic from Phase 1 prefers (in order): explicit
  per-call lang → `default_lang` from config → `default_voice`.

- [ ] Task 7. **Offer a per-language voice override** (advanced step, hidden
  behind the "customize" branch). For each supported Kokoro lang, show the
  voices from VOICES.md with their grades and let the user replace the
  catalogue default. Persist as `voice_overrides_by_lang = { "a" = "af_bella",
  "f" = "ff_siwis", … }` under `[tts.cloud]`. Rationale: matches the
  per-route customisation pattern already in the wizard for assistant
  models and keeps power-user control without complicating the default
  path.

- [ ] Task 8. **Refresh the wizard's catalogue copy for OpenRouter TTS** so
  the entry reads "OpenRouter (Kokoro) — Kokoro / open weights — 9 languages,
  54 voices" instead of the current English-implying tagline. Rationale: the
  user's surprise here is partly a documentation failure; the wizard advertised
  Kokoro as a generic TTS without flagging that voice and language are
  coupled.

### Phase 3 — Tests and regression guards

- [ ] Task 9. **Unit tests for `pick_voice`** covering: bare `fr`, `fr-FR`,
  `EN` (case), `pt-BR` vs `pt-PT`, `zh-CN` vs `zh-TW`, an unknown tag
  (`sv`), and an empty string. Rationale: this map is the new linchpin;
  silent regressions here would re-introduce the original bug.

- [ ] Task 10. **Integration test against the OpenAI-compat client** that
  asserts (a) Kokoro + `lang="fr"` + no voice override → wire request
  carries `voice="ff_siwis"`, (b) Kokoro + explicit voice override →
  the override wins and `lang` is ignored, (c) OpenAI + `lang="fr"` →
  wire request still carries the catalogue default voice (no routing
  for non-Kokoro models). Implement with a `mockito` or in-process
  `wiremock` instance — same pattern already used by the existing
  client tests at `crates/fono-tts/src/openai_compat.rs:413-504`.

- [ ] Task 11. **Wizard test pin** for the new TTS table row (OpenRouter
  language + voice steps appearing in the "customize" walk) and for the
  persisted config shape, matching the style of the existing 28 wizard
  tests.

### Phase 4 — Documentation & follow-up

- [ ] Task 12. **Update `docs/providers.md`** (or create the OpenRouter
  TTS subsection if missing) with a one-paragraph explanation of Kokoro's
  voice-per-language model, the 9 supported locales, and how to override.
  Rationale: this is the natural place a future user will look before
  filing the same bug.

- [ ] Task 13. **Add a follow-up plan stub** for opt-in OpenAI
  `gpt-audio-mini` TTS via OpenRouter's chat-completions audio shape
  (`/v1/chat/completions` with `modalities: ["text","audio"]`, audio
  pulled from `message.audio.data` base64). Scope: new
  `OpenRouterChatTtsClient` (different from `OpenAiCompatTtsClient`),
  separate `TtsEndpoint::OpenRouterChatAudio` variant, separate
  catalogue entry, kept off by default — not a Kokoro replacement,
  just a multilingual fallback for users who want it. Cross-link from
  the existing `plans/2026-05-14-google-chirp-stt-v1.md` so both
  multilingual TTS options are tracked together.

## Verification Criteria

- Synthesising the French sentence "Bonjour, comment allez-vous ?" through
  OpenRouter+Kokoro produces audio with a French voice (default `ff_siwis`),
  not English-accented French.
- Synthesising the same text through OpenAI's `tts-1` produces unchanged
  audio compared to the pre-change baseline (no regression for non-Kokoro
  providers).
- Setting an explicit voice override in `[tts.cloud]` makes that voice win
  over the language-derived voice in 100% of requests.
- `cargo test -p fono-tts -p fono` is green; the 3 new unit tests and the
  integration test pass; no existing test needs to be relaxed.
- `cargo clippy --workspace --no-deps` introduces no new warnings.
- Wizard flow: picking OpenRouter as primary, then accepting defaults,
  results in `secrets.toml` plus `config.toml` carrying
  `[tts.cloud] provider = "openrouter" model = "hexgrad/kokoro-82m"
   default_lang = "<OS-detected>"`.
- The TTS log line for a synthesis includes `voice=ff_siwis lang=fr` (or
  equivalent) so the routing decision is observable.

## Potential Risks and Mitigations

1. **Kokoro G2P quality varies by language.**
   French has <11 h of training data and only one voice; Spanish/Italian/Hindi
   have a handful of low-grade voices. Users dictating in those languages
   may still find quality mediocre even after correct routing.
   Mitigation: document the voice-grade table in `docs/providers.md`,
   surface the grade alongside the voice name in the wizard customize step,
   and keep the OpenAI `gpt-audio-mini` follow-up (Task 13) ready as the
   escape hatch for users who need higher-quality multilingual TTS.

2. **Language detection at the caller can be wrong.**
   The Phase 1 Task 4 heuristic will mis-classify short or code-switched
   utterances.
   Mitigation: prefer STT-provided language when available (Whisper Turbo
   on OpenRouter already returns it for forced-language and allow-list
   modes per the earlier conversation); fall back to `default_lang` from
   config; only as a last resort run the heuristic. Log every fallback
   level so we can tune later.

3. **OpenRouter could change Kokoro's default upstream provider.**
   Today routes to DeepInfra (per the live `/endpoints` query). A future
   re-route could subtly change voice rendering even with identical wire
   inputs.
   Mitigation: pin the upstream provider preference via OpenRouter's
   `provider.order` request field once we are confident in DeepInfra,
   or — equivalently — leave the routing to OpenRouter but treat any
   audible regression as a catalogue-version bump (mirrors how we
   already manage Groq Whisper choices).

4. **Adding `default_lang` to `[tts.cloud]` is a config-schema change.**
   Older `config.toml` files will lack the field.
   Mitigation: make the field `Option<String>` with the existing
   `serde(default)` pattern used elsewhere in `crates/fono-core/src/config.rs`;
   no migration script needed. Document in `CHANGELOG.md` under the next
   release entry per the AGENTS.md rule.

5. **Per-language voice override map is a bigger schema change.**
   Mitigation: ship Task 7 *after* Tasks 1–6 land; if the simpler
   `default_lang` field is enough for ≈ 95 % of users, defer Task 7 to
   a follow-up release rather than gold-plating the initial fix.

## Alternative Approaches

1. **Do nothing in code; document the limitation and tell users to set
   `voice` manually per language.**
   Trade-off: minimum churn, but the wizard already advertises Kokoro as a
   complete default — silently expecting users to know the voice prefix
   convention is hostile to non-English first-time setup. Reject.

2. **Drop Kokoro as the default and pick a multilingual cloud TTS.**
   Trade-off: would require introducing a new chat-completions audio code
   path (OpenAI `gpt-audio-mini`) or wiring native Google Cloud TTS
   (Chirp 3), both of which are larger projects already captured in
   follow-up plans. Also conflicts with the OSI-license-only default
   policy in `docs/decisions/0004-default-models.md` — gpt-audio is
   proprietary, and even on OpenRouter the user pays per-second. Reject
   as the *default* but track as an opt-in via Task 13.

3. **Move voice routing entirely into the assistant pipeline / TTS engine,
   not into the OpenAI-compat client.**
   Trade-off: keeps the OpenAI-compat client provider-agnostic, but
   pushes Kokoro-specific knowledge up into the call sites — every future
   call site would need to remember to apply the map. Centralising in the
   client (gated on `model.starts_with("hexgrad/kokoro")`) is the
   smaller blast radius. Prefer Phase 1 Task 2 as drafted.

4. **Use a single multilingual Kokoro voice (some community fine-tunes
   exist).**
   Trade-off: there is no first-party multilingual Kokoro voice — the
   model architecture itself binds voice to G2P. Community fine-tunes on
   HF would need separate model ids that OpenRouter does not currently
   route. Not actionable until upstream ships.
