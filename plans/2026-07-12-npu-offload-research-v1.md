# NPU Acceleration Research (Intel AI Boost / OpenVINO)

## Objective

Investigate whether Fono can offload parts of its inference pipeline to an
on-device **NPU** (Neural Processing Unit) via **OpenVINO**, for **power and
CPU/GPU offload** — not raw latency — while keeping the default single-binary
ship artefact and its size budget (ADR 0022) completely untouched. This is a
**low-priority, spike-first research project**: it produces measurements and a
go/no-go memo before any shipping code is written. Nothing here blocks or
reorders the current roadmap.

## Why now / why at all

- Modern laptops increasingly carry NPUs (Intel Lunar/Meteor Lake "AI Boost",
  AMD XDNA, Apple Neural Engine, Qualcomm Hexagon). They are built for
  **sustained low-power fixed-shape inference** — exactly the profile of Fono's
  always-on listening path.
- The motivating value is **battery and thermals on thin laptops**, plus
  keeping the CPU/GPU free for the user's foreground app while Fono listens or
  speaks. On these machines Fono's small models already beat real-time on CPU,
  so the NPU is not expected to win on wall-clock latency.
- A working reference environment now exists (see "Reference hardware"), so the
  feasibility questions can be answered with real measurements rather than
  vendor spec sheets.

## Reference hardware (measured, dev machine — 2026-07-12)

Verified end-to-end through the OpenVINO C API on the dev box (Intel Core Ultra
7 258V, Lunar Lake), so the numbers below are real, not spec-sheet:

- Device: **Intel(R) AI Boost**, NPU architecture **4.0**, 6 NCE tiles,
  `DEVICE_TYPE = integrated` (shared system memory, no dedicated VRAM).
- Precisions (`OPTIMIZATION_CAPABILITIES`): **FP16, INT8, EXPORT_IMPORT**.
  **No FP32** (`DEVICE_GOPS` reports `f32: 0`).
- Throughput (`DEVICE_GOPS`): **INT8 ≈ 46.7 TOPS**, **FP16 ≈ 23.3 TFLOPS**
  (INT8 is 2× FP16 — models want to be INT8-quantised to be worth it).
- Toolchain present: OpenVINO 2024.4.1 built from the NimbleX SlackBuild
  (`/mnt/nvme0n1p5/Work/slackbuilds/openvino`) with the `intel_npu` plugin
  enabled; Level-Zero loader + NPU UMD + `intel_vpu` kernel driver all live.
  `get_available_devices()` returns `[CPU, NPU]`. (No `plugins.xml` needed —
  the registry is compiled into `libopenvino.so`; `ENABLE_PLUGINS_XML` defaults
  OFF.) The iGPU does **not** enumerate — that needs the Intel Compute Runtime
  (NEO) stack, out of scope here.

Design implications: models must be **static-shape** and **INT8/FP16**; compiled
blobs should be **cached to disk** (EXPORT_IMPORT) to avoid multi-second graph
compilation on every launch.

## Non-negotiable constraints

- **Default ship binary is unchanged.** OpenVINO stays a *runtime `dlopen`
  dependency*, never linked into the default 25 MiB static build. The NPU path
  is an **opt-in build variant** (mirroring the existing `accel-*` feature-flag
  pattern) and, per ADR 0032, must not perturb the minimal static
  `libonnxruntime.a` used by the default build.
- **Detect-and-fall-back, always.** Even in the opt-in variant, the NPU is used
  only if `get_available_devices()` lists it at runtime; otherwise Fono runs
  exactly as today on CPU. Fallback order: **NPU → CPU** (GPU later, if the NEO
  stack is ever in scope). Surface the active device in
  `hardware_acceleration_summary()` (`crates/fono/src/daemon.rs`) / `fono doctor`.
- **No new-to-project dependency** in the default graph without sign-off
  (AGENTS.md). OpenVINO is a *system* dependency of the opt-in variant, not a
  crate in the default binary's graph.
- **License:** OpenVINO is Apache-2.0 (GPL-3.0-compatible). Any NPU-specific
  model exports must keep to OSI/GPL-3.0-compatible licenses per ADR 0004.

## Candidate workloads (ranked by fit)

| Rank | Workload | Engine today | NPU path | Fit | Primary benefit |
|------|----------|--------------|----------|-----|-----------------|
| 1 | **Wake word** (openWakeWord) | ONNX / `ort` | ORT OpenVINO EP | excellent | battery (always-on, off-CPU) |
| 2 | **Whisper encoder** | whisper.cpp / GGML | `WHISPER_OPENVINO` encoder | excellent | CPU-offload + power; fixed shape, no bucketing |
| 3 | **Kokoro / Piper TTS** | ONNX / `ort` | ORT OpenVINO EP | good | CPU free while speaking; power |
| 4 | **Neural VAD (Silero)** — *when it lands* | (planned) ONNX | ORT OpenVINO EP | fair | battery; but recurrent + tiny → small win |
| — | **Assistant / polish LLM** | llama.cpp / GGML | — | **poor — excluded** | bandwidth-bound autoregressive decode; GGML can't target OpenVINO; iGPU/CPU better |

Rationale detail lives in the chat analysis of 2026-07-12; the short version:
the NPU rewards static-shape feed-forward CNN/transformer-encoder graphs run
continuously or once-per-utterance, and punishes dynamic autoregressive decode.

## Phase A — Investigation spike (NO Fono changes)

External harnesses on a scratch machine; zero Fono code changes.

- [ ] Task A1. Confirm the ORT **OpenVINO Execution Provider** story for Fono's
      pinned onnxruntime (2.0.0-rc.12 / ort-sys, ADR 0032): is the EP available
      as a **dynamically-loaded shared provider**
      (`libonnxruntime_providers_openvino.so`) that `dlopen`s system OpenVINO
      without touching the default minimal static `libonnxruntime.a`? Document
      the exact build recipe and its disk footprint (shared lib only, must stay
      out of the default binary).
- [ ] Task A2. **Wake word on NPU** — run the shipped openWakeWord graphs
      (melspectrogram → `speech_embedding` backbone → per-phrase classifier)
      through OpenVINO on `NPU` vs `CPU`. Measure: per-hop latency, **continuous
      power draw** (RAPL/`turbostat` for CPU package vs NPU), and CPU utilisation
      at the 80 ms hop cadence (`HOP_SAMPLES`). This is the headline experiment.
- [ ] Task A3. **Whisper encoder on NPU** — build a `WHISPER_OPENVINO` whisper.cpp
      and export an OpenVINO encoder for the default model tier. Measure encoder
      wall-clock and power NPU vs CPU; confirm the decoder stays GGML/CPU and the
      end-to-end transcript is unchanged.
- [ ] Task A4. **Kokoro/Piper on NPU** — quantify the static-shape friction:
      pick a small set of padded token-length buckets, compile per bucket, cache
      via EXPORT_IMPORT, and measure first-inference compile cost, warm latency,
      power, and any CPU-fallback subgraphs. Compare against current CPU `ort`.
- [ ] Task A5. **Portability survey** — how the same story looks on AMD XDNA
      (Ryzen AI, via OpenVINO/Vitis or ONNX EP), Apple Neural Engine (CoreML EP,
      not OpenVINO), and Windows (DirectML / OpenVINO). Establishes whether an
      NPU abstraction is worth generalising or should stay Intel-only v1.
- [ ] Task A6. **Model-format + license audit** — for every "go" candidate,
      confirm an INT8/FP16 static-shape export exists or is producible under a
      GPL-3.0-compatible license (ADR 0004), and estimate the extra download
      size (NPU-specific blobs are separate from the CPU models).
- [ ] Task A7. Write the **go/no-go memo** into this file: per-candidate table of
      power delta (mW / mWh-per-utterance), CPU-time freed, latency delta,
      compile/cache cost, added download size, and integration effort. Suggested
      "go" bar for the wake word: **≥ 40 % lower package power** during idle
      listening at no latency regression, with zero impact on the default binary.

## Phase B — Integration (gated on a Phase A "go", per candidate)

Only the candidates that clear the Phase A bar proceed, wake word first.

- [ ] Task B1. Add an **opt-in build variant / feature flag** (e.g. `accel-npu`
      / `openvino`) that links the ORT OpenVINO EP shared provider. Prove the
      default build's binary size and `NEEDED` allowlist are byte-for-byte
      unchanged (`./tests/check.sh --size-budget`).
- [ ] Task B2. **Runtime device detection + fallback** helper: enumerate
      devices, prefer NPU, fall back to CPU cleanly when OpenVINO or the device
      is absent (failed `dlopen` must not crash or slow startup). Wire the
      chosen device into `hardware_acceleration_summary()` and `fono doctor`.
- [ ] Task B3. Route the winning workload(s) through the EP with disk-cached
      compiled blobs (EXPORT_IMPORT), static-shape/bucketing as needed, and a
      per-session log line naming the device actually used.
- [ ] Task B4. Packaging: document the OpenVINO + NPU-driver **system
      dependencies** in `docs/providers.md` and the SlackBuild `REQUIRES=`
      (NimbleX rule: document, do not auto-install). Note the Ubuntu equivalents
      (`intel-driver-compiler-npu`, `intel-level-zero-npu`, `intel-fw-npu`,
      `level-zero`) for cross-distro users.
- [ ] Task B5. Author an **ADR** capturing the decision, the opt-in/dlopen
      architecture, the size-budget guarantee, and the detect-and-fallback
      contract.

## Out of scope

- The assistant/polish LLM (excluded above).
- The Intel iGPU via OpenVINO (needs the NEO/IGC compute-runtime stack; separate
  question, and GGML-Vulkan is the simpler route to the Arc iGPU regardless).
- Making OpenVINO part of the default binary (explicitly forbidden — opt-in only).

## Open questions

- Does the pinned ort/ort-sys expose the OpenVINO EP cleanly, or does it force a
  full (non-minimal) onnxruntime build for the opt-in variant? (Task A1 decides
  whether the ONNX-path candidates are even practical for us.)
- Is the always-on wake-word power win large enough to justify a system-dep,
  Intel-only opt-in variant — i.e. does anyone actually run idle-listening long
  enough for it to matter? (Task A2/A7.)
- Cross-vendor: is one NPU abstraction worth building, or is this permanently a
  niche Intel-only opt-in? (Task A5.)
