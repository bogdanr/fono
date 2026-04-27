// SPDX-License-Identifier: GPL-3.0-only
//! `fono` library surface — re-exports the modules used by the binary
//! entrypoint and the integration tests under `crates/fono/tests/`.
//!
//! All real logic lives in submodules; `main.rs` is a thin entrypoint
//! and `tests/pipeline.rs` exercises the pipeline orchestrator without
//! a microphone or a network.

// `whisper-rs-sys` and `llama-cpp-sys-2` each statically link their own
// copy of ggml. Combining both in one binary blows up at link time with
// "multiple definition of ggml_backend_*" errors. Until `llama-cpp-sys-2`'s
// `dynamic-link` feature is wired up (which would build libllama.so and
// ship it in our distro packages), users have to pick one local backend:
//
//   * default build:   local STT (whisper) + cloud LLM (recommended for
//                      laptops — see docs/status.md)
//   * llama-local:     cloud STT + local LLM cleanup; build with
//                      `cargo build --release --no-default-features \
//                       --features tray,llama-local,cloud-all`
#[cfg(all(feature = "local-models", feature = "llama-local"))]
compile_error!(
    "fono cannot enable both `local-models` (whisper-local) and `llama-local` \
     at once: whisper-rs-sys and llama-cpp-sys-2 both statically link ggml, \
     which collides at link time. Build with --no-default-features and \
     re-add only the features you want, e.g.:\n\n    \
     cargo build --release --no-default-features --features tray,llama-local,cloud-all"
);

pub mod cli;
pub mod daemon;
pub mod doctor;
pub mod models;
pub mod session;
pub mod wizard;
