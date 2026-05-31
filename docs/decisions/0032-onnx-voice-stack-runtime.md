# ADR 0032 — ONNX Runtime as the local voice-stack platform

## Status

Accepted 2026-05-31.

Supersedes the **2026-05-31 "ggml-reuse, no ONNX" amendment** to ADR
0022 (which assumed local TTS would be hand-ported onto the shared ggml
runtime). Complements ADR 0005 (single static binary), ADR 0018 (ggml
link trick), ADR 0022 (binary size budget), ADR 0004 (default models),
and reactivates the intent of ADR 0012 (wake-word activation) by naming
the engine.

## Context

Fono is committed to becoming a **full local voice stack**, not just a
dictation tool. The roadmap (`ROADMAP.md`) calls for, beyond today's
Whisper STT + LLM cleanup:

- local **text-to-speech** (Piper, later Kokoro) — see
  `plans/2026-05-31-local-tts-onnx-voice-stack-and-wyoming-server-v3.md`;
- a **Wyoming TTS server** for Home Assistant (the server glue already
  shipped, Phase 2a);
- **wake-word** activation (ADR 0012);
- **streaming / live STT** (which whisper.cpp cannot do natively);
- **neural VAD** (an accuracy upgrade over today's energy envelope in
  `crates/fono-audio/src/silence_watch.rs`);
- later: punctuation restoration, speaker-ID / diarisation.

The question was which inference substrate runs these. Three were
weighed across a multi-step investigation (recorded in the v3 plan's
spike sections):

1. **ggml-reuse** — hand-port each model onto the ggml runtime Fono
   already links (whisper.cpp + llama.cpp). Smallest *if* shared-ggml
   lands, but a from-scratch VITS / StyleTTS2 / transducer graph **per
   model**, with no reference to crib, and no ggml/GGUF Piper exists.
2. **candle** — pure-Rust per-model ports. No new system deps, but a
   bespoke port per model and **no production Vulkan backend**; does not
   amortise across the stack (each feature pays its own engineering).
3. **ONNX Runtime** (via the `ort` crate / sherpa-onnx) — one
   Apache-2.0 runtime that already runs **all** of these model classes
   off-the-shelf through a single ONNX model format.

The deciding insight: the substrate choice is not "an engine for Piper"
but "the runtime for the whole voice stack." Piper, Kokoro, Matcha (TTS),
Zipformer (streaming STT), Silero (VAD), transducer KWS (wake-word),
punctuation and speaker-ID models are all distributed as ONNX and run on
one runtime. The runtime cost is paid **once** and amortised across 6+
features, versus a bespoke port per feature on ggml/candle.

## Decision

Adopt **ONNX Runtime, linked statically via the `ort` crate, as Fono's
local voice-stack inference runtime.** Concretely:

- **`ort` (currently `2.0.0-rc.12`, wraps onnxruntime 1.24.2 — pyke
  `ms@1.24.2`, verified from `ort-sys`'s `build/download/dist.txt`)** behind a
  `tts-local` / `voice-local` cargo feature, off in source-default
  builds, **on** in the shipped `cpu` and `gpu` artefacts. No third
  release variant (per the ADR 0022 no-variant rule).
- **Piper is the first consumer** (local TTS, incl. Romanian). Kokoro,
  wake-word, streaming STT, neural VAD follow on the *same* runtime with
  no new engine integration — only model wiring.
- **The LLM stays on ggml** (llama.cpp); **Whisper STT stays on ggml**
  for now and *may* migrate to ONNX later. The two runtimes coexist and
  split cleanly along the GPU boundary (see "CPU / Vulkan split").

### Why static ONNX clears Fono's hard constraints (spike evidence)

Measured 2026-05-31 with a scratch crate on `ort 2.0.0-rc.12`, built
`release` + LTO + `opt-level=s` + `strip`:

- **onnxruntime links statically.** `ort`'s default `download-binaries`
  fetches a prebuilt **static** `libonnxruntime.a` and embeds it. There
  is **no `libonnxruntime.so` in `NEEDED`** (`readelf -d` verified). This
  honours the "no external deps except runtime downloads" rule — only
  model weights download at runtime, never engine code.
- **`NEEDED` stays the four-entry allowlist** (`libc`, `libm`,
  `libgcc_s`, `ld-linux`) once libstdc++ is linked statically. `ort-sys`
  (`build/static_link/mod.rs:20-32`) emits a *dynamic*
  `cargo:rustc-link-lib=stdc++`; Fono already forces static libstdc++ for
  ggml's C++ via the `llama-cpp-2/static-stdcxx` feature. The spike
  proved `libstdc++.so.6` drops out and the binary still runs
  (`ort init committed: true`).

### Keep it small: build only what we use

The default `ort` prebuilt is the **full** onnxruntime — every operator
for every model class, all execution providers — measured at **~19 MiB**
of `.text`+data. Fono runs a small, fixed model set, so we **must** ship
a **custom minimal build** tuned to exactly our operators:

```
build.sh --config MinSizeRel --build_shared_lib \
  --minimal_build \
  --include_ops_by_config <ops.config generated from our models> \
  --enable_reduced_operator_type_support \
  --disable_ml_ops --disable_exceptions --disable_rtti \
  --skip_tests
```

with shipped models converted to **ORT format**. This is the same path
ONNX Runtime Mobile uses (mobile minimal builds land ~5–7 MiB). Realistic
target for Fono's op set (Piper VITS + Kokoro + Silero + Zipformer + KWS):
**~7–11 MiB**, roughly halving the measured cost. The custom static
`libonnxruntime.a` is built in CI and pinned via `ORT_LIB_LOCATION`
(turning `download-binaries` off). **Every new model added to the stack
must regenerate `ops.config`** so the operator set tracks what we
actually ship — this is the standing discipline that keeps the runtime
small as the stack grows.

CPU acceleration uses the **XNNPACK** EP (statically linkable, `ort`'s
`xnnpack` feature) — not GPU.

### CPU / Vulkan split

ONNX Runtime has **no Vulkan EP**. The only cross-vendor GPU paths are
DirectML (Windows-only) and WebGPU-via-Dawn; `ort-sys`
(`build/static_link/mod.rs:64-67`) ships Dawn as a **dynamic** library
("Dawn cannot be linked statically yet"), which would break the
four-entry `NEEDED` allowlist. It is also unnecessary: Piper, Kokoro,
Silero, KWS and small/streaming STT are all **CPU-realtime**.

Therefore the runtimes split along the GPU boundary:

- **ggml-Vulkan** (existing `gpu` variant) serves the only GPU-hungry
  workloads — whisper-large and the LLM.
- **ONNX** serves the voice stack, **CPU-only** (XNNPACK).

### Offsetting the size increase

The ONNX runtime is additive to the canonical binary (no variant). Two
standing levers offset it:

1. **Minimal build** (above) — the primary lever, ~19 → ~7–11 MiB.
2. **Source-shared ggml** (ADR 0022 Phase 1 Task 1.2, deferred) —
   reclaims ~7 MiB by deduplicating the whisper.cpp / llama.cpp ggml
   copies and retiring the `--allow-multiple-definition` trick (ADR
   0018). Scheduled *after* Piper ships, explicitly to offset the ONNX
   addition.

## Consequences

- **Size budget rises.** ADR 0022's `cpu` cap moves from 20 MiB; the new
  cap is set there after the minimal-build number is measured (target
  ~32 MiB, re-measured post-dedup). `gpu` stays ≤ 64 MiB.
- **Two inference runtimes** (ggml + ONNX) to maintain. Accepted: they
  do not overlap in responsibility and split cleanly by GPU need.
- **Build-engineering tail:** Fono now owns an onnxruntime minimal-build
  pipeline + ORT-format model conversion + `ops.config` generation in
  CI, pinned via `ORT_LIB_LOCATION`. Documented in `docs/binary-size.md`.
- **Model licensing** is per-model (ADR 0004): Piper GPL-3.0 (compatible),
  Kokoro / Silero / Zipformer / KWS Apache-2.0. No Llama/Gemma defaults.
- **Reproducible/offline builds** require the vendored static lib, not the
  pyke CDN fetch — handled by the `ORT_LIB_LOCATION` pin.

## Alternatives rejected

- **ggml-reuse** — smallest in theory but a from-scratch model-graph port
  per feature, gated on shared-ggml, with no Piper/Kokoro ggml reference.
  Does not scale to a multi-model stack.
- **candle** — pure-Rust and dependency-clean, but a bespoke port per
  model, no production Vulkan backend, and no amortisation across the
  stack. Reasonable for Piper *alone*; wrong for a growing stack.
- **Dynamic onnxruntime (`load-dynamic`)** — would add `libonnxruntime.so`
  to `NEEDED`, breaking ADR 0005 / ADR 0022. Rejected.

## Surviving artefacts

- `docs/binary-size.md` — the consolidated size-and-capability
  engineering guide (minimal build, NEEDED allowlist, dedup, the
  per-model `ops.config` discipline).
- `plans/2026-05-31-local-tts-onnx-voice-stack-and-wyoming-server-v3.md`
  — implementation plan.
- `docs/decisions/0022-binary-size-budget.md` — amended: new cap, ONNX
  replaces the ggml-reuse TTS line.
- `docs/decisions/0004-default-models.md` — amended: per-model licensing.
