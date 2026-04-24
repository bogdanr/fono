// SPDX-License-Identifier: GPL-3.0-only
//! Text-formatter trait + cloud (Cerebras default, OpenAI-compatible, Anthropic)
//! and opt-in local (`llama-cpp-2`) backends. Phase 5 of
//! `docs/plans/2026-04-24-fono-design-v1.md`.

pub mod registry;
pub mod traits;

#[cfg(feature = "anthropic")]
pub mod anthropic;
#[cfg(feature = "llama-local")]
pub mod llama_local;
#[cfg(any(feature = "cerebras", feature = "openai-compat"))]
pub mod openai_compat;

pub use registry::{LlmModelInfo, LlmRegistry};
pub use traits::{FormatContext, TextFormatter};
