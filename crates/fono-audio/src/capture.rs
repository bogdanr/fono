// SPDX-License-Identifier: GPL-3.0-only
//! Audio capture with ring-buffer + soft/hard recording caps.
//!
//! Linux release builds use a process-backed PulseAudio/PipeWire path
//! (`parec`) so the Fono binary does not link ALSA/libasound. The cpal
//! implementation remains available behind the `cpal-backend` feature for
//! non-Linux targets and explicit bare-ALSA Linux builds.

#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
use std::io::Read;
#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{debug, warn};

#[cfg(feature = "cpal-backend")]
use crate::resample::Resampler;
#[cfg(feature = "cpal-backend")]
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
#[cfg(feature = "cpal-backend")]
use cpal::{Sample, SampleFormat};

/// Target rate fed to whisper. Non-negotiable.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Hard cap: 5 minutes. Prevents runaway memory use.
pub const HARD_CAP: Duration = Duration::from_secs(5 * 60);

/// Soft cap: 2 minutes. Emits a warning event through the caller-supplied
/// channel (hook placeholder in this phase).
pub const SOFT_CAP: Duration = Duration::from_secs(2 * 60);

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Preferred sample rate (will resample if device doesn't support it).
    pub target_sample_rate: u32,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
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

/// Capture orchestrator. Owns the platform stream/process and a shared buffer.
pub struct AudioCapture {
    cfg: CaptureConfig,
}

/// RAII handle — dropping it stops capture.
pub struct CaptureHandle {
    _backend: CaptureBackendHandle,
    pub buffer: Arc<Mutex<RecordingBuffer>>,
}

/// RAII handle for a forwarder-driven capture stream. Dropping it stops
/// capture. Differs from [`CaptureHandle`] in that it does not own a
/// [`RecordingBuffer`] — every PCM slice produced by the backend is pushed
/// straight through the user-supplied forwarder.
pub struct CaptureStreamHandle {
    _backend: CaptureBackendHandle,
}

#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
type CaptureBackendHandle = ProcessCapture;
#[cfg(feature = "cpal-backend")]
type CaptureBackendHandle = cpal::Stream;
#[cfg(all(not(target_os = "linux"), not(feature = "cpal-backend")))]
struct CaptureBackendHandle;

impl AudioCapture {
    #[must_use]
    pub fn new(cfg: CaptureConfig) -> Self {
        Self { cfg }
    }

    /// Begin capture. Returns a handle whose `buffer` fills with resampled
    /// f32 mono samples until the handle is dropped.
    pub fn start(&self) -> Result<CaptureHandle> {
        let target_rate = self.cfg.target_sample_rate;
        let cap_samples = (HARD_CAP.as_secs() as usize) * (target_rate as usize);
        let buffer = Arc::new(Mutex::new(RecordingBuffer::default()));
        let buffer_cb = Arc::clone(&buffer);
        let backend = self.start_backend(move |pcm: &[f32]| {
            if let Ok(mut b) = buffer_cb.lock() {
                b.push_slice(pcm, cap_samples);
            }
        })?;

        Ok(CaptureHandle {
            _backend: backend,
            buffer,
        })
    }

    /// Begin capture wired to a real-time PCM forwarder. Each backend callback
    /// yields mono f32 at [`CaptureConfig::target_sample_rate`] and invokes
    /// `forward(&[f32])` synchronously on the capture thread.
    ///
    /// The forwarder MUST be cheap. The typical pattern is to
    /// `crossbeam_channel::Sender::try_send` into a bounded SPSC and drop on
    /// overflow rather than block.
    pub fn start_with_forwarder<F>(&self, forward: F) -> Result<CaptureStreamHandle>
    where
        F: FnMut(&[f32]) + Send + 'static,
    {
        Ok(CaptureStreamHandle {
            _backend: self.start_backend(forward)?,
        })
    }

    #[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
    fn start_backend<F>(&self, forward: F) -> Result<CaptureBackendHandle>
    where
        F: FnMut(&[f32]) + Send + 'static,
    {
        start_process_capture(self.cfg.target_sample_rate, forward)
    }

    #[cfg(feature = "cpal-backend")]
    fn start_backend<F>(&self, forward: F) -> Result<CaptureBackendHandle>
    where
        F: FnMut(&[f32]) + Send + 'static,
    {
        start_cpal_capture(self.cfg.target_sample_rate, forward)
    }

    #[cfg(all(not(target_os = "linux"), not(feature = "cpal-backend")))]
    fn start_backend<F>(&self, _forward: F) -> Result<CaptureBackendHandle>
    where
        F: FnMut(&[f32]) + Send + 'static,
    {
        Err(anyhow::anyhow!(
            "audio capture on this platform requires the fono-audio/cpal-backend feature"
        ))
    }
}

#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
struct ProcessCapture {
    child: Child,
    reader: Option<JoinHandle<()>>,
}

#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
impl Drop for ProcessCapture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
fn start_process_capture<F>(target_rate: u32, mut forward: F) -> Result<ProcessCapture>
where
    F: FnMut(&[f32]) + Send + 'static,
{
    let mut child = spawn_parec(target_rate)?;
    let mut stdout = child
        .stdout
        .take()
        .context("parec did not expose stdout for PCM capture")?;

    let reader = thread::Builder::new()
        .name("fono-parec-capture".into())
        .spawn(move || {
            let mut buf = [0_u8; 8192];
            let mut pending = Vec::<u8>::new();
            loop {
                match stdout.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        pending.extend_from_slice(&buf[..n]);
                        let even_len = pending.len() & !1;
                        if even_len == 0 {
                            continue;
                        }
                        let pcm = s16le_to_f32(&pending[..even_len]);
                        pending.drain(..even_len);
                        if !pcm.is_empty() {
                            forward(&pcm);
                        }
                    }
                    Err(e) => {
                        warn!("parec capture read failed: {e}");
                        break;
                    }
                }
            }
        })
        .context("spawn parec capture reader thread")?;

    Ok(ProcessCapture {
        child,
        reader: Some(reader),
    })
}

#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
fn spawn_parec(target_rate: u32) -> Result<Child> {
    debug!("capture: spawning parec raw s16le mono at {target_rate} Hz");
    Command::new("parec")
        .args([
            "--raw",
            "--format=s16le",
            "--channels=1",
            &format!("--rate={target_rate}"),
            // Cap the PulseAudio fragment at 20 ms so PCM lands in
            // small, frequent chunks. Without this PA picks a default
            // fragment of several hundred ms — fine for plain
            // capture, but it makes the waveform overlay's RMS tail
            // look frozen between chunks (each tick re-reads the
            // same bytes) and adds end-of-utterance latency to the
            // streaming pipeline. 20 ms matches typical voice-VAD
            // working sets and is well within PulseAudio's safe
            // range.
            "--latency-msec=20",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| {
            "spawn parec for PulseAudio/PipeWire capture failed; install PulseAudio/PipeWire \
             client tools or build with fono-audio/cpal-backend for bare ALSA"
        })
}

#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
fn s16le_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / i16::MAX as f32)
        .collect()
}

#[cfg(feature = "cpal-backend")]
fn start_cpal_capture<F>(target_rate: u32, mut forward: F) -> Result<cpal::Stream>
where
    F: FnMut(&[f32]) + Send + 'static,
{
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow::anyhow!("no default input device"))?;

    let supported = device
        .default_input_config()
        .context("default_input_config failed")?;
    debug!(
        "capture(cpal): device={:?} rate={} ch={} fmt={:?}",
        device.name(),
        supported.sample_rate().0,
        supported.channels(),
        supported.sample_format()
    );

    let device_rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;

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
        other => return Err(anyhow::anyhow!("unsupported cpal sample format {other:?}")),
    };

    stream.play()?;
    Ok(stream)
}

#[cfg(any(test, feature = "cpal-backend"))]
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

    #[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
    #[test]
    fn s16le_conversion_maps_samples() {
        let pcm = s16le_to_f32(&[0, 0, 0xff, 0x7f, 0x00, 0x80]);
        assert!(pcm[0].abs() < f32::EPSILON);
        assert!((pcm[1] - 1.0).abs() < 0.000_1);
        assert!((pcm[2] + 1.000_03).abs() < 0.000_1);
    }

    /// Synthetic callback stand-in: drive a forwarder closure 100x with
    /// deterministic samples and assert every sample arrives in order,
    /// exactly once. This exercises the contract that
    /// [`AudioCapture::start_with_forwarder`] gives its caller without
    /// requiring a real audio device.
    #[test]
    fn forwarder_receives_every_callback_in_order() {
        let collected = Arc::new(Mutex::new(Vec::<f32>::new()));
        let collected_cb = Arc::clone(&collected);
        let forward = move |pcm: &[f32]| {
            collected_cb.lock().unwrap().extend_from_slice(pcm);
        };

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
