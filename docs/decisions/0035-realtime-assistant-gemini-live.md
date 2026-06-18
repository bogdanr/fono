# ADR 0035 — Realtime assistant via Gemini Live (audio loop first, tools deferred)

- **Status:** Accepted
- **Date:** 2026-06-17
- **Supersedes:** none
- **Related:** [ADR 0034 — Google via the Gemini API (single key)](0034-google-via-gemini-single-key.md),
  [ADR 0025 — Cloud provider capability catalogue](0025-cloud-provider-catalogue.md),
  [ADR 0024 — Assistant multimodal & web search](0024-assistant-multimodal-and-search.md)
- **Plan:** [`plans/2026-05-25-realtime-end-to-end-assistant-v4.md`](../plans/2026-05-25-realtime-end-to-end-assistant-v4.md)

## Context

The staged Gemini assistant pipeline (STT → LLM → per-sentence TTS) has two
problems the staged path **cannot** fix, both confirmed against the live API and
a `/tmp/fono-traces` waterfall:

1. **Per-sentence voice drift.** Each sentence is a separate Gemini TTS
   `generateContent` call, and the prebuilt voices differ slightly between
   calls, so a multi-sentence reply sounds "wired" / inconsistent.
2. **Batch-TTS latency.** `gemini-3.1-flash-tts-preview` returns each call's
   audio as a **single terminal block** (~6 s/sentence first-block), even over
   `streamGenerateContent` — so intra-utterance streaming has nothing to release
   early. First audio lands at the *end* of the first sentence's synthesis.

The Gemini **Live API** (`BidiGenerateContent` WebSocket) synthesises the whole
reply as **one continuous voice** and emits audio **incrementally** as it is
generated — fixing both problems at once, on the same single `GEMINI_API_KEY`
(ADR 0034), with no provider mixing.

The original realtime design (`plans/2026-05-25-…-v4.md`) mandated **tool
support before realtime ships** (it explicitly rejected tool-less realtime). But
tool-calling is blocked on the `fono-action` dispatcher crate, which does not
exist yet — a large arc to build before the user hears any latency/voice win.

## Decision

**Land the realtime audio loop first (Path B); defer tool-calling.** The Gemini
Live audio loop does not need `fono-action` — only tool-calling does. The
`RealtimeAssistant::open_session` seam is designed so that passing tool specs is
an additive, later change.

- A `RealtimeProfile` is added **additively** to the catalogue `AssistantDefaults`
  (`crates/fono-core/src/provider_catalog.rs`) — no reshape of the existing
  `text_model` / `multimodal_model` slots; all non-Gemini providers get `None`.
  Gemini carries a Gemini Live profile (16 kHz mic in / 24 kHz reply out).
- A `RealtimeAssistant` trait (`fono-assistant/src/traits.rs`) yields a
  `RealtimeSession` (mic-in `mpsc` + a reply `events` stream of
  `RealtimeEvent::{Audio, AssistantTextDelta, UserTextFinal, Done}`).
- `GeminiLive` (`fono-assistant/src/gemini_live.rs`, behind a default-on
  `realtime` feature) implements it over `tokio-tungstenite` — **net-zero on
  binary size** (already in the graph via `fono-stt`/`fono-net`/`fono-mcp-server`).
- The factory returns an `AssistantHandle::{Staged, Realtime}`. Selection is
  **opt-in by model id**: realtime activates only when the backend is Gemini
  **and** `[assistant.cloud].model` equals the catalogue's `RealtimeProfile`
  model. A blank/default model stays staged, so existing Gemini users are
  unaffected.
- The orchestrator (`session.rs`) short-circuits to `run_realtime_turn`
  (`assistant.rs`) when a realtime backend is loaded: it streams the captured
  mic PCM into the session and plays the reply gaplessly through a
  `LocalPlaybackSink` as frames arrive, reporting honest first-frame TTFA.

Push-to-talk is **one-shot** for now (capture on release, then stream in); live
mic streaming during hold and barge-in are a follow-up increment.

## Consequences

- The voice-consistency and latency problems are fixed for the realtime path
  without waiting on the entire tool stack.
- **Tool-calling is absent** until `fono-action` lands; the trait + Live setup
  message already leave room for it (`open_session` takes the context; a `tools`
  argument is the additive follow-up).
- The Live model id and wire schema are **Preview** — verified offline by unit
  tests only (the dev key was rotated). The id lives solely in the catalogue, so
  a rename is one line and `fono doctor` reports the active mode; the known-GA
  fallback is `gemini-2.0-flash-live-001`.
- Realtime sources its transcription from the Live session's own input/output
  transcription, independent of the staged STT backend; the staged STT/LLM/TTS
  slots are empty in realtime mode (the orchestrator must branch before them).
- If Live quality/latency disappoints, the staged Gemini path (ADR 0034) remains
  the default; realtime is opt-in by model id and trivially reverted.
