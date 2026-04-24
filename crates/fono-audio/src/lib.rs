// SPDX-License-Identifier: GPL-3.0-only
//! Audio capture via `cpal`, resampling to 16 kHz mono f32, and a pluggable
//! VAD trait. Phase 2 of `docs/plans/2026-04-24-fono-design-v1.md`.
//!
//! Silero VAD (ONNX) and auto-mute integration are scaffolded here as
//! pluggable interfaces; concrete ONNX wiring lands once `ort` is added to
//! the `fono-audio` deps (requires the model blob to be vendored).

pub mod capture;
pub mod mute;
pub mod resample;
pub mod vad;

pub use capture::{AudioCapture, CaptureConfig, CaptureHandle, RecordingBuffer};
pub use vad::{SileroVad, Vad, VadDecision, WebRtcVadStub};
