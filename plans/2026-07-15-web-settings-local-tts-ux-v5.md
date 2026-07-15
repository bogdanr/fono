# Web Settings TTS UX + OpenAI-Compatible Audio Gateway (speech & transcriptions)

## Objective

Two coupled outcomes:

**A. Settings UI (the original ask):** first-class local-TTS configuration — an engine
selector (Auto / Piper / Kokoro / Supertonic), voice dropdowns from the predefined
catalogs, and a sample-text test box that plays synthesized audio **in the browser**
(daemon may be remote).

**B. Universal audio API (the standard):** extend Fono's existing OpenAI-compatible
server surface from chat-only to a full **AI gateway** covering LLM + TTS + STT:

- `POST /v1/chat/completions` — already shipped (proxy fast-lane + local adapter, ADR 0036).
- `POST /v1/audio/speech` — **new**: local engines *and* cloud TTS providers,
  **routable per request**.
- `POST /v1/audio/transcriptions` — **new**: local Whisper *and* cloud STT providers.

Each audio endpoint follows the LLM server's established two-path pattern
(`crates/fono-net/src/llm_server/openai.rs:104-143`): **proxy pass-through** when the
selected provider's endpoint is OpenAI-shaped and its key resolves (verbatim forward,
key injected as Bearer, per `crates/fono-net/src/llm_server/proxy.rs:39-77`); **adapter
path** otherwise (drive the local engine or non-OpenAI-shaped provider through the
existing `TextToSpeech` / STT traits and encode the response ourselves).

The **Wyoming server is unaffected and retained** — it already serves ASR + TTS + wake
over TCP (`crates/fono-net/src/wyoming/server.rs:433,458,461`) for Home Assistant;
the OpenAI HTTP surface is the complementary browser/tooling-facing standard.

## Gateway routing model (per-request provider selection + defaults)

The contract that answers "speak via Gemini, then Kokoro, then ElevenLabs, with a
default when unspecified" — modelled directly on OpenRouter's own scheme (OpenAI-shaped
schema, `author/model` namespacing, omitted model → user's default):

- **`model` is a namespaced route selector, split on the FIRST slash only**:
  `provider[/upstream-model]`, where the upstream-model part may itself contain slashes
  (OpenRouter models are `author/model`, e.g.
  `openrouter/openai/gpt-4o-mini-tts` → provider `openrouter`, upstream model
  `openai/gpt-4o-mini-tts` forwarded untouched).
  - Local engines (always available, no key needed): `piper`, `kokoro`, `supertonic`,
    `local` (= honour `tts.local.engine`, i.e. configured auto/pinned behaviour).
  - Cloud providers: `openai[/tts-1]`, `groq[/…]`, `openrouter[/author/model]`,
    `gemini[/…]`, `elevenlabs[/eleven_turbo_v2]`, `cartesia[/…]`, `deepgram[/…]`,
    `speechmatics[/…]`. Omitted suffix → that provider's configured/default model.
- **Double-proxy discipline (the OpenRouter case):** each hop strips its own prefix,
  injects its own auth, and forwards the rest verbatim. Client sends
  `openrouter/openai/gpt-4o-mini-tts` to Fono; Fono sends
  `model: "openai/gpt-4o-mini-tts"` to OpenRouter with Fono's OpenRouter key **plus the
  attribution headers** (`fono_core::openrouter_attribution::headers()`, already applied
  on every outbound OpenRouter call, e.g. `crates/fono-stt/src/openrouter.rs:144-146`);
  OpenRouter routes onward with its own provider keys. Verbatim forwarding at both hops
  is what makes the composition safe (ADR 0036 lesson).
- **A provider is callable iff its key resolves** via the existing chain
  (`Secrets::resolve`, `crates/fono-core/src/secrets.rs:54-59`: `secrets.toml` `[keys]`
  then env), regardless of the active `[tts].backend`. Three configured keys → three
  routable providers in consecutive requests, no config changes. Unresolvable key →
  OpenAI-shaped 401/400 naming the missing key ref. Local engines never need a key.
- **Defaults when unspecified**: blank/omitted `model` → the configured
  `[tts].backend` (and its engine/voice settings); blank/omitted `voice` → the
  configured voice for the selected provider (`tts.voice` cloud, `tts.local.voice` /
  language-based auto for local), else the provider's own default voice. So
  `{"input":"hello"}` alone speaks exactly as the daemon would.
- **Same model for STT**: `model` = `whisper`/`local` or `provider[/model]`; blank →
  configured `[stt].backend`; key rule identical.
- **STT proxy-shape caveat (verified):** OpenRouter's transcription endpoint does
  **not** accept `multipart/form-data` — it requires a JSON body with base64 audio
  (documented in our own client, `crates/fono-stt/src/openrouter.rs:2-9,56`). So the
  STT proxy fast-lane (verbatim multipart forward) applies to **OpenAI and Groq only**;
  the `openrouter` STT route goes through the **adapter path**, reusing the existing
  `OpenRouterStt` client which already does the JSON/base64 translation and attribution
  headers. TTS proxying to OpenRouter is unaffected (that endpoint is OpenAI-shaped).
- **Discoverability**: routable speech/transcription models (local engines always;
  cloud providers whose keys resolve) advertised via `GET /v1/models` on the LLM server
  and `tts_local`/`tts_cloud` meta on the settings server.

## Assumptions (documented, decided autonomously)

- **Proxy-when-OpenAI-shaped, adapter otherwise — mirroring ADR 0036.**
  - TTS proxy-capable: OpenAI, Groq, OpenRouter (shared OpenAI-compat client,
    `crates/fono-tts/src/factory.rs:257-283`). ElevenLabs, Cartesia, Deepgram,
    Speechmatics, Gemini → adapter path via existing `TextToSpeech` impls (client still
    receives a standard OpenAI response).
  - STT proxy-capable: OpenAI, Groq only (see caveat above); OpenRouter and the rest via
    adapter (`crates/fono-stt/src/factory.rs:100-119`); local Whisper via adapter.
- **Per-request client construction is acceptable**: cloud clients are cheap; local
  engines are cached/reused (see memory risk). The gateway builds the route's backend
  from `(provider, model, voice)` using the factories' resolve helpers
  (`crates/fono-tts/src/factory.rs:188-250`, `crates/fono-stt/src/factory.rs:44-72`).
- **Response formats (adapter path): `wav` (default) and `pcm` only** — mp3/opus/aac/
  flac would need encoder crates (binary-size rule) → OpenAI-shaped 400. Proxy path
  passes `response_format` through; proxied clouds may return mp3 etc. natively.
- **Transcriptions accepts OpenAI-shaped multipart/form-data** (WAV file) in the
  adapter path; proxy path forwards multipart verbatim (OpenAI/Groq). Check the
  dependency graph for an in-graph multipart parser before hand-rolling a minimal one.
- **Mounting:** shared handlers in `fono-net`, mounted on the **LLM server** (canonical
  OpenAI surface) **and** the settings server (same-origin for the settings UI and the
  future assistant page; works with the LLM server disabled).
- **Supertonic wiring is in scope** (engine exists at `crates/fono-tts/src/supertonic/`
  but is unreachable from factory/router); minimal explicit-selection wiring, full
  `auto` routing participation deferrable.
- **New config field `tts.local.engine`** (`auto` default) — serde-defaulted, existing
  configs unaffected; `auto` preserves ADR 0033 routing byte-for-byte.
- **Cold-start model downloads** (Supertonic pack ~140 MiB) surface as a
  busy/downloading state in the UI; third-party clients block until ready.
- **Slice order:** the STT endpoint (Section E) is independent of the settings-UI goal
  and can ship as a follow-up slice; everything else lands together.

## Implementation Plan

### A. Config schema: explicit engine selection

- [ ] Task 1. Add `engine` field to `TtsLocal` (`crates/fono-core/src/config.rs:619-631`)
      as a new `TtsLocalEngine` enum (`auto` | `piper` | `kokoro` | `supertonic`,
      lowercase serde, default `auto`). Rationale: single authoritative knob; `auto`
      keeps every existing config working.
- [ ] Task 2. Verify the web-settings config-leaf coverage test
      (`crates/fono-net/src/web_settings/mod.rs:438-540`) passes once the UI binds
      `tts.local.engine`. Rationale: this test gates UI/schema sync.

### B. Engine wiring: make Supertonic selectable and honour the pin

- [ ] Task 3. Extend `build_local` / voice resolution
      (`crates/fono-tts/src/factory.rs:110-139`) to consult `tts.local.engine`:
      `piper`/`kokoro` constrain catalog lookup; `supertonic` constructs
      `SupertonicLocal` (via `ensure_pack`, `crates/fono-tts/src/supertonic/mod.rs:76-88`);
      `auto` unchanged. Rationale: the picker must change runtime behaviour.
- [ ] Task 4. Teach `LocalRouter` (`crates/fono-tts/src/local_router.rs:36-57, 289-325`)
      the engine pin: `engine != auto` restricts per-utterance voice selection to that
      engine; a pinned voice keeps existing "pin disables routing" semantics
      (`local_router.rs:89-92`). Rationale: Auto stays smart; explicit stays predictable.
- [ ] Task 5. Unit tests in `fono-tts`: engine pin → correct dispatch; supertonic
      selection resolves a default speaker; `auto` unchanged (ADR 0033 regression guard).

### C. Catalog & provider exposure to the browser

- [ ] Task 6. Extend `GET /api/meta` (hook in `crates/fono/src/daemon.rs:3771-3863`,
      route at `crates/fono-net/src/web_settings/mod.rs:290`) with: (a) `tts_local` —
      per-engine voice lists from the embedded catalog
      (`crates/fono-tts/src/voices.rs:163-196`) and Supertonic speakers
      (`crates/fono-tts/src/supertonic/style.rs`); (b) `tts_cloud` — which cloud
      providers are currently routable (key resolves; never expose the key itself).
      Rationale: dropdowns and the test box need to know what's callable.

### D. `POST /v1/audio/speech` — universal TTS endpoint (routing + proxy + adapter)

- [ ] Task 7. Promote the f32→WAV encoder (`crates/fono-stt/src/groq.rs:586`) into a
      shared location usable by `fono-net` (e.g. `fono-audio` or `fono-core` util),
      with `fono-stt` delegating. Rationale: avoids a `fono-net → fono-stt` edge; zero
      new crates.
- [ ] Task 8. Implement the **route resolver** per the "Gateway routing model" above:
      split `model` on the first slash into `(provider, upstream_model)` — upstream
      part opaque and forwarded untouched (OpenRouter's `author/model` ids must
      survive); blank → configured `[tts]` defaults; key-resolution check for cloud
      routes (OpenAI-shaped 401/400 naming the missing key ref); `voice` resolution via
      request → configured → provider-default chain. Unit-test the resolver in
      isolation, including the `openrouter/author/model` nested-namespace case.
      Rationale: this is the piece that makes multi-provider consecutive calls work,
      and first-slash splitting is the subtle correctness point.
- [ ] Task 9. Implement the shared speech handler in `fono-net` on the LLM server's
      two-path structure (`crates/fono-net/src/llm_server/openai.rs:104-143`): parse
      `{model, input, voice, response_format, speed}`, run the route resolver, then
      **proxy fast-lane** for OpenAI-shaped TTS routes (verbatim forward + Bearer,
      mirroring `forward_chat`, `proxy.rs:39-77`, streaming upstream bytes back; for
      the `openrouter` route also inject the attribution headers,
      `fono_core::openrouter_attribution::headers()`) or **adapter path** (build the
      engine/provider via the TTS factory, call `synthesize`,
      `crates/fono-tts/src/traits.rs:41-46`, encode `wav`/`pcm`, 400 for other
      formats). Input cap ~4096 chars, single-flight lock per local engine, request
      timeout. Rationale: one standards-shaped implementation for preview, assistant
      page, and external clients across local *and* cloud.
- [ ] Task 10. Wire reload-safe snapshot closures (pattern of `server_upstream_snapshot`,
      `crates/fono/src/session.rs:1351-1352`, recomputed at `session.rs:880-882,
      1191-1199`) carrying configured defaults + secrets handle into the gateway; cache
      constructed **local** engine instances across requests (drop on config reload);
      construct cloud clients per request. Rationale: hot-reload safety; avoids repeated
      ONNX loads for consecutive local calls.
- [ ] Task 11. Mount the handler on **both** servers: LLM server route table
      (`crates/fono-net/src/llm_server/mod.rs`) with its auth semantics — and advertise
      routable speech models in `GET /v1/models` — plus the settings-server dispatcher
      (`crates/fono-net/src/web_settings/mod.rs:254-326`) token-gated like `/api/*` via
      a `WebSettingsHooks` closure (`mod.rs:97-106`). Rationale: OpenAI clients find it
      (and discover models) where they expect; the settings UI gets same-origin access
      with the LLM server off.
- [ ] Task 12. Cold-start handling: uncached voice/pack downloads surface as a pollable
      JSON status for the settings UI (or minimally a kept-open connection with UI
      "downloading voice model…" state); third-party clients block until ready.

### E. `POST /v1/audio/transcriptions` — universal STT endpoint (proxy + adapter)

*(Independent slice; may ship as an immediate follow-up if the change grows too large.)*

- [ ] Task 13. Implement the shared transcription handler: OpenAI-shaped
      `multipart/form-data` (`file`, `model`, `language?`, `response_format?`
      json/text); reuse the route-resolver pattern (Task 8) for STT; **proxy fast-lane
      for OpenAI and Groq only** (verbatim multipart forward + Bearer); **adapter path**
      for everything else — including `openrouter`, whose transcription endpoint
      requires JSON/base64 rather than multipart (`crates/fono-stt/src/openrouter.rs:2-9`)
      and is served by the existing `OpenRouterStt` client — decode WAV → f32 PCM,
      drive the STT factory (`crates/fono-stt/src/factory.rs:100-119`), return
      `{"text": …}`. Upload size cap. Multipart parser: in-graph crate if present, else
      minimal hand-rolled. Rationale: completes the gateway; STT foundation for the
      assistant page's mic input; the OpenRouter shape difference is exactly why the
      adapter path exists.
- [ ] Task 14. Mount on both servers and wire the STT snapshot closure (same patterns
      as Tasks 10–11, keys via `crates/fono-stt/src/factory.rs:44-72`).
- [ ] Task 15. Tests: route resolver (STT variant, incl. openrouter nested ids),
      multipart parsing, WAV decode, proxy header injection (Bearer + OpenRouter
      attribution), adapter response shape; a `curl`-level integration check in the
      test script. Rationale: net-new public ingest surface.

### F. Settings UI: engine cards, voice dropdown, test box

- [ ] Task 16. In the Voice section
      (`crates/fono-net/src/web_settings/assets/app.js:458-489`), when the Local
      segment is active, render an **engine card row** (Auto / Piper / Kokoro /
      Supertonic, styled like the provider grid at `app.js:475`) bound to
      `tts.local.engine`, each with a one-line hint. Rationale: visual parity with the
      cloud backend.
- [ ] Task 17. Replace the free-text voice input (`app.js:471-472`) with a **dropdown**
      fed by `/api/meta`'s `tts_local`, filtered by selected engine (Auto shows the
      full catalog grouped by engine/language), first entry "Auto — match my language"
      → empty `tts.local.voice`. Keep the output-device input (`app.js:485`) as-is.
- [ ] Task 18. Add the **test box**: sample-text input, Play button, status line. On
      click, `fetch()` POST same-origin `/v1/audio/speech` with the *currently selected
      (possibly unsaved)* route — `{model: engineOrProvider, voice, input,
      response_format: "wav"}` — decode via `AudioContext.decodeAudioData`, play
      through an `AudioBufferSourceNode`; busy/downloading state; inline errors. Works
      for the cloud segment too (model = selected provider, callable per `tts_cloud`
      meta). Rationale: instant audition without saving; Web Audio is the foundation
      for the assistant page (`getUserMedia` + AudioWorklet later).
- [ ] Task 19. `app.css` updates for cards, dropdown, test box within the existing
      accordion visual language.

### G. Docs, housekeeping, gates

- [ ] Task 20. Document the gateway routing model (`model` namespacing with first-slash
      split, the double-proxy OpenRouter example, key-based availability, default
      chain, the OpenRouter-STT adapter caveat), `tts.local.engine`,
      `/v1/audio/speech`, and `/v1/audio/transcriptions` (proxy vs adapter per
      provider, formats, mounts, auth) in config reference / providers / LLM-server
      docs; extend or add an ADR (successor to ADR 0036) covering the gateway + Web
      Audio groundwork; update `docs/status.md` at session end.
- [ ] Task 21. Run the pre-commit gate (`cargo fmt --all -- --check`, `cargo clippy
      --workspace --all-targets -- -D warnings`, `cargo test --workspace --tests
      --lib`) and the size-budget gate (`./tests/check.sh --size-budget`) — size gate
      **early** (after Task 3) as well as at the end.

## Verification Criteria

- **Multi-provider routing:** with keys for three cloud providers, three consecutive
  `POST /v1/audio/speech` calls with `model` = `gemini`, `kokoro`, `elevenlabs` each
  return audio from the named engine/provider, no config changes between calls.
- **Nested namespace:** `model: "openrouter/openai/gpt-4o-mini-tts"` reaches OpenRouter
  with `model: "openai/gpt-4o-mini-tts"`, Fono's OpenRouter key, and attribution
  headers; the returned audio streams back to the client unmodified.
- **Defaults:** `{"input":"hello"}` with no `model`/`voice` speaks via the configured
  `[tts]` backend and voice; blank `voice` on an explicit route uses that provider's
  configured/default voice.
- **Key gating:** a cloud route without a resolvable key returns an OpenAI-shaped
  error naming the missing key ref; local routes always work.
- Engine picker: selecting Piper/Kokoro/Supertonic, saving, and dictating produces
  speech from that engine; `auto` behaves identically to the pre-change router.
- Voice dropdown lists only voices valid for the selected engine; "Auto" voice resolves
  per configured language as before.
- Test box plays audio in the browser from a remote daemon, for local engines and
  configured cloud providers; cold voices show a downloading state.
- `GET /v1/models` lists routable speech (and transcription) models.
- `curl -F file=@sample.wav -F model=whisper …/v1/audio/transcriptions` returns
  `{"text": …}` via local Whisper; `model=groq` proxies multipart verbatim;
  `model=openrouter` succeeds via the adapter (JSON/base64 translation) despite
  OpenRouter's non-multipart endpoint.
- An off-the-shelf OpenAI client pointed at Fono can chat, synthesize, and transcribe
  with zero Fono-specific code. Wyoming behaviour unchanged.
- Unsupported adapter formats (e.g. `mp3` on a local engine) return an OpenAI-shaped 400.
- Existing configs without `tts.local.engine` load, migrate, round-trip unchanged.
- Config-coverage test, fmt, clippy, workspace tests, size-budget gate all pass.

## Potential Risks and Mitigations

1. **Scope growth** — routing + gateway (especially STT) is much bigger than the
   original settings-UI ask.
   Mitigation: Section E is severable; the route resolver (Task 8) is pure logic
   testable in isolation; Sections A–D+F alone deliver the user-visible goal.
2. **Key-spending exposure** — any authenticated client can spend credits on *any*
   provider whose key is configured, not just the active backend.
   Mitigation: both mounts authenticated; document prominently; input caps and
   timeouts; optional follow-up: a routable-provider allowlist config.
3. **Per-provider proxy-shape drift** (the OpenRouter STT multipart surprise is the
   proven example; other providers may change shapes too).
   Mitigation: proxy fast-lane only for endpoints verified OpenAI-shaped; everything
   else through adapters that already encapsulate provider quirks; resolver-level
   provider table makes reclassification a one-line change.
4. **Multipart parsing without a new crate.**
   Mitigation: check `cargo tree` first; OpenAI uploads are flat multipart; minimal
   parser ~100 lines; any new crate needs sign-off per project rules.
5. **Supertonic wiring larger than it looks.**
   Mitigation: minimum-viable explicit-selection wiring; `auto` participation deferred.
6. **Binary size growth** from Supertonic reachability + two endpoints + resolver.
   Mitigation: size gate early and late; no new codec crates by design.
7. **Local engine cache memory** (Kokoro + Supertonic + live orchestrator engine
   resident after mixed-route calls).
   Mitigation: cap the gateway's engine cache (keep-last-one, drop on reload).
8. **Autoplay policies** blocking `AudioContext`.
   Mitigation: context created/resumed inside the Play click handler (user gesture).

## Alternative Approaches

1. **Bespoke `/api/tts/preview`** (plan v1) — dead-end one-off; superseded. Rejected.
2. **Wyoming as the browser-facing surface** — browsers can't speak raw TCP. Retained
   for Home Assistant; rejected for browser/tooling use.
3. **Single-backend gateway (no per-request routing)** — fails the "Gemini, then
   Kokoro, then ElevenLabs" requirement. Rejected.
4. **Separate base paths per provider** (e.g. `/gemini/v1/audio/speech`) — breaks
   off-the-shelf clients; OpenRouter-style model namespacing is the ecosystem
   convention. Rejected.
5. **Adapter-only (no proxy) audio endpoints** — loses upstream fidelity and
   contradicts ADR 0036. Rejected.
6. **Body-translating proxy for OpenRouter STT** (multipart → JSON/base64 in the proxy
   lane) — duplicates logic the `OpenRouterStt` adapter already owns; adapter path is
   the same behaviour with less code. Rejected.
7. **Streaming audio responses now** — deferred; the OpenAI shape is unchanged when
   streaming lands with the assistant page.
