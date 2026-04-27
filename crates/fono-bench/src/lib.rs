// SPDX-License-Identifier: GPL-3.0-only
//! Latency + accuracy benchmark crate for Fono.
//!
//! See `crates/fono-bench/README.md` for the user-facing overview, and
//! `docs/plans/2026-04-25-fono-latency-v1.md` (tasks L27–L30) for the
//! plan it implements. The streaming↔batch equivalence harness lives
//! in [`equivalence`] (plan v6 R18).

pub mod equivalence;
pub mod fakes;
pub mod fixtures;
pub mod report;
pub mod runner;
pub mod wav;
pub mod wer;

pub use equivalence::{
    levenshtein_norm, normalize_for_compare, EquivalenceReport, EquivalenceResult, Manifest,
    ManifestFixture, Metrics, ModeResult, Modes, Verdict,
};
pub use fixtures::{Fixture, FIXTURES};
pub use report::{ClipReport, LangReport, Report};
pub use runner::{BenchOutcome, BenchRunner};
pub use wer::word_error_rate;
