// SPDX-License-Identifier: GPL-3.0-only
//! Audio capture via PulseAudio/PipeWire process capture on Linux (or optional
//! `cpal`), resampling to 16 kHz mono f32, and a pluggable VAD trait. Phase 2
//! of `docs/plans/2026-04-24-fono-design-v1.md`.
//!
//! The shipped VAD is the energy-threshold [`vad::WebRtcVadStub`]; a
//! neural VAD (Silero on `ort`) is a planned upgrade, not yet wired.
pub mod capture;
pub mod devices;
pub mod envelope;
pub mod mute;
pub mod playback;
pub mod pulse;
pub mod resample;
pub mod silence_watch;
pub mod sink;
pub mod trim;
pub mod vad;
pub mod wake_registry;
pub mod wakeword;
pub mod wpctl;

#[cfg(feature = "streaming")]
pub mod stream;

pub use capture::{
    AudioCapture, CaptureConfig, CaptureHandle, CaptureStreamHandle, RecordingBuffer,
};
pub use envelope::{dbfs_to_rms, rms_to_dbfs, EnvelopeConfig, EnvelopeFollower, EnvelopeSnapshot};
pub use playback::{AudioPlayback, PlaybackError};
pub use silence_watch::{SilenceEvent, SilenceState, SilenceWatch, SilenceWatchConfig};
pub use sink::{LocalPlaybackSink, PcmSink};
pub use trim::{trim_silence, TrimConfig};
pub use vad::{Vad, VadDecision, WebRtcVadStub};
pub use wake_registry::{ResolvedWakeModel, WakeLicense, WakeModelClass, WakeModelEntry};
pub use wakeword::{EnergyWakeStub, HopBuffer, WakeDecision, WakeWord, HOP_SAMPLES};

#[cfg(feature = "streaming")]
pub use stream::{AudioFrameStream, FrameEvent, StreamConfig, DEFAULT_CAPACITY};
