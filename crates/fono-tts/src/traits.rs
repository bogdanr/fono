// SPDX-License-Identifier: GPL-3.0-only
//! TTS trait definition.

use anyhow::Result;
use async_trait::async_trait;

/// One synthesised utterance: mono `f32` PCM in -1.0..1.0 at
/// `sample_rate` Hz. Backends pick whatever native rate their model
/// produces (16000 / 22050 / 24000 are common); the playback layer
/// resamples to the output device's rate.
#[derive(Debug, Clone)]
pub struct TtsAudio {
    pub pcm: Vec<f32>,
    pub sample_rate: u32,
}

#[async_trait]
pub trait TextToSpeech: Send + Sync {
    /// Synthesise `text` into one PCM utterance. `voice` and `lang`
    /// are best-effort hints — backends that pick voice from config
    /// alone may ignore them. Empty `text` MUST return an empty PCM
    /// buffer (callers rely on this for the "stream-end with no
    /// trailing partial" case).
    async fn synthesize(
        &self,
        text: &str,
        voice: Option<&str>,
        lang: Option<&str>,
    ) -> Result<TtsAudio>;

    /// Backend identifier for history / logging.
    fn name(&self) -> &'static str;

    /// Hint for the playback layer to size its resampler. Backends
    /// that vary per voice should report the most common rate; the
    /// playback layer keys resamplers on the actual `TtsAudio.sample_rate`
    /// so a wrong hint here is at most a missed warmup.
    fn native_sample_rate(&self) -> u32;

    /// Optional best-effort warmup — pay TCP+TLS+DNS or model load off
    /// the hot path before the user's first F10 press. Errors are
    /// non-fatal; the caller should log + continue.
    async fn prewarm(&self) -> Result<()> {
        Ok(())
    }
}
