# Local LLM server — cloud pass-through proxy (full-fidelity fast lane)

## Objective

Extend the local LLM server (see
`plans/2026-07-01-local-llm-openai-ollama-server-v1.md`, shipped) with an
optional **pass-through proxy** mode: when the served assistant backend
is an **OpenAI-compatible cloud** provider, forward the client's HTTP
request straight to the upstream provider (injecting the stored key)
and stream the response back verbatim — instead of adapting through the
`Assistant` trait.

This gives, for cloud backends, **full wire fidelity for free**: every
model the provider offers, tool/function-calling, vision, JSON mode,
logprobs, and every request parameter pass through untouched, with no
per-feature adapter to maintain. The biggest concrete payoff is that
**cloud tool-calling works immediately** (the Home Assistant
"Direction B" device-control path) without waiting on `Assistant`-trait
tool plumbing.

The proxy is a *fast lane layered on top of* the existing adapter, not a
replacement. The adapter remains the universal floor for everything the
proxy cannot reach (local llama.cpp, Anthropic's non-OpenAI Messages
API, realtime, and the Ollama-native surface).

## Pinned decisions (settled in chat, 2026-07-01)

| Decision | Choice | Rationale |
|---|---|---|
| Shape | **Hybrid** — proxy when possible, adapt otherwise. | Proxy gives full fidelity but *cannot* serve local/realtime/Anthropic/Ollama-native. The adapter is the lowest-common-denominator floor that always works; the proxy is a high-fidelity fast lane for the cloud-OpenAI case. |
| When proxy applies (Phase 1) | Client hits the **OpenAI surface** (`/v1/*`) **and** the resolved server backend is an **OpenAI-compat cloud** (openai, gemini, groq, cerebras, openrouter). | These already target a `/chat/completions` URL Fono knows (`crates/fono-assistant/src/openai_compat_chat.rs:112-135`). Anthropic (Messages API, `crates/fono-assistant/src/anthropic_chat.rs:21`), embedded llama.cpp, and realtime are **not** proxyable → adapter. |
| Realtime primary | Proxy still applies — it resolves through the **flash-lite fallback** we already built. | `server_assistant_model_name` already yields `gemini-flash-lite-latest` for a Gemini-Live primary (`crates/fono-assistant/src/factory.rs:329-354`); that model lives on Gemini's OpenAI-compat endpoint, so we proxy to it and never build a local client. |
| Proxy vs adapt | **Automatic, no knob.** OpenAI-compat cloud backend → proxy; everything else → adapt. | "Adapt" only adds Fono's own persona/history shaping, which is wrong for a server whose clients supply their own prompt/history. So there is no real use case for forcing adapt on a proxyable backend. An escape hatch can be added later if proxying ever misbehaves (YAGNI). |
| Model selection | **Honour the client's requested `model`**; default to the resolved server model when the client omits it. | This is what "expose all the models" means; no knob. Cost/exposure is bounded by the loopback default + optional bearer token, not by silently rewriting the model — pinning would contradict the goal and surprise clients. |
| Ollama-native surface | Phase 2 **translate-proxy** (Ollama↔OpenAI), never pure pass-through. | Upstream clouds don't speak Ollama-native; the HA path (`/api/chat` + `tools`) must be translated. Phase 1 keeps the existing adapter for the Ollama surface. |
| Outbound HTTP client | **`reqwest`** (already in the graph). | Used by `fono-assistant`/`fono-stt` today; adding it as a `fono-net` edge is net-zero on binary size (AGENTS.md: already-present crate ⇒ no flag). |
| Upstream URL source of truth | Lift the per-provider `/chat/completions` endpoint constants into one `fono-assistant` lookup. | Today they're inline in the `OpenAiCompatChat::{openai,gemini,groq,cerebras,openrouter}` constructors (`openai_compat_chat.rs:112-135`); the proxy needs the same URLs, so centralise them. |

## Non-goals (this plan)

- Anthropic proxy (its Messages API is not OpenAI-shaped; stays adapter).
  A future translate-proxy is possible but out of scope.
- Proxying the **local** backends (embedded llama.cpp has no upstream;
  the Ollama *backend* is itself a server the adapter already streams
  from — no benefit to proxying it).
- Native (non-compat) provider shapes (e.g. Gemini `generateContent`).
  Proxying the OpenAI-compat layer is a deliberate subset; 100% native
  fidelity would force clients to speak provider-native, defeating the
  universal-API goal.
- Embeddings, model pulling, multi-model hot-swap (unchanged from the
  base plan).

## Background — what already ships (grounding)

- **Every OpenAI-compat cloud backend already targets a fixed
  `/chat/completions` URL**:
  - OpenAI `https://api.openai.com/v1/chat/completions`
    (`openai_compat_chat.rs:120`)
  - Gemini `https://generativelanguage.googleapis.com/v1beta/openai/chat/completions`
    — Google's OpenAI-compat layer (`openai_compat_chat.rs:131`)
  - Groq `:116`, Cerebras `:112`, OpenRouter `:124`.
  - `derive_models_endpoint` already maps `…/chat/completions` →
    `…/models` (`openai_compat_chat.rs:143`).
- **Key + model resolution already exists**: `resolve_cloud`
  (`crates/fono-assistant/src/factory.rs:49-121`) returns the resolved
  key and model from `[assistant.cloud]` + catalogue defaults;
  `default_cloud_model` (`:129-135`) is the per-provider text model.
- **Server-assistant resolver** (shipped last session): the server
  already knows *which* assistant/model it serves via
  `SessionOrchestrator::server_assistant_snapshot()` and
  `server_assistant_model_name(...)` /
  `build_server_assistant_override(...)`
  (`crates/fono-assistant/src/factory.rs:279-354`). The proxy plugs into
  the *same* resolution, choosing a URL+key instead of building a client.
- **The LLM server** (`crates/fono-net/src/llm_server/`) already has the
  OpenAI (`openai.rs`) and Ollama (`ollama.rs`) surfaces, the router
  (`mod.rs`), and the provider-closure hot-reload wired from
  `spawn_llm_server_if_enabled` (`crates/fono/src/daemon.rs`).
- **`reqwest` and `hyper` (client + server)** are already in the graph —
  no new crates for outbound forwarding.

## Architecture

Introduce a routing decision the daemon computes per request (hot-reload
friendly, exactly like today's provider closure):

```rust
// fono-net::llm_server
enum ServeTarget {
    /// Existing path: drive the Assistant trait, encode to the wire.
    Adapt(Arc<dyn Assistant>),
    /// New: forward verbatim to an OpenAI-compat upstream.
    Proxy(CloudUpstream),
}
struct CloudUpstream {
    chat_url:   String,          // provider /chat/completions
    models_url: Option<String>,  // provider /models (for /v1/models passthrough)
    api_key:    String,          // injected as Authorization: Bearer
    model:      String,          // the pinned model id
    allow_model_override: bool,  // honour client-chosen model when true
}
type TargetProvider =
    Arc<dyn Fn() -> Option<ServeTarget> + Send + Sync>;
```

- **Daemon resolves the target** from config on each call: try to build a
  `CloudUpstream` from the server-assistant config (proxyable cloud
  backend, `proxy != off`); on success → `Proxy`, else →
  `Adapt(server_assistant_snapshot())`. This reuses `resolve_cloud`'s
  key/model logic and the new centralised endpoint lookup.
- **OpenAI-surface handlers branch on the target**:
  - `Adapt` → today's behaviour unchanged.
  - `Proxy` → rewrite the incoming body (`model` pinned unless
    `allow_model_override`), set `Authorization`, `reqwest`-POST to
    `chat_url`, and **stream the upstream response body straight through**
    (SSE bytes pass verbatim; non-stream JSON relayed). Relay the
    upstream status code and content-type.
- **Ollama-surface handlers**: Phase 1 always `Adapt` (unchanged). Phase
  2 adds a translate-proxy arm.

```
                 ┌───────────────────────────────────────────────┐
 client ────────▶│ fono-net::llm_server  (one hyper listener)     │
                 │  OpenAI /v1/*      Ollama /api/*                │
                 │      │                  │                       │
                 │      ▼                  ▼                       │
                 │  ServeTarget?      (P1: Adapt)                  │
                 │   ├─ Proxy ─────▶ reqwest → provider upstream   │──▶ OpenAI / Gemini-compat /
                 │   └─ Adapt ─────▶ Assistant::reply_stream       │    Groq / Cerebras / OpenRouter
                 └───────────────────────────────────────────────┘
```

## Configuration

`struct ServerLlm` (`crates/fono-core/src/config.rs:1299`) is **unchanged** —
the existing block already carries everything the proxy needs:

```toml
[server.llm]
enabled = false
bind = "127.0.0.1"
port = 11434
model = ""                 # (existing) explicit staged/text model override,
                           #  used as the default when the client omits `model`
auth_token_ref = ""
```

**No new fields.** Pass-through is automatic: when the served backend is
an OpenAI-compat cloud, requests are proxied to it; otherwise the
existing adapter serves them. The client's requested `model` is honoured,
falling back to the resolved server model (the existing `model` override,
or the catalogue/realtime-fallback default) when omitted. `ServerLlm`
keeps its current shape and `#[serde(deny_unknown_fields)]`.

## Tasks

### Phase 1 — OpenAI-surface pass-through (auto mode, pinned model)

- [x] **1.1** `fono-assistant`: lift the per-provider `/chat/completions`
  endpoint strings out of the `OpenAiCompatChat` constructors into a
  single `pub fn chat_endpoint(backend: &AssistantBackend) ->
  Option<&'static str>` (returns `None` for anthropic/ollama/none), and
  refactor the constructors to consume it (no behaviour change). One
  source of truth for the proxy + client.
- [x] **1.2** `fono-assistant`: add
  `pub fn cloud_chat_upstream(cfg: &AssistantCfg, server_model_override:
  Option<&str>, secrets: &Secrets) -> Result<Option<CloudUpstream>>`.
  Returns `Some` only for OpenAI-compat cloud backends (including the
  realtime→flash-lite fallback, which resolves to Gemini). Reuses
  `resolve_cloud` for key+model and `chat_endpoint`/`derive_models_endpoint`
  for URLs. `None` for anthropic/local/ollama/none. Unit-test the mapping
  (gemini-live primary → gemini flash-lite upstream; openai → openai URL;
  anthropic → None; explicit `[server.llm].model` override honoured).
- [x] **1.3** `fono-net`: add `reqwest` (already in graph) as a direct
  dep with the minimal features it already resolves with; add
  `fono-net::llm_server::proxy` with `CloudUpstream`, `ServeTarget`, and
  an async `forward(req_body, upstream, stream) -> hyper::Response` that
  POSTs via `reqwest` and streams the response body back. Relay status +
  content-type; on upstream/network error emit a `502`-style JSON error
  in the client's format.
- [x] **1.4** `fono-net`: (implemented as a parallel `UpstreamProvider`
  closure alongside the existing `AssistantProvider`, rather than a
  single `ServeTarget`-returning closure — simpler and non-breaking; the
  OpenAI handlers check the upstream first, else adapt.) change the
  provider closure type from
  "assistant snapshot" to `TargetProvider` returning `ServeTarget`;
  branch the OpenAI handlers (`openai.rs`) on `Adapt` vs `Proxy`. Forward
  the client's `model` unchanged; substitute `upstream.model` only when
  the client omits/blanks it.
- [x] **1.5** `fono-net`: `GET /v1/models` in proxy mode proxies the
  upstream `/models` (via `models_url`) so clients discover the full
  provider catalogue; fall back to a single-model list (the resolved
  default) if the upstream call fails.
- [x] **1.6** `crates/fono/src/daemon.rs`: build the `TargetProvider` in
  `spawn_llm_server_if_enabled` — resolve `CloudUpstream` via
  `cloud_chat_upstream`; `Some` → `Proxy`, else →
  `Adapt(server_assistant_snapshot())`. Log one INFO line stating
  whether it proxies (upstream host + default model) or adapts (served
  model) — never the key.
- [x] **1.7** Diagnostics: `fono doctor` LLM line reflects the effective
  path (`proxying → api.openai.com (default gpt-…)` vs `serving <model>`
  for adapt). Tray enable-notification updated to match. (No new config
  fields — `ServerLlm` is unchanged.)

### Phase 2 — model passthrough + Ollama translate-proxy (HA cloud tools)

- [ ] **2.1** *(Optional, deferred until a real need appears.)* A
  `model_allowlist` on `[server.llm]` to bound which models an exposed
  instance will forward — only if someone runs a shared `0.0.0.0`
  instance and wants cost guardrails. Not built speculatively.
- [ ] **2.2** Ollama-surface translate-proxy: `/api/chat` translates the
  Ollama request (incl. `tools`) → OpenAI shape, forwards via the proxy,
  and translates the streamed OpenAI response (incl. `tool_calls`) back
  to Ollama NDJSON. `/api/tags` lists the pinned model (or upstream
  models under passthrough). This unlocks **Home Assistant device
  control against a cloud model** without touching the `Assistant` trait.
- [ ] **2.3** HA end-to-end doc + manual verification: point HA's Ollama
  conversation agent at Fono, expose entities, confirm a light-control
  turn round-trips through the cloud model's tool calls.

## Testing

- **Offline unit tests** (no socket, no network):
  - `chat_endpoint` / `cloud_chat_upstream` mapping (all proxyable
    providers, anthropic/local/ollama → None, realtime→flash-lite,
    override precedence).
  - Request rewrite: `model` pinned vs passthrough; `Authorization`
    injected; body otherwise untouched (tools/vision/params preserved).
- **Round-trip integration test** (mirror
  `crates/fono-net/tests/llm_server_round_trip.rs`): stand up a **mock
  upstream** (a second local hyper server returning scripted SSE), point
  a `CloudUpstream` at it, and assert a `reqwest` client hitting Fono's
  `/v1/chat/completions` gets the upstream bytes verbatim (stream +
  non-stream), that `model` is pinned/passed per policy, and that an
  unknown path/malformed body/upstream-500 produce the right errors.
- **Adapter regression**: existing `Adapt`-path tests stay green with
  `proxy = "off"`.

## Binary size

- `reqwest`, `hyper` client+server, `serde`: **already present → 0**.
- Proxy module (request rewrite + stream relay) + config enum: **low tens
  of KB**.
- **Projected ≈ 0.0–0.1 MiB.** Run `./tests/check.sh --size-budget`
  before pushing; current `release-slim` `cpu` ≈ 21.79 MiB against the
  25 MiB gate (per `docs/status.md`) — ample headroom.

## Security

- Exposure is bounded by the two gates that matter: the `bind =
  127.0.0.1` default and the optional bearer token. Because the server
  honours the client's requested model, an instance exposed on `0.0.0.0`
  **without** a bearer token is an open relay to the user's cloud
  account — document this loudly; the mitigation is the loopback default
  + token, not model pinning. A `model_allowlist` (Phase 2.1) is the
  escape hatch if a shared deployment needs cost guardrails.
- The upstream key never leaves the daemon: clients authenticate to Fono
  (optional `auth_token_ref`); Fono injects the provider key on the
  outbound leg. Never log the key; log only host + model.
- `bind = 127.0.0.1` default unchanged; LAN/off-box exposure remains the
  user's explicit opt-in with the same "plaintext HTTP, use a
  reverse-proxy for TLS" caveat as Wyoming.
- Proxy mode makes Fono an authenticated egress to the provider — note in
  docs that enabling it on `0.0.0.0` without the bearer token turns the
  box into an open relay to the user's account.

## Risks

1. **Silent cost / open relay.** Mitigated by the pinned-model default,
   the bearer token, loopback default, and explicit `doctor`/tray
   reporting of the proxy target.
2. **Upstream wire drift / new params.** Pass-through is *more* robust
   here than adapting (unknown fields flow through), but our request
   rewrite must touch only `model`/auth and leave the rest verbatim —
   covered by the "body otherwise untouched" test.
3. **Streaming back-pressure / disconnects.** Relay must propagate client
   cancellation to the upstream request (drop the `reqwest` stream on
   client hang-up) to avoid dangling upstream calls — test with an
   early-abort client.
4. **Provider OpenAI-compat gaps** (esp. Gemini): some native features
   aren't exposed via the compat endpoint. Documented as a known ceiling;
   users needing native fidelity point clients directly at the provider.
5. **`fono-net → reqwest` edge.** Net-zero on binary (already linked);
   note it in `Cargo.toml`.

## ADR

Add **decision 9** to `docs/decisions/0036-local-llm-server-openai-ollama.md`
(the existing ADR), recording: the hybrid proxy-vs-adapt model, when
proxy applies (OpenAI surface + OpenAI-compat cloud), the realtime→
flash-lite proxy path, that proxy-vs-adapt is automatic (no mode knob)
and the client's `model` is honoured (no pinning knob) with the resolved
server model as the omitted-field default, the Ollama translate-proxy
deferral to Phase 2, and reuse of
`reqwest`/`resolve_cloud`/centralised `chat_endpoint`. Update
`docs/configuration.md` (the "Serve local inference over HTTP" section:
note the automatic cloud pass-through and that the client's `model` is
honoured), `docs/home-assistant.md` (cloud
tool-calling via the Ollama translate-proxy once Phase 2 lands), and
`docs/status.md` at session end.

## Verification gate (per AGENTS.md, before commit/push)

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace --tests --lib`
4. `./tests/check.sh --size-budget` (before any push/tag)

Commit signed off (`-s`); no `Co-authored-by` trailer; do not push until
the maintainer says so.
