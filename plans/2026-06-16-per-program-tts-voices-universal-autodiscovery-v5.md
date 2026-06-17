# Universal TTS Voice Autodiscovery (Task 10)

> Continues the per-program voices work (`plans/2026-06-16-per-program-tts-voices-v4.md`,
> Tasks 1–9 + the Deepgram fix shipped). This is the deferred **Task 10**, reframed by the
> latest requirement: discovery must be **universal** — one mechanism that serves every
> current and future provider — and must **never break** normal operation when it fails.

## Objective

Let Fono expand a cloud TTS provider's curated voice palette by querying that provider's
live voice catalogue, so single-voice backends (Cartesia, OpenRouter, Speechmatics) and
larger ones alike present a useful, gender-labelled palette (`Female 1`, `Male 2`, …)
without anyone hand-curating cryptic ids. The mechanism must be:

- **Universal** — adding a new provider means filling in a small *declarative* descriptor
  (list URL + how to read id/gender from the JSON), not writing new bespoke code. This
  mirrors the existing `KeyValidation`/`KeyAuth` pattern already in the catalogue
  (`crates/fono-core/src/provider_catalog.rs:172-234`).
- **Bounded** — the resulting palette is capped (default 10) and gender-balanced.
- **Fail-safe** — discovery is opt-in/on-demand, runs off the hot path, and any failure
  (no network, bad key, unparseable JSON, provider with no list endpoint) silently leaves
  the existing curated/static palette in place. Speaking, listing, and resolving never
  depend on a successful probe.

## Current-state findings (sources)

- **Palette today is static.** `active_palette` returns the curated catalogue list for
  cloud backends and the local catalog for on-device (`crates/fono-mcp-server/src/voice_io.rs:454-479`).
  Cloud lists live in `TtsDefaults.voices` (`crates/fono-core/src/provider_catalog.rs:96-126`),
  read via `tts_palette(id)` (`crates/fono-core/src/provider_catalog.rs:756`).
- **The reusable substrate already exists.** `KeyValidation { url, auth: KeyAuth, extra_headers }`
  (`crates/fono-core/src/provider_catalog.rs:197-206`) plus a `KeyAuth`→`reqwest` mapping
  helper (`crates/fono/src/wizard.rs:1920-1939`) already describe, declaratively, how to
  GET a provider endpoint with the key attached. Voice discovery is the same shape:
  a GET to a list endpoint with the same auth. **Reuse this; do not reinvent.**
- **Keys + clients live where expected.** Cloud key resolution is centralised in
  `resolve_cloud`/`resolve_key` (`crates/fono-tts/src/factory.rs:184-243`); `Secrets::resolve`
  turns a key-ref into the actual key. The cloud HTTP clients (`reqwest`) live in `fono-tts`
  behind per-provider features (e.g. ElevenLabs `crates/fono-tts/src/elevenlabs.rs`).
- **Paths for a cache exist.** `paths.cache_dir` / `paths.voices_dir()`
  (`crates/fono-core/src/paths.rs:39,102-103`) and `paths.state_dir`
  (`crates/fono-core/src/paths.rs:40`) are available for a discovered-voices cache.
- **Resolver consumes the palette by position.** Auto-assignment hashes a program name onto
  the gender-filtered palette (`crates/fono-core/src/voice_resolver.rs`). Therefore the
  discovered palette **order must be deterministic** or automatic assignments and positional
  labels would drift between refreshes (see Risks).
- **Provider list endpoints (researched, to confirm at build time):**
  - ElevenLabs: `GET https://api.elevenlabs.io/v1/voices`, auth `xi-api-key`
    (matches `crates/fono-core/src/provider_catalog.rs:606`); per-voice `voice_id` + `name`
    + `labels.gender`.
  - Cartesia: `GET https://api.cartesia.ai/voices`, auth `X-Api-Key`
    (matches `crates/fono-core/src/provider_catalog.rs:568`) + the API-version header it
    already sends; per-voice `id` + `name` + `gender`.
  - OpenAI / Groq: **no per-account TTS voice-list endpoint** — fixed voice set. Descriptor
    is `None`; they keep their curated lists. (Demonstrates the universal layer degrades to
    "no discovery" cleanly.)
  - Deepgram / Speechmatics / OpenRouter: TTS voice sets are effectively fixed or
    undocumented for listing; ship descriptor `None` now, add later when an endpoint is
    confirmed — no code change needed, only a descriptor.

## Design overview

Three pure-data + one network layer, deliberately split so everything except the actual
HTTP call is unit-testable without a network:

1. **Descriptor (fono-core, declarative).** A new `VoiceDiscovery` struct on `TtsDefaults`,
   `Option<VoiceDiscovery>`, modelled on `KeyValidation`:
   - `list_url: &'static str`, `auth: KeyAuth`, `extra_headers: &'static [(…)]`.
   - A declarative **field map**: JSON pointer/key to the voices array, the id field, the
     (optional) display-name field, the (optional) gender field, and a gender-token mapping
     (`"female"|"f"|"feminine" → Female`, etc., reusing `Gender::parse`).
   - An optional `custom: Option<fn(&serde_json::Value) -> Vec<RawDiscoveredVoice>>` escape
     hatch for the rare provider whose JSON the declarative map can't express. Declarative
     path is primary; the hook is the exception, keeping the layer universal.
2. **Pure mapping + cap (fono-core).** `map_discovered(json, &VoiceDiscovery) -> Vec<PaletteVoice>`
   and `cap_and_balance(voices, max) -> Vec<PaletteVoice>` (gender-balanced, deterministic
   order). No I/O — fully unit-tested with fixture JSON.
3. **Network probe (fono-tts).** One async `discover_palette(descriptor, api_key, max)`:
   build GET, attach auth via a **shared** `KeyAuth`→request helper (lifted from the wizard
   into fono-core so wizard + discovery share it), parse, map, cap. Returns `Result`; never
   panics. Feature-gated alongside the existing cloud features.
4. **Cache + consumption (fono-core + fono-mcp-server).** A serialisable `DiscoveredVoices`
   record (`backend`, `fetched_at`, `Vec<PaletteVoice>`) persisted under
   `cache_dir/voices/discovered/<backend>.json`. `active_palette` consults the cache first
   and falls through to the curated/static palette on any miss or read error.

### Precedence (palette source), fail-safe by construction
1. Fresh discovered cache for the active backend (if present and discovery enabled).
2. Curated static catalogue palette (`tts_palette`) — the guaranteed offline fallback.
3. Empty palette ⇒ resolver uses the backend default voice.

Discovery **refresh** is explicit (`fono voices discover`) and/or a best-effort,
non-blocking daemon refresh behind a toggle; the **read** path is always local and
infallible.

## Implementation Plan

- [x] Task 1. **Add the `VoiceDiscovery` descriptor to fono-core.** Define `VoiceDiscovery`
      (and a small `VoiceFieldMap` + `RawDiscoveredVoice`) next to `KeyValidation` in
      `crates/fono-core/src/provider_catalog.rs`, add `discovery: Option<VoiceDiscovery>`
      to `TtsDefaults` (default `None` for every existing entry). Rationale: makes discovery
      a *declarative property of a provider*, so future providers are onboarded by data, not
      code — the core "universal" requirement.

- [x] Task 2. **Lift the `KeyAuth`→request helper into fono-core.** Move the auth-application
      logic from `crates/fono/src/wizard.rs:1920-1939` into a shared, reusable fono-core
      function (e.g. `provider_catalog::apply_key_auth(req, auth, key)`), and have the wizard
      call it. Rationale: discovery and key-validation must attach keys identically; a single
      helper prevents drift and is the reusable spine of the universal mechanism.

- [x] Task 3. **Pure mapping + cap/balance helpers (fono-core).** Implement
      `map_discovered(&serde_json::Value, &VoiceDiscovery) -> Vec<PaletteVoice>` (declarative
      field map first, `custom` hook fallback; unmapped/missing gender ⇒ `Gender::Neutral`)
      and `cap_and_balance(Vec<PaletteVoice>, usize) -> Vec<PaletteVoice>` that yields a
      **deterministic, gender-balanced** subset (e.g. interleave by gender up to the cap,
      stable sort by backend id within gender). Add a `MAX_DISCOVERED_VOICES` const (10).
      Rationale: keeps all logic that can be wrong testable without a network, and pins the
      "bounded list" requirement.

- [x] Task 4. **Async network probe in fono-tts.** Add `discovery` module with
      `discover_palette(&VoiceDiscovery, api_key, max) -> Result<Vec<PaletteVoice>>`: build
      the GET with the shared auth helper + `extra_headers`, enforce a short timeout, parse
      JSON, call `map_discovered` + `cap_and_balance`. Feature-gate per existing cloud flags;
      a build without a provider's feature simply has no probe for it. Rationale: isolates the
      only fallible/networked part; returns `Result` so callers degrade gracefully.

- [x] Task 5. **Discovered-voices cache type + load/save (fono-core).** Define a serde
      `DiscoveredVoices { backend, fetched_at, voices: Vec<PaletteVoice> }`, with
      `load(cache_dir, backend) -> Option<DiscoveredVoices>` (any error ⇒ `None`, never
      propagated) and `save(...)`. Persist under `cache_dir/voices/discovered/<backend>.json`.
      Rationale: moves discovery off the hot path and makes the read path local + infallible.

- [x] Task 6. **Teach `active_palette` to prefer a fresh cache (fono-mcp-server).** In
      `crates/fono-mcp-server/src/voice_io.rs:454-464`, for cloud backends consult
      `DiscoveredVoices::load` first (gated by the new config toggle), falling back to
      `tts_palette(id)` on miss/disabled/error. Local backend path unchanged. Rationale: makes
      every consumer (resolver, `fono voices list`, preview, speak) benefit transparently
      while keeping the curated palette as the guaranteed fallback.

- [x] Task 7. **Config toggle (fono-core).** Add a single opt-out under the TTS/MCP config
      (e.g. `[tts].voice_discovery: bool`, default `true`), serialised via the existing
      `default_true`/`is_true` skip helpers so a default config never grows the key (matches
      the Task 5 pattern from v4, `crates/fono-core/src/config.rs`). Rationale: lets users who
      want strictly curated voices opt out; default-on keeps it useful without setup.

- [x] Task 8. **`fono voices discover` subcommand (fono).** Add `VoicesCmd::Discover { json }`
      to `crates/fono/src/cli.rs:548` and a handler in `voices_cmd`
      (`crates/fono/src/cli.rs:860`) that: resolves the active backend's descriptor + key,
      runs `discover_palette`, on success writes the cache and prints the new labelled palette,
      and on **failure prints an actionable message and exits 0-ish without touching the cache**
      (existing palette preserved). Rationale: gives the explicit, safe refresh entry point;
      reuses the resolver/labelling already in `voices_cmd`.

- [x] Task 9. **Populate descriptors for ElevenLabs and Cartesia.** Fill `discovery: Some(..)`
      for the two providers whose curated lists are weakest (ElevenLabs `/v1/voices`,
      Cartesia `/voices`) with their field maps; leave OpenAI/Groq/Deepgram/Speechmatics/
      OpenRouter at `None` for now. Rationale: delivers immediate value where it matters and
      proves the descriptor covers two genuinely different JSON shapes.

- [x] Task 10. **Optional best-effort daemon refresh (fono).** Behind the Task 7 toggle, on
      daemon startup (or first TTS use) spawn a non-blocking, timeout-bounded refresh of the
      active backend's cache if it is missing/stale; failures are logged at debug only.
      Rationale: keeps palettes current without the user remembering to run `discover`, while
      never blocking or breaking startup. Mark optional — ship Tasks 1–9 first.

- [x] Task 11. **Tests + docs.** Unit-test `map_discovered` (ElevenLabs + Cartesia fixture
      JSON, missing-gender → Neutral, custom-hook path), `cap_and_balance` (determinism,
      gender balance, cap), cache round-trip, and `active_palette` cache-preference +
      fallback-on-corrupt-cache. Document the descriptor pattern and the
      `fono voices discover` flow in `docs/providers.md`. Rationale: locks the fail-safe and
      determinism guarantees; documents how to onboard the next provider declaratively.

## Verification Criteria

- Running `fono voices discover` on an ElevenLabs/Cartesia key produces a labelled,
  gender-tagged palette of at most `MAX_DISCOVERED_VOICES`, and `fono voices list` then shows
  it; `fono voices preview "Male 2"` speaks a discovered voice.
- With **no network / a bad key / a 5xx**, `fono voices discover` reports the failure and the
  previously active palette (curated or last-good cache) is unchanged; `fono.speak`,
  `fono voices list`, and the resolver continue to work unaffected.
- A provider with `discovery: None` (e.g. OpenAI) behaves exactly as today (curated palette);
  no errors, no probes.
- `map_discovered` correctly maps both ElevenLabs (`voice_id`/`labels.gender`) and Cartesia
  (`id`/`gender`) fixture payloads; missing gender → `Neutral`.
- `cap_and_balance` is deterministic (same input ⇒ same order/selection) and respects the cap
  and gender balance.
- `active_palette` prefers a fresh cache, falls back to curated on a corrupt/missing cache,
  and never returns an error.
- With `[tts].voice_discovery = false`, the cache is ignored and the curated palette is used;
  the key is absent from a freshly written default config.
- Full gate green: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -D warnings`,
  `cargo test --workspace`, plus the `tts-local`/cloud-feature-gated suites.

## Potential Risks and Mitigations

1. **Palette reshuffle breaks stable auto-assignment / positional pins.** Auto-assignment and
   `Female N` labels are positional; a changed discovered set would remap them.
   *Mitigation:* deterministic ordering in `cap_and_balance` (stable sort by backend id within
   gender); document that a *refresh* may change assignments if the provider's set changes;
   pins stored as raw backend ids remain stable regardless.
2. **Provider returns no/garbage gender metadata.** Labels would all be `Neutral N`.
   *Mitigation:* accept `Neutral` gracefully (the resolver already treats Neutral as "no
   preference"); allow the curated list's hand-tuned genders to remain the fallback when
   discovery is disabled.
3. **Plan-gated / unusable voices in the list (e.g. ElevenLabs professional voices return 402
   at synth time).** A discovered voice might not actually speak on the user's plan.
   *Mitigation:* prefer premade/owned voices in the field map where the API exposes a category;
   document the 402 behaviour (already noted in `crates/fono-tts/src/elevenlabs.rs:25-29`);
   the curated default remains usable.
4. **Network call on a latency-sensitive path.** *Mitigation:* discovery never runs inline with
   speak/list; only the explicit command or a detached, timeout-bounded daemon task writes the
   cache. Reads are local file I/O with a `None`-on-error contract.
5. **Stale cache after switching providers/keys.** *Mitigation:* cache is keyed per backend;
   `active_palette` reads the active backend's file only; provide `fono voices discover` to
   refresh and document clearing the cache file.
6. **Endpoint/JSON shape assumptions wrong at build time.** *Mitigation:* descriptors are data;
   confirm the two endpoints against live responses during Task 9 and adjust the field map only
   — no structural code change.

## Alternative Approaches

1. **Per-provider bespoke discovery functions** (one `discover_elevenlabs`, one
   `discover_cartesia`, …). Simpler per provider but violates the universality requirement —
   every new provider needs new code. Rejected as the primary design; the `custom` hook keeps
   this option available for genuinely irregular APIs.
2. **Discover live on every `active_palette` call (no cache).** Always fresh, zero new config,
   but puts a network call on the hot path and breaks the fail-safe guarantee. Rejected.
3. **Augment rather than replace the curated list** (merge discovered + curated, dedup by id).
   Richer palettes but non-deterministic ordering and confusing duplicate labels. Rejected in
   favour of "discovered replaces, curated is the fallback," which keeps labels predictable.
4. **Bake discovered voices into the committed catalogue via a build-time script** (like the
   voice-mirror flow). Zero runtime cost and fully deterministic, but stale between releases
   and can't reflect a user's account-specific voices. Could complement runtime discovery later.
