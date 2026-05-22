// SPDX-License-Identifier: GPL-3.0-only
//! Audio envelope follower.
//!
//! Two EMAs over frame RMS:
//!
//! - `inst_rms` — fast (~30 ms) tracker of the current frame energy.
//! - `voiced_rms` — **asymmetric** EMA of "how loud is the user's voice":
//!   fast attack (~300 ms) so the green tick reaches your speaking
//!   level within a syllable or two, slow release (~3000 ms) so it
//!   stays up across natural sentence pauses instead of drifting
//!   down toward room noise. This is the reference signal the
//!   silence-watch state machine compares against when deciding
//!   whether the current frame counts as silence — it self-
//!   calibrates from the user's own dictation regardless of
//!   mic / room / gain, and the slow release is what makes
//!   `PONDERING` stay engaged across multi-second pauses.
//!   Sub-floor frames (below `voice_floor_dbfs`) are skipped
//!   entirely so mic self-noise can't drag the reference down.

#[derive(Debug, Clone, Copy)]
pub struct EnvelopeConfig {
    pub sample_rate: u32,
    pub inst_ema_window_ms: u32,
    /// Attack time constant for `voiced_rms` — used when the current
    /// frame is louder than the running estimate. Fast so the green
    /// reference catches up to real speech within a syllable.
    pub voiced_attack_ms: u32,
    /// Release time constant for `voiced_rms` — used when the current
    /// frame is quieter than the running estimate. Slow (~3 s) so the
    /// reference holds across natural pauses; slice 2's `Pondering`
    /// indicator depends on this hold to stay engaged.
    pub voiced_release_ms: u32,
    pub voice_floor_dbfs: f32,
}

impl Default for EnvelopeConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16_000,
            inst_ema_window_ms: 30,
            voiced_attack_ms: 300,
            voiced_release_ms: 3_000,
            voice_floor_dbfs: -55.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EnvelopeSnapshot {
    pub inst_rms: f32,
    pub voiced_rms: f32,
    pub frames_observed: u64,
    pub voiced_frames: u64,
}

impl EnvelopeSnapshot {
    #[must_use]
    pub fn inst_dbfs(&self) -> f32 {
        rms_to_dbfs(self.inst_rms)
    }
    #[must_use]
    pub fn voiced_dbfs(&self) -> f32 {
        rms_to_dbfs(self.voiced_rms)
    }
}

#[must_use]
pub fn rms_to_dbfs(rms: f32) -> f32 {
    if rms <= 1.0e-7 {
        -140.0
    } else {
        20.0 * rms.log10()
    }
}

#[must_use]
pub fn dbfs_to_rms(dbfs: f32) -> f32 {
    10.0_f32.powf(dbfs / 20.0)
}

pub struct EnvelopeFollower {
    cfg: EnvelopeConfig,
    inst_rms: f32,
    voiced_rms: f32,
    alpha_inst: f32,
    alpha_voiced_attack: f32,
    alpha_voiced_release: f32,
    voice_floor_rms: f32,
    frames_observed: u64,
    voiced_frames: u64,
    last_frame_ms: f32,
}

impl EnvelopeFollower {
    #[must_use]
    pub fn new(cfg: EnvelopeConfig) -> Self {
        let voice_floor_rms = dbfs_to_rms(cfg.voice_floor_dbfs);
        Self {
            cfg,
            inst_rms: 0.0,
            voiced_rms: 0.0,
            alpha_inst: 0.0,
            alpha_voiced_attack: 0.0,
            alpha_voiced_release: 0.0,
            voice_floor_rms,
            frames_observed: 0,
            voiced_frames: 0,
            last_frame_ms: 0.0,
        }
    }

    /// Returns the RMS of this frame, for callers that want to record
    /// the per-frame value into a histogram.
    pub fn push_frame(&mut self, frame: &[f32]) -> f32 {
        if frame.is_empty() || self.cfg.sample_rate == 0 {
            return 0.0;
        }
        let frame_ms = (frame.len() as f32 * 1000.0) / self.cfg.sample_rate as f32;
        if (frame_ms - self.last_frame_ms).abs() > 0.5 {
            self.last_frame_ms = frame_ms;
            self.alpha_inst = ema_alpha(frame_ms, self.cfg.inst_ema_window_ms as f32);
            self.alpha_voiced_attack = ema_alpha(frame_ms, self.cfg.voiced_attack_ms as f32);
            self.alpha_voiced_release = ema_alpha(frame_ms, self.cfg.voiced_release_ms as f32);
        }
        let rms = rms(frame);
        self.inst_rms = ema_step(self.inst_rms, rms, self.alpha_inst);
        self.frames_observed = self.frames_observed.saturating_add(1);
        // Skip sub-floor frames entirely so mic self-noise can't
        // pull `voiced_rms` down even via the slow release path.
        if self.inst_rms > self.voice_floor_rms {
            // Asymmetric: fast when rising, slow when falling.
            let alpha = if self.inst_rms > self.voiced_rms {
                self.alpha_voiced_attack
            } else {
                self.alpha_voiced_release
            };
            self.voiced_rms = ema_step(self.voiced_rms, self.inst_rms, alpha);
            self.voiced_frames = self.voiced_frames.saturating_add(1);
        }
        rms
    }

    #[must_use]
    pub fn snapshot(&self) -> EnvelopeSnapshot {
        EnvelopeSnapshot {
            inst_rms: self.inst_rms,
            voiced_rms: self.voiced_rms,
            frames_observed: self.frames_observed,
            voiced_frames: self.voiced_frames,
        }
    }

    #[must_use]
    pub fn config(&self) -> &EnvelopeConfig {
        &self.cfg
    }
}

fn rms(frame: &[f32]) -> f32 {
    let sum_sq: f32 = frame.iter().map(|x| x * x).sum();
    (sum_sq / frame.len() as f32).sqrt()
}

fn ema_alpha(frame_ms: f32, tau_ms: f32) -> f32 {
    if tau_ms <= 0.0 {
        return 1.0;
    }
    1.0 - (-frame_ms / tau_ms).exp()
}

fn ema_step(prev: f32, sample: f32, alpha: f32) -> f32 {
    alpha.mul_add(sample - prev, prev)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_16k() -> EnvelopeConfig {
        EnvelopeConfig::default()
    }

    fn tone_frame(n: usize, amp: f32) -> Vec<f32> {
        vec![amp; n]
    }

    #[test]
    fn inst_rms_tracks_signal() {
        let mut e = EnvelopeFollower::new(cfg_16k());
        let loud = tone_frame(320, 0.1);
        for _ in 0..100 {
            e.push_frame(&loud);
        }
        let snap = e.snapshot();
        assert!((rms_to_dbfs(snap.inst_rms) - -20.0).abs() < 0.5);
        assert_eq!(snap.frames_observed, 100);
    }

    #[test]
    fn voiced_rms_tracks_signal_when_above_floor() {
        let mut e = EnvelopeFollower::new(cfg_16k());
        let loud = tone_frame(320, 0.1);
        for _ in 0..100 {
            e.push_frame(&loud);
        }
        let snap = e.snapshot();
        assert!((rms_to_dbfs(snap.voiced_rms) - -20.0).abs() < 1.0);
        assert!(snap.voiced_frames > 50);
    }

    #[test]
    fn voiced_rms_ignores_subfloor_signal() {
        let mut e = EnvelopeFollower::new(cfg_16k());
        // -70 dBFS, well below the -55 dBFS voice floor.
        let quiet = tone_frame(320, dbfs_to_rms(-70.0));
        for _ in 0..200 {
            e.push_frame(&quiet);
        }
        let snap = e.snapshot();
        assert_eq!(snap.voiced_frames, 0);
        assert!(snap.voiced_rms.abs() < 1.0e-6);
    }

    #[test]
    fn voiced_rms_holds_during_silence() {
        // Asymmetric EMA: 200 ms attack, 3000 ms release. After
        // ramp-up to -20 dBFS, a 500 ms silent tail should leave
        // voiced_rms within ~3 dB of its peak (since silent frames
        // below the -55 dBFS floor are skipped entirely, voiced_rms
        // should not drop at all in this test — but we leave 3 dB
        // headroom for slow-release drift if a future tweak admits
        // sub-floor frames).
        let mut e = EnvelopeFollower::new(cfg_16k());
        let loud = tone_frame(320, 0.1); // -20 dBFS
        for _ in 0..50 {
            e.push_frame(&loud);
        }
        let peak_dbfs = rms_to_dbfs(e.snapshot().voiced_rms);
        let silent = tone_frame(320, dbfs_to_rms(-70.0));
        for _ in 0..25 {
            // 25 × 20 ms = 500 ms of silence
            e.push_frame(&silent);
        }
        let hold_dbfs = rms_to_dbfs(e.snapshot().voiced_rms);
        assert!(
            (peak_dbfs - hold_dbfs).abs() < 3.0,
            "voiced_rms dropped {peak_dbfs} -> {hold_dbfs} dBFS during silence"
        );
    }

    #[test]
    fn voiced_rms_attack_catches_up_quickly() {
        // Attack window is 300 ms. After 1 EMA window the value
        // should be within ~6 dB of target (1 − e⁻¹ = 63%).
        let mut e = EnvelopeFollower::new(cfg_16k());
        let loud = tone_frame(320, 0.1); // -20 dBFS target
        for _ in 0..15 {
            // 15 × 20 ms = 300 ms ≈ one attack window
            e.push_frame(&loud);
        }
        let dbfs = rms_to_dbfs(e.snapshot().voiced_rms);
        assert!(dbfs > -26.0, "voiced_rms only reached {dbfs} dBFS after 300 ms");
    }

    #[test]
    fn empty_frame_is_noop() {
        let mut e = EnvelopeFollower::new(cfg_16k());
        let r = e.push_frame(&[]);
        assert!(r.abs() < 1.0e-9);
        assert_eq!(e.snapshot().frames_observed, 0);
    }

    #[test]
    fn push_frame_returns_frame_rms() {
        let mut e = EnvelopeFollower::new(cfg_16k());
        let r = e.push_frame(&tone_frame(320, 0.5));
        assert!((rms_to_dbfs(r) - -6.0).abs() < 0.1);
    }

    #[test]
    fn rms_to_dbfs_clamps_at_floor() {
        assert!((rms_to_dbfs(1.0) - 0.0).abs() < 1.0e-3);
        assert!((rms_to_dbfs(0.1) - -20.0).abs() < 1.0e-2);
        assert!((rms_to_dbfs(0.0) - -140.0).abs() < 1.0e-3);
        assert!((rms_to_dbfs(-1.0e-12) - -140.0).abs() < 1.0e-3);
    }

    #[test]
    fn dbfs_to_rms_is_inverse() {
        for &dbfs in &[0.0, -6.0, -20.0, -40.0, -55.0, -70.0] {
            let rms = dbfs_to_rms(dbfs);
            assert!((rms_to_dbfs(rms) - dbfs).abs() < 1.0e-3);
        }
    }

    #[test]
    fn ema_alpha_monotone_in_window() {
        let fast = ema_alpha(20.0, 30.0);
        let slow = ema_alpha(20.0, 500.0);
        assert!(fast > slow);
        assert!((0.0..=1.0).contains(&fast));
        assert!((0.0..=1.0).contains(&slow));
    }
}
