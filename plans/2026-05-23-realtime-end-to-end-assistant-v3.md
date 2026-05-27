# Realtime End-to-End Voice Assistant (S2S short-circuit)

## Objective

Add a **realtime / speech-to-speech (S2S) assistant path** for cloud providers
that expose a unified voice-bidi API (OpenAI Realtime, Google Gemini Live),
so that pressing **F8** with a realtime model selected bypasses Fono's
sequential STT → LLM → SentenceSplitter → TTS pipeline and instead opens a
single WebSocket where the model ingests the user's mic PCM and streams
reply audio back directly.

This v3 sharpens the catalogue data shape — `transport` (HTTP vs WebSocket)
becomes the single load-bearing dimension that determines whether F8 runs
the staged or realtime pipeline, with multimodality / reasoning / web-search
as orthogonal metadata fields.

Goals:

- **Latency drop** for F8 assistant turns. Target time-to-first-audio
  ≤ 800 ms median (staged-pipeline baseline today: 1.5–3 s).
- **F7 dictation is untouched.** Every change here applies to F8 only.
- **One source of truth: `[assistant.cloud].model`.** The catalogue's
  per-model `Transport` decides staged vs realtime. No parallel boolean.
- **First-class multi-model-per-provider support**, with a data shape that
  composes additively: a new model is one catalogue entry, a new realtime
  protocol is one enum variant + one client.
- **Honest scope**: wire OpenAI Realtime and Gemini Live in v1; AWS Nova
  Sonic and Azure OpenAI Realtime tracked as natural follow-ons.

## Data Shape (final — see v2 plan history for the iterations this replaces)

```text
// crates/fono-core/src/provider_catalog.rs

struct AssistantDefaults {
    models: &'static [ModelEntry],

    // Named defaults — each MUST point at an id present in `models`.
    // Presence of `default_realtime_model` is also what the wizard reads
    // to decide whether to show the realtime option for this provider.
    default_model: &'static str,
    default_vision_model: Option<&'static str>,
    default_realtime_model: Option<&'static str>,

    web_search: WebSearch,                    // unchanged
    badges: &'static [Badge],                 // provider-level UI badges
}

struct ModelEntry {
    id: &'static str,
    transport: Transport,                     // THE staged-vs-realtime signal
    accepts_images: bool,                     // orthogonal: vision input
    badges: &'static [Badge],                 // per-model UI badges
}

enum Transport {
    Http,                                     // staged: chat-completions HTTP
    WebSocket(RealtimeProfile),               // S2S: bidi voice WebSocket
}

struct RealtimeProfile {
    ws_url: &'static str,
    protocol: RealtimeProtocol,               // picks the client impl
    input_sample_rate: u32,
    output_sample_rate: u32,
}

enum RealtimeProtocol {
    OpenAiRealtime,
    GeminiLive,
    // future: NovaSonic, AzureRealtime, …
}
```

### Why this shape and not a `ModelKind { Text, Multimodal, Realtime }` enum

`ModelKind` collapses two orthogonal axes into one. `Realtime` is a
*transport* property (HTTP vs WebSocket) — it determines which trait
the factory builds. `Multimodal` is an *input-modality* property —
orthogonal to transport. gpt-4o-realtime is **both**; a single-kind enum
forces a false choice. Promoting `transport` to its own field and
`accepts_images` to a sibling boolean composes additively as providers
add models that mix capabilities.

### Why named defaults (`default_*`) instead of `default: bool` per entry

A `default: bool` flag on `ModelEntry` requires the catalogue to enforce
"exactly one default per axis", which gets fragile when axes are
orthogonal. Naming the default model id explicitly turns the invariant
into "each named default exists in `models`" — one assert, one
regression test, no ambiguity.

### Why split `RealtimeEndpoint` into URL data + `RealtimeProtocol` enum

URL is *data* (varies per model, per provider, per GA-vs-preview). Wire
protocol is *code dispatch* (OpenAI events ≠ Gemini events; factory must
build different clients). Conflating them in a single
`RealtimeEndpoint { OpenAIRealtime { ws } | GeminiLive { ws } }` enum
makes "add AWS Nova Sonic with a new URL" require an enum variant that
carries nothing more than a URL — pure noise. Splitting them gives the
catalogue free-form URL data + a small protocol-dispatch enum.

### Explicit scope-cut

**Only `AssistantDefaults` adopts the list shape in this slice.**
`SttDefaults`, `PolishDefaults`, `TtsDefaults` keep their single-model
shape today. The pattern can migrate later if a provider ships multiple
STT models worth surfacing; bundling those migrations into this slice
would balloon scope without adding immediate user value.

## Background — current pipeline anatomy

- **Press**: `crates/fono/src/session.rs:1729-1847` decides streaming-STT
  vs batch-STT capture. Adds a third branch in this plan: realtime
  (raw PCM streamer, no local STT).
- **Release / sequencer**: `crates/fono/src/assistant.rs:123-431`
  (`run_assistant_turn`) wires STT → `Assistant::reply_stream` →
  `SentenceSplitter` → `TextToSpeech::synthesize` → playback. Adds a
  sibling `run_realtime_turn` for the S2S branch.
- **Backend / model selection**: `[assistant].backend` picks the
  *provider*; `[assistant.cloud].model` picks the *model*. Catalogue at
  `crates/fono-core/src/provider_catalog.rs:140-163` declares what each
  provider offers. Factory at `crates/fono-assistant/src/factory.rs:138-154`
  is taught to look up the chosen model in the new `models` list and
  dispatch on `transport`.
- **Playback**: `crates/fono-audio/src/playback.rs:67-122` accepts mono
  f32 PCM at any sample rate; both realtime providers emit int16 PCM
  (OpenAI 24 kHz, Gemini 24 kHz out / 16 kHz in) — trivial conversion.

## UX walk-through (concrete contract)

1. **Fresh user, OpenAI primary, doesn't know what realtime is.** Wizard
   lists models from the catalogue with badges:
   ```
   Choose an assistant model:
     > gpt-5.4-mini                  text · fast        (default)
       gpt-5.4                       text · reasoning
       gpt-5.4-vision                text · vision
       gpt-4o-realtime-preview       realtime · voice
   ```
   Default → text staged pipeline. F7 dictation unchanged.

2. **User picks realtime explicitly.** `[assistant.cloud].model =
   "gpt-4o-realtime-preview"`. Catalogue lookup → `transport =
   WebSocket(...)`. F8 opens single WebSocket; **no local STT for that
   turn**. F7 dictation untouched.

3. **Multi-model power user, Gemini.** Hand-edits config to
   `[assistant] backend = "gemini"` + `[assistant.cloud] model =
   "gemini-2.0-flash-live-001"`. Factory finds id in the catalogue,
   dispatches on `transport`, builds `GeminiLive`. No flag, no surprise.

4. **User on OpenAI without realtime.** Default model is `text`; staged
   pipeline runs. OpenAI Realtime client never constructed.

## Implementation Plan

### Phase 0 — ADR

- [ ] Task 0.1. **Write ADR `docs/decisions/00NN-realtime-assistant.md`**
  capturing (a) the catalogue reshape from named slots
  (`text_model` + `multimodal_model`) to a `models: &[ModelEntry]` list
  with orthogonal `transport` / `accepts_images` fields, (b) why model
  id is the single toggle, (c) why only OpenAI Realtime + Gemini Live
  in v1, (d) echo-cancellation posture (trust OS audio AEC), (e) strict
  F8-only scope, (f) why STT/polish/TTS catalogue shape is unchanged in
  this slice. Reference `feedback_centralize_decisions`.

### Phase 1 — Catalogue reshape

- [ ] Task 1.1. **Introduce `Transport`, `RealtimeProfile`,
  `RealtimeProtocol`, `ModelEntry`** in
  `crates/fono-core/src/provider_catalog.rs`. Add `Badge::Realtime` to
  the existing badge enum at lines 74-92.

- [ ] Task 1.2. **Replace `AssistantDefaults` fields** `text_model` +
  `multimodal_model` with `models: &[ModelEntry]` + named defaults
  (`default_model`, `default_vision_model`, `default_realtime_model`).
  Update OpenAI, Anthropic, Groq, Cerebras, OpenRouter, Ollama entries
  to the new shape, preserving today's default ids. Populate a new
  Gemini assistant entry (catalogue currently has `assistant: None` for
  Gemini at lines 303-317) with a single realtime `ModelEntry` so
  `fono use assistant gemini` becomes legal.

- [ ] Task 1.3. **Helper accessors** in the same module:
  `find_model(id) -> Option<&ModelEntry>`, `default_text(&self) ->
  &ModelEntry`, `default_realtime(&self) -> Option<&ModelEntry>`,
  `default_vision(&self) -> Option<&ModelEntry>`, `has_realtime() ->
  bool`. Keeps call sites readable.

- [ ] Task 1.4. **Catalogue regression tests** in the same module:
  - `every_named_default_exists_in_models`
  - `every_realtime_model_has_websocket_transport`
  - `every_websocket_model_has_realtime_in_id_or_badges` (sanity check)
  - `default_vision_model_accepts_images_is_true`
  - The existing `no_orphan_cloud_variants` test continues to pass.

### Phase 2 — `Gemini` backend variant + key plumbing

- [ ] Task 2.1. **Add `Gemini` to `AssistantBackend`** at
  `crates/fono-core/src/config.rs:654-664`. Update
  `parse_assistant_backend` / `assistant_backend_str` /
  `all_assistant_backends` in `crates/fono-core/src/providers.rs`. Add
  `GEMINI_API_KEY` to `assistant_key_env` for the new variant.

### Phase 3 — `RealtimeAssistant` trait

- [ ] Task 3.1. **Add `RealtimeAssistant` trait + `RealtimeEvent` enum**
  in `crates/fono-assistant/src/traits.rs`. One async method
  `run_turn(audio_in: mpsc::Receiver<Vec<f32>>, sample_rate,
  ctx: &AssistantContext) -> BoxStream<Result<RealtimeEvent>>`. Events:
  `Audio { pcm, sample_rate }`, `AssistantTextDelta(String)`,
  `UserTextFinal(String)`, `Done`. Plus `name()`,
  `native_input_rate()`, default `prewarm()`. Export from
  `crates/fono-assistant/src/lib.rs`. Rationale: input shape is
  fundamentally different from `Assistant::reply_stream` (PCM stream
  vs `&str`), cancellation differs (closing `audio_in` = end-of-
  utterance), and forcing every text backend to default an audio field
  on `TokenDelta` is the wrong tradeoff at N=2 realtime providers.

### Phase 4 — Per-backend realtime clients

- [ ] Task 4.1. **Workspace deps**: add `tokio-tungstenite` (rustls
  features only) and `base64` to `fono-assistant`. Update `deny.toml`
  allow-list.

- [ ] Task 4.2. **`crates/fono-assistant/src/openai_realtime.rs`** —
  `OpenAiRealtime` implementing `RealtimeAssistant`, protocol-tagged
  `RealtimeProtocol::OpenAiRealtime`. Opens
  `wss://api.openai.com/v1/realtime?model=...` with
  `Authorization: Bearer $OPENAI_API_KEY` + `OpenAI-Beta: realtime=v1`.
  `session.update` carries system prompt + server-VAD config. Forwards
  `audio_in` (mono f32 → 24 kHz int16, base64) as
  `input_audio_buffer.append`. Consumes `response.audio.delta`,
  `response.audio_transcript.delta`,
  `conversation.item.input_audio_transcription.completed`,
  `response.done`. Auth/network failures route through
  `fono_core::critical_notify::Stage::Assistant`.

- [ ] Task 4.3. **`crates/fono-assistant/src/gemini_live.rs`** —
  `GeminiLive` implementing `RealtimeAssistant`, protocol-tagged
  `RealtimeProtocol::GeminiLive`. Opens
  `wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key=$GEMINI_API_KEY`.
  `setup` carries model + generation config + system instruction.
  Forwards `audio_in` (resampled to 16 kHz int16) as
  `realtime_input.media_chunks`. Consumes
  `serverContent.modelTurn.parts[].inlineData` (24 kHz PCM out),
  `output_transcription` / `input_transcription`,
  `serverContent.turnComplete`.

- [ ] Task 4.4. **Factory dispatch on `Transport`** in
  `crates/fono-assistant/src/factory.rs`. New entry point
  `build_assistant_or_realtime(backend, cloud_cfg, secrets, catalogue)
  -> Result<AssistantHandle>` where `AssistantHandle = Staged(Arc<dyn
  Assistant>) | Realtime(Arc<dyn RealtimeAssistant>)`. Steps:
  1. Look up `cloud_cfg.model` in the provider's
     `AssistantDefaults.models`.
  2. If not found, log a warning and fall back to
     `defaults.default_model` (unknown ids never silently become
     realtime).
  3. Dispatch on `entry.transport`: `Http` → existing text-backend
     constructors at `factory.rs:156-220`; `WebSocket(profile)` →
     match `profile.protocol` and construct
     `OpenAiRealtime` / `GeminiLive`.

### Phase 5 — Orchestrator short-circuit (F8 only)

- [ ] Task 5.1. **Replace the `assistant_backend` slot** at
  `crates/fono/src/session.rs:336-337` with an `AssistantHandle`-typed
  slot. Reload path at `crates/fono/src/session.rs:609-622` calls
  `build_assistant_or_realtime` and stores the returned variant. F7
  dictation paths do not touch this slot.

- [ ] Task 5.2. **Press-side branch** in `on_assistant_hold_press`
  (`crates/fono/src/session.rs:1729-1847`). Above the existing
  streaming-STT branch at `:1740-1764`, add: if the current handle is
  `AssistantHandle::Realtime(rt)`, open a *raw PCM streamer* capture
  (no local STT). Capture task forwards mono f32 frames into
  `rt.run_turn`'s `audio_in` mpsc. Parallel consumer task drains
  `BoxStream<RealtimeEvent>`. Overlay flips `AssistantRecording →
  AssistantSpeaking` on first `Audio` event.

- [ ] Task 5.3. **Release-side symmetry** in
  `on_assistant_hold_release` (`:1853-2055`). For a realtime session,
  closing the `audio_in` mpsc is the end-of-utterance signal. Reuse
  the drain-wait loop at `crates/fono/src/assistant.rs:394-429`
  unchanged.

- [ ] Task 5.4. **`run_realtime_turn` helper** in
  `crates/fono/src/assistant.rs`, parallel to `run_assistant_turn`.
  Event routing: `Audio` → playback enqueue + first-frame overlay
  flip; `UserTextFinal` → `history.push_user(text)`;
  `AssistantTextDelta` → accumulate into `full_reply`; `Done` →
  `history.push_assistant(full_reply)` + drain poll. Critical-notify
  routing mirrors `Stage::Assistant` calls.

- [ ] Task 5.5. **Barge-in / cancel**. Existing
  `AssistantSessionState::stop_current_turn`
  (`crates/fono/src/assistant.rs:54-63`) drains playback and notifies
  the pump. Realtime consumer task translates that notify into "drop
  the `BoxStream`"; each backend's `Drop` impl sends `Close` and
  aborts the read task.

### Phase 6 — User-facing surfaces

- [ ] Task 6.1. **Wizard model picker** at
  `crates/fono/src/wizard.rs`. Replace the implicit "pick
  provider.text_model" with an explicit picker over
  `defaults.models`, labelled with badges and a transport hint:
  ```
  Choose an assistant model:
    > gpt-5.4-mini                  text · fast       (default)
      gpt-5.4                       text · reasoning
      gpt-5.4-vision                text · vision
      gpt-4o-realtime-preview       realtime · voice
  ```
  Conservative default (`defaults.default_model`); power users opt
  into realtime by selecting it here.

- [ ] Task 6.2. **`prefer_vision` becomes a one-line shim** in the
  factory: when `prefer_vision = true` and the configured model
  doesn't accept images, prefer `defaults.default_vision_model` if
  present in the catalogue. Long-term retirable; for this slice keep
  as compat shim so existing configs behave unchanged.
  `prefer_web_search` is orthogonal and untouched.

- [ ] Task 6.3. **CLI**. `fono use assistant <backend>` unchanged
  (picks provider). Add `fono use assistant-model <id>` writing
  `[assistant.cloud].model`, validating the id against the active
  provider's catalogue. Rejects unknown ids with a list of valid
  options. Symmetric with `fono use stt` / `fono use polish`.

- [ ] Task 6.4. **`fono doctor`** row replaces the current assistant
  line with `Assistant : <provider> / <model_id> (<transport-mode>)`,
  e.g. `Assistant : openai / gpt-4o-realtime-preview (realtime —
  single WebSocket S2S)` or `Assistant : openai / gpt-5.4-mini
  (text — staged STT → LLM → TTS)`. Active mode unambiguous at a
  glance.

- [ ] Task 6.5. **CHANGELOG `[Unreleased]`**:
  - `### Added` — "Realtime voice assistant mode (OpenAI Realtime,
    Google Gemini Live): when `[assistant.cloud].model` is a
    realtime model, F8 opens a single bidi WebSocket and bypasses
    the staged STT → LLM → TTS pipeline. F7 dictation unaffected."
  - `### Changed` — "Cloud catalogue assistant section now carries
    a `models: &[ModelEntry]` list with per-model `transport`
    (`Http` vs `WebSocket(...)`) and `accepts_images` instead of
    `text_model` + `multimodal_model` named slots. Backwards-compat
    shim keeps `prefer_vision` working."
  ROADMAP update at tag time per AGENTS.md.

### Phase 7 — Tests + manual verification

- [ ] Task 7.1. **Unit tests** for `run_realtime_turn`: deterministic
  fake `RealtimeAssistant` yielding scripted event sequences; assert
  `ConversationHistory` end-state, `AudioPlayback` enqueue counts,
  FSM transitions. Plus an explicit test: selecting a `Text` model on
  a realtime-capable provider keeps the staged pipeline running.

- [ ] Task 7.2. **Factory tests**: unknown-model-id warns and falls
  back to `default_model` (does *not* silently default to realtime);
  known realtime id constructs `Realtime` variant; known text id
  constructs `Staged` variant.

- [ ] Task 7.3. **Catalogue tests** from Task 1.4 stay green across
  every catalogue edit in subsequent phases.

- [ ] Task 7.4. **F7 dictation regression**: extend
  `tests/live_pipeline.rs` (or add a sibling) covering a dictation
  turn under both `Transport::Http` and `Transport::WebSocket`
  assistant configurations, asserting byte-identical injection
  output. Locks in "F8 changes never touch F7."

- [ ] Task 7.5. **Manual smoke**:
  - `OPENAI_API_KEY` set, `model = "gpt-4o-realtime-preview"`, F8-hold,
    speak, verify TTFA ≤ 800 ms median over 10 turns.
  - Same provider, `model = "gpt-5.4-mini"`, verify staged pipeline
    runs (STT call visible in `RUST_LOG=fono_stt=debug`).
  - `GEMINI_API_KEY` set, `backend = "gemini"`, default Live model —
    same realtime UX.
  - Barge-in (F8 mid-reply) and Escape (shut up) in realtime mode.

## Verification Criteria

- With `[assistant.cloud].model = "gpt-4o-realtime-preview"`, F8 opens
  a single `wss://api.openai.com/v1/realtime` connection and **no
  local STT model is loaded for that turn**.
- With `model = "gpt-5.4-mini"` (or any `Transport::Http` model), the
  existing staged pipeline runs unchanged.
- F7 dictation behaviour is byte-identical in both cases (Task 7.4).
- `fono doctor` line accurately names the active provider, model id,
  and transport mode.
- Unknown model ids log a warning and fall back to
  `defaults.default_model` (never silently to realtime).
- All Phase 1.4 / Phase 7 catalogue and factory tests pass.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets --
  -D warnings`, `cargo test --workspace --tests --lib` clean.
- `cargo deny check` clean for `tokio-tungstenite` + `base64`.
- Manual TTFA ≤ 800 ms median in realtime mode.
- Barge-in + Escape work in realtime mode.

## Potential Risks and Mitigations

1. **Catalogue reshape touches every provider entry.** Mechanical but
   wide.
   Mitigation: land Phase 1 + Phase 2 in one merge behind the Phase 1.4
   regression tests. Subsequent phases cannot regress the catalogue
   invariants without lighting up CI.

2. **Realtime APIs are preview-tier and may drift.** OpenAI uses
   `OpenAI-Beta: realtime=v1`; Gemini Live uses `v1beta`.
   Mitigation: each client is self-contained; JSON event handling
   stays in one `match` block per client; provider-specific events
   do not leak into `RealtimeEvent`.

3. **Echo / acoustic feedback in full-duplex mode.** Staged pipeline
   never overlapped capture and playback.
   Mitigation: trust the OS AEC (PipeWire `echo-cancel`, PulseAudio
   `module-echo-cancel`); document in `docs/providers.md`. Do not
   ship a custom AEC in v1.

4. **Cost / token accounting.** Realtime bills per audio-minute, not
   tokens.
   Mitigation: track `audio_in_ms` / `audio_out_ms` as a
   `PipelineMetrics` sibling; surface in `fono doctor` + history.
   Out-of-scope for v1 but flagged in the ADR.

5. **Cancellation race**: dropping the realtime stream must close the
   WebSocket promptly or the model continues billing into a closed
   pipe.
   Mitigation: each backend wraps `WebSocketStream` in a `Drop` impl
   that sends `Close` + aborts the read task; covered by a unit test
   that drops the stream mid-turn.

6. **WebSocket through restrictive proxies.** Some networks only allow
   HTTPS.
   Mitigation: `tokio-tungstenite` over `rustls` uses the same TLS
   handshake as `reqwest`; document in `docs/troubleshooting.md`;
   connect failure surfaces one critical-notify with guidance to pick
   a different model.

7. **Wizard model list bloats as providers add models.**
   Mitigation: surface `default_model` + `default_vision_model` +
   `default_realtime_model` first; show full list with `--full` /
   second-screen drilldown when count exceeds ~5. Defer the
   threshold until a provider catalogue actually crosses it.

## Alternative Approaches Considered

1. **Keep `text_model` + `multimodal_model` named slots, add a
   `realtime_model` slot.** Smallest diff.
   Rejected — defers the multi-model-per-provider problem one step;
   the user explicitly called it out as a future need.

2. **`prefer_realtime: bool` flag on `[assistant]` (v1 proposal).**
   Symmetric with `prefer_vision` / `prefer_web_search`.
   Rejected — adds a second source of truth that can disagree with
   the model id. One automatic decision in one place.

3. **`kind: ModelKind { Text | Multimodal | Realtime }` enum (v2
   proposal).** Single field on `ModelEntry`.
   Rejected — collapses two orthogonal axes (transport, input
   modality). gpt-4o-realtime is *both* realtime and multimodal; a
   single-kind enum forces a false choice.

4. **`fn` pointer in the catalogue entry instead of
   `RealtimeProtocol` enum** (`builder: fn(...) -> Arc<dyn
   RealtimeAssistant>`). Removes the enum entirely.
   Rejected — awkward in const data, harder to inspect in
   `fono doctor`, harder to test. The two-line dispatch match in the
   factory is fine.

5. **Migrate `SttDefaults` / `PolishDefaults` / `TtsDefaults` to the
   same list shape in this slice.** Symmetric, future-proof.
   Rejected — ballooning scope. The list shape can migrate later
   capability-by-capability without breaking the `AssistantDefaults`
   contract.

6. **Auto-flip to realtime whenever a realtime-capable provider is
   selected.** Less configuration.
   Rejected — realtime models are preview-tier, more expensive,
   acoustic-echo-sensitive. Defaulting to them would surprise users
   in bad ways.

7. **Polyfill `RealtimeAssistant` over the staged pipeline for
   providers without S2S APIs.** Unified trait surface.
   Rejected — the latency win comes from the model owning VAD and
   first-audio emission. Polyfill = staged pipeline with extra
   indirection.
