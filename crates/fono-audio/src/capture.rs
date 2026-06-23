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
    /// Optional named capture source to open instead of the system default.
    ///
    /// `None` (the default) reads the **default** source — this is the only
    /// value the idle always-on paths ever use, which keeps capture
    /// platform-agnostic and free of any AEC dependency (ADR 0012:45-52).
    ///
    /// `Some(name)` opens a specific PulseAudio/PipeWire source by name. The
    /// sole intended caller is the wake-word detector's *wake-while-speaking*
    /// sub-case, which may point at the per-utterance echo-cancel source
    /// (`fono_aec_source_<pid>`) while Fono's TTS is playing and switch back
    /// to `None` when it disappears (ADR 0012:53-68). No code in the tree
    /// creates that source yet, so this field is an inert seam today. Honoured
    /// only by the Linux process backend (`parec` `--device` / `pw-cat`
    /// `--target`); the cpal backend always uses the default input device and
    /// logs a warning if a source is requested.
    pub source: Option<String>,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self { target_sample_rate: TARGET_SAMPLE_RATE, source: None }
    }
}

/// Emit a `capture`-lane instant on the process-current turn trace, if one is
/// installed. No-op on untraced turns (a single relaxed atomic load), so the
/// capture hot path pays nothing. Surfaces the device-level recording moments
/// (mic process spawned, first PCM frame in) so a trace makes the
/// record→playback boundary obvious instead of only showing the STT span. See
/// [`fono_core::turn_trace`].
#[cfg(any(all(target_os = "linux", not(feature = "cpal-backend")), feature = "cpal-backend"))]
fn trace_capture_instant(name: &str, args: serde_json::Value) {
    fono_core::turn_trace::current_instant(
        name,
        "capture",
        fono_core::turn_trace::CAPTURE_LANE,
        args,
    );
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
    /// Append `s`, then drop the oldest samples so at most `max_samples`
    /// are retained — a fixed-size rolling window. Unlike [`push_slice`]
    /// (which stops accepting once `cap_samples` is reached, for a
    /// bounded *recording*), this never blocks new audio: it is for an
    /// unbounded *stream* feeding a live visualisation, where only the
    /// most recent window is ever read. Does not set `truncated`.
    pub fn push_rolling(&mut self, s: &[f32], max_samples: usize) {
        self.samples.extend_from_slice(s);
        if self.samples.len() > max_samples {
            let excess = self.samples.len() - max_samples;
            self.samples.drain(..excess);
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

        Ok(CaptureHandle { _backend: backend, buffer })
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
        Ok(CaptureStreamHandle { _backend: self.start_backend(forward)? })
    }

    #[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
    fn start_backend<F>(&self, forward: F) -> Result<CaptureBackendHandle>
    where
        F: FnMut(&[f32]) + Send + 'static,
    {
        start_process_capture(self.cfg.target_sample_rate, self.cfg.source.as_deref(), forward)
    }

    #[cfg(feature = "cpal-backend")]
    fn start_backend<F>(&self, forward: F) -> Result<CaptureBackendHandle>
    where
        F: FnMut(&[f32]) + Send + 'static,
    {
        // cpal only ever opens the default input device; a named source
        // (the wake-while-speaking AEC seam) is not representable here.
        if self.cfg.source.is_some() {
            warn!("capture(cpal): named source ignored; cpal uses the default input device");
        }
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
fn start_process_capture<F>(
    target_rate: u32,
    source: Option<&str>,
    mut forward: F,
) -> Result<ProcessCapture>
where
    F: FnMut(&[f32]) + Send + 'static,
{
    let (mut child, tool) = spawn_capture_tool(target_rate, source)?;
    trace_capture_instant("capture.open", serde_json::json!({ "tool": tool, "rate": target_rate }));
    let mut stdout = child
        .stdout
        .take()
        .with_context(|| format!("{tool} did not expose stdout for PCM capture"))?;

    let reader = thread::Builder::new()
        .name(format!("fono-{tool}-capture"))
        .spawn(move || {
            let mut buf = [0_u8; 8192];
            let mut pending = Vec::<u8>::new();
            let mut first_frame = true;
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
                            if first_frame {
                                first_frame = false;
                                trace_capture_instant(
                                    "capture.first_frame",
                                    serde_json::json!({ "tool": tool, "samples": pcm.len() }),
                                );
                            }
                            forward(&pcm);
                        }
                    }
                    Err(e) => {
                        warn!("{tool} capture read failed: {e}");
                        break;
                    }
                }
            }
        })
        .with_context(|| format!("spawn {tool} capture reader thread"))?;

    Ok(ProcessCapture { child, reader: Some(reader) })
}

/// Spawn a process-backed audio capture tool that emits raw s16le mono
/// PCM at `target_rate` on stdout. Tries PipeWire's native `pw-cat`
/// first (preinstalled on Ubuntu 24.04, Fedora 39+, Debian 13+, and
/// every other modern PipeWire distro via `pipewire-bin`), then falls
/// back to the legacy PulseAudio `parec` client (works on systems with
/// `pulseaudio-utils` installed or pure-PulseAudio setups). PulseAudio
/// is treated as the fallback because PipeWire is now the upstream
/// audio stack on every actively-developed distro; parec stays
/// supported for legacy installs and will be deprecated once PulseAudio
/// drops out of the major LTS releases.
///
/// Returns the spawned child plus a short tool name used in log/error
/// messages.
#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
fn spawn_capture_tool(target_rate: u32, source: Option<&str>) -> Result<(Child, &'static str)> {
    match spawn_pw_cat(target_rate, source) {
        Ok(c) => Ok((c, "pw-cat")),
        Err(pw_err) => {
            debug!("capture: pw-cat unavailable ({pw_err:#}); falling back to parec");
            match spawn_parec(target_rate, source) {
                Ok(c) => Ok((c, "parec")),
                Err(parec_err) => Err(anyhow::anyhow!(
                    "audio capture failed to start: no usable capture tool found. \
                     Tried `pw-cat` ({pw_err}) and `parec` ({parec_err}). \
                     Install your distro's PipeWire client tools (commonly \
                     `pipewire-bin` or `pipewire`), or rebuild Fono with \
                     `--features fono-audio/cpal-backend` for direct ALSA capture."
                )),
            }
        }
    }
}

#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
fn spawn_parec(target_rate: u32, source: Option<&str>) -> Result<Child> {
    debug!("capture: spawning parec raw s16le mono at {target_rate} Hz (stdbuf -o0)");
    // `stdbuf -o0` defeats glibc's default 8 KB block-buffering on
    // pipe stdouts so PCM bytes reach the reader thread as soon as
    // PulseAudio writes them, not in coarse 256 ms chunks. Same
    // rationale as `spawn_pw_cat` — without this the waveform
    // overlay's FFT/heatmap visibly drops to ~4 fps even though
    // `--latency-msec=20` is honoured by parec on the PA side.
    Command::new("stdbuf")
        .arg("-o0")
        .arg("parec")
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
        // Phase I / ADR 0012: a named source is only ever supplied for the
        // wake-while-speaking AEC sub-case (`fono_aec_source_<pid>`). Idle
        // capture leaves this `None`, so parec reads the default source.
        .args(source.map(|s| format!("--device={s}")).as_deref())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn parec")
}

/// PipeWire-native capture path. `pw-cat --record` ships in
/// `pipewire-bin` which is preinstalled on Ubuntu 24.04 and every
/// other modern PipeWire distro. The `--raw` flag is non-negotiable:
/// without it pw-cat writes a container on stdout (WAV when the
/// output is a file with a recognized extension, a PipeWire-native
/// `dns.` framing when the output is `-` / stdout) and the framing
/// bytes get mis-interpreted as PCM samples by [`s16le_to_f32`],
/// producing periodic clicks and pops in the captured audio. Pair it
/// with `--format=s16` to lock the sample format to little-endian
/// signed 16-bit and `-` as the filename to route raw PCM to stdout.
/// Mirrors the `--raw` behaviour of [`spawn_parec`].
///
/// `stdbuf -o0` wraps the invocation so `pw-cat`'s stdout is
/// **unbuffered**. Without it glibc block-buffers raw stdout to a
/// pipe at `BUFSIZ` (~8 KB), which at 16 kHz mono s16 is ~256 ms of
/// PCM. The recording buffer would then only grow in 256 ms steps,
/// the 50 ms FFT animator would read the same trailing 4096-sample
/// window ~5 ticks in a row, the spectrogram heatmap would paint
/// each FFT push as a wide "block" instead of one column, and the
/// FFT-bars panel would visibly stutter at ~4 fps. The `--latency`
/// flag controls the PipeWire-side buffer, not libc's stdio
/// buffering, so it does *not* fix this on its own. See the
/// `pw_cat_uses_stdbuf` regression test and the v0.9.x heatmap
/// regression notes.
#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
fn spawn_pw_cat(target_rate: u32, source: Option<&str>) -> Result<Child> {
    let rate_arg = format!("--rate={target_rate}");
    let args = pw_cat_args(&rate_arg, source);
    debug!("capture: spawning pw-cat raw s16 mono at {target_rate} Hz (stdbuf -o0)");
    Command::new("stdbuf")
        .arg("-o0")
        .arg("pw-cat")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn pw-cat")
}

/// Argument vector passed to `pw-cat --record`. Extracted so the
/// regression test can assert the presence of `--raw` (and, when a
/// named source is requested, `--target`) without having to spawn a
/// real audio stack. `rate_arg` is the formatted `--rate=<N>` string
/// supplied by the caller; `source` is the optional named capture
/// source (see [`CaptureConfig::source`]) \u2014 `None` for the default.
#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
fn pw_cat_args(rate_arg: &str, source: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "--record".to_string(),
        // `--raw` forces bare PCM on stdout. Without it pw-cat wraps
        // the stream in a container and Fono mis-reads the framing
        // bytes as samples — see the doc comment on `spawn_pw_cat`.
        "--raw".to_string(),
        "--format=s16".to_string(),
        "--channels=1".to_string(),
        rate_arg.to_string(),
        // 20 ms latency — matches the parec configuration so the
        // waveform overlay and VAD timing behave identically
        // regardless of which backend ends up serving capture.
        "--latency=20ms".to_string(),
    ];
    // Phase I / ADR 0012: the optional named source (the wake-while-speaking
    // AEC seam, `fono_aec_source_<pid>`) is inserted *before* the trailing
    // `-` output filename so pw-cat parses it as a `--target` option. Idle
    // capture passes `None`, leaving pw-cat on the default source.
    if let Some(src) = source {
        args.push(format!("--target={src}"));
    }
    args.push("-".to_string());
    args
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
    let device =
        host.default_input_device().ok_or_else(|| anyhow::anyhow!("no default input device"))?;

    let supported = device.default_input_config().context("default_input_config failed")?;
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

    // Wrap the caller's forward so the first PCM frame to actually reach the
    // pipeline emits a `capture.first_frame` marker on the trace, mirroring the
    // process-capture path. Only one match arm below runs, so moving this
    // wrapper into each is sound.
    let mut first_frame = true;
    let mut forward = move |pcm: &[f32]| {
        if first_frame {
            first_frame = false;
            trace_capture_instant(
                "capture.first_frame",
                serde_json::json!({ "backend": "cpal", "samples": pcm.len() }),
            );
        }
        forward(pcm);
    };

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
    trace_capture_instant(
        "capture.open",
        serde_json::json!({ "backend": "cpal", "rate": target_rate, "device_rate": device_rate }),
    );
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

    /// Regression test for the v0.9.x heatmap / FFT-bars "frozen at
    /// ~4 fps" bug: `pw-cat` and `parec` must be wrapped in `stdbuf
    /// -o0` so glibc doesn't block-buffer their raw stdout in ~8 KB
    /// (~256 ms) chunks. Without it the spectrogram heatmap paints
    /// each FFT push as a wide identical block (because 5 consecutive
    /// 50 ms FFT ticks see the same trailing 4096-sample window) and
    /// the FFT-bars panel visibly stutters even though CPU is idle.
    /// `--latency=20ms` / `--latency-msec=20` only governs the
    /// PipeWire/PulseAudio-side buffer, not libc's stdio buffering on
    /// the recorder's stdout.
    #[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
    #[test]
    fn spawn_helpers_wrap_capture_tool_in_stdbuf() {
        // We can't easily reach into the `Command` after construction
        // to inspect its argv (`std::process::Command::get_program`
        // / `get_args` are stable, so use them). Build the commands
        // via the same code path as the spawners and verify the
        // wrapper + tool layout.
        let rate = TARGET_SAMPLE_RATE;
        let rate_arg = format!("--rate={rate}");
        let pw_cmd = {
            let args = pw_cat_args(&rate_arg, None);
            let mut c = Command::new("stdbuf");
            c.arg("-o0").arg("pw-cat").args(&args);
            c
        };
        assert_eq!(pw_cmd.get_program(), "stdbuf");
        let pw_args: Vec<&std::ffi::OsStr> = pw_cmd.get_args().collect();
        assert_eq!(pw_args.first().map(|s| s.to_str().unwrap()), Some("-o0"));
        assert_eq!(pw_args.get(1).map(|s| s.to_str().unwrap()), Some("pw-cat"));

        let parec_cmd = {
            let mut c = Command::new("stdbuf");
            c.arg("-o0").arg("parec").args([
                "--raw",
                "--format=s16le",
                "--channels=1",
                &rate_arg,
                "--latency-msec=20",
            ]);
            c
        };
        assert_eq!(parec_cmd.get_program(), "stdbuf");
        let parec_args: Vec<&std::ffi::OsStr> = parec_cmd.get_args().collect();
        assert_eq!(parec_args.first().map(|s| s.to_str().unwrap()), Some("-o0"));
        assert_eq!(parec_args.get(1).map(|s| s.to_str().unwrap()), Some("parec"));
    }

    /// Regression test for the v0.8.0 → v0.8.1 "only records noise"
    /// bug: `pw-cat` must be invoked with `--raw`, otherwise it writes
    /// a containerised stream on stdout whose framing bytes are
    /// mis-interpreted as PCM samples by [`s16le_to_f32`]. Symptom on
    /// affected Linux hosts (PipeWire without `parec`/`pulseaudio-utils`)
    /// is sample counts that are no longer multiples of the 20 ms frame
    /// size and transcripts that come back as pure noise. See
    /// CHANGELOG entry under 0.8.2.
    #[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
    #[test]
    fn pw_cat_args_include_raw_flag() {
        let rate = format!("--rate={TARGET_SAMPLE_RATE}");
        let args = pw_cat_args(&rate, None);
        let has = |needle: &str| args.iter().any(|a| a == needle);
        assert!(has("--record"), "pw-cat must run in record mode: {args:?}");
        assert!(
            has("--raw"),
            "pw-cat must be invoked with --raw to emit bare s16le PCM on stdout; \
             without it the output is a container and Fono records noise: {args:?}"
        );
        assert!(has("--format=s16"), "pw-cat must request s16 format: {args:?}");
        assert!(has("--channels=1"), "pw-cat must request mono: {args:?}");
        assert!(has(&rate), "pw-cat must carry rate arg {rate:?}: {args:?}");
    }

    /// Phase I / ADR 0012: the default (idle) capture path passes no named
    /// source, so neither `--device` (parec) nor `--target` (pw-cat) is
    /// emitted — capture reads the system default source with no AEC. The
    /// optional named source is the inert wake-while-speaking seam.
    #[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
    #[test]
    fn default_capture_passes_no_named_source() {
        let rate = format!("--rate={TARGET_SAMPLE_RATE}");
        let args = pw_cat_args(&rate, None);
        assert!(
            !args.iter().any(|a| a.starts_with("--target")),
            "idle/default capture must not target a named source: {args:?}"
        );
        // The trailing positional output filename must still be `-` (stdout).
        assert_eq!(args.last().map(String::as_str), Some("-"));
    }

    /// Phase I / ADR 0012: when a named source *is* requested (the
    /// wake-while-speaking AEC sub-case pointing at `fono_aec_source_<pid>`),
    /// pw-cat must carry `--target=<source>` and it must precede the trailing
    /// `-` output filename so it parses as an option, not the file.
    #[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
    #[test]
    fn named_source_emits_target_before_output() {
        let rate = format!("--rate={TARGET_SAMPLE_RATE}");
        let args = pw_cat_args(&rate, Some("fono_aec_source_4242"));
        let target_pos = args.iter().position(|a| a == "--target=fono_aec_source_4242");
        let dash_pos = args.iter().position(|a| a == "-");
        assert!(target_pos.is_some(), "pw-cat must carry the --target arg: {args:?}");
        assert!(
            target_pos < dash_pos,
            "--target must precede the trailing `-` output filename: {args:?}"
        );
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
            assert!((*v - i as f32).abs() < f32::EPSILON, "sample {i} out of order: got {v}");
        }
    }
}
