// SPDX-License-Identifier: GPL-3.0-only
//! STT trait definition.

use anyhow::Result;
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct Transcription {
    pub text: String,
    pub language: Option<String>,
    pub duration_ms: Option<u64>,
}

/// Per-call transcription options. Passed to [`SpeechToText::transcribe_with_opts`].
///
/// Added in Phase D (hover-context injection). The default value produces
/// identical behaviour to calling [`SpeechToText::transcribe`] directly, so
/// backends that don't override `transcribe_with_opts` are unaffected.
#[derive(Debug, Clone, Default)]
pub struct TranscribeOptions {
    /// Per-call language override. `None` defers to the backend's configured
    /// allow-list / auto-detect behaviour (same as passing `None` to
    /// `transcribe()`).
    pub lang_override: Option<String>,
    /// Short vocabulary hint for Whisper's `initial_prompt` (or the cloud
    /// equivalent). Backends that don't support it silently ignore this field.
    pub context_hint: Option<String>,
}

#[async_trait]
pub trait SpeechToText: Send + Sync {
    /// One-shot transcription of a full PCM buffer (mono f32 @ `sample_rate`).
    async fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
        lang: Option<&str>,
    ) -> Result<Transcription>;

    /// Context-aware transcription. The default implementation forwards to
    /// [`Self::transcribe`] ignoring the `context_hint` so existing backends
    /// that don't override this method behave identically to before.
    ///
    /// Override in backends that support prompt injection (WhisperLocal,
    /// OpenAI, Groq) to route `opts.context_hint` into the appropriate field.
    async fn transcribe_with_opts(
        &self,
        pcm: &[f32],
        sample_rate: u32,
        opts: &TranscribeOptions,
    ) -> Result<Transcription> {
        self.transcribe(pcm, sample_rate, opts.lang_override.as_deref()).await
    }

    /// Backend identifier for history / logging.
    fn name(&self) -> &'static str;

    fn supports_streaming(&self) -> bool {
        false
    }

    /// Optional best-effort warmup. Cloud backends should fire a cheap
    /// HEAD/GET to pay TCP+TLS+DNS off the hot path; local backends
    /// should mmap their model. Default impl is a no-op so most
    /// implementors don't need to override.
    ///
    /// Errors are non-fatal — callers log + continue. See latency
    /// plan task L2/L3.
    async fn prewarm(&self) -> Result<()> {
        Ok(())
    }

    /// True for backends that run entirely on the local machine
    /// (whisper.cpp, local-only Wyoming, future Vosk). Cloud backends
    /// (OpenAI, Groq, OpenRouter) leave this at the `false` default.
    ///
    /// Used by the orchestrator to decide whether the post-release
    /// "polishing" overlay should run the synthetic thinking
    /// animation: local backends take 1–3 s and benefit from active
    /// feedback; cloud backends finish sub-second and would just
    /// flash.
    fn is_local(&self) -> bool {
        false
    }
}
