# Fono: Single Static Binary with SOTA Local Whisper + Llama via Shared `ggml`

## Status: Superseded

This plan was never executed. The lighter-touch
`-Wl,--allow-multiple-definition` link trick at
`.cargo/config.toml:21-28` ships the same single-binary outcome without
the work of forking the two sys crates onto a shared `ggml`. See
`docs/status.md:276-310` and `docs/decisions/0018-ggml-link-trick.md`
for the active decision.

This plan is preserved — not deleted — because it remains the
**documented rollback path** if a future linker (lld, ld64, or a
hardened gold) ever stops honouring `--allow-multiple-definition`. If
that day comes, this plan is the escape hatch: drop the duplicate ggml
sources and link against a single shared copy.

## Objective

Ship Fono as a single, statically-linked binary that bundles both `whisper.cpp` (STT) and `llama.cpp` (LLM cleanup) at full upstream performance, with hardware acceleration (CPU SIMD, Vulkan, CUDA, Metal) auto-detected and selected at runtime. No system dependencies, no companion `.so` files, no extracted helper binaries. Distro packages are kept as a marginal convenience but are not the primary distribution channel — `target/release/fono` is the ship vehicle.

## Product Constraints (from user feedback — non-negotiable)

- **Single static binary** is the headline distribution. One ELF the user downloads, `chmod +x`, runs.
- **SOTA performance** — Rust-native frameworks that lag `llama.cpp` are not acceptable.
- **Hardware acceleration out of the box** when the host has a capable GPU/SIMD/NPU; CPU fallback when not.
- **Self-contained**: no external libraries to install, no sidecar processes, no runtime extraction tricks.
- **Auto-download models** to `~/.cache/fono/models/` on first use, SHA-256 verified, mirror-overridable.
- **Latest model formats supported** (GGUF universal).
- One backend ships in production. Other backends exist only as dev-time benchmark controls.

## Strategic Approach

### The Root Cause Re-Stated
Both `whisper-rs-sys` and `llama-cpp-sys-2` vendor `ggml` and produce static archives that each define `ggml_init`, `ggml_compute_*`, etc. The linker sees multiple definitions → fatal error. The fix is **not** to make Fono use one engine — it is to make both engines use the **same** statically-built `ggml`.

### Why This Is Actually Tractable
1. `llama-cpp-sys-2` upstream already exposes `system-ggml-static` and `system-ggml` features (verified in its `Cargo.toml`) precisely for this scenario.
2. `whisper.cpp`'s CMake build accepts `WHISPER_USE_SYSTEM_GGML=ON` (or equivalent `-DGGML_PROVIDED_EXTERNALLY=ON`) to skip building its bundled `ggml` and link against an existing one.
3. Both projects are by the same author and the `ggml` API has been stable across the relevant versions.
4. `ggml` 2024+ implements **runtime backend selection**: one static binary can contain CPU/AVX2/AVX512/Vulkan/CUDA/Metal kernels simultaneously and pick the best on launch. This delivers "hardware acceleration out of the box" without per-host build variants.

### Distribution Model
- **Primary:** `target/release/fono` — single static-musl ELF, ~50–80 MB, runs on any glibc-or-musl Linux ≥ 3.2. Attached to GitHub Releases, downloaded directly.
- **Marginal convenience:** `.deb`, `.txz`, `.lzm`, `.rpm` wrappers around the same ELF + a `.desktop` file + an optional systemd user unit. They install the same binary at `/usr/bin/fono`. No private `.so` files.
- **Per-target GPU SIMD variants** (CPU+CUDA only, CPU+Metal only) ship as additional ELFs for users who want a smaller binary or know their hardware. Default ELF carries everything.

## Implementation Plan

### 1. Shared `ggml` Build Infrastructure
- [ ] Task 1.1. Create a new internal workspace crate `crates/fono-ggml-sys` whose sole job is to build `ggml` once from a vendored source (pinned commit) with CPU + optional Vulkan/CUDA/Metal backends, producing `libggml.a` exposed to the rest of the build via Cargo's `links =` and `cargo:rustc-link-lib=` machinery.
- [ ] Task 1.2. The crate's `build.rs` accepts cargo features `vulkan`, `cuda`, `metal`, `openblas`, `openmp`. Default = CPU multi-arch (x86_64 with AVX/AVX2/AVX512 runtime detection, aarch64 with NEON).
- [ ] Task 1.3. Pin the `ggml` source to a commit known to be ABI-compatible with both the `llama-cpp-2` and the `whisper.cpp` versions we depend on. Document the bump procedure in the crate's README.
- [ ] Task 1.4. Verify the produced `libggml.a` exports the `ggml_backend_*` runtime-selection API and that `ggml_backend_load_best()` (or its current equivalent) is callable from C.

### 2. `whisper-rs` Integration
- [ ] Task 2.1. Audit `whisper-rs-sys` upstream for a `system-ggml` (or `external-ggml`) cargo feature; if present, enable it. If absent, prepare a minimal patch.
- [ ] Task 2.2. If a patch is needed: fork `whisper-rs` to a `crates/fono-whisper-sys` vendored copy or a published `fono-whisper-rs` crate. Modify its `build.rs` to set `WHISPER_USE_SYSTEM_GGML=ON` (or pass `-DGGML_USE_EXTERNAL=ON` cmake flag), and replace the `cargo:rustc-link-lib=ggml` self-reference with a dependency on `fono-ggml-sys`. Submit the patch upstream as a PR; track via `[patch.crates-io]` until merged.
- [ ] Task 2.3. Update `crates/fono-stt/Cargo.toml` to depend on the shared-ggml whisper crate.
- [ ] Task 2.4. Smoke test: `cargo build -p fono-stt` produces no `libggml.a` of its own; `nm` on the staticlib shows `ggml_*` symbols come from `fono-ggml-sys` only.

### 3. `llama-cpp-2` Integration
- [ ] Task 3.1. In `crates/fono-llm/Cargo.toml`, switch the `llama-cpp-2` dep to enable `llama-cpp-sys-2/system-ggml-static`. Drop the optional gating — local LLM becomes always-on.
- [ ] Task 3.2. Verify `llama-cpp-sys-2`'s build script accepts the system-ggml linkage by setting `LLAMA_GGML_LIB_DIR` (or its current env-var equivalent) to point at `fono-ggml-sys`'s output directory. May require a small `build.rs` tweak in `fono-llm` to bridge the two crates.
- [ ] Task 3.3. Smoke test: `cargo build -p fono-llm` succeeds with both `whisper-rs` and `llama-cpp-2` linked into the same staticlib, and `nm fono | grep -c '^.*T ggml_init$'` reports exactly **1**.

### 4. Top-Level Binary Wiring
- [ ] Task 4.1. In `crates/fono/Cargo.toml`, drop the `local-models` and `llama-local` cargo features — both backends are unconditional. Default features become `["tray"]` (and the cloud-feature set).
- [ ] Task 4.2. In `crates/fono/src/lib.rs`, delete the `compile_error!` block. Both backends coexist by design.
- [ ] Task 4.3. Verify `cargo build --release --target x86_64-unknown-linux-musl` produces a single ELF, `ldd` reports "not a dynamic executable", and `file` reports static-pie.

### 5. Runtime Hardware Acceleration
- [ ] Task 5.1. On daemon startup, call `ggml`'s runtime backend enumeration API and log the available backends (CPU+AVX2, Vulkan-on-NVIDIA, CUDA, Metal, etc.) at `info` level so users see what's active.
- [ ] Task 5.2. Implement a config knob `[performance].llm_backend = "auto" | "cpu" | "vulkan" | "cuda" | "metal"` that lets advanced users override the auto-pick. Default `auto` selects the fastest available backend; `cpu` forces CPU-only for debug.
- [ ] Task 5.3. Same knob mirrored for STT (`[performance].stt_backend = "auto" | …`). Both backends share the `ggml` runtime so they see the same accelerator menu.
- [ ] Task 5.4. Surface the active backend in the tray menu and `fono doctor` output: `local LLM backend: vulkan (RTX 4070)` / `local STT backend: cpu (AVX2)`.

### 6. Build Variants & Release Pipeline
- [ ] Task 6.1. Default build features for releases: `[ "tray", "cloud-all", "ggml-vulkan", "ggml-openmp" ]`. Vulkan covers NVIDIA + AMD + Intel + Apple-via-MoltenVK; CPU+OpenMP is the safe baseline. This single binary is the canonical download.
- [ ] Task 6.2. Optional GPU-specific variants for users who know their hardware: `fono-cuda` (NVIDIA-only, smaller), `fono-metal` (Apple-only, smaller). Built by extending `.github/workflows/release.yml` with extra matrix entries.
- [ ] Task 6.3. Confirm the static-musl build picks up the `vulkan-loader` headers but does **not** statically link `libvulkan.so.1` — Vulkan ICDs are loaded at runtime via `dlopen` on `libvulkan.so.1` in the user's `LD_LIBRARY_PATH`, with a graceful CPU fallback when Vulkan is absent. (`ggml`'s Vulkan backend already does this; verify the build doesn't add a hard `NEEDED` entry.)
- [ ] Task 6.4. Strip + UPX-compress the release ELF? Test whether UPX trips antivirus heuristics; if so, ship un-compressed.

### 7. Auto-Download UX (unchanged from previous plan, reaffirmed)
- [ ] Task 7.1. Wizard auto-selects model based on `HardwareSnapshot::probe()` tier without asking. Tier→model table lives in `fono-llm/src/registry.rs` and `fono-stt/src/registry.rs`, refreshed quarterly.
- [ ] Task 7.2. On first dictation when the configured GGUF is missing, auto-download with progress shown in tray + system notification, blocking dictation until complete (queue any in-flight hotkey events). Reuses `fono-download`.
- [ ] Task 7.3. Honour `FONO_MODEL_MIRROR` env var for users behind restrictive networks.
- [ ] Task 7.4. Pin SHA-256 hashes for: Qwen2.5-1.5B-Instruct, Qwen2.5-3B-Instruct, Qwen2.5-7B-Instruct, SmolLM2-360M, SmolLM2-1.7B, Whisper small/medium multilingual + their `.en` variants.

### 8. Benchmark Suite (Dev-Only)
- [ ] Task 8.1. Add `candle-core`, `candle-transformers` as **dev-dependencies only** in `crates/fono-bench/Cargo.toml`. Production builds never see candle.
- [ ] Task 8.2. Implement `CandleBaseline: TextFormatter` in `crates/fono-bench/src/candle_baseline.rs` (not in `fono-llm`) — exists solely as a benchmark control.
- [ ] Task 8.3. Create `crates/fono-bench/benches/llm_compare.rs` Criterion benchmark: load identical Qwen2.5-1.5B GGUF into both `LlamaLocal` (production) and `CandleBaseline` (control), measure tokens/sec on a fixed cleanup prompt. Assert `llama.cpp` p50 ≤ `candle` p50 / 1.3 — CI gate that prevents future regressions where someone "simplifies" by switching backends.
- [ ] Task 8.4. Create `crates/fono-bench/benches/backend_compare.rs` Criterion benchmark: run `llama.cpp` against the same model on each available `ggml` backend (CPU, Vulkan, CUDA where applicable). Track tokens/sec per backend and emit a regression-comparable JSON report.
- [ ] Task 8.5. Add a quarterly review checklist item in `docs/status.md`: re-run all benchmarks, re-evaluate whether `mistral.rs` or future Rust-native frameworks have closed the gap. The benchmark is the decision document.

### 9. Documentation
- [ ] Task 9.1. Update `docs/plans/2026-04-24-fono-design-v1.md` Phase 9 to reaffirm the static-musl single-binary primary distribution.
- [ ] Task 9.2. New ADR `docs/decisions/0009-shared-ggml-static.md` capturing this decision, the alternatives weighed (`candle`, `mistral.rs`, `ort`, `dynamic-link`, sidecar, symbol prefixing), and the SOTA-performance + single-binary rationale.
- [ ] Task 9.3. Mark `docs/decisions/0008-llama-local-deferred.md` as superseded by 0009.
- [ ] Task 9.4. Update `README.md` install snippets: emphasise `curl -L … -o fono && chmod +x fono` as the canonical install. Distro packages mentioned as secondary.

## Verification Criteria

- `cargo build --release --target x86_64-unknown-linux-musl` produces `target/x86_64-unknown-linux-musl/release/fono` with no `target/.../libllama.so` or `libggml.so`. `ldd target/.../fono` reports "not a dynamic executable" or only `linux-vdso.so.1`.
- `nm target/.../fono | grep -c '^.*T ggml_init$'` returns exactly **1**.
- `cargo test --workspace` passes — the smoke test loads a whisper model and runs a llama inference in the same process with no segfault and no symbol-clash panic.
- On a host with a Vulkan-capable GPU, `fono doctor` reports `local LLM backend: vulkan` and `local STT backend: vulkan`. `fono` produces a 100-token cleanup in ≤ 1.5 s on Qwen2.5-1.5B-Q4_K_M.
- On a CPU-only host, `fono doctor` reports `local … backend: cpu (AVX2)`. The same cleanup completes in ≤ 8 s on a 4-core box (the existing latency target).
- `cargo bench -p fono-bench --bench llm_compare` reports `llama_cpp` p50 ≤ `candle` p50 / 1.3 on the reference machine.
- Single binary size ≤ 80 MB stripped (with Vulkan + OpenMP + cloud HTTP); ≤ 50 MB for cloud-only build.
- First-run on a fresh `HOME` triggers the wizard, auto-picks Qwen2.5-1.5B, auto-downloads with progress UI, verifies SHA-256, and produces the first cleaned dictation without manual config editing.

## Potential Risks and Mitigations

1. **`whisper-rs-sys` upstream rejects or stalls the `system-ggml` PR.**
   *Mitigation:* Maintain a minimal in-tree fork at `crates/fono-whisper-sys` with the patch applied. The fork is small (build.rs tweak + a flag) and trivial to rebase. Submit the PR as goodwill but don't block on merge.

2. **`ggml` API drift between the `whisper.cpp` and `llama.cpp` versions we want.**
   *Mitigation:* `fono-ggml-sys` pins the `ggml` source at a commit chosen to satisfy both consumers' minimum API. When a consumer wants a newer `ggml`, the bump is one PR with a re-run of the smoke + benchmark suite.

3. **Vulkan loader missing at runtime on minimal Linux installs.**
   *Mitigation:* `ggml`'s Vulkan backend already weak-`dlopen`s `libvulkan.so.1`. If absent, the runtime backend probe just doesn't expose Vulkan, and CPU is selected. Verified by smoke-testing the binary in an Alpine container with no `vulkan-loader`.

4. **Static-musl + CUDA = NVIDIA's proprietary toolchain doesn't fully static-link.**
   *Mitigation:* Default release variant ships **without** CUDA. Vulkan covers NVIDIA via the open Mesa/proprietary driver Vulkan ICD, which is dynamically loaded at runtime — works on every modern NVIDIA driver. CUDA-specific variant (`fono-cuda`) is offered only as a glibc dynamic build for users who specifically want it.

5. **Binary size balloons past 100 MB once Vulkan + OpenMP + multi-arch CPU kernels stack.**
   *Mitigation:* Profile with `cargo bloat` and `bloaty` after the first integration. If above 100 MB, drop multi-arch CPU kernels (assume AVX2 baseline; ship a separate `fono-portable` for pre-AVX2 hosts) and consider UPX compression.

6. **`ggml` runtime backend selection misclassifies the host (picks Vulkan on a slow integrated GPU when CPU+AVX2 would be faster).**
   *Mitigation:* `[performance].llm_backend = "cpu"` override + tray menu toggle. `fono doctor` reports a one-line micro-bench result so users can confirm the auto-pick is right.

7. **The combined ELF needs `libstdc++` even with musl.**
   *Mitigation:* Build `ggml`, `whisper.cpp`, and `llama.cpp` with `-static-libstdc++ -static-libgcc` flags. Verified via `ldd`.

## Alternative Approaches (rejected and why)

1. **`dynamic-link` + bundled `libllama.so`** (previous plan revision). Rejected per user direction: violates the single-static-binary headline.

2. **Symbol prefixing via `objcopy --redefine-sym` on `whisper-rs-sys`.** Rejected: ships **two** copies of `ggml` in the binary (one for whisper, one for llama), each potentially picking different runtime backends, leading to confusing behaviour and ~2× the GPU memory footprint. Larger binary, harder to reason about.

3. **`mistral.rs` as the production backend.** Rejected: ~80–95% of `llama.cpp` perf and ~100 transitive deps. Fails the SOTA bar and the lightweight bar.

4. **`candle` as the production backend.** Rejected: 50–70% of `llama.cpp` perf on CPU. Fails the SOTA bar.

5. **`ort` (ONNX Runtime) as the production backend.** Rejected: ONNX Runtime as a static library is ≥ 50 MB on its own and the GGUF ecosystem is bigger than the ONNX ecosystem for new model releases. Worth re-evaluating in v0.2 alongside Silero VAD.

6. **Sidecar binary (extract-and-spawn `fono-llm-runner`).** Rejected: extracting executables at runtime is a single-binary violation (the user-visible artifact is one ELF, but it spawns a second), and trips antivirus heuristics on Windows/macOS.

7. **Drop `whisper-rs` for sherpa-onnx.** Rejected for v0.1: invalidates the existing whisper model registry, breaks the `fono-stt` factory, and defers SOTA STT (whisper.cpp is itself SOTA on CPU). Worth re-evaluating in v0.3 alongside diarization.