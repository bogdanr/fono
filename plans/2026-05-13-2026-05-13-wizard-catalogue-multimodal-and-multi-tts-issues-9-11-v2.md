# Wizard rework v2 — provider capability matrix, primary-provider collapse, assistant extras, multi-provider TTS (issues #9 + #11)

Supersedes `plans/2026-05-12-2026-05-12-wizard-primary-provider-collapse-issue-9-v1.md`.

Tracks: <https://github.com/bogdanr/fono/issues/9> (parent) and
<https://github.com/bogdanr/fono/issues/11> (sub-issue, TTS coverage).

## Objective

Deliver three coordinated changes behind one capability matrix:

1. **Issue #9 — Wizard collapse.** A cloud user enters **one API key,
   once**, and walks away with every capability that key can drive
   (STT, LLM cleanup, assistant chat, TTS). Power users keep an
   explicit "Customize per capability" escape hatch.
2. **Multimodal & web-search assistant extras.** When the chosen
   primary provider offers vision-capable or web-search-capable
   models, the assistant overlay surfaces a single optional
   `MultiSelect` row that flips between text-only and richer model
   variants and toggles a server-side web-search tool. Zero new
   prompts for users who don't care.
3. **Issue #11 — Multi-provider TTS.** Replace the OpenAI-only TTS
   monopoly with additional providers (Groq, **OpenRouter with
   Kokoro**, Cartesia, Deepgram) so the assistant works for users
   who picked a non-OpenAI primary. OpenRouter's
   `/api/v1/audio/speech` endpoint is confirmed live and
   OpenAI-compatible — default model `hexgrad/kokoro-82m`, default
   voice `af_heart`.

The three changes share one substrate — a capability catalogue in
`fono-core` — so they ship together as one coherent release.

## Initial assessment

### Wizard pain points (unchanged from v1)

The current cloud branch (`crates/fono/src/wizard.rs:46-128`) asks for
up to four independent API keys. Capability information is hardcoded
across five `match` blocks (`wizard.rs:161-209, 232-285, 1074-1107,
1116-1154`, plus the assistant submenu reuse logic at lines 223-252).
Re-runs prompt "keep existing `GROQ_API_KEY`?" up to three times in a
row even when the user already entered it on a prior pass.

### TTS pain points (issue #11)

`crates/fono-tts/src/openai.rs:1-192` is the only cloud TTS client and
hard-codes `https://api.openai.com/v1/audio/speech`. Wizard
(`wizard.rs:226-285`), tray (`crates/fono-tray/src/lib.rs` TTS
submenu), and `fono use` all treat OpenAI as the sole cloud TTS
option. Anyone whose primary cloud key isn't OpenAI cannot run the
assistant with audio output unless they also obtain an OpenAI key —
exactly the symptom issue #11 calls out.

### Reality check on TTS providers (mid-2026)

| Provider | TTS available? | API shape | Notes |
|---|---|---|---|
| **OpenAI** | ✓ | `POST /v1/audio/speech` (PCM) | Already wired (`crates/fono-tts/src/openai.rs`). |
| **Groq** | ✓ | `POST /openai/v1/audio/speech` — **OpenAI-compatible** | Models: `playai-tts`, `playai-tts-arabic`. Beta tier-of-service flag; document but no consent gate needed. |
| **Cartesia** | ✓ | `POST /tts/bytes` — native shape, model `sonic-2` | Already a Fono STT provider (`CARTESIA_API_KEY` may already exist). Sonic-2 quality is widely regarded as best-in-class for the latency budget. |
| **Deepgram** | ✓ | `POST /v1/speak` — native shape, model `aura-2-thalia-en` | Already a Fono STT provider (`DEEPGRAM_API_KEY` may already exist). |
| **OpenRouter** | ✗ (as of 2026-05) | No published `/v1/audio/speech` endpoint | OpenRouter is chat-routing only today. Plan adds a guarded catalogue stub pointing at `https://openrouter.ai/api/v1/audio/speech`; the wizard never shows it as a choice unless a runtime `HEAD /v1/audio/speech` probe succeeds (deferred wiring — keeps the door open without lying to users). |
| **ElevenLabs** | ✓ | Native | High quality, niche pricing, new provider in fono — left out of the first wave to keep the scope tight. |
| **Azure** | ✓ | Native (Cognitive Services Speech) | Already enumerated as an STT backend with `AZURE_API_KEY`. Listed in deferred-work. |
| **Wyoming-Piper** | ✓ (local) | Wyoming protocol | Already wired (`crates/fono-tts/src/wyoming.rs`). |

The first-wave TTS additions become **Groq + Cartesia + Deepgram**.
Of these, Groq slots in as a parameterisation of the existing OpenAI
client (same payload, different base URL); Cartesia and Deepgram need
small native clients. All three keys are likely already present in
`secrets.toml` for users coming via STT, so a returning user enabling
the assistant doesn't need a new key prompt — which is the precise
fix issue #11 asks for.

### Revised capability matrix (post-implementation)

| Provider | STT | LLM cleanup | Assistant chat | Vision | Web search | TTS |
|---|---|---|---|---|---|---|
| **OpenAI**     | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| **Groq**       | ✓ | ✓ | ✓ | ✓ (Maverick/L3-Vision) | ✗ | ✓ (PlayAI, **new**) |
| **Anthropic**  | ✗ | ✓ | ✓ | ✓ | ✓ | ✗ |
| **Cerebras**   | ✗ | ✓ | ✓ | ✗ | ✗ | ✗ |
| **Gemini**     | ✗ | ✓ | ✓ | ✓ | ✓ | ✗ |
| **OpenRouter** | ✗ | ✓ | ✓ | (route-dependent) | ✗ | (stub — deferred until endpoint exists) |
| **Cartesia**   | ✓ | ✗ | ✗ | ✗ | ✗ | ✓ (Sonic-2, **new**) |
| **Deepgram**   | ✓ | ✗ | ✗ | ✗ | ✗ | ✓ (Aura-2, **new**) |
| **AssemblyAI** | ✓ | ✗ | ✗ | ✗ | ✗ | ✗ |

Post-change, **five providers** can drive the assistant end-to-end
(OpenAI, Groq, Anthropic + Groq/Cartesia/Deepgram TTS, Gemini + same,
or any pairing thereof) instead of OpenAI-only.

### Key design decisions / assumptions

- **One capability catalogue, three consumers.** The new
  `fono_core::provider_catalog` is the single source of truth for the
  wizard (issue #9), the assistant extras prompt, and the
  multi-provider TTS picker (issue #11). `fono use cloud <name>`'s
  legacy `cloud_pair` is rerouted through the catalogue.
- **OpenAI-compatible TTS as a parameterised client.** Refactor
  `crates/fono-tts/src/openai.rs` into `openai_compat.rs` taking
  `base_url`, `default_model`, `default_voice`, and an `auth_header`
  enum (`Bearer` vs custom). Use it for both `OpenAI` and `Groq`
  (and the speculative `OpenRouter` slot). Native clients for
  Cartesia + Deepgram live in their own modules.
- **No new dependencies.** All three changes are pure rearrangement
  + new catalogue + new HTTP request shapes; everything routes
  through the existing `reqwest` warm-client pattern.
- **Forward-compatible config.** `[tts]` gains the existing
  `backend = "openai" | "groq" | "cartesia" | "deepgram" | "wyoming"
  | "openrouter" | "none"` set; `[assistant]` gains
  `prefer_vision: bool` and `prefer_web_search: bool` (both default
  `false`). Old configs upgrade with no migration.
- **Runtime opt-in for OpenRouter TTS.** The catalogue carries
  OpenRouter's hypothetical TTS endpoint URL but the wizard hides
  it unless a one-shot `HEAD` probe at first-run returns 2xx/405.
  Costs one HTTP request when the user picks OpenRouter as primary;
  zero cost otherwise. If/when OpenRouter ships TTS, the wizard
  lights it up with no code change.
- **Vision is a (provider, model) pair, not a provider boolean.**
  `AssistantDefaults` carries `text_model` and
  `multimodal_model: Option<&str>`; the wizard's "prefer vision"
  toggle swaps between them. Cerebras/Cartesia/Deepgram-only
  setups suppress the vision row entirely.
- **Web search is a server-side tool for OpenAI / Anthropic /
  Gemini today.** `AssistantDefaults::web_search:
  WebSearchSupport` carries either `None`, `NativeTool(tool_id)`,
  or `Always` (Perplexity-shape, deferred). Runtime hook is one
  match statement in `fono-llm` adding `tools: [...]` to the
  request body.

## Findings, ranked by user impact

1. **Cloud key prompts are not deduplicated across capabilities.**
   (`wizard.rs:1093-1095, 1144-1146, 210-214`.) Highest-impact,
   lowest-risk fix; addressed by the catalogue + a single
   `prompt_or_reuse_key` helper.
2. **Assistant TTS is OpenAI-locked.** (Issue #11.) Anyone whose
   primary cloud key isn't OpenAI gets a text-only assistant or
   has to obtain a second key. Adding Groq + Cartesia + Deepgram
   TTS lifts that.
3. **Capability coverage is invisible to the user.** Wizard
   prompts hint at neither cross-capability coverage nor
   vision/search; primary picker labels with badges fix this.
4. **No capability catalogue exists.** Required substrate for the
   above three.
5. **"Mixed" is mislabelled.** Rename to "Customize per capability".
6. **Assistant + TTS are unconditionally a second mini-wizard.**
   Today even OpenAI-cloud users walk through `configure_assistant`'s
   full provider menu. Collapses to a single yes/no when the
   primary already covers chat + has any TTS option available.

## Implementation Plan

### Phase A — Capability catalogue (foundation)

- [ ] Task A1. Add `crates/fono-core/src/provider_catalog.rs`
      exposing a `CloudProvider` struct (`id`, `display_name`,
      `tagline`, `console_url`, `key_env`, plus
      `stt: Option<SttDefaults>`, `llm: Option<LlmDefaults>`,
      `assistant: Option<AssistantDefaults>`,
      `tts: Option<TtsDefaults>`) and a `const`
      `CLOUD_PROVIDERS: &[CloudProvider]`. `AssistantDefaults`
      carries `text_model: &str`, `multimodal_model:
      Option<&str>`, `web_search: WebSearchSupport`, and
      `badges: &[Badge]`. `TtsDefaults` carries `model: &str`,
      `default_voice: &str`, `endpoint: TtsEndpoint`
      (enum: `OpenAiCompat { base_url }`, `Cartesia`,
      `Deepgram`, `OpenRouterStub { base_url }`), and an
      optional `runtime_probe: bool` flag that gates wizard
      display (used by OpenRouter).
- [ ] Task A2. Re-export the catalogue from `fono_core::lib.rs`
      next to `providers`. The existing
      `crates/fono-core/src/providers.rs` env-var tables stay
      authoritative for env-var names; the catalogue references
      them by `pub use` so a wrong env-var pair fails at compile
      time.
- [ ] Task A3. Reroute `cloud_pair` (`providers.rs:230-243`) to
      consume the catalogue: `cloud_pair(id)` returns
      `(SttBackend, LlmBackend)` derived from each entry.
      Regression test: every existing pair (`groq`, `cerebras`,
      `openai`, `anthropic`, `openrouter`, `deepgram`,
      `assemblyai`) resolves identically.
- [ ] Task A4. Unit tests: every catalogue entry's `key_env`
      matches the canonical env var returned by
      `providers::*_key_env`; every `Backend` variant claimed by
      a catalogue entry parses back through `parse_*_backend`;
      every entry's lower-case `id` matches the corresponding
      `*_backend_str`; no orphans.

### Phase B — Wizard cloud-path collapse (issue #9)

- [ ] Task B1. Introduce `pick_primary_cloud_provider` in
      `crates/fono/src/wizard.rs` that renders the catalogue as
      a `Select` with capability-badge labels (e.g.
      `"OpenAI — STT · LLM · Assistant · TTS · vision · search"`,
      `"Groq — STT · LLM · Assistant · TTS · vision"`,
      `"Anthropic — LLM · Assistant · vision · search"`,
      `"Customize per capability (advanced)"`). Default cursor on
      the broadest-coverage provider, or on the provider whose
      key is already in `secrets.toml` to make re-runs cheap.
- [ ] Task B2. Replace `configure_cloud` so the cloud branch
      calls `pick_primary_cloud_provider`, runs **one**
      `prompt_or_reuse_key` against the chosen provider's
      `key_env`, then walks the catalogue entry and fills each
      capability with its default. Capabilities the primary
      can't serve trigger a follow-up "Add <capability>? — yes /
      skip / pick from <secondary list>" prompt that only
      enumerates providers actually offering that capability.
      Live-mode + language pickers run unchanged at the branch
      tail.
- [ ] Task B3. Update `configure_assistant`
      (`wizard.rs:134-287`) to consult the catalogue and the
      primary-provider choice. When the primary already covers
      assistant chat **and** offers (or has been paired with) a
      TTS provider, the assistant prompt collapses to a single
      `Confirm` ("Enable the voice assistant with `<primary>`
      chat + `<tts_choice>` TTS?"). Decline falls through to
      the full picker for power users.
- [ ] Task B4. Rename `PathChoice::Mixed` to
      `PathChoice::Customize`, update the menu label to
      "Customize each capability (advanced)", and apply the
      catalogue-aware key-reuse messaging to that path as well.
- [ ] Task B5. Centralise key-reuse logic into one
      `prompt_or_reuse_key(key_env, catalogue_entry)` helper.
      Prints a one-line `"reusing OPENAI_API_KEY from secrets.toml"`
      summary before delegating to
      `prompt_api_key_with_validation`. Every cloud key prompt in
      `wizard.rs` routes through it.
- [ ] Task B6. Pre-seed wizard defaults from an existing config
      when one is on disk: the primary picker should pre-select
      the user's current provider rather than the catalogue's
      first row. Add a unit test that round-trips a v0.7.1-shaped
      config through the wizard helpers without flipping
      `tts.backend` from `Wyoming` to `OpenAI`.

### Phase C — Documentation & user-facing copy

- [ ] Task C1. Update `docs/providers.md` to lead with the
      revised capability matrix (the table above), and add new
      sub-sections for Groq, Cartesia, and Deepgram TTS
      (endpoints, voice catalogues, OpenAI-compat status).
- [ ] Task C2. Refresh `README.md`'s "Switching providers"
      paragraph: one quoted wizard run for the new collapsed
      cloud branch, mention multi-provider TTS in passing.
- [ ] Task C3. Add a `CHANGELOG.md` `[Unreleased]` entry:
      - **Added** — Groq, Cartesia, and Deepgram TTS backends
        for the voice assistant (issue #11). Multi-modal /
        web-search-enabled assistant extras as opt-in toggles.
      - **Changed** — Wizard cloud path collapsed onto a single
        primary-provider picker; `[hotkeys].mode` removal already
        landed in the prior unreleased section, leave intact.
      - **Deprecated** — `[wizard.mixed]` flow renamed to
        Customize; legacy `Mixed` configs continue to load.

### Phase D — Tests, smoke, ADR (foundation for #9 + #11)

- [ ] Task D1. Integration tests in `crates/fono/tests/`:
      - Cloud branch + OpenAI → STT/LLM/Assistant/TTS all set to
        OpenAI; one key entry written.
      - Cloud branch + Groq → STT/LLM/Assistant/TTS all set to
        Groq (new!); one key entry written. Verifies issue #11's
        single-key-covers-everything scenario.
      - Cloud branch + Anthropic → LLM/Assistant set to
        Anthropic; user picks Cartesia for TTS (key auto-detected
        because Cartesia STT key already present); two key
        entries.
      - Re-run with secrets pre-populated → zero new prompts,
        one-line "reusing" notice each.
- [ ] Task D2. Unit test for the Customize regression guard:
      Groq STT + Anthropic LLM + Wyoming TTS round-trips through
      the helpers without surprise overrides.
- [ ] Task D3. Manual smoke checklist appended to
      `plans/2026-05-04-fono-prelaunch-ux-polish-and-smoke-tests-v1.md`:
      fresh wizard with each of OpenAI / Groq / Cartesia /
      Deepgram as primary; verify the assistant produces audio in
      all cases.
- [ ] Task D4. ADR `docs/decisions/0006-cloud-provider-catalogue.md`
      capturing the catalogue design, the alternatives
      considered, and the deferred-work list (Cartesia STT
      already wired; Azure TTS; Perplexity Sonar; OpenRouter TTS
      pending endpoint; Gemini STT; ElevenLabs TTS).

### Phase E — Multimodal & web-search assistant extras

- [ ] Task E1. Extend `AssistantDefaults` (already introduced in
      A1) with `multimodal_model: Option<&str>`,
      `web_search: WebSearchSupport`, and `badges: &[Badge]`.
      Define `WebSearchSupport::{None, NativeTool(&'static str),
      Always}`. Populate per provider:
      - OpenAI: `multimodal_model = Some("gpt-5.4-mini")`,
        `web_search = NativeTool("web_search_preview")`.
      - Anthropic: `multimodal_model =
        Some("claude-haiku-4-5-20251001")`, `web_search =
        NativeTool("web_search_20250305")`.
      - Gemini: `multimodal_model = Some(<flash>)`, `web_search
        = NativeTool("google_search")`.
      - Groq: `multimodal_model =
        Some("llama-4-maverick-17b-128e-instruct")`,
        `web_search = None`.
      - Cerebras / OpenRouter / Ollama: `multimodal_model = None`,
        `web_search = None`.
- [ ] Task E2. Surface capability badges in the primary picker
      labels from B1 — pure label change driven by
      `assistant.badges` and `tts.is_some()`.
- [ ] Task E3. Add a conditional `MultiSelect` in
      `configure_assistant`: only rendered when at least one
      option applies to the chosen primary. Rows:
      - `[ ] Let the assistant see images on demand`
      - `[ ] Let the assistant search the web for fresh info`
      Toggles set new config fields
      `[assistant].prefer_vision: bool` and
      `[assistant].prefer_web_search: bool` (both default
      `false`, both serde-aliased so old configs continue to
      load).
- [ ] Task E4. In the assistant builder (`fono-llm` /
      `fono::assistant`), consult the catalogue at startup: if
      `prefer_vision && primary.assistant.multimodal_model.is_some()`,
      use the multimodal model variant; else fall back and print
      a one-line wizard notice when the toggle was on but no
      variant existed (e.g., user toggled vision then switched
      primary to Cerebras).
- [ ] Task E5. Wire the **server-side web-search tool** for
      `WebSearchSupport::NativeTool(tool_id)` in `fono-llm`'s
      OpenAI-compat client + Anthropic client + Gemini client:
      one match statement per client adding `tools: [{type:
      tool_id}]` to the request payload when
      `prefer_web_search` is on. Skip the tool when the
      provider's catalogue entry says `None`. Log a single
      one-line `info!` per assistant invocation when the tool
      is active.
- [ ] Task E6. ADR `docs/decisions/0007-assistant-multimodal-and-search.md`
      capturing the catalogue extension, the runtime decisions,
      and the deferred-work list (screen-capture for vision,
      local search-tool plumbing for Groq/Cerebras/OpenRouter,
      intent-detection auto-routing, modifier-hotkey for
      per-query screen-capture). Vision-via-screenshot is
      explicitly out of scope here.
- [ ] Task E7. Update `docs/providers.md` with a new
      "Assistant capabilities" subsection summarising which
      providers offer vision + web search and how `prefer_vision`
      / `prefer_web_search` map to model variants and tool
      payloads.

### Phase F — Multi-provider TTS (issue #11)

- [ ] Task F1. Refactor `crates/fono-tts/src/openai.rs` into
      `openai_compat.rs`. Parameterise on `base_url`,
      `default_model`, `default_voice`, and `auth_header`
      (enum: `Bearer { token }` for OpenAI/Groq, future
      `XApiKey` for hypothetical providers). The existing
      OpenAI client becomes a thin constructor wrapper. PCM
      decode + warm client + prewarm stay unchanged.
- [ ] Task F2. Add `TtsBackend::Groq` to
      `fono_core::config::TtsBackend` (and to
      `crates/fono-core/src/providers.rs` enums/string maps),
      with `tts_key_env(&Groq) = "GROQ_API_KEY"`. Update
      `crates/fono-tts/src/factory.rs` to instantiate the
      OpenAI-compat client against
      `https://api.groq.com/openai/v1/audio/speech` with the
      `playai-tts` model. Defaults voice: `Fritz-PlayAI`
      (English) — picked because it matches OpenAI's `alloy`
      neutral-male timbre and is the documented baseline voice.
- [ ] Task F3. Add `TtsBackend::Cartesia` with a native client
      `crates/fono-tts/src/cartesia.rs`. Endpoint
      `https://api.cartesia.ai/tts/bytes`; auth via
      `X-API-Key`; model `sonic-2`; default voice
      `"a0e99841-438c-4a64-b679-ae501e7d6091"` (Cartesia's
      neutral English voice id). Response is raw PCM at
      configurable sample rate; request 24 kHz to match the
      assistant's audio pipeline. Add to factory + catalogue.
- [ ] Task F4. Add `TtsBackend::Deepgram` with a native client
      `crates/fono-tts/src/deepgram.rs`. Endpoint
      `https://api.deepgram.com/v1/speak?model=aura-2-thalia-en`;
      auth via `Authorization: Token <key>`; encoding
      `linear16`, sample rate 24000. Add to factory +
      catalogue.
- [ ] Task F5. Add `TtsBackend::OpenRouter` as a first-class
      catalogue entry. OpenRouter TTS is OpenAI-compatible
      (`POST https://openrouter.ai/api/v1/audio/speech`, same
      body/PCM shape). Default model: `hexgrad/kokoro-82m`
      (Kokoro, $0.62 / 1M chars), default voice `af_heart`.
      No runtime probe — endpoint is confirmed live. Reuses
      the parameterised `openai_compat.rs` client from F1.
- [ ] Task F6. Tray + `fono use tts <name>` paths consume
      `configured_tts_backends` (new helper in
      `crates/fono-core/src/providers.rs`, mirroring
      `configured_stt_backends`) so the TTS submenu shows
      every provider whose key is already in `secrets.toml`.
      The submenu pre-checks the active backend.
- [ ] Task F7. Wizard TTS picker (consumed by
      `configure_assistant` from B3): builds from the
      catalogue's TTS-bearing providers, ordered by
      key-already-present first. Rendered labels:
      `"Groq TTS (cloud, key already set) — fastest"`,
      `"Cartesia TTS (cloud, key already set) — best quality"`,
      `"Deepgram TTS (cloud, key already set)"`,
      `"OpenAI TTS (cloud, will ask for key)"`,
      `"Wyoming TTS server (LAN piper)"`,
      `"Skip — text-only assistant"`. Issue #11 is solved when
      this list reads "key already set" for at least one cloud
      provider on any non-OpenAI primary.

### Phase G — Release engineering

- [ ] Task G1. Bump version, update `CHANGELOG.md` `[Unreleased]`
      to a tagged section, update `ROADMAP.md` (move issue #9
      and #11 from Planned/In-progress to Shipped with the
      release tag and date), and tag per AGENTS.md release
      checklist. The release notes lead with the wizard collapse
      and the multi-provider TTS as the two headline features.

## Verification Criteria

- A fresh wizard run on the cloud branch with no existing secrets
  asks for **at most one** API key when the user picks any provider
  whose catalogue entry covers STT + LLM + Assistant + TTS (today:
  OpenAI and **Groq** after Phase F lands).
- Re-running the wizard with an existing `secrets.toml` never re-asks
  for any stored key; one-line "reusing …" notice replaces every
  duplicate prompt.
- A user with only `GROQ_API_KEY` (or only `CARTESIA_API_KEY`, or
  only `DEEPGRAM_API_KEY`, or only `ANTHROPIC_API_KEY`) can run the
  full voice-assistant loop (record → STT → LLM → TTS) without ever
  acquiring an OpenAI key. Verified by integration tests in D1.
- Vision and web-search toggles in the assistant overlay change the
  effective request payload (multimodal model variant or `tools: [{
  type: ... }]`) and only render when the chosen primary supports
  them; a Cerebras-only setup never sees the toggle row.
- `cargo test -p fono-core -p fono-tts -p fono-llm -p fono` is
  green; `cargo clippy --workspace --all-targets -- -D warnings` is
  clean; `cargo deny check` exit-zero; no new dependencies.
- `fono doctor` resolves every catalogue entry (no orphan ids, no
  missing env-var mapping). `fono use cloud <name>` continues to
  resolve every legacy pair.
- `docs/providers.md`, `README.md`, and `CHANGELOG.md` updates
  match the shipped wizard wording (verified by the smoke checklist
  in `plans/2026-05-04-…-prelaunch-ux-polish-…-v1.md`).
- OpenRouter TTS surfaces in the wizard if and only if the
  runtime endpoint probe returns 2xx/405; never appears on a 404
  response. The probe never blocks the wizard for more than 2 s.

## Potential Risks and Mitigations

1. **OpenRouter TTS does not actually exist (today).** Surfacing it
   blindly would lead users into a 404 wall.
   Mitigation: the catalogue stub is gated behind a runtime probe
   (Task F5); the wizard only shows OpenRouter TTS when the
   endpoint validates. Probe is cached for the wizard session, so
   no per-press latency at runtime.
2. **Groq TTS PlayAI models are beta-tier and may change names /
   pricing.** Same risk applies to Cartesia's Sonic-2 and
   Deepgram's Aura-2 model identifiers.
   Mitigation: centralise model strings in `fono-tts::defaults` so
   a renamed model is a one-line edit. Doctor's `fono keys check`
   path can probe `/v1/models` (or equivalent) on each provider
   to surface deprecation.
3. **Cartesia and Deepgram TTS request shapes are not
   OpenAI-compatible.** Native clients double the test surface.
   Mitigation: keep each native client small (≤200 LOC each),
   share PCM decode + warm-client helpers via a new
   `crates/fono-tts/src/common.rs`, and add per-provider
   integration smoke tests gated behind the same env vars as the
   STT equivalence harness already uses (skipped on forks /
   `-no-cloud-gate` tags).
4. **Catalogue drift versus runtime factories.** Model strings live
   in `fono-stt::defaults` / `fono-llm::defaults` /
   `fono-tts::defaults`; catalogue re-exports them.
   Mitigation: compile-time `pub use` (Task A4) makes a missing
   constant a build failure; unit test asserts every catalogue
   reference resolves.
5. **Capability badges in the wizard label list overflow narrow
   terminals.** On an 80-column TTY,
   `"OpenAI — STT · LLM · Assistant · TTS · vision · search"` is
   ~57 chars — fits, but the badge list is unbounded as new
   capabilities land.
   Mitigation: the label-builder truncates badges to a hard cap
   of three (most distinguishing first: TTS > vision > search >
   …) and ends with `"…"` when more exist. Full breakdown is
   visible in `fono doctor`.
6. **`[assistant].prefer_vision` / `prefer_web_search` defaults
   are `false` but old configs that exist before this release
   might already set fields with similar names from user
   experimentation.** Unlikely in practice (these fields don't
   exist yet) but worth a serde alias.
   Mitigation: add the two fields with `#[serde(default)]` and
   no aliases — net-new fields can't collide with anything that
   doesn't already exist. Document in CHANGELOG.
7. **Provider terms-of-service drift.** Groq PlayAI models are
   sometimes flagged as "beta" with stricter rate limits; Cartesia
   has a free tier limit; Deepgram Aura-2 has a per-month free
   minute cap.
   Mitigation: `docs/providers.md` calls these limits out per
   provider; `critical_notify` (issue #8 work, already landed)
   surfaces rate-limit responses as Critical-urgency
   notifications.
8. **Issue #11 framing names OpenRouter explicitly, but
   OpenRouter has no TTS endpoint today.** Failing to address
   OpenRouter could read as "you ignored the issue."
   Mitigation: the plan ships a forward-compatible OpenRouter
   stub (Task F5) so the surface is ready the day OpenRouter
   exposes the endpoint; the user explanation in CHANGELOG /
   `docs/providers.md` is honest that the OpenRouter TTS slot
   is endpoint-gated and will light up automatically.

## Alternative Approaches

1. **Skip the catalogue and address each issue independently.**
   Pros: smaller diffs per issue; faster to ship the TTS work.
   Cons: re-introduces the duplication problem the catalogue
   solves; assistant extras (multimodal + search) would need a
   second wizard rework to surface; #9 and #11 end up partially
   contradicting each other on which provider drives what.
2. **Add ElevenLabs as the second TTS provider instead of
   Groq/Cartesia/Deepgram.** Pros: best voice quality. Cons:
   different pricing model, no overlap with existing Fono STT
   keys; doesn't unlock the assistant for users who already
   chose Groq/Cartesia/Deepgram for STT. Reject as the first-wave
   pick; ElevenLabs moves to deferred-work.
3. **Auto-route to vision/search by intent classification.**
   The dream UX. Cost: an extra round-trip per assistant turn,
   plus a real privacy footgun (false-positive screen captures).
   Defer to a future modifier-hotkey scheme (`Shift+F8 =
   assistant-with-screen`) once vision runtime support exists.
4. **Use a single "Cartesia full-stack" path** (STT + TTS, no
   LLM) and skip Cartesia LLM integration. This is actually the
   plan's implicit choice — Cartesia doesn't ship an LLM
   product. The catalogue already encodes "missing capability"
   as `Option::None`, so the wizard handles this without
   special-casing.
5. **Defer Phase E entirely.** Pros: ship #9 + #11 faster.
   Cons: another wizard rewrite when extras finally land. Phase
   E touches the same files Phase B does; bundling them
   amortises the test work. Reject.

## Open questions to resolve at implementation time

- **OpenRouter TTS endpoint shape.** If OpenRouter ships TTS
  before this work lands, confirm the API shape and adjust Task
  F5 to either drop the probe (if shape is fully OpenAI-compat)
  or add a native client.
- **Cartesia / Deepgram free-tier limits.** Confirm at impl time
  that an unauthenticated probe (or a 1-character TTS call)
  doesn't incur a paid charge before adding a doctor reachability
  test.
- **Groq PlayAI English voice catalogue.** Confirm
  `Fritz-PlayAI` is still the documented neutral default; if
  Groq has shipped a wider voice set since plan v2 was written,
  populate `tts.voice_options` in the catalogue and surface a
  tray submenu (out of scope but a natural follow-up).
