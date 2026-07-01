# Fono as a local LLM inference server (OpenAI- + Ollama-compatible)

## Objective

Expose Fono's already-embedded local LLM inference (llama.cpp GGUF chat
models) over the network through a **universal HTTP API** so any
OpenAI- or Ollama-speaking client — editors, `llm`, LangChain, Open
WebUI, and crucially **Home Assistant** — can use Fono as its local
inference backend. This is the mirror image of what Fono already does
as a *client* (`crates/fono-assistant/src/openai_compat_chat.rs`
consumes exactly this API surface today) and follows the same
"bind a listener, serve the active `Arc<dyn Trait>`" pattern the
Wyoming STT/TTS/wake server already ships.

The single-binary, size-first spirit holds: the inference engine, model
management, streaming, sampler/stop policy, and prompt-state cache are
**already in the binary**. This work adds only a thin HTTP shell around
them.

## Pinned decisions (settled in chat, 2026-07-01)

| Decision | Choice | Rationale |
|---|---|---|
| HTTP layer | **Raw `hyper 1.x`** (`service_fn` + hand-rolled path `match`). **No axum.** | `hyper`/`hyper-util`/`http-body-util`/`bytes` are already in the graph (via `reqwest`); axum + `matchit` are net-new crates. Route count is small; a framework buys only weight. Verified: `matchit` absent, all hyper deps present. |
| Wire formats | **Both** OpenAI-compatible **and** Ollama-native, one listener. | OpenAI = universal reach; Ollama-native `/api/tags` + `/api/chat` is what Home Assistant's Ollama conversation integration probes. Near-zero byte cost (shared core + `serde` already present). |
| Reuse | Drive the existing **`Assistant` trait** (`reply_stream`). | The server is a wire adapter over `AssistantContext` → `BoxStream<TokenDelta>`. No new inference code. |
| Home crate | **`fono-net`** (new `llm_server` module), mirroring `wyoming/server.rs`. | Already hosts LAN servers and depends on `fono-stt`/`fono-tts` traits; adding a `fono-assistant` trait edge is net-zero on binary size (crate already in graph). |
| Bind default | `127.0.0.1` | Same safety posture as `[server.wyoming]` (`crates/fono-core/src/config.rs:1274`). LAN exposure is explicit opt-in. |
| Feature gating | Compiled in unconditionally (no gate). | Per maintainer: all features compile into the one shipped artefact; there are no ship variants to hide a gate in. Size impact is measured against the budget (below), not hidden. |
| Tool/function calling | **Phase 2**, promoted to MVP *iff* the HA device-control story is wanted early. | Chat-only ships first. HA light control (Direction B) needs `tools` passthrough; the plumbing (`ToolEvent`/`ToolCall` in `history.rs`) already exists. |

## Non-goals (this plan)

- Embeddings endpoints (`/v1/embeddings`, `/api/embed`).
- Model pulling/creation (`/api/pull`, `/api/create`, `/api/push`).
- Multi-model hot-swapping in one process (serve the one configured
  local model; list it in discovery).
- Authentication beyond the optional pre-shared bearer token already
  modelled by `[server.wyoming].auth_token_ref`.
- The web configuration UI. It is a *future consumer of the same hyper
  listener*; this plan leaves room for it (see Architecture) but does
  not build it.

## Background — what already ships

- **Embedded llama.cpp assistant** with streaming, prompt-state cache,
  shared sampler/stop policy — `crates/fono-assistant/src/llama_local.rs`,
  `crates/fono-core/src/llama_gen.rs`.
- **`Assistant` trait**: `reply_stream(user_text, ctx) ->
  BoxStream<'static, Result<TokenDelta>>` — `crates/fono-assistant/src/traits.rs:146-156`.
  `AssistantContext` carries `system_prompt`, `history: Vec<ChatTurn>`,
  `max_new_tokens`, tool/vision knobs (`:108-130`).
- **History types** `ChatTurn` / `ChatRole` / `ToolCall` / `ToolEvent`
  — `crates/fono-assistant/src/history.rs`.
- **The exact wire we must emit, already parsed as a client** —
  `crates/fono-assistant/src/openai_compat_chat.rs` (SSE
  `chat.completions` decode; reuse the struct shapes in reverse).
- **LAN server precedent**: `WyomingServer` accept loop + hot-reload via
  a provider closure — `crates/fono-net/src/wyoming/server.rs:1-49`,
  `:216-233`. Spawned by `spawn_wyoming_server_if_enabled`
  (`crates/fono/src/daemon.rs:3157`).
- **Orchestrator snapshots**: `stt_snapshot()` (`crates/fono/src/session.rs:1101`),
  `tts_snapshot()` (`:1110`), `current_assistant() -> Option<Arc<dyn Assistant>>`
  (`:3002`). We add a public `assistant_snapshot()` accessor.
- **`[server]` config** with `wyoming: ServerWyoming`
  (`crates/fono-core/src/config.rs:1236-1279`). We add `llm: ServerLlm`.
- **mDNS advertiser/browser** — `crates/fono-net/src/discovery/`.
- **HTTP server deps already in the graph** (verified via `cargo tree`):
  `hyper 1.9`, `hyper-util 0.1`, `http-body-util 0.1.3`, `bytes 1.11`,
  `serde`/`serde_json`. **Zero new crates required.** `hyper` is
  currently client-only; we enable its `server` + `http1` features.

## Architecture

```
                 ┌────────────────────────────────────────┐
 HTTP client ───▶│  fono-net::llm_server (hyper listener)  │
 (HA / editor /  │  ┌──────────────┐   ┌────────────────┐  │
  Open WebUI)    │  │ OpenAI routes│   │ Ollama routes  │  │
                 │  │ /v1/models   │   │ /api/tags      │  │
                 │  │ /v1/chat/... │   │ /api/chat      │  │
                 │  └──────┬───────┘   └───────┬────────┘  │
                 │         └──── translate ────┘           │
                 │                  │                      │
                 │        Arc<dyn Assistant> provider      │
                 └──────────────────┼──────────────────────┘
                                    ▼
              fono-assistant::LlamaLocalAssistant.reply_stream
                     (existing engine, cache, sampler)
```

- **One listener, one router.** A single `hyper` server binds
  `[server.llm].bind:port`. A hand-rolled `route(&method, path)` match
  dispatches to handlers. The future web-config UI adds route arms to
  the *same* router (its static assets embedded via `include_bytes!`) —
  no second server, no framework.
- **Provider closure**, exactly like Wyoming's `SttProvider`: the
  daemon hands the server `Arc<dyn Fn() -> Option<Arc<dyn Assistant>>>`
  so a config hot-reload swaps the model without restarting the
  listener.
- **Streaming.** `reply_stream` yields `TokenDelta`s. A per-format
  encoder wraps the stream into a `hyper` streaming body: SSE
  (`data: {chunk}\n\n` … `data: [DONE]\n\n`) for OpenAI, NDJSON (one
  JSON object per line) for Ollama. Non-stream requests buffer to a
  single JSON response.

## Wire contracts

### OpenAI-compatible

- `GET /v1/models` → `{ "object": "list", "data": [ { "id": "<model>",
  "object": "model", "owned_by": "fono" } ] }`.
- `POST /v1/chat/completions`:
  - Request subset: `model`, `messages[] {role, content}`, `stream`,
    `max_tokens`, `temperature` (accepted, mapped where meaningful).
  - Map: system message → `ctx.system_prompt`; all but the last
    user/assistant messages → `ctx.history` (`ChatTurn` via
    `ChatRole`); last user message → `user_text`; `max_tokens` →
    `ctx.max_new_tokens`.
  - `stream:false` → one `chat.completion` object.
  - `stream:true` → SSE of `chat.completion.chunk` with
    `choices[0].delta.content`, terminated by `data: [DONE]`.

### Ollama-native

- `GET /api/tags` → `{ "models": [ { "name": "<model>:latest",
  "model": "<model>", "size": <bytes>, "details": {...} } ] }`
  (HA and Ollama clients probe this to enumerate).
- `POST /api/chat`:
  - Request subset: `model`, `messages[] {role, content}`, `stream`
    (Ollama defaults `stream:true`), `options`, `tools` (Phase 2).
  - Same message→context mapping as above.
  - `stream:true` → NDJSON lines `{ "model", "created_at", "message":
    {"role":"assistant","content":"<delta>"}, "done": false }`,
    final line `{"done": true, ...}`.
  - `stream:false` → single `{... "message": {...}, "done": true}`.
- (Optional) `GET /api/version`, `POST /api/show` — trivial stubs some
  clients call on connect; add only if a target client requires them.

### Tool calling (Phase 2)

- OpenAI: accept `tools`/`tool_choice`; surface `TokenDelta` carrying
  `ToolEvent::Called(ToolCall)` as `choices[].delta.tool_calls`.
- Ollama: `message.tool_calls[]`.
- HA supplies its device tools; the model emits tool-call JSON; **HA
  executes** them. Fono never touches the entities. This reuses the
  existing `ToolEvent`/`ToolCall` history plumbing.

## Configuration

Add to `crates/fono-core/src/config.rs` `struct Server` (`:1236`):

```toml
[server.llm]
enabled = false          # master switch (off by default)
bind = "127.0.0.1"       # loopback default; "0.0.0.0" for LAN
port = 11434             # Ollama's default port ⇒ drop-in for HA/clients
auth_token_ref = ""      # optional pre-shared bearer, resolved via secrets/env
```

`struct ServerLlm` mirrors `ServerWyoming` (`:1252-1279`): `#[serde(default,
deny_unknown_fields)]`, `Default` with the values above. Port `11434`
is Ollama's default so existing client configs work unchanged; document
the STT/Wyoming port (`10300`) is separate.

## Tasks

### Phase 1 — chat-only MVP (OpenAI + Ollama)

- [x] **1.1** Enable `hyper` `server` + `http1` features in the
  workspace `Cargo.toml`; add `hyper`, `hyper-util`, `http-body-util`,
  `bytes` as direct deps of `fono-net` (all already resolved — confirm
  `cargo tree -p fono -i hyper` shows no new crate). Add `fono-assistant`
  as a `fono-net` dependency (net-zero; already in graph). Update
  `deny.toml` only if `cargo deny` flags a newly-*direct* edge (no new
  crate ⇒ expected no-op).
- [x] **1.2** `crates/fono-net/src/llm_server/mod.rs`: `LlmServer`,
  `LlmServerConfig` (bind/port/auth/model-name/server-version),
  `AssistantProvider = Arc<dyn Fn() -> Option<Arc<dyn Assistant>> + Send +
  Sync>`, `start()` → `LlmServerHandle`. Mirror `WyomingServer`'s accept
  loop, idle timeout, and loopback-only guard.
- [x] **1.3** `route(method, path)` dispatch (in `mod.rs`): hand-rolled
  dispatch + `404`/`405`/`400` fallbacks + optional bearer-token check.
- [x] **1.4** `llm_server/openai.rs`: request/response structs
  (reuse/mirror `openai_compat_chat.rs` shapes), the message→
  `AssistantContext` mapping, `GET /v1/models`, `POST /v1/chat/completions`
  (stream + non-stream), SSE encoder.
- [x] **1.5** `llm_server/ollama.rs`: `GET /api/tags`,
  `POST /api/chat` (stream + non-stream), NDJSON encoder. Shares the
  message-mapping + reply-driver from a common `chat.rs`.
- [x] **1.6** `crates/fono/src/session.rs`: add public
  `assistant_snapshot(&self) -> Option<Arc<dyn Assistant>>` alongside
  `stt_snapshot`/`tts_snapshot` (thin wrapper over `current_assistant`).
- [x] **1.7** `crates/fono/src/daemon.rs`: `spawn_llm_server_if_enabled`
  mirroring `spawn_wyoming_server_if_enabled` (`:3157`); wire the
  provider closure to `orch.assistant_snapshot()`; log one INFO line on
  bind (bind/port/model), warn-and-continue on bind failure.
- [x] **1.8** Config: `ServerLlm` struct + `Server.llm` field + defaults
  + round-trip serde test.
- [x] **1.9** mDNS: advertise the LLM service (e.g. add an `llm` cap tag
  / `_ollama._tcp`-style record) when `[server.llm].enabled`, mirroring
  the automatic advertising for enabled `[server.*]` blocks.
- [x] **1.10** Discoverability: `fono doctor` line ("LLM server:
  <bind>:<port> serving <model>" + loopback/LAN note) and tray label,
  parity with the Wyoming reporting.

### Phase 2 — tool calling (gated on the HA-control decision)

- [ ] **2.1** OpenAI `tools`/`tool_choice` in; `delta.tool_calls` out
  from `ToolEvent::Called`.
- [ ] **2.2** Ollama `tools` in; `message.tool_calls` out.
- [ ] **2.3** HA integration doc: point HA's Ollama conversation agent
  at `http://<host>:11434`, expose entities, verify a light-control turn.

## Testing

- Offline unit tests (no model, no socket) for both wire encoders:
  message→context mapping (system/history/last-user split), SSE framing
  + `[DONE]`, NDJSON framing + terminal `done:true`, `/v1/models` +
  `/api/tags` shapes, error responses (400 bad JSON, 404, 405, 401 with
  auth on). These mirror the offline posture of the existing
  STT/TTS/realtime wire tests.
- Round-trip integration test (feature-free, mock `Arc<dyn Assistant>`
  yielding scripted deltas over an ephemeral `port: 0`) asserting a real
  `reqwest` client gets the expected streamed bytes for both formats —
  mirror `crates/fono-net/tests/wyoming_server_round_trip.rs`.
- Dogfood: point Fono's *own* `openai_compat_chat` client at the local
  server (loopback) and confirm a full turn round-trips (client↔server
  symmetry check).

## Binary size

- Inference engine / model mgmt / streaming / `serde`: **0** (present).
- `hyper` `server`+`http1` code paths: **~0.1–0.3 MiB**, no new crates.
- Second (Ollama) wire format atop OpenAI: **low tens of KB**.
- **Projected total ≈ 0.2–0.3 MiB**, inside the headroom under the
  28 MiB `cpu` budget (current `release-slim` `cpu` ≈ 26.60 MiB per
  `docs/status.md`). **Run `./tests/check.sh --size-budget` before
  pushing**; if it eats more than expected, record the real number in
  `docs/binary-size.md` and, only with sign-off, reconcile the `cpu`
  row in `ci.yml` + ADR 0022 (hard cap ≤ 28 MiB).

## Security

- Default `bind = 127.0.0.1`; `loopback_only` flag derived exactly like
  Wyoming (`daemon.rs:3174`). LAN exposure is explicit.
- Optional bearer token via `auth_token_ref` (resolved through
  secrets/env), enforced in the router before dispatch. Document that,
  like Wyoming v1, the plaintext HTTP transport offers no confidentiality
  — a token gates access, not eavesdropping; TLS/reverse-proxy is the
  user's responsibility for off-LAN use.

## Risks

1. **Wire drift.** Vendors evolve the JSON (cf. the deprecated
   `realtimeInput.mediaChunks` incident). Mitigation: keep the accepted
   subset minimal, `serde(default)` on request structs for
   forward-compat, and pin the emitted shapes with offline tests.
2. **HA expects Ollama quirks** (`:latest` tag suffix, `/api/tags`
   `details`, sometimes `/api/version`/`/api/show`). Mitigation: shape
   `/api/tags` after a real Ollama response and add the stub endpoints a
   target client actually calls — verify against a live HA instance in
   Phase 2.
3. **Concurrency.** The embedded llama backend serialises on one model
   (`Arc<Mutex<…>>`); simultaneous HTTP turns queue. Mitigation:
   document single-stream expectation for the MVP; a bounded request
   queue / 503-on-busy is a later refinement.
4. **`fono-net → fono-assistant` edge** enlarges `fono-net`'s compile
   graph. Net-zero on binary (crate already linked) but note it in the
   crate's `Cargo.toml` comment.

## ADR

Land an ADR (next number in `docs/decisions/`, ~0036) recording: the
hyper-not-axum choice (with the "already in graph / zero new crates"
evidence), the dual OpenAI+Ollama surface, port `11434`, loopback
default, and the "same listener will host the future web-config UI"
intent. Update `docs/providers.md` (a "Fono as a local LLM server"
section), `docs/status.md` at session end, and `ROADMAP.md` (move the
item to Shipped at release time per the release checklist).

## Verification gate (per AGENTS.md, before commit/push)

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace --tests --lib`
4. `./tests/check.sh --size-budget` (before any push/tag)

Commit signed off (`-s`); no `Co-authored-by` trailer; do not push until
the maintainer says so.
