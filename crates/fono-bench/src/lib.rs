// SPDX-License-Identifier: GPL-3.0-only
//! Latency + accuracy benchmark crate for Fono.
//!
//! See `crates/fono-bench/README.md` for the user-facing overview, and
//! `docs/plans/2026-04-25-fono-latency-v1.md` (tasks L27–L30) for the
//! plan it implements. The streaming↔batch equivalence harness lives
//! in [`equivalence`] (plan v6 R18).

pub mod assistant_factual;
pub mod assistant_tool_use;
pub mod capabilities;
pub mod equivalence;
pub mod fakes;
pub mod fixtures;
pub mod polish_text;
pub mod report;
pub mod runner;
pub mod wav;
pub mod wer;

pub use assistant_factual::{
    load_manifest as load_assistant_factual_manifest, AssistantFactualFixture,
    AssistantFactualFixtureReport, AssistantFactualLangReport, AssistantFactualManifest,
    AssistantFactualMetrics, AssistantFactualReport, AssistantFactualRunConfig,
    DEFAULT_FIXTURE_RELATIVE_PATH as DEFAULT_ASSISTANT_FACTUAL_FIXTURE_PATH,
};
pub use assistant_tool_use::{
    load_manifest as load_assistant_tool_use_manifest, AssistantToolUseFixture,
    AssistantToolUseFixtureReport, AssistantToolUseLangReport, AssistantToolUseManifest,
    AssistantToolUseMetrics, AssistantToolUseReport, AssistantToolUseRunConfig,
    DEFAULT_FIXTURE_RELATIVE_PATH as DEFAULT_ASSISTANT_TOOL_USE_FIXTURE_PATH,
};
pub use capabilities::ModelCapabilities;
pub use equivalence::{
    baseline_subset, levenshtein_norm, normalize_for_compare, EquivalenceReport, EquivalenceResult,
    Manifest, ManifestFixture, Metrics, ModeResult, Modes, SkipReason, Verdict,
};
pub use fixtures::{Fixture, FIXTURES};
pub use polish_text::{
    load_manifest as load_polish_text_manifest, PolishTextFixture, PolishTextFixtureReport,
    PolishTextLangReport, PolishTextManifest, PolishTextMetrics, PolishTextReport,
    PolishTextRunConfig, DEFAULT_FIXTURE_RELATIVE_PATH as DEFAULT_POLISH_TEXT_FIXTURE_PATH,
};
pub use report::{ClipReport, LangReport, Report};
pub use runner::{BenchOutcome, BenchRunner};
pub use wer::word_error_rate;
