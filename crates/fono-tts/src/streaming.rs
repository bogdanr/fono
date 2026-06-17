// SPDX-License-Identifier: GPL-3.0-only
//! Provider- and transport-agnostic streaming-TTS driver.
//!
//! [`TextToSpeech::synthesize_stream`] produces a stream of [`TtsChunk`]s for
//! one utterance. This module decouples that *production* from *consumption*
//! via the [`PcmSink`] trait so the same driver feeds either a local audio
//! device (the binary's `LocalPlaybackSink`) or, later, a network transport for
//! server mode — without the streaming backends knowing which.
//!
//! [`stream_utterance`] is the glue: it pulls chunks from a backend, holds back
//! a small fixed prebuffer ([`DEFAULT_STREAM_PREBUFFER_MS`]) to absorb network
//! jitter, then pushes audio to the sink as it arrives. Cloud streaming
//! backends emit faster than realtime, so the prebuffer alone keeps the device
//! from underrunning; local engines are left on the batch `synthesize` +
//! enqueue path and never reach this driver.
//!
//! The [`PcmSink`] trait itself lives in `fono-audio` (the low-level audio
//! crate) so both the daemon and the MCP server can implement it; this module
//! re-exports it for convenience.

use anyhow::Result;
use futures::StreamExt;
use tracing::warn;

pub use fono_audio::PcmSink;

use crate::traits::{TextToSpeech, TtsChunk};

/// Milliseconds of audio the driver buffers before it starts playback. A small
/// fixed lead absorbs network jitter so the audio device doesn't underrun
/// mid-utterance while later chunks are still in flight; too large adds
/// latency. Only cloud streaming backends (which emit faster than realtime)
/// reach this driver, so a single tuned constant suffices — no config knob.
pub const DEFAULT_STREAM_PREBUFFER_MS: u32 = 300;

/// Drive one utterance from `tts` into `sink`, holding back
/// [`DEFAULT_STREAM_PREBUFFER_MS`] of audio before the first push to absorb
/// network jitter.
///
/// `on_first_audio` is invoked exactly once, the moment the first PCM actually
/// reaches the sink (i.e. the prebuffer is released and playback begins) — not
/// when the whole utterance finishes. Callers use it to mark the true
/// time-to-first-audio and flip UI state; pass `|| {}` if not needed.
///
/// Returns `Ok(true)` if any audio was produced and streamed, `Ok(false)` if
/// the backend yielded no audio (empty text / silent result), and `Err` if the
/// backend stream or a sink operation failed (the sink is aborted first).
pub async fn stream_utterance<F: FnMut() + Send>(
    tts: &dyn TextToSpeech,
    text: &str,
    voice: Option<&str>,
    lang: Option<&str>,
    sink: &mut dyn PcmSink,
    mut on_first_audio: F,
) -> Result<bool> {
    let prebuffer_ms = DEFAULT_STREAM_PREBUFFER_MS;
    let mut stream = tts.synthesize_stream(text, voice, lang).await?;

    let mut began = false;
    let mut started = false; // prebuffer threshold crossed → pushing live
    let mut fired = false; // on_first_audio invoked
    let mut buffered: Vec<(Vec<f32>, u32)> = Vec::new();
    let mut buffered_samples: usize = 0;
    let mut prebuffer_target: usize = 0; // samples; set from first chunk's rate
    let mut any_audio = false;

    while let Some(item) = stream.next().await {
        let chunk: TtsChunk = match item {
            Ok(c) => c,
            Err(e) => {
                if began {
                    let _ = sink.abort().await;
                }
                return Err(e);
            }
        };

        let is_final = chunk.is_final;
        if !chunk.pcm.is_empty() {
            any_audio = true;
            if prebuffer_target == 0 {
                prebuffer_target =
                    (u64::from(chunk.sample_rate) * u64::from(prebuffer_ms) / 1000) as usize;
            }
            if !began {
                sink.begin().await?;
                began = true;
            }
            if started {
                if let Err(e) = sink.push(chunk.pcm, chunk.sample_rate).await {
                    let _ = sink.abort().await;
                    return Err(e);
                }
            } else {
                buffered_samples += chunk.pcm.len();
                buffered.push((chunk.pcm, chunk.sample_rate));
                if buffered_samples >= prebuffer_target {
                    if !fired {
                        on_first_audio();
                        fired = true;
                    }
                    if let Err(e) = flush_buffer(sink, &mut buffered).await {
                        let _ = sink.abort().await;
                        return Err(e);
                    }
                    started = true;
                }
            }
        }

        if is_final {
            break;
        }
    }

    // Flush any audio still held in the prebuffer (utterance shorter than the
    // prebuffer target, or it ended before the threshold was crossed).
    if began && !buffered.is_empty() {
        if !fired {
            on_first_audio();
        }
        if let Err(e) = flush_buffer(sink, &mut buffered).await {
            let _ = sink.abort().await;
            return Err(e);
        }
    }

    if began {
        if let Err(e) = sink.end().await {
            warn!(target: "fono::tts::streaming", error = %e, "sink end failed");
            return Err(e);
        }
    }
    Ok(any_audio)
}

async fn flush_buffer(sink: &mut dyn PcmSink, buffered: &mut Vec<(Vec<f32>, u32)>) -> Result<()> {
    for (pcm, rate) in buffered.drain(..) {
        sink.push(pcm, rate).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::TtsAudio;
    use async_trait::async_trait;
    use futures::stream::{self, BoxStream};

    /// A backend that emits a fixed list of chunks for any text.
    struct ScriptedStream {
        chunks: Vec<TtsChunk>,
    }

    #[async_trait]
    impl TextToSpeech for ScriptedStream {
        async fn synthesize(
            &self,
            _text: &str,
            _voice: Option<&str>,
            _lang: Option<&str>,
        ) -> Result<TtsAudio> {
            unreachable!("streaming path should not call synthesize")
        }
        fn supports_streaming(&self) -> bool {
            true
        }
        async fn synthesize_stream(
            &self,
            _text: &str,
            _voice: Option<&str>,
            _lang: Option<&str>,
        ) -> Result<BoxStream<'static, Result<TtsChunk>>> {
            let chunks = self.chunks.clone();
            Ok(Box::pin(stream::iter(chunks.into_iter().map(Ok))))
        }
        fn name(&self) -> &'static str {
            "scripted"
        }
        fn native_sample_rate(&self) -> u32 {
            24_000
        }
    }

    /// Records the begin/push/end sequence for assertions.
    #[derive(Default)]
    struct RecordingSink {
        began: bool,
        ended: bool,
        aborted: bool,
        pushes: Vec<usize>, // sample count per push
    }

    #[async_trait]
    impl PcmSink for RecordingSink {
        async fn begin(&mut self) -> Result<()> {
            self.began = true;
            Ok(())
        }
        async fn push(&mut self, pcm: Vec<f32>, _sample_rate: u32) -> Result<()> {
            self.pushes.push(pcm.len());
            Ok(())
        }
        async fn end(&mut self) -> Result<()> {
            self.ended = true;
            Ok(())
        }
        async fn abort(&mut self) -> Result<()> {
            self.aborted = true;
            Ok(())
        }
    }

    fn chunk(n: usize, is_final: bool) -> TtsChunk {
        TtsChunk { pcm: vec![0.1; n], sample_rate: 24_000, is_final }
    }

    #[tokio::test]
    async fn prebuffer_coalesces_then_streams_live() {
        // 24 kHz, 300 ms prebuffer = 7200 samples. The first three 2000-sample
        // chunks (6000) stay buffered; the final chunk crosses the threshold on
        // the tail flush. All audio still reaches the sink.
        let tts = ScriptedStream {
            chunks: vec![
                chunk(2000, false),
                chunk(2000, false),
                chunk(2000, false),
                chunk(2000, true),
            ],
        };
        let mut sink = RecordingSink::default();
        let produced = stream_utterance(&tts, "hi", None, None, &mut sink, || {}).await.unwrap();
        assert!(produced);
        assert!(sink.began);
        assert!(sink.ended);
        assert!(!sink.aborted);
        // All 8000 samples reach the sink, regardless of buffering boundaries.
        assert_eq!(sink.pushes.iter().sum::<usize>(), 8000);
    }

    #[tokio::test]
    async fn short_utterance_under_prebuffer_still_flushes() {
        // One 100-sample final chunk never reaches the 7200-sample threshold,
        // but the tail flush must still deliver it and end cleanly.
        let tts = ScriptedStream { chunks: vec![chunk(100, true)] };
        let mut sink = RecordingSink::default();
        let produced = stream_utterance(&tts, "hi", None, None, &mut sink, || {}).await.unwrap();
        assert!(produced);
        assert!(sink.began);
        assert!(sink.ended);
        assert_eq!(sink.pushes.iter().sum::<usize>(), 100);
    }

    #[tokio::test]
    async fn empty_stream_produces_no_audio_and_never_begins() {
        let tts = ScriptedStream { chunks: vec![chunk(0, true)] };
        let mut sink = RecordingSink::default();
        let produced = stream_utterance(&tts, "", None, None, &mut sink, || {}).await.unwrap();
        assert!(!produced);
        assert!(!sink.began, "no begin() when there is no audio");
        assert!(!sink.ended);
    }

    #[tokio::test]
    async fn on_first_audio_fires_exactly_once_at_first_push() {
        // Several chunks, but the callback must fire once — when the prebuffer
        // first releases to the sink, not per chunk and not at stream end.
        let tts = ScriptedStream {
            chunks: vec![chunk(4000, false), chunk(4000, false), chunk(4000, true)],
        };
        let mut sink = RecordingSink::default();
        let mut fires = 0u32;
        let produced =
            stream_utterance(&tts, "hi", None, None, &mut sink, || fires += 1).await.unwrap();
        assert!(produced);
        assert_eq!(fires, 1, "callback must fire exactly once");
    }

    #[tokio::test]
    async fn on_first_audio_never_fires_without_audio() {
        let tts = ScriptedStream { chunks: vec![chunk(0, true)] };
        let mut sink = RecordingSink::default();
        let mut fires = 0u32;
        let produced =
            stream_utterance(&tts, "", None, None, &mut sink, || fires += 1).await.unwrap();
        assert!(!produced);
        assert_eq!(fires, 0, "no callback when there is no audio");
    }
}
