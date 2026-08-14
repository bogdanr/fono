// SPDX-License-Identifier: GPL-3.0-only
//! Text-formatter trait + cloud (Cerebras default, OpenAI-compatible, Anthropic)
//! and opt-in local (`llama-cpp-2`) backends.

pub mod defaults;
pub mod factory;
pub mod registry;
pub mod traits;

#[cfg(feature = "anthropic")]
pub mod anthropic;
#[cfg(feature = "llama-local")]
pub mod llama_local;
#[cfg(any(feature = "cerebras", feature = "openai-compat"))]
pub mod openai_compat;

pub use factory::{build_polish, offload_plan};
pub use registry::{LocalLlmModelInfo, LocalLlmRegistry, LOCAL_LLM_MODELS};
pub use traits::{
    has_enough_text_for_language_guard, looks_like_clarification, looks_like_degenerate_cleanup,
    looks_like_translated_cleanup, FormatContext, TextFormatter,
};
