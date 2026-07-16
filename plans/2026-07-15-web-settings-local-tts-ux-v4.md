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
selected provider is OpenAI-shaped and its key resolves (verbatim forward, key injected
as Bearer, per `crates/fono-net/src/llm_server/proxy.rs:39-77`); **adapter path**
otherwise (drive the local engine or non-OpenAI-shaped provider through the existing
`TextToSpeech` / STT traits and encode the response ourselves).

The **Wyoming server is unaffected and retained** — it already serves ASR + TTS + wake
over TCP (`crates/fono-net/src/wyoming/server.rs:433,458,461`) for Home Assistant;
the OpenAI HTTP surface is the complementary browser/tooling-facing standard.

## Gateway routing model (per-request provider selection + defaults)

This is the contract that answers "speak this via Gemini, the next via Kokoro, the next
via ElevenLabs, and give me a default when I don't specify":

- **`model` is a namespaced route selector**: `provider[/upstream-model]`.
  - Local engines (always available, no key needed): `piper`, `kokoro`, `supertonic`,
    `local` (= honour `tts.local.engine`, i.e. the configured auto/pinned behaviour).
  - Cloud providers: `openai[/tts-1]`, `groq[/…]`, `openrouter[/…]`, `gemini[/…]`,
    `elevenlabs[/eleven_turbo_v2]`, `cartesia[/…]`, `deepgram[/…]`, `speechmatics[/…]`.
    The part after `/` is the upstream model id, passed through (proxy) or used by the
    adapter client; omitted → that provider's configured/default model.
- **A provider is callable iff its key resolves** via the existing chain
  (`Secrets::resolve`, `crates/fono-core/src/secrets.rs:54-59`: `secrets.toml` `[keys]`
  then env), regardless of which backend is the *active* `[tts].backend`. Configuring
  three cloud keys makes all three routable in consecutive requests with no config
  changes. A route whose key doesn't resolve → OpenAI-shaped 401/400 naming the missing
  key ref. Local engines never require a key.
- **Defaults when unspecified**: blank/omitted `model` → the daemon's configured
  `[tts].backend` (and its engine/voice settings); blank/omitted `voice` → the
  configured voice for the selected provider (`tts.voice` for cloud, `tts.local.voice`
  / language-based auto-resolution for local), else the provider's own default voice.
  So `{"input":"hello"}` alone speaks exactly as the daemon would.
- **Same model for STT**: `model` = `whisper`/`local` or `provider[/model]`; blank →
  configured `[stt].backend`; key-resolution rule identical.
- **Discoverability**: the gateway advertises its routable speech/transcription models
  (local engines always; cloud providers whose keys resolve) so clients and the settings
  UI can enumerate options — via `GET /v1/models` entries on the LLM server and the
  `tts_local`/`tts_cloud` meta on the settings server.

## Assumptions (documented, decided autonomously)

- **Proxy-when-OpenAI-shaped, adapter otherwise — mirroring ADR 0036.**
  - TTS proxy-capable: OpenAI, Groq, OpenRouter (shared OpenAI-compat client,
    `crates/fono-tts/src/factory.rs:257-283`). ElevenLabs, Cartesia, Deepgram,
    Speechmatics, Gemini are not OpenAI-shaped for TTS → adapter path via their existing
    `TextToSpeech` impls (client still receives a standard OpenAI response).
  - STT proxy-capable: OpenAI, Groq, OpenRouter (outbound shapes at
    `crates/fono-stt/src/groq.rs:15`, `openai.rs:17`, `openrouter.rs:56`); the rest via
    adapter (`crates/fono-stt/src/factory.rs:100-119`); local Whisper via adapter.
- **Per-request client construction is acceptable**: cloud TTS/STT clients are cheap
  (HTTP + key); local engines are cached/reused (see memory risk below). The gateway
  builds the route's backend from `(provider, model, voice)` per request using the
  existing factories' resolve helpers (`crates/fono-tts/src/factory.rs:188-250`,
  `crates/fono-stt/src/factory.rs:44-72`).
- **Response formats (adapter path): `wav` (default) and `pcm` only** — mp3/opus/aac/flac
  would need encoder crates (binary-size rule) → clean OpenAI-shaped 400. Proxy path
  passes `response_format` through, so proxied clouds may return mp3 etc. natively.
- **Transcriptions accepts OpenAI-shaped multipart/form-data** (WAV file) in the adapter
  path; proxy path forwards the multipart body verbatim. Check the dependency graph for
  an in-graph multipart parser before hand-rolling a minimal one (size-budget rule).
- **Mounting:** shared handlers in `fono-net`, mounted on the **LLM server** (canonical
  OpenAI surface) **and** the settings server (same-origin for the settings UI and the
  future assistant page; works with the LLM server disabled).
- **Supertonic wiring is in scope** (engine exists at `crates/fono-tts/src/supertonic/`
  but is unreachable from factory/router); minimal explicit-selection wiring, full `auto`
  routing participation deferrable.
- **New config field `tts.local.engine`** (`auto` default) — serde-defaulted, existing
  configs unaffected; `auto` preserves ADR 0033 routing byte-for-byte.
- **Cold-start model downloads** (Supertonic pack ~140 MiB) surface as a busy/downloading
  state in the UI; third-party clients simply block until ready.
- **Slice order:** the STT endpoint (Section E) is independent of the settings-UI goal and
  can ship as a follow-up slice; everything else lands together.

## Implementation Plan

### A. Config schema: explicit engine selection

- [ ] Task 1. Add `engine` field to `TtsLocal` (`crates/fono-core/src/config.rs:619-631`)
      as a new `TtsLocalEngine` enum (`auto` | `piper` | `kokoro` | `supertonic`,
      lowercase serde, default `auto`). Rationale: single authoritative knob for UI and
      file users; `auto` keeps every existing config working.
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
- [ ] Task 8. Implement the **route resolver** implementing the "Gateway routing model"
      above: parse `model` into `(provider, upstream_model)`; blank → configured
      `[tts]` defaults; verify key resolution for cloud routes (OpenAI-shaped
      401/400 with the missing key ref name on failure); resolve `voice` through the
      request → configured → provider-default chain. Unit-test the resolver in
      isolation (it is pure logic). Rationale: this is the piece that makes
      Gemini-then-Kokoro-then-ElevenLabs consecutive calls work.
- [ ] Task 9. Implement the shared speech handler in `fono-net` on the LLM server's
      two-path structure (`crates/fono-net/src/llm_server/openai.rs:104-143`): parse
      `{model, input, voice, response_format, speed}`, run the route resolver, then
      **proxy fast-lane** for OpenAI-shaped TTS routes (verbatim forward + Bearer,
      mirroring `forward_chat`, `proxy.rs:39-77`, streaming upstream bytes back) or
      **adapter path** (build the engine/provider via the TTS factory, call
      `synthesize`, `crates/fono-tts/src/traits.rs:41-46`, encode `wav`/`pcm`, 400 for
      other formats). Input cap ~4096 chars, single-flight lock per local engine,
      request timeout. Rationale: one standards-shaped implementation for preview,
      assistant page, and external clients across local *and* cloud.
- [ ] Task 10. Wire reload-safe snapshot closures (pattern of `server_upstream_snapshot`,
      `crates/fono/src/session.rs:1351-1352`, recomputed at `session.rs:880-882,
      1191-1199`) carrying the configured defaults + secrets handle into the gateway;
      cache constructed **local** engine instances across requests (drop on config
      reload); construct cloud clients per request. Rationale: hot-reload safety without
      listener restarts; avoids repeated ONNX model loads for consecutive local calls.
- [ ] Task 11. Mount the handler on **both** servers: LLM server route table
      (`crates/fono-net/src/llm_server/mod.rs`) with its auth semantics — and advertise
      routable speech models in `GET /v1/models` — plus the settings-server dispatcher
      (`crates/fono-net/src/web_settings/mod.rs:254-326`) token-gated like `/api/*` via
      a `WebSettingsHooks` closure (`mod.rs:97-106`). Rationale: OpenAI clients find it
      (and can discover models) where they expect; the settings UI gets same-origin
      access with the LLM server off.
- [ ] Task 12. Cold-start handling: uncached voice/pack downloads surface as a pollable
      JSON status for the settings UI (or minimally a kept-open connection with UI
      "downloading voice model…" state); third-party clients block until ready.

### E. `POST /v1/audio/transcriptions` — universal STT endpoint (proxy + adapter)

*(Independent slice; may ship as an immediate follow-up if the change grows too large.)*

- [ ] Task 13. Implement the shared transcription handler: OpenAI-shaped
      `multipart/form-data` (`file`, `model`, `language?`, `response_format?`
      json/text); reuse the route-resolver pattern from Task 8 for STT (`model` =
      `whisper`/`local` or `provider[/model]`, blank → configured `[stt].backend`,
      key-resolution rule identical); **proxy fast-lane** for OpenAI-shaped STT routes
      (verbatim multipart forward + Bearer); **adapter path** otherwise (decode WAV →
      f32 PCM, drive the STT factory, `crates/fono-stt/src/factory.rs:100-119`, return
      `{"text": …}`). Upload size cap. Multipart parser: in-graph crate if present,
      else minimal hand-rolled. Rationale: completes the LLM+TTS+STT gateway; STT
      foundation for the assistant page's mic input.
- [ ] Task 14. Mount on both servers and wire the STT snapshot closure (same patterns as
      Tasks 10–11, keys via `crates/fono-stt/src/factory.rs:44-72`).
- [ ] Task 15. Tests: route resolver (STT variant), multipart parsing, WAV decode, proxy
      header injection, adapter response shape; a `curl`-level integration check in the
      test script. Rationale: net-new public ingest surface.

### F. Settings UI: engine cards, voice dropdown, test box

- [ ] Task 16. In the Voice section
      (`crates/fono-net/src/web_settings/assets/app.js:458-489`), when the Local segment
      is active, render an **engine card row** (Auto / Piper / Kokoro / Supertonic,
      styled like the provider grid at `app.js:475`) bound to `tts.local.engine`, each
      with a one-line hint. Rationale: visual parity with the cloud backend.
- [ ] Task 17. Replace the free-text voice input (`app.js:471-472`) with a **dropdown**
      fed by `/api/meta`'s `tts_local`, filtered by selected engine (Auto shows the full
      catalog grouped by engine/language), first entry "Auto — match my language" →
      empty `tts.local.voice`. Keep the output-device input (`app.js:485`) as-is.
- [ ] Task 18. Add the **test box**: sample-text input, Play button, status line. On
      click, `fetch()` POST same-origin `/v1/audio/speech` with the *currently selected
      (possibly unsaved)* route — `{model: engineOrProvider, voice, input,
      response_format: "wav"}` — decode via `AudioContext.decodeAudioData`, play through
      an `AudioBufferSourceNode`; busy/downloading state; inline errors. Works for the
      cloud segment too (model = selected provider, callable per `tts_cloud` meta) — one
      test box for both backends. Rationale: instant audition without saving; Web Audio
      is the exact foundation for the assistant page (`getUserMedia` + AudioWorklet
      later).
- [ ] Task 19. `app.css` updates for cards, dropdown, test box within the existing
      accordion visual language.

### G. Docs, housekeeping, gates

- [ ] Task 20. Document the gateway routing model (`model` namespacing, key-based
      availability, default chain), `tts.local.engine`, `/v1/audio/speech`, and
      `/v1/audio/transcriptions` (proxy vs adapter per provider, formats, mounts, auth)
      in config reference / providers / LLM-server docs; extend or add an ADR (successor
      to ADR 0036) covering the gateway + Web Audio groundwork; update `docs/status.md`
      at session end.
- [ ] Task 21. Run the pre-commit gate (`cargo fmt --all -- --check`, `cargo clippy
      --workspace --all-targets -- -D warnings`, `cargo test --workspace --tests --lib`)
      and the size-budget gate (`./tests/check.sh --size-budget`) — run the size gate
      **early** (after Task 3) as well as at the end.

## Verification Criteria

- **Multi-provider routing:** with keys configured for three cloud providers, three
  consecutive `POST /v1/audio/speech` calls with `model` = `gemini`, `kokoro`,
  `elevenlabs` each return audio from the named engine/provider, no config changes
  between calls.
- **Defaults:** `{"input":"hello"}` with no `model`/`voice` speaks via the configured
  `[tts]` backend and voice; blank `voice` on an explicit route uses that provider's
  configured/default voice.
- **Key gating:** a cloud route without a resolvable key returns an OpenAI-shaped error
  naming the missing key ref; local routes always work.
- Engine picker: selecting Piper/Kokoro/Supertonic, saving, and dictating produces speech
  from that engine; `auto` behaves identically to the pre-change router.
- Voice dropdown lists only voices valid for the selected engine; "Auto" voice resolves
  per configured language as before.
- Test box plays audio in the browser from a remote daemon, for both local engines and
  configured cloud providers; cold voices show a downloading state.
- `GET /v1/models` on the LLM server lists routable speech (and transcription) models.
- `curl -F file=@sample.wav -F model=whisper …/v1/audio/transcriptions` returns
  `{"text": …}` via local Whisper; with a cloud STT key, an explicit cloud model proxies.
- An off-the-shelf OpenAI client pointed at Fono can chat, synthesize, and transcribe
  with zero Fono-specific code. Wyoming behaviour unchanged.
- Unsupported adapter formats (e.g. `mp3` on a local engine) return an OpenAI-shaped 400.
- Existing configs without `tts.local.engine` load, migrate, and round-trip unchanged.
- Config-coverage test, fmt, clippy, workspace tests, and size-budget gate all pass.

## Potential Risks and Mitigations

1. **Scope growth** — routing + gateway (especially STT) is much bigger than the original
   settings-UI ask.
   Mitigation: Section E is severable; within D, the route resolver (Task 8) is pure
   logic testable in isolation; Sections A–D+F alone deliver the user-visible goal.
2. **Key-spending exposure** — any authenticated client can now spend credits on *any*
   provider whose key is configured, not just the active backend.
   Mitigation: both mounts are authenticated; document this property prominently; input
   caps and timeouts bound per-request cost; (optional follow-up: an allowlist config
   for routable providers).
3. **Multipart parsing without a new crate.**
   Mitigation: check `cargo tree` first; OpenAI uploads are flat multipart; minimal
   parser ~100 lines; any new crate needs sign-off per project rules.
4. **Proxy fidelity gaps** (provider quirks).
   Mitigation: verbatim body forwarding (ADR 0036 lesson); only inject auth and default
   a blank model.
5. **Supertonic wiring larger than it looks.**
   Mitigation: minimum-viable explicit-selection wiring; `auto` participation deferred.
6. **Binary size growth** from Supertonic reachability + two endpoints + resolver.
   Mitigation: size gate early and late; no new codec crates by design.
7. **Local engine cache memory** (Kokoro + Supertonic + live orchestrator engine could
   be resident simultaneously after mixed-route calls).
   Mitigation: cap the gateway's engine cache (e.g. keep-last-one, drop on reload);
   document transient cost.
8. **Autoplay policies** blocking `AudioContext`.
   Mitigation: context created/resumed inside the Play click handler (user gesture).

## Alternative Approaches

1. **Bespoke `/api/tts/preview`** (plan v1) — dead-end one-off; superseded. Rejected.
2. **Wyoming as the browser-facing surface** — browsers can't speak raw TCP. Retained
   for Home Assistant; rejected for browser/tooling use.
3. **Single-backend gateway (no per-request routing)** — simpler, but fails the
   "Gemini, then Kokoro, then ElevenLabs" requirement and would force config flips
   between calls. Rejected.
4. **Separate base paths per provider** (e.g. `/gemini/v1/audio/speech`) instead of
   namespaced `model` — LiteLLM-style model namespacing is the ecosystem convention and
   keeps one endpoint; path-per-provider breaks off-the-shelf clients. Rejected.
5. **Adapter-only (no proxy) audio endpoints** — loses upstream fidelity (formats,
   features) and contradicts ADR 0036 and the explicit proxy requirement. Rejected.
6. **Streaming audio responses now** — deferred; the OpenAI shape is unchanged when
   streaming lands with the assistant page.
