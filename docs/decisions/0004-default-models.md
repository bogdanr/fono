# ADR 0004 — Default models (STT and LLM)

## Status

Accepted 2026-04-24.

## Context

The first-run wizard must offer both local-model and cloud-API paths, each with at
least two options. Defaults must be multilingual (Fono targets NimbleX, whose user
base is Romanian/English-primary but not English-only), must fit on modest hardware
(4-core CPU, 8 GB RAM, spinning-disk-tolerant), and must have licenses compatible
with GPL-3.0.

## Decision

### STT (Speech-to-Text)

- **Local default**: `whisper small` **multilingual** (Q5_1 quantisation, ≈180 MB,
  MIT license).
- **Local alternatives offered**: `tiny.en` (40 MB), `base` multilingual (60 MB),
  `base.en` (60 MB), `small.en` (180 MB).
- **Cloud options offered**: Groq, Deepgram, OpenAI, Cartesia, AssemblyAI,
  Speechmatics, Azure, Google, Nemotron.

### LLM (cleanup / formatting)

- **Local default**: `Qwen2.5-1.5B-Instruct` at `Q4_K_M` quantisation (≈1 GB,
  **Apache-2.0**, multilingual).
- **Local alternatives offered**: Qwen2.5-0.5B (400 MB, Apache-2.0),
  SmolLM2-1.7B (1 GB, Apache-2.0), skip-LLM (no cleanup stage).
- **Cloud options offered**: Cerebras, Groq, OpenAI, Anthropic, Gemini, OpenRouter,
  Ollama.

## Deliberate exclusions from defaults

- **Llama 3.x family** — the Llama Community License is **not OSI-approved**; its
  acceptable-use clauses conflict with the GPL-3.0 project ethos. Offered as
  opt-in only, gated behind an explicit `--accept-llama-license` flag.
- **Gemma family** — custom Google license, same reasoning. Opt-in only.
- **Parakeet (NVIDIA)** — Apache-2.0, so license-clean, but ~600 MB quantised and
  English-only. Too big for the default tier; available as opt-in for power users
  who want higher English STT accuracy.

## Rationale for `Qwen2.5-1.5B-Instruct` as the LLM default

- **License**: Apache-2.0, permissive, zero conflict with GPL-3.0.
- **Multilingual**: matters for Romanian, English, Spanish, etc. — the mix NimbleX
  users actually dictate in.
- **Instruction following**: excellent per size class. The LLM task here is filler
  removal + punctuation/capitalisation cleanup; it does **not** need reasoning. A
  1.5B-parameter model handles this at 20–30 tok/s on a modest CPU, keeping
  end-to-end dictation latency sub-second.
- **Tooling**: well supported by llama.cpp GGUF tooling, which Fono already uses.

## Rationale for `whisper-small` (multilingual) as the STT default

- **License**: MIT, permissive, zero conflict with GPL-3.0.
- **Size/quality sweet spot**: 180 MB delivers the best quality-to-size ratio for
  real-time dictation on a 4-core CPU.
- **Coverage**: ~95 languages with minimal sacrifice in English accuracy vs.
  `small.en`.

## Total first-run footprint

- **Balanced defaults (whisper-small + Qwen2.5-1.5B)**: ~1.2 GB total download.
- **"Lite" tier** (whisper-base + Qwen2.5-0.5B): ~440 MB total.
- **"Cloud-only"**: zero model download; user provides API keys instead.
