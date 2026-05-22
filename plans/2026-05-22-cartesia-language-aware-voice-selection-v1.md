# Cartesia Language-Aware Voice Selection

## Objective

Pair each language the user has configured (or the language detected for a given
utterance) with an appropriate Cartesia voice, so the assistant speaks Romanian
text with a Romanian voice, French text with a French voice, and so on — instead
of always falling back to Cartesia's single "Sonic English Female" UUID
hard-coded at `crates/fono-core/src/provider_catalog.rs:410` and the literal
`"en"` language hint at `crates/fono-tts/src/cartesia.rs:50`.

The resolution must be deterministic, testable offline, configurable by power
users, and degrade gracefully (unknown language → today's English fallback —
never an error or silent skip).

## Current State (one-screen audit)

- `CartesiaTts` stores a single `voice_id` and a hard-coded `language = "en"`
  (`crates/fono-tts/src/cartesia.rs:25-53`). The synth path ignores the trait's
  `lang: Option<&str>` parameter at `crates/fono-tts/src/cartesia.rs:133-150`.
- Trait contract already exposes the slot: `TextToSpeech::synthesize(text,
  voice, lang)` (`crates/fono-tts/src/traits.rs:24-29`). Backends are allowed to
  ignore it; today Cartesia does.
- The assistant calls `tts.synthesize(sentence, None, None)`
  (`crates/fono/src/assistant.rs:451`) — neither voice nor lang is threaded.
- User-configured languages live in `general.languages: Vec<String>`
  (`crates/fono-core/src/config.rs:133`) as BCP-47 alpha-2 codes. Single-entry
  means "force this language"; multi-entry means "allow this set"; empty means
  "auto-detect" (`crates/fono-core/src/config.rs:121-148`).
- `Tts` config (`crates/fono-core/src/config.rs:374-401`) has a single `voice:
  String` field — no per-language map and no per-backend sub-table beyond
  `TtsCloud` / `TtsWyoming`.
- Provider catalogue exposes `TtsDefaults { model, default_voice, endpoint,
  runtime_probe }` (`crates/fono-core/src/provider_catalog.rs:94-108`) — one
  string slot, no language axis.

Implication: language-aware voice selection requires (a) a lookup table, (b) a
config surface for overrides, (c) a resolver inside `CartesiaTts`, and (d) at
least one caller (assistant) threading a useful `lang` hint.

## Design Decisions and Assumptions

1. **Cartesia-only scope for v1.** OpenAI / Groq / OpenRouter / Deepgram have
   their own voice catalogues with very different semantics (OpenAI voices are
   already multilingual; Deepgram voices are model names). Generalising to a
   per-backend trait method is premature — solve Cartesia first, lift later.
2. **Voice picking is a pure function** of `(requested_voice, requested_lang,
   user_voice_map, baked_defaults, fallback_voice)` returning a single voice id
   plus the language code to send on the wire. Keep it in a free function with
   exhaustive unit tests; no I/O.
3. **Three-tier override stack** (highest precedence first):
   1. Explicit `tts.voice` config / per-call `voice` arg — user wants this
      exact voice regardless of language. **Today's behaviour preserved.**
   2. User map in a new `[tts.cartesia]` sub-table: `voices = { en = "<uuid>",
      ro = "<uuid>", fr = "<uuid>" }`.
   3. Baked-in defaults: a small const table inside the Cartesia client
      mapping the most common BCP-47 alpha-2 codes Cartesia supports (English,
      Spanish, French, German, Portuguese, Chinese, Japanese, Korean, Italian,
      Dutch, Polish, Russian, Turkish, Hindi) to known-good public voice
      UUIDs. Unknown language → catalogue `default_voice`.
4. **Language source order** when the caller passes `lang = None`:
   1. The trait's `lang: Option<&str>` argument — once the assistant pipeline
      threads STT's detected language through (future caller change).
   2. The config's `general.languages` field when it has exactly **one** entry
      — this is the "force this language" mode and is the safest signal that
      the user truly speaks that language.
   3. Otherwise the existing `"en"` fallback.
   Multi-language allow-lists (`languages = ["en", "ro"]`) are explicitly
   **not** used to pick a voice — there's no honest tie-break. The detected-
   language path (1) is the right place to handle bilingual users.
5. **Wire `language` field tracks the resolved language**, not the voice's
   native language. Cartesia's request body has a top-level `language` field
   (`crates/fono-tts/src/cartesia.rs:14`, `:95`); sending the correct value
   lets Sonic-2's multilingual voices pronounce correctly even when the
   resolved voice is itself multilingual.
6. **No live `GET /voices` call in v1.** Cartesia exposes a voice catalogue
   endpoint but adding a runtime probe pulls in: caching, version skew, an
   extra failure mode at startup, and a network dependency for offline-ish
   environments. The const table is the conservative first step; a future
   ADR can revisit if voice churn becomes a real maintenance burden.
7. **Assistant caller stays `None`-passing in v1.** Threading STT's detected
   language end-to-end is a separate, larger change (touches the STT trait
   return shape, the dispatcher, history rows). With the static config path
   in place the single-language user (`languages = ["ro"]`) is already
   served. Multilingual threading is called out as a follow-on.
8. **Catalogue keeps `default_voice` as the "language unknown" fallback** so
   no other call sites change.
9. **No wizard surface in v1.** Power users edit `~/.config/fono/config.toml`.
   Wizard integration (voice picker per language) is plausible later but
   would multiply the wizard's branching dramatically; defer.
10. **Voice UUIDs come from Cartesia's public starred voice list** and are
    documented inline with their `name` and source date, so an agent can
    re-verify them against `https://play.cartesia.ai/voices` without
    archaeology.

## Implementation Plan

- [ ] Task 1. **Extend the `Tts` config struct with an optional Cartesia
      sub-table.** Add `pub cartesia: Option<TtsCartesia>` to `Tts` at
      `crates/fono-core/src/config.rs:376-389`, gated behind
      `skip_serializing_if = "Option::is_none"` so existing config files round-
      trip unchanged. Define `TtsCartesia { voices:
      std::collections::BTreeMap<String, String> }` (BTreeMap for stable TOML
      output) with serde defaults. Rationale: keeps the per-backend surface
      pattern already used for `TtsCloud` / `TtsWyoming`; uses `Option<>` so
      the table is omitted from serialised defaults; map keys are lowercase
      BCP-47 alpha-2 codes (`"en"`, `"ro"`, …). Add round-trip tests parallel
      to `languages_round_trip_serializes_plural_only`
      (`crates/fono-core/src/config.rs:1282`).

- [ ] Task 2. **Bake a baked-in language → voice id table inside
      `crates/fono-tts/src/cartesia.rs`.** Add a private `const VOICE_TABLE:
      &[(&str, &str, &str)]` of `(lang_code, voice_uuid, human_name)` triples
      covering the languages Cartesia officially supports today (en, es, fr,
      de, pt, zh, ja, ko, it, nl, pl, ru, tr, hi). Each row carries a short
      `// name + verified <YYYY-MM-DD>` comment so future agents can audit
      against Cartesia's catalogue. The catalogue's `default_voice` stays as
      the "no language match" fallback. Rationale: keeping the table next to
      the request builder localises Cartesia-specific knowledge; the
      catalogue stays provider-agnostic.

- [ ] Task 3. **Introduce a pure resolver
      `resolve_voice_and_language`.** Free function in the Cartesia module:
      input = `(requested_voice: Option<&str>, requested_lang: Option<&str>,
      user_map: &BTreeMap<String, String>, fallback_voice: &str,
      fallback_lang: &str)`, output = `(voice_id: String, language: String)`.
      Order: explicit `requested_voice` wins (paired with requested or
      fallback lang); else if `requested_lang` is `Some`, look it up in user
      map → baked table → fallback voice (lang on wire still set to the
      requested code so pronunciation matches even when the voice is the
      generic English UUID); else fall back to `(fallback_voice,
      fallback_lang)`. Keep the function `#[must_use]` and fully covered by
      tests in `crates/fono-tts/src/cartesia.rs:177-223`.

- [ ] Task 4. **Refactor `CartesiaTts` to hold the override surface, not a
      frozen string pair.** Replace the current `voice_id` + `language`
      fields with `default_voice: String`, `default_language: String`, and
      `user_map: BTreeMap<String, String>`. Constructor signature becomes
      `pub fn new(api_key, model_override, voice_override,
      default_language, user_map)` where `voice_override` continues to mean
      "ignore language entirely". Default language: when the caller has a
      single-entry `general.languages`, that entry; else `"en"`. Compute
      this in the factory (Task 5) so the TTS crate stays free of `Config`
      coupling.

- [ ] Task 5. **Wire the resolver into `synthesize`.** At
      `crates/fono-tts/src/cartesia.rs:124-150` call
      `resolve_voice_and_language(_voice, _lang, &self.user_map,
      &self.default_voice, &self.default_language)` and feed the result into
      `build_request_body` (which becomes lang-aware: accept `voice_id` and
      `language` parameters instead of reading `self`). Update existing
      tests `request_body_shape_matches_spec` and
      `cartesia_client_uses_catalogue_defaults` to cover the new shape; add
      cases for (a) explicit voice override beats lang, (b) user map beats
      baked table, (c) baked table beats fallback, (d) unknown lang falls
      back to default voice while still sending the requested lang on the
      wire.

- [ ] Task 6. **Update `build_cartesia` in
      `crates/fono-tts/src/factory.rs:182-187`** to thread the new inputs:
      pull the single-language hint from `cfg.general.languages` (or
      whatever struct the factory has access to — if the factory only sees
      `&Tts`, expose the hint via a new arg propagated from the call site in
      `crates/fono/src/assistant.rs` or wherever the TTS is built). Read
      `cfg.cartesia.as_ref().map(|c| c.voices.clone()).unwrap_or_default()`
      for the user map. Add a unit test mirroring
      `cartesia_with_key_succeeds`
      (`crates/fono-tts/src/factory.rs:294-300`) that asserts the user map
      and language hint reach the constructed client.

- [ ] Task 7. **Audit other TTS construction sites** for the new
      constructor signature. Likely just `factory.rs` and the existing
      tests in `crates/fono-tts/src/cartesia.rs:177-223`; the smoke
      assistant example (`crates/fono/examples/smoke_assistant.rs:337,463`)
      passes `None, None` and does not construct backends directly so it
      should be untouched.

- [ ] Task 8. **Document the new surface.** Update `docs/providers.md` —
      the Cartesia TTS section already at `docs/providers.md:367-376` —
      with a "Language-aware voice selection" subsection: TOML example,
      precedence rules, list of baked-in defaults, link to Cartesia's voice
      catalogue. Keep wording short; the resolver itself is the source of
      truth.

- [ ] Task 9. **Add a `CHANGELOG.md` entry under the next release
      heading** describing the new behaviour, the new `[tts.cartesia]`
      block, and the preserved fallback. Per AGENTS.md this is required
      before tagging; better to land with the feature than to remember at
      release time.

- [ ] Task 10. **(Optional / follow-on, do not block v1.)** Thread STT's
      detected language through to TTS. Capture the language returned by
      cloud STT (`detected_language` already used by
      `cloud_rerun_on_language_mismatch`,
      `crates/fono-core/src/config.rs:141`) into the assistant turn's state
      and pass it as the `lang` arg in
      `crates/fono/src/assistant.rs:451`. Open a separate plan; the static
      path in Tasks 1-9 already covers the single-language user.

## Verification Criteria

- A config with `general.languages = ["ro"]` and no `tts.cartesia` block
  causes the next assistant TTS request to send `language = "ro"` and a
  Romanian voice UUID from the baked table, verifiable by a unit test that
  inspects the JSON body returned from `build_request_body`.
- A config with `[tts.cartesia.voices] ro = "<custom-uuid>"` overrides the
  baked Romanian voice, verifiable by a unit test on
  `resolve_voice_and_language`.
- Setting `tts.voice = "<some-uuid>"` continues to force that voice
  regardless of language (today's contract preserved), verifiable by a unit
  test.
- An unknown language code (e.g. `"xx"`) falls back to the catalogue's
  `default_voice` but still sends `language = "xx"` on the wire — no
  panics, no errors.
- Default `Config { tts: Tts::default() }` serialises to TOML without
  emitting an empty `[tts.cartesia]` table (round-trip test).
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --
  -D warnings`, and `cargo test --workspace --tests --lib` all pass.
- Existing tests in `crates/fono-tts/src/cartesia.rs:177-223` and
  `crates/fono-tts/src/factory.rs:294-300` pass after refactor (signature
  changes accommodated, behaviour preserved).

## Potential Risks and Mitigations

1. **Voice UUIDs drift in Cartesia's catalogue.**
   Mitigation: pick public, starred voices that have been stable for
   months; comment each row with verification date; document the audit
   procedure in `docs/providers.md`; a `GET /voices` probe behind an
   opt-in flag is a future ADR if churn becomes painful.
2. **A user's existing `tts.voice` setting silently stops being used after
   the refactor.**
   Mitigation: keep `tts.voice` as the highest-precedence override and
   add an explicit unit test that proves it wins over both the user map
   and the baked table.
3. **Cartesia rejects requests where the resolved voice isn't valid for
   the resolved language** (some voices are single-language).
   Mitigation: in v1 we only ever pair a voice UUID with its known native
   language; the baked table is the source of truth. If a user maps a
   single-language voice to a non-matching language they're on their own,
   and the error surfaces verbatim through the existing `cartesia TTS
   returned 400 …` path — acceptable for an explicit power-user override.
4. **Config schema migration confuses existing users.**
   Mitigation: the new `[tts.cartesia]` block is `Option<>` and
   `skip_serializing_if = Option::is_none`. Old configs load unchanged;
   `fono use tts cartesia` continues to work without writing the block.
5. **Test surface grows; resolver becomes a maintenance hot-spot.**
   Mitigation: keep `resolve_voice_and_language` pure and table-driven —
   a single `#[test]` with a `[(input, expected)]` array covers every
   precedence rule in ~30 lines.

## Alternative Approaches

1. **Live `GET https://api.cartesia.ai/voices` lookup at startup, cache
   in `~/.cache/fono/cartesia_voices.json`.** Always current; survives
   Cartesia's catalogue churn. Trade-off: extra HTTP at startup, an
   additional failure mode (offline-but-configured users see a degraded
   experience until the cache exists), version-skew handling needed, and
   testing requires either a mock HTTP layer or a snapshot fixture. Not
   worth it for v1 when the supported-language list changes maybe twice
   a year.

2. **Per-call language detection from the assistant reply text** (langid
   crate or fastText-style detector). Avoids any config — language is
   inferred from what the model emitted. Trade-off: pulls in a dependency
   (review under `deny.toml` and the GPL-3 compatibility rule), adds a
   non-trivial per-sentence cost, and fights the trait's existing `lang:
   Option<&str>` slot which already gives us a clean injection point from
   STT. Better to populate that slot from STT (Task 10) than to detect
   downstream.

3. **Generic per-backend voice-by-language map in the provider catalogue
   itself**, applied to every TTS backend uniformly. Trade-off: forces
   OpenAI / Groq / Deepgram models to either implement the same surface
   or stub it out, even though their voice semantics differ (OpenAI's
   `alloy` is multilingual; Deepgram's voice = model id). Premature
   generalisation; revisit if a second backend ever needs the same
   pattern.

4. **A single `[tts.voices]` map at the top of `Tts` (not nested under a
   per-backend table).** Simpler to type, but couples voice naming
   conventions across providers (Cartesia UUIDs alongside OpenAI string
   names) and forces the resolver to know which backend it's building
   for. The per-backend sub-table (`[tts.cartesia]`) keeps the namespace
   clean.
