# Per-Program Voices via a Friendly Voice Palette (v2)

## Objective

Give every program that talks through Fono its **own voice**, so the
user can tell by ear who is speaking — "that was the coach, that was the
chat, that was the coding tool" — and let them **manually pick** a voice
they like for each. Solve the real pain the user named: backend voice
ids (`EXAVITQu4vr4xnSDxMaL`, `a0e99841-…`, `af_heart`) are impossible to
remember. The solution is a **backend-agnostic voice palette**: a short,
curated, gender-labelled list of friendly-named voices per TTS backend,
which programs are mapped onto either automatically (distinct voice per
program, zero config) or manually (the user pins a friendly name).

This supersedes plan v1 (which only added a raw `source_app → voice-id`
map and inherited the cryptic-id problem). v2 keeps v1's local-backend
fix and shared-resolver ideas but layers a friendly abstraction on top
and unifies program identity across all four tools.

## Why v1 was not enough (findings)

- v1 keyed a flat `source_app → voice-id` map. The user explicitly
  rejected this shape: the ids "sunt dificil de luat" (hard to obtain)
  and differ per provider. **Implication:** the mapping must be in terms
  of *friendly names*, not raw ids.
- v1 only covered the notification path (`fono.summarize`). But the
  user's examples ("coach", "chat", "coding tool") are distinct **MCP
  clients** driving `fono.speak`. **Implication:** identity must come
  from the MCP client, not only the summarize payload.
- The MCP `initialize` handshake already carries `clientInfo.name` +
  `version` (`crates/fono-mcp-server/src/protocol.rs:130-135`,
  parsed into `InitializeParams.client_info`,
  `crates/fono-mcp-server/src/protocol.rs:89-97`), but the server
  currently **drops it** (`crates/fono-mcp-server/src/server.rs:58-77`)
  and `McpContext` has no field for it
  (`crates/fono-mcp-server/src/tools/mod.rs:27-53`). **Implication:**
  one small wiring change unlocks per-program voices for speak / listen
  / confirm.
- TTS voice metadata today is a single `default_voice: &'static str`
  per provider in the catalogue
  (`crates/fono-core/src/provider_catalog.rs:96-100, 256-272, …`); there
  is no list and no gender. **Implication:** to offer a palette with
  gender separation we must extend the catalogue (cloud) and the local
  `voices/catalog.json` schema (local) with a small voice list + gender.
- Per-call voice override is honoured by OpenAI-compat cloud backends
  (`crates/fono-tts/src/openai_compat.rs:248-252`) but **ignored by the
  local `LocalRouter`** (`crates/fono-tts/src/local_router.rs:89-119`).
  **Implication:** the local backend still needs the v1 fix to honour a
  per-call voice. Cartesia/Deepgram/ElevenLabs/Speechmatics each select
  voices differently (UUID, model id, path, premade id) — the palette
  abstraction hides this from the user but the resolver must feed the
  right wire value per backend.
- Kokoro local voices encode gender in the name (`af_*`/`bf_*` = female,
  an `am_*`/`bm_*` male convention; current catalog ships `af_heart`,
  `af_bella`, `af_nicole`, `bf_emma` — all female — plus Piper
  `ro_RO-mihai-medium`, male) (`crates/fono-tts/src/voices.rs:307-332`).
  **Implication:** gender can be derived/declared for local voices; the
  English local palette currently lacks a male voice and would need one
  added to the mirror/catalog to honour a male preference.

### Risk prioritisation (highest first)

1. **Voice palette data does not exist yet** — cloud catalogue has one
   default voice each; local catalog has no gender. Building the palette
   (curated lists + gender) is the foundational work everything else
   depends on.
2. **Local backend ignores per-call voice** — without the v1 fix, the
   whole feature is a no-op for local-TTS users.
3. **Program identity not captured for speak/listen/confirm** — needs
   the `clientInfo.name` wiring; without it, only summarize can vary.
4. **Gender coverage gaps** — a backend's curated palette may be
   single-gender (e.g. current English local set is all female), so a
   "male" request can't be honoured until a voice is added.
5. **Backend switch invalidates pins** — friendly names are resolved
   against the *active* backend's palette, so a pin must degrade
   gracefully (fall back to auto) when the active backend lacks that
   slot.

## Recommended Approach (the layered model)

Three layers, each independently useful:

1. **Voice palette (the abstraction).** Each TTS backend exposes a short
   curated list (cap ~10) of `PaletteVoice { friendly_name, backend_id,
   gender }`. Friendly names are human ("Aria", "Marcus") or positional
   ("female 1", "male 2") — TBD with the user. The palette is the single
   place the cryptic backend id lives; users and config never type ids.

2. **Program identity (who is speaking).** Unify two sources behind one
   key: for `fono.speak`/`listen`/`confirm`, the MCP `clientInfo.name`
   captured at initialize; for `fono.summarize`, the payload
   `source_app` (falling back to `clientInfo.name`). Normalised
   (trim+casefold) into a stable program key.

3. **Assignment (program → palette voice).** Resolution precedence:
   (a) explicit `voice` argument on the call (still wins, may be a
   friendly name OR a raw id); (b) a manual `[mcp.voices]` pin for that
   program key; (c) **automatic** deterministic assignment — hash the
   program key onto the gendered palette so each program stably gets a
   distinct voice with no config; (d) backend default. A global/per-
   program gender preference filters the palette before (c).

This satisfies every stated want: automatic distinctness, manual choice,
friendly names instead of ids, gender separation, and "I just don't like
the default voice" (pin a different palette entry globally).

## Implementation Plan

- [ ] Task 1. **Define the palette data model.** Add a `PaletteVoice`
  type (`friendly_name`, `backend_id`, `gender: Gender`) and a
  `Gender { Female, Male, Neutral }` enum in `fono-core`. Decide the
  friendly-naming scheme with the user (human names vs. positional
  "female 1 / male 1"). *Rationale:* the shared vocabulary every other
  task references.

- [ ] Task 2. **Populate the cloud palette in the catalogue.** Extend
  `TtsDefaults` (`crates/fono-core/src/provider_catalog.rs:94-119`) with
  `voices: &'static [PaletteVoice]` and fill a curated, gender-balanced
  set (≤10) for each cloud TTS provider (OpenAI alloy/echo/nova/…,
  Groq's six Orpheus voices, ElevenLabs premade voices, Cartesia presets,
  Deepgram aura model-ids, Speechmatics' four). Keep `default_voice` as
  the fallback. Pin the lists in a unit test like the existing
  `tts_english_only_pinned` so casual edits can't silently drift.
  *Rationale:* turns cryptic ids into a curated, labelled menu (Risk 1).

- [ ] Task 3. **Populate the local palette + gender.** Add an optional
  `gender` field to the local catalog `Voice`
  (`crates/fono-tts/src/voices.rs:43-77`, `voices/catalog.json`), derive
  it for Kokoro from the name convention where unset, and expose a
  `local_palette(languages)` helper that returns the gendered voices for
  the active languages. Note in the plan that the English local set is
  currently all-female; adding a male English Kokoro voice to the mirror
  is a prerequisite for honouring a male preference locally (Risk 4).
  *Rationale:* gender separation for the on-device path.

- [ ] Task 4. **Capture MCP client identity.** Store `clientInfo.name`
  (and version) from the initialize handshake
  (`crates/fono-mcp-server/src/server.rs:58-77`) and thread it into
  `McpContext` (`crates/fono-mcp-server/src/tools/mod.rs:27-53`) so every
  tool can read the calling program's name. *Rationale:* unlocks
  per-program voices for speak/listen/confirm (Risk 3).

- [ ] Task 5. **Add the config surface.** Under `[mcp]`
  (`crates/fono-core/src/config.rs:1108-1169`) add: a `voices`
  `BTreeMap<String, String>` (program key → friendly palette name or
  the literal `"auto"`), an optional global `voice_gender` preference,
  and an `auto_assign_voices: bool` (default true). All defaulted and
  `skip_serializing_if` empty for backward compatibility. *Rationale:*
  manual control + the automatic-distinctness toggle.

- [ ] Task 6. **Build the unified voice resolver.** A pure function in
  `fono-mcp-server` (shared by all four tools + the CLI) that takes the
  program key, explicit `voice` arg, active backend, and config, and
  returns the concrete backend voice id, applying the precedence in the
  Recommended Approach. Auto-assignment uses a stable hash of the
  program key modulo the gender-filtered palette length, so the same
  program always gets the same voice across restarts. *Rationale:* one
  source of truth; identical behaviour across tools and CLI (mirrors v1
  Task 2 but palette-aware).

- [ ] Task 7. **Wire all call sites to the resolver.** `fono.speak`
  (`crates/fono-mcp-server/src/tools/speak.rs`), `fono.summarize`
  (`crates/fono-mcp-server/src/tools/summarize.rs:136-184`), the CLI
  `summarize_cmd` (`crates/fono/src/cli.rs:721-784`), and — for prompt
  TTS — `fono.listen`/`fono.confirm` resolve the voice through Task 6
  before calling `speak_text` (`crates/fono-mcp-server/src/voice_io.rs:458`).
  *Rationale:* the feature reaches every spoken channel, not just
  notifications.

- [ ] Task 8. **Teach the local backend to honour a per-call voice
  (carryover from v1).** Extend `LocalRouter::synthesize`/`voice_for`
  (`crates/fono-tts/src/local_router.rs:89-119`) to look up an explicit,
  non-empty voice name via `voices::by_name` and use that engine (via
  the existing lazy cache), falling back to language routing when
  empty/unknown; pin still wins. *Rationale:* closes Risk 2 so palette
  selection works on the on-device engine.

- [ ] Task 9. **Add a guided management CLI.** `fono voices list`
  (show the active backend's palette with friendly name + gender),
  `fono voices set <program> <friendly-name|auto>`, `fono voices gender
  <male|female|any>`, `fono voices preview <friendly-name>`. Validates
  the friendly name against the active backend's palette at write time —
  the best mitigation point for Risk 5. *Rationale:* users configure by
  ear and by friendly name, never by editing cryptic ids in TOML.

- [ ] Task 10. **(Optional) Auto-discover provider voices.** For
  backends with a voice-list endpoint (Cartesia `/voices`, ElevenLabs
  `/v1/voices`), add an opt-in probe that fetches the live list, caps it
  at ~10, maps gender from the provider's metadata, and merges into the
  palette so the curated static list can be refreshed. *Rationale:*
  keeps palettes current without catalogue edits; deferred because each
  provider's list schema differs and the curated static palette already
  satisfies the core requirement.

- [ ] Task 11. **Docs + tests.** Document the palette, friendly names,
  gender preference, auto vs. manual assignment, and per-program mapping
  in `docs/configuration.md` / `docs/coding-agents.md`, and update the
  tool descriptions. Unit-test: resolver precedence, deterministic
  auto-assignment stability, gender filtering, friendly-name → id
  mapping per backend, graceful fallback when a pinned name is absent
  from the active backend, and the local per-call override. *Rationale:*
  locks in the two correctness-critical pieces (assignment + local fix).

## Verification Criteria

- With auto-assignment on and no manual config, three different MCP
  clients (distinct `clientInfo.name`) each consistently speak in a
  different palette voice across restarts; a fourth client reuses one of
  the voices only after the palette is exhausted.
- `fono voices set coach "Aria"` makes everything from the `coach`
  program speak as Aria; an explicit per-call `voice` still overrides.
- Setting the gender preference to male restricts both auto-assignment
  and the `set` picker to male palette entries (and surfaces a clear
  notice when the active backend has no male voice).
- Friendly names work identically on a cloud backend and on the local
  backend (the latter requires Task 8).
- A config with none of the new keys behaves exactly as today and
  re-serialises without adding them.
- Project pre-commit gate passes: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`.

## Potential Risks and Mitigations

1. **Palette must be authored per backend.** Mitigation: curate ≤10
   gender-balanced voices per provider in the catalogue (Task 2) +
   derive local gender from naming (Task 3); pin with a drift test.
2. **Local backend ignores per-call voice.** Mitigation: Task 8 +
   tests.
3. **Gender coverage gaps (e.g. all-female English local set).**
   Mitigation: document the gap, add a male English Kokoro voice to the
   mirror as a follow-up; resolver surfaces a clear "no male voice for
   this backend" notice and falls back rather than erroring.
4. **Program names collide / are generic** (two tools both report
   "claude-code"). Mitigation: allow a manual pin to disambiguate; let
   `source_app` refine the key on the summarize path; document that
   distinctness needs distinct client names.
5. **Backend switch invalidates friendly pins.** Mitigation: resolve
   pins against the active backend's palette each call; missing slot →
   fall back to auto + warn (Task 6/9).
6. **Scope creep (auto-discovery, full TTS routing).** Mitigation:
   Task 10 is explicitly optional/deferred; v2 ships the curated palette.

## Alternative Approaches

1. **Positional friendly names only ("female 1..5 / male 1..5").**
   Simplest to author and explain; gender is in the name itself. Trade-
   off: less memorable than human names, but zero ambiguity and trivial
   to map. Good default if the user prefers minimalism.
2. **Human-named palette ("Aria", "Marcus", …) mapped to ids.** More
   natural to reference by ear; needs a curated name per slot per
   backend. Slightly more authoring, nicer UX.
3. **Pure auto-hash, no palette/config** (each program gets a stable
   voice from hashing its name onto the raw backend voice list). Zero
   config and zero authoring, but no gender control, no manual choice,
   and no friendly names — fails the user's "manual + gender" wants. Best
   kept as the *engine* behind layer 3, not the whole solution.
4. **Live auto-discovery as the primary source** (Task 10 promoted).
   Always-current palettes, but per-provider schema work, network
   dependency at config time, and unstable ordering. Better as an
   optional refresh on top of the curated static palette.
