// SPDX-License-Identifier: GPL-3.0-only
//! `fono` library surface — re-exports the modules used by the binary
//! entrypoint and the integration tests under `crates/fono/tests/`.
//!
//! All real logic lives in submodules; `main.rs` is a thin entrypoint
//! and `tests/pipeline.rs` exercises the pipeline orchestrator without
//! a microphone or a network.

// `whisper-rs-sys` and `llama-cpp-sys-2` each statically link their own
// copy of ggml. The workspace-level `.cargo/config.toml` passes
// `-Wl,--allow-multiple-definition` to the GNU/musl linker so the
// duplicate ggml symbols dedupe at link time instead of aborting the
// build. Both bundled ggml versions come from the same upstream
// (`ggerganov/ggml`) and are ABI-compatible by construction, so the
// linker keeping the first copy and discarding the second is safe;
// the smoke test in `crates/fono/tests/pipeline.rs` exercises both
// engines in the same process to catch any regression. See
// `plans/2026-04-27-shared-ggml-static-binary-v1.md` for the full
// rationale and the long-term shared-ggml plan.

pub mod cli;
pub mod daemon;
pub mod doctor;
pub mod models;
pub mod session;
pub mod wizard;
