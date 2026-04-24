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

#[async_trait]
pub trait SpeechToText: Send + Sync {
    /// One-shot transcription of a full PCM buffer (mono f32 @ `sample_rate`).
    async fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
        lang: Option<&str>,
    ) -> Result<Transcription>;

    /// Backend identifier for history / logging.
    fn name(&self) -> &'static str;

    fn supports_streaming(&self) -> bool {
        false
    }
}
