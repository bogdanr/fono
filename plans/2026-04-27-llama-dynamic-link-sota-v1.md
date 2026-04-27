# Fono: SOTA Local LLM via Dynamic-Linked llama.cpp

## Objective

Adopt `llama-cpp-sys-2`'s `dynamic-link` feature as Fono's single, canonical local LLM backend. Resolve the `ggml` symbol collision with `whisper-rs` by isolating llama.cpp into a private shared library shipped inside Fono's distribution packages. End users get state-of-the-art local-LLM performance with zero build steps, zero system dependencies to install, zero choices to make. Devs/CI keep `candle` only as a benchmark control to validate the chosen backend stays best.

## Product Constraints (from user feedback — non-negotiable)

- End users **never compile** — only devs and AI agents build.
- One backend ships in the user-facing release. Other backends exist only as dev-time benchmark controls.
- Performance must be SOTA (≥ 95% of upstream `llama.cpp`).
- Self-contained: user installs one package, gets working dictation. No "install this system library first" steps.
- Models auto-download to `~/.cache/fono/models/llm/` on first use, with SHA-256 verification.
- Latest model formats supported (GGUF universal).
- Cross-machine: at minimum x86_64 CPU + AVX2; opt-in GPU variants for CUDA/Metal/Vulkan as they mature.

## Strategic Approach

### Why `llama.cpp` Wins
- It is the SOTA CPU LLM inference reference. Every Rust-native framework (`candle`, `mistral.rs`, `burn`) measures itself against `llama.cpp` and lands at 50–95%.
- Universal GGUF support — every quant of every modern model lands on `llama.cpp` first.
- Active GPU backend portfolio: CUDA, Metal, Vulkan, ROCm, OpenMP — all behind feature flags in `llama-cpp-sys-2`.
- `llama-cpp-2` Rust bindings are already a project dependency.

### Why Dynamic Linking Resolves the Collision
At static-link time the linker sees two `.a` archives (`libwhisper.a`, `libllama.a`) each defining `ggml_init`, `ggml_compute_*`, etc. → multiple-definition error. With `--features dynamic-link`, `llama-cpp-sys-2`'s build script produces a standalone `libllama.so` that internalises its `ggml` symbols. The Fono binary statically links `whisper-rs-sys`'s private `ggml` and dynamically loads `libllama.so` at startup. The two `ggml` copies coexist in distinct `dlopen` namespaces — no symbol clash.

### Self-Contained from the User's Perspective
The `.deb`/`.txz`/`.lzm`/`.rpm` package layout becomes:
```
/usr/bin/fono                           # main ELF, RPATH=$ORIGIN/../lib/fono
/usr/lib/fono/libllama.so               # bundled, private to fono
/usr/share/applications/fono.desktop
/usr/share/doc/fono/...
/lib/systemd/user/fono.service
```
The user runs `apt install fono` (or equivalent) and `fono` works. They never know `libllama.so` exists.

### Static-Musl Tarball Trade-off
The historical "single-file static-musl ELF tarball" goal in `docs/plans/2026-04-24-fono-design-v1.md:22-23` is relaxed: the canonical distribution channels become the per-distro packages (`.deb`, `.txz`, `.lzm`, `.rpm`, `.dmg`, `.msi`). The GitHub Release attaches packages, not a raw ELF. This matches how end users actually install desktop apps. Devs running `cargo build --release` from source still get a working binary because Cargo links against the `libllama.so` in `target/release/`.

## Implementation Plan

### 1. Backend Switch
- [ ] Task 1.1. In `crates/fono-llm/Cargo.toml`, remove the optional `llama-cpp-2` dep and replace with a non-optional `llama-cpp-2` enabling its `dynamic-link` (or equivalent) flag. Drop the `llama-local` cargo feature gate — local LLM becomes always-on, like `whisper-rs` is always-on.
- [ ] Task 1.2. In `crates/fono/Cargo.toml`, remove the `llama-local` feature line; default features become `["tray", "local-models"]` where `local-models` now implies both whisper *and* llama.cpp because they no longer conflict.
- [ ] Task 1.3. In `crates/fono/src/lib.rs`, delete the `compile_error!` block and its block comment. Both backends now coexist by design.
- [ ] Task 1.4. Verify `cargo build --release` produces `target/release/libllama.so` next to `target/release/fono`, and that `ldd target/release/fono` lists `libllama.so` resolving via RPATH.

### 2. Cargo & Build Wiring
- [ ] Task 2.1. Set `rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib/fono:$ORIGIN` in a top-level `.cargo/config.toml` or in `crates/fono/build.rs` so the installed binary finds `libllama.so` in `/usr/lib/fono/` (package install) or alongside the binary (cargo build / portable tarball).
- [ ] Task 2.2. Confirm `whisper-rs-sys`'s static `ggml` and the dynamically-linked `libllama.so`'s internal `ggml` do not clash at runtime via a smoke test that loads a whisper model and runs a llama inference in the same process.
- [ ] Task 2.3. Strip and `chrpath`-verify `libllama.so` so it has no host-machine RPATHs leaking through.

### 3. Packaging Updates
- [ ] Task 3.1. Update `packaging/slackbuild/fono/fono.SlackBuild` to install `libllama.so` to `$PKG/usr/lib/fono/libllama.so` and stage it in the `.txz` payload. Verify `slackpkg install fono-*.txz` resolves the library at runtime via RPATH.
- [ ] Task 3.2. Update `.github/workflows/release.yml` Slackware/NimbleX job to copy `libllama.so` from `target/release/` into the staged `pkg/usr/lib/fono/` tree before invoking `mksquashfs` for the `.lzm`.
- [ ] Task 3.3. Update `packaging/debian/` rules to install `libllama.so` into `usr/lib/fono/`. Add a `Depends:` on libstdc++6 (already implied by Rust) but no llama-specific package.
- [ ] Task 3.4. Update `packaging/arch/PKGBUILD` and `packaging/nix/default.nix` analogously.
- [ ] Task 3.5. Document in `docs/providers.md` that local LLM ships out-of-the-box (no opt-in), and document the per-package layout for downstream packagers.

### 4. Auto-Download UX
- [ ] Task 4.1. In `crates/fono/src/wizard.rs`, change the local-LLM offer flow: stop asking "skip vs install" — auto-select Qwen2.5-1.5B-Instruct (Q4_K_M) for `Comfortable`/`Recommended`/`HighEnd` tiers and SmolLM2-360M for `Minimum`. `Unsuitable` falls back to cloud LLM with a clear note.
- [ ] Task 4.2. In `crates/fono/src/daemon.rs` startup, if `cfg.llm.backend == Local` and the resolved GGUF file is missing, trigger an auto-download via `fono-download` with progress shown in the tray menu and a system notification ("Downloading Qwen2.5-1.5B-Instruct (1.0 GB)…"). Block dictation until the download completes; queue any in-flight hotkey events.
- [ ] Task 4.3. Update `crates/fono-llm/src/registry.rs` (or equivalent registry module) to pin SHA-256 hashes for the latest 3–5 community-favoured models: Qwen2.5-1.5B-Instruct, Qwen2.5-3B-Instruct, SmolLM2-1.7B, SmolLM2-360M, plus one larger flagship (Qwen2.5-7B-Instruct) for `HighEnd` boxes. Refresh quarterly.
- [ ] Task 4.4. Implement `FONO_MODEL_MIRROR` env var override (already in design plan Task 9.5) and confirm the auto-downloader honours it.

### 5. GPU Acceleration Variants
- [ ] Task 5.1. Add cargo features `gpu-cuda`, `gpu-metal`, `gpu-vulkan`, `gpu-rocm` on `crates/fono/Cargo.toml` that turn on the corresponding `llama-cpp-sys-2` feature. Default build remains CPU + OpenMP.
- [ ] Task 5.2. Extend `.github/workflows/release.yml` matrix to produce per-variant packages: `fono-<ver>-x86_64.txz` (CPU), `fono-cuda-<ver>-x86_64.txz`, `fono-vulkan-<ver>-x86_64.txz`, `fono-metal-<ver>-aarch64.dmg`. Devs/users pick their hardware variant.
- [ ] Task 5.3. Implement runtime GPU detection in `crates/fono/src/doctor.rs` and the wizard: report which variant the user is running and recommend swapping if the host has a GPU but the CPU variant is installed.

### 6. Benchmark Suite (Dev-Only)
- [ ] Task 6.1. Add `candle-core`, `candle-transformers` as **dev-dependencies only** in `crates/fono-bench/Cargo.toml`. Production builds never see candle.
- [ ] Task 6.2. Add a behind-the-scenes `CandleLocal` impl of `TextFormatter` inside `crates/fono-bench/src/candle_baseline.rs` (not in `fono-llm`) — exists solely as a benchmark control.
- [ ] Task 6.3. Create `crates/fono-bench/benches/llm_compare.rs` Criterion benchmark: load identical Qwen2.5-1.5B GGUF into both `LlamaLocal` (production) and `CandleLocal` (control), measure tokens/sec on a fixed cleanup prompt, and assert `llama.cpp` ≥ 1.3× `candle` (CI gate that catches future regressions where someone "simplifies" by switching backends).
- [ ] Task 6.4. Add a quarterly review checklist item in `docs/status.md`: re-run `llm_compare`, also benchmark `mistral.rs` and `ort` if they've matured. If any pure-Rust backend reaches ≥ 95% of `llama.cpp` with comparable feature support, re-evaluate the dynamic-link decision.

### 7. Documentation
- [ ] Task 7.1. Update `docs/plans/2026-04-24-fono-design-v1.md` Phase 9 to reflect per-distro packaging as primary distribution; the static-musl tarball goal is downgraded to "dev artifact only".
- [ ] Task 7.2. Update `docs/decisions/` with a new ADR `0009-llama-dynamic-link.md` capturing this decision, the alternatives weighed (`candle`, `mistral.rs`, `ort`, sidecar binary, symbol prefixing), and the SOTA-performance rationale.
- [ ] Task 7.3. Update `README.md` install snippets: `apt install ./fono.deb`, `installpkg fono-*.txz`, `pacman -U`, `nix profile install`. Drop the old "download static binary, chmod +x" instruction.
- [ ] Task 7.4. Mark the obsolete `docs/decisions/0008-llama-local-deferred.md` as superseded.

## Verification Criteria

- `cargo build --release` produces `target/release/fono` + `target/release/libllama.so`. `ldd target/release/fono` resolves `libllama.so` via RPATH.
- `cargo test --workspace` passes with both `whisper-rs` and `llama-cpp-2` linked into the same test process (smoke test loads both, runs a 1-token inference each, no segfault, no symbol-clash panic).
- `apt install ./target/debian/fono_<ver>_amd64.deb` on a clean Ubuntu/Debian VM yields a working `fono` from `/usr/bin/fono`. First dictation triggers Qwen2.5-1.5B auto-download with SHA-256 verification.
- `installpkg /tmp/fono-<ver>-x86_64-1_NimbleX.txz` on a clean NimbleX VM yields a working `fono` with no missing-library errors.
- `cargo bench -p fono-bench --bench llm_compare` reports `llama_cpp` p50 latency ≤ `candle` p50 latency / 1.3 on the reference machine.
- `fono doctor` reports the active llama backend variant (CPU/CUDA/Vulkan/Metal/ROCm) and the detected GPU (if any).
- The release pipeline produces at least four artifacts per tag: `fono.deb`, `fono.txz`, `fono.lzm`, `fono-cuda.deb` (and macOS+Windows analogues).

## Potential Risks and Mitigations

1. **`libllama.so` ABI drift across `llama-cpp-2` minor releases.**
   *Mitigation:* Pin the `llama-cpp-2` version exactly in `Cargo.toml` (no caret, no tilde). Bump only via explicit PRs that re-run `llm_compare` and the smoke tests.

2. **Dynamic loader can't find `libllama.so` if user moves the binary.**
   *Mitigation:* RPATH includes both `$ORIGIN/../lib/fono` (package install) and `$ORIGIN` (portable). For truly relocatable use, ship the package, not the bare binary.

3. **GPU variant mismatched to host (e.g. user installs `fono-cuda` on a non-NVIDIA box).**
   *Mitigation:* `fono doctor` and the wizard detect the GPU at runtime and warn. `llama-cpp-sys-2` with the `cuda-no-vmm` flag avoids hard-linking against `libcuda.so` so a CUDA-built binary still launches on a CPU-only host (just falls back).

4. **`cargo install fono` (no package) leaves users with a broken binary.**
   *Mitigation:* Add a `crates/fono/build.rs` post-build hook that prints "fono is not designed to be installed via `cargo install`; please use a distro package or download from GitHub Releases" and a one-liner `RUSTFLAGS=-Wl,-rpath,$ORIGIN` workaround for adventurous devs.

5. **Static-musl tarball requirement from the original design plan.**
   *Mitigation:* Already addressed — original requirement relaxed to "per-distro packages preferred". Static-musl + cloud-only LLM remains buildable for niche use cases (`cargo build --release --no-default-features --features tray,cloud-all`).

6. **Future improvement in pure-Rust frameworks could obsolete the dynamic-link choice.**
   *Mitigation:* Quarterly benchmark review (Task 6.4). The `candle` baseline and the regression test ensure we'll *see* the moment a Rust-native option closes the gap.

## Alternative Approaches (rejected and why)

1. **`mistral.rs` as the production backend.** Rejected: built on `candle`, peaks at ~80–95% of `llama.cpp`, and pulls in `scraper`, `html2text`, `image`, `symphonia`, `mcp`, `openai-harmony` — over 100 transitive crates for a text-cleanup task that needs ~5. Violates the lightweight-binary principle.

2. **`candle` as the production backend.** Rejected: 50–70% of `llama.cpp` perf on CPU. Fails the SOTA bar.

3. **`ort` (ONNX Runtime) as the production backend.** Rejected for v0.1: would force a parallel ONNX model registry (users couldn't reuse a downloaded GGUF) and ONNX Runtime itself is a 50+ MB dynamic library — same packaging story as `libllama.so` but without the SOTA performance. Worth re-evaluating in v0.2 if Silero VAD already drags `ort` in.

4. **Sidecar binary (extract-and-spawn `fono-llm-runner`).** Rejected: extracting executables at runtime trips antivirus heuristics, complicates code-signing on macOS/Windows, and adds IPC latency. Dynamic linking is the cleaner solve.

5. **Symbol prefixing via `objcopy` of `whisper-rs-sys`'s `ggml`.** Rejected: cross-platform `objcopy` flag drift (GNU vs llvm vs macOS), maintaining a `whisper-rs` fork is a permanent tax, and we still ship two copies of `ggml` in the binary.

6. **Dropping `whisper-rs` for an ONNX-based STT (e.g. sherpa-onnx).** Rejected for v0.1 — would invalidate the existing whisper model registry, the `fono-stt` factory, and break user installations on update. Worth re-evaluating in v0.3 alongside diarization (which already wants sherpa-onnx).