// SPDX-License-Identifier: GPL-3.0-only
//! `fono` library surface — re-exports the modules used by the binary
//! entrypoint and the integration tests under `crates/fono/tests/`.
//!
//! All real logic lives in submodules; `main.rs` is a thin entrypoint
//! and `tests/pipeline.rs` exercises the pipeline orchestrator without
//! a microphone or a network.

pub mod cli;
pub mod daemon;
pub mod doctor;
pub mod models;
pub mod session;
pub mod wizard;
