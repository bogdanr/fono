# ADR 0024 — Assistant multimodal and web-search extras

## Status

Accepted 2026-05-13.

## Context

The voice assistant added in earlier phases is a chat-only client per
provider — text in, text-streamed out, no awareness of images on the
screen and no ability to look anything up on the web. Several of the
providers Fono already speaks to (OpenAI, Anthropic, Google Gemini,
Groq) ship multimodal sibling models and / or a native web-search
tool that lights up with a single field on the request body. The
wizard rework v2 (issues #9 / #11) is the natural place to expose
those two extras: the user already picks a primary provider, so we
can offer the toggles inline without inventing a separate menu.

We also want to avoid two anti-patterns:

1. **Always-on vision and search**, which would silently bump latency
   and cost on every assistant invocation even when the user just
   wants a quick "what time is it in Tokyo?" reply.
2. **Auto-routing by intent classification**, which costs an extra
   round-trip per turn and introduces a real privacy footgun
   (false-positive screen captures, accidental external lookups).

## Decision

- Extend the capability catalogue (`crates/fono-core/src/provider_catalog.rs`)
  with two per-assistant fields:
  - `multimodal_model: Option<&'static str>` — the provider's
    vision-capable sibling model (or `None` when the provider has no
    multimodal endpoint Fono is willing to default to).
  - `web_search: WebSearchSupport { None | NativeTool(&'static str)
    | Always }` — describing whether the provider exposes a
    server-side web-search tool, and under what tool id.
- Add two `[assistant]` config flags, both default `false`:
  - `prefer_vision: bool`
  - `prefer_web_search: bool`
- Wizard (`crates/fono/src/wizard.rs::configure_assistant`) surfaces
  these as a single optional `MultiSelect` rendered **only** when the
  chosen primary provider's catalogue entry advertises at least one
  of the two. The collapsed-Confirm fast path skips the MultiSelect
  entirely (users on that path opted into defaults).
- The assistant factory (`crates/fono-assistant/src/factory.rs`)
  consults the catalogue at startup:
  - If `prefer_vision && multimodal_model.is_some()`, use the
    multimodal variant; otherwise log a single `warn!` and stay on
    the text model.
  - If `prefer_web_search && web_search == NativeTool(tool_id)`,
    pass `tool_id` to the per-provider chat client via
    `with_web_search`. The client injects the appropriate `tools`
    field on every request and emits a one-line `info!` per
    invocation. `WebSearchSupport::None` and `Always` are no-ops in
    this phase.
- Per-provider tool payload shapes:
  - **OpenAI** (chat/completions): `tools: [{"type":
    "web_search_preview"}]`. Documented inline at
    `openai_compat_chat.rs::with_web_search` with a link to the
    Responses-API tool reference; the Responses-API migration is
    deferred.
  - **Anthropic** (Messages API): `tools: [{"type":
    "web_search_20250305", "name": "web_search", "max_uses": 3}]`.
  - **Gemini**: `tools: [{"google_search": {}}]` — declared in the
    catalogue but not wired in code because there's no Gemini chat
    client yet (deferred).
- Capability badges in the wizard's primary picker (Vision, Search)
  are derived from runtime state — `multimodal_model.is_some()` and
  `web_search != None` — not from the catalogue's static `badges`
  array. A single catalogue edit keeps the label, the assistant
  builder, and the tool-injection logic in lockstep.

## Consequences

- No new runtime dependencies.
- Old configs without `[assistant].prefer_vision` /
  `[assistant].prefer_web_search` load unchanged — both fields carry
  `#[serde(default)]` and default to `false`.
- Vision input is currently *configurational only*: the model is
  swapped, but Fono does not yet capture or attach screenshots /
  image bytes to user turns. Screen-capture plumbing (modifier
  hotkey, image grab, base64 packaging) is deferred.
- Intent-detection auto-routing for vision and search is **not**
  shipped (see Context above). Toggles are explicit opt-ins.
- Local web-search plumbing for providers without a native tool
  (Groq, Cerebras, OpenRouter, Ollama) is deferred. Their catalogue
  entries continue to advertise `WebSearchSupport::None` and the
  toggle is a no-op when chosen as primary.
- Gemini's catalogue entry carries `multimodal_model` and
  `web_search` so the wizard can surface the badges, but the
  assistant factory still rejects Gemini (no chat client).
  Lighting up Gemini chat is a follow-up.

## Deferred follow-ups

- Screen-capture for vision input (modifier hotkey + image grab).
- Gemini assistant chat client + multimodal request wiring.
- Migration to OpenAI's Responses API to consume `web_search_preview`
  natively rather than relying on chat/completions silently accepting
  the descriptor.
- Local web-search tool plumbing for non-native providers.
- Intent-detection auto-routing — gated on a working screen-capture
  privacy story first.
