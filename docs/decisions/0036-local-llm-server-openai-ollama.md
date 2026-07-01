# ADR 0036 — Local LLM server: OpenAI + Ollama API on raw hyper

- **Status:** Accepted
- **Date:** 2026-07-01
- **Supersedes:** none
- **Related:** [ADR 0030 — Fono as an MCP server for coding agents](0030-fono-as-mcp-server-for-coding-agents.md),
  [ADR 0004 — Default models](0004-default-models.md)
- **Plan:** [`plans/2026-07-01-local-llm-openai-ollama-server-v1.md`](../plans/2026-07-01-local-llm-openai-ollama-server-v1.md),
  [`plans/2026-07-01-local-llm-server-cloud-proxy-v1.md`](../plans/2026-07-01-local-llm-server-cloud-proxy-v1.md)

## Context

Fono already ships a complete local inference stack: embedded llama.cpp
generation, streaming, sampler/stop policy, GGUF download + cache, and a
multi-turn prompt-state cache. It is also *already a client* of the
OpenAI/Ollama `chat/completions` wire format (`openai_compat_chat.rs`).
Exposing that inference capability over a universal HTTP API turns Fono
into a drop-in local inference backend for editors, Open WebUI,
LangChain, the `llm` CLI, and — via the Ollama-native surface — Home
Assistant's Ollama conversation agent.

The question was not *whether we can* (the engine is done) but *how thin
a network shim* to wrap around it, and *which HTTP stack* to use given
that binary size is the project's top priority and everything compiles
into the one shipped binary (no cargo-feature variants are shipped).

Three consumers want a local HTTP listener: this LLM API, a future
web-config UI, and a potential Home Assistant endpoint. That breadth
initially argued for a full framework (axum). But:

- `hyper` / `hyper-util` / `http-body-util` / `bytes` are **already in
  the dependency graph** via `reqwest`'s client stack (verified with
  `cargo tree`). Enabling hyper's `server` + `http1` features adds
  **no new crate**.
- `axum` + `matchit` + `serde_urlencoded` + `sync_wrapper` are
  **net-new** crates (~0.4–0.7 MiB) for extractor/router ergonomics a
  ~6-route surface does not need.
- The codebase already hand-rolls a protocol dispatch loop for the
  Wyoming server; a `match` on `(method, path)` is house idiom.

## Decision

1. **Serve on raw `hyper 1.x`** — `service_fn` + a hand-rolled
   `route(method, path)` match. No axum. Enable hyper's `server` +
   `http1` features only. Static assets for the future web UI will be
   `include_bytes!`-embedded and served from a match arm (no `ServeDir`).
2. **Serve both wire formats from one listener:**
   - OpenAI-compatible: `GET /v1/models`, `POST /v1/chat/completions`
     (SSE stream + single JSON).
   - Ollama-native: `GET /api/tags`, `POST /api/chat` (NDJSON stream +
     single JSON), `GET /api/version`.
   Both map their `messages[]` onto the same `AssistantContext` and drive
   the one `Assistant::reply_stream`. The near-zero marginal cost of the
   second format buys universal reach (OpenAI) plus the Home Assistant
   path (Ollama-native).
3. **Default port `11434`** — Ollama's — so existing Ollama/OpenAI client
   configs and Home Assistant's Ollama integration point at Fono
   unchanged.
4. **Off by default; `127.0.0.1` bind; optional bearer token.** Config
   lives in `[server.llm]`, mirroring `[server.wyoming]`. Loopback-only
   is enforced defensively when bound to a loopback address. The
   plaintext HTTP transport gives no confidentiality; the token gates
   access, not eavesdropping.
5. **Reuse the daemon's active backend.** A provider closure
   (`AssistantProvider`) invokes `orchestrator.server_assistant_snapshot()`
   per request so `Reload`-driven backend swaps (`fono use assistant …`)
   are tracked without restarting the listener — the same pattern as the
   Wyoming STT/TTS providers.
6. **Advertise over mDNS as `_ollama._tcp`** when enabled, mirroring the
   automatic advertising of enabled `[server.*]` blocks.
7. **Tool/function-calling passthrough is deferred to Phase 2**, gated on
   whether the Home Assistant device-control story (HA emits tool calls,
   HA executes them) is a near-term target. The MVP is chat + stream +
   list.
8. **Realtime assistants fall back to a same-provider text sibling.**
   A *realtime* speech-to-speech backend (Gemini Live) has no text
   `reply_stream` and cannot answer a chat-completions request. Rather
   than refuse to start, the server serves the same provider's default
   staged **text** model (Gemini → the catalogue `text_model`,
   `gemini-flash-lite-latest`), reusing the same API key. This is built
   in the orchestrator (`build_server_assistant_override` +
   `server_assistant_extra` slot) and rebuilt on reload, so the user
   keeps Gemini Live for F8 voice *and* gets a fast, cheap, smart text
   model on the API with zero extra config. An optional
   `[server.llm].model` override pins a specific staged model, winning
   over both the primary assistant and the fallback. Rejected
   alternatives: reusing the `[polish]` cleanup model (wrong trait,
   typically a small model weak at chat/tool-use) and requiring manual
   configuration (not seamless — the server would stay dark until the
   user acted).
9. **Cloud backends are proxied verbatim; everything else is adapted
   (hybrid, automatic, no knob).** When the served backend is an
   **OpenAI-compatible cloud** provider (openai, gemini, groq, cerebras,
   openrouter), the OpenAI surface (`/v1/models`,
   `/v1/chat/completions`) forwards the client's request **straight to
   the upstream** (injecting the stored key) and streams the response
   back unchanged. This gives full wire fidelity for free — every model,
   tool/function-calling, vision, JSON mode, and request parameter pass
   through untouched, with no per-feature adapter to maintain — and is
   the cheapest path to cloud tool-calling (the Home Assistant
   device-control story). For everything the proxy cannot reach —
   embedded llama.cpp (no upstream), Anthropic (its own Messages API,
   not OpenAI-shaped), and the Ollama-native surface (clients speak
   Ollama, clouds speak OpenAI) — the `Assistant`-trait adapter remains
   the universal floor. The choice is **automatic**: proxyable cloud
   backend → proxy, else adapt; there is no mode knob, because "adapt"
   only adds Fono's persona/history shaping, which is wrong for a server
   whose clients supply their own prompt and history. A **realtime**
   primary (Gemini Live) still proxies — it resolves through the
   flash-lite fallback of decision 8, which lives on Gemini's
   OpenAI-compat endpoint, so no local client is built. The client's
   requested `model` is **honoured verbatim**; the resolved server model
   (the `[server.llm].model` override, catalogue default, or realtime
   fallback) is substituted only when the client omits it — no pinning
   knob, because that would contradict "expose all the models". `GET
   /v1/models` proxies the provider's `/models` catalogue so clients
   discover every model they can request. Implemented as a parallel
   `UpstreamProvider` closure alongside the existing `AssistantProvider`
   (simpler and non-breaking); the OpenAI handlers check the upstream
   first, else drive the adapter. The Ollama-surface translate-proxy
   (Ollama↔OpenAI, incl. `tools`) is deferred to Phase 2. Outbound
   forwarding uses `reqwest` (already in the graph → no new crate).
   Reuses `resolve_cloud`'s key/model resolution and a centralised
   `chat_endpoint(backend)` lookup for the per-provider URLs.

   **Security:** the upstream key never leaves the daemon (clients
   authenticate to Fono via the optional bearer token; Fono injects the
   provider key outbound). Because the client's model is honoured, an
   instance exposed on `0.0.0.0` **without** a bearer token is an open
   relay to the user's cloud account — the mitigation is the loopback
   default + token, not model pinning. A future `model_allowlist`
   (Phase 2.1) is the escape hatch for shared deployments.

## Consequences

- **Binary size:** engine / model management / streaming / serde are
  already present (0); hyper `server`+`http1` code paths add
  ~0.1–0.3 MiB with **no new crates**; the second wire format adds low
  tens of KB. Measured: the `release-slim` `cpu` ship artefact stays well
  inside the CI size budget.
- **Maintenance:** two wire contracts to keep correct as vendors drift.
  Mitigated by offline unit tests on both encoders plus a real
  `reqwest` round-trip integration test against a mock `Arc<dyn
  Assistant>`.
- **Migration path:** if the route count ever explodes (a large
  web-config API), migrating to axum later is incremental — axum is
  hyper underneath, so nothing built now is wasted.
- **No new default models.** This ADR exposes whatever `[assistant]`
  backend the user already configured; it does not change model
  defaults (ADR 0004 still governs).
