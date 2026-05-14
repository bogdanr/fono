# OpenRouter Gemini 3.1 Flash TTS: opt-in, never default

## Objective

Expose `google/gemini-3.1-flash-tts-preview` as an **opt-in** OpenRouter
TTS choice for power users who want 70+ language coverage or inline
audio-tag steering (`[whispers]`, `[laughs]`, etc.). It must surface
**only in the wizard's customize step**, never as the primary-pick
default, and the catalogue copy must flag its Preview status and
per-token pricing risk. The default OpenRouter TTS remains OpenAI
GPT-4o Mini TTS — see
`plans/2026-05-14-openrouter-tts-swap-to-openai-mini-v1.md`.

## Why opt-in only

- **Preview-stage snapshot.** The `-preview` suffix means the model id
  can be re-routed or deprecated without notice. Pin the dated
  snapshot if/when one becomes available.
- **Pricing skew.** $1 / M input tokens + $20 / M output tokens —
  roughly 20× the per-turn cost of OpenAI Mini TTS for typical Fono
  workloads. Defaulting to it would be a footgun for users who don't
  realise OpenRouter TTS bills per token rather than per character.
- **Wire-format gap.** Our `OpenAiCompatTtsClient` cannot expose the
  inline audio-tag steering or the long-form streaming features
  without new wire work. Power users who want those features will
  edit `config.toml` directly anyway.

## Scope summary

1. **Catalogue entry** — extend
   `crates/fono-core/src/provider_catalog.rs` with a secondary
   OpenRouter TTS variant under a new `extras: &[TtsDefaults]` field
   (or equivalent), tagged `kind: Preview`, model
   `google/gemini-3.1-flash-tts-preview`, default voice TBD from the
   Gemini Flash voice list. Keep `endpoint = TtsEndpoint::OpenAiCompat`
   (OpenRouter's proxy normalises Gemini onto the OpenAI-compat shape
   for the speech endpoint).
2. **Wizard customize-step surface only** — the primary-pick row for
   OpenRouter never advertises Gemini. The customize TTS picker lists
   `OpenRouter (Gemini Flash TTS — Preview)` as a row beneath the
   default; selecting it writes the model + voice into
   `[tts.cloud]` and prints a one-line Preview-pricing notice.
3. **Pricing prompt** — when the user picks Gemini Flash TTS, the
   wizard prints the live $/M token figures (cached from the existing
   `/v1/models/{id}/endpoints` validation hit) and asks for explicit
   confirmation. No silent opt-in.
4. **CHANGELOG entry** under `### Added` when this ships.

## Verification criteria (when this lands)

- Running `fono setup` and accepting all primary-pick defaults never
  selects Gemini Flash TTS.
- Choosing customize → TTS → `OpenRouter (Gemini Flash TTS — Preview)`
  writes the correct model id to `config.toml` and prints the pricing
  warning verbatim.
- Synthesising in Swahili / Bengali / Hindi / Tagalog produces
  natural-sounding output (the 70+-language coverage claim).
- No regression on the default OpenRouter path: the swap from Kokoro
  to OpenAI Mini TTS remains the documented default.

## Out of scope

- Inline audio tags (`[whispers]`, `[laughs]`) — needs wire-format
  work beyond what the OpenAI-compat TTS shape exposes.
- Multimodal Gemini features (text+image+audio fusion) — not a TTS
  concern.
- Replacing OpenAI Mini TTS as the OpenRouter default — explicitly
  *not* the goal.
