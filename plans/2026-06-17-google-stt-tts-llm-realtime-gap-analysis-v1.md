# Google Support (STT · TTS · LLM · Realtime+Tools) — Gap Analysis & Updated Roadmap

## Objective

Assess whether Fono's existing plans cover full Google support across the four
capabilities the user named — **STT, TTS, LLM, and a realtime assistant with
tool support** — identify the gaps, and define an updated, correctly-sequenced
plan that closes them. This is a planning/assessment deliverable only; no code
is changed here.

## Critical framing: "Google" is two different API surfaces

Fono already encodes this split in the catalogue, but no decision record ties
the two together into one coherent product story. Any plan must pick a lane per
capability:

- **Gemini API (Google AI Studio / Generative Language API)** — simple
  `GEMINI_API_KEY`, REST + `wss://generativelanguage.googleapis.com`. Covers
  **LLM/chat**, **vision**, **Gemini Live realtime S2S with function calling**,
  and (newer) **Gemini native TTS** and **audio transcription (STT)**.
  Catalogue id: `gemini`.
- **Google Cloud Speech (Chirp 3 / Cloud TTS)** — service-account OAuth
  (`GOOGLE_APPLICATION_CREDENTIALS`), `speech.googleapis.com` /
  `texttospeech.googleapis.com`. Best-in-class **STT (Chirp 3)** and
  **TTS (Chirp 3 HD voices)** with language hints, phrase boost, diarisation.
  Catalogue id: `google`.

The single biggest open decision is **whether STT/TTS go through Cloud Speech
(Chirp, service-account auth) or through the Gemini API (one key, slightly less
specialised)**. The whole plan set currently assumes Cloud Speech for STT/TTS
(the Chirp plan) and Gemini for LLM/realtime — i.e. **two auth mechanisms and
two catalogue entries**. That is defensible but must be made explicit, because
a "single GEMINI_API_KEY covers all four" path is materially simpler for users
and is not currently planned anywhere.

## Capability → plan → reality matrix

| Capability the user asked for | Existing plan | Plan status | Actually landed in code? | Gap |
|---|---|---|---|---|
| **STT** (Chirp 3 / Cloud Speech) | `plans/2026-05-14-google-chirp-stt-v1.md` | Draft, **unscheduled** | No — `google` is a stub (`model:"default"`, no `key_validation`) | Plan exists but is **stale** (pre-dates catalogue reshape, `key_validation`, english-only fallback, voice palette); needs OAuth crate + new `fono-net-google` crate |
| **STT** (Gemini multimodal audio) | none | — | No | **Unplanned** alternative path |
| **TTS** (Chirp 3 HD) | same Chirp plan | Draft, unscheduled | No (`tts: None` on `google`) | Same plan; needs `TtsEndpoint::Google` variant + voice palette integration |
| **TTS** (Gemini native 2.5 TTS) | none | — | No | **Unplanned** alternative path |
| **LLM** — polish (Gemini) | catalogued only | — | **Stub** — `fono-polish` returns "not yet implemented" | No real Gemini polish client |
| **LLM** — assistant chat (Gemini staged/HTTP) | mentioned-deferred in ADR 0025 & realtime v4 Task 1.2 | — | No — `Gemini` absent from `AssistantBackend` | **No dedicated plan** for a staged Gemini `generateContent` chat client (text + multimodal + `google_search`) |
| **Realtime assistant** (Gemini Live) | `plans/2026-05-25-realtime-end-to-end-assistant-v4.md` | Drafted, **not started** | No (`RealtimeAssistant` trait, `gemini_live.rs` absent) | Blocked on voice-actions; catalogue reshape (`ModelEntry`) not yet landed |
| **Tool support** (general dispatcher) | `plans/2026-05-22-voice-actions-via-mcp-v1.md` | Drafted, **not landed** | No — `fono-action` crate does not exist | Hard prerequisite for realtime-with-tools |

## Findings — where the plan set has gaps

1. **No staged Gemini LLM/chat client is planned anywhere.** The realtime v4
   plan only adds a *realtime* Gemini `ModelEntry` (Task 1.2) and a Gemini Live
   client (Task 4.3). ADR 0025 lists "Gemini chat client" as deferred with no
   owning plan. Result: a user who picks Gemini as primary and selects a text
   model would have **no working assistant**, and Gemini **polish is a stub**.
   This is the largest gap relative to the user's "LLM" request.

2. **The Chirp STT/TTS plan (2026-05-14) is stale.** It predates: the catalogue
   capability reshape (ADR 0025), the `key_validation` metadata
   (`plans/2026-06-13-...`), the english-only TTS fallback, the per-program
   voice palette (`voice_palette` / `voice_resolver`), and the cloud TTS voice
   palette baked per provider. Its field shapes (`SttDefaults`, `TtsDefaults`)
   and wizard flow references no longer match the current catalogue. It needs a
   refresh before it can be executed.

3. **The realtime v4 plan's own prerequisites are unmet.** Its Phase 1 catalogue
   reshape (`AssistantDefaults.text_model` → `models: &[ModelEntry]` with
   `transport`/`cost_tier`) **has not landed** (catalogue still uses
   `text_model`/`multimodal_model`). And it is explicitly **blocked on
   voice-actions-via-mcp**, which also has not landed (no `fono-action` crate).
   So "realtime assistant with tool support" sits behind a two-deep dependency
   chain that is currently entirely unbuilt.

4. **Auth strategy is undocumented.** No ADR reconciles `GEMINI_API_KEY` (Gemini
   surface, used by realtime Gemini Live) vs `GOOGLE_APPLICATION_CREDENTIALS`
   service-account (Cloud Speech surface, used by Chirp). The two `google`/
   `gemini` catalogue entries imply the split but nothing records the decision,
   the secrets.toml shape for a service-account JSON path, or how `fono doctor`
   validates each.

5. **No single sequenced roadmap ties the four capabilities together.** The four
   asks live across three plans (Chirp, voice-actions, realtime) plus an unwritten
   Gemini-chat plan, with cross-blocking dependencies that aren't visible in any
   one place.

6. **Newer Gemini capabilities are unplanned.** Gemini 2.5 native TTS and Gemini
   audio transcription could let a single `GEMINI_API_KEY` cover all four asks.
   If the user prefers the one-key path, none of the existing plans deliver it.

## Recommended strategy (assumptions, stated explicitly)

Assumption A — **Lane split kept**: STT/TTS via Google **Cloud Speech (Chirp)**
for quality and feature depth; LLM + realtime via the **Gemini API**. Rationale:
matches the existing two-entry catalogue and the realtime plan; Chirp is the
distinctive STT/TTS the original plan was written for. (If the user instead wants
the simplest one-key experience, switch to Assumption A′ — Gemini API for all
four — which collapses auth to a single key but needs two net-new unplanned
clients for Gemini STT and Gemini TTS; see Alternative Approaches.)

Assumption B — **LLM means both polish and assistant chat.** A real Gemini
client is required for staged assistant chat; Gemini polish stub should either be
implemented on the same client or formally dropped.

Assumption C — **"Realtime with tool support" requires the tool dispatcher
first.** Voice-actions-via-mcp is a non-negotiable prerequisite, per realtime v4's
own dependency statement.

## Implementation Plan

### Section A — Decisions & plan hygiene (do first; unblocks the rest)

- [ ] Task A1. **Write an ADR "Google integration surfaces & auth"** recording
      the lane split (Cloud Speech vs Gemini API), the two key shapes
      (`GOOGLE_APPLICATION_CREDENTIALS` service-account path vs `GEMINI_API_KEY`),
      and how each is validated. Rationale: removes the single biggest ambiguity
      blocking every downstream task.
- [ ] Task A2. **Refresh `plans/2026-05-14-google-chirp-stt-v1.md` to v2**
      against the current catalogue: align to ADR 0025 field shapes, add a
      `key_validation` descriptor for the `google` entry, integrate with the
      english-only fallback and the cloud **voice palette** (per-provider gendered
      positional voices), and reconcile the service-account secret shape with the
      current `secrets.toml` model. Rationale: the existing plan cannot be executed
      as-is because the catalogue moved underneath it.
- [ ] Task A3. **Author a new plan "Gemini staged assistant + LLM client"**
      covering a `generateContent` streaming SSE chat client
      (`fono-assistant::gemini`), `AssistantBackend::Gemini` wiring, the
      `google_search` web-search tool, vision/multimodal input, and a decision on
      the **Gemini polish stub** (implement on the shared client, or drop the
      `PolishBackend::Gemini` arm). Rationale: this is the missing "LLM" plan; the
      realtime plan assumes it exists but no plan owns it.
- [ ] Task A4. **Add a single "Google roadmap" index** (section in
      `docs/status.md` or a short umbrella plan) that orders the four capabilities
      and their cross-dependencies, so the blocking chain is visible in one place.

### Section B — Prerequisite infrastructure (blocks realtime-with-tools)

- [ ] Task B1. **Land `voice-actions-via-mcp-v1`** (the `fono-action` crate:
      `Tool`/`ToolSpec`/`ToolCall`/`ToolResult`/`ToolRegistry`/`Dispatcher`, MCP
      stdio+SSE transports, `[assistant.tools]` config, confirmation skeleton).
      Rationale: realtime v4 is explicitly blocked on this; staged tool-calling is
      also a precondition for "tool support" on the LLM path.
- [ ] Task B2. **Land realtime v4 Phase 1 catalogue reshape** (`ModelEntry`,
      `Transport`, `RealtimeProfile`, `RealtimeProtocol`, `CostTier`,
      `Badge::Realtime`, and the five regression invariants), migrating all
      existing provider entries off `text_model`/`multimodal_model`. Rationale:
      independent of B1, can land in parallel; every later realtime task depends on
      it; touching every provider entry is safest as one gated change.

### Section C — Google STT + TTS (Cloud Speech / Chirp)

- [ ] Task C1. **Create `fono-net-google` crate** owning service-account JWT
      minting + OAuth token cache (e.g. `gcp_auth`/`yup-oauth2`, MIT/Apache-2,
      GPL-3-compatible). Update `deny.toml`. Rationale: shared auth used by both
      STT and TTS clients; keeps token logic in one place.
- [ ] Task C2. **`fono-stt::google` Chirp 3 batch `:recognize` client** + promote
      the `google` catalogue entry (`stt: chirp-3`, real `key_validation`, default
      model fix in `defaults.rs`). Rationale: the core STT ask.
- [ ] Task C3. **`fono-tts::google` Chirp 3 HD client** + `TtsEndpoint::Google`
      variant + per-locale voice palette entries. Rationale: the core TTS ask;
      must slot into the existing voice-palette + english-only-fallback machinery.
- [ ] Task C4. **Wizard + doctor + docs**: service-account JSON picker, Chirp
      locale picker, Chirp voice picker; `fono use stt google` / `fono use tts
      google`; `fono doctor` token-mint + reachability check; `docs/providers.md`
      Google section + free-tier note. Rationale: parity with the Groq/OpenAI flow.
- [ ] Task C5. **(Deferred sub-slice) Chirp streaming `:streamingRecognize`**
      behind a `google-streaming` (gRPC/tonic) feature flag. Rationale: keeps the
      default build slim; streaming dictation parity is a follow-on, not v1.

### Section D — Gemini LLM (staged) + polish

- [ ] Task D1. **`fono-assistant::gemini` staged chat client** (streaming
      `generateContent`), `AssistantBackend::Gemini` + providers.rs wiring, and the
      Gemini `ModelEntry` set in the reshaped catalogue (text + vision; mark the
      realtime entry too for Section E). Rationale: makes "pick Gemini as primary
      assistant" actually work in staged mode.
- [ ] Task D2. **Gemini `google_search` web-search tool + multimodal image
      input** on the staged client, matching the OpenAI/Anthropic tool round-trip
      pattern. Rationale: completes the "LLM with tools/vision" surface for Gemini.
- [ ] Task D3. **Resolve the Gemini polish stub**: either implement polish on the
      shared Gemini client or remove the `PolishBackend::Gemini` arm and its
      catalogue claim. Rationale: today it advertises a capability that errors at
      runtime.

### Section E — Realtime Gemini Live with tools (depends on B1, B2, D1)

- [ ] Task E1. **`RealtimeAssistant` trait + `RealtimeSession` handle** (realtime
      v4 Phase 3): session-handle shape so tool results flow back into the same
      WebSocket. Rationale: the load-bearing abstraction for tool-capable realtime.
- [ ] Task E2. **`fono-assistant::gemini_live` client** (realtime v4 Task 4.3):
      `wss://…BidiGenerateContent`, `setup` with `tools.functionDeclarations` from
      `&[ToolSpec]`, audio in/out + transcription + `toolCall`/`toolResponse`
      mapping to `RealtimeEvent`. Rationale: the realtime+tools ask for Google.
- [ ] Task E3. **Factory dispatch on `Transport` + orchestrator F8 short-circuit
      + barge-in/cancel** (realtime v4 Phases 4.4–5.5), reusing the **same
      `Dispatcher`** from Section B so tool policy is identical across staged and
      realtime. Rationale: parity is the whole point; no second policy surface.
- [ ] Task E4. **Wizard cost-labelled model picker, `fono doctor` mode/tool-count
      row, cost docs, tests** (realtime v4 Phases 6–7). Rationale: honest cost UX
      + regression coverage (tool round-trip, F7-untouched, TTFA harness).

## Verification Criteria

- An ADR exists that unambiguously states, per capability, which Google API
  surface and which credential Fono uses.
- The Chirp plan (v2) references only catalogue shapes that exist in the current
  tree (`key_validation`, voice palette, english-only flag).
- A Gemini staged assistant plan exists and is cross-linked from realtime v4 as
  its LLM prerequisite.
- The dependency chain (voice-actions → catalogue reshape → realtime; Gemini chat
  client → realtime model selection) is captured in one ordered index.
- For each shipped slice: `fono use {stt,tts,assistant} google/gemini` configures
  a working backend; `fono doctor` validates credentials and reachability;
  pre-commit gate (`cargo fmt --check`, `clippy -D warnings`, workspace tests)
  green; `cargo deny` clean for any new deps.
- Realtime tool calls dispatch through the **same** `fono-action::Dispatcher` as
  the staged path (single policy/confirmation surface), and F7 dictation is
  byte-identical regardless of assistant transport.

## Potential Risks and Mitigations

1. **Auth divergence confuses users (two key styles).**
   Mitigation: ADR A1 + a wizard that picks the right credential flow per chosen
   capability; `fono doctor` reports which surface/credential each Google backend
   uses.
2. **Deep blocking chain stalls the realtime ask.** Realtime-with-tools sits
   behind voice-actions + catalogue reshape + Gemini chat client.
   Mitigation: deliver value incrementally — Chirp STT/TTS (Section C) and Gemini
   staged LLM (Section D) ship independently and usefully before realtime (E).
3. **Stale plans executed verbatim cause rework.**
   Mitigation: Tasks A2/A3 refresh/author the plans against the current catalogue
   before any code lands.
4. **gRPC bloat for Chirp streaming.**
   Mitigation: REST batch first; streaming behind a feature flag (Task C5), per the
   original plan's ≤800 KB / ≤4-dep budget.
5. **Gemini polish stub keeps shipping as a runtime error.**
   Mitigation: Task D3 forces a decision (implement or remove) rather than leaving
   an advertised-but-broken capability.
6. **Realtime preview-API schema drift (Gemini Live).**
   Mitigation: per-client module quarantine; provider events never leak into the
   shared `RealtimeEvent` enum (realtime v4 risk #3).

## Alternative Approaches

1. **Assumption A′ — single Gemini API key for all four.** Use Gemini multimodal
   audio for STT, Gemini 2.5 native TTS, Gemini chat for LLM, Gemini Live for
   realtime. Trade-off: dramatically simpler auth (one key, no service account)
   and one catalogue entry, but needs two **net-new unplanned** clients (Gemini
   STT, Gemini TTS) and gives up Chirp's language-hint/diarisation/voice depth.
   Strong candidate if the user values simplicity over best-in-class speech.
2. **Ship Google STT/TTS only now; defer all LLM/realtime.** Smallest scope that
   still answers part of the ask; matches the one already-written (if stale) plan.
   Trade-off: leaves "LLM" and "realtime with tools" entirely unaddressed.
3. **Gemini LLM + realtime first; defer Chirp STT/TTS.** Prioritises the
   harder-to-replace assistant/realtime capabilities (no other provider gives
   Gemini Live) and leans on existing cloud STT/TTS providers meanwhile.
   Trade-off: requires the full B→D→E chain before any Google capability ships.
