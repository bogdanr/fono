// SPDX-License-Identifier: GPL-3.0-only
//! Streaming chat assistant trait + cloud and (stubbed) local
//! backends. Mirrors the shape of `fono-llm` but with a different
//! invariant: the assistant *expects* chat-style replies (the cleanup
//! path explicitly rejects them via the `looks_like_clarification`
//! heuristic), maintains rolling conversation history, and streams
//! token deltas so the TTS pump can begin synthesis on the first
//! sentence.
//!
//! Designed to be plumbed through `fono`'s assistant pipeline
//! (Step 3 of the voice-assistant plan): STT → push user turn →
//! `Assistant::reply_stream(text, ctx)` → split into sentences →
//! `fono-tts::synthesize` → `fono-audio::playback`.

pub mod factory;
pub mod history;
pub mod traits;

#[cfg(any(feature = "openai-compat", feature = "anthropic"))]
mod sse;

#[cfg(feature = "anthropic")]
pub mod anthropic_chat;
#[cfg(feature = "openai-compat")]
pub mod openai_compat_chat;

pub use factory::build_assistant;
pub use history::{ChatRole, ChatTurn, ConversationHistory};
pub use traits::{Assistant, AssistantContext, TokenDelta};
