// SPDX-License-Identifier: GPL-3.0-only
//! Shared types, config loader, XDG path resolver, and SQLite schema for Fono.
//!
//! Implemented per Phase 1 of `docs/plans/2026-04-24-fono-design-v1.md`.

pub mod config;
pub mod error;
pub mod history;
pub mod hwcheck;
pub mod languages;
pub mod locale;
pub mod notify;
pub mod paths;
pub mod providers;
pub mod secrets;

#[cfg(feature = "budget")]
pub mod budget;

#[cfg(feature = "vulkan-probe")]
pub mod vulkan_probe;

pub use config::Config;
pub use error::{Error, Result};
pub use hwcheck::{HardwareSnapshot, LocalTier};
pub use paths::Paths;
pub use secrets::Secrets;

#[cfg(feature = "budget")]
pub use budget::{BudgetController, BudgetVerdict, PerSecondCostUMicros, PriceTable, QualityFloor};
