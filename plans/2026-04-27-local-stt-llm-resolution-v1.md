# Fono: Resolving Local STT and Local LLM Linker Collision

## Objective

Enable both local STT (`whisper-rs`) and local LLM (`llama-cpp-2`) cleanup in the default Fono build without requiring external dependencies (like shared libraries), resolving the `ggml` symbol collision at link time.

## Initial Assessment

### Project Structure Summary
- `crates/fono/src/lib.rs`: Currently contains a `compile_error!` preventing both `local-models` (whisper) and `llama-local` (llama.cpp) from being enabled simultaneously due to `ggml_backend_*` symbol collisions.
- `crates/fono-llm/src/llama_local.rs`: Implements the local LLM backend using `llama-cpp-2`.
- `crates/fono-stt/src/whisper_local.rs`: Implements the local STT backend using `whisper-rs`.

### The Core Problem
Both `whisper-rs-sys` and `llama-cpp-sys-2` compile and statically link their own vendored copies of `ggml` (a C library). Because C lacks namespaces, the linker sees multiple definitions of global symbols (e.g., `ggml_init`, `ggml_compute`), causing a fatal link error. 

Since the user requires "no external dependencies", we cannot rely on dynamic linking (`libllama.so` or `libwhisper.so` installed system-wide). The solution must remain self-contained within the Fono distribution.

### Prioritization of Approaches
1. **Option A: Pure Rust LLM via `candle`** (Recommended) - Idiomatic, truly single-binary, cross-platform, zero C/C++ linker conflicts.
2. **Option B: Multi-Process Bundling** - Isolates memory spaces, keeps `llama.cpp` performance, but extracts a binary at runtime.
3. **Option C: Symbol Prefixing via `objcopy`** - Keeps both C++ backends but requires maintaining a fragile fork of `whisper-rs-sys`.

---

## Option A: Replace `llama-cpp-2` with `candle` (Recommended Strategy)

Replace the `llama-cpp-2` dependency with HuggingFace's `candle-core` and `candle-transformers`. `candle` is a pure Rust ML framework that supports GGUF models and quantized inference without linking `ggml`.

### Implementation Plan

- [ ] Task 1. Update `crates/fono-llm/Cargo.toml` to replace `llama-cpp-2` and `llama-cpp-sys-2` with `candle-core` and `candle-transformers` under the `llama-local` (or a new `candle-local`) feature.
- [ ] Task 2. Rewrite `crates/fono-llm/src/llama_local.rs` to use `candle` for inference. Implement model loading via `candle::quantized::gguf_file` and text generation using `candle_transformers::generation::LogitsProcessor`.
- [ ] Task 3. Implement the ChatML prompt formatting and stop-token logic (`<|im_end|>`) using `candle`'s tokenizer integration (via `tokenizers` crate).
- [ ] Task 4. Remove the `compile_error!` in `crates/fono/src/lib.rs` since the symbol collision is eliminated.
- [ ] Task 5. Update `crates/fono/Cargo.toml` to include both `local-models` and `llama-local` in the default feature set.

### Verification Criteria
- [ ] `cargo build --release` succeeds without linker errors.
- [ ] `ldd target/release/fono` confirms no external `libllama` or `libwhisper` dependencies.
- [ ] End-to-end dictation successfully runs audio through local Whisper and cleans up text via local Candle LLM.

### Potential Risks and Mitigations
1. **Performance Drop:** `candle` CPU inference might be slightly slower than `llama.cpp`.
   *Mitigation:* Benchmark `candle` against `llama.cpp` on the reference hardware. If unacceptable, fallback to Option B.

---

## Option B: Multi-Process Bundling (Alternative Strategy)

Keep `llama-cpp-2` but compile it as a standalone helper binary that is bundled inside the main Fono executable.

### Implementation Plan

- [ ] Task 1. Create a new minimal binary crate `fono-llm-runner` that depends *only* on `llama-cpp-2` and accepts a prompt via stdin, outputting the generated text to stdout.
- [ ] Task 2. Modify the Fono build process to compile `fono-llm-runner` first, and use `include_bytes!` in `fono-llm` to embed the resulting executable.
- [ ] Task 3. Rewrite `crates/fono-llm/src/llama_local.rs` to extract the embedded binary to `$XDG_CACHE_HOME/fono/bin/fono-llm-runner` at runtime (if it doesn't already exist or hash differs).
- [ ] Task 4. Implement the `TextFormatter` trait to spawn the runner as a subprocess (`std::process::Command`), passing the model path and prompt.
- [ ] Task 5. Remove the `compile_error!` in `crates/fono/src/lib.rs` and update default features.

### Verification Criteria
- [ ] Main Fono binary compiles successfully (since `llama-cpp-2` is only linked in the runner binary).
- [ ] At runtime, the helper binary is extracted and executes inference correctly.

### Potential Risks and Mitigations
1. **Antivirus/Security Software Interference:** Extracting and executing binaries at runtime can trigger false positives.
   *Mitigation:* Extract to a user-owned cache directory and ensure the binary is signed (on Windows/macOS).

---

## Option C: Symbol Prefixing via `objcopy` (Alternative Strategy)

Maintain both C++ backends in a single binary by renaming the `ggml` symbols in `whisper-rs-sys` during the build process.

### Implementation Plan

- [ ] Task 1. Fork `whisper-rs` and modify `whisper-rs-sys/build.rs`.
- [ ] Task 2. Add a build step in the fork that runs `objcopy --prefix-symbols=whisper_` on the compiled `libggml.a` and `libwhisper.a` before they are linked by Cargo.
- [ ] Task 3. Update the Rust FFI bindings in the fork to use the `whisper_` prefixed symbols.
- [ ] Task 4. Add a `[patch.crates-io]` section in `fono/Cargo.toml` pointing to the patched fork.
- [ ] Task 5. Remove the `compile_error!` in `crates/fono/src/lib.rs`.

### Verification Criteria
- [ ] `cargo build` succeeds with both features enabled.
- [ ] `nm` on the final binary shows both standard `ggml_*` (from llama) and `whisper_ggml_*` (from whisper) symbols.

### Potential Risks and Mitigations
1. **Cross-Platform Build Fragility:** `objcopy` flags differ between Linux (GNU), macOS (llvm-objcopy), and Windows.
   *Mitigation:* Implement platform-specific `objcopy` detection in the `build.rs` script.