// SPDX-License-Identifier: GPL-3.0-only
//! Live-dictation orchestrator glue. Plan R7.4 + v7 R2.5/R7.3a.
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
//!
//! The boundary heuristics (R2.5 / R7.3a) live here, not in `fono-stt`,
//! per design decision 22 in plan v7: they are **session-layer policy**
//! that depends on per-session config (`commit_use_prosody`, etc.) and
//! must remain easy for callers to disable without rebuilding the STT
//! backends. ADR 0015 captures the rationale.

use std::collections::VecDeque;
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
use tracing::{debug, field, info, instrument, warn, Span};

/// Tunable knobs for the boundary heuristics. Built from
/// `fono_core::config::Interactive` by the orchestrator (see
/// [`HeuristicConfig::from_interactive_defaults`] in tests / cli.rs's
/// runtime config plumbing).
#[derive(Debug, Clone)]
pub struct HeuristicConfig {
    pub use_prosody: bool,
    pub prosody_extend_ms: u32,
    pub use_punctuation_hint: bool,
    pub punct_extend_ms: u32,
    pub hold_on_filler: bool,
    pub filler_words: Vec<String>,
    pub dangling_words: Vec<String>,
    pub eou_drain_extended_ms: u32,
    pub chunk_ms_steady: u32,
}

impl Default for HeuristicConfig {
    fn default() -> Self {
        Self {
            use_prosody: false,
            prosody_extend_ms: 250,
            use_punctuation_hint: true,
            punct_extend_ms: 150,
            hold_on_filler: true,
            filler_words: fono_core::config::default_filler_words(),
            dangling_words: fono_core::config::default_dangling_words(),
            eou_drain_extended_ms: 1500,
            chunk_ms_steady: 1500,
        }
    }
}

impl HeuristicConfig {
    /// All-off variant for the equivalence harness's `A2-no-heur` row
    /// and the test asserting heuristics are additive.
    #[must_use]
    pub fn all_off() -> Self {
        Self {
            use_prosody: false,
            prosody_extend_ms: 0,
            use_punctuation_hint: false,
            punct_extend_ms: 0,
            hold_on_filler: false,
            filler_words: Vec::new(),
            dangling_words: Vec::new(),
            eou_drain_extended_ms: 0,
            chunk_ms_steady: 1500,
        }
    }
}

/// Reason a drain-time hold-on-filler / hold-on-dangling check fired.
/// Exposed on [`LiveTranscript`] as informational fields so callers and
/// tests can observe the heuristic without needing to scrape logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainExtensionReason {
    /// Trailing word matched [`HeuristicConfig::filler_words`].
    Filler(String),
    /// Trailing word matched [`HeuristicConfig::dangling_words`].
    Dangling(String),
}

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
    /// True when end-of-input arrived with a trailing filler word and
    /// the heuristic flagged it (`commit_hold_on_filler = true`).
    /// Slice A exposes this as an *informational signal* — see
    /// `LiveSession::run` for the rationale on why we don't extend the
    /// drain window in-place.
    pub drain_extended_by_filler: bool,
    /// Set to `Some(word)` when end-of-input arrived with a trailing
    /// syntactically-dangling word and the heuristic flagged it.
    pub drain_extended_by_dangling: Option<String>,
    /// Cumulative milliseconds the prosody heuristic extended segment
    /// boundaries during this session (R10.5).
    pub commit_extended_by_prosody_ms: u32,
    /// Cumulative milliseconds the punctuation heuristic extended
    /// segment boundaries during this session (R10.5). Always 0 in
    /// Slice A — the wiring is a Slice B follow-up because the
    /// translator task does not currently see preview text. The pure
    /// function is unit-tested.
    pub commit_extended_by_punct_ms: u32,
}

/// Builder for a live-dictation session.
pub struct LiveSession {
    stt: Arc<dyn StreamingStt>,
    overlay: Option<OverlayHandle>,
    sample_rate: u32,
    language: Option<String>,
    budget: BudgetController,
    stream_cfg: StreamConfig,
    heuristics: HeuristicConfig,
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
            heuristics: HeuristicConfig::default(),
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

    #[must_use]
    pub fn with_heuristics(mut self, h: HeuristicConfig) -> Self {
        self.heuristics = h;
        self
    }

    /// Run the session against an already-subscribed broadcast receiver
    /// of [`FrameEvent`]s. The caller is expected to obtain the receiver
    /// from [`Pump::take_receiver`] *before* pushing any audio so that
    /// pushed frames are not lost (`tokio::sync::broadcast` discards
    /// messages sent while no receivers are subscribed and only delivers
    /// post-subscribe messages to a fresh subscriber).
    ///
    /// `quality_floor` is plumbed for the future R12.5 finalize-skip
    /// extension; Slice A treats it as informational only (the current
    /// finalize lane always runs — see ADR 0009).
    #[instrument(
        skip_all,
        fields(
            stt = self.stt.name(),
            rate = self.sample_rate,
            live.commit_extended_by_prosody_ms = field::Empty,
            live.commit_extended_by_punct_ms = field::Empty,
            live.drain_extended_by_filler = field::Empty,
            live.drain_extended_by_dangling = field::Empty,
        )
    )]
    #[allow(clippy::too_many_lines)] // R2.5/R7.3a wiring; further extraction is Slice B.
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
            heuristics,
        } = self;
        let budget = Arc::new(Mutex::new(budget));

        // Translate FrameEvent -> StreamFrame and feed the StreamingStt.
        //
        // R2.5 prosody hint: maintain a rolling 200 ms tail of the most
        // recent voiced PCM and consult `prosody_extend_ms` when a
        // SegmentBoundary arrives. The hint is additive — it can only
        // *delay* the boundary forwarding (capped at chunk_ms_steady *
        // 1.5 by `cap_extension_ms`), never advance it.
        //
        // R2.5 punctuation hint: stub in Slice A. The translator task
        // does not have access to the latest preview text; wiring that
        // up requires plumbing a feedback channel from the updates
        // loop, which is a Slice B follow-up. The pure function is
        // unit-tested below.
        let (sf_tx, sf_rx) = mpsc::unbounded_channel::<StreamFrame>();
        let budget_for_pump = Arc::clone(&budget);
        let prosody_extend_ms_cap =
            cap_extension_ms(heuristics.prosody_extend_ms, heuristics.chunk_ms_steady);
        let prosody_on = heuristics.use_prosody;
        let prosody_metric = Arc::new(Mutex::new(0_u32));
        let prosody_metric_for_pump = Arc::clone(&prosody_metric);
        let translator = tokio::spawn(async move {
            // ~200 ms tail buffer at sample_rate.
            let tail_capacity = (sample_rate as usize / 5).max(1);
            let mut tail: VecDeque<f32> = VecDeque::with_capacity(tail_capacity);
            loop {
                match frame_rx.recv().await {
                    Ok(FrameEvent::Voiced { pcm, .. }) => {
                        // Charge the budget controller for the audio
                        // duration we're about to send. Slice A's
                        // local-only path returns Continue every time;
                        // the verdict is recorded for telemetry.
                        let dur = Duration::from_secs_f32(pcm.len() as f32 / sample_rate as f32);
                        let verdict = budget_for_pump
                            .lock()
                            .map(|mut b| b.record(dur))
                            .unwrap_or(BudgetVerdict::Continue);
                        if matches!(verdict, BudgetVerdict::StopStreaming) {
                            warn!("budget controller asked to stop streaming");
                            let _ = sf_tx.send(StreamFrame::Eof);
                            return;
                        }
                        // Update the rolling 200 ms tail before
                        // forwarding so the next SegmentBoundary sees
                        // the freshest pitch contour.
                        for s in &pcm {
                            if tail.len() == tail_capacity {
                                tail.pop_front();
                            }
                            tail.push_back(*s);
                        }
                        if sf_tx.send(StreamFrame::Pcm(pcm)).is_err() {
                            return;
                        }
                    }
                    Ok(FrameEvent::SegmentBoundary { .. }) => {
                        if prosody_on {
                            let tail_vec: Vec<f32> = tail.iter().copied().collect();
                            let ext =
                                prosody_extend_ms(&tail_vec, sample_rate, prosody_extend_ms_cap);
                            if ext > 0 {
                                if let Ok(mut g) = prosody_metric_for_pump.lock() {
                                    *g = g.saturating_add(ext);
                                }
                                tokio::time::sleep(Duration::from_millis(u64::from(ext))).await;
                            }
                        }
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
            // Clear any text held over from a previous session BEFORE
            // we flip to LiveDictating, so the user never sees the
            // tail of the last dictation flash up at session start.
            o.update_text(String::new());
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

        // R7.3a hold-on-filler: pure-functional check on the committed
        // text after the upstream stream has closed. Slice A ships this
        // as an *informational signal* exposed on `LiveTranscript`.
        //
        // Why not a real drain-window extension here? The upstream
        // broadcast receiver was consumed by the translator task and
        // closed; resubscribing post-close would yield no late frames
        // (`tokio::sync::broadcast` does not replay history). Plumbing
        // an explicit `extended_drain_window` argument all the way
        // back to `Pump::finish` would require a >80 LoC pump
        // refactor — outside Slice A scope per the v7 plan's
        // pragmatic-fallback note. The daemon (caller) sees these
        // flags and may choose to extend the drain by holding the
        // hotkey-FSM in `Recording` for `eou_drain_extended_ms` before
        // calling `pump.finish()` next session. Slice D's adaptive-EOU
        // work makes the extension first-class.
        if heuristics.hold_on_filler {
            if let Some(reason) = drain_should_extend(
                &transcript.committed,
                &heuristics.filler_words,
                &heuristics.dangling_words,
            ) {
                match reason {
                    DrainExtensionReason::Filler(w) => {
                        transcript.drain_extended_by_filler = true;
                        debug!(filler = %w, "drain heuristic: trailing filler detected");
                    }
                    DrainExtensionReason::Dangling(w) => {
                        transcript.drain_extended_by_dangling = Some(w.clone());
                        debug!(dangling = %w, "drain heuristic: trailing dangling word detected");
                    }
                }
            }
        }

        // Promote the prosody metric out of the translator task.
        if let Ok(g) = prosody_metric.lock() {
            transcript.commit_extended_by_prosody_ms = *g;
        }

        // R10.5: stamp the heuristic outcomes on the run span.
        let span = Span::current();
        span.record(
            "live.commit_extended_by_prosody_ms",
            transcript.commit_extended_by_prosody_ms,
        );
        span.record(
            "live.commit_extended_by_punct_ms",
            transcript.commit_extended_by_punct_ms,
        );
        span.record(
            "live.drain_extended_by_filler",
            transcript.drain_extended_by_filler,
        );
        if let Some(w) = transcript.drain_extended_by_dangling.as_deref() {
            span.record("live.drain_extended_by_dangling", w);
        }

        info!(
            "live session done: {} segments, {} committed chars, prosody+{}ms, filler={}, dangling={:?}",
            transcript.segments_finalized,
            transcript.committed.len(),
            transcript.commit_extended_by_prosody_ms,
            transcript.drain_extended_by_filler,
            transcript.drain_extended_by_dangling,
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

// =====================================================================
// Pure heuristic helpers (R2.5 / R7.3a). Kept module-private to make
// the API surface small; tested via the in-module `mod tests` block.
// =====================================================================

/// R2.5 punctuation hint. Peeks at the trailing non-whitespace char of
/// `preview_text`. Returns `0` when that char is a terminal punctuation
/// (`.?!`) — the speaker has clearly closed the clause and the boundary
/// should fire immediately. Returns `base_extend_ms` otherwise (mid-
/// clause punctuation `,;:` or alphanumerics) to give the speaker a
/// little extra time to continue.
#[must_use]
#[allow(dead_code)] // stub: wired in Slice B once translator gets preview-text feedback.
pub(crate) fn punctuation_extend_ms(preview_text: &str, base_extend_ms: u32) -> u32 {
    match preview_text.chars().rev().find(|c| !c.is_whitespace()) {
        Some('.' | '?' | '!') => 0,
        Some(_) => base_extend_ms,
        None => 0,
    }
}

/// R2.5 prosody hint. Estimates F0 over the last 200 ms of `tail_pcm`
/// using a hand-rolled time-domain autocorrelation (no FFT, no extra
/// crate). The window is split into 10 ms frames and an F0 is picked
/// per frame in the 80–400 Hz band; we then linear-regress those F0
/// samples against frame index and inspect the slope.
///
/// * Slope < +5 Hz over the window (flat) → `base_extend_ms`.
/// * Slope > +5 Hz over the window (rising) → `base_extend_ms`.
/// * Slope ≤ −5 Hz (falling, i.e. the speaker is ending the
///   thought) → `0`.
/// * Insufficient voiced frames → `0` (treat as silence: no hint).
#[must_use]
pub(crate) fn prosody_extend_ms(tail_pcm: &[f32], sample_rate: u32, base_extend_ms: u32) -> u32 {
    if base_extend_ms == 0 || sample_rate == 0 || tail_pcm.is_empty() {
        return 0;
    }
    let frame_len = (sample_rate / 100) as usize; // 10 ms.
    if frame_len < 8 || tail_pcm.len() < frame_len * 4 {
        return 0;
    }
    let min_period = (sample_rate as usize / 400).max(1); // 400 Hz max.
    let max_period = (sample_rate as usize / 80).max(min_period + 1); // 80 Hz min.
    let mut f0_samples: Vec<f32> = Vec::new();
    for frame in tail_pcm.chunks(frame_len) {
        if frame.len() < frame_len {
            break;
        }
        if let Some(f0) = autocorr_f0(frame, sample_rate, min_period, max_period) {
            f0_samples.push(f0);
        }
    }
    if f0_samples.len() < 3 {
        return 0;
    }
    // Simple least-squares slope (Hz per frame) → scale to Hz across
    // the whole tail window.
    let n = f0_samples.len() as f32;
    let mean_x = (n - 1.0) / 2.0;
    let mean_y = f0_samples.iter().sum::<f32>() / n;
    let mut num = 0.0;
    let mut den = 0.0;
    for (i, y) in f0_samples.iter().enumerate() {
        let dx = i as f32 - mean_x;
        num += dx * (*y - mean_y);
        den += dx * dx;
    }
    if den.abs() < f32::EPSILON {
        return base_extend_ms;
    }
    let slope_per_frame = num / den;
    let slope_total_hz = slope_per_frame * (n - 1.0);
    if slope_total_hz <= -5.0 {
        0
    } else {
        base_extend_ms
    }
}

/// Time-domain autocorrelation pitch tracker for one fixed-size frame.
/// Returns `None` if the autocorr peak is below a small SNR floor
/// (treats noise / silence as unvoiced).
fn autocorr_f0(
    frame: &[f32],
    sample_rate: u32,
    min_period: usize,
    max_period: usize,
) -> Option<f32> {
    // Cap max_period at half the frame so there's at least N/2
    // overlap for the autocorrelation lag — at 10 ms / 16 kHz the
    // frame is 160 samples and we'd otherwise reject any pitch below
    // ~100 Hz outright.
    let max_period = max_period.min(frame.len() / 2);
    if max_period <= min_period {
        return None;
    }
    let energy: f32 = frame.iter().map(|s| *s * *s).sum();
    if energy < 1e-6 {
        return None;
    }
    let mut best_period = 0usize;
    let mut best_r = 0.0_f32;
    for period in min_period..=max_period {
        let mut r = 0.0;
        for i in 0..(frame.len() - period) {
            r += frame[i] * frame[i + period];
        }
        if r > best_r {
            best_r = r;
            best_period = period;
        }
    }
    if best_period == 0 || best_r / energy < 0.3 {
        return None;
    }
    Some(sample_rate as f32 / best_period as f32)
}

/// R7.3a end-of-utterance heuristic. Returns the reason the drain
/// should be extended, or `None` if the trailing word doesn't match
/// either vocabulary. Comparison is case-insensitive after stripping
/// trailing terminal punctuation (`.,;:!?`) — a sentence that ends in
/// `"and."` is *not* a dangling-and (the speaker closed the clause).
#[must_use]
pub(crate) fn drain_should_extend(
    committed: &str,
    filler_words: &[String],
    dangling_words: &[String],
) -> Option<DrainExtensionReason> {
    let trailing = committed.split_whitespace().next_back()?;
    // Reject anything with a terminal punctuation — the speaker
    // explicitly closed the clause.
    if trailing
        .chars()
        .last()
        .map(|c| matches!(c, '.' | '!' | '?'))
        .unwrap_or(false)
    {
        return None;
    }
    let stripped: String = trailing.trim_end_matches([',', ';', ':']).to_lowercase();
    if stripped.is_empty() {
        return None;
    }
    if filler_words
        .iter()
        .any(|w| w.eq_ignore_ascii_case(&stripped))
    {
        return Some(DrainExtensionReason::Filler(stripped));
    }
    if dangling_words
        .iter()
        .any(|w| w.eq_ignore_ascii_case(&stripped))
    {
        return Some(DrainExtensionReason::Dangling(stripped));
    }
    None
}

/// Cap an extension at `chunk_ms_steady * 1.5` so a misbehaving
/// heuristic can never freeze a session.
fn cap_extension_ms(requested: u32, chunk_ms_steady: u32) -> u32 {
    let cap = chunk_ms_steady.saturating_add(chunk_ms_steady / 2);
    requested.min(cap)
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
        assert!(matches!(
            parse_quality_floor("BALANCED"),
            QualityFloor::Balanced
        ));
        assert!(matches!(
            parse_quality_floor("Aggressive"),
            QualityFloor::Aggressive
        ));
        assert!(matches!(parse_quality_floor("nonsense"), QualityFloor::Max));
    }

    // ---------------- R2.5 / R7.3a heuristic isolation tests --------

    #[test]
    fn punctuation_hint_commits_immediately_on_terminal_punct() {
        assert_eq!(punctuation_extend_ms("hello world.", 200), 0);
        assert_eq!(punctuation_extend_ms("really?", 200), 0);
        assert_eq!(punctuation_extend_ms("wait!  ", 200), 0);
    }

    #[test]
    fn punctuation_hint_extends_on_mid_clause_punct_or_alnum() {
        assert_eq!(punctuation_extend_ms("first,", 150), 150);
        assert_eq!(punctuation_extend_ms("hello;", 150), 150);
        assert_eq!(punctuation_extend_ms("hello", 150), 150);
        assert_eq!(punctuation_extend_ms("", 150), 0);
    }

    /// Synthesize a sine wave of `freq_hz` for `duration_ms` at
    /// `sample_rate`.
    fn sine_pcm(freq_hz: f32, duration_ms: u32, sample_rate: u32) -> Vec<f32> {
        let n = (sample_rate * duration_ms / 1000) as usize;
        let dt = 1.0 / sample_rate as f32;
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq_hz * i as f32 * dt).sin() * 0.5)
            .collect()
    }

    #[test]
    fn prosody_flat_pitch_returns_extension() {
        // 200 Hz steady tone for 200 ms → flat F0 contour.
        let tail = sine_pcm(200.0, 200, 16_000);
        let ext = prosody_extend_ms(&tail, 16_000, 250);
        assert_eq!(ext, 250, "flat pitch should grant the full extension");
    }

    #[test]
    fn prosody_falling_pitch_returns_zero() {
        // Manually splice high-pitch (300 Hz) → low-pitch (100 Hz)
        // tails so the autocorrelator sees a clear downward slope.
        let mut tail = sine_pcm(300.0, 60, 16_000);
        tail.extend(sine_pcm(220.0, 60, 16_000));
        tail.extend(sine_pcm(140.0, 60, 16_000));
        tail.extend(sine_pcm(100.0, 60, 16_000));
        let ext = prosody_extend_ms(&tail, 16_000, 250);
        assert_eq!(ext, 0, "sharply falling pitch should commit immediately");
    }

    #[test]
    fn prosody_silence_returns_zero() {
        let tail = vec![0.0_f32; 16_000 / 5]; // 200 ms silence
        assert_eq!(prosody_extend_ms(&tail, 16_000, 250), 0);
    }

    #[test]
    fn drain_filler_suffix_matches() {
        let f = vec!["um".to_string(), "uh".to_string()];
        let d = vec!["and".to_string()];
        match drain_should_extend("hello um", &f, &d) {
            Some(DrainExtensionReason::Filler(w)) => assert_eq!(w, "um"),
            other => panic!("expected Filler(um), got {other:?}"),
        }
    }

    #[test]
    fn drain_dangling_suffix_matches() {
        let f = Vec::new();
        let d = vec!["and".to_string(), "but".to_string()];
        match drain_should_extend("first and", &f, &d) {
            Some(DrainExtensionReason::Dangling(w)) => assert_eq!(w, "and"),
            other => panic!("expected Dangling(and), got {other:?}"),
        }
    }

    #[test]
    fn drain_terminal_punct_suppresses_match() {
        let f = Vec::new();
        let d = vec!["and".to_string()];
        assert!(drain_should_extend("first and.", &f, &d).is_none());
        assert!(drain_should_extend("hello.", &f, &d).is_none());
    }

    #[test]
    fn drain_no_match_returns_none() {
        let f = vec!["um".to_string()];
        let d = vec!["and".to_string()];
        assert!(drain_should_extend("hello", &f, &d).is_none());
        assert!(drain_should_extend("", &f, &d).is_none());
    }

    #[test]
    fn cap_extension_clamps_to_chunk_ms_steady_x1_5() {
        assert_eq!(cap_extension_ms(100, 1500), 100);
        // Cap is 2250 → 9000 should clamp.
        assert_eq!(cap_extension_ms(9000, 1500), 2250);
    }
}
