// SPDX-License-Identifier: GPL-3.0-only
//! Streaming chat assistant trait + cloud and (stubbed) local
//! backends. Mirrors the shape of `fono-polish` but with a different
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
#[cfg(feature = "realtime")]
pub mod gemini_live;
#[cfg(feature = "llama-local")]
pub mod llama_local;
#[cfg(feature = "openai-compat")]
pub mod openai_compat_chat;

pub use factory::{
    build_assistant, build_assistant_handle, build_server_assistant_override, chat_endpoint,
    cloud_chat_upstream, server_assistant_model_name, uses_embedded_local_model, AssistantHandle,
    CloudUpstream,
};
#[cfg(feature = "realtime")]
pub use gemini_live::GeminiLive;
pub use history::{ChatRole, ChatTurn, ConversationHistory, ToolCall};
pub use traits::{
    Assistant, AssistantCacheTrigger, AssistantContext, AssistantPromptCacheSnapshot,
    AssistantPromptCacheWarmup, RealtimeAssistant, RealtimeEvent, RealtimeMode, RealtimeSession,
    ScreenCaptureFn, TokenDelta, ToolEvent,
};
