// SPDX-License-Identifier: GPL-3.0-only
//! The `Assistant` trait + per-turn context type.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use fono_core::screen_capture::{CaptureError, CaptureMode, CapturedImage};
use futures::stream::BoxStream;

use crate::history::{ChatTurn, ToolCall};

/// Hotkey/runtime trigger that selected a prompt-state cache family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistantCacheTrigger {
    /// F7 dictation/polish flow. Local assistant backends may warm a compatible
    /// cleanup prompt prefix when they share the embedded llama.cpp runtime.
    F7,
    /// F8 voice-assistant flow.
    F8,
}

/// Stable prompt families the daemon may ask an assistant backend to warm at
/// startup/idle time. Non-local backends ignore this through the default trait
/// implementation.
#[derive(Debug, Clone, Default)]
pub struct AssistantPromptCacheWarmup {
    pub f7_system_prompt: Option<String>,
    pub f8_system_prompt: Option<String>,
    pub assistant_tool_prompt: Option<String>,
}

/// Per-turn cache preparation request captured at hotkey press time, before STT
/// finishes. The user transcript is intentionally absent; only stable prompt and
/// active-window state are available this early.
#[derive(Debug, Clone)]
pub struct AssistantPromptCacheSnapshot {
    pub trigger: AssistantCacheTrigger,
    pub system_prompt: String,
    pub history: Vec<ChatTurn>,
    pub active_window_context: Option<String>,
    pub prefer_vision: bool,
}

/// One token delta yielded by [`Assistant::reply_stream`]. Most
/// deltas carry spoken `text`; a small number carry a sentinel
/// [`ToolEvent`] that the caller MUST record in
/// [`crate::ConversationHistory`] so subsequent turns can echo the
/// tool sequence back to the model.
///
/// A single delta carries _either_ `text` or `tool_event` — never
/// both at once. Callers should branch on `tool_event` first; if
/// `Some`, ignore `text`.
#[derive(Debug, Clone, Default)]
pub struct TokenDelta {
    pub text: String,
    /// Sentinel for non-text events on the stream (tool call issued,
    /// tool result observed). When `Some`, this delta has no spoken
    /// content and the caller should append a corresponding entry
    /// to the rolling history before pushing the final assistant
    /// reply.
    pub tool_event: Option<ToolEvent>,
}

impl TokenDelta {
    /// Build a pure-text delta. Equivalent to `TokenDelta { text,
    /// tool_event: None }` but reads cleaner at call sites.
    #[must_use]
    pub fn text(text: String) -> Self {
        Self { text, tool_event: None }
    }

    /// Build a sentinel delta carrying a [`ToolEvent`]. The `text`
    /// field is empty and must not be spoken.
    #[must_use]
    pub fn tool(event: ToolEvent) -> Self {
        Self { text: String::new(), tool_event: Some(event) }
    }
}

/// Side-band events on the token stream that record tool usage.
/// Emitted by the assistant client during a turn where the model
/// invoked a function-calling tool.
#[derive(Debug, Clone)]
pub enum ToolEvent {
    /// The model issued a tool call. The caller should append an
    /// assistant turn with this tool call to history so the next
    /// turn's wire request can echo it back to the model.
    Called(ToolCall),
    /// The tool returned a result. `summary` is a short, prose
    /// description suitable for storing in history; the actual
    /// payload (image bytes, etc.) is _not_ retained.
    Result { tool_call_id: String, summary: String },
}

/// Synchronous screen-capture callback type. The closure runs the
/// full [`fono_core::screen_capture::GrabberProbe`] pipeline (including the
/// privacy gate) and returns the captured PNG or a [`CaptureError`].
/// Wrapped in [`Arc`] so it can be cheaply cloned into spawned tasks.
pub type ScreenCaptureFn =
    Arc<dyn Fn(CaptureMode) -> Result<CapturedImage, CaptureError> + Send + Sync>;

/// Per-turn context passed to [`Assistant::reply_stream`]. The history
/// is a snapshot taken by the caller *before* it pushed the new user
/// turn, so the user's current text is the `user_text` argument and
/// not duplicated here. `system_prompt` is the chat-style prompt from
/// `[assistant].prompt_main` (distinct from the cleanup prompt).
#[derive(Clone, Default)]
pub struct AssistantContext {
    pub system_prompt: String,
    pub language: Option<String>,
    pub history: Vec<ChatTurn>,
    /// Short, runtime-only description of the window active when the assistant
    /// hotkey was pressed. This is cached separately from stable prompts so a
    /// window change cannot invalidate F8's base prompt checkpoint.
    pub active_window_context: Option<String>,
    /// When `Some`, tool-calling is enabled and the model may invoke
    /// `fono_screen` to capture the screen during a voice turn.
    /// Set from the F8 voice loop when a `GrabberProbe` is available.
    pub screen_capture: Option<ScreenCaptureFn>,
    /// When `true` (and [`screen_capture`] is `Some`), include the
    /// `fono_screen` tool descriptor in every request. Users opt in
    /// with `[assistant].prefer_vision = true`.
    pub prefer_vision: bool,
}

impl std::fmt::Debug for AssistantContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssistantContext")
            .field("system_prompt", &self.system_prompt)
            .field("language", &self.language)
            .field("history", &self.history)
            .field("active_window_context", &self.active_window_context)
            .field("screen_capture", &self.screen_capture.is_some())
            .field("prefer_vision", &self.prefer_vision)
            .finish()
    }
}

#[async_trait]
pub trait Assistant: Send + Sync {
    /// Stream the model's reply token-by-token. The returned stream
    /// yields `Ok(TokenDelta)` per delta and ends when the model
    /// finishes (or errors). Cancellation is by dropping the stream;
    /// implementations MUST not require an explicit cancel call.
    async fn reply_stream(
        &self,
        user_text: &str,
        ctx: &AssistantContext,
    ) -> Result<BoxStream<'static, Result<TokenDelta>>>;

    /// Backend identifier for history / logging.
    fn name(&self) -> &'static str;

    /// Optional best-effort warmup. Cloud backends should fire a cheap
    /// HEAD/GET; local backends should mmap their model. Failures are
    /// non-fatal.
    async fn prewarm(&self) -> Result<()> {
        Ok(())
    }

    /// Optional startup/idle prompt-state cache warmup. Embedded local
    /// backends use this to prefill stable F7/F8/tool prompts without making
    /// the hotkey path pay the prompt cost. Cloud backends ignore it.
    async fn prewarm_prompt_caches(&self, _warmup: AssistantPromptCacheWarmup) -> Result<()> {
        Ok(())
    }

    /// Optional hotkey-time cache preparation. The default is a no-op; embedded
    /// local backends may restore/build a stable checkpoint and, when window
    /// context is available, schedule a dynamic window checkpoint.
    async fn prepare_prompt_cache_for_turn(
        &self,
        _snapshot: AssistantPromptCacheSnapshot,
    ) -> Result<()> {
        Ok(())
    }
}
