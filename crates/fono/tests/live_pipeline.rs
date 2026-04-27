// SPDX-License-Identifier: GPL-3.0-only
//! End-to-end integration test for the live-dictation pipeline. Plan v6
//! R10.2.
//!
//! Drives [`fono::live::LiveSession`] with a synthetic [`StreamingStt`]
//! that emits a scripted `Preview` → `Finalize` sequence per segment,
//! and a [`fono::live::Pump`] fed by hand-crafted silence/voice
//! transitions that make the VAD-driven segmentation in
//! `fono-audio::AudioFrameStream` fire deterministic boundaries.
//!
//! Compiled only with the `interactive` feature (the streaming primitives
//! it exercises live behind the same gate). Invoke via:
//!
//! ```bash
//! cargo test --workspace --tests --features fono/interactive
//! ```

#![cfg(feature = "interactive")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use fono_audio::StreamConfig;
use fono_core::QualityFloor;
use fono_stt::{StreamFrame, StreamingStt, TranscriptUpdate};

use fono::live::{HeuristicConfig, LiveSession, Pump};

/// Per-segment script: a list of preview texts followed by the
/// authoritative finalize text. Constructed by the test.
#[derive(Debug, Clone)]
struct SegmentScript {
    previews: Vec<String>,
    finalize: String,
}

impl SegmentScript {
    fn new<P, S, F>(previews: P, finalize: F) -> Self
    where
        P: IntoIterator<Item = S>,
        S: Into<String>,
        F: Into<String>,
    {
        Self {
            previews: previews.into_iter().map(Into::into).collect(),
            finalize: finalize.into(),
        }
    }
}

/// Streaming STT fake. Each `SegmentBoundary` (or the final `Eof`)
/// triggers emission of the next scripted segment's previews then its
/// finalize update. PCM frames are counted but otherwise ignored.
struct FakeStreamingStt {
    script: Mutex<std::vec::IntoIter<SegmentScript>>,
}

impl FakeStreamingStt {
    fn new(script: Vec<SegmentScript>) -> Self {
        Self {
            script: Mutex::new(script.into_iter()),
        }
    }
}

#[async_trait]
impl StreamingStt for FakeStreamingStt {
    async fn stream_transcribe(
        &self,
        mut frames: BoxStream<'static, StreamFrame>,
        _sample_rate: u32,
        _lang: Option<String>,
    ) -> Result<BoxStream<'static, TranscriptUpdate>> {
        let (tx, rx) = mpsc::unbounded_channel::<TranscriptUpdate>();
        // Drain the script up-front so the spawned task is fully owned.
        let mut remaining: Vec<SegmentScript> = {
            let mut guard = self.script.lock().expect("script mutex");
            guard.by_ref().collect()
        };

        tokio::spawn(async move {
            let mut segment_index: u32 = 0;
            // Track whether we've seen any voiced PCM in this segment so
            // an Eof flush only fires a finalize if the audio side had
            // an open segment.
            let mut seg_has_audio = false;
            let mut elapsed = Duration::from_millis(0);
            while let Some(frame) = frames.next().await {
                elapsed += Duration::from_millis(1);
                match frame {
                    StreamFrame::Pcm(_) => {
                        seg_has_audio = true;
                    }
                    StreamFrame::SegmentBoundary => {
                        if seg_has_audio {
                            emit_segment(&tx, segment_index, &mut remaining, elapsed);
                            segment_index += 1;
                            seg_has_audio = false;
                        }
                    }
                    StreamFrame::Eof => {
                        if seg_has_audio {
                            emit_segment(&tx, segment_index, &mut remaining, elapsed);
                        }
                        break;
                    }
                }
            }
        });

        Ok(UnboundedReceiverStream::new(rx).boxed())
    }

    fn name(&self) -> &'static str {
        "fake-streaming-stt"
    }
}

fn emit_segment(
    tx: &mpsc::UnboundedSender<TranscriptUpdate>,
    idx: u32,
    remaining: &mut Vec<SegmentScript>,
    elapsed: Duration,
) {
    if remaining.is_empty() {
        return;
    }
    let seg = remaining.remove(0);
    for (i, p) in seg.previews.iter().enumerate() {
        let upd = TranscriptUpdate::preview(idx, p.clone(), elapsed + Duration::from_millis(i as u64));
        if tx.send(upd).is_err() {
            return;
        }
    }
    let upd = TranscriptUpdate::finalize(
        idx,
        seg.finalize,
        elapsed + Duration::from_millis(seg.previews.len() as u64 + 1),
    );
    let _ = tx.send(upd);
}

/// Tight test config so a handful of frames trigger boundaries quickly.
fn test_stream_config() -> StreamConfig {
    StreamConfig {
        frame_samples: 16,
        silence_frames_for_boundary: 2,
        channel_capacity: 256,
    }
}

/// Frame-sized "voiced" PCM (`0.5` is well above `WebRtcVadStub`'s
/// 0.01 energy threshold).
fn voiced_frame() -> Vec<f32> {
    vec![0.5_f32; 16]
}

/// Frame-sized silence.
fn silent_frame() -> Vec<f32> {
    vec![0.0_f32; 16]
}

#[tokio::test]
async fn live_session_concatenates_finalized_segments() {
    let stt = Arc::new(FakeStreamingStt::new(vec![
        SegmentScript::new(["hell", "hello"], "hello"),
        SegmentScript::new(["wor"], "world"),
    ]));

    let mut pump = Pump::new(test_stream_config());
    let frame_rx = pump.take_receiver().expect("take_receiver");

    let session = LiveSession::new(stt, 16_000);
    let task = tokio::spawn(session.run(frame_rx, QualityFloor::Max));

    // Segment 0: 4 voiced frames, then 2 silent frames -> SegmentBoundary(0).
    for _ in 0..4 {
        pump.push(&voiced_frame());
    }
    for _ in 0..2 {
        pump.push(&silent_frame());
    }
    // Segment 1: 4 voiced frames, then finish() -> trailing
    // SegmentBoundary(1) + Eof.
    for _ in 0..4 {
        pump.push(&voiced_frame());
    }
    pump.finish();
    drop(pump);

    let transcript = task
        .await
        .expect("join")
        .expect("run result");

    assert_eq!(
        transcript.committed, "hello world",
        "committed text should concatenate finalize-lane segments"
    );
    assert_eq!(transcript.segments_finalized, 2);
    assert!(
        transcript.last_preview.is_none(),
        "last_preview must be cleared after a Finalize lands"
    );
}

#[tokio::test]
async fn live_session_returns_empty_on_early_finish_with_no_voice() {
    // Cancellation-shaped path: finish before any voiced frame arrives.
    let stt = Arc::new(FakeStreamingStt::new(vec![SegmentScript::new(
        ["should-never-emit"],
        "should-never-emit",
    )]));

    let mut pump = Pump::new(test_stream_config());
    let frame_rx = pump.take_receiver().expect("take_receiver");

    let session = LiveSession::new(stt, 16_000);
    let task = tokio::spawn(session.run(frame_rx, QualityFloor::Max));

    // No pushes — straight to finish, then drop.
    pump.finish();
    drop(pump);

    let transcript = task
        .await
        .expect("join")
        .expect("run result");

    assert!(
        transcript.committed.is_empty(),
        "committed should be empty when no voiced audio was pumped"
    );
    assert_eq!(transcript.segments_finalized, 0);
    assert!(transcript.last_preview.is_none());
}

/// R10.6: heuristics-off vs default-heuristics path produce identical
/// `committed` text on a fixture that does not trigger any heuristic
/// ("hello world" — no filler, no dangling-word suffix, no prosody-
/// driven boundary delay since prosody is off by default anyway). This
/// is the additive-only contract: heuristics never *change* the
/// transcript, only delay segment boundaries.
#[tokio::test]
async fn heuristics_are_additive_when_no_trigger_present() {
    async fn run_once(heur: HeuristicConfig) -> String {
        let stt = Arc::new(FakeStreamingStt::new(vec![
            SegmentScript::new(["hell", "hello"], "hello"),
            SegmentScript::new(["wor"], "world"),
        ]));
        let mut pump = Pump::new(test_stream_config());
        let frame_rx = pump.take_receiver().expect("take_receiver");
        let session = LiveSession::new(stt, 16_000).with_heuristics(heur);
        let task = tokio::spawn(session.run(frame_rx, QualityFloor::Max));
        for _ in 0..4 {
            pump.push(&voiced_frame());
        }
        for _ in 0..2 {
            pump.push(&silent_frame());
        }
        for _ in 0..4 {
            pump.push(&voiced_frame());
        }
        pump.finish();
        drop(pump);
        let transcript = task.await.expect("join").expect("run");
        transcript.committed
    }

    let off = run_once(HeuristicConfig::all_off()).await;
    let on = run_once(HeuristicConfig::default()).await;
    assert_eq!(
        off, on,
        "heuristics must be additive: same input → same committed text"
    );
    assert_eq!(off, "hello world");
}
