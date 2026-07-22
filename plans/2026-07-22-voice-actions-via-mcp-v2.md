# Voice-Triggered Actions via MCP — v2

Supersedes `2026-05-22-voice-actions-via-mcp-v1.md`. The v1 shape
(Alternative A: the configured assistant LLM does its own tool-calling,
no router; a `fono-action` crate with registry + dispatcher +
confirmation skeleton; MCP as the connector) **survives review**. What
changed since May is that roughly half of v1's Phase 5 shipped in a
different form, the MCP-server crate landed first, and Home Assistant
moved its MCP endpoint to streamable HTTP. v2 rebases the plan on the
2026-07-21 codebase and narrows the initial user-facing scope to the
two capabilities requested: **(1) Home Assistant light control, (2)
built-in timer / Pomodoro tools.**

## Objective

Hold F8, say "turn on the kitchen lights" → lights come on and Fono
confirms out loud. Say "start a 25-minute pomodoro" → a timer runs
in-process, shows in the tray menu, and fires a desktop notification on
expiry. Questions keep streaming prose exactly as today. One hotkey,
one LLM; the model decides per turn via native function calling.

**Local-first is a hard requirement, not a follow-up.** The same
commands MUST work with the embedded local model, fully on the LAN
(Fono → local model → Home Assistant, zero WAN packets), at
perceived latency comparable to cloud — and of course keep working on
cloud. This is an explicit user requirement (2026-07-22) and it
reshapes the phase ordering below: local tool calling is a numbered,
gated phase rather than deferred future work.

## What already exists (do NOT rebuild)

- **Tool-calling wire layer, OpenAI-compat family** (OpenAI, Groq,
  Cerebras, OpenRouter, Gemini via compat surface, Ollama-remote):
  streamed `tool_calls` delta accumulation and finalization at
  `crates/fono-assistant/src/openai_compat_chat.rs:854-902`, tool
  descriptor builder at
  `crates/fono-assistant/src/openai_compat_chat.rs:300-318`, in-client
  round-trip (turn 1 → execute → continuation POST) at
  `crates/fono-assistant/src/openai_compat_chat.rs:522-617`.
- **Trait-level types**: `ToolCall` at
  `crates/fono-assistant/src/history.rs:46-51`, `TokenDelta.tool_event`
  + `ToolEvent::{Called, Result}` at
  `crates/fono-assistant/src/traits.rs:54-94`, `ChatRole::Tool` +
  history pushers at `crates/fono-assistant/src/history.rs:114-124`.
  v1's proposed `TokenDelta.tool_call` / `tool_translation.rs` are
  obsolete — the shipped shape is different and fine.
- **Daemon-side orchestration**: ToolEvent logging, per-call timing,
  history write-back at `crates/fono/src/assistant.rs:641-648`,
  `crates/fono/src/assistant.rs:908-926`.
- **JSON-RPC / MCP wire types + stdio framing** in
  `crates/fono-mcp-server/src/protocol.rs` and
  `crates/fono-mcp-server/src/transport.rs:34-70` (server-direction
  derives only).
- **SSE parser** (`SseBuffer`, private) at
  `crates/fono-assistant/src/sse.rs:28-97`; watchdog SSE reader in
  `crates/fono-http/src/sse.rs`.
- **Secrets idiom**: `*_ref` keys resolved through
  `crates/fono-core/src/secrets.rs:54-59`. (v1's `token_env` is
  replaced by this.)
- **Notifications**: `fono_core::notify::send` at
  `crates/fono-core/src/notify.rs:28-71`. Tray dynamic menu labels via
  provider closures at `crates/fono-tray/src/lib.rs:63-117` (note: ksni
  has **no badge** — v1's "🍅 14:23" tray badge is replaced by a menu
  row countdown + icon tint).

## Key deltas vs v1 (decisions)

1. **Home Assistant transport is streamable HTTP, not SSE.** Since HA
   2025.2 the `mcp_server` integration serves `/api/mcp` using the
   stateless streamable-HTTP transport with Bearer (long-lived access
   token) auth. v1's SSE transport work is dropped; the client ships
   **streamable HTTP first**, stdio (child-process) second for
   community servers. No `Mcp-Session-Id` lifecycle needed for the
   stateless HA case; design the seam so stateful session support can
   be added later.
2. **HA's tool catalogue is small.** It exposes the Assist intent
   tools (`HassTurnOn`, `HassTurnOff`, light setters, `GetLiveContext`)
   over *exposed entities only* — not one tool per entity. v1's
   "catalogue overflow" risk is largely retired for HA; the prefilter
   hook stays as a seam only.
3. **Shared wire types, not a new protocol module.** The voice-loop MCP
   server shipped first, so per the v1 extraction clause: widen the
   Serialize/Deserialize derives on the existing types in
   `crates/fono-mcp-server/src/protocol.rs` (they are direction-
   asymmetric today) and let the client import them. Only extract a
   `fono-mcp-protocol` crate if a dependency cycle forces it.
4. **Cloud OpenAI-compat family lands first; local and Anthropic are
   numbered phases, not deferrals.** The OpenAI-compat family (OpenAI,
   Groq, Cerebras, Gemini-compat, OpenRouter, Ollama-remote) already
   has the wire layer, so it is the first working target. Anthropic has
   zero function calling today
   (`crates/fono-assistant/src/anthropic_chat.rs:242-252` ignores
   `input_json_delta`) — its own phase, droppable to a fast-follow.
   **Embedded local llama.cpp tool calling is now Phase 5 (was v1
   out-of-scope)** per the local-first requirement; the dormant
   prompt-cache slot at
   `crates/fono-assistant/src/llama_local.rs:1591-1594` is the seam it
   plugs into. Prior art exists: the `homeassistant_lights` bench
   (`crates/fono-bench/src/assistant_tool_use.rs`) was run against
   gemma-4-12b over the local server on 2026-07-04 — tool calls
   parsed correctly but only 50–67% pass and p50 7.6–8.7 s
   end-to-end (report archived in `../fono-tmp/`). That is the
   baseline Phase 5 must beat.
5. **Needle-class tiny dispatcher is the measured local-latency
   fallback (Phase 6 spike).** cactus-compute/needle is a 26M-param
   MIT-licensed single-shot function-call model — GPL-compatible, could
   be a default, but runs on a custom architecture (no llama.cpp
   support) so it needs self-export to ONNX on the ORT runtime already
   in the binary, plus a new autoregressive decode loop in Rust. It
   cannot converse (dispatch-only) so it fits the deferred
   polish-LLM-as-router seam (`[assistant.router]`), not the assistant
   role. It becomes relevant only if Phase 5's general local path
   can't hit the latency gate; spike-first, no shipping code until
   measured.
6. **Realtime parity is planned, staged ships first.** ROADMAP promises
   voice actions "in lockstep" for staged + realtime, and
   `crates/fono-assistant/src/traits.rs:200-204` already reserves the
   `ToolCallRequested` event "when fono-action lands". Gemini Live
   already declares one hardcoded tool
   (`crates/fono-assistant/src/gemini_live.rs:188-194`), so the
   plumbing extends rather than starts from zero. It is its own phase,
   gated on the staged path being verified.
7. **Generalize the existing single-tool loop instead of adding a
   parallel one.** The `fono_screen` round-trip is the template:
   replace the hardcoded descriptor with a tool list from the registry,
   turn the scalar tool-call accumulator
   (`openai_compat_chat.rs:785-787`) into a `Vec`, and allow a bounded
   multi-iteration loop (tools attached on continuations, hard cap ~4
   iterations) instead of the current one-shot anti-loop guard.
   `fono_screen` migrates into the registry as a first-class tool.
8. **Built-ins ship before MCP.** Pomodoro/timer exercises registry +
   dispatch + LLM loop end-to-end with zero network dependencies —
   fastest path to a demo and to validating the seams.

## Phases

### Phase 0 — ADRs and bookkeeping

- [ ] ADR 0038 "Voice-triggered actions" — records Alternative A,
      OpenAI-compat-only v1 scope, streamable-HTTP-first MCP client,
      shared wire types with `fono-mcp-server`, confirmation skeleton,
      and the realtime / Anthropic / local-LLM deferrals.
- [ ] ROADMAP.md: refresh the Voice actions entry (SSE → streamable
      HTTP; scope note); CHANGELOG `[Unreleased]` → `Added`.

### Phase 1 — `fono-action` crate: types, registry, built-ins

- [ ] Create `crates/fono-action` (SPDX headers, deny.toml unchanged —
      target **zero new external crates**; serde/serde_json/tokio/
      reqwest are all in-graph).
- [ ] `ToolSpec { name, description, input_schema, capability, provider }`
      with `ToolCapability` (`StateRead`/`StateWrite`/`Action`/`Dangerous`),
      `ToolResult`, `ToolError`. Reuse `fono_assistant::ToolCall` rather
      than duplicating it (check crate direction; move it into
      `fono-action` and re-export if the dependency points the wrong way).
- [ ] `ToolRegistry` (`register` / `list_for_turn` with no-op prefilter
      hook) and `Dispatcher` trait + `DefaultDispatcher::execute(&ToolCall)`.
- [ ] `ConfirmationPolicy` trait + `NoOpPolicy`; `Decision::{Allow,
      Deny, RequireConfirmation}` (last arm unreachable in v1).
- [ ] Built-ins: `timer_start`, `pomodoro_start`, `timer_cancel`,
      `timer_status`. Implementation: tokio timer task in the daemon,
      expiry via `notify::send` (Normal urgency + sound-free), state
      queryable over a new IPC `Request` variant.
- [ ] Tray: countdown row via the existing provider-closure pattern
      ("Pomodoro — 17:32 · click to cancel" → new `TrayAction`).
      Escape-cancel path also cancels an active timer only when the
      overlay/assistant is idle (do not overload mid-turn Escape).
- [ ] Unit tests: registry routing, dispatch errors, missing tool,
      timer lifecycle with `tokio::time::pause`.

Verification: `cargo test -p fono-action` green.

### Phase 2 — Generalize the assistant tool loop (OpenAI-compat)

- [ ] `AssistantContext` gains `tools: Vec<ToolSpec>` +
      `tool_executor: Option<Arc<dyn ToolExecutor>>` (same pattern as
      the existing `screen_capture: ScreenCaptureFn` at
      `crates/fono-assistant/src/traits.rs:100-138`; executor calls the
      dispatcher, keeping the round-trip inside the provider client as
      today).
- [ ] Replace `build_screen_tool()` special-casing: serialize the full
      tool list; `fono_screen` becomes a registry entry whose executor
      closes over the capture fn (image-content continuation handling
      stays as the one special content shape).
- [ ] Scalar accumulator → `Vec<ToolCallAccumulator>` (parallel tool
      calls per turn); bounded loop: continuations keep the `tools`
      field, hard iteration cap (default 4), then force a text turn.
- [ ] Emit `ToolEvent::Called/Result` per call (already consumed by
      `crates/fono/src/assistant.rs`); history write-back extends the
      existing canonical triplet to N calls.
- [ ] Cancellation: `select!` the executor future against the existing
      cancel channel; Escape/barge-in aborts an in-flight dispatch.
- [ ] Daemon wiring: build `ToolRegistry` + dispatcher at session
      construction; attach to F8 turns only (F7 dictation never gets
      tools); `[assistant.tools].enabled` master toggle (default off
      until Phase 4 lands, flipped on at release).
- [ ] Integration test: mock executor → tool call → result →
      spoken continuation; multi-call turn; iteration-cap test. Extend
      `crates/fono-bench/src/assistant_tool_use.rs` with a
      lights/pomodoro scenario.

Verification: manual on Groq/Cerebras — "start a five minute timer"
works end-to-end with no MCP configured.

### Phase 3 — MCP client (streamable HTTP + stdio)

- [ ] Widen derives on `crates/fono-mcp-server/src/protocol.rs` types
      (Serialize on `ClientMessage`/`InitializeParams`/`ToolCallParams`,
      Deserialize on `InitializeResult`/`ToolsListResult`/`ToolDef`/
      `ToolCallResult`/`ContentBlock`); round-trip tests.
- [ ] `fono-action/src/mcp/http.rs` — streamable HTTP client:
      POST-per-message to the server URL, `Accept: application/json,
      text/event-stream`, Bearer auth via `auth_token_ref` +
      `Secrets::resolve`, SSE-response handling by promoting
      `SseBuffer` to `pub` (or moving it to `fono-http` next to the
      watchdog). Stateless first; session-ID seam stubbed.
- [ ] `fono-action/src/mcp/stdio.rs` — child-process transport
      (`tokio::process`, piped stdio, stderr drained to
      `tracing::warn!`, reap on drop with 2 s kill escalation).
- [ ] `McpClient`: initialize handshake (advertise protocol
      `2025-06-18`, accept server's version), `tools/list` import into
      the registry (namespaced `home_assistant.HassTurnOn` on
      collision), `tools/call` with per-call timeout, pending-id map
      for correlation.
- [ ] Config: `[[assistant.tools.mcp]]` repeatable block
      (`name`, `transport = "http" | "stdio"`, `url`/`command`,
      `auth_token_ref`), following the `#[serde(default)]` +
      skip-if-default house pattern in `crates/fono-core/src/config.rs`.
- [ ] Failure isolation: a dead/unreachable server logs, marks itself
      down, and never poisons built-ins or other servers.
- [ ] Integration test: Python-stub MCP server fixture over stdio
      (initialize → list → call), skipped without `python3`; HTTP
      transport tested against an in-process hyper stub.

Verification: against a live HA instance — `fono doctor` shows the
server, tool count, and "turn off the desk lamp" round-trips.

### Phase 4 — UX: wizard, doctor, tray, CLI, docs

- [ ] Wizard step (cloud-assistant lane only): "Enable voice actions
      via Home Assistant?" → URL + long-lived token, handshake
      validated before save, token → `secrets.toml`.
- [ ] `fono doctor` "Actions:" section (enabled flag, per-server
      status + tool count, last handshake error).
- [ ] `fono use actions on|off`; tray "Actions" submenu (master toggle
      + per-server status rows).
- [ ] Settings web page: Actions card in the assistant section.
- [ ] `docs/providers.md` "Voice actions" section: HA `mcp_server`
      setup, exposed-entities scoping, token generation, privacy note
      (tool names + entity names travel to the cloud assistant;
      Assist-exposure list is the control surface).

### Phase 5 — Embedded local model tool calling (LAN-only, gated)

Ordered after cloud parity because it reuses the identical
`AssistantContext` tools/executor + `Arc<dyn Dispatcher>` seam, but it
is a **release requirement**, not optional.

- [ ] Per-family tool prompt templates in
      `crates/fono-assistant/src/llama_local.rs` (Qwen `<tool_call>`,
      Gemma JSON) fed from the same `ToolSpec` list; stop dropping
      `ChatRole::Tool` turns (`llama_local.rs:1906`, `llama_local.rs:1934`).
- [ ] **GBNF / JSON-schema-constrained sampling** in
      `crates/fono-core/src/llama_gen.rs:85-89` when tools are attached,
      so small models emit only parseable, in-schema calls — the direct
      fix for the 50–67% pass rate.
- [ ] **Prompt-cache the tool catalogue** into the reserved
      `AssistantTools` checkpoint (`llama_local.rs:1591-1594`) so warm
      turns prefill only the user utterance — the main TTFT lever.
- [ ] **Templated confirmations for `Action`-capability tools**
      ("Done — kitchen lights on.") that skip the second model turn;
      config knob to force full model phrasing. Halves local latency
      and is a free win on cloud too.
- [ ] Wizard recommends a tool-capable small instruct model (Qwen-class)
      when local assistant + actions are both enabled; larger models are
      the quality fallback, not the default.
- [ ] Gate (commit the bench run alongside): `fono-bench
      assistant-tool-use` on `homeassistant_lights`, EN + RO, **≥ 90%
      pass and p50 end-to-end ≤ ~2.5 s** on reference hardware, beating
      the 2026-07-04 gemma-4-12b baseline (50–67% / 7.6–8.7 s).

Verification: bench gate met; a full "turn on the kitchen light" turn
round-trips with the embedded model and no packet leaves the LAN
(verified by network capture / offline test).

### Phase 6 — Router-dispatcher spike (Needle-class), measurement-only

Only if Phase 5's general path misses the latency gate on modest
hardware. Produces a go/no-go memo before any shipping code (same
pattern as the NPU / vision spikes in ROADMAP).

- [ ] Probe ONNX exportability of cactus-compute/needle via the pinned
      `tmp/venv` converter; run the op-union diff against
      `../fono-voice/onnxruntime/ops.config` (expect a possible
      minimal-runtime rebuild, per the ReDimNet2 precedent).
- [ ] Measure zero-shot accuracy on the `homeassistant_lights`
      fixtures + the built-in timer tools; measure tool-decision
      latency.
- [ ] Design the escalate-vs-dispatch convention (query that is not a
      tool call must hand off to the full assistant).
- [ ] Memo: ship as `[assistant.router]` dispatcher, or reject.

### Phase 7 — Realtime (Gemini Live) parity

- [ ] `RealtimeEvent::ToolCallRequested` + a tool-result submission
      channel on the session (extend the existing `functionDeclarations`
      setup at `crates/fono-assistant/src/gemini_live.rs:188-200` to
      include registry tools alongside `end_conversation`).
- [ ] Daemon realtime loop dispatches via the same
      `Arc<dyn Dispatcher>`; confirmation policy shared.
- [ ] May ship one release after Phases 1–4 if verification drags;
      ROADMAP wording then says "realtime follows next release".

### Phase 8 — Anthropic native tool_use (droppable to fast-follow)

- [ ] Serialize `ToolSpec` to Anthropic `tools` shape; parse
      `content_block` `tool_use` + `input_json_delta` accumulation in
      `crates/fono-assistant/src/anthropic_chat.rs`; `tool_result`
      continuation messages; reuse the same executor/dispatcher.

### Phase 9 — Release engineering

- [ ] Pre-commit gate + `./tests/check.sh --size-budget` (expected
      growth ≈ tens of KB — no new crates; if any new dep becomes
      unavoidable, flag it per AGENTS.md before adding).
- [ ] CHANGELOG section, ROADMAP move to Shipped, version bump,
      release checklist.

## Out of scope (unchanged from v1 unless noted)

- Polish-LLM router (`[assistant.router]` block still parsed +
  ignored as the v2 seam; the Needle spike in Phase 8 is what may
  eventually populate it).
- Confirmation UX beyond the `NoOpPolicy` skeleton. Until it lands,
  the HA exposed-entities list is the documented safety boundary —
  tell users not to expose locks/covers/garage doors.
- OAuth to HA (long-lived token only in v1; OAuth is a follow-up if
  demand appears).
- GitHub/calendar/etc. servers: *work* via the generic stdio/HTTP
  client but get no wizard lane or docs walkthrough in v1.

## Risks

| Risk | Mitigation |
|---|---|
| Generalizing the screen-tool loop regresses `fono_screen` | It becomes a registry tool exercised by the existing bench (`crates/fono-bench/src/assistant_tool_use.rs`) + integration tests before any MCP code lands. |
| Multi-iteration loop runaway (model keeps calling tools) | Hard cap (4), tools stripped on the final forced turn, per-call timeout, Escape cancels in-flight dispatch. |
| HA protocol still evolving (spec marked "work in progress") | Pin tested protocol rev in the handshake; degrade gracefully on unknown shapes; doctor surfaces the negotiated version. |
| Widening `fono-mcp-server` derives couples client and server | Types are already round-trip tested; if coupling hurts, mechanical extraction to `fono-mcp-protocol` is the pre-agreed escape hatch. |
| Cloud assistant sees entity names | Documented; exposed-entities page is the control; wizard shows a one-line privacy note. |
| Dangerous actions fire unconfirmed | Capability metadata on `ToolSpec` from day 1; docs recommend not exposing locks; confirmation policies are the first fast-follow. |
| Small local models emit malformed / wrong tool calls (50–67% baseline) | GBNF-constrained sampling + the `homeassistant_lights` bench gate + Needle-class fallback (Phase 6). |
| Local second confirmation turn dominates latency (~3 s of the 7–8 s) | Templated confirmations for `Action` tools skip the second model turn; prompt-cached catalogue cuts first-turn TTFT. |

## Verification criteria

- "Turn on/off <light>" works against live HA via `/api/mcp` with a
  long-lived token, spoken confirmation included.
- "Start a pomodoro / 5-minute timer" works with **no** MCP server
  configured; tray countdown visible; notification on expiry; Escape
  or tray click cancels.
- **Local/LAN parity**: "turn on/off <light>" works with the embedded
  model against HA on the LAN with zero WAN packets, meeting the Phase
  5 bench gate (≥ 90% pass, p50 ≤ ~2.5 s).
- Q&A turns are byte-identical in behaviour with `[assistant.tools]`
  disabled; F7 dictation never sees tools.
- `fono doctor` reports server health; a dead server degrades, never
  crashes.
- Size budget green; zero new external crates (or signed-off).

## Status

- **2026-07-21** — v2 drafted after codebase + HA-docs review; v1
  architecture confirmed, phases rebased. Awaiting sign-off.
- **2026-07-22** — Amended after user review: (1) local embedded
  tool calling promoted from out-of-scope to a gated release
  requirement (Phase 5) with the `homeassistant_lights` bench as its
  bar, anchored to the 2026-07-04 gemma-4-12b baseline; (2) added the
  Needle-class router-dispatcher spike (Phase 6) as the measured
  local-latency fallback; (3) recorded the LAN-only, cloud-comparable
  latency goal in the objective and verification criteria. Still
  awaiting sign-off before code lands.
