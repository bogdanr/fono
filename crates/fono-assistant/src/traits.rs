// SPDX-License-Identifier: GPL-3.0-only
//! The `Assistant` trait + per-turn context type.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use fono_core::screen_capture::{CaptureError, CaptureMode, CapturedImage};
use futures::stream::BoxStream;

use crate::history::ChatTurn;

/// One token delta yielded by [`Assistant::reply_stream`]. `text` is
/// the new piece of model output (typically a single token's worth of
/// characters). The struct is kept future-proof for tool-call deltas
/// (function calls, audio output, etc.) by leaving room for new
/// optional fields without a breaking change.
#[derive(Debug, Clone, Default)]
pub struct TokenDelta {
    pub text: String,
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
}
