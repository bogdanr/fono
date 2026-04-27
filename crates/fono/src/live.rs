// SPDX-License-Identifier: GPL-3.0-only
//! Live-dictation orchestrator glue. Plan R7.4.
//!
//! Wires:
//!
//! * `fono-audio::AudioFrameStream` (R2)
//! * `fono-stt::StreamingStt` (R1/R3)
//! * `fono-overlay::OverlayHandle` (R5)
//! * `fono-core::BudgetController` (R12)
//!
//! into a single [`LiveSession`] that the daemon drives when the FSM
//! emits [`fono_hotkey::HotkeyEvent::StartLiveDictation`].
//!
//! Slice A intentionally keeps this module thin: a daemon that wants to
//! support both batch and live mode reads `cfg.interactive.enabled` at
//! start / on `Reload`; if true *and* this module is compiled in, it
//! routes hotkey actions to the `LiveHold*` / `LiveToggle*` variants.
//! Otherwise the existing batch path runs unchanged. The behaviour
//! contract is documented in `docs/interactive.md`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use fono_audio::{AudioFrameStream, FrameEvent, StreamConfig, Vad, WebRtcVadStub};
use fono_core::{BudgetController, BudgetVerdict, PriceTable, QualityFloor};
use fono_overlay::{OverlayHandle, OverlayState};
use fono_stt::{StreamFrame, StreamingStt, TranscriptUpdate, UpdateLane};
use futures::stream::{BoxStream, StreamExt};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{debug, info, instrument, warn};

/// Aggregated state of a live-dictation session that the orchestrator
/// reads back when the user releases the hotkey.
#[derive(Debug, Clone, Default)]
pub struct LiveTranscript {
    /// Concatenation of every `Finalize` segment seen, in order.
    pub committed: String,
    /// The most recent `Preview` text (per segment), kept around so the
    /// orchestrator can show it to the user even if no `Finalize`
    /// arrived (e.g. cancelled mid-segment).
    pub last_preview: Option<String>,
    /// Number of segments finalized.
    pub segments_finalized: u32,
}

/// Builder for a live-dictation session.
pub struct LiveSession {
    stt: Arc<dyn StreamingStt>,
    overlay: Option<OverlayHandle>,
    sample_rate: u32,
    language: Option<String>,
    budget: BudgetController,
    stream_cfg: StreamConfig,
}

impl LiveSession {
    pub fn new(stt: Arc<dyn StreamingStt>, sample_rate: u32) -> Self {
        Self {
            stt,
            overlay: None,
            sample_rate,
            language: None,
            budget: BudgetController::local(),
            stream_cfg: StreamConfig::default(),
        }
    }

    #[must_use]
    pub fn with_overlay(mut self, h: OverlayHandle) -> Self {
        self.overlay = Some(h);
        self
    }

    #[must_use]
    pub fn with_language(mut self, lang: Option<String>) -> Self {
        self.language = lang;
        self
    }

    #[must_use]
    pub fn with_budget(mut self, b: BudgetController) -> Self {
        self.budget = b;
        self
    }

    #[must_use]
    pub fn with_stream_config(mut self, c: StreamConfig) -> Self {
        self.stream_cfg = c;
        self
    }

    /// Run the session against an already-subscribed broadcast receiver
    /// of [`FrameEvent`]s. The caller is expected to obtain the receiver
    /// from [`Pump::take_receiver`] *before* pushing any audio so that
    /// pushed frames are not lost (`tokio::sync::broadcast` discards
    /// messages sent while no receivers are subscribed and only delivers
    /// post-subscribe messages to a fresh subscriber).
    ///
    /// Typical usage spawns a task that drives the pump while `run` is
    /// awaited:
    ///
    /// ```ignore
    /// let mut pump = Pump::new(StreamConfig::default());
    /// let frame_rx = pump.take_receiver();
    /// let task = tokio::spawn(session.run(frame_rx, QualityFloor::Max));
    /// // … pump.push(pcm); pump.finish(); drop(pump);
    /// let transcript = task.await??;
    /// ```
    ///
    /// `quality_floor` is plumbed for the future R12.5 finalize-skip
    /// extension; Slice A treats it as informational only (the current
    /// finalize lane always runs — see ADR 0009).
    #[instrument(skip_all, fields(stt = self.stt.name(), rate = self.sample_rate))]
    pub async fn run(
        self,
        mut frame_rx: broadcast::Receiver<FrameEvent>,
        _quality_floor: QualityFloor,
    ) -> Result<LiveTranscript> {
        let Self {
            stt,
            overlay,
            sample_rate,
            language,
            budget,
            stream_cfg: _,
        } = self;
        let budget = Arc::new(Mutex::new(budget));

        // Translate FrameEvent -> StreamFrame and feed the StreamingStt.
        let (sf_tx, sf_rx) = mpsc::unbounded_channel::<StreamFrame>();
        let budget_for_pump = Arc::clone(&budget);
        let translator = tokio::spawn(async move {
            loop {
                match frame_rx.recv().await {
                    Ok(FrameEvent::Voiced { pcm, .. }) => {
                        // Charge the budget controller for the audio
                        // duration we're about to send. Slice A's
                        // local-only path returns Continue every time;
                        // the verdict is recorded for telemetry.
                        let dur = Duration::from_secs_f32(
                            pcm.len() as f32 / sample_rate as f32,
                        );
                        let verdict = budget_for_pump
                            .lock()
                            .map(|mut b| b.record(dur))
                            .unwrap_or(BudgetVerdict::Continue);
                        if matches!(verdict, BudgetVerdict::StopStreaming) {
                            warn!("budget controller asked to stop streaming");
                            let _ = sf_tx.send(StreamFrame::Eof);
                            return;
                        }
                        if sf_tx.send(StreamFrame::Pcm(pcm)).is_err() {
                            return;
                        }
                    }
                    Ok(FrameEvent::SegmentBoundary { .. }) => {
                        if sf_tx.send(StreamFrame::SegmentBoundary).is_err() {
                            return;
                        }
                    }
                    Ok(FrameEvent::Eof) => {
                        let _ = sf_tx.send(StreamFrame::Eof);
                        return;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("live: dropped {n} frames (lagged consumer)");
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });

        let frames_stream: BoxStream<'static, StreamFrame> =
            UnboundedReceiverStream::new(sf_rx).boxed();
        let mut updates = stt
            .stream_transcribe(frames_stream, sample_rate, language)
            .await?;

        let mut transcript = LiveTranscript::default();
        if let Some(o) = overlay.as_ref() {
            o.set_state(OverlayState::LiveDictating);
        }
        while let Some(upd) = updates.next().await {
            apply_update(&mut transcript, &upd);
            if let Some(o) = overlay.as_ref() {
                let display = preview_display(&transcript, &upd);
                o.update_text(display);
            }
            debug!(
                "live update: lane={:?} seg={} chars={}",
                upd.lane,
                upd.segment_index,
                upd.text.len()
            );
        }
        if let Some(o) = overlay.as_ref() {
            o.set_state(OverlayState::Hidden);
        }
        info!(
            "live session done: {} segments, {} committed chars",
            transcript.segments_finalized,
            transcript.committed.len()
        );
        translator.abort();
        Ok(transcript)
    }
}

/// Frontend that owns the [`AudioFrameStream`] and lets the caller push
/// PCM and signal end-of-input.
///
/// The pump pre-subscribes a single "primary" broadcast receiver at
/// construction time so the caller can hand that receiver to
/// [`LiveSession::run`] *before* any frames are pushed. This avoids the
/// otherwise-easy mistake of pushing frames into a broadcast channel
/// with zero subscribers and losing them silently.
pub struct Pump {
    stream: AudioFrameStream,
    vad: Box<dyn Vad>,
    rx: Option<broadcast::Receiver<FrameEvent>>,
}

impl Pump {
    #[must_use]
    pub fn new(cfg: StreamConfig) -> Self {
        let stream = AudioFrameStream::new(cfg);
        let rx = stream.subscribe();
        Self {
            stream,
            vad: Box::new(WebRtcVadStub::default()),
            rx: Some(rx),
        }
    }

    pub fn push(&mut self, pcm: &[f32]) {
        self.stream.push(pcm, self.vad.as_mut());
    }

    pub fn finish(&mut self) {
        self.stream.finish();
    }

    /// Take the pre-subscribed primary receiver. Callable exactly once
    /// per pump; panics in debug / returns an error if called twice.
    pub fn take_receiver(&mut self) -> Result<broadcast::Receiver<FrameEvent>> {
        self.rx
            .take()
            .ok_or_else(|| anyhow!("Pump::take_receiver called twice"))
    }

    /// Subscribe an *additional* receiver. Note: any frames pushed
    /// before this call are not visible to the new receiver — only use
    /// this for fanning out to a passive observer (logger, recorder).
    pub fn subscribe(&self) -> broadcast::Receiver<FrameEvent> {
        self.stream.subscribe()
    }
}

fn apply_update(transcript: &mut LiveTranscript, upd: &TranscriptUpdate) {
    match upd.lane {
        UpdateLane::Preview => {
            transcript.last_preview = Some(upd.text.clone());
        }
        UpdateLane::Finalize => {
            if !transcript.committed.is_empty() && !upd.text.is_empty() {
                transcript.committed.push(' ');
            }
            transcript.committed.push_str(&upd.text);
            transcript.last_preview = None;
            transcript.segments_finalized = transcript.segments_finalized.saturating_add(1);
        }
    }
}

fn preview_display(transcript: &LiveTranscript, upd: &TranscriptUpdate) -> String {
    let mut s = transcript.committed.clone();
    if matches!(upd.lane, UpdateLane::Preview) {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(&upd.text);
    }
    s
}

/// Convenience: build a budget controller from `[interactive]` config
/// + the active STT backend's price-table entry.
#[must_use]
pub fn budget_for(provider: &str, ceiling_per_minute_umicros: u64) -> BudgetController {
    let table = PriceTable::defaults();
    let cost = table.get(provider);
    BudgetController::new(cost, ceiling_per_minute_umicros, QualityFloor::Max)
}

/// Parse the `quality_floor` config string.
#[must_use]
pub fn parse_quality_floor(s: &str) -> QualityFloor {
    match s.to_ascii_lowercase().as_str() {
        "aggressive" => QualityFloor::Aggressive,
        "balanced" => QualityFloor::Balanced,
        _ => QualityFloor::Max,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalize_appends_to_committed_with_separator() {
        let mut t = LiveTranscript::default();
        let u1 = TranscriptUpdate::finalize(0, "hello", Duration::from_millis(10));
        let u2 = TranscriptUpdate::finalize(1, "world", Duration::from_millis(20));
        apply_update(&mut t, &u1);
        apply_update(&mut t, &u2);
        assert_eq!(t.committed, "hello world");
        assert_eq!(t.segments_finalized, 2);
        assert!(t.last_preview.is_none());
    }

    #[test]
    fn preview_does_not_commit() {
        let mut t = LiveTranscript::default();
        let u = TranscriptUpdate::preview(0, "hi", Duration::from_millis(50));
        apply_update(&mut t, &u);
        assert!(t.committed.is_empty());
        assert_eq!(t.last_preview.as_deref(), Some("hi"));
    }

    #[test]
    fn quality_floor_parser_falls_back_to_max() {
        assert!(matches!(parse_quality_floor("max"), QualityFloor::Max));
        assert!(matches!(parse_quality_floor("BALANCED"), QualityFloor::Balanced));
        assert!(matches!(parse_quality_floor("Aggressive"), QualityFloor::Aggressive));
        assert!(matches!(parse_quality_floor("nonsense"), QualityFloor::Max));
    }
}
