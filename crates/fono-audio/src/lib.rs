// SPDX-License-Identifier: GPL-3.0-only
//! Audio capture via PulseAudio/PipeWire process capture on Linux (or optional
//! `cpal`), resampling to 16 kHz mono f32, and a pluggable VAD trait. Phase 2
//! of `docs/plans/2026-04-24-fono-design-v1.md`.
//!
//! Silero VAD (ONNX) and auto-mute integration are scaffolded here as
//! pluggable interfaces; concrete ONNX wiring lands once `ort` is added to
//! the `fono-audio` deps (requires the model blob to be vendored).

pub mod capture;
pub mod devices;
pub mod mute;
pub mod pulse;
pub mod resample;
pub mod trim;
pub mod vad;

#[cfg(feature = "streaming")]
pub mod stream;

pub use capture::{
    AudioCapture, CaptureConfig, CaptureHandle, CaptureStreamHandle, RecordingBuffer,
};
pub use trim::{trim_silence, TrimConfig};
pub use vad::{SileroVad, Vad, VadDecision, WebRtcVadStub};

#[cfg(feature = "streaming")]
pub use stream::{AudioFrameStream, FrameEvent, StreamConfig, DEFAULT_CAPACITY};
