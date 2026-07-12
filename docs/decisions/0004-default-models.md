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

### TTS (text-to-speech) — added 2026-05-31

Local TTS defaults, per
`plans/2026-05-31-local-tts-onnx-voice-stack-and-wyoming-server-v3.md`:

- **Piper** (`OHF-Voice/piper1-gpl`) — **GPL-3.0**. The `rhasspy/piper`
  repo (formerly MIT) was archived 2025-10; upstream relicensed to
  GPL-3.0 under the Open Home Foundation. GPL-3.0 is **compatible with
  Fono's GPL-3.0-only license** — fine to link, arguably cleaner than a
  permissive dep. Used for Romanian and the long tail of languages.
- **Kokoro** — **Apache-2.0**. Used for its trained high-prosody
  locales.

Both clear the bar below: OSI-/GPL-compatible, neither is a
Llama-family nor non-OSI Gemma model. The engines run on the **ONNX Runtime**
voice-stack platform (ADR 0032), statically linked via `ort`; Piper and
Kokoro are distributed as `.onnx` and load directly. Model weights
download at runtime, never bundled.

### Voice stack (other ONNX models) — added 2026-05-31, per ADR 0032

The same ONNX runtime carries the rest of the local voice stack. Default
models, all license-clean:

- **Silero VAD** — MIT/Apache (neural VAD upgrade over the energy
  envelope).
- **Zipformer transducer** (k2-fsa / sherpa) — Apache-2.0 (streaming
  STT, which whisper.cpp cannot do natively).
- **Wake-word KWS** — **openWakeWord** on the existing minimal `ort`
  runtime, with a clean Apache-2.0 default phrase model (`hey_fono`).
  **This supersedes the earlier note here that named a k2-fsa / sherpa
  transducer KWS as the wake-word engine** — see ADR 0012 (Accepted,
  2026-06-23) for the reversal. The deciding factor was binary size:
  sherpa-onnx would require a *second* full ONNX runtime (failing the
  ADR 0022 size-gate and the four-entry `NEEDED` allowlist), whereas
  openWakeWord adds only a handful of small ops to the shared runtime.
  Upstream CC-BY-NC-SA community phrase models are opt-in / on-demand /
  notice-on-download / never bundled — the same "opt-in only, never
  default" carve-out applied to restricted models below.

These are **opt-in capabilities** layered on the shared runtime as the
stack grows; each new model must be added to the minimal-build
`ops.config` (see `docs/binary-size.md`) so the runtime stays small.

## License tiers for default models — amended 2026-07-12

Default-model eligibility is decided by license tier:

1. **OSI-approved / GPL-compatible** (MIT, Apache-2.0, GPL-3.0, CC-BY-4.0) —
   default-eligible, unchanged.
2. **RAIL-class behavioral-restriction licenses** (e.g. OpenRAIL-M) —
   **default-eligible**, provided the restrictions are behavioral-only (no
   commercial limits, no field-of-use bans beyond harmful/illegal use, no user
   caps). Rationale: model weights are runtime-downloaded data, never linked
   into or bundled with the GPL binary, and RAIL's use restrictions largely
   restate applicable law. Requirement: the download notice must name the
   license and link its text — the same notice-on-download pattern already
   applied to the wake-word community models (ADR 0012). RAIL licenses remain
   formally non-OSI (any use restriction fails OSD #6), so this tier is a
   deliberate, documented exception, revertible per model at any time since
   the OSI-clean engines stay in the binary.
3. **Commercial / field-restricted licenses** (Llama Community License, CC-NC
   variants) — opt-in only, never default, unchanged (see exclusions below).

Motivating case: Supertonic 3 TTS — code MIT, weights OpenRAIL-M; see
`plans/2026-07-12-supertonic3-local-tts-engine-v1.md`.

## Deliberate exclusions from defaults

- **Llama 3.x family** — the Llama Community License is **not OSI-approved**; its
  acceptable-use clauses conflict with the GPL-3.0 project ethos. Offered as
  opt-in only, gated behind an explicit `--accept-llama-license` flag.
- **Gemma family** — older Gemma releases used a custom Google license and remain
  opt-in only when published under non-OSI or extra-restriction terms. Gemma models
  may be defaults only when the specific artifact and its upstream base model are
  both published under an OSI-approved, GPL-3.0-compatible license such as
  Apache-2.0, with no additional use restrictions. Verified examples: Gemma 4 E2B
  and E4B instruction-tuned QAT/GGUF artifacts published by Google with
  `license: apache-2.0` and `license_link` to the Gemma 4 Apache-2.0 license.
- **Parakeet (NVIDIA)** — *amended 2026-07-12:* the original exclusion
  ("~600 MB quantised and English-only") described v1/v2. **Parakeet-TDT 0.6b
  v3 is multilingual** — 25 European languages including Romanian, with
  automatic language detection — and CC-BY-4.0, so the language objection no
  longer holds. Under re-evaluation as an opt-in high-accuracy
  European-language STT tier on the shared minimal `ort` runtime; feasibility
  on CPU demonstrated by CrispASR (ggml) and by the sherpa-onnx ONNX exports.
  See `plans/2026-07-12-parakeet-v3-stt-reevaluation-v1.md`. Size still keeps
  it out of the default download tier.

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
