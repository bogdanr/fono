# Fono: Implementing Candle as Alternative Default Backend & Benchmarking

## Status: Superseded

This plan was never executed. The `whisper-rs` × `llama-cpp-2` ggml
symbol collision it was designed to work around was instead resolved
in-tree by `-Wl,--allow-multiple-definition` at
`.cargo/config.toml:21-28` (documented in `docs/status.md:276-310` and
in `docs/decisions/0018-ggml-link-trick.md`). The single-binary outcome
the plan targeted now ships from `llama-cpp-2` directly, with no candle
dependency added.

Rollback path if the link trick ever fails on a future linker: plan H
— `plans/closed/2026-04-27-shared-ggml-static-binary-v1.md` — is the
documented escape hatch (shared ggml).

## Objective

Introduce `candle` as an alternative local LLM backend (`candle-local`) to solve the `ggml` linker collision while keeping `llama-cpp-2` (`llama-local`) available for benchmarking. Add comprehensive benchmarking to compare the performance of both backends.

## Strategic Approach

By adding `candle-local`, we achieve a truly single-binary build with both local STT (`whisper-rs`) and local LLM (`candle`) enabled by default, avoiding the `ggml` symbol collision. `llama-local` remains as an optional feature. We will update the `fono-bench` suite to allow head-to-head performance comparisons between the two.

## Implementation Plan

### 1. Dependency and Feature Updates
- [ ] Task 1.1. In `crates/fono-llm/Cargo.toml`, add `candle-core`, `candle-transformers`, and `tokenizers` as optional dependencies.
- [ ] Task 1.2. In `crates/fono-llm/Cargo.toml`, add a new feature `candle-local = ["dep:candle-core", "dep:candle-transformers", "dep:tokenizers"]`.
- [ ] Task 1.3. In `crates/fono/Cargo.toml`, add `candle-local` to the `default` features alongside `local-models` and `tray`. Add `candle-local = ["fono-llm/candle-local"]`.
- [ ] Task 1.4. In `crates/fono-bench/Cargo.toml`, add `candle-local = ["fono-llm/candle-local"]` and `llama-local = ["fono-llm/llama-local"]` to the `[features]` block.

### 2. Core Config & Factory Wiring
- [ ] Task 2.1. In `crates/fono-core/src/config.rs`, add `Candle` to the `LlmBackend` enum.
- [ ] Task 2.2. In `crates/fono-llm/src/factory.rs`, add a `build_candle` function gated by `#[cfg(feature = "candle-local")]`.
- [ ] Task 2.3. In `crates/fono-llm/src/factory.rs`, update the `match &cfg.backend` block to route `LlmBackend::Candle` to `build_candle(cfg, llm_models_dir)`.

### 3. CandleLocal Implementation
- [ ] Task 3.1. Create `crates/fono-llm/src/candle_local.rs`.
- [ ] Task 3.2. Implement the `TextFormatter` trait for `CandleLocal`. The implementation must mirror `llama_local.rs` in functionality:
  - Lazy load the GGUF model via `candle_core::quantized::gguf_file`.
  - Format the input using the ChatML prompt template.
  - Run the generation loop using `candle_transformers::generation::LogitsProcessor`.
  - Handle the `<|im_end|>` stop token properly.
  - Wrap the CPU-bound inference in `tokio::task::spawn_blocking`.

### 4. Benchmarking Infrastructure
- [ ] Task 4.1. In `crates/fono-bench/src/bin/fono-bench.rs`, add `Llama` and `Candle` variants to the `LlmProvider` enum.
- [ ] Task 4.2. Update the `build_llm` function in `fono-bench.rs` to instantiate `LlamaLocal` when `--llm llama` is passed (gated by `llama-local` feature) and `CandleLocal` when `--llm candle` is passed (gated by `candle-local` feature).
- [ ] Task 4.3. Create a new Criterion benchmark file `crates/fono-bench/benches/llm_compare.rs`.
- [ ] Task 4.4. In `llm_compare.rs`, write a benchmark group that loads the same GGUF model into both `LlamaLocal` and `CandleLocal` and measures the latency of `.format()` on a standard 100-word transcription cleanup task.
- [ ] Task 4.5. Register the new benchmark in `crates/fono-bench/Cargo.toml` under `[[bench]]`.

## Verification Criteria
- [ ] `cargo build` succeeds with default features, building both `whisper-local` and `candle-local` without linker errors.
- [ ] `cargo build --no-default-features --features llama-local` succeeds.
- [ ] `cargo run -p fono-bench --no-default-features --features fono-llm/candle-local,fono-llm/llama-local -- --provider fake --llm candle` executes successfully.
- [ ] `cargo bench -p fono-bench --bench llm_compare --no-default-features --features fono-llm/candle-local,fono-llm/llama-local` successfully measures and reports the performance difference between the two backends.

## Potential Risks and Mitigations
1. **Performance Discrepancy:** `candle` CPU inference might be significantly slower than `llama.cpp`'s highly optimized GGML routines.
   *Mitigation:* The benchmark test will explicitly quantify this. If the gap is too large, we can document it and users can still opt-in to `llama-local` (at the cost of dropping local STT in the same binary, or using cloud STT).
2. **Tokenizer Fidelity:** `candle` requires the `tokenizers` crate which might parse ChatML tokens slightly differently than `llama.cpp`'s built-in tokenizer.
   *Mitigation:* Ensure `tokenizers` is configured to correctly parse the `tokenizer.json` embedded in the GGUF or handle special tokens explicitly.