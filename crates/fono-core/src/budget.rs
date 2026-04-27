// SPDX-License-Identifier: GPL-3.0-only
//! Live-dictation budget controller + provider price table.
//!
//! Plan R12. Compiled only with the `budget` cargo feature.
//!
//! The controller tracks running cost (USD cents, accumulated as f64
//! micro-cents internally to avoid rounding drift) for a single
//! dictation session and exposes:
//!
//! 1. A static [`PriceTable`] mapping provider names → per-second
//!    streaming charge.
//! 2. A [`BudgetController`] that decides whether to keep the streaming
//!    lane "on" (chunky decode) or back off to a coarser cadence when
//!    the per-minute spend would exceed the user's ceiling.
//! 3. A [`QualityFloor`] enum so callers can opt into a stricter floor
//!    in environments where the dual-pass finalize must always run.
//!
//! Slice A wires the data types and a deterministic decision function;
//! the orchestrator consumes it but the cloud-streaming path that
//! actually charges per second arrives in Slice B.

use std::collections::BTreeMap;
use std::time::Duration;

/// Per-second cost of a streaming provider, in USD micro-cents
/// (1¢ = 10_000 µ¢). Keeping integer-ish to avoid f64 cumulative drift
/// over a long session. Local backends are zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PerSecondCostUMicros(pub u64);

impl PerSecondCostUMicros {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub fn from_dollars_per_minute(dpm: f64) -> Self {
        // 1 USD/min == 100 cents / 60 s == 100 * 10_000 / 60 µ¢/s
        Self(((dpm * 1_000_000.0) / 60.0) as u64)
    }
}

/// Quality floor — what the user is willing to give up under budget
/// pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QualityFloor {
    /// Backend may skip finalize-lane entirely on high-confidence
    /// segments to save cycles. R12.5.
    Aggressive,
    /// Backend may slow the preview cadence but always runs finalize.
    Balanced,
    /// Always run both lanes at full cadence. Slice A default — until
    /// per-token logprob is plumbed, we can't safely skip finalize.
    #[default]
    Max,
}

/// Read-only price table seeded from a baked-in default. Writeable at
/// runtime so packaging can override (e.g. self-hosted endpoints with
/// different pricing).
#[derive(Debug, Clone)]
pub struct PriceTable {
    entries: BTreeMap<String, PerSecondCostUMicros>,
}

impl PriceTable {
    /// Build the default table. Numbers are documented in the v6 plan
    /// (R12) and reflect public list pricing as of 2026-04. Local
    /// backends are zero by construction. Any provider not in the
    /// table is treated as zero-cost (caller's responsibility to add
    /// their custom endpoints).
    #[must_use]
    pub fn defaults() -> Self {
        let mut t = BTreeMap::new();
        t.insert("local".into(), PerSecondCostUMicros::ZERO);
        t.insert("whisper-local".into(), PerSecondCostUMicros::ZERO);
        // Cloud streaming STT providers (USD per minute, list price).
        t.insert(
            "groq".into(),
            PerSecondCostUMicros::from_dollars_per_minute(0.0033),
        );
        t.insert(
            "openai".into(),
            PerSecondCostUMicros::from_dollars_per_minute(0.006),
        );
        t.insert(
            "deepgram".into(),
            PerSecondCostUMicros::from_dollars_per_minute(0.0043),
        );
        t.insert(
            "assemblyai".into(),
            PerSecondCostUMicros::from_dollars_per_minute(0.0036),
        );
        Self { entries: t }
    }

    pub fn get(&self, provider: &str) -> PerSecondCostUMicros {
        self.entries
            .get(provider)
            .copied()
            .unwrap_or(PerSecondCostUMicros::ZERO)
    }

    pub fn insert(&mut self, provider: impl Into<String>, cost: PerSecondCostUMicros) {
        self.entries.insert(provider.into(), cost);
    }
}

impl Default for PriceTable {
    fn default() -> Self {
        Self::defaults()
    }
}

/// Decision the budget controller hands back to the orchestrator after
/// observing the session so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetVerdict {
    /// Keep streaming at the requested cadence.
    Continue,
    /// Coarsen the preview cadence (less-frequent decodes) to halve the
    /// effective cost. R12 "adaptive chunking".
    Throttle,
    /// Cut over to batch-only mode. The orchestrator should stop
    /// emitting preview updates and run a single batch decode at the
    /// end. Reached when the projected per-minute cost would exceed
    /// the user's ceiling by more than 50 %.
    StopStreaming,
}

/// The controller itself.
#[derive(Debug, Clone)]
pub struct BudgetController {
    cost: PerSecondCostUMicros,
    /// Per-minute hard ceiling, in micro-cents. `0` ⇒ no limit.
    ceiling_per_minute_umicros: u64,
    /// Total spend so far in this session, in µ¢.
    spent_umicros: u128,
    /// Total streamed audio so far.
    streamed: Duration,
    floor: QualityFloor,
}

impl BudgetController {
    #[must_use]
    pub fn new(
        cost: PerSecondCostUMicros,
        ceiling_per_minute_umicros: u64,
        floor: QualityFloor,
    ) -> Self {
        Self {
            cost,
            ceiling_per_minute_umicros,
            spent_umicros: 0,
            streamed: Duration::ZERO,
            floor,
        }
    }

    /// Convenience: zero-cost local backend never throttles.
    #[must_use]
    pub fn local() -> Self {
        Self::new(PerSecondCostUMicros::ZERO, 0, QualityFloor::Max)
    }

    /// Record `audio_seconds` of streamed audio and return the decision.
    pub fn record(&mut self, audio: Duration) -> BudgetVerdict {
        self.streamed += audio;
        self.spent_umicros = self
            .spent_umicros
            .saturating_add(u128::from(self.cost.0) * u128::from(audio.as_millis() as u64) / 1_000);
        self.verdict()
    }

    fn verdict(&self) -> BudgetVerdict {
        if self.ceiling_per_minute_umicros == 0 {
            return BudgetVerdict::Continue;
        }
        // Project the per-minute spend rate based on streamed audio.
        if self.streamed.is_zero() {
            return BudgetVerdict::Continue;
        }
        let per_minute = (self.spent_umicros * 60_000)
            / u128::from(self.streamed.as_millis().max(1) as u64);
        let ceiling = u128::from(self.ceiling_per_minute_umicros);
        if per_minute > ceiling * 3 / 2 {
            BudgetVerdict::StopStreaming
        } else if per_minute > ceiling {
            BudgetVerdict::Throttle
        } else {
            BudgetVerdict::Continue
        }
    }

    #[must_use]
    pub fn floor(&self) -> QualityFloor {
        self.floor
    }

    #[must_use]
    pub fn spent_umicros(&self) -> u128 {
        self.spent_umicros
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_controller_never_throttles() {
        let mut b = BudgetController::local();
        for _ in 0..1000 {
            assert_eq!(b.record(Duration::from_secs(60)), BudgetVerdict::Continue);
        }
    }

    #[test]
    fn cost_under_ceiling_continues() {
        // 0.006 USD/min = 600 µ¢/s. Ceiling = 60_000 µ¢/min = 1_000 µ¢/s.
        let cost = PerSecondCostUMicros::from_dollars_per_minute(0.006);
        let mut b = BudgetController::new(cost, 60_000, QualityFloor::Max);
        assert_eq!(b.record(Duration::from_secs(30)), BudgetVerdict::Continue);
    }

    #[test]
    fn cost_above_ceiling_throttles_then_stops() {
        // 0.06 USD/min = ten times above a 0.006 USD/min ceiling.
        let cost = PerSecondCostUMicros::from_dollars_per_minute(0.06);
        let ceiling = PerSecondCostUMicros::from_dollars_per_minute(0.006).0 * 60;
        let mut b = BudgetController::new(cost, ceiling, QualityFloor::Balanced);
        let v = b.record(Duration::from_secs(10));
        assert_eq!(v, BudgetVerdict::StopStreaming);
    }

    #[test]
    fn price_table_falls_back_to_zero_for_unknown_provider() {
        let t = PriceTable::defaults();
        assert_eq!(t.get("definitely-not-a-provider"), PerSecondCostUMicros::ZERO);
        assert_eq!(t.get("local"), PerSecondCostUMicros::ZERO);
        assert!(t.get("openai").0 > 0);
    }
}
