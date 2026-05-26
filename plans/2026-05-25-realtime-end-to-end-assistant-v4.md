# Realtime End-to-End Voice Assistant (S2S short-circuit)

## Objective

Add a **realtime / speech-to-speech (S2S) assistant path** for cloud providers
that expose a unified voice-bidi API (OpenAI Realtime, Google Gemini Live),
so that pressing **F8** with a realtime model selected bypasses Fono's
sequential STT → LLM → SentenceSplitter → TTS pipeline and instead opens a
single WebSocket where the model ingests the user's mic PCM and streams
reply audio back directly — **with full tool-calling support so the
voice-actions feature works identically across staged and realtime paths**.

v4 incorporates two structural learnings from earlier rounds:

- The catalogue's `Transport` field (HTTP vs WebSocket) is the single load-
  bearing dimension that decides staged vs realtime. Other capabilities
  (`accepts_images`, `supports_tools`) are orthogonal flags.
- The realtime trait must be a **session handle**, not a one-shot stream,
  because tool results have to flow *back into* the same WebSocket session
  that issued the call. This is non-negotiable for parity with the
  staged-pipeline tool-calling feature shipped by `voice-actions-via-mcp-v1`.

## Dependency

**This plan is blocked on `plans/2026-05-22-voice-actions-via-mcp-v1.md`
landing first.** Specifically it depends on the existence of:

- The `fono-action` crate with `Tool`, `ToolSpec`, `ToolCall`, `ToolResult`,
  `ToolRegistry`, `Dispatcher`.
- The `TokenDelta` extension carrying tool-call structured fields.
- The `[assistant.tools]` config block + wizard prompt + tray submenu.

Without these the realtime path would either ship without tools (making
realtime models — as the user correctly pointed out — "we would probably
not use them very often") or duplicate the entire actions stack on the
realtime side. Strict serial ordering avoids both failure modes.

The catalogue reshape in Phase 1 below is independent and may land before
actions-via-MCP without harm; it just sits unused until Phases 3+ land.

## Goals

- **Latency drop** for F8 assistant turns. Target time-to-first-audio
  ≤ 800 ms median for North American users, ≤ 1.2 s for European /
  Asian users (RTT-dominated past a certain floor — see TTFA budget in
  v3 discussion). Staged-pipeline baseline today: 1.5-3 s.
- **F7 dictation is untouched.** Every change here applies to F8 only.
- **Tool calling works identically on both paths.** A `[assistant.tools]`
  config that fires Home Assistant via MCP from the staged path fires
  the same way from the realtime path, with the same `Dispatcher` and
  the same confirmation policy.
- **One source of truth: `[assistant.cloud].model`.** Catalogue's
  per-model `Transport` decides staged vs realtime. No parallel flag.
- **Mini-tier default for realtime models.** Audio tokens cost ~10×
  text tokens on the full preview tier; ~2× on the mini tier. Wizard
  defaults to mini; full tier reachable for power users who explicitly
  pick it.
- **Cost is visible at the click site.** Wizard labels each realtime
  entry with its rough cost multiplier so users opt in honestly.
- **First-class multi-model-per-provider support** via `ModelEntry` list
  in the catalogue.
- **Honest scope**: OpenAI Realtime (`gpt-realtime-mini` default,
  `gpt-realtime` opt-in) and Gemini Live (`gemini-2.0-flash-live-001`
  default) in v1. AWS Nova Sonic and Azure OpenAI Realtime tracked as
  natural follow-ons.

## Data shape

```text
// crates/fono-core/src/provider_catalog.rs

struct AssistantDefaults {
    models: &'static [ModelEntry],

    // Named defaults — each MUST point at an id present in `models`.
    default_model: &'static str,
    default_vision_model: Option<&'static str>,
    default_realtime_model: Option<&'static str>,   // points at mini tier

    web_search: WebSearch,
    badges: &'static [Badge],
}

struct ModelEntry {
    id: &'static str,
    transport: Transport,
    accepts_images: bool,
    supports_tools: bool,
    cost_tier: CostTier,             // Standard | Premium — drives wizard label
    badges: &'static [Badge],
}

enum Transport {
    Http,
    WebSocket(RealtimeProfile),
}

struct RealtimeProfile {
    ws_url: &'static str,
    protocol: RealtimeProtocol,
    input_sample_rate: u32,
    output_sample_rate: u32,
}

enum RealtimeProtocol {
    OpenAiRealtime,
    GeminiLive,
}

enum CostTier {
    Standard,    // mini realtime, all text models — no special warning
    Premium,     // full realtime tier — wizard adds "use sparingly" label
}
```

### Catalogue invariants (regression-tested)

1. Every named default in `AssistantDefaults` exists in `models`.
2. Every `ModelEntry` with `transport = WebSocket(...)` has
   `supports_tools = true`. (We ship no tool-less realtime models.)
3. Every `default_vision_model` entry has `accepts_images = true`.
4. Every `default_realtime_model` entry has `cost_tier = Standard` —
   i.e. defaults are always the mini tier, never the premium tier.
5. The existing `no_orphan_cloud_variants` test continues to pass.

## Trait shape — session handle pattern

```text
// crates/fono-assistant/src/traits.rs

#[async_trait]
trait RealtimeAssistant: Send + Sync {
    async fn open_session(
        &self,
        ctx: &AssistantContext,
        tools: &[ToolSpec],                // from ToolRegistry; [] if disabled
    ) -> Result<RealtimeSession>;

    fn name(&self) -> &'static str;
    fn native_input_rate(&self) -> u32;
    async fn prewarm(&self) -> Result<()> { Ok(()) }
}

struct RealtimeSession {
    pub audio_in: mpsc::Sender<Vec<f32>>,                  // mono f32 PCM
    pub events: BoxStream<'static, Result<RealtimeEvent>>,
    pub tool_results: mpsc::Sender<ToolResultMessage>,     // submit back
    // Drop closes the WS; sends Close frame + aborts reader task.
}

enum RealtimeEvent {
    Audio { pcm: Vec<f32>, sample_rate: u32 },
    AssistantTextDelta(String),
    UserTextFinal(String),
    ToolCallRequested {
        call_id: String,
        name: String,
        arguments: serde_json::Value,
    },
    Done,
}

struct ToolResultMessage {
    call_id: String,
    result: Result<serde_json::Value, ToolError>,
}
```

**Why session handle, not stream-only:**

- Tool results have to feed *back into* the same WebSocket. A
  `run_turn(audio_in) -> BoxStream<Event>` shape can't represent that
  round-trip — there's no place to write the result. v3's shape would
  have boxed us into either two parallel sessions per turn (bad: model
  loses context) or out-of-band tool plumbing (bad: leaky abstraction).
- Closing the session = dropping the `RealtimeSession`. Single owner,
  single drop site, single place to test cancellation race.
- `tools: &[ToolSpec]` parameter is empty when `[assistant.tools]` is
  disabled or no MCP servers are configured — that branch is the
  "realtime without actions" mode. Same code path, different input.

## UX walk-through

1. **Fresh user, OpenAI primary.** Wizard lists models with cost-visible
   labels:
   ```
   Choose an assistant model:
     > gpt-5.4-mini                  text · fast              (default)
       gpt-5.4                       text · reasoning
       gpt-5.4-vision                text · vision
       gpt-realtime-mini             realtime · voice · ~2× cost
       gpt-realtime                  realtime · voice · ~10× cost · use sparingly
   ```
   Default is text staged. F7 dictation untouched.

2. **User picks realtime.** `model = "gpt-realtime-mini"`. Catalogue
   lookup → `transport = WebSocket(...)`, `supports_tools = true`,
   `cost_tier = Standard`. If `[assistant.tools]` has entries, they're
   passed to `open_session(tools)`. Voice action ("turn on the kitchen
   lights") works identically to the staged path.

3. **Power user picks `gpt-realtime` full tier.** Same flow, just a
   different model id. No warning at runtime — the wizard already showed
   the cost label; opting in is opting in.

4. **User selected OpenAI without realtime.** Default model is text;
   staged pipeline runs. Realtime client never constructed.

## Implementation Plan

### Phase 0 — Dependency check + ADR

- [ ] Task 0.1. **Confirm `voice-actions-via-mcp-v1` is fully landed**
  before starting Phase 3. Specifically `fono-action` crate must export
  `ToolSpec` / `ToolCall` / `ToolResult` / `ToolError` / `Dispatcher` /
  `ToolRegistry` and the `[assistant.tools]` config block must be live.
  Phase 1 (catalogue reshape) may land in parallel; the trait/factory/
  orchestrator phases wait.

- [ ] Task 0.2. **Write ADR `docs/decisions/00NN-realtime-assistant.md`**
  capturing: (a) catalogue reshape from named slots to `models: &[ModelEntry]`
  with orthogonal `transport` / `accepts_images` / `supports_tools` /
  `cost_tier` fields, (b) why session-handle trait shape, (c) why only
  mini-tier defaults, (d) why blocked on actions-via-MCP, (e) F8-only
  scope, (f) STT/Polish/TTS catalogue shapes unchanged in this slice.

### Phase 1 — Catalogue reshape (independent; can land first)

- [ ] Task 1.1. **Introduce `Transport`, `RealtimeProfile`,
  `RealtimeProtocol`, `ModelEntry`, `CostTier`** in
  `crates/fono-core/src/provider_catalog.rs`. Add `Badge::Realtime` to
  the badge enum.

- [ ] Task 1.2. **Replace `AssistantDefaults.text_model` +
  `multimodal_model`** with `models: &[ModelEntry]` + named defaults
  (`default_model`, `default_vision_model`, `default_realtime_model`).
  Update OpenAI, Anthropic, Groq, Cerebras, OpenRouter, Ollama entries.
  Populate new Gemini assistant entry with a single realtime
  `ModelEntry` (`gemini-2.0-flash-live-001`) so `fono use assistant
  gemini` becomes legal.

- [ ] Task 1.3. **Helper accessors**: `find_model(id)`, `default_text`,
  `default_realtime`, `default_vision`, `has_realtime`. Keeps call
  sites readable.

- [ ] Task 1.4. **Catalogue regression tests** asserting the five
  invariants above. Lock the shape before Phases 3+ land.

### Phase 2 — `Gemini` backend variant + key plumbing

- [ ] Task 2.1. **Add `Gemini` to `AssistantBackend`** at
  `crates/fono-core/src/config.rs`. Update
  `parse_assistant_backend` / `assistant_backend_str` /
  `all_assistant_backends` / `assistant_key_env` (`GEMINI_API_KEY`)
  in `crates/fono-core/src/providers.rs`.

### Phase 3 — `RealtimeAssistant` trait + session handle

- [ ] Task 3.1. **Add `RealtimeAssistant` trait, `RealtimeSession`
  struct, `RealtimeEvent` enum, `ToolResultMessage` struct** in
  `crates/fono-assistant/src/traits.rs`. Tool-call event variants
  carry `serde_json::Value` arguments — matches the shape used by
  `fono-action::Tool::call`. Re-export `ToolSpec` from `fono-action`
  through `fono-assistant` so the realtime clients don't need a
  direct dep on `fono-action`.

### Phase 4 — Per-backend realtime clients

- [ ] Task 4.1. **Workspace deps**: add `tokio-tungstenite` (rustls
  features only) and `base64` to `fono-assistant`. Update `deny.toml`
  allow-list.

- [ ] Task 4.2. **`crates/fono-assistant/src/openai_realtime.rs`** —
  `OpenAiRealtime` impl. Opens
  `wss://api.openai.com/v1/realtime?model=...` with `Authorization`
  + `OpenAI-Beta: realtime=v1`. `session.update` carries system
  prompt + `tools: [...]` array converted from `&[ToolSpec]` to
  OpenAI tool schema + server-VAD config. Consumes
  `response.audio.delta` → `RealtimeEvent::Audio`,
  `response.audio_transcript.delta` →
  `RealtimeEvent::AssistantTextDelta`,
  `conversation.item.input_audio_transcription.completed` →
  `RealtimeEvent::UserTextFinal`,
  `response.function_call_arguments.done` →
  `RealtimeEvent::ToolCallRequested`,
  `response.done` → `RealtimeEvent::Done`. `ToolResultMessage` from
  the `tool_results` channel is forwarded as
  `conversation.item.create { type: "function_call_output", … }`
  followed by `response.create` to resume generation.

- [ ] Task 4.3. **`crates/fono-assistant/src/gemini_live.rs`** —
  `GeminiLive` impl. Opens
  `wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key=...`.
  `setup` carries model + generation config + system instruction +
  `tools.functionDeclarations` converted from `&[ToolSpec]`. Consumes
  `serverContent.modelTurn.parts[].inlineData` →
  `RealtimeEvent::Audio`, `output_transcription` /
  `input_transcription` → text events, `toolCall.functionCalls` →
  `RealtimeEvent::ToolCallRequested`, `serverContent.turnComplete`
  → `RealtimeEvent::Done`. `ToolResultMessage` is forwarded as
  `toolResponse.functionResponses`.

- [ ] Task 4.4. **Factory dispatch on `Transport`** in
  `crates/fono-assistant/src/factory.rs`. New entry point
  `build_assistant_or_realtime(backend, cloud_cfg, secrets, catalogue,
  registry: &ToolRegistry) -> Result<AssistantHandle>` where
  `AssistantHandle = Staged(Arc<dyn Assistant>) | Realtime(Arc<dyn
  RealtimeAssistant>)`. Steps: look up `cloud_cfg.model` in
  `defaults.models`; if missing, log warning and fall back to
  `default_model` (never silently to realtime); dispatch on
  `entry.transport`; for `WebSocket`, match `profile.protocol`.

### Phase 5 — Orchestrator short-circuit (F8 only)

- [ ] Task 5.1. **Replace `assistant_backend` slot** at
  `crates/fono/src/session.rs:336-337` with `AssistantHandle`-typed
  slot. Reload path calls `build_assistant_or_realtime` and stores
  whichever variant comes back. F7 paths do not touch this slot.

- [ ] Task 5.2. **Press-side branch** in `on_assistant_hold_press`.
  If `AssistantHandle::Realtime(rt)`: snapshot the current
  `ToolRegistry`'s active specs, call `rt.open_session(ctx,
  &tool_specs).await`, retain the `RealtimeSession`. Open a raw PCM
  streamer capture forwarding mono f32 frames to
  `session.audio_in`. No local STT involved.

- [ ] Task 5.3. **`run_realtime_turn` helper** in
  `crates/fono/src/assistant.rs`. Parallel to `run_assistant_turn`.
  Event routing:
  - `Audio` → `playback.enqueue(pcm, sample_rate)` + first-frame
    overlay flip to `AssistantSpeaking`.
  - `UserTextFinal` → `history.push_user(text)`.
  - `AssistantTextDelta` → accumulate into `full_reply`.
  - `ToolCallRequested { call_id, name, arguments }` →
    `dispatcher.execute(ToolCall { name, arguments }).await` → send
    `ToolResultMessage { call_id, result }` through
    `session.tool_results`. **Same `Dispatcher`** used by the staged
    path — single point of policy (confirmation, audit, errors).
  - `Done` → `history.push_assistant(full_reply)` + drain-poll loop
    at `crates/fono/src/assistant.rs:394-429` (reused unchanged).

- [ ] Task 5.4. **Release-side**: closing `session.audio_in` is the
  end-of-utterance signal. Existing drain-wait loop reused.

- [ ] Task 5.5. **Barge-in / cancel**. Existing
  `AssistantSessionState::stop_current_turn` already drains playback
  and notifies the pump. Realtime consumer task translates that
  notify into "drop the `RealtimeSession`"; backend `Drop` impls
  send `Close` frame and abort the read task.

### Phase 6 — User-facing surfaces

- [ ] Task 6.1. **Wizard model picker** at
  `crates/fono/src/wizard.rs`. Picker over `defaults.models` with
  cost-visible labels:
  - `cost_tier = Standard` realtime → `realtime · voice · ~2× cost`.
  - `cost_tier = Premium` realtime → `realtime · voice · ~10× cost ·
    use sparingly`.
  - Text / multimodal entries: no cost label (baseline).
  Conservative default (`default_model`); realtime opt-in is by
  explicit selection.

- [ ] Task 6.2. **`prefer_vision` one-line compat shim** in factory:
  when `true` and configured model isn't multimodal, prefer
  `default_vision_model`. Long-term retirable.

- [ ] Task 6.3. **CLI**: add `fono use assistant-model <id>` writing
  `[assistant.cloud].model`, validating against the active provider's
  catalogue. Rejects unknown ids with list of valid options.

- [ ] Task 6.4. **`fono doctor`** row: `Assistant : <provider> /
  <model_id> (<mode>) [tools: N]` where mode is "text staged",
  "multimodal staged", "realtime mini" or "realtime premium", and N
  is the count of active tools from the registry (so users can see
  at a glance whether voice actions are wired through).

- [ ] Task 6.5. **`docs/providers.md` cost section**. One short table
  with per-provider per-tier audio-minute costs and a note that
  realtime tokens are billed differently from text. Sets correct
  expectations before users get a bill.

- [ ] Task 6.6. **CHANGELOG + ROADMAP**. Standard release entries.

### Phase 7 — Tests + manual verification

- [ ] Task 7.1. **Unit tests** for `run_realtime_turn`: deterministic
  fake `RealtimeAssistant` yielding scripted event sequences
  including `ToolCallRequested`. Assert dispatcher is called with
  the expected args, `ToolResultMessage` is submitted, the model's
  subsequent `AssistantTextDelta` lands in history. Plus a test that
  selecting a `Text` model on a realtime-capable provider keeps the
  staged pipeline running.

- [ ] Task 7.2. **Factory tests**: unknown-model-id falls back to
  `default_model` with warning; known realtime id constructs
  `Realtime` variant; tools are passed through to the realtime
  client's `open_session`.

- [ ] Task 7.3. **Catalogue tests** from Phase 1.4 stay green.

- [ ] Task 7.4. **F7 dictation regression** in
  `tests/live_pipeline.rs`: dictation turn under both
  `Transport::Http` and `Transport::WebSocket` assistant
  configurations produces byte-identical injection output. Locks in
  "F8 changes never touch F7."

- [ ] Task 7.5. **TTFA measurement harness**: extend `tests/bench.sh`
  or add `fono debug realtime-ttfa` to run N turns, log per-stage
  timings, produce p50/p95 summary. Honest numbers for the release
  notes.

- [ ] Task 7.6. **Manual smoke**:
  - `OPENAI_API_KEY` set, default `gpt-realtime-mini`, F8 hold,
    speak "what time is it" → expect realtime reply; TTFA p50 in
    target range.
  - Same provider, switch to `gpt-5.4-mini` → expect staged pipeline
    (STT call visible in `RUST_LOG=fono_stt=debug`).
  - With `[assistant.tools]` configured for Home Assistant: F8,
    "turn on the kitchen lights" → lights turn on under both
    `gpt-realtime-mini` and `gpt-5.4-mini` configurations. Same
    outcome, same dispatcher, two different transports.
  - `GEMINI_API_KEY` set, `backend = "gemini"` → realtime default.
  - Barge-in (F8 mid-reply) and Escape (shut up) in realtime mode.

## Verification Criteria

- `voice-actions-via-mcp-v1` is fully landed before Phase 3+ work
  begins. Phase 1 is independent.
- With `model = "gpt-realtime-mini"`, F8 opens a single
  `wss://api.openai.com/v1/realtime` connection. **No local STT model
  is loaded.** Tool calls (when configured) dispatch through the same
  `fono-action::Dispatcher` used by the staged path.
- With `model = "gpt-5.4-mini"` (or any `Transport::Http` model), the
  existing staged pipeline runs unchanged.
- F7 dictation behaviour is byte-identical in both cases (Task 7.4).
- `fono doctor` accurately names provider, model id, mode, and tool
  count.
- Unknown model ids fall back to `default_model` (never silently to
  realtime).
- Catalogue invariants enforce: no tool-less realtime models ship;
  no premium-tier default realtime models ship.
- Catalogue / factory / orchestrator / dictation-regression tests
  green; `cargo fmt`, `cargo clippy --workspace --all-targets --
  -D warnings`, `cargo test --workspace --tests --lib` clean.
- `cargo deny check` clean for `tokio-tungstenite` + `base64`.
- Manual TTFA: ≤ 800 ms median NA, ≤ 1.2 s EU/AS (Task 7.5 harness).
- Tool call flow validated end-to-end under both transports
  (Task 7.6 manual).
- Barge-in + Escape work in realtime mode.

## Potential Risks and Mitigations

1. **`voice-actions-via-mcp-v1` slips or changes its tool-spec API.**
   Mitigation: re-export `ToolSpec` etc. through `fono-assistant` so
   the realtime clients depend on the same surface the staged
   `Assistant` clients depend on. Any breaking change to the tool
   API lights up both paths in lockstep.

2. **Catalogue reshape touches every provider entry.**
   Mitigation: land Phases 1+2 behind the Phase 1.4 invariant tests
   in one merge.

3. **Realtime APIs are preview-tier; wire schemas may drift.**
   Mitigation: self-contained per-client modules; event handling in
   one `match` per client; provider-specific events do not leak into
   `RealtimeEvent`.

4. **Echo / acoustic feedback in full-duplex mode.**
   Mitigation: trust OS AEC (PipeWire `echo-cancel`, PulseAudio
   `module-echo-cancel`); document in `docs/providers.md`. Do not
   ship a custom AEC. (Same posture as the existing
   `2026-05-25-double-talk-barge-in-pipewire-aec-v1.md` plan, which
   covers AEC enablement; coordinate so the realtime users get the
   benefit of that work.)

5. **Cost / token accounting**. Realtime bills per audio-minute.
   Mitigation: `audio_in_ms` / `audio_out_ms` tracker on
   `PipelineMetrics`; surface in `fono doctor` + history table. The
   wizard's cost label is the user's first line of defence; metrics
   are post-hoc.

6. **Tool call confirmation policy must work over WebSocket too.**
   `voice-actions-via-mcp-v1` allows for a confirmation hook (voice /
   notification / hotkey).
   Mitigation: `Dispatcher` owns the confirmation policy; the
   realtime path calls `dispatcher.execute(call)` which blocks on
   confirmation if configured. The WebSocket stays open; the model
   sees a delayed `function_call_output` and resumes. No realtime-
   specific confirmation UX needed.

7. **Cancellation race**: dropping the session must close the WS
   promptly to avoid billing into a closed pipe.
   Mitigation: `RealtimeSession::Drop` sends `Close` + aborts read
   task; covered by a unit test that drops mid-turn.

8. **Premium-tier opt-in is hard to undo if a user hand-edits config
   then forgets.**
   Mitigation: `fono doctor` "realtime premium" label is sticky and
   visible at every health check; a one-line `RUST_LOG=warn`
   reminder on every session start with a premium model.

9. **WebSocket through restrictive proxies.** Mitigation:
   `tokio-tungstenite` over `rustls` uses the same TLS handshake as
   `reqwest`; document in `docs/troubleshooting.md`; connect failure
   surfaces one critical-notify with guidance to pick a different
   model.

10. **Wizard list bloats as providers add models.** Mitigation:
    surface named defaults first; full list via second-screen
    drilldown when count exceeds ~5. Defer until a provider crosses
    the threshold.

## Alternative Approaches Considered

1. **Ship realtime first, wire tools later.** Faster perceived
   progress. **Rejected** per user feedback: realtime models without
   tools "would probably not use them very often"; we'd be shipping
   a feature in a deliberately gimped state.

2. **Two parallel dispatcher stacks (staged + realtime).** Would
   unblock parallel development. **Rejected**: two policy surfaces,
   two confirmation hooks, two audit logs. Single `Dispatcher` is
   the whole point of `fono-action`.

3. **Stream-only `RealtimeAssistant` trait (v3 proposal).**
   **Rejected**: cannot represent tool-result submission back into
   the same session. Session-handle pattern is the right shape.

4. **`kind: ModelKind` enum (v2 proposal).** **Rejected**:
   conflates orthogonal axes (transport, modality, tools).

5. **Auto-flip to realtime whenever a realtime-capable provider is
   selected.** **Rejected**: cost surprise; opt-in via model
   selection is honest.

6. **Default to the full-tier realtime model.** **Rejected**:
   cost is ~10× the mini tier with marginal quality difference for
   voice assistant use cases. Catalogue invariant 4 makes this a
   compile-time / startup-time error.

7. **Polyfill `RealtimeAssistant` over the staged pipeline for
   providers without S2S.** **Rejected**: latency win comes from
   the model owning VAD and first-audio emission; polyfill = staged
   with extra indirection.
