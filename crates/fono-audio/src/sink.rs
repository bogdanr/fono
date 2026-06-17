// SPDX-License-Identifier: GPL-3.0-only
//! [`PcmSink`] — a transport-agnostic consumer of streamed PCM, plus the
//! local-device implementation [`LocalPlaybackSink`].
//!
//! The streaming-TTS driver (`fono_tts::stream_utterance`) produces PCM chunks
//! for one utterance and pushes them to a `PcmSink`, without knowing whether
//! the audio plays on the local device or is forwarded to a remote client in
//! server mode. The trait lives here in `fono-audio` (the low-level audio
//! crate) so both the daemon and the MCP server can reach it, and so a future
//! network sink can implement it from its own crate.

use anyhow::Result;
use async_trait::async_trait;

use crate::playback::AudioPlayback;

/// A consumer of streamed mono `f32` PCM for one utterance.
///
/// The driver calls [`Self::begin`] once, [`Self::push`] for each chunk, then
/// [`Self::end`] (normal completion) or [`Self::abort`] (mid-stream failure /
/// cancellation). `sample_rate` is constant for a single begin→end session.
#[async_trait]
pub trait PcmSink: Send {
    /// Open a session. Called exactly once before any [`Self::push`].
    async fn begin(&mut self) -> Result<()>;
    /// Append one mono `f32` PCM slice (in -1.0..1.0) at `sample_rate` Hz.
    async fn push(&mut self, pcm: Vec<f32>, sample_rate: u32) -> Result<()>;
    /// Close the session normally and let queued audio drain.
    async fn end(&mut self) -> Result<()>;
    /// Abort the session, discarding any unplayed audio. Default: [`Self::end`].
    async fn abort(&mut self) -> Result<()> {
        self.end().await
    }
}

/// Streams PCM chunks to the local audio device via [`AudioPlayback`]'s gapless
/// streaming session.
///
/// `begin` opens a gapless session, `push` appends a chunk, `end` closes it and
/// lets the audio drain, and `abort` tears it down immediately (stopping the
/// playback worker). The worker's pending counter tracks the session as a
/// single unit, so callers can poll [`AudioPlayback::is_idle`] to await drain
/// exactly as with `enqueue`.
pub struct LocalPlaybackSink {
    playback: AudioPlayback,
    open: bool,
}

impl LocalPlaybackSink {
    #[must_use]
    pub fn new(playback: AudioPlayback) -> Self {
        Self { playback, open: false }
    }
}

#[async_trait]
impl PcmSink for LocalPlaybackSink {
    async fn begin(&mut self) -> Result<()> {
        self.playback.begin_stream()?;
        self.open = true;
        Ok(())
    }

    async fn push(&mut self, pcm: Vec<f32>, sample_rate: u32) -> Result<()> {
        self.playback.push_stream(pcm, sample_rate)
    }

    async fn end(&mut self) -> Result<()> {
        if self.open {
            self.open = false;
            self.playback.end_stream()?;
        }
        Ok(())
    }

    async fn abort(&mut self) -> Result<()> {
        // `stop()` drains the queue, kills any open session, and resets the
        // pending counter to zero — so we must NOT also send `end_stream`
        // afterwards (that would underflow the counter). Clearing `open`
        // makes a later `end()` a no-op.
        if self.open {
            self.open = false;
            self.playback.stop();
        }
        Ok(())
    }
}
