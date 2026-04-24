// SPDX-License-Identifier: GPL-3.0-only
//! cpal-based capture with ring-buffer + soft/hard recording caps.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat};
use tracing::{debug, warn};

use crate::resample::Resampler;

/// Target rate fed to whisper. Non-negotiable.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Hard cap: 5 minutes. Prevents runaway memory use.
pub const HARD_CAP: Duration = Duration::from_secs(5 * 60);

/// Soft cap: 2 minutes. Emits a warning event through the caller-supplied
/// channel (hook placeholder in this phase).
pub const SOFT_CAP: Duration = Duration::from_secs(2 * 60);

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// cpal device name; empty = system default.
    pub input_device: String,
    /// Preferred sample rate (will resample if device doesn't support it).
    pub target_sample_rate: u32,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            input_device: String::new(),
            target_sample_rate: TARGET_SAMPLE_RATE,
        }
    }
}

/// Shared PCM buffer (f32 mono @ `target_sample_rate`).
#[derive(Debug, Default)]
pub struct RecordingBuffer {
    samples: Vec<f32>,
    truncated: bool,
}

impl RecordingBuffer {
    #[must_use]
    pub fn len(&self) -> usize {
        self.samples.len()
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
    #[must_use]
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
    #[must_use]
    pub fn was_truncated(&self) -> bool {
        self.truncated
    }
    pub fn clear(&mut self) {
        self.samples.clear();
        self.truncated = false;
    }
    pub fn push_slice(&mut self, s: &[f32], cap_samples: usize) {
        let room = cap_samples.saturating_sub(self.samples.len());
        if room == 0 {
            self.truncated = true;
            return;
        }
        let take = s.len().min(room);
        self.samples.extend_from_slice(&s[..take]);
        if take < s.len() {
            self.truncated = true;
        }
    }
}

/// Capture orchestrator. Owns the `cpal` stream and a shared buffer.
pub struct AudioCapture {
    cfg: CaptureConfig,
}

/// RAII handle — dropping it stops the stream.
pub struct CaptureHandle {
    _stream: cpal::Stream,
    pub buffer: Arc<Mutex<RecordingBuffer>>,
}

impl AudioCapture {
    #[must_use]
    pub fn new(cfg: CaptureConfig) -> Self {
        Self { cfg }
    }

    /// Begin capture. Returns a handle whose `buffer` fills with resampled
    /// f32 mono samples until the handle is dropped.
    pub fn start(&self) -> Result<CaptureHandle> {
        let host = cpal::default_host();
        let device = if self.cfg.input_device.is_empty() {
            host.default_input_device()
                .ok_or_else(|| anyhow!("no default input device"))?
        } else {
            host.input_devices()?
                .find(|d| {
                    d.name()
                        .map(|n| n == self.cfg.input_device)
                        .unwrap_or(false)
                })
                .ok_or_else(|| anyhow!("input device {:?} not found", self.cfg.input_device))?
        };

        let supported = device
            .default_input_config()
            .context("default_input_config failed")?;
        debug!(
            "capture: device={:?} rate={} ch={} fmt={:?}",
            device.name(),
            supported.sample_rate().0,
            supported.channels(),
            supported.sample_format()
        );

        let device_rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        let target_rate = self.cfg.target_sample_rate;

        let cap_samples = (HARD_CAP.as_secs() as usize) * (target_rate as usize);

        let buffer = Arc::new(Mutex::new(RecordingBuffer::default()));
        let buffer_cb = Arc::clone(&buffer);

        let mut resampler = if device_rate == target_rate {
            None
        } else {
            Some(Resampler::new(device_rate, target_rate)?)
        };

        let err_cb = |e| warn!("cpal stream error: {e}");

        let fmt = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();

        let stream = match fmt {
            SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    let mono = to_mono_f32(data, channels);
                    let resampled = match resampler.as_mut() {
                        Some(r) => r.process(&mono),
                        None => mono,
                    };
                    if let Ok(mut b) = buffer_cb.lock() {
                        b.push_slice(&resampled, cap_samples);
                    }
                },
                err_cb,
                None,
            )?,
            SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    let f: Vec<f32> = data.iter().map(|s| s.to_sample::<f32>()).collect();
                    let mono = to_mono_f32(&f, channels);
                    let resampled = match resampler.as_mut() {
                        Some(r) => r.process(&mono),
                        None => mono,
                    };
                    if let Ok(mut b) = buffer_cb.lock() {
                        b.push_slice(&resampled, cap_samples);
                    }
                },
                err_cb,
                None,
            )?,
            SampleFormat::U16 => device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    let f: Vec<f32> = data.iter().map(|s| s.to_sample::<f32>()).collect();
                    let mono = to_mono_f32(&f, channels);
                    let resampled = match resampler.as_mut() {
                        Some(r) => r.process(&mono),
                        None => mono,
                    };
                    if let Ok(mut b) = buffer_cb.lock() {
                        b.push_slice(&resampled, cap_samples);
                    }
                },
                err_cb,
                None,
            )?,
            other => return Err(anyhow!("unsupported cpal sample format {other:?}")),
        };

        stream.play()?;
        Ok(CaptureHandle {
            _stream: stream,
            buffer,
        })
    }
}

/// Collapse `n`-channel interleaved f32 to mono by averaging channels.
fn to_mono_f32(data: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return data.to_vec();
    }
    let frames = data.len() / channels;
    let mut out = Vec::with_capacity(frames);
    for i in 0..frames {
        let base = i * channels;
        let mut sum = 0.0;
        for c in 0..channels {
            sum += data[base + c];
        }
        out.push(sum / channels as f32);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_respects_cap() {
        let mut b = RecordingBuffer::default();
        b.push_slice(&[1.0; 100], 50);
        assert_eq!(b.len(), 50);
        assert!(b.was_truncated());
    }

    #[test]
    fn mono_averages_channels() {
        let out = to_mono_f32(&[1.0, 3.0, 2.0, 4.0], 2);
        assert_eq!(out, vec![2.0, 3.0]);
    }
}
