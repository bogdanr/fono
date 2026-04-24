// SPDX-License-Identifier: GPL-3.0-only
//! Shared types, config loader, XDG path resolver, and SQLite schema for Fono.
//!
//! Implemented per Phase 1 of `docs/plans/2026-04-24-fono-design-v1.md`.

pub mod config;
pub mod error;
pub mod history;
pub mod paths;
pub mod secrets;

pub use config::Config;
pub use error::{Error, Result};
pub use paths::Paths;
pub use secrets::Secrets;
