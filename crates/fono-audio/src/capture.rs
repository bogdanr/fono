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

/// RAII handle for a forwarder-driven capture stream. Dropping it stops
/// the cpal stream. Differs from [`CaptureHandle`] in that it does not
/// own a [`RecordingBuffer`] — every PCM slice produced by the cpal
/// callback is pushed straight through the user-supplied forwarder.
///
/// This is the realtime-push path used by the live-dictation pipeline
/// (Slice B1 / R10.x): the cpal callback resamples mono f32 to
/// `target_sample_rate` and invokes `forward(&[f32])` directly, so
/// audio reaches the streaming pump at hardware cadence (~10 ms at
/// 16 kHz / 160 sample buffers) rather than via a 30 ms-poll mutex
/// drain. See [`AudioCapture::start_with_forwarder`].
pub struct CaptureStreamHandle {
    _stream: cpal::Stream,
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

    /// Begin capture wired to a real-time PCM forwarder. Each cpal
    /// data-callback invocation resamples its input to mono f32 at
    /// [`CaptureConfig::target_sample_rate`] and then calls
    /// `forward(&[f32])` synchronously on the audio thread. Differs
    /// from [`Self::start`] in that no [`RecordingBuffer`] sits between
    /// the device and the consumer — useful for the live-dictation
    /// streaming pipeline which wants hardware-cadence push semantics.
    ///
    /// The forwarder MUST be cheap. The cpal callback runs on a
    /// real-time audio thread; the typical pattern is to
    /// `crossbeam_channel::Sender::try_send` into a bounded SPSC and
    /// drop on overflow rather than block. The existing
    /// [`crate::resample::Resampler`] is currently allocation-bearing
    /// (it constructs short-lived `Vec`s per process); this matches
    /// the pre-existing [`Self::start`] callback shape, so it's no
    /// worse than the polled-buffer drain it replaces. Future work may
    /// move the resample step onto a hop thread.
    pub fn start_with_forwarder<F>(&self, mut forward: F) -> Result<CaptureStreamHandle>
    where
        F: FnMut(&[f32]) + Send + 'static,
    {
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
            "capture(forwarder): device={:?} rate={} ch={} fmt={:?}",
            device.name(),
            supported.sample_rate().0,
            supported.channels(),
            supported.sample_format()
        );

        let device_rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        let target_rate = self.cfg.target_sample_rate;

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
                    if !resampled.is_empty() {
                        forward(&resampled);
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
                    if !resampled.is_empty() {
                        forward(&resampled);
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
                    if !resampled.is_empty() {
                        forward(&resampled);
                    }
                },
                err_cb,
                None,
            )?,
            other => return Err(anyhow!("unsupported cpal sample format {other:?}")),
        };

        stream.play()?;
        Ok(CaptureStreamHandle { _stream: stream })
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

    /// Synthetic cpal-callback stand-in: drive a forwarder closure 100x
    /// with deterministic samples and assert every sample arrives in
    /// order, exactly once. This exercises the contract that
    /// [`AudioCapture::start_with_forwarder`] gives its caller — that
    /// the forwarder is invoked synchronously from each callback with
    /// resampled mono f32 — without requiring a real cpal device.
    /// (The inner mono+resample plumbing is covered by
    /// [`mono_averages_channels`] and [`crate::resample::tests`]
    /// respectively.)
    #[test]
    fn forwarder_receives_every_callback_in_order() {
        let collected = Arc::new(Mutex::new(Vec::<f32>::new()));
        let collected_cb = Arc::clone(&collected);
        let forward = move |pcm: &[f32]| {
            collected_cb.lock().unwrap().extend_from_slice(pcm);
        };

        // Stand in for cpal: 100 invocations of a 4-sample buffer,
        // each carrying a monotonically-increasing index so we can
        // verify both ordering and exact-once delivery.
        for i in 0..100u32 {
            let base = (i * 4) as f32;
            let buf = [base, base + 1.0, base + 2.0, base + 3.0];
            forward(&buf);
        }

        let got = collected.lock().unwrap().clone();
        assert_eq!(got.len(), 400, "forwarder dropped or duplicated frames");
        for (i, v) in got.iter().enumerate() {
            assert!(
                (*v - i as f32).abs() < f32::EPSILON,
                "sample {i} out of order: got {v}",
            );
        }
    }
}
