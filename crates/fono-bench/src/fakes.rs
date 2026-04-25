// SPDX-License-Identifier: GPL-3.0-only
//! Test/bench fakes — implementations of `SpeechToText` and
//! `TextFormatter` that don't touch the network or load any model.
//!
//! These are exposed publicly so the criterion benchmark and the
//! integration smoke test (and downstream consumers writing their own
//! latency tests) can build a deterministic pipeline.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

use fono_llm::traits::{FormatContext, TextFormatter};
use fono_stt::traits::{SpeechToText, Transcription};

/// STT that returns a canned transcript after an optional fixed delay.
/// Useful for measuring orchestrator overhead in isolation.
pub struct FakeStt {
    pub canned: String,
    pub delay: Duration,
}

impl FakeStt {
    pub fn new(canned: impl Into<String>) -> Self {
        Self {
            canned: canned.into(),
            delay: Duration::ZERO,
        }
    }
    pub fn with_delay(canned: impl Into<String>, delay: Duration) -> Self {
        Self {
            canned: canned.into(),
            delay,
        }
    }
}

#[async_trait]
impl SpeechToText for FakeStt {
    async fn transcribe(
        &self,
        _pcm: &[f32],
        _sample_rate: u32,
        lang: Option<&str>,
    ) -> Result<Transcription> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        Ok(Transcription {
            text: self.canned.clone(),
            language: lang.map(str::to_string),
            duration_ms: None,
        })
    }
    fn name(&self) -> &'static str {
        "fake-stt"
    }
}

/// LLM that echoes the raw text after an optional delay (zero-edit
/// cleanup — useful for bounding orchestrator overhead).
pub struct FakeLlm {
    pub delay: Duration,
}

impl FakeLlm {
    pub fn new() -> Self {
        Self {
            delay: Duration::ZERO,
        }
    }
    pub fn with_delay(delay: Duration) -> Self {
        Self { delay }
    }
}

impl Default for FakeLlm {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TextFormatter for FakeLlm {
    async fn format(&self, raw: &str, _ctx: &FormatContext) -> Result<String> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        Ok(raw.trim().to_string())
    }
    fn name(&self) -> &'static str {
        "fake-llm"
    }
}
