# ADR 0034 — Google support via the Gemini API (single key, free tier)

- **Status:** Accepted
- **Date:** 2026-06-17
- **Supersedes:** none
- **Related:** [ADR 0025 — Cloud provider capability catalogue](0025-cloud-provider-catalogue.md),
  [ADR 0024 — Assistant multimodal & web search](0024-assistant-multimodal-and-search.md),
  [ADR 0004 — Default models](0004-default-models.md)
- **Plan:** [`plans/2026-06-17-google-via-gemini-single-key-stt-tts-llm-realtime-v2.md`](../plans/2026-06-17-google-via-gemini-single-key-stt-tts-llm-realtime-v2.md)

## Context

We want "Google" as a provider across all four Fono capabilities — STT, TTS,
LLM polish/assistant, and a realtime assistant with tool support. Google exposes
these through **two distinct API surfaces** with different auth models:

- **Google Cloud Speech** (Chirp 3 STT, Chirp 3 HD TTS) — requires a GCP
  project, a service-account JSON credential (`GOOGLE_APPLICATION_CREDENTIALS`),
  OAuth2/JWT token exchange, and an active billing account. No simple
  single-key-with-free-quota path.
- **Gemini API / Google AI Studio** (`GEMINI_API_KEY`) — a single API key from
  <https://aistudio.google.com/apikey>, with a **Free usage tier** (per-model
  RPM/TPM/RPD limits, daily counts reset at midnight Pacific) that needs only an
  active project, **no billing**. The one base host
  `https://generativelanguage.googleapis.com` serves LLM chat
  (`generateContent` / `streamGenerateContent`, function calling,
  `google_search` grounding, vision), audio-understanding STT, native TTS
  (`responseModalities:["AUDIO"]` → raw 24 kHz mono 16-bit PCM, 30 prebuilt
  voices, 40+ languages), and the realtime Live API
  (`BidiGenerateContent` WebSocket with tool use).

The earlier two-lane design (Chirp for speech + Gemini for LLM/realtime) implied
two keys and two auth styles.

## Decision

**Fono's Google support is the Gemini API on a single `GEMINI_API_KEY`, relying
on its free tier.** The Google Cloud Speech / Chirp lane and any
service-account/OAuth credential crate are **dropped**. Every capability is wired
onto the existing `gemini` catalogue entry
(`crates/fono-core/src/provider_catalog.rs`):

- **Polish + staged assistant LLM** reuse the existing OpenAI-compatible client
  against Gemini's compat surface
  (`https://generativelanguage.googleapis.com/v1beta/openai/chat/completions`,
  `Authorization: Bearer <key>`). No bespoke chat client. Note the compat layer
  does **not** expose the native `google_search` grounding tool; native web
  search for the staged path is a follow-up that uses the `generateContent`
  endpoint directly.
- **STT** uses a bespoke audio-understanding client (`generateContent` with a
  transcribe-only instruction). It is prompt-driven ASR: **no per-segment
  confidence**, batch-only (no native streaming ASR), so it stays an opt-in
  choice — F7 streaming dictation remains on the streaming STT backends, and the
  language-stickiness logprob rerun is skipped for Gemini (see ADR 0017).
- **TTS** uses a bespoke native-speech client (`generateContent` /
  `streamGenerateContent`, `responseModalities:["AUDIO"]`). Output is 24 kHz mono
  PCM straight into the pipeline; multilingual (`english_only = false`).
- **Realtime** uses the Live API WebSocket with the same key; it depends on the
  `fono-action` tool dispatcher and the catalogue `ModelEntry` reshape, both
  tracked separately.

The Cloud-Speech `google` catalogue entry is left inert and out of scope.

## Consequences

- One key configures all four capabilities; the wizard prompts for
  `GEMINI_API_KEY` once and `fono doctor` validates it via the already-catalogued
  `KeyValidation` (query-param probe against `/v1beta/models`).
- Users are subject to free-tier RPD/RPM limits; a 429 must surface an
  actionable error (quota + midnight-Pacific reset) and never crash the daemon.
  Limits are documented in `docs/providers.md`.
- TTS and Live are **Preview** models — wire schemas may drift; model ids live
  only in the catalogue so a rename is a one-line fix and `fono doctor` reports
  the active id.
- We do **not** ship Chirp-quality dedicated ASR; Gemini STT quality/latency is
  accepted as the trade for the single-key/free-tier simplicity. If it proves
  limiting, the documented fallback is to keep STT/TTS on existing cloud
  providers and use Gemini only for LLM/realtime.
