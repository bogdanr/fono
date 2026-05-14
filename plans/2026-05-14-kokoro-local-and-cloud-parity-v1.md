# Kokoro: local + cloud parity for Fono TTS

## Objective

Bring Kokoro back into Fono as a **first-class TTS option whose audio
output is identical whether the user picks the local backend or the
OpenRouter cloud backend**, while keeping
`openai/gpt-4o-mini-tts-2025-12-15` (swapped in on 2026-05-14 — see
`plans/2026-05-14-openrouter-tts-swap-to-openai-mini-v1.md`) as the
out-of-the-box OpenRouter default. This plan is a future-work tracker;
it intentionally trades step-level detail for scope clarity so the
work can be re-scoped when ONNX/runtime tooling settles.

## Scope summary

1. **Local Kokoro backend in `fono-tts`** — Apache-2.0-licensed
   end-to-end (model weights, G2P, runtime) so it can be the offline
   default per `docs/decisions/0004-default-models.md`.
   - ONNX-Runtime-based Kokoro inference (the upstream `hexgrad/Kokoro-82M`
     ONNX export is Apache-2.0).
   - `misaki` G2P bindings or, preferably, a pure-Rust phonemizer if
     one materialises (`espeak-ng` is GPL-3.0 — fine for us, but a
     pure-Rust option avoids the system dependency).
   - Streaming chunked synthesis so first-token latency is bounded.
   - Voice file cache and download flow (catalogue + checksum, cache
     dir under `~/.cache/fono/models/kokoro-82m-v1.0/`).
   - Latency target: **first-token < 200 ms on a 4-core x86 CPU**
     (matches the Comfortable hardware tier).

2. **Shared `KokoroVoiceRouter`** (consumed by both backends) — the
   language→voice mapping designed in the prior plan
   `plans/2026-05-14-openrouter-kokoro-multilingual-voice-routing-v1.md`.
   - Single `pick_voice(lang) -> &'static str` helper, with the full
     54-voice / 9-locale table (`a→af_heart`, `b→bf_emma`,
     `e→ef_dora`, `f→ff_siwis`, `h→hf_alpha`, `i→if_sara`,
     `j→jf_alpha`, `p→pf_dora`, `z→zf_xiaobei`).
   - BCP-47 → Kokoro lang-code adapter.
   - Used identically by the local ONNX backend and the OpenRouter
     passthrough so the **same `(text, lang, voice)` triple yields
     the same audio** regardless of where inference happens.

3. **Unified wizard UX** — voice + language presented as one setting,
   independent of backend.
   - Wizard picker shows "Kokoro" once, with a sub-prompt for
     local-vs-cloud purely as a runtime/latency knob, not a feature
     differentiator.
   - Voice override picker lists the full 54-voice catalogue with
     locale and quality grades from upstream `VOICES.md`.
   - Pinned test for the picker row strings.

4. **Cross-link / supersession**
   - This plan **supersedes the catalogue-fix portion** of
     `plans/2026-05-14-openrouter-kokoro-multilingual-voice-routing-v1.md`
     (which proposed wiring the router into the OpenRouter client
     while Kokoro was the default). The voice-router design notes in
     that plan remain reusable artifacts; the implementation lands
     here, gated on local Kokoro shipping first.

## Verification criteria (when this lands)

- `fono setup` offering Kokoro local picks ONNX weights, downloads
  voices, and synthesises French/German/Spanish/Mandarin in their
  native voices on the first run.
- Switching `[tts.cloud] model = "hexgrad/kokoro-82m"` against
  OpenRouter produces *audibly identical* output to the local backend
  for the same `(text, lang, voice)` triple (within decoder noise).
- Local first-token latency p50 < 200 ms on a 4-core x86 CPU.
- All 54 voices addressable from both backends through a single
  picker.

## Out of scope (deliberately)

- Kokoro v2 / multilingual fine-tunes (no public roadmap exists today).
- Streaming WebSocket TTS (Fono's pipeline batches per assistant turn).
- Voice-cloning / custom voices (Kokoro doesn't expose that surface).
