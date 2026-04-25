// SPDX-License-Identifier: GPL-3.0-only
//! Latency + accuracy benchmark crate for Fono.
//!
//! See `crates/fono-bench/README.md` for the user-facing overview, and
//! `docs/plans/2026-04-25-fono-latency-v1.md` (tasks L27–L30) for the
//! plan it implements.

pub mod fakes;
pub mod fixtures;
pub mod report;
pub mod runner;
pub mod wav;
pub mod wer;

pub use fixtures::{Fixture, FIXTURES};
pub use report::{ClipReport, LangReport, Report};
pub use runner::{BenchOutcome, BenchRunner};
pub use wer::word_error_rate;
