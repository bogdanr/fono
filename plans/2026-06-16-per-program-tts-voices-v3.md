# Per-Program Voices via a Gendered Voice Palette (v3 — FINAL)

> Supersedes v2. The only change from v2 is the **resolved naming
> decision** (and its rationale): friendly names are **positional +
> gendered** ("Female 1", "Male 2"), NOT human names. Everything else
> below is carried forward from v2 unchanged.

## Resolved decision: positional gendered names

The user chose positional names over human names. Rationale (user's
own): it would be confusing to rename an existing voice that already has
an identity — e.g. relabelling the local `af_heart` voice as "Maria", or
the `ro_RO-mihai-medium` voice as "Robert". Positional labels avoid
clashing with the voice's intrinsic name and read unambiguously by ear.

**Naming scheme:** `Female 1`, `Female 2`, …, `Male 1`, `Male 2`, …
(optionally a `Neutral N` bucket for voices a backend doesn't gender).
The number is the stable slot index within that gender for the active
backend. The intrinsic backend voice (e.g. `af_heart`, `alloy`, a
Cartesia UUID) stays hidden behind the slot.

## Objective

Give every program that talks through Fono its **own voice**, so the
user can tell by ear who is speaking — "that was the coach, that was the
chat, that was the coding tool" — and let them **manually pick** a voice
they like for each, by a friendly gendered label rather than a cryptic
backend id. Solve the real pain: backend ids (`EXAVITQu4vr4xnSDxMaL`,
`a0e99841-…`, `af_heart`) are impossible to remember.

## Findings (carried from v2)

- v1's flat `source_app → voice-id` map was rejected: ids are
  hard to obtain and differ per provider. The mapping must use friendly
  labels, not ids.
- The user's examples are distinct **MCP clients** driving `fono.speak`,
  not just the notification path. Identity must come from the MCP client.
- The MCP `initialize` handshake already carries `clientInfo.name` +
  `version` (`crates/fono-mcp-server/src/protocol.rs:130-135`, parsed
  into `InitializeParams.client_info`,
  `crates/fono-mcp-server/src/protocol.rs:89-97`) but the server
  **drops it** (`crates/fono-mcp-server/src/server.rs:58-77`) and
  `McpContext` has no field for it
  (`crates/fono-mcp-server/src/tools/mod.rs:27-53`). One small wiring
  change unlocks per-program voices for speak/listen/confirm.
- Cloud TTS metadata today is a single `default_voice: &'static str` per
  provider (`crates/fono-core/src/provider_catalog.rs:96-100, 256-272`);
  no list, no gender. The local catalog (`crates/fono-tts/src/voices.rs:43-77`,
  `voices/catalog.json`) has no gender either. A palette with gender
  must be authored (cloud) and derived/declared (local).
- Per-call voice override is honoured by OpenAI-compat cloud backends
  (`crates/fono-tts/src/openai_compat.rs:248-252`) but **ignored by the
  local `LocalRouter`** (`crates/fono-tts/src/local_router.rs:89-119`) —
  needs the v1 fix.
- Kokoro local names encode gender (`af_*`/`bf_*` female; current set
  `af_heart`, `af_bella`, `af_nicole`, `bf_emma` is **all female**;
  `ro_RO-mihai-medium` is male) (`crates/fono-tts/src/voices.rs:307-332`).
  Honouring a male English preference locally needs a male English voice
  added to the mirror.

### Risk prioritisation (highest first)

1. Voice palette data does not exist yet (cloud one-each, local no
   gender).
2. Local backend ignores per-call voice → no-op on-device without a fix.
3. Program identity not captured for speak/listen/confirm.
4. Gender coverage gaps (English local set is all-female).
5. Backend switch invalidates pins (a slot may not exist on the new
   backend).

## Recommended Approach (the layered model)

1. **Voice palette (abstraction).** Each TTS backend exposes a curated
   list (cap ~10) of `PaletteVoice { backend_id, gender }`. The friendly
   label shown to the user is computed positionally: the Nth voice of a
   given gender becomes "Female N" / "Male N". The cryptic id lives only
   in the palette.

2. **Program identity.** Unify two sources behind one normalised key:
   for `fono.speak`/`listen`/`confirm`, the MCP `clientInfo.name`; for
   `fono.summarize`, the payload `source_app` (falling back to
   `clientInfo.name`).

3. **Assignment.** Precedence: (a) explicit `voice` arg (friendly label
   OR raw id) wins; (b) manual `[mcp.voices]` pin for that program;
   (c) automatic deterministic assignment — stable hash of the program
   key onto the gender-filtered palette; (d) backend default. A global/
   per-program gender preference filters the palette before (c).

## Implementation Plan

- [ ] Task 1. **Palette data model.** Add `PaletteVoice { backend_id,
  gender }` and `Gender { Female, Male, Neutral }` in `fono-core`. Add a
  helper that renders the **positional friendly label** ("Female N" /
  "Male N") for a palette entry, and the inverse (parse "female 2" →
  the 2nd female palette entry, case-insensitive). *Rationale:* encodes
  the resolved naming decision in one place.

- [ ] Task 2. **Populate the cloud palette.** Extend `TtsDefaults`
  (`crates/fono-core/src/provider_catalog.rs:94-119`) with
  `voices: &'static [PaletteVoice]`, filling a curated, gender-balanced
  set (≤10) per cloud TTS provider (OpenAI alloy/echo/nova/onyx/…, Groq's
  six Orpheus voices, ElevenLabs premade voices, Cartesia presets,
  Deepgram aura ids, Speechmatics' four), each tagged with gender. Keep
  `default_voice` as fallback. Pin the lists in a drift test like
  `tts_english_only_pinned`. *Rationale:* turns cryptic ids into a
  labelled, gendered menu (Risk 1).

- [ ] Task 3. **Populate the local palette + gender.** Add an optional
  `gender` field to the local catalog `Voice`
  (`crates/fono-tts/src/voices.rs:43-77`, `voices/catalog.json`), derive
  it for Kokoro from the name convention where unset, and add a
  `local_palette(languages)` helper returning gendered voices for the
  active languages. Document that the English local set is all-female
  and adding a male English Kokoro voice to the mirror is a prerequisite
  for a male English preference (Risk 4). *Rationale:* gender separation
  on-device.

- [ ] Task 4. **Capture MCP client identity.** Store `clientInfo.name`
  (+ version) from the initialize handshake
  (`crates/fono-mcp-server/src/server.rs:58-77`) and thread it into
  `McpContext` (`crates/fono-mcp-server/src/tools/mod.rs:27-53`).
  *Rationale:* unlocks per-program voices for speak/listen/confirm
  (Risk 3).

- [ ] Task 5. **Config surface.** Under `[mcp]`
  (`crates/fono-core/src/config.rs:1108-1169`) add: `voices`
  `BTreeMap<String, String>` (program key → friendly label like
  "male 1", or `"auto"`), an optional global `voice_gender`
  preference, and `auto_assign_voices: bool` (default true). All
  defaulted + `skip_serializing_if` empty for backward compatibility.

- [ ] Task 6. **Unified voice resolver.** A pure function in
  `fono-mcp-server`, shared by all four tools + the CLI, taking the
  program key, explicit `voice` arg, active backend, and config, and
  returning the concrete backend voice id per the precedence above.
  Accepts friendly positional labels ("female 2") and raw ids on the
  explicit-arg path. Auto-assignment hashes the program key modulo the
  gender-filtered palette length for stable per-program voices.

- [ ] Task 7. **Wire all call sites.** `fono.speak`
  (`crates/fono-mcp-server/src/tools/speak.rs`), `fono.summarize`
  (`crates/fono-mcp-server/src/tools/summarize.rs:136-184`), CLI
  `summarize_cmd` (`crates/fono/src/cli.rs:721-784`), and prompt TTS in
  `fono.listen`/`fono.confirm` resolve the voice via Task 6 before
  calling `speak_text` (`crates/fono-mcp-server/src/voice_io.rs:458`).

- [ ] Task 8. **Local per-call voice (carryover from v1).** Extend
  `LocalRouter::synthesize`/`voice_for`
  (`crates/fono-tts/src/local_router.rs:89-119`) to honour an explicit
  voice name via `voices::by_name` (using the existing lazy engine
  cache), falling back to language routing when empty/unknown; pin still
  wins. *Rationale:* closes Risk 2.

- [ ] Task 9. **Guided management CLI.** `fono voices list` (show the
  active backend's palette as "Female 1: <intrinsic-name>", "Male 1: …"
  with the intrinsic voice name shown for context), `fono voices set
  <program> <female N|male N|auto>`, `fono voices gender
  <male|female|any>`, `fono voices preview <female N|male N>`. Validate
  the label against the active backend's palette at write time
  (mitigates Risk 5). Showing the intrinsic name in `list`/`preview`
  honours the user's wish not to *rename* the underlying voice — the
  positional label is an addressing scheme, not a rename.

- [ ] Task 10. **(Optional) Auto-discover provider voices.** For
  backends with a voice-list endpoint (Cartesia `/voices`, ElevenLabs
  `/v1/voices`), an opt-in probe that fetches the live list, caps at ~10,
  maps gender from provider metadata, and refreshes the palette.
  Deferred; curated static palette satisfies the core requirement.

- [ ] Task 11. **Docs + tests.** Document the palette, positional
  gendered labels, gender preference, auto vs. manual assignment, and
  per-program mapping in `docs/configuration.md` /
  `docs/coding-agents.md`; update tool descriptions. Unit-test: resolver
  precedence, deterministic auto-assignment stability, gender filtering,
  positional-label ↔ id mapping per backend, graceful fallback when a
  pinned label is absent on the active backend, and the local per-call
  override.

## Verification Criteria

- Auto-assignment on, no manual config: three MCP clients (distinct
  `clientInfo.name`) each consistently speak in a different palette voice
  across restarts; a fourth reuses one only after the palette is
  exhausted.
- `fono voices set coach "female 1"` makes the `coach` program speak as
  the first female palette voice; an explicit per-call `voice` still
  overrides.
- Gender preference = male restricts auto-assignment and the picker to
  male entries, with a clear notice when the active backend has none.
- Positional labels resolve identically on a cloud backend and the local
  backend (latter requires Task 8).
- `fono voices list` shows each positional label alongside the intrinsic
  voice name (no renaming of the underlying voice).
- A config with none of the new keys behaves exactly as today and
  re-serialises without adding them.
- Pre-commit gate passes: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`.

## Potential Risks and Mitigations

1. **Palette authored per backend.** Curate ≤10 gendered voices per
   provider (Task 2) + derive local gender (Task 3); pin with a drift
   test.
2. **Local ignores per-call voice.** Task 8 + tests.
3. **Gender coverage gaps (all-female English local).** Document; add a
   male English Kokoro voice as follow-up; resolver surfaces a clear
   notice and falls back rather than erroring.
4. **Generic/colliding program names.** Allow manual pin to
   disambiguate; `source_app` refines the summarize key; document that
   distinctness needs distinct client names.
5. **Backend switch invalidates pins.** Resolve labels against the
   active palette each call; missing slot → auto + warn.
6. **Scope creep.** Task 10 optional/deferred; v3 ships the curated
   palette with positional labels.

## Alternative Approaches (for the record)

1. **Human names ("Aria", "Marcus").** Rejected by the user: confusing
   to relabel a voice that already has an intrinsic name (heart → Maria,
   Mihai → Robert).
2. **Pure auto-hash, no palette/config.** Zero config but no gender
   control, no manual choice, no friendly labels — kept only as the
   engine behind layer-3 auto-assignment.
3. **Live auto-discovery as primary source.** Always current but
   per-provider schema work + network dependency + unstable ordering;
   better as an optional refresh (Task 10).
