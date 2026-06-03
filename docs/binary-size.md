# Keeping Fono small and capable

Fono ships as a **single statically-linked binary** (ADR 0005) that grows
into a **full local voice stack** — STT, LLM cleanup, text-to-speech,
wake-word, neural VAD, streaming recognition — without bloating the
download or pulling in system libraries. Those two goals pull against each
other, so the project enforces an explicit size-and-capability discipline.
This document is the single place that explains how it all fits together.
Read it before adding any dependency, model, or inference runtime.

The authoritative *decisions* live in the ADRs; this guide ties them into
one engineering picture:

- **ADR 0005** — single static binary distribution.
- **ADR 0018** — the `--allow-multiple-definition` ggml link trick.
- **ADR 0022** — the binary-size budget and the `NEEDED` allowlist gate.
- **ADR 0032** — ONNX Runtime as the voice-stack platform.
- **ADR 0033** — TTS engine routing, `.ort` voice distribution, embedded
  phoneme data.

## The two hard invariants

Everything below serves two non-negotiable invariants, enforced in CI
(`tests/check.sh --size-budget`, and the `size-budget` job in
`.github/workflows/ci.yml`):

1. **Size budget.** The canonical `cpu` artefact has a hard byte cap;
   1 byte over fails the build. Current caps (ADR 0022):
   - `cpu`: **≤ 32 MiB** (was 20 MiB; raised for the ONNX voice stack,
     re-measured after the minimal build + Piper landed — the minimal
     ONNX runtime adds only ~2.1 MiB, so headroom is generous, and again
     after the ggml dedup offset).
   - `gpu` (Vulkan): **≤ 64 MiB**.
2. **`NEEDED` allowlist.** `readelf -d` on the shipped binary must list
   **only** the universal glibc + libgcc ABI present on every desktop
   Linux since ~2018 — and nothing else:
   - `cpu`: `{ libc.so.6, libm.so.6, libgcc_s.so.1, ld-linux-x86-64.so.2 }`
   - `gpu`: the above **+ `libvulkan.so.1`**

   Any other entry (libgtk, libstdc++, libonnxruntime, libgomp,
   libasound, …) fails the gate. Engine code is **compiled into the
   binary**, never shipped as a companion `.so`.

If you are about to add something that would break either invariant,
stop and read the relevant ADR — the answer is almost always "link it
statically" or "build only what we use," not "raise the cap."

## What's allowed to grow, and what isn't

- **Model weights are never in the binary.** Whisper, Qwen, Piper,
  Kokoro, Silero, Zipformer, KWS — all download at runtime via
  `fono-download` (SHA-256-pinned, range-resume). The binary carries
  *engine code only*. Because the minimal ONNX runtime loads **`.ort`**
  (not `.onnx`) and no public hub hosts `.ort`, voices come from Fono's
  own SHA-256-pinned `.ort` mirror (GitHub Releases / HF `fono-voice`),
  fetched on demand keyed off `general.languages` (ADR 0033).
- **One small exception: embedded espeak G2P data (~102 KiB).** Piper's
  pure-Rust phonemizer needs a *shared* phoneme set that is the same for
  every language, so it is embedded in the binary via `include_bytes!`
  rather than downloaded: `phontab` + `phonindex` + `intonations` + an
  **8-byte `phondata` stub** (version magic `0x01_48_01` + sample rate).
  The full `phonemes` crate is 2.3 MiB on disk, but its 554 KB spectral
  `phondata` body is used only by espeak's own synthesizer — never the
  text→IPA path (verified 2026-05-31: an 8-byte stub yields byte-identical
  IPA). Embed it **raw, no decompressor** — a runtime decoder
  (`lzma-rs`/`ruzstd`) costs more code than the ~37 KiB it would save
  (ADR 0033). Per-language espeak **dicts** (en 106 KiB, ro 38 KiB; the
  Russian `ru_dict` is a 4.5 MiB outlier) download on demand with the
  voice; the stub must be regenerated if `VERSION_PHDATA` changes.
- **Capabilities ride shared runtimes.** Fono runs exactly **two**
  inference runtimes, split along the GPU boundary (see below). Adding a
  voice feature should mean *wiring another model into an existing
  runtime*, not linking a third one.

## Runtime split: ggml vs ONNX

| Runtime | Linked via | Workloads | Acceleration |
|---|---|---|---|
| **ggml** | `whisper-rs` + `llama-cpp-2` | Whisper STT, LLM cleanup | CPU + **Vulkan** (`gpu` variant) |
| **ONNX Runtime** | `ort` (static) | Piper/Kokoro TTS, Silero VAD, Zipformer streaming STT, KWS wake-word | **CPU only** (XNNPACK) |

The split is deliberate (ADR 0032): only whisper-large and the LLM are
GPU-hungry, and ONNX has **no Vulkan EP**, so the voice stack stays CPU
(it is all CPU-realtime). ggml keeps Vulkan; ONNX never touches it.
Whisper STT may migrate to ONNX later, but that is optional.

## The size levers (in priority order)

### 1. Build only what we use — minimal ONNX Runtime

The off-the-shelf `ort` prebuilt is the **full** onnxruntime (every
operator, every execution provider) — measured at **~19 MiB** of
`.text`+data. Fono runs a small, fixed model set, so we ship a **custom
minimal build** tuned to exactly our operators:

```sh
build.sh --config MinSizeRel --build_shared_lib \
  --minimal_build \
  --include_ops_by_config <ops.config generated from our ORT-format models> \
  --enable_reduced_operator_type_support \
  --disable_ml_ops --disable_exceptions --disable_rtti \
  --skip_tests
```

- Models are converted to **ORT format**; `ops.config` is generated from
  that set and lists exactly the operators + types to keep.
- This is the same path ONNX Runtime Mobile uses (mobile minimal builds
  land ~5–7 MiB). **Measured 2026-05-31: the minimal build adds only
  ~2.1 MiB** to a release binary for the 10-operator Piper VITS op set
  — far below the ~7–11 MiB estimate. The minimal `libonnxruntime.a` is
  ~50 MiB on disk, but `--gc-sections` discards everything the fixed op
  set never references, so only ~2 MiB actually links in.
- **Measured 2026-06-03: adding Kokoro (q8f16) to the union op set costs
  only ~0.77 MiB more.** The runtime now also registers Kokoro's net-new
  operators (LSTM, STFT, LayerNormalization, Atan, Cos, Sin, plus the
  q8f16 quant kernels — ConvInteger, DynamicQuantize{Linear,LSTM,MatMul},
  MatMulInteger{,ToFloat}, DequantizeLinear, SkipLayerNormalization). The
  union `libonnxruntime.a` is ~50.4 MiB on disk (up from ~50.3 MiB Piper-
  only — the operator delta is dwarfed by shared infrastructure), and a
  `release-slim --features tts-local` glibc binary links in at **25.22 MiB**
  (up from the 24.45 MiB Piper-only baseline below), still well under the
  32 MiB `cpu` cap with the four-entry `NEEDED` allowlist intact.
- The resulting static `libonnxruntime.a` is built in CI and pinned via
  the `ORT_LIB_LOCATION` env var, which turns off `ort`'s
  `download-binaries` (so builds are reproducible/offline and no
  `libonnxruntime.so` can sneak into `NEEDED`).

> **Standing discipline:** every new model added to the voice stack
> **must regenerate `ops.config`** so the operator set tracks what we
> actually ship. A model that needs an operator not in the build will
> fail to load — that is the signal to regenerate, not to switch back to
> the full runtime.

### 2. Static C++/runtime linkage — protect the `NEEDED` allowlist

C/C++ dependencies (ggml, onnxruntime) want to pull `libstdc++.so.6`,
`libgomp.so.1`, etc. dynamically. Fono forces them static:

- `libstdc++` / `libgomp` go static via the `llama-cpp-2/static-stdcxx`
  and `llama-cpp-2/static-openmp` features, which make the sys crate emit
  both a `libstdc++.a` search path (from `gcc --print-file-name`) and
  `static=stdc++` / `static=gomp` on the final link line.
- `ort-sys` emits its own C++ stdlib link directive, driven by the
  `ORT_CXX_STDLIB` env var (passed through verbatim into
  `cargo:rustc-link-lib=<value>`). A plain `static=stdc++` makes rustc try
  to *bundle* `libstdc++.a` into the `ort-sys` rlib at its own compile
  time — where no search path is visible (the path emitted by a sibling
  build script does not reach an already-compiling crate), so the build
  fails with "could not find native static library `stdc++`". The fix is
  `ORT_CXX_STDLIB="static:-bundle=stdc++"`: the `-bundle` modifier defers
  the static archive to the **final `fono` link**, where llama's (and
  `fono-tts`'s own) `libstdc++.a` search path is present. Set in
  `.cargo/config.toml [env]`; `crates/fono-tts/build.rs` (feature-gated on
  `tts-local`) emits the matching `rustc-link-search` so the archive
  resolves regardless of whether llama is in the build.
- Verified 2026-06-01: a plain `cargo build -p fono --profile
  release-slim --features tts-local` (only `ORT_LIB_LOCATION` set, no
  manual `RUSTFLAGS`) yields **24.45 MiB** with exactly the four-entry
  allowlist — both `libstdc++` and `libonnxruntime` statically embedded.
  This is ~0.9 MiB *smaller* than leaving `libstdc++` dynamic, because
  `--gc-sections` prunes the unreferenced bulk of the 6.3 MiB archive.
- Linker flags live in `.cargo/config.toml`: `--gc-sections`,
  `--as-needed`, and (legacy) `--allow-multiple-definition`.

### 3. Dead-code elimination

`-Os -ffunction-sections -fdata-sections` on the C/C++ side +
`-Wl,--gc-sections` on the link drops unused arch kernels and helpers.
`GGML_NATIVE=OFF` pins the ISA baseline (see `.cargo/config.toml` for the
full rationale). The `release-slim` profile sets `strip = "symbols"`,
LTO, and `opt-level` for size.

### 4. Deduplicate ggml — the offset for the ONNX addition

Today `whisper-rs-sys` and `llama-cpp-sys-2` each vendor their **own**
copy of ggml; the `--allow-multiple-definition` trick (ADR 0018) keeps
one and discards the duplicate, wasting **~7 MiB**. **Source-shared ggml**
(ADR 0022 Phase 1 Task 1.2) patches both sys crates to compile against
one shared ggml build, reclaiming that ~7 MiB and retiring the link
trick.

This is **scheduled after Piper ships, specifically to offset the ONNX
addition** — it is no longer a prerequisite for anything, just the
counterweight that keeps the `cpu` cap honest. The blocker is real
engineering: the two crates currently track *different* ggml revisions
(77-line `ggml.h` drift, measured 2026-05-31), so it needs a
`whisper-rs-sys` fork + ABI reconciliation + a pinned remote.

## Adding a new capability: the checklist

Before merging a feature that adds a model, runtime, or dependency:

1. **Does it need a new inference runtime?** Almost certainly not — wire
   the model into ggml or ONNX. A third runtime needs an ADR.
2. **New ONNX model?** Add it to the ORT-format set and **regenerate
   `ops.config`**; confirm the minimal build still loads it.
3. **New C/C++ dep?** Confirm it links statically and adds nothing to
   `NEEDED`. Update `deny.toml` and verify GPL-3.0 license compatibility.
4. **New model weights?** They download at runtime via `fono-download`
   (SHA-256-pinned) — never bundled. Check the license (ADR 0004: no
   Llama/Gemma defaults; OSI/GPL-compatible only).
5. **Run the gate:** `tests/check.sh --size-budget` must pass — size
   under cap, `NEEDED` within the allowlist.
6. **If the cap genuinely must rise,** amend ADR 0022 with the measured
   number and the justification. Raising the cap is a last resort, not a
   default.

## Quick reference

- Size/`NEEDED` gate: `tests/check.sh --size-budget`,
  `.github/workflows/ci.yml` `size-budget` job.
- Link flags: `.cargo/config.toml`.
- Size profile: `[profile.release-slim]` in `Cargo.toml`.
- Decisions: ADR 0005 (static binary), ADR 0018 (ggml link trick),
  ADR 0022 (budget + allowlist), ADR 0032 (ONNX voice-stack runtime),
  ADR 0033 (TTS routing + `.ort` distribution + embedded phoneme data).
