# Voice-Triggered Actions via MCP — v1

## Objective

Give Fono's existing assistant pipeline the ability to **perform actions**
(turn on lights, start a Pomodoro, run an MCP-exposed tool) in addition
to answering questions. Ship the simplest design that delivers this
end-to-end on the assistant hotkey (F8), while baking in the structural
seams that make the polish-router design (`brainstorm rounds 2–5`,
2026-05-21) a *purely additive* follow-up rather than a rewrite.

The deliberate non-goal of v1 is the two-tier router. That decision is
deferred until we have telemetry showing real users' action-vs-question
ratios and per-backend TTFT distributions. v1 ships **Alternative A**
from the brainstorm: the configured assistant LLM does its own
tool-calling decision, no separate dispatcher.

## Outcome

A user with Home Assistant on their LAN and a cloud assistant backend
configured (Groq / Cerebras / OpenAI / Anthropic) holds F8, says "turn
on the kitchen lights", and the lights come on. The same user can hold
F8, ask "why does my Rust build fail with E0277", and get a normal
streamed spoken explanation. **One hotkey, one LLM, one config; the
LLM decides per turn whether to invoke a tool or stream prose.**

Concrete user-facing surfaces:

- New `[assistant.tools]` config block with one or more MCP server
  entries (stdio + SSE transport).
- Tools advertised to the assistant via the existing OpenAI / Anthropic
  tool-calling API (normalised across providers in `fono-assistant`).
- A small in-process tool registry shipping two built-ins by default:
  `pomodoro_start` / `pomodoro_cancel`. Demonstrates the
  in-process-tool path without depending on an external MCP server.
- Wizard prompt: "Enable voice actions? (configures Home Assistant via
  MCP)" with optional HA URL + token capture. No-op when declined.
- Tray "Actions" submenu listing enabled tool servers + a global
  "Actions enabled" toggle.
- `fono doctor` "Actions:" section listing reachable MCP servers, tool
  counts, and any handshake failures.
- `[assistant.tools.confirmation]` config knob — defaults to "off"
  (everything fires immediately) but the structure exists so we can
  layer voice/notification/hotkey confirmation in later without schema
  churn.

What v1 deliberately does **not** include:

- The polish-LLM-as-router design. Tracked separately, will land as v2
  on top of this plan's extension seams.
- Local-LLM tool calling. Only cloud assistants get tools in v1 because
  llama-cpp-2 grammar-constrained sampling is its own slice of work
  (see `Future work` at bottom).
- Vision / screen-capture tools. Belongs to its own plan; the trait
  shape is forward-compatible.
- Confirmation UX beyond the config skeleton. Designed for, not
  implemented.

## Architecture

### Where it sits in the existing pipeline

```
F8 ─ audio ─ STT ─ fono-assistant::Assistant::reply_stream() ─┐
                                                              │
                                            ┌─── streamed token ───┐
                                            │                      ▼
                                            │            TTS ── speaker
                                            ▼
                                    ToolCallEvent
                                            │
                                            ▼
                                    fono-action::Dispatcher
                                            │
                                            ▼
                          ┌────────┬────────┴────────┐
                          ▼        ▼                 ▼
                     in-process  MCP stdio       MCP HTTP/SSE
                     (pomodoro)  (HA, GitHub)    (LAN services)
                          │        │                 │
                          └────────┴────────┬────────┘
                                            ▼
                                      ToolResult
                                            │
                                            ▼
                                  fed back into assistant
                                  for natural-language reply
```

The assistant stream is *already* `BoxStream<Result<TokenDelta>>`
(`crates/fono-assistant/src/traits.rs:38-42`). We extend `TokenDelta`
with optional structured fields for tool calls. Existing
text-only consumers continue to work unchanged.

### New crate: `fono-action`

| File | Role |
|---|---|
| `src/lib.rs` | Re-exports + crate doc. |
| `src/tool.rs` | `Tool`, `ToolSpec` (JSON Schema), `ToolCall`, `ToolResult`, `ToolError`. |
| `src/registry.rs` | `ToolRegistry` — owns all enabled tools (in-process + MCP-imported). One source of truth. |
| `src/dispatcher.rs` | `Dispatcher::execute(call) -> ToolResult`. Routes by tool name → backend. Handles confirmation hook. |
| `src/builtin/mod.rs` | In-process tools, one file per family. |
| `src/builtin/pomodoro.rs` | `pomodoro_start`, `pomodoro_cancel`. Uses `tokio::time::sleep` + tray badge. |
| `src/mcp/mod.rs` | `McpClient` trait + factory. |
| `src/mcp/stdio.rs` | Stdio JSON-RPC transport. |
| `src/mcp/sse.rs` | HTTP/SSE transport. |
| `src/mcp/protocol.rs` | Wire types (initialize, list_tools, call_tool, …). |
| `src/confirmation.rs` | `ConfirmationPolicy` trait + `NoOpPolicy`. Forward-compatible skeleton. |

### Extension seams (the reason v1 is structured this way)

Every numbered item below is a place where the polish-router or other
follow-on work can plug in without modifying call sites or breaking
APIs.

1. **`ToolRegistry` is a separate type from the assistant.** Today the
   assistant calls `registry.list_for_turn(transcript)` once at turn
   start and gets a `Vec<ToolSpec>`. v2's polish-router calls the same
   method but with a different transcript-context and gets the
   prefiltered subset.
2. **`Dispatcher::execute` takes `&ToolCall`, not a token stream.** The
   thing that decides *what* to dispatch (today: the assistant LLM; v2:
   polish or assistant) is upstream of dispatch. Dispatcher is reusable
   verbatim.
3. **`AssistantContext` gains `tools: Vec<ToolSpec>` field.** Assistant
   implementations consume this. Future v2 polish-context will pass the
   same field shape; same downstream consumer code.
4. **`TokenDelta` gains `tool_call: Option<ToolCallDelta>`.** Streaming
   detection of tool calls is a property of the assistant impl, not the
   pipeline. A grammar-constrained polish dispatcher in v2 emits the
   same `ToolCallDelta` shape.
5. **`ConfirmationPolicy` trait** is wired in v1 with a `NoOpPolicy`
   default. Voice/notification/hotkey confirmation later swap the impl
   without touching dispatch.
6. **MCP client transports behind a `Transport` trait.** Stdio and SSE
   in v1; future transports (Unix socket, named pipe) just add an impl.
7. **Tool prefilter is a no-op function in v1** but exists as a hook
   (`registry.prefilter(transcript) -> Vec<ToolSpec>`, default impl
   returns all). v2 plugs in regex/keyword filtering here.
8. **Config has a `[assistant.router]` block reserved.** Empty in v1;
   v2 populates without schema migration churn.
9. **Per-tool `capability` metadata** (`state_read`, `state_write`,
   `dangerous`, `requires_confirmation`) is on `ToolSpec` from day 1.
   Confirmation UX and router prefilter both read it later.
10. **`fono-assistant` calls `dispatcher.execute()` via a `&dyn
    Dispatcher` trait object, not a concrete type.** v2's polish-router
    is a different `Dispatcher` impl that wraps the v1 one. Drop-in
    replacement.

### Configuration shape (final form, extension seams included)

```toml
[assistant.tools]
enabled = true                          # master toggle

[assistant.tools.confirmation]
# v1: structure exists, default policy is "always allow"
default = "auto"                        # auto | always | never | per_tool
per_tool = {}                           # filled later; tool_name -> "always"/"never"

[[assistant.tools.mcp]]                 # repeatable
name = "home_assistant"
transport = "sse"
url = "http://homeassistant.local:8123/mcp_server/api"
token_env = "HA_TOKEN"                  # secrets.toml-style; the actual
                                        # token lives there, not here

[[assistant.tools.mcp]]
name = "github"
transport = "stdio"
command = ["mcp-server-github"]
env = { GITHUB_TOKEN = "from-secrets" }

# Reserved for v2; v1 ignores this block but accepts it without error.
[assistant.router]
mode = "never"                          # never (v1 default) | auto | always
```

The `[assistant.router]` block being parsed (and ignored) in v1 means
v2 lands without a config-migration step: users who edited it in
anticipation just have it start working.

### Tool-call normalisation across providers

`fono-assistant` already abstracts over OpenAI-compat (Cerebras / Groq /
OpenAI / OpenRouter / Ollama) and Anthropic. Tool calling has two
provider conventions:

- **OpenAI-style:** `tools: [{type: "function", function: {name, description, parameters: JSONSchema}}]`. Streamed back via `tool_calls[].function.{name, arguments}` deltas.
- **Anthropic-style:** `tools: [{name, description, input_schema}]`. Streamed back as `content[].type == "tool_use"` blocks.

The normalisation lives in `fono-assistant/src/tool_translation.rs`
(new): a single `serialize_tools(&[ToolSpec], provider)` /
`parse_tool_call_delta(raw, provider) -> Option<ToolCallDelta>` pair.
Provider differences are quarantined.

Anthropic and OpenAI-compat both support the tool-use loop natively
(submit tool result back as a message, get the spoken reply). v1
implements the round-trip end-to-end for both families.

## Phases

### Phase 0 — Decisions and ADRs

- [ ] ADR 0028 "Voice-triggered actions via MCP". Records the decision
      to ship Alternative A first, defer router to v2, use MCP as the
      first-class action transport, and the extension seams above.
- [ ] ADR 0029 "Action confirmation policy" (skeleton). Documents the
      `ConfirmationPolicy` trait and the four policy types the schema
      reserves (`auto` / `always` / `never` / `per_tool`). v1 ships
      `NoOpPolicy`; concrete policies are future work.
- [ ] ROADMAP.md "In progress" entry.
- [ ] CHANGELOG.md `[Unreleased]` under `Added`.

Verification: ADRs reviewed, ROADMAP + CHANGELOG entries pass the
release-checklist gate when the work ships.

### Phase 1 — `fono-action` crate skeleton

- [ ] Create `crates/fono-action/` with `Cargo.toml`, `src/lib.rs`,
      SPDX header on every file.
- [ ] `Tool`, `ToolSpec`, `ToolCall`, `ToolResult`, `ToolError` types.
      `ToolSpec` carries `name`, `description`, `input_schema:
      serde_json::Value`, `capability: ToolCapability` (enum:
      `StateRead`, `StateWrite`, `Action`, `Dangerous`), `provider:
      String` (source server name for debugging).
- [ ] `ToolRegistry` type with `register`, `unregister`,
      `list_all`, `list_for_turn(&str)` (no-op prefilter in v1), and a
      `Drop` impl that tears down MCP clients cleanly.
- [ ] `Dispatcher` trait + `DefaultDispatcher` impl. `execute(&ToolCall)
      -> Result<ToolResult>`. Confirmation hook is a call to
      `policy.should_dispatch(&call)` returning a `Decision` enum
      (`Allow` / `Deny` / `RequireConfirmation`); the `RequireConfirmation`
      arm is unreachable in v1 (no policy emits it) but compiles.
- [ ] `ConfirmationPolicy` trait + `NoOpPolicy` (always `Allow`).
- [ ] Unit tests covering registry registration, dispatch routing
      by name, error propagation, missing-tool case.

Verification: `cargo test -p fono-action` green. Crate compiles
standalone with `default-features = false` ready for feature gating in
later phases.

### Phase 2 — In-process tools: Pomodoro

- [ ] `builtin/pomodoro.rs` with two tool impls + a `PomodoroState`
      keyed by user-level session ID. Cancel hotkey (Escape) already
      cancels assistant turns; extend to cancel an active timer.
- [ ] Tray badge: "🍅 14:23" while a timer is active (additive on the
      existing tray, no new submenu).
- [ ] Critical-notification fire on timer expiry via the existing
      `fono_core::critical_notify` plumbing (`crates/fono-core/src/critical_notify.rs:37-69`).
- [ ] `tests/pomodoro_round_trip.rs` — drives `start` → simulated time
      → expiry notification path.

Verification: `cargo test -p fono-action --test pomodoro_round_trip`
green. Manual: F8 "start a pomodoro for 1 minute" → tray badge appears
→ notification at 60s.

### Phase 3 — MCP client implementation

- [ ] `mcp/protocol.rs` with serde types for the subset of MCP we
      consume: `initialize`, `initialized`, `list_tools`, `call_tool`,
      `tools/list_changed` notification. Reference:
      https://spec.modelcontextprotocol.io/specification/.
- [ ] `mcp/transport.rs` with `Transport` trait (async `send` / `recv`
      JSON values).
- [ ] `mcp/stdio.rs` — spawn subprocess, line-delimited JSON-RPC over
      stdout/stdin. Reaps subprocess on `Drop`. Surfaces stderr via
      `tracing::warn!`.
- [ ] `mcp/sse.rs` — HTTP POST request + SSE response stream via
      `reqwest`. Reuses the existing `fono-core` HTTP client.
- [ ] `McpClient` aggregating type that owns a `Transport`, performs
      the `initialize` handshake on construction, calls `list_tools`,
      and exposes `call_tool(name, arguments) -> Result<ToolResult>`.
- [ ] Integration test `tests/mcp_stdio_round_trip.rs` driving a
      Python-stub MCP server (one file under `tests/fixtures/`) through
      `initialize` → `list_tools` → `call_tool` → result. Skips when
      `python3` is unavailable.

Verification: `cargo test -p fono-action --test mcp_stdio_round_trip`
green when `python3` is present; cleanly skipped otherwise.

### Phase 4 — Wire MCP into the registry

- [ ] `ToolRegistry::load_from_config(&AssistantTools)` constructor:
      spawns each configured MCP client, awaits handshake, imports
      tools into the registry tagged with their server name. Failures
      are logged and the server is marked dead but don't poison the
      registry — other servers and built-in tools stay live.
- [ ] Tool-name collisions across servers: prefix with server name
      (`home_assistant.light_turn_on`). Documented behaviour.
- [ ] `tools/list_changed` notifications trigger a re-import for the
      affected server. Important for HA's dynamic entity registry.
- [ ] Daemon startup hook: after polish + STT + assistant are built,
      build the `ToolRegistry`; pass an `Arc<ToolRegistry>` and an
      `Arc<dyn Dispatcher>` into the assistant orchestrator.

Verification: with one stub MCP server in config, `fono doctor` prints
"Actions: 1 server, 3 tools" within 5 s of daemon start. Killing the
stub server shows the server as dead on next `doctor` invocation
without crashing the daemon.

### Phase 5 — Assistant tool-calling integration

This is the largest phase but each piece is small.

- [ ] Extend `TokenDelta` (`crates/fono-assistant/src/traits.rs:14-18`)
      with `tool_call: Option<ToolCallDelta>` and `tool_result_for:
      Option<String>` (the tool call ID being replied to, populated on
      the *next* turn after a tool round-trip).
- [ ] Extend `AssistantContext` with `tools: Vec<ToolSpec>` and
      `tool_results: Vec<(String, ToolResult)>` (for the continuation
      turn).
- [ ] `tool_translation.rs`: `serialize_tools` and
      `parse_tool_call_delta` per provider family. Cover OpenAI-compat
      (`crates/fono-assistant/src/openai_compat_chat.rs`) and Anthropic
      (`anthropic_chat.rs`).
- [ ] Plumb tool serialisation into both backends' request builders.
      Streaming code path now decodes both content deltas and
      tool-call deltas; `BoxStream` yields both kinds.
- [ ] Orchestrator (in `crates/fono/src/assistant.rs`):
      - On `TokenDelta { text, .. }` → existing TTS path unchanged.
      - On `TokenDelta { tool_call: Some(call), .. }` → buffer call
        until the stream completes (tool call args may stream as multiple
        deltas).
      - On stream end with a tool call buffered → invoke
        `dispatcher.execute(&call)`, then issue a *continuation turn*
        to the assistant with the tool result attached. Stream that
        turn's tokens normally.
      - Multi-tool turns: same loop, no special cases.
- [ ] Cancellation: existing Escape cancel + barge-in path must
      cancel a tool dispatch in-flight (the dispatcher's `execute`
      future should be `select!`'d against the cancel channel). MCP
      `call_tool` is best-effort cancelled; in-process tools must
      respect cancellation (the Pomodoro `start` is instant so this
      doesn't matter for it).

Verification: integration test in `crates/fono/tests/`
`assistant_tool_round_trip.rs` driving a mock assistant + mock
dispatcher through tool call → tool result → spoken continuation.
End-to-end manual on Groq or Cerebras with the stub MCP server.

### Phase 6 — Home Assistant configuration UX

- [ ] Wizard step (after the assistant tier picker): "Enable voice
      actions via Home Assistant?" with HA URL + long-lived token
      prompt. Validates handshake before saving. Token stored in
      `secrets.toml` (existing pattern); config file references via
      `token_env`.
- [ ] `docs/providers.md` new section "Voice actions and MCP" with
      HA setup walkthrough (how to install HA's MCP server, where to
      generate a long-lived token, the resulting config block).
- [ ] `fono use actions on|off` CLI subcommand toggles
      `[assistant.tools].enabled`.
- [ ] Tray "Actions" submenu: top entry is enable/disable; under it,
      one row per configured MCP server with reachable/unreachable
      status. Click on a row toggles its `enabled` field.
- [ ] `fono doctor` "Actions:" section: enabled flag, per-server
      status, total tool count, latest handshake error if any.

Verification: wizard flow tested manually against a live HA instance.
`fono doctor` output reviewed.

### Phase 7 — Release engineering

- [ ] Workspace version bump.
- [ ] CHANGELOG.md `[Unreleased]` graduates to a versioned section.
- [ ] ROADMAP.md "In progress" → "Shipped".
- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
      -- -D warnings`, `cargo test --workspace --tests --lib` all green.
- [ ] Binary-size delta vs prior release ≤ 1 MB (MCP transport adds
      `serde_json` and a few KB of wire-types code; we already have
      `reqwest`, `tokio`, `async-trait`). If it exceeds 1 MB, gate
      `fono-action` behind a `actions` cargo feature defaulting on
      and revisit before tag.

Verification: full release-checklist pass.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Provider tool-calling shapes drift (OpenAI updates schema) | `tool_translation.rs` is the only file that knows the wire shape; provider-shaped tests pin the current shape. |
| HA's MCP server protocol still stabilising (introduced HA 2025.2, evolving) | Vendor the protocol version we tested against; gracefully degrade on unknown `initialize` response shape. |
| User confusion between dictation (F7) and actions (F8) | F7 never triggers actions; tool list is only attached to F8 turns. Doctor row makes this explicit. |
| Tool catalogue overflow (HA with 100+ entities, GitHub + others combined) eats assistant context budget | v1 sends the full catalogue; the prefilter hook exists for v2. Document the cost in `providers.md` and the wizard. |
| Cloud assistant sees user's HA entity names / GitHub data via tool descriptions | New `privacy.md` section covering tool-list leakage; wizard warns when enabling cloud assistant + tools simultaneously. |
| Dangerous actions (door locks) fire without confirmation in v1 | Document the v1 limitation; confirmation skeleton is in place for fast follow-up. Recommend users not enable lock-related tools until confirmation lands. |
| MCP stdio subprocess hangs / leaks | `Drop` reaps; integration test asserts subprocess exits within 2 s of `McpClient` drop. |

## Future work (explicitly out of scope for v1)

The pieces below are tracked for visibility but are **not** v1
deliverables. Each is enabled by a v1 extension seam.

### v2 — Polish-LLM-as-router (the design from brainstorm rounds 3–5)

- Activates `[assistant.router].mode = "auto" | "always"`.
- Plumbs a `PolishDispatcher` impl of the `Dispatcher` trait that wraps
  the v1 `DefaultDispatcher`. The polish LLM is called first; on a tool
  hit it dispatches via the wrapped dispatcher; on escalate it routes
  the turn to the configured assistant (which already has tool-calling
  from v1).
- Requires GBNF grammar-constrained sampling in `fono-polish`'s
  llama.cpp invocation (new sampler config in
  `crates/fono-polish/src/llama_local.rs`).
- Requires polish-default model bump to Qwen 2.5 1.5B Instruct (or
  larger) when actions are enabled. Wizard prompt.
- Honest gate: build only if telemetry from v1 shows action-heavy
  laptop-local-assistant users actually exist in non-trivial numbers
  and current latency is felt as annoying.

### Vision / screen-capture tool

- New in-process tool `screen_capture_and_describe`. Capture via
  xdg-desktop-portal on Wayland, `xcb` on X11. Routes to a
  vision-capable assistant backend; errors when none is configured.
- Likely lands alongside or shortly after v2.

### Local-LLM tool calling (for offline-only users)

- Adds `Assistant` impl for local llama.cpp with grammar-constrained
  tool emission. Same `tool_translation.rs` boundary; new impl behind
  it.

### Confirmation policies

- `VoiceConfirmation` ("say yes within 5 s"), `HotkeyConfirmation`
  ("press F8 again"), `NotificationConfirmation` (desktop toast with
  Yes/No actions). `ConfirmationPolicy` trait already exists; each new
  policy is a new impl + a wizard step + a tray submenu row.

### Tool prefilter for context budget

- `registry.prefilter(transcript)` is a no-op in v1; v2 layers in
  regex/keyword matching, future work may layer a tiny embedding model
  for semantic filtering on hosts that can afford it.

## Verification gates summary

Per AGENTS.md pre-commit gate, **every commit** in this plan must pass:

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --tests --lib
```

Each phase additionally has a phase-specific verification listed above.
The release tag at the end of Phase 7 must additionally pass the full
release-checklist (cloud-equivalence gate, size-budget gate, etc. — see
`docs/dev/release-checklist.md`).

## Status

- **2026-05-22** — Plan drafted; awaiting human sign-off before any
  code lands. No phases ticked yet.
- **2026-05-25** — Complementary plan landed for the **inverse**
  direction (Fono as MCP *server* for coding agents) at
  `plans/2026-05-25-fono-voice-loop-for-coding-agents-v1.md`. The two
  plans share JSON-RPC wire types and stdio transport code; whichever
  ships second extracts the common types into a shared
  `fono-mcp-protocol` crate. Do not pre-create the crate.

## Surviving artefacts

Brainstorm rounds that led to this plan (chat history, not committed):

- Round 1 — MCP vs REST for HA, tool catalogue, ActionBackend trait.
- Round 2 — Tool-calling is intent routing; don't add a router.
- Round 3 — Self-escalation; capability framing; tiered model
  discussion.
- Round 4 — Two tiers with polish-RAM reuse; tool-trained small-model
  honest assessment.
- Round 5 — Polish as strict tool-or-escalate dispatcher with GBNF
  grammar.
- Round 6 — Brutal honesty pass: yes, that's an LLM router. Latency
  math. Recommendation: ship the simple thing first, extension seams
  ready for v2.

This plan v1 implements the round-6 recommendation: Alternative A
(direct assistant tool-calling, no polish-router) with the v2
extension seams baked in.

## Relationship to `plans/2026-05-25-fono-voice-loop-for-coding-agents-v1.md`

That plan is the **inverse direction**: while this plan makes Fono an MCP
*client* (dispatching actions to Home Assistant and other MCP servers on
the user's behalf), the voice-loop plan makes Fono an MCP *server* (letting
a coding agent call Fono's voice hardware via `fono.speak`, `fono.listen`,
`fono.confirm`).

Both plans use JSON-RPC wire types and stdio transport. Whichever lands
second will extract the common wire types into a shared `fono-mcp-protocol`
crate. Do not pre-create the crate; the extraction is mechanical once we
know what both plans actually need.
