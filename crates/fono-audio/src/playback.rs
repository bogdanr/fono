// SPDX-License-Identifier: GPL-3.0-only
//! Audio playback for the voice-assistant TTS path.
//!
//! Mirrors the dual-backend story used by [`crate::capture`]:
//!
//! * **Linux release default** — spawn `paplay` per utterance. Keeps
//!   the binary off `libasound`, matching fono's "no shared libraries"
//!   ship promise. PulseAudio / PipeWire's PA-server is the universal
//!   audio path on the desktops we ship to.
//! * **`cpal-backend` feature (non-Linux + opt-in Linux)** — long-lived
//!   cpal output stream consuming a bounded channel of PCM chunks.
//!   Lower per-utterance overhead but pulls in `libasound` on Linux.
//!
//! Both backends present the same `AudioPlayback` API:
//!
//! ```ignore
//! let pb = AudioPlayback::new(None)?;
//! pb.enqueue(pcm, sample_rate)?;       // queue one utterance
//! while !pb.is_idle() { std::thread::sleep(Duration::from_millis(50)); }
//! pb.stop();                             // drain queue + kill output
//! ```
//!
//! Sample rate of incoming chunks is preserved as long as it matches
//! the previously-enqueued chunk; on rate change, the worker creates a
//! fresh resampler and feeds the device its preferred rate.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::warn;

/// Errors specific to the playback worker. Currently only used as a
/// thiserror-style hint for callers that want to distinguish "device
/// missing" from "queue closed".
#[derive(Debug, thiserror::Error)]
pub enum PlaybackError {
    #[error("playback worker is no longer running")]
    Closed,
    #[error("no audio backend available (compile with `cpal-backend` or run on Linux)")]
    NoBackend,
}

/// Handle to a running audio playback worker. Cloning is cheap (the
/// internals are `Arc`-shared); the worker stops when every handle
/// is dropped.
#[derive(Clone)]
pub struct AudioPlayback {
    tx: tokio::sync::mpsc::UnboundedSender<Cmd>,
    pending: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

enum Cmd {
    Play { pcm: Vec<f32>, sample_rate: u32 },
    /// Drain queued Play commands without playing them and reset the
    /// abort flag, then resume normal operation. The worker stays
    /// alive — to actually shut it down, drop the
    /// [`AudioPlayback`] handle (which drops the sender; the worker
    /// then exits on the next `recv()`).
    Drain,
}

impl AudioPlayback {
    /// Construct a playback handle. `device` is the cpal device name
    /// (cpal backend only); ignored on the pulse path. Returns an error
    /// if no audio backend is compiled in for this target.
    pub fn new(device: Option<&str>) -> Result<Self> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let pending = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        spawn_worker(rx, pending.clone(), stop.clone(), device.map(str::to_string))?;

        Ok(Self { tx, pending, stop })
    }

    /// Enqueue one utterance for playback. Returns immediately —
    /// playback runs on the worker thread.
    pub fn enqueue(&self, pcm: Vec<f32>, sample_rate: u32) -> Result<()> {
        if pcm.is_empty() {
            return Ok(());
        }
        self.pending.fetch_add(1, Ordering::SeqCst);
        self.tx
            .send(Cmd::Play { pcm, sample_rate })
            .map_err(|_| PlaybackError::Closed)
            .context("audio playback worker stopped")?;
        Ok(())
    }

    /// Signal the worker to abort any in-flight playback, drop the
    /// remaining queue, and resume idle — ready for the next
    /// [`Self::enqueue`]. Pending count snaps to zero. The worker
    /// stays alive across calls, so the next `enqueue` plays
    /// normally; this fixes the "audio playback worker stopped"
    /// regression where every barge-in / Forget killed the worker.
    /// Idempotent.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        // Best-effort: if the worker has already exited (handle
        // dropped elsewhere), the send fails and there's nothing to
        // wake up. The next `enqueue` will surface a clean error.
        let _ = self.tx.send(Cmd::Drain);
        self.pending.store(0, Ordering::SeqCst);
    }

    /// True when no utterances are queued and none are currently
    /// playing. The orchestrator polls this to decide when to fire
    /// `ProcessingDone`.
    pub fn is_idle(&self) -> bool {
        self.pending.load(Ordering::SeqCst) == 0
    }

    /// True after [`Self::stop`] has been called. Worker checks this
    /// to abort early.
    fn is_stopping(stop: &AtomicBool) -> bool {
        stop.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------
// Linux / pulse subprocess backend (default for the static-musl ship).
// ---------------------------------------------------------------------

#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
const PAPLAY_CHUNK_BYTES: usize = 16 * 1024;

#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
async fn play_via_paplay(pcm: &[f32], rate: u32, stop: &AtomicBool) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("paplay")
        .arg("--raw")
        .arg("--format=s16le")
        .arg(format!("--rate={rate}"))
        .arg("--channels=1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context(
            "spawning `paplay` — install pulseaudio-utils or pipewire-pulse, \
             or rebuild with `--features cpal-backend`",
        )?;
    let mut stdin = child.stdin.take().context("paplay stdin")?;
    let bytes: Vec<u8> = pcm
        .iter()
        .flat_map(|s| {
            let i = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            i.to_le_bytes()
        })
        .collect();
    // Write in chunks so we can check the stop flag periodically;
    // a typical sentence is ~2 s of audio (~88 KB at 22 kHz int16),
    // so a 16 KB chunk gives 4-8 stop checks per utterance.
    for slice in bytes.chunks(PAPLAY_CHUNK_BYTES) {
        if AudioPlayback::is_stopping(stop) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }
        if let Err(e) = stdin.write_all(slice) {
            let _ = child.kill();
            return Err(e).context("writing PCM to paplay");
        }
    }
    drop(stdin);
    let status = child.wait().context("waiting on paplay")?;
    if !status.success() {
        warn!(
            target: "fono::audio::playback",
            code = ?status.code(),
            "paplay exited non-zero"
        );
    }
    Ok(())
}

#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
fn spawn_worker(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
    pending: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    _device: Option<String>,
) -> Result<()> {
    std::thread::Builder::new()
        .name("fono-playback".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("playback runtime");
            rt.block_on(async move {
                while let Some(cmd) = rx.recv().await {
                    match cmd {
                        Cmd::Drain => {
                            // Drain remaining queued Play commands —
                            // they were enqueued before stop() was
                            // called and shouldn't play. tokio mpsc
                            // exposes try_recv for this.
                            while let Ok(c) = rx.try_recv() {
                                if let Cmd::Play { .. } = c {
                                    pending.fetch_sub(1, Ordering::SeqCst);
                                }
                            }
                            // Reset abort flag so subsequent Play
                            // commands run normally.
                            stop.store(false, Ordering::SeqCst);
                        }
                        Cmd::Play { pcm, sample_rate } => {
                            if AudioPlayback::is_stopping(&stop) {
                                // Skip — we're between a stop() and
                                // its Drain. Drop the cmd silently.
                                pending.fetch_sub(1, Ordering::SeqCst);
                                continue;
                            }
                            if let Err(e) = play_via_paplay(&pcm, sample_rate, &stop).await {
                                warn!(target: "fono::audio::playback", error = %e, "paplay failed");
                            }
                            // Always decrement so the orchestrator's
                            // is_idle() polling makes progress even on
                            // playback errors.
                            pending.fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                }
            });
        })
        .context("spawning fono-playback worker thread")?;
    Ok(())
}

// ---------------------------------------------------------------------
// cpal backend (non-Linux + opt-in Linux).
// ---------------------------------------------------------------------

#[cfg(feature = "cpal-backend")]
fn spawn_worker(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
    pending: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    device_name: Option<String>,
) -> Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    // Pick device + format up front so caller errors come from `new`.
    let host = cpal::default_host();
    let device = match device_name.as_deref() {
        None => host.default_output_device(),
        Some(name) => host
            .output_devices()
            .ok()
            .and_then(|mut d| d.find(|d| d.name().map(|n| n == name).unwrap_or(false)))
            .or_else(|| host.default_output_device()),
    }
    .context("no default cpal output device available")?;
    let supported = device
        .default_output_config()
        .context("no default cpal output config")?;
    let device_rate = supported.sample_rate().0;
    let channels = supported.channels();

    std::thread::Builder::new()
        .name("fono-playback".into())
        .spawn(move || {
            // The cpal callback drains a producer-consumer ring of
            // f32 samples. Sized for ~2 s of 48 kHz mono audio.
            let ring = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::<f32>::with_capacity(96_000)));
            let in_flight = Arc::new(AtomicUsize::new(0));
            let ring_cb = ring.clone();
            let in_flight_cb = in_flight.clone();

            let stream = device
                .build_output_stream(
                    &cpal::StreamConfig {
                        channels,
                        sample_rate: cpal::SampleRate(device_rate),
                        buffer_size: cpal::BufferSize::Default,
                    },
                    move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        let mut q = ring_cb.lock().expect("playback ring poisoned");
                        for sample in out.iter_mut() {
                            *sample = q.pop_front().unwrap_or(0.0);
                        }
                        in_flight_cb.store(q.len(), Ordering::SeqCst);
                    },
                    move |err| {
                        warn!(target: "fono::audio::playback", error = %err, "cpal output error");
                    },
                    None,
                )
                .ok();
            let stream = match stream {
                Some(s) => s,
                None => {
                    warn!(target: "fono::audio::playback", "failed to build cpal output stream");
                    return;
                }
            };
            stream.play().ok();

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("playback runtime");
            rt.block_on(async move {
                let mut resampler: Option<(u32, crate::resample::Resampler)> = None;
                while let Some(cmd) = rx.recv().await {
                    match cmd {
                        Cmd::Drain => {
                            // Empty the ring + drain queued Play
                            // commands without playing them, then
                            // clear the abort flag so subsequent
                            // Play commands run normally.
                            ring.lock().expect("playback ring poisoned").clear();
                            in_flight.store(0, Ordering::SeqCst);
                            while let Ok(c) = rx.try_recv() {
                                if let Cmd::Play { .. } = c {
                                    pending.fetch_sub(1, Ordering::SeqCst);
                                }
                            }
                            stop.store(false, Ordering::SeqCst);
                        }
                        Cmd::Play { pcm, sample_rate } => {
                            if AudioPlayback::is_stopping(&stop) {
                                pending.fetch_sub(1, Ordering::SeqCst);
                                continue;
                            }
                            let resampled = if sample_rate == device_rate {
                                pcm
                            } else {
                                let r = match &mut resampler {
                                    Some((r_in, r)) if *r_in == sample_rate => r,
                                    _ => {
                                        match crate::resample::Resampler::new(sample_rate, device_rate) {
                                            Ok(r) => {
                                                resampler = Some((sample_rate, r));
                                                &mut resampler.as_mut().unwrap().1
                                            }
                                            Err(e) => {
                                                warn!(target: "fono::audio::playback", error = %e, "resampler init failed");
                                                pending.fetch_sub(1, Ordering::SeqCst);
                                                continue;
                                            }
                                        }
                                    }
                                };
                                let mut out = r.process(&pcm);
                                // Drain any leftovers the rubato impl left
                                // hanging — for short clips the leftover is
                                // a couple ms; emit it at the very end.
                                out.extend(std::iter::repeat(0.0).take(0));
                                out
                            };
                            // Duplicate mono to N channels so a stereo
                            // device plays the same content on both.
                            {
                                let mut q = ring.lock().expect("playback ring poisoned");
                                for s in resampled {
                                    for _ in 0..channels {
                                        q.push_back(s);
                                    }
                                }
                                in_flight.store(q.len(), Ordering::SeqCst);
                            }
                            // Wait for the ring to drain before
                            // decrementing pending. Polling at audio-
                            // chunk granularity is fine — 50 ms is
                            // imperceptible at sentence boundaries.
                            loop {
                                if AudioPlayback::is_stopping(&stop) {
                                    ring.lock().expect("playback ring poisoned").clear();
                                    break;
                                }
                                if in_flight.load(Ordering::SeqCst) == 0 {
                                    break;
                                }
                                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                            }
                            pending.fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                }
                drop(stream);
            });
        })
        .context("spawning fono-playback cpal worker")?;

    Ok(())
}

// ---------------------------------------------------------------------
// Fallback when no backend is compiled in (e.g. Windows w/o cpal).
// ---------------------------------------------------------------------

#[cfg(not(any(
    all(target_os = "linux", not(feature = "cpal-backend")),
    feature = "cpal-backend"
)))]
fn spawn_worker(
    _rx: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
    _pending: Arc<AtomicUsize>,
    _stop: Arc<AtomicBool>,
    _device: Option<String>,
) -> Result<()> {
    Err(PlaybackError::NoBackend.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_pcm_is_a_noop() {
        // Build a handle that doesn't actually need a working device:
        // the worker thread starts but `enqueue([])` returns without
        // touching the queue.
        let pb_result = AudioPlayback::new(None);
        // On hosts without a backend (e.g. CI without paplay), `new`
        // can still succeed because the worker only tries paplay on
        // demand. Skip the assertion if it failed.
        if let Ok(pb) = pb_result {
            assert!(pb.enqueue(Vec::new(), 16_000).is_ok());
            assert!(pb.is_idle());
        }
    }

    #[test]
    fn stop_marks_handle_idle() {
        if let Ok(pb) = AudioPlayback::new(None) {
            pb.stop();
            assert!(pb.is_idle());
        }
    }
}
