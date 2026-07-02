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
   - `cpu`: **≤ 28 MiB** hard cap (was 20 MiB, then ≤ 32 MiB, then
     ≤ 30 MiB; lowered to 28 MiB on 2026-07-01 when `release-slim` adopted
     `opt-level = "s"`). The **enforced gate row is 25 MiB**
     (26 214 400 B) — see `.github/workflows/ci.yml`; the ~3 MiB gap to the
     hard cap is deliberate ceiling.
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
  (up from the 24.45 MiB Piper-only baseline below), comfortably within the
  `cpu` cap with the four-entry `NEEDED` allowlist intact. (These figures
  predate the 2026-07-01 `opt-level = "s"` switch — see §2 — which cut the
  shipped binary to 21.64 MiB.)
- The resulting static `libonnxruntime.a` is built in CI and pinned via
  the `ORT_LIB_LOCATION` env var, which turns off `ort`'s
  `download-binaries` (so builds are reproducible/offline and no
  `libonnxruntime.so` can sneak into `NEEDED`).

> **Standing discipline:** every new model added to the voice stack
> **must regenerate `ops.config`** so the operator set tracks what we
> actually ship. A model that needs an operator not in the build will
> fail to load — that is the signal to regenerate, not to switch back to
> the full runtime.

### 2. Rust codegen — optimize for size (`opt-level = "s"`)

The `release-slim` profile sets `opt-level = "s"` (`Cargo.toml`), telling
LLVM to optimise Rust codegen for **size, not speed**. This is one of the
largest single levers: measured 2026-07-01 on `x86_64-unknown-linux-gnu`
(default features), it drops the shipped binary from **26.60 MiB to
21.64 MiB (−4.96 MiB)** and `.text` from 21.47 MiB to 16.65 MiB, with the
four-entry `NEEDED` allowlist intact.

The saving is **duplicated Rust machine code, not features**. Fono's
async-heavy glue (tokio/reqwest/serde, the `ort` / `whisper-rs` /
`llama-cpp-2` bindings, the provider/realtime/streaming code) is heavily
generic, so `opt-level = 3` monomorphises it and then aggressively
inlines and unrolls each copy. `"s"` keeps the real optimisations but
suppresses the size-inflating ones.

**Inference speed is unaffected.** The compute-bound work — Whisper,
llama, ggml, onnxruntime — is C/C++ compiled by `cc`/`cmake` with its own
flags (`-Os` scaffolding + cmake-gated high-opt vectorised kernels, see
`.cargo/config.toml`); Rust `opt-level` never touches it. `"s"` only
affects the Rust glue, which is I/O- or orchestration-bound and typically
lands within a few percent of `"3"` (often faster via better I-cache
behaviour). Measured ladder (x86_64, default features):

| `opt-level` | Size | Δ vs `3` |
|---|---:|---:|
| `3` (speed, old default) | 26.60 MiB | — |
| `2` (speed) | 25.99 MiB | −0.61 MiB |
| **`"s"` (size, shipped)** | **21.64 MiB** | **−4.96 MiB** |
| `"z"` (min size) | 20.39 MiB | −6.51 MiB |

`2` and `3` are both *speed* settings, so `2` barely helps; the cliff is
the jump to the size family. `"z"` saves ~1.25 MiB more but disables
Rust-side loop vectorisation — a real risk to any Rust numeric loop — so
`"s"` is the shipped sweet spot.

### 3. Static C++/runtime linkage — protect the `NEEDED` allowlist

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

### 4. Dead-code elimination

`-Os -ffunction-sections -fdata-sections` on the C/C++ side +
`-Wl,--gc-sections` on the link drops unused arch kernels and helpers.
`GGML_NATIVE=OFF` pins the ISA baseline (see `.cargo/config.toml` for the
full rationale). The `release-slim` profile sets `strip = "symbols"`,
LTO, and `opt-level` for size.

### 5. Deduplicate ggml — measured ≈ 0 MiB win, deferred

`whisper-rs-sys` and `llama-cpp-sys-2` each vendor their **own** copy of
ggml; the `--allow-multiple-definition` trick (ADR 0018) keeps the first
and discards the duplicate at link time.

**The ~7 MiB once attributed to this duplicate is an archive-size
inheritance, not a section measurement — and it does not survive the
link.** Re-measured 2026-06-24 on the canonical `release-slim`
`x86_64-unknown-linux-gnu` `cpu` artefact (26.60 MiB,
`plans/2026-06-23-shared-ggml-size-reclaim-spike-v1.md`): a non-stripped
relink shows `ggml_init` defined exactly **once**, **zero** duplicated
ggml globals, and single-copy ggml/quant `.text` ≈ 1.03 MiB. The same
`-ffunction-sections -fdata-sections` + `--gc-sections` that prune the
stale `libstdc++` bulk (§3) also collect the loser ggml copy's
per-function sections. **Realised reclaim from a source-level shared ggml
≈ 0 MiB.**

Consequence: **source-shared ggml (ADR 0022 Phase 1 Task 1.2) is
deferred** — it buys no binary size. The only residual cost of the link
trick is build time (ggml compiled twice), which the size budget does not
count. If ever revisited, the front-runner is upstreaming a `system-ggml`
feature: `llama-cpp-sys-2` already ships one, so only a `whisper-rs-sys`
fork/PR (its upstream lives at `codeberg.org/tazz4843/whisper-rs`; the
GitHub mirror is archived) would remain — but the trigger should be a
correctness or build-time motivation, not size. ADR 0018 stays the
documented steady state.

## 0.11.0 size-regression notes (2026-06-19)

CI rejected the 0.11.0 CPU artefact because the x86_64 `release-slim`
binary was **28,033,384 B** against the enforced **27,262,976 B** budget
(**+770,408 B**). A local reproduction of the same profile/target shape
measured **27,952,328 B** for 0.11.0 versus **26,764,488 B** for 0.10.0
(**+1,187,840 B**). The local/CI absolute numbers differ by ~81 KiB, but
the growth shape matches.

Section-level comparison showed the regression is mostly executable code,
not bundled assets or model weights:

| Section | 0.10.0 | 0.11.0 | Delta |
|---|---:|---:|---:|
| `.text` | 20,088,956 B | 21,075,740 B | **+986,784 B** |
| `.rodata` | 2,422,448 B | 2,472,560 B | +50,112 B |
| `.rela.dyn` | 1,243,224 B | 1,286,592 B | +43,368 B |
| `.data.rel.ro` | 742,936 B | 771,608 B | +28,672 B |
| `.eh_frame` | 1,431,456 B | 1,494,612 B | +63,156 B |
| `.eh_frame_hdr` | 221,244 B | 230,540 B | +9,296 B |
| `.gcc_except_table` | 87,236 B | 89,508 B | +2,272 B |

So `.text` accounts for ~83% of the local release-to-release growth, while
the EH/frame-table growth accounts for ~6%.

Unwind/frame experiments on 0.11.0 (temporary worktree only, no committed
code changes) found useful but insufficient headroom:

| Variant | Size | Saving vs local baseline | Budget result |
|---|---:|---:|---:|
| baseline | 27,952,328 B | — | +689,352 B over |
| Rust `-C force-unwind-tables=no` | 27,566,024 B | -386,304 B | +303,048 B over |
| native `-fno-asynchronous-unwind-tables -fno-unwind-tables` | 27,784,648 B | -167,680 B | +521,672 B over |
| Rust + native no-unwind tables | 27,398,344 B | -553,984 B | +135,368 B over |
| `lld --icf=safe` | 27,911,264 B | -41,064 B | +648,288 B over |
| `lld --icf=safe` + Rust no-unwind tables | 27,524,960 B | -427,368 B | +261,984 B over |
| copied binary with `.eh_frame`, `.eh_frame_hdr`, `.gcc_except_table` removed | 26,137,552 B | -1,814,776 B | -1,125,424 B under |

The copied-binary strip is an upper-bound measurement only, not a proposed
shipping change: removing EH sections wholesale may break native C++/OpenMP
exception behavior and crash diagnostics. Disabling C++ exceptions globally
was tested and fails to compile because the `llama-cpp-sys-2` wrapper uses
`try`/`catch`. The safe conclusion is that unwind-table tuning recovers
roughly 0.55 MiB. Together with the new 27 MiB CPU budget (28,311,552 B),
this keeps the 0.11.0 default CPU artefact under the gate while preserving all
features and OpenMP.

A follow-up native experiment removed the `llama-cpp-2` `openmp` /
`static-openmp` features in a temporary checkout (keeping `static-stdcxx` so
the `NEEDED` allowlist stayed unchanged):

| Variant | Size | Saving vs local baseline | Budget result |
|---|---:|---:|---:|
| no OpenMP | 27,743,016 B | -209,312 B | +480,040 B over |
| no OpenMP + Rust no-unwind tables | 27,356,584 B | -595,744 B | +93,608 B over |
| no OpenMP + Rust/native no-unwind tables | 27,188,904 B | -763,424 B | -74,072 B under |

All no-OpenMP variants kept the four-entry `NEEDED` allowlist and started
successfully with `--version`. This shows the llama/OpenMP bucket is real,
but removing OpenMP is a performance trade-off for local LLM cleanup and
assistant replies, not a free size-only tweak. The project kept OpenMP and
instead raised the strict CPU gate to 27 MiB while applying unwind-table
reduction. Future size work should still target actual `.text` growth:
benchmark the local-LLM cost before any OpenMP change, and separately attack
the new async provider/realtime codegen.

### 6. Executable hygiene — hidden exports + GNU-only hash (2026-07-02)

Static archives (libstdc++.a, libgomp.a, libonnxruntime.a, the ggml
archives) can leak their default-visibility symbols into the executable's
dynamic export table. The linux-gnu rustflags in `.cargo/config.toml` add
`-Wl,--exclude-libs,ALL` and `-Wl,--hash-style=gnu` to forbid that.

**Scope caveat (measured 2026-07-02):** the Ubuntu release runners
already produce export-clean (1 symbol), gnu-hash-only binaries —
verified by inspecting the released v0.13.0/v0.13.1 artefacts — so these
flags do **not** shrink what CI ships. Their value is pinning that
behaviour across host toolchains: the NimbleX dev box leaked ~1,011
exports (985 libstdc++ `__cxa_*`/demangler) plus a legacy SysV `.hash`,
inflating *local* artefacts by ~0.9 MiB (gpu: −934,344 B once flagged;
local cpu: 21.82 → 20.92 MiB). With the flags, local size measurements
track CI's, and the invariant no longer depends on distro linker
defaults. Nothing consumes the exports either way (fono is never
dlopen'd). `NEEDED` allowlist unchanged on both variants.

## The `gpu` (Vulkan) variant: where its bytes live

Audited 2026-07-02 (v0.13.0, release-slim, x86_64 glibc, baseline
60,961,144 B = 58.14 MiB):

| Piece | Size |
|---|---:|
| `.rodata` — 1,551 embedded SPIR-V shader blobs | 36.55 MB |
| `.text` | 18.08 MB |
| `.rela.dyn` + `.eh_frame` + `.data.rel.ro` | ~2.9 MB |

Findings, so the next size pass does not re-litigate them:

- **The ggml-vulkan duplicate dedups cleanly.** whisper-rs-sys and
  llama-cpp-sys-2 each generate a full shader set (2,280 / 2,317 blobs);
  the ADR 0018 link trick + `--gc-sections` keep exactly one union
  (1,693 survive, 0 duplicate symbols, 0 byte-identical blobs —
  hash-verified). The surviving Vulkan backend code is whisper's copy.
- **Shader optimisation is already on.** `vulkan-shaders-gen` runs
  `glslc -O` (spirv-opt) on everything except the coopmat/bf16/rope
  shaders, where upstream deliberately disables it to work around driver
  bugs (llama.cpp #10734/#15344/#16860). Do **not** re-enable `-O`
  there — that is a correctness risk, i.e. a capability loss.
- **What *is* safe on those blobs: `spirv-opt --strip-debug`** (removes
  OpName/OpLine/OpSource only, semantics-neutral). The GPU matrix row in
  `.github/workflows/release.yml` shims `glslc` to apply it to every
  generated blob: measured **−785,052 B (−0.75 MiB)** across the
  surviving set. **Post-mortem:** the v0.13.1 release shipped the shim
  but its gpu binary did not shrink (+4,096 B vs v0.13.0) — the
  Swatinem rust-cache reused the pre-shim ggml-vulkan shader objects, so
  the generator (and therefore the shim) never re-ran. Fixed by bumping
  the cache-key suffix (`-shaderstrip1`); any future change to the
  shader toolchain must bump it again.
- **Shader-variant pruning is off the table.** coopmat1/coopmat2
  (13.4 MB) and the MoE `matmul_id_*` family (18.6 MB) are all selected
  at runtime per GPU/model; dropping any of them drops hardware or
  model support.
- **Not adopted:** `opt-level = "z"` on the GPU variant would save a
  further measured 1,231,616 B (−1.17 MiB) but was rejected for the same
  Rust-vectorisation reason as on `cpu` (§2), and per-variant codegen
  divergence is undesirable. RELR packed relocations (~−1.2 MiB of
  `.rela.dyn`) need glibc ≥ 2.36, above the Ubuntu 22.04 (2.35) floor.
- **Future big fish:** compressing the SPIR-V payload at build time and
  inflating once at Vulkan init would cut the 36.5 MB blob set to a
  likely single-digit MB, but needs a ggml patch (ideally upstream) plus
  a decompressor dependency — flag before attempting (new-to-graph dep
  rule).

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
