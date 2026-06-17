# Google = Gemini API (single key, free tier) — STT · TTS · LLM · Realtime+Tools

> **Supersedes v1** (`2026-06-17-google-stt-tts-llm-realtime-gap-analysis-v1.md`).
> User decision (2026-06-17): Google support must be **one API key with a free
> monthly quota**. That is the **Gemini API (Google AI Studio)** — *not* Google
> Cloud Speech. This collapses the v1 two-lane design (Chirp + service-account
> OAuth for speech, Gemini for LLM/realtime) into a **single-surface, single-key**
> plan. The Cloud Speech / Chirp / `fono-net-google` OAuth crate path from v1 is
> **dropped**.

## Objective

Deliver Google STT, TTS, LLM (polish + assistant chat), and a realtime assistant
with tool support — **all through the Gemini API on a single `GEMINI_API_KEY`**
with its free tier. Consolidate everything onto the existing `gemini` catalogue
entry; retire the unused Cloud-Speech `google` stub from this effort.

## Grounded facts (verified against ai.google.dev, 2026-06-17)

- **One key, free tier.** `GEMINI_API_KEY` from `aistudio.google.com/apikey`.
  Rate-limits page confirms a **Free** usage tier (active project / free trial,
  no billing) with per-model RPM/TPM/RPD caps; RPD resets midnight Pacific. Auth
  header `x-goog-api-key: <key>` (query `?key=` also works — the catalogue
  already validates Gemini via `KeyAuth::QueryParam("key")`).
- **Base**: `https://generativelanguage.googleapis.com/v1beta`.
- **LLM / chat / polish**: `models/<model>:generateContent` and
  `:streamGenerateContent`. Function calling + `google_search` tool + image
  (vision) input all supported on one key. Current default candidate:
  `gemini-2.5-flash` (the catalogued `gemini-1.5-flash` is stale).
- **STT**: audio understanding — send audio (inline base64 ≤ ~20 MB, or Files
  API) to a normal model via `generateContent` with a "transcribe" instruction.
  Prompt-driven ASR: **no per-segment confidence/logprobs** (same class as
  Cartesia `ink-whisper` / ElevenLabs Scribe, which Fono already accommodates).
  No native streaming ASR for F7 dictation (batch only; Live API input
  transcription covers the realtime path).
- **TTS**: `models/<tts-model>:generateContent` / `:streamGenerateContent` with
  `generationConfig.responseModalities:["AUDIO"]` +
  `speechConfig.voiceConfig.prebuiltVoiceConfig.voiceName`. Output is **raw PCM
  24 kHz mono 16-bit** (Fono's pipeline format). **30 prebuilt voices**
  (Zephyr/Puck/Charon/Kore/Aoede/Fenrir/Leda/…); **40+ languages** incl. `ro`,
  auto-detected (so `english_only = false`, no local fallback needed).
  TTS models are **text-in/audio-out only** and **Preview**. Streaming supported
  from the 3.x TTS models — enables sentence-by-sentence assistant replies.
- **Realtime (Live API)**: `wss://generativelanguage.googleapis.com/ws/
  google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key=`,
  native audio in/out + input/output transcription + `toolCall`/`toolResponse`
  function calling. Same key, free tier applies. **Preview** (schema may drift).

## What changes vs the current tree

- The `gemini` catalogue entry gains real `stt`, `tts`, and `assistant`
  capabilities (today: polish-only, and even that is a stub).
- New backend clients: `fono-stt::gemini`, `fono-tts::gemini`,
  `fono-polish::gemini` (replace the stub), `fono-assistant::gemini` (staged) +
  `fono-assistant::gemini_live` (realtime).
- `SttBackend::Gemini`, `TtsBackend::Gemini`, `AssistantBackend::Gemini` added;
  `PolishBackend::Gemini` stub implemented.
- New `TtsEndpoint::Gemini` variant (generateContent AUDIO shape — not
  OpenAI-compatible).
- The Cloud-Speech `google` entry is left as an inert stub (or removed); it is
  **not** part of this work. No service account, no JWT, no gRPC, no new auth crate.

## Implementation Plan

### Section A — Decision + plan hygiene

- [ ] Task A1. **Short ADR "Google via Gemini API (single key)"** recording: one
      `GEMINI_API_KEY`, free-tier reliance and its RPD/RPM consequences, why the
      Cloud-Speech/Chirp lane and v1's `fono-net-google` OAuth crate are dropped,
      and the per-capability endpoint/model choices. Rationale: locks the
      single-surface decision so no future contributor re-opens the Chirp path.
- [ ] Task A2. **Retire/redirect the v1 Chirp plan and the `google` stub.** Mark
      `2026-05-14-google-chirp-stt-v1.md` superseded; note in the catalogue that
      `google` (Cloud Speech) is out of scope. Rationale: avoid two conflicting
      "Google" stories.
- [ ] Task A3. **Document free-tier limits** in `docs/providers.md` (RPD/RPM per
      model, preview-model caveat for TTS/Live, midnight-Pacific reset) so users
      set correct expectations before hitting a quota wall.

### Section B — Catalogue + config wiring (foundation for all clients)

- [ ] Task B1. **Expand the `gemini` catalogue entry**: `stt: Some(...)` (model
      `gemini-2.5-flash`), `tts: Some(TtsDefaults { model:
      "gemini-2.5-flash-preview-tts", default_voice: "Kore", endpoint:
      TtsEndpoint::Gemini{…}, english_only: false, … })`, `assistant:
      Some(AssistantDefaults{…, web_search: NativeTool("google_search")})`, and
      bump polish from `gemini-1.5-flash` to `gemini-2.5-flash`. Add a
      `TtsEndpoint::Gemini` variant. Rationale: the catalogue is the single source
      of truth every wizard/doctor/factory consumer already reads.
- [ ] Task B2. **Add backend enum variants + round-trip wiring**: `SttBackend::
      Gemini`, `TtsBackend::Gemini`, `AssistantBackend::Gemini`; update all
      `parse_*`/`*_str`/`all_*`/`*_key_env` (`GEMINI_API_KEY`) in
      `crates/fono-core/src/providers.rs`. Keep the catalogue's compile-time
      no-orphan-variant tests green. Rationale: makes `fono use {stt,tts,
      assistant} gemini` legal and keeps the drift guards intact.
- [ ] Task B3. **Cloud TTS voice palette for Gemini**: map the 30 prebuilt voices
      into `voice_palette` with a `Gender` per voice so per-program/positional
      voice selection works (consistent with the Kokoro/cloud palette work).
      Rationale: integrate with the existing voice-resolver rather than bolting on
      a bespoke picker.

### Section C — STT (Gemini audio understanding)

- [ ] Task C1. **`fono-stt::gemini` client**: WAV/PCM → inline base64 (or Files
      API for large clips) → `generateContent` with a deterministic transcribe
      prompt + language hint from `general.languages`; parse the text out.
      Honour the `fono.http` instrumentation + watchdog like other cloud STT.
      Rationale: the STT ask on the single key.
- [ ] Task C2. **Language-stickiness degradation note**: Gemini returns no
      per-segment confidence, so the logprob rerun/silence-hallucination filter is
      unavailable — warn once per process and accept the detected response (mirror
      the Cartesia/ElevenLabs handling). Rationale: don't silently pretend a signal
      exists that the API doesn't provide.
- [ ] Task C3. **Wizard + `fono use stt gemini` + doctor + docs.** Rationale:
      parity with the Groq/OpenAI STT flow; key reuse across all four capabilities.

### Section D — TTS (Gemini native speech)

- [ ] Task D1. **`fono-tts::gemini` client**: `generateContent` (and
      `:streamGenerateContent` where the model supports it) with
      `responseModalities:["AUDIO"]` + `prebuiltVoiceConfig.voiceName`; decode
      base64 `inlineData` → 24 kHz mono PCM straight into the playback pipeline.
      Resolve voice via the palette (Task B3); `english_only = false`. Rationale:
      the TTS ask; 24 kHz mono matches the pipeline with no resampling.
- [ ] Task D2. **Sentence-by-sentence streaming** for assistant replies on the
      streaming-capable TTS model; one-shot fallback otherwise. Rationale: keeps
      assistant time-to-first-audio low, matching the existing sentence-splitter
      contract.
- [ ] Task D3. **Wizard voice picker + `fono use tts gemini` + doctor + docs**
      (incl. the preview-model caveat). Rationale: parity with existing cloud TTS.

### Section E — LLM (polish + staged assistant)

- [ ] Task E1. **`fono-polish::gemini`**: replace the `not yet implemented` stub
      (`crates/fono-polish/src/factory.rs:97`) with a real `generateContent`
      cleanup client reusing the shared polish prompt/guards. Rationale: today the
      catalogue advertises Gemini polish but it errors at runtime.
- [ ] Task E2. **`fono-assistant::gemini` staged chat client**:
      `streamGenerateContent` SSE → `TokenDelta` stream; system instruction;
      multi-turn history; **function calling** mapped to the existing tool
      round-trip; `google_search` tool when `prefer_web_search`; image/vision
      input when `prefer_vision`. Wire `AssistantBackend::Gemini` into the factory.
      Rationale: makes "pick Gemini as the assistant" work in staged (HTTP) mode —
      the gap v1 flagged as having no owning plan.

### Section F — Prerequisites for realtime-with-tools

- [ ] Task F1. **Land `voice-actions-via-mcp-v1`** (the `fono-action` crate:
      `Tool`/`ToolSpec`/`ToolCall`/`ToolResult`/`ToolRegistry`/`Dispatcher`, MCP
      stdio+SSE, `[assistant.tools]`, confirmation skeleton). Not built yet; hard
      prerequisite. Section E2's function-calling should share this tool surface.
- [ ] Task F2. **Land realtime v4 Phase 1 catalogue reshape** (`ModelEntry`,
      `Transport`, `RealtimeProfile`/`RealtimeProtocol`, `CostTier`,
      `Badge::Realtime`, the five invariants), migrating every provider off
      `text_model`/`multimodal_model`. Independent of F1; gate as one change since
      it touches all entries.

### Section G — Realtime Gemini Live with tools (depends on E2, F1, F2)

- [ ] Task G1. **`RealtimeAssistant` trait + `RealtimeSession` handle** (realtime
      v4 Phase 3): session-handle shape so tool results feed back into the same
      WebSocket.
- [ ] Task G2. **`fono-assistant::gemini_live` client**: `BidiGenerateContent`
      WS; `setup` with model + system instruction + `tools.functionDeclarations`
      from `&[ToolSpec]`; map `serverContent…inlineData`→audio,
      input/output transcription→text events, `toolCall`→`ToolCallRequested`,
      `turnComplete`→`Done`; submit results as `toolResponse.functionResponses`.
- [ ] Task G3. **Factory `Transport` dispatch + F8 orchestrator short-circuit +
      barge-in/cancel**, reusing the **same `fono-action::Dispatcher`** as the
      staged path (single policy/confirmation surface). F7 dictation untouched.
- [ ] Task G4. **Wizard cost-labelled model picker, `fono doctor` mode/tool-count
      row, tests (tool round-trip, F7-byte-identical regression), free-tier-aware
      cost docs.**

## Verification Criteria

- A user pastes **one** `GEMINI_API_KEY`, and `fono use {stt,tts,polish,
  assistant} gemini` each yield a working backend; `fono doctor` validates the key
  (already catalogued) and reports reachability.
- STT transcribes via `generateContent`; the no-confidence degradation is logged
  once, not silently faked.
- TTS plays 24 kHz mono PCM with a selected prebuilt voice; non-English text
  (e.g. Romanian) is spoken natively (no english-only fallback engaged).
- Polish runs on a real Gemini client (stub removed); staged assistant streams,
  calls `google_search`/functions, and accepts image input under the existing
  flags.
- Realtime: F8 with a Gemini Live model opens one WebSocket, **no local STT
  loaded**, and tool calls dispatch through the same `Dispatcher` as staged; F7
  dictation byte-identical regardless of transport.
- `docs/providers.md` states the free-tier RPD/RPM limits and the preview caveat
  for TTS/Live.
- Pre-commit gate green (`cargo fmt --check`, `clippy -D warnings`, workspace
  tests); `cargo deny` clean for any new deps (Live adds `tokio-tungstenite` +
  `base64`, already used elsewhere).

## Potential Risks and Mitigations

1. **Free-tier quota walls (RPD/RPM) surprise users mid-session.**
   Mitigation: document limits (A3); surface a clear, actionable error on HTTP 429
   pointing at the quota and the midnight-Pacific reset; never crash the daemon.
2. **TTS and Live are Preview — schemas drift.**
   Mitigation: quarantine wire shapes in the per-client modules; pin the model ids
   in the catalogue; provider events never leak into the shared `RealtimeEvent`.
3. **Gemini STT is LLM-driven (no confidence, higher/again variable latency, batch
   only).** Mitigation: Task C2 degradation note; keep Gemini STT a deliberate
   choice, not the default; F7 streaming dictation stays on the existing streaming
   backends; realtime transcription comes from the Live path.
4. **STT prompt-injection / non-transcript replies** (the model "answers" the
   audio instead of transcribing). Mitigation: a strict transcribe-only system
   instruction + post-hoc guard, reusing the polish clarification-refusal pattern.
5. **Realtime sits behind two unbuilt prerequisites (F1, F2).**
   Mitigation: Sections C/D/E ship independently useful value on the single key
   before realtime (G) is reachable; F1/F2 can proceed in parallel.
6. **Preview model ids get renamed/retired by Google.**
   Mitigation: ids live only in the catalogue; `fono doctor` reports the active id
   so a `model_not_found` is diagnosable; CHANGELOG note on each bump.

## Alternative Approaches

1. **Keep STT/TTS on Cloud Speech (Chirp) for quality, Gemini for LLM/realtime
   (v1's design).** Rejected by the user's single-key/free-quota requirement —
   Cloud Speech needs a GCP project, service-account JSON, and billing.
2. **Gemini for LLM/realtime only; leave STT/TTS on existing cloud providers
   (Groq/Deepgram/etc.).** Smaller scope, still single Gemini key for the
   assistant. Trade-off: not "Google for all four," but a pragmatic interim if
   Gemini STT/TTS quality or quota proves limiting. Worth keeping as a fallback
   posture.
3. **Use the OpenAI-compatibility shim Gemini exposes (`/openai`) to reuse Fono's
   existing OpenAI-compat chat/STT/TTS clients.** Could cut new-client work for
   LLM/STT/TTS. Trade-off: the compat layer lags native features (function-calling
   nuances, native TTS voices, Live), so it likely can't cover TTS voices or
   realtime — evaluate as a shortcut for the LLM client only, not the whole plan.
