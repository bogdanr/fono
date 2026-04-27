// SPDX-License-Identifier: GPL-3.0-only
//! Voiced-frame stream: a `tokio::sync::broadcast` channel fed from cpal's
//! capture callback, gated by [`crate::vad::Vad`], that emits both raw
//! audio frames and segment-boundary events.
//!
//! Plan R2. Compiled only with the `streaming` cargo feature.
//!
//! The [`AudioFrameStream`] is a *consumer-side* abstraction — the
//! capture path is unchanged. Higher layers (orchestrator, equivalence
//! harness) push frames into a stream via [`AudioFrameStream::push`] and
//! subscribe with [`AudioFrameStream::subscribe`]. This deliberately
//! decouples the stream contract from cpal's callback so both real
//! capture *and* WAV-file replay (in the harness) reuse the same code
//! path.

use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use crate::vad::{Vad, VadDecision};

/// Default broadcast-channel capacity. 16 frames * 30 ms ≈ 480 ms of
/// buffered backpressure tolerance — generous, but cheap (each frame is
/// a `Vec<f32>` of ~480 samples).
pub const DEFAULT_CAPACITY: usize = 64;

/// One emission from the voiced-frame stream.
#[derive(Debug, Clone)]
pub enum FrameEvent {
    /// A voiced PCM frame (post-VAD-gating). 16 kHz mono f32.
    Voiced { pcm: Vec<f32>, elapsed: Duration },
    /// VAD detected a silence boundary that's long enough to mark the
    /// end of a logical segment. The streaming STT uses this as the
    /// trigger to run a finalize-lane pass.
    SegmentBoundary {
        /// 0-based segment index. Increments after every emitted boundary.
        segment_index: u32,
        elapsed: Duration,
    },
    /// Stream EOF — capture stopped or the upstream channel closed. The
    /// streaming STT uses this to flush any pending segment.
    Eof,
}

/// Configuration for the voiced-frame gate.
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// Frame size fed to the VAD, in samples (e.g. 480 for 30 ms at
    /// 16 kHz).
    pub frame_samples: usize,
    /// Number of *consecutive* silent frames that mark a segment
    /// boundary. At 30 ms / frame, 20 frames ≈ 600 ms of silence.
    pub silence_frames_for_boundary: usize,
    /// Broadcast-channel capacity (frames).
    pub channel_capacity: usize,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            frame_samples: 480,
            silence_frames_for_boundary: 20,
            channel_capacity: DEFAULT_CAPACITY,
        }
    }
}

/// VAD-gated, broadcast-fanout audio frame stream.
pub struct AudioFrameStream {
    cfg: StreamConfig,
    tx: broadcast::Sender<FrameEvent>,
    started_at: Instant,
    /// Number of consecutive silent frames observed since the last
    /// voiced frame. Used to detect segment boundaries.
    silence_run: usize,
    /// Have we seen at least one voiced frame in the current segment?
    /// Without this we'd emit a boundary on a permanently-silent
    /// stream, which is meaningless.
    saw_voice_in_segment: bool,
    /// Monotonic segment counter.
    segment_index: u32,
    /// Pending PCM remainder when an upstream chunk did not align to
    /// `frame_samples`.
    remainder: Vec<f32>,
}

impl AudioFrameStream {
    pub fn new(cfg: StreamConfig) -> Self {
        let (tx, _) = broadcast::channel(cfg.channel_capacity);
        Self {
            cfg,
            tx,
            started_at: Instant::now(),
            silence_run: 0,
            saw_voice_in_segment: false,
            segment_index: 0,
            remainder: Vec::new(),
        }
    }

    /// Subscribe a new consumer.
    pub fn subscribe(&self) -> broadcast::Receiver<FrameEvent> {
        self.tx.subscribe()
    }

    /// Push a chunk of PCM samples into the stream. The chunk may have
    /// arbitrary length; this method buffers an internal remainder so
    /// the VAD always sees aligned frames.
    pub fn push(&mut self, pcm: &[f32], vad: &mut dyn Vad) {
        // Accumulate then drain frame-by-frame.
        self.remainder.extend_from_slice(pcm);
        while self.remainder.len() >= self.cfg.frame_samples {
            let frame: Vec<f32> = self.remainder.drain(..self.cfg.frame_samples).collect();
            let elapsed = self.started_at.elapsed();
            let decision = vad.classify(&frame).unwrap_or(VadDecision::Silence);
            match decision {
                VadDecision::Speech => {
                    self.silence_run = 0;
                    self.saw_voice_in_segment = true;
                    let _ = self.tx.send(FrameEvent::Voiced {
                        pcm: frame,
                        elapsed,
                    });
                }
                VadDecision::Silence => {
                    self.silence_run += 1;
                    if self.saw_voice_in_segment
                        && self.silence_run >= self.cfg.silence_frames_for_boundary
                    {
                        let idx = self.segment_index;
                        self.segment_index += 1;
                        self.saw_voice_in_segment = false;
                        self.silence_run = 0;
                        let _ = self.tx.send(FrameEvent::SegmentBoundary {
                            segment_index: idx,
                            elapsed,
                        });
                    }
                }
            }
        }
    }

    /// Signal end-of-stream. Emits an `Eof` event so subscribers can
    /// flush any pending segment.
    pub fn finish(&mut self) {
        if self.saw_voice_in_segment {
            // Flush the trailing segment as a boundary so the streaming
            // STT runs its finalize pass.
            let idx = self.segment_index;
            self.segment_index += 1;
            let _ = self.tx.send(FrameEvent::SegmentBoundary {
                segment_index: idx,
                elapsed: self.started_at.elapsed(),
            });
            self.saw_voice_in_segment = false;
        }
        let _ = self.tx.send(FrameEvent::Eof);
    }

    #[must_use]
    pub fn segment_index(&self) -> u32 {
        self.segment_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vad::WebRtcVadStub;

    fn drain(rx: &mut broadcast::Receiver<FrameEvent>) -> Vec<FrameEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    #[test]
    fn voiced_frames_propagate() {
        let mut stream = AudioFrameStream::new(StreamConfig {
            frame_samples: 16,
            silence_frames_for_boundary: 4,
            channel_capacity: 64,
        });
        let mut rx = stream.subscribe();
        let mut vad = WebRtcVadStub::default();
        // Push two frames of "speech".
        let speech = vec![0.5_f32; 32];
        stream.push(&speech, &mut vad);
        let evs = drain(&mut rx);
        let voiced_count = evs
            .iter()
            .filter(|e| matches!(e, FrameEvent::Voiced { .. }))
            .count();
        assert_eq!(voiced_count, 2);
    }

    #[test]
    fn silence_after_speech_emits_segment_boundary() {
        let mut stream = AudioFrameStream::new(StreamConfig {
            frame_samples: 16,
            silence_frames_for_boundary: 3,
            channel_capacity: 64,
        });
        let mut rx = stream.subscribe();
        let mut vad = WebRtcVadStub::default();
        stream.push(&vec![0.5_f32; 16], &mut vad); // 1 voiced frame
        stream.push(&vec![0.0_f32; 16 * 3], &mut vad); // 3 silent frames → boundary
        let evs = drain(&mut rx);
        assert!(evs.iter().any(|e| matches!(
            e,
            FrameEvent::SegmentBoundary {
                segment_index: 0,
                ..
            }
        )));
    }

    #[test]
    fn eof_flushes_pending_segment_then_emits_eof() {
        let mut stream = AudioFrameStream::new(StreamConfig {
            frame_samples: 16,
            silence_frames_for_boundary: 100,
            channel_capacity: 64,
        });
        let mut rx = stream.subscribe();
        let mut vad = WebRtcVadStub::default();
        stream.push(&vec![0.5_f32; 32], &mut vad);
        stream.finish();
        let evs = drain(&mut rx);
        let last_two: Vec<&FrameEvent> = evs.iter().rev().take(2).collect();
        assert!(matches!(last_two[0], FrameEvent::Eof));
        assert!(matches!(last_two[1], FrameEvent::SegmentBoundary { .. }));
    }
}
