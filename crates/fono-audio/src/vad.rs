// SPDX-License-Identifier: GPL-3.0-only
//! Voice-activity detection trait + backends.
//!
//! Today the only backend is the trivial energy-threshold
//! [`WebRtcVadStub`]; a neural VAD (e.g. Silero on `ort`) is a planned
//! upgrade but is deliberately not wired yet.

use anyhow::Result;

/// Per-chunk VAD decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadDecision {
    Speech,
    Silence,
}

pub trait Vad: Send {
    /// Inspect a 10–30 ms frame of 16 kHz mono f32 samples.
    fn classify(&mut self, frame: &[f32]) -> Result<VadDecision>;
}

/// Energy-threshold "VAD" — the `"energy"` backend that actually ships.
/// Far less accurate than a neural VAD would be, but dependency-free and
/// keeps the pipeline functional. Threshold tuned for typical mic input.
pub struct WebRtcVadStub {
    pub threshold: f32,
}

impl Default for WebRtcVadStub {
    fn default() -> Self {
        Self { threshold: 0.01 }
    }
}

impl Vad for WebRtcVadStub {
    fn classify(&mut self, frame: &[f32]) -> Result<VadDecision> {
        let energy = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len().max(1) as f32).sqrt();
        Ok(if energy >= self.threshold { VadDecision::Speech } else { VadDecision::Silence })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_below_threshold() {
        let mut v = WebRtcVadStub::default();
        assert_eq!(v.classify(&[0.0; 160]).unwrap(), VadDecision::Silence);
    }

    #[test]
    fn speech_above_threshold() {
        let mut v = WebRtcVadStub::default();
        assert_eq!(v.classify(&[0.5; 160]).unwrap(), VadDecision::Speech);
    }
}
