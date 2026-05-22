// SPDX-License-Identifier: GPL-3.0-only
//! Silence-watch state machine for dictation auto-stop.
//!
//! Produces a stream of state transitions
//! (`Armed → Speaking → Pondering → {Speaking | Committed}`) that
//! drive the overlay's `PONDERING` label and — when
//! `SilenceWatchConfig::auto_stop_silence_ms` is set — the actual
//! auto-stop commit. The state machine compares each frame's
//! `inst_rms` against the follower's `voiced_rms` so the silence
//! test is relative to the user's own voice rather than an
//! absolute dBFS threshold; this self-calibrates across mic /
//! gain / room without needing a noise-floor estimator.
//!
//! Commit can never fire from `Armed` (preamble required by
//! construction), and a `Committed` event also moves the state
//! back to `Armed` so the watch is single-shot per recording.

use crate::envelope::{rms_to_dbfs, EnvelopeSnapshot};

/// Default parameters per `plans/2026-05-22-fono-auto-stop-silence-v1.md`.
pub const DEFAULT_SPEECH_CONFIRM_ARM_MS: u32 = 100;
/// Minimum duration of voiced frames required to leave `Pondering`
/// back to `Speaking`. Without this, a single noisy frame (breath,
/// chair creak, single-frame RMS spike) flips the label off and on
/// during a real pause. The user-visible symptom is `PONDERING`
/// flashing for one frame and snapping back to `RECORDING`.
pub const DEFAULT_SPEECH_CONFIRM_RESUME_MS: u32 = 200;
pub const DEFAULT_PONDERING_VISUAL_MS: u32 = 1_000;
pub const DEFAULT_SILENCE_GAP_DB: f32 = 12.0;

#[derive(Debug, Clone, Copy)]
pub struct SilenceWatchConfig {
    /// Minimum duration of `inst_rms` above the voice floor required
    /// to leave `Armed`. Rejects coughs / hotkey clicks.
    pub speech_confirm_arm_ms: u32,
    /// Minimum duration of voiced frames required to leave
    /// `Pondering` back to `Speaking`. Larger than the arm value
    /// because impulse noises (mouse clicks, breaths) sustain voiced
    /// energy for ~80-150 ms; the gate must clear those without
    /// rejecting real short words like "OK" whose vowel tail runs
    /// 250-350 ms.
    pub speech_confirm_resume_ms: u32,
    /// Minimum duration of silence required to flip `Speaking →
    /// Pondering` (the visual indicator). Sentence-end pauses
    /// shorter than this never trigger the label.
    pub pondering_visual_ms: u32,
    /// Signal is "silent" when `inst_dbfs < voiced_dbfs - silence_gap_db`.
    pub silence_gap_db: f32,
    /// Total duration (from the start of the silence run, NOT from
    /// `Pondering` entry) after which `SilenceEvent::Committed`
    /// fires. `None` disables auto-stop entirely; the state machine
    /// still drives the visual `Pondering` state but never commits.
    /// Must be ≥ `pondering_visual_ms` for the visual + commit
    /// timing to make sense.
    pub auto_stop_silence_ms: Option<u32>,
}

impl Default for SilenceWatchConfig {
    fn default() -> Self {
        Self {
            speech_confirm_arm_ms: DEFAULT_SPEECH_CONFIRM_ARM_MS,
            speech_confirm_resume_ms: DEFAULT_SPEECH_CONFIRM_RESUME_MS,
            pondering_visual_ms: DEFAULT_PONDERING_VISUAL_MS,
            silence_gap_db: DEFAULT_SILENCE_GAP_DB,
            auto_stop_silence_ms: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SilenceState {
    /// No qualifying speech observed yet this session. No transitions
    /// out of this state ever fire `Pondering`.
    Armed,
    /// Speech has been confirmed and is currently active (or the
    /// current silence is shorter than `pondering_visual_ms`).
    Speaking,
    /// Silence has persisted long enough to be considered "pondering".
    /// Slice 4 will fire auto-stop after a further
    /// `auto_stop_silence_ms - pondering_visual_ms`; slice 2 only
    /// drives the label.
    Pondering,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SilenceEvent {
    None,
    EnteredSpeaking,
    EnteredPondering,
    ResumedFromPondering,
    /// Total silence in the current `Speaking → Pondering` run has
    /// reached `auto_stop_silence_ms`. The state machine resets to
    /// `Armed` so the watch is single-shot per recording session.
    Committed,
}

pub struct SilenceWatch {
    cfg: SilenceWatchConfig,
    state: SilenceState,
    /// Accumulated voiced duration while in `Armed`; reset to 0 on
    /// any silent frame so the confirmation window must be
    /// contiguous.
    armed_voiced_ms: f32,
    /// Accumulated silence duration while in `Speaking`; reset on
    /// any voiced frame.
    silence_ms: f32,
    /// Time spent in the current `Pondering` segment (drives the
    /// walking-letter highlight progress).
    pondering_ms: f32,
    /// Accumulated voiced duration while in `Pondering`; reset to
    /// 0 on any silent frame. Resume fires only when this clears
    /// `speech_confirm_resume_ms`.
    resume_voiced_ms: f32,
}

impl SilenceWatch {
    #[must_use]
    pub fn new(cfg: SilenceWatchConfig) -> Self {
        Self {
            cfg,
            state: SilenceState::Armed,
            armed_voiced_ms: 0.0,
            silence_ms: 0.0,
            pondering_ms: 0.0,
            resume_voiced_ms: 0.0,
        }
    }

    #[must_use]
    pub fn state(&self) -> SilenceState {
        self.state
    }

    /// Returns `0.0..=1.0` only while in `Pondering`, where 0.0 means
    /// "just entered Pondering" and 1.0 means "have been in
    /// Pondering for `walk_target_ms` ms". Callers map this into a
    /// walking-letter highlight position. Returns 0.0 in all other
    /// states.
    #[must_use]
    pub fn pondering_progress(&self, walk_target_ms: u32) -> f32 {
        if self.state != SilenceState::Pondering || walk_target_ms == 0 {
            return 0.0;
        }
        (self.pondering_ms / walk_target_ms as f32).clamp(0.0, 1.0)
    }

    #[must_use]
    pub fn pondering_elapsed_ms(&self) -> f32 {
        if self.state == SilenceState::Pondering {
            self.pondering_ms
        } else {
            0.0
        }
    }

    /// Drive the state machine with a per-frame snapshot from the
    /// envelope follower. `frame_ms` is the elapsed wall-clock time
    /// covered by this push (typically 20 ms).
    pub fn push(&mut self, snap: EnvelopeSnapshot, frame_ms: f32) -> SilenceEvent {
        let voiced_dbfs = rms_to_dbfs(snap.voiced_rms);
        let inst_dbfs = rms_to_dbfs(snap.inst_rms);
        // Only meaningful once voiced_rms has actually been
        // populated — otherwise voiced_dbfs is -140 and every frame
        // looks "loud" relative to it. Treat `voiced_frames == 0`
        // as "no voice reference yet"; the only state that makes
        // sense in that case is Armed.
        let has_voice_ref = snap.voiced_frames > 0;
        let is_silent = !has_voice_ref || inst_dbfs < voiced_dbfs - self.cfg.silence_gap_db;
        let is_voiced = has_voice_ref && !is_silent;
        match self.state {
            SilenceState::Armed => {
                if is_voiced {
                    self.armed_voiced_ms += frame_ms;
                    if self.armed_voiced_ms >= self.cfg.speech_confirm_arm_ms as f32 {
                        self.state = SilenceState::Speaking;
                        self.silence_ms = 0.0;
                        self.armed_voiced_ms = 0.0;
                        return SilenceEvent::EnteredSpeaking;
                    }
                } else {
                    self.armed_voiced_ms = 0.0;
                }
                SilenceEvent::None
            }
            SilenceState::Speaking => {
                if is_silent {
                    self.silence_ms += frame_ms;
                    if self.silence_ms >= self.cfg.pondering_visual_ms as f32 {
                        self.state = SilenceState::Pondering;
                        self.pondering_ms = 0.0;
                        return SilenceEvent::EnteredPondering;
                    }
                } else {
                    self.silence_ms = 0.0;
                }
                SilenceEvent::None
            }
            SilenceState::Pondering => {
                if is_voiced {
                    self.resume_voiced_ms += frame_ms;
                    if self.resume_voiced_ms >= self.cfg.speech_confirm_resume_ms as f32 {
                        self.state = SilenceState::Speaking;
                        self.silence_ms = 0.0;
                        self.pondering_ms = 0.0;
                        self.resume_voiced_ms = 0.0;
                        return SilenceEvent::ResumedFromPondering;
                    }
                } else {
                    self.resume_voiced_ms = 0.0;
                    self.silence_ms += frame_ms;
                }
                self.pondering_ms += frame_ms;
                if let Some(total_ms) = self.cfg.auto_stop_silence_ms {
                    if self.silence_ms >= total_ms as f32 {
                        self.state = SilenceState::Armed;
                        self.silence_ms = 0.0;
                        self.pondering_ms = 0.0;
                        self.resume_voiced_ms = 0.0;
                        self.armed_voiced_ms = 0.0;
                        return SilenceEvent::Committed;
                    }
                }
                SilenceEvent::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::dbfs_to_rms;

    fn snap(inst_dbfs: f32, voiced_dbfs: f32, voiced_frames: u64) -> EnvelopeSnapshot {
        EnvelopeSnapshot {
            inst_rms: dbfs_to_rms(inst_dbfs),
            voiced_rms: dbfs_to_rms(voiced_dbfs),
            frames_observed: 0,
            voiced_frames,
        }
    }

    #[test]
    fn silence_only_never_leaves_armed() {
        let mut w = SilenceWatch::new(SilenceWatchConfig::default());
        for _ in 0..500 {
            let ev = w.push(snap(-80.0, 0.0, 0), 20.0);
            assert_eq!(ev, SilenceEvent::None);
        }
        assert_eq!(w.state(), SilenceState::Armed);
    }

    #[test]
    fn speech_arms_state_machine() {
        let mut w = SilenceWatch::new(SilenceWatchConfig::default());
        let mut entered = false;
        for _ in 0..10 {
            if matches!(w.push(snap(-20.0, -20.0, 50), 20.0), SilenceEvent::EnteredSpeaking) {
                entered = true;
            }
        }
        assert!(entered);
        assert_eq!(w.state(), SilenceState::Speaking);
    }

    #[test]
    fn short_pause_does_not_trigger_pondering() {
        let mut w = SilenceWatch::new(SilenceWatchConfig::default());
        for _ in 0..10 {
            w.push(snap(-20.0, -20.0, 100), 20.0);
        }
        assert_eq!(w.state(), SilenceState::Speaking);
        // 600 ms of silence — well under the 1000 ms visual gate.
        for _ in 0..30 {
            let ev = w.push(snap(-50.0, -20.0, 100), 20.0);
            assert_ne!(ev, SilenceEvent::EnteredPondering);
        }
        assert_eq!(w.state(), SilenceState::Speaking);
    }

    #[test]
    fn long_pause_enters_pondering() {
        let mut w = SilenceWatch::new(SilenceWatchConfig::default());
        for _ in 0..10 {
            w.push(snap(-20.0, -20.0, 100), 20.0);
        }
        let mut entered = false;
        for _ in 0..70 {
            if matches!(w.push(snap(-50.0, -20.0, 100), 20.0), SilenceEvent::EnteredPondering) {
                entered = true;
            }
        }
        assert!(entered);
        assert_eq!(w.state(), SilenceState::Pondering);
    }

    #[test]
    fn speech_resumes_from_pondering() {
        let mut w = SilenceWatch::new(SilenceWatchConfig::default());
        for _ in 0..10 {
            w.push(snap(-20.0, -20.0, 100), 20.0);
        }
        for _ in 0..70 {
            w.push(snap(-50.0, -20.0, 100), 20.0);
        }
        assert_eq!(w.state(), SilenceState::Pondering);
        // Resume requires `speech_confirm_resume_ms` of contiguous
        // voiced frames (200 ms = 10 frames at 20 ms). The first
        // nine voiced frames stay in Pondering; the tenth clears
        // the threshold and emits the event.
        for _ in 0..9 {
            let ev = w.push(snap(-20.0, -20.0, 100), 20.0);
            assert_eq!(ev, SilenceEvent::None);
            assert_eq!(w.state(), SilenceState::Pondering);
        }
        let ev = w.push(snap(-20.0, -20.0, 100), 20.0);
        assert_eq!(ev, SilenceEvent::ResumedFromPondering);
        assert_eq!(w.state(), SilenceState::Speaking);
    }

    #[test]
    fn single_voiced_frame_does_not_resume_pondering() {
        let mut w = SilenceWatch::new(SilenceWatchConfig::default());
        for _ in 0..10 {
            w.push(snap(-20.0, -20.0, 100), 20.0);
        }
        for _ in 0..70 {
            w.push(snap(-50.0, -20.0, 100), 20.0);
        }
        assert_eq!(w.state(), SilenceState::Pondering);
        // A single noisy frame (breath, chair creak) followed by
        // silence must NOT flip the label back to Recording.
        let ev = w.push(snap(-20.0, -20.0, 100), 20.0);
        assert_eq!(ev, SilenceEvent::None);
        for _ in 0..10 {
            let ev = w.push(snap(-50.0, -20.0, 100), 20.0);
            assert_eq!(ev, SilenceEvent::None);
        }
        assert_eq!(w.state(), SilenceState::Pondering);
    }

    #[test]
    fn pondering_progress_advances_with_time() {
        let mut w = SilenceWatch::new(SilenceWatchConfig::default());
        for _ in 0..10 {
            w.push(snap(-20.0, -20.0, 100), 20.0);
        }
        for _ in 0..70 {
            w.push(snap(-50.0, -20.0, 100), 20.0);
        }
        let p0 = w.pondering_progress(5000);
        for _ in 0..50 {
            w.push(snap(-50.0, -20.0, 100), 20.0);
        }
        let p1 = w.pondering_progress(5000);
        assert!(p1 > p0);
        assert!(p0 < 1.0 && p1 < 1.0);
    }

    #[test]
    fn cough_does_not_arm() {
        let mut w = SilenceWatch::new(SilenceWatchConfig::default());
        // 60 ms of voiced — under the 100 ms confirmation window.
        for _ in 0..3 {
            w.push(snap(-20.0, -20.0, 5), 20.0);
        }
        // Followed by silence.
        for _ in 0..50 {
            w.push(snap(-80.0, -20.0, 5), 20.0);
        }
        assert_eq!(w.state(), SilenceState::Armed);
    }

    /// Helper for slice-4 commit tests: build a watch with the given
    /// total `auto_stop_silence_ms` (and a shortened
    /// `pondering_visual_ms = 100 ms` so tests can drive commit
    /// within a few frames), run preamble speech, then feed silence
    /// frames and return the trailing state plus event log.
    fn run_until_commit(
        auto_stop_ms: u32,
        post_speech_silent_frames: usize,
    ) -> (SilenceState, Vec<SilenceEvent>) {
        let cfg = SilenceWatchConfig {
            pondering_visual_ms: 100,
            auto_stop_silence_ms: Some(auto_stop_ms),
            ..Default::default()
        };
        let mut w = SilenceWatch::new(cfg);
        let mut events: Vec<SilenceEvent> = Vec::new();
        for _ in 0..10 {
            events.push(w.push(snap(-20.0, -20.0, 100), 20.0));
        }
        for _ in 0..post_speech_silent_frames {
            events.push(w.push(snap(-50.0, -20.0, 100), 20.0));
        }
        (w.state(), events)
    }

    #[test]
    fn commit_fires_after_total_silence_window() {
        // pondering_visual_ms = 100 ms, auto_stop = 200 ms. After
        // 200 ms of total silence Committed fires exactly once.
        // (Frame 5: enter Pondering at silence_ms = 100. Frame 10:
        // silence_ms = 200, commit fires.)
        let (state, events) = run_until_commit(200, 12);
        let commits = events.iter().filter(|e| **e == SilenceEvent::Committed).count();
        assert_eq!(commits, 1, "expected exactly one Committed event, got {commits}");
        assert_eq!(state, SilenceState::Armed);
    }

    #[test]
    fn commit_resets_to_armed_single_shot() {
        // After commit fires, the watch is back in Armed. Continued
        // silence alone must not fire a second commit.
        let cfg = SilenceWatchConfig {
            pondering_visual_ms: 100,
            auto_stop_silence_ms: Some(200),
            ..Default::default()
        };
        let mut w = SilenceWatch::new(cfg);
        for _ in 0..10 {
            w.push(snap(-20.0, -20.0, 100), 20.0);
        }
        let mut commits = 0;
        for _ in 0..200 {
            if w.push(snap(-50.0, -20.0, 100), 20.0) == SilenceEvent::Committed {
                commits += 1;
            }
        }
        assert_eq!(commits, 1, "commit must be single-shot per recording");
        assert_eq!(w.state(), SilenceState::Armed);
    }

    #[test]
    fn silence_only_never_commits() {
        // No preamble speech: Armed forever, commit cannot fire.
        let cfg = SilenceWatchConfig {
            pondering_visual_ms: 100,
            auto_stop_silence_ms: Some(200),
            ..Default::default()
        };
        let mut w = SilenceWatch::new(cfg);
        for _ in 0..500 {
            let ev = w.push(snap(-70.0, -70.0, 0), 20.0);
            assert_ne!(ev, SilenceEvent::Committed);
        }
        assert_eq!(w.state(), SilenceState::Armed);
    }

    #[test]
    fn impulse_during_pondering_does_not_cancel_commit() {
        // Mouse-click-shaped event: 60 ms voiced (3 frames at 20 ms)
        // — below `speech_confirm_resume_ms = 200 ms`, so resume does
        // NOT fire. Commit timer must keep running.
        let cfg = SilenceWatchConfig {
            pondering_visual_ms: 100,
            auto_stop_silence_ms: Some(400),
            ..Default::default()
        };
        let mut w = SilenceWatch::new(cfg);
        for _ in 0..10 {
            w.push(snap(-20.0, -20.0, 100), 20.0); // preamble
        }
        // 10 silent frames = 200 ms (in Pondering by frame 5).
        for _ in 0..10 {
            w.push(snap(-50.0, -20.0, 100), 20.0);
        }
        assert_eq!(w.state(), SilenceState::Pondering);
        // 60 ms impulse.
        for _ in 0..3 {
            w.push(snap(-20.0, -20.0, 100), 20.0);
        }
        assert_eq!(w.state(), SilenceState::Pondering, "60 ms impulse must not resume");
        // Further silence must eventually commit. Total run after
        // impulse: silence_ms is reset by the voiced frames (each
        // voiced frame resets `resume_voiced_ms` but the impulse
        // does NOT reset `silence_ms` — only a confirmed resume
        // would). So commit fires after enough silence.
        let mut saw_commit = false;
        for _ in 0..60 {
            if w.push(snap(-50.0, -20.0, 100), 20.0) == SilenceEvent::Committed {
                saw_commit = true;
                break;
            }
        }
        assert!(saw_commit, "commit must fire after impulse + further silence");
    }

    #[test]
    fn auto_stop_none_disables_commit() {
        // Default config has auto_stop = None; long silence must not
        // commit even though Pondering is entered.
        let mut w = SilenceWatch::new(SilenceWatchConfig::default());
        for _ in 0..10 {
            w.push(snap(-20.0, -20.0, 100), 20.0);
        }
        for _ in 0..1000 {
            let ev = w.push(snap(-50.0, -20.0, 100), 20.0);
            assert_ne!(ev, SilenceEvent::Committed);
        }
        assert_eq!(w.state(), SilenceState::Pondering);
    }
}
