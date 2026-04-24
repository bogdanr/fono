// SPDX-License-Identifier: GPL-3.0-only
//! Voice-activity detection trait + backends.
//!
//! The `Silero` backend is scaffolded for ONNX wiring (via `ort`) in a
//! follow-up patch — the trait and a trivial energy-threshold fallback
//! (`WebRtcVadStub`) let the orchestrator compile today.

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

/// Energy-threshold "VAD" used as the `webrtc` / `none` fallback when the
/// Silero ONNX model isn't available. Far less accurate than Silero but
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
        Ok(if energy >= self.threshold {
            VadDecision::Speech
        } else {
            VadDecision::Silence
        })
    }
}

/// Placeholder for the Silero-ONNX backend. Returns `Silence` until wired.
pub struct SileroVad;

impl Vad for SileroVad {
    fn classify(&mut self, _frame: &[f32]) -> Result<VadDecision> {
        Ok(VadDecision::Silence)
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
