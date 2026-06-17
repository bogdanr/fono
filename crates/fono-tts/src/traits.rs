// SPDX-License-Identifier: GPL-3.0-only
//! TTS trait definition.

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::{self, BoxStream};

/// One synthesised utterance: mono `f32` PCM in -1.0..1.0 at
/// `sample_rate` Hz. Backends pick whatever native rate their model
/// produces (16000 / 22050 / 24000 are common); the playback layer
/// resamples to the output device's rate.
#[derive(Debug, Clone)]
pub struct TtsAudio {
    pub pcm: Vec<f32>,
    pub sample_rate: u32,
}

/// One streamed slice of a single utterance, yielded by
/// [`TextToSpeech::synthesize_stream`]. `pcm` is mono `f32` in -1.0..1.0 at
/// `sample_rate` Hz (same contract as [`TtsAudio`]); `is_final` marks the last
/// chunk of the utterance so the playback driver can flush and stop.
///
/// Intra-utterance streaming lets the first audio of a sentence play before the
/// whole sentence is synthesised — cutting time-to-first-audio on cloud
/// backends that emit faster than realtime. Empty / batch backends produce a
/// single `is_final` chunk via the default [`TextToSpeech::synthesize_stream`].
#[derive(Debug, Clone)]
pub struct TtsChunk {
    pub pcm: Vec<f32>,
    pub sample_rate: u32,
    pub is_final: bool,
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

    /// Whether this backend overrides [`Self::synthesize_stream`] with a real
    /// incremental wire format. Callers use it to decide between the streaming
    /// playback path and the existing batch `synthesize` + `enqueue` path.
    ///
    /// Defaults to `false`: the default `synthesize_stream` just wraps
    /// `synthesize`, so reporting `true` requires a genuine streaming override
    /// (cloud backends only — see
    /// `plans/2026-06-17-cloud-streaming-tts-v2.md`).
    fn supports_streaming(&self) -> bool {
        false
    }

    /// Synthesise `text` as a stream of [`TtsChunk`]s for the same utterance.
    ///
    /// The default implementation calls [`Self::synthesize`] and yields exactly
    /// one terminal chunk, so every batch backend (local Piper/Kokoro, batch
    /// cloud backends) works unchanged. Streaming-capable backends override
    /// this to emit incremental chunks as the model produces audio and MUST
    /// also return `true` from [`Self::supports_streaming`].
    ///
    /// Empty `text` yields a single empty `is_final` chunk (mirrors the
    /// empty-PCM contract of `synthesize`).
    async fn synthesize_stream(
        &self,
        text: &str,
        voice: Option<&str>,
        lang: Option<&str>,
    ) -> Result<BoxStream<'static, Result<TtsChunk>>> {
        let audio = self.synthesize(text, voice, lang).await?;
        let chunk = TtsChunk { pcm: audio.pcm, sample_rate: audio.sample_rate, is_final: true };
        Ok(Box::pin(stream::once(async move { Ok(chunk) })))
    }

    /// Backend identifier for history / logging.
    fn name(&self) -> &'static str;

    /// Hint for the playback layer to size its resampler. Backends
    /// that vary per voice should report the most common rate; the
    /// playback layer keys resamplers on the actual `TtsAudio.sample_rate`
    /// so a wrong hint here is at most a missed warmup.
    fn native_sample_rate(&self) -> u32;

    /// Optional best-effort warmup — pay TCP+TLS+DNS or model load off
    /// the hot path before the user's first F8 press. Errors are
    /// non-fatal; the caller should log + continue.
    async fn prewarm(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    struct Batch;

    #[async_trait]
    impl TextToSpeech for Batch {
        async fn synthesize(
            &self,
            text: &str,
            _voice: Option<&str>,
            _lang: Option<&str>,
        ) -> Result<TtsAudio> {
            // Two samples per char, so the test can tell content from length.
            let pcm = if text.is_empty() { Vec::new() } else { vec![0.5; text.len() * 2] };
            Ok(TtsAudio { pcm, sample_rate: 24_000 })
        }
        fn name(&self) -> &'static str {
            "batch"
        }
        fn native_sample_rate(&self) -> u32 {
            24_000
        }
    }

    #[tokio::test]
    async fn default_stream_yields_one_final_chunk_equal_to_synthesize() {
        let b = Batch;
        assert!(!b.supports_streaming());
        let audio = b.synthesize("hello", None, None).await.unwrap();
        let chunks: Vec<_> =
            b.synthesize_stream("hello", None, None).await.unwrap().collect().await;
        assert_eq!(chunks.len(), 1);
        let only = chunks.into_iter().next().unwrap().unwrap();
        assert!(only.is_final);
        assert_eq!(only.sample_rate, audio.sample_rate);
        assert_eq!(only.pcm, audio.pcm);
    }

    #[tokio::test]
    async fn default_stream_empty_text_yields_one_empty_final_chunk() {
        let chunks: Vec<_> = Batch.synthesize_stream("", None, None).await.unwrap().collect().await;
        assert_eq!(chunks.len(), 1);
        let only = chunks.into_iter().next().unwrap().unwrap();
        assert!(only.is_final);
        assert!(only.pcm.is_empty());
    }
}
