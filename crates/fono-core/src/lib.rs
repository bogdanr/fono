// SPDX-License-Identifier: GPL-3.0-only
//! Shared types, config loader, XDG path resolver, and SQLite schema for Fono.
//!
//! Implemented per Phase 1 of `docs/plans/2026-04-24-fono-design-v1.md`.

pub mod api_keys;
pub mod config;
pub mod correction;
pub mod critical_notify;
pub mod error;
pub mod history;
pub mod hwcheck;
pub mod languages;
pub mod locale;
pub mod notify;
pub mod openrouter_attribution;
pub mod paths;
pub mod prompt_cache;
pub mod provider_catalog;
pub mod providers;
pub mod screen_capture;
pub mod secrets;
pub mod speakers;
pub mod turn_trace;
pub mod voice_discovery;
pub mod voice_palette;
pub mod voice_resolver;
pub mod wav;

#[cfg(feature = "budget")]
pub mod budget;

#[cfg(feature = "llama-local")]
pub mod brain_tap;

#[cfg(feature = "llama-local")]
pub mod llama_backend;

#[cfg(feature = "llama-local")]
pub mod llama_gen;

#[cfg(feature = "vulkan-probe")]
pub mod vulkan_probe;

// Soft-load shim for the Vulkan loader: defines the handful of bare
// `vk*` symbols ggml references at link time so they resolve to our own
// `dlopen`-based forwarders instead of hard-linking `libvulkan.so.1`.
// Lives here (not in a backend crate) so it is defined exactly once and
// compiled whenever *either* the whisper (`fono-stt`) or llama
// (`fono-polish`/`fono-assistant`) Vulkan backend is active. Lets the
// GPU build launch on hosts without the Vulkan loader and fall back to
// CPU. See `plans/2026-07-12-vulkan-soft-load-single-build-v1.md`.
#[cfg(all(feature = "accel-vulkan", any(target_os = "linux", target_os = "windows")))]
pub mod vk_loader_shim;

pub use api_keys::{ApiKeyStore, ApiKeyView};
pub use config::Config;
pub use error::{Error, Result};
pub use hwcheck::{HardwareSnapshot, LocalTier};
pub use paths::Paths;
pub use provider_catalog::{
    AssistantDefaults, Badge, CloudProvider, PolishDefaults, SttDefaults, TtsDefaults, TtsEndpoint,
    WebSearchSupport, CLOUD_PROVIDERS,
};
pub use screen_capture::{CaptureError, CaptureMode, CaptureSource, CapturedImage, GrabberProbe};
pub use secrets::Secrets;
pub use speakers::{Calibration, SpeakerStore, SpeakerView, Utterance};

#[cfg(feature = "budget")]
pub use budget::{BudgetController, BudgetVerdict, PerSecondCostUMicros, PriceTable, QualityFloor};
