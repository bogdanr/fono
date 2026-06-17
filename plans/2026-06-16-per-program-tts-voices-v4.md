# Per-Program Voices via a Gendered Voice Palette (v4 — FINAL)

> Supersedes v3. Changes from v3: (a) explicitly add two **male English
> Kokoro voices — `am_michael` ("Michael") and `bm_lewis` ("Lewis")** —
> to the local catalog, closing the all-female English gap (former
> Risk 4); (b) a dedicated task + asset prerequisite for them. Naming
> decision (positional gendered labels) and everything else carry over
> from v3 unchanged.

## Naming decision (locked, from v3)

Friendly labels are **positional + gendered** — `Female 1`, `Female 2`,
…, `Male 1`, `Male 2`, … — NOT human renames. Rationale (user's): it
would be confusing to relabel a voice that already has an intrinsic
identity (e.g. `af_heart` → "Maria", `ro_RO-mihai-medium` → "Robert").
The positional label is an **addressing scheme**, not a rename; the
intrinsic voice name (`af_heart`, `am_michael`, `alloy`, a UUID) is shown
beside the label in `fono voices list` for context.

## Local English male voices to add (this revision)

The current English local (Kokoro) set is all female: `af_heart`,
`af_bella`, `af_nicole` (en-us) and `bf_emma` (en-gb)
(`crates/fono-tts/voices/catalog.json:125-194`). Add two males so a
"male" English preference can be honoured on-device:

- **`am_michael`** — American male, `espeak_voice = "en-us"` → palette
  label "Male 1".
- **`bm_lewis`** — British male, `espeak_voice = "en-gb"` → palette
  label "Male 2".

Both are standard Kokoro v1.0 voices sharing the existing model
`kokoro-v1.0-q8f16.ort` (sha `651436…`, 93,914,104 bytes,
`crates/fono-tts/voices/catalog.json:132-134`); each adds only a
per-voice `<name>.style.bin` (the existing female packs are 522,240
bytes — a `[510, 256]` f32 tensor).

## Objective

Give every program that talks through Fono its **own voice** (tell by
ear who is speaking: coach vs. chat vs. coding tool), with **manual
override** by a friendly gendered label rather than a cryptic backend id,
and proper **male/female separation** on every backend.

## Findings (carried from v3)

- v1's flat `source_app → voice-id` map was rejected: ids are
  hard to obtain and differ per provider — mapping must use friendly
  labels.
- The user's examples are distinct **MCP clients** driving `fono.speak`,
  so identity must come from the MCP client, not only the summarize
  payload.
- The MCP `initialize` handshake carries `clientInfo.name` + `version`
  (`crates/fono-mcp-server/src/protocol.rs:130-135`,
  `crates/fono-mcp-server/src/protocol.rs:89-97`) but the server drops it
  (`crates/fono-mcp-server/src/server.rs:58-77`) and `McpContext` has no
  field for it (`crates/fono-mcp-server/src/tools/mod.rs:27-53`). One
  small wiring change unlocks per-program voices for speak/listen/confirm.
- Cloud TTS metadata is a single `default_voice` per provider
  (`crates/fono-core/src/provider_catalog.rs:96-100, 256-272`); no list,
  no gender. Local catalog (`crates/fono-tts/src/voices.rs:43-77`,
  `voices/catalog.json`) has no gender. A gendered palette must be
  authored (cloud) and derived/declared (local).
- Per-call voice override is honoured by OpenAI-compat cloud backends
  (`crates/fono-tts/src/openai_compat.rs:248-252`) but **ignored by the
  local `LocalRouter`** (`crates/fono-tts/src/local_router.rs:89-119`).
- Kokoro names encode gender (`af_*`/`bf_*` female; `am_*`/`bm_*` male).
  Kokoro voices share one model and carry a per-voice style pack
  (`crates/fono-tts/src/voices.rs:307-343`).

### Risk prioritisation (highest first)

1. Voice palette data does not exist yet (cloud one-each, local no
   gender).
2. Local backend ignores per-call voice → no-op on-device without a fix.
3. Program identity not captured for speak/listen/confirm.
4. **(Resolved by Task 3b)** Gender coverage gap for English local —
   closed by adding `am_michael` + `bm_lewis`; remaining sub-risk is the
   asset prerequisite (style packs must be generated + uploaded with
   known SHA-256).
5. Backend switch invalidates pins (a slot may not exist on the new
   backend).

## Recommended Approach (layered model, from v3)

1. **Voice palette** — each backend exposes a curated list (≤10) of
   `PaletteVoice { backend_id, gender }`; the friendly label is the Nth
   voice of a gender ("Female N" / "Male N"); the cryptic id lives only
   here.
2. **Program identity** — unify `clientInfo.name` (speak/listen/confirm)
   and `source_app` (summarize, falling back to `clientInfo.name`) into a
   normalised key.
3. **Assignment** — precedence: explicit `voice` arg → manual
   `[mcp.voices]` pin → automatic stable hash onto the gender-filtered
   palette → backend default; a gender preference filters first.

## Implementation Plan

- [x] Task 1. **Palette data model.** Add `PaletteVoice { backend_id,
  gender }` and `Gender { Female, Male, Neutral }` in `fono-core`, plus
  helpers to render the positional label ("Female N"/"Male N") and parse
  it back to a palette entry (case-insensitive).

- [x] Task 2. **Cloud palette.** Extend `TtsDefaults`
  (`crates/fono-core/src/provider_catalog.rs:94-119`) with
  `voices: &'static [PaletteVoice]`; fill a curated, gender-balanced set
  (≤10) per cloud TTS provider, each gender-tagged; keep `default_voice`
  as fallback; pin in a drift test like `tts_english_only_pinned`.

- [x] Task 3a. **Local gender field + palette helper.** Add an optional
  `gender` field to the local catalog `Voice`
  (`crates/fono-tts/src/voices.rs:43-77`, `voices/catalog.json`), derive
  it for Kokoro from the `a?_`/`b?_` convention where unset, and add a
  `local_palette(languages)` helper returning gendered voices for the
  active languages.

- [x] Task 3b. **Add `am_michael` + `bm_lewis` to the local catalog.**
  Append two Kokoro catalog entries mirroring the existing female ones
  (`crates/fono-tts/voices/catalog.json:125-194`): shared model
  `kokoro-v1.0-q8f16.ort`, `engine = "kokoro"`, `language = "en"`,
  `ort_version`/`release_tag = "ort-1.24.2"`, per-voice
  `am_michael.style.bin` / `bm_lewis.style.bin`, `espeak_voice` of
  `en-us` / `en-gb` respectively, and `gender = "male"`. **Asset
  prerequisite (external):** extract the `am_michael` and `bm_lewis`
  style tensors from the upstream Kokoro v1.0 voice pack as
  `[510,256]` f32 `.style.bin` files, upload them to the `fono-voice`
  mirror under the `ort-1.24.2` release, and record each file's real
  SHA-256 + byte size in the catalog (the resolver verifies the hash on
  download, `crates/fono-tts/src/voices.rs:252-277`). Extend the
  `kokoro_english_voices_are_present_and_well_formed` test
  (`crates/fono-tts/src/voices.rs:307-332`) to cover the two new voices
  and their `en-us`/`en-gb` accents. *Rationale:* closes the all-female
  English gap so "Male 1/Male 2" exist on-device.

- [x] Task 4. **Capture MCP client identity.** Store `clientInfo.name`
  (+ version) from the initialize handshake
  (`crates/fono-mcp-server/src/server.rs:58-77`) into `McpContext`
  (`crates/fono-mcp-server/src/tools/mod.rs:27-53`).

- [x] Task 5. **Config surface.** Under `[mcp]`
  (`crates/fono-core/src/config.rs:1108-1169`): `voices`
  `BTreeMap<String, String>` (program → "male 1"/"female 2"/`"auto"`),
  optional global `voice_gender`, `auto_assign_voices: bool` (default
  true); all defaulted + `skip_serializing_if` empty.

- [x] Task 6. **Unified voice resolver.** Pure, shared by all four tools
  + CLI: program key + explicit `voice` arg + active backend + config →
  concrete backend voice id, per the precedence; accepts positional
  labels and raw ids on the explicit path; auto-assignment hashes the
  program key modulo the gender-filtered palette length for stability.

- [x] Task 7. **Wire all call sites.** `fono.speak`
  (`crates/fono-mcp-server/src/tools/speak.rs`), `fono.summarize`
  (`crates/fono-mcp-server/src/tools/summarize.rs:136-184`), CLI
  `summarize_cmd` (`crates/fono/src/cli.rs:721-784`), and prompt TTS in
  `fono.listen`/`fono.confirm` resolve via Task 6 before `speak_text`
  (`crates/fono-mcp-server/src/voice_io.rs:458`).

- [x] Task 8. **Local per-call voice (carryover from v1).** Extend
  `LocalRouter::synthesize`/`voice_for`
  (`crates/fono-tts/src/local_router.rs:89-119`) to honour an explicit
  voice name via `voices::by_name` (existing lazy cache), falling back to
  language routing when empty/unknown; pin still wins.

- [x] Task 9. **Guided management CLI.** `fono voices list` (palette as
  "Male 1: am_michael", "Male 2: bm_lewis", "Female 1: af_heart", … with
  intrinsic name beside each label), `fono voices set <program> <female
  N|male N|auto>`, `fono voices gender <male|female|any>`, `fono voices
  preview <female N|male N>`; validate labels against the active
  backend's palette at write time.

- [ ] Task 10. **(Optional) Auto-discover provider voices.** Opt-in probe
  of Cartesia `/voices` / ElevenLabs `/v1/voices` to refresh the palette
  (cap ~10, map gender from metadata). Deferred.

- [x] Task 11. **Docs + tests.** Document the palette, positional
  gendered labels (including the new `am_michael`/`bm_lewis` males),
  gender preference, auto vs. manual assignment, per-program mapping
  (`docs/configuration.md`, `docs/coding-agents.md`); update tool
  descriptions. Unit-test resolver precedence, deterministic
  auto-assignment, gender filtering, label↔id mapping, graceful fallback
  for a missing pinned slot, and the local per-call override.

## Verification Criteria

- Auto-assignment on, no config: three MCP clients (distinct
  `clientInfo.name`) consistently speak in different palette voices
  across restarts; a fourth reuses one only after the palette is
  exhausted.
- `fono voices set coach "male 1"` makes the `coach` program speak as
  `am_michael` on the local backend; an explicit per-call `voice` still
  overrides.
- Gender preference = male restricts auto-assignment + picker to male
  entries; on the English local backend this now resolves to
  `am_michael` / `bm_lewis` rather than erroring.
- `fono voices list` shows each positional label beside its intrinsic
  voice name (no rename of the underlying voice).
- Downloading the new voices verifies the catalog SHA-256 and caches the
  style packs (no network on a verified cache hit).
- A config with none of the new keys behaves exactly as today and
  re-serialises without adding them.
- Pre-commit gate passes: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`.

## Potential Risks and Mitigations

1. **Palette authored per backend.** Curate ≤10 gendered voices per
   provider (Task 2) + derive local gender (Task 3a); pin with drift
   tests.
2. **Local ignores per-call voice.** Task 8 + tests.
3. **New male style packs must exist on the mirror with correct hashes.**
   Mitigation: Task 3b asset prerequisite — generate the `[510,256]`
   tensors from the upstream Kokoro pack, upload under `ort-1.24.2`, and
   record real SHA-256 + size; the download verifier
   (`crates/fono-tts/src/voices.rs:252-277`) will reject a mismatch, so a
   wrong hash fails loudly rather than silently.
4. **Generic/colliding program names.** Manual pin disambiguates;
   `source_app` refines the summarize key; document the need for distinct
   client names.
5. **Backend switch invalidates pins.** Resolve labels against the active
   palette each call; missing slot → auto + warn.
6. **Scope creep.** Task 10 optional/deferred.

## Alternative Approaches (for the record)

1. **Human names ("Aria", "Marcus").** Rejected — confusing to relabel a
   voice with an intrinsic name. (Note: `am_michael`/`bm_lewis` keep
   their *intrinsic* Kokoro names "Michael"/"Lewis"; the palette still
   addresses them positionally as "Male 1"/"Male 2".)
2. **Pure auto-hash, no palette/config.** No gender control / manual
   choice / labels — kept only as the engine behind auto-assignment.
3. **Live auto-discovery as primary source.** Per-provider schema work +
   network dependency + unstable ordering; better as optional refresh.
