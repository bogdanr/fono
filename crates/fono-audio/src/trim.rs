// SPDX-License-Identifier: GPL-3.0-only
//! Leading/trailing silence trim for PCM buffers fed to STT.
//!
//! Whisper compute scales linearly with audio length — a 5-second
//! utterance with 1.5 s of tail silence costs ~30 % more wall-clock
//! than the same utterance with the silence trimmed. See latency plan
//! tasks L11 and L12.
//!
//! Algorithm: split the buffer into fixed-size frames, compute RMS
//! energy per frame, then walk inward from each end skipping frames
//! whose RMS is below `threshold`. A small `pad_ms` keeps the first
//! and last spoken frames intact so we don't clip consonants.

/// Configuration for [`trim_silence`].
#[derive(Debug, Clone, Copy)]
pub struct TrimConfig {
    /// Sample rate of the input PCM in Hz.
    pub sample_rate: u32,
    /// Frame size in milliseconds. 20 ms (320 samples @ 16 kHz) is a
    /// reasonable default — small enough for tight trims, large enough
    /// that one stray sample doesn't flip a frame's verdict.
    pub frame_ms: u32,
    /// RMS energy threshold below which a frame counts as silence.
    /// 0.005 ≈ -46 dBFS — quiet rooms register slightly under this.
    pub threshold: f32,
    /// Pre/post padding around the speech region, in milliseconds, so
    /// trim doesn't clip consonants at boundaries.
    pub pad_ms: u32,
}

impl Default for TrimConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16_000,
            frame_ms: 20,
            threshold: 0.005,
            pad_ms: 60,
        }
    }
}

/// Returns a trimmed view (`start..end`) of `pcm` with leading/trailing
/// silence removed. If the entire buffer is silent, returns the original
/// `(0, pcm.len())` so the caller can still pass it through (Whisper
/// will return empty text and the orchestrator will notify-rust).
#[must_use]
pub fn trim_silence(pcm: &[f32], cfg: TrimConfig) -> (usize, usize) {
    if pcm.is_empty() {
        return (0, 0);
    }
    let frame_len = ((cfg.sample_rate as u64) * (cfg.frame_ms as u64) / 1000) as usize;
    if frame_len == 0 || pcm.len() <= frame_len {
        return (0, pcm.len());
    }
    let pad = ((cfg.sample_rate as u64) * (cfg.pad_ms as u64) / 1000) as usize;

    let mut first = None;
    let mut last = None;
    let total_frames = pcm.len() / frame_len;
    for i in 0..total_frames {
        let s = i * frame_len;
        let e = s + frame_len;
        let rms = rms(&pcm[s..e]);
        if rms >= cfg.threshold {
            if first.is_none() {
                first = Some(s);
            }
            last = Some(e);
        }
    }

    match (first, last) {
        (Some(f), Some(l)) => {
            let start = f.saturating_sub(pad);
            let end = (l + pad).min(pcm.len());
            (start, end)
        }
        _ => (0, pcm.len()),
    }
}

fn rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = frame.iter().map(|x| x * x).sum();
    (sum_sq / frame.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pulse(prefix: usize, body: usize, suffix: usize) -> Vec<f32> {
        let mut v = vec![0.0_f32; prefix];
        v.extend(std::iter::repeat_n(0.5_f32, body));
        v.extend(std::iter::repeat_n(0.0_f32, suffix));
        v
    }

    #[test]
    fn trims_leading_and_trailing_silence() {
        let cfg = TrimConfig::default();
        // 1s silence + 0.5s body + 1s silence @ 16 kHz
        let pcm = pulse(16_000, 8_000, 16_000);
        let (s, e) = trim_silence(&pcm, cfg);
        // Body is roughly 16_000..24_000; trim should be a strict subset
        // of the original buffer and contain the entire speech region.
        assert!(s < 16_000, "expected start before body, got {s}");
        assert!(e > 24_000, "expected end after body, got {e}");
        assert!(e - s < pcm.len(), "trim should be shorter than original");
    }

    #[test]
    fn all_silence_returns_full_buffer() {
        let cfg = TrimConfig::default();
        let pcm = vec![0.0_f32; 16_000];
        let (s, e) = trim_silence(&pcm, cfg);
        assert_eq!((s, e), (0, pcm.len()));
    }

    #[test]
    fn empty_returns_zero() {
        let (s, e) = trim_silence(&[], TrimConfig::default());
        assert_eq!((s, e), (0, 0));
    }
}
