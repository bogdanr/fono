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

/// Milliseconds of digital silence prepended to every enqueued utterance.
///
/// The Linux default backend spawns a fresh `pw-play`/`paplay` process per
/// utterance, and an idle PipeWire/Pulse sink (or a USB/Bluetooth DAC that has
/// powered down) takes a moment to actually start moving samples. Without a
/// lead-in, that wake latency eats the first phonemes — Piper starts at the
/// first phoneme with no natural leading silence, so the very start of a word
/// gets clipped. A short silent head wakes the device before real audio plays;
/// because the sink plays its buffer in order, the silence is heard *after* the
/// device is up, so no speech is lost. 150 ms covers built-in/USB DACs
/// comfortably while staying below the ~250 ms "feels laggy" threshold and
/// reading as a natural inter-sentence pause. (A fully-cold Bluetooth link can
/// take longer to wake; the pad still prevents clipping there — the first word
/// may just start slightly late.)
const LEAD_IN_MS: u32 = 150;

/// Prepend [`LEAD_IN_MS`] of digital silence (zero samples at `sample_rate`) to
/// `pcm`. Caller guarantees `pcm` is non-empty; the silent head count is
/// `round(sample_rate * LEAD_IN_MS / 1000)`.
fn with_lead_in(pcm: Vec<f32>, sample_rate: u32) -> Vec<f32> {
    let lead = (u64::from(sample_rate) * u64::from(LEAD_IN_MS) / 1000) as usize;
    if lead == 0 {
        return pcm;
    }
    let mut out = Vec::with_capacity(lead + pcm.len());
    out.resize(lead, 0.0);
    out.extend_from_slice(&pcm);
    out
}

enum Cmd {
    Play {
        pcm: Vec<f32>,
        sample_rate: u32,
    },
    /// Open a gapless streaming session. The worker plays subsequent
    /// [`Cmd::StreamChunk`]s of the same utterance back-to-back without the
    /// drain-between-`Play` gap, then closes on [`Cmd::StreamEnd`]. Counts as a
    /// single pending unit (incremented when the session opens, decremented
    /// when it ends or is aborted). The output device / resampler is created
    /// lazily from the first chunk's sample rate.
    StreamBegin,
    /// One slice of the open streaming session's utterance.
    StreamChunk {
        pcm: Vec<f32>,
        sample_rate: u32,
    },
    /// Close the open streaming session: flush remaining audio, then mark the
    /// pending unit done.
    StreamEnd,
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
    ///
    /// A short [`LEAD_IN_MS`] head of digital silence is prepended so the
    /// audio sink has time to wake before real speech plays (see the constant's
    /// docs). Empty input stays a no-op.
    pub fn enqueue(&self, pcm: Vec<f32>, sample_rate: u32) -> Result<()> {
        if pcm.is_empty() {
            return Ok(());
        }
        let pcm = with_lead_in(pcm, sample_rate);
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

    /// Open a gapless streaming session for one utterance.
    ///
    /// Follow with one or more [`Self::push_stream`] calls and a final
    /// [`Self::end_stream`]. Unlike [`Self::enqueue`] (one process / one
    /// drain-gated `Play` per utterance), chunks of a session play back-to-back
    /// with no inserted silence, so intra-utterance streaming TTS keeps audio
    /// continuous. The session counts as a single pending unit for
    /// [`Self::is_idle`]. A [`LEAD_IN_MS`] silent head is written once, before
    /// the first chunk.
    pub fn begin_stream(&self) -> Result<()> {
        self.pending.fetch_add(1, Ordering::SeqCst);
        self.tx
            .send(Cmd::StreamBegin)
            .map_err(|_| PlaybackError::Closed)
            .context("audio playback worker stopped")?;
        Ok(())
    }

    /// Append one chunk of PCM to the open streaming session. Empty input is a
    /// no-op. `sample_rate` should be constant across a session; the first
    /// chunk's rate fixes the output stream / resampler.
    pub fn push_stream(&self, pcm: Vec<f32>, sample_rate: u32) -> Result<()> {
        if pcm.is_empty() {
            return Ok(());
        }
        self.tx
            .send(Cmd::StreamChunk { pcm, sample_rate })
            .map_err(|_| PlaybackError::Closed)
            .context("audio playback worker stopped")?;
        Ok(())
    }

    /// Close the open streaming session and let its audio drain. After this the
    /// session's pending unit clears once playback finishes (poll
    /// [`Self::is_idle`]).
    pub fn end_stream(&self) -> Result<()> {
        self.tx
            .send(Cmd::StreamEnd)
            .map_err(|_| PlaybackError::Closed)
            .context("audio playback worker stopped")?;
        Ok(())
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

/// Emit a `playback`-lane instant on the process-current turn trace, if one is
/// installed. No-op on untraced turns (a single relaxed atomic load), so the
/// hot path pays nothing. Lets the trace show device-level playback moments
/// (player spawned, first audio out) instead of only the higher-level `tts`
/// synthesis spans — see [`fono_core::turn_trace`].
#[cfg(any(all(target_os = "linux", not(feature = "cpal-backend")), feature = "cpal-backend"))]
fn trace_playback_instant(name: &str, args: serde_json::Value) {
    fono_core::turn_trace::current_instant(
        name,
        "playback",
        fono_core::turn_trace::PLAYBACK_LANE,
        args,
    );
}

/// Emit a `playback`-lane duration span ending now, if a turn trace is current.
/// Renders as a bar whose left edge is when audio actually started reaching the
/// device. No-op on untraced turns.
#[cfg(any(all(target_os = "linux", not(feature = "cpal-backend")), feature = "cpal-backend"))]
fn trace_playback_span(name: &str, started: std::time::Instant, args: serde_json::Value) {
    if let Some(t) = fono_core::turn_trace::TurnTrace::current() {
        t.duration_between(
            name,
            "playback",
            fono_core::turn_trace::PLAYBACK_LANE,
            started,
            std::time::Instant::now(),
            args,
        );
    }
}

// ---------------------------------------------------------------------
// Linux / pulse subprocess backend (default for the static-musl ship).
// ---------------------------------------------------------------------

#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
const PAPLAY_CHUNK_BYTES: usize = 16 * 1024;

/// Convert mono `f32` PCM (-1.0..1.0) to signed 16-bit little-endian bytes,
/// the wire format `pw-play --format=s16` / `paplay --format=s16le` expect.
#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
fn pcm_to_s16le(pcm: &[f32]) -> Vec<u8> {
    pcm.iter()
        .flat_map(|s| {
            let i = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            i.to_le_bytes()
        })
        .collect()
}

/// Spawn a raw-PCM player (`pw-play`, falling back to `paplay`) reading mono
/// s16 from stdin at `rate`. Returns the child, its piped stdin, and the tool
/// name. Used by both the per-utterance [`play_via_paplay`] path and the
/// gapless streaming session.
#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
fn spawn_player(
    rate: u32,
) -> Result<(std::process::Child, std::process::ChildStdin, &'static str)> {
    use std::process::{Command, Stdio};
    let (mut child, tool) = match Command::new("pw-play")
        .arg("--raw")
        .arg(format!("--rate={rate}"))
        .arg("--channels=1")
        .arg("--format=s16")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => (c, "pw-play"),
        Err(pw_err) => {
            // Legacy fallback for PulseAudio-only systems. Will be
            // deprecated once PulseAudio drops out of the major LTS
            // releases (Ubuntu 22.04 LTS is the last one still
            // shipping it as default).
            match Command::new("paplay")
                .arg("--raw")
                .arg("--format=s16le")
                .arg(format!("--rate={rate}"))
                .arg("--channels=1")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(c) => (c, "paplay"),
                Err(paplay_err) => {
                    return Err(anyhow::anyhow!(
                        "no usable audio playback tool found. Tried `pw-play` \
                         ({pw_err}) and `paplay` ({paplay_err}). Install your \
                         distro's PipeWire client tools (commonly `pipewire-bin` \
                         or `pipewire`), or rebuild with `--features cpal-backend`."
                    ));
                }
            }
        }
    };
    let stdin = child.stdin.take().with_context(|| format!("{tool} stdin"))?;
    Ok((child, stdin, tool))
}

#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
async fn play_via_paplay(pcm: &[f32], rate: u32, stop: &AtomicBool) -> Result<()> {
    use std::io::Write;
    let (mut child, mut stdin, tool) = spawn_player(rate)?;
    let bytes = pcm_to_s16le(pcm);
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
            return Err(e).with_context(|| format!("writing PCM to {tool}"));
        }
    }
    drop(stdin);
    let status = child.wait().with_context(|| format!("waiting on {tool}"))?;
    if !status.success() {
        warn!(
            target: "fono::audio::playback",
            code = ?status.code(),
            tool,
            "{tool} exited non-zero"
        );
    }
    Ok(())
}

/// State of an open gapless streaming session on the paplay backend. The player
/// process is spawned lazily on the first chunk (its `--rate` needs the chunk's
/// sample rate). `lead_in_written` ensures the silent head is written exactly
/// once, before the first real audio.
#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
struct PaplayStream {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    tool: &'static str,
    rate: u32,
    lead_in_written: bool,
}

#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
impl PaplayStream {
    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn finish(mut self) {
        drop(self.stdin);
        if let Ok(status) = self.child.wait() {
            if !status.success() {
                warn!(
                    target: "fono::audio::playback",
                    code = ?status.code(),
                    tool = self.tool,
                    "{} exited non-zero (stream)", self.tool
                );
            }
        }
    }

    /// Write one PCM chunk to the player's stdin. On the first chunk, prepend
    /// the [`LEAD_IN_MS`] silent head once. Returns `Err` if the write fails
    /// (the player exited / the pipe broke) so the worker can drop the session.
    fn push(&mut self, pcm: &[f32]) -> std::io::Result<()> {
        use std::io::Write;
        if !self.lead_in_written {
            let lead = (u64::from(self.rate) * u64::from(LEAD_IN_MS) / 1000) as usize;
            if lead > 0 {
                self.stdin.write_all(&vec![0u8; lead * 2])?;
            }
            self.lead_in_written = true;
        }
        self.stdin.write_all(&pcm_to_s16le(pcm))
    }
}

#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]
/// Handle one streaming PCM chunk for the paplay backend: lazily spawn the
/// player on the first chunk (emitting the `playback.first_audio` marker and
/// recording `stream_started` for the closing span) and push the PCM.
fn handle_paplay_stream_chunk(
    session: &mut Option<PaplayStream>,
    stream_started: &mut Option<std::time::Instant>,
    pcm: &[f32],
    sample_rate: u32,
) {
    if session.is_none() {
        match spawn_player(sample_rate) {
            Ok((child, stdin, tool)) => {
                *session = Some(PaplayStream {
                    child,
                    stdin,
                    tool,
                    rate: sample_rate,
                    lead_in_written: false,
                });
                *stream_started = Some(std::time::Instant::now());
                trace_playback_instant(
                    "playback.first_audio",
                    serde_json::json!({ "backend": "paplay", "sample_rate": sample_rate, "streaming": true }),
                );
            }
            Err(e) => {
                warn!(target: "fono::audio::playback", error = %e, "paplay stream spawn failed");
                return;
            }
        }
    }
    if let Some(s) = session.as_mut() {
        if let Err(e) = s.push(pcm) {
            warn!(target: "fono::audio::playback", error = %e, "paplay stream write failed");
            if let Some(s) = session.take() {
                s.kill();
            }
        }
    }
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
                let mut session: Option<PaplayStream> = None;
                // Wall-clock of the first audio frame written to the device for
                // the open streaming session, used to close the `playback.stream`
                // trace span on StreamEnd.
                let mut stream_started: Option<std::time::Instant> = None;
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
                            // Abort any open streaming session too.
                            if let Some(s) = session.take() {
                                s.kill();
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
                            let samples = pcm.len();
                            let play_started = std::time::Instant::now();
                            if let Err(e) = play_via_paplay(&pcm, sample_rate, &stop).await {
                                warn!(target: "fono::audio::playback", error = %e, "paplay failed");
                            }
                            trace_playback_span(
                                "playback.play",
                                play_started,
                                serde_json::json!({ "backend": "paplay", "samples": samples, "sample_rate": sample_rate }),
                            );
                            // Always decrement so the orchestrator's
                            // is_idle() polling makes progress even on
                            // playback errors.
                            pending.fetch_sub(1, Ordering::SeqCst);
                        }
                        Cmd::StreamBegin => {
                            // The player is spawned lazily on the first chunk
                            // (its --rate needs the chunk's sample rate); just
                            // mark that a session is open by clearing any stale
                            // one.
                            if let Some(s) = session.take() {
                                s.kill();
                            }
                            stream_started = None;
                            trace_playback_instant(
                                "playback.stream_open",
                                serde_json::json!({ "backend": "paplay" }),
                            );
                        }
                        Cmd::StreamChunk { pcm, sample_rate } => {
                            if AudioPlayback::is_stopping(&stop) {
                                continue;
                            }
                            handle_paplay_stream_chunk(
                                &mut session,
                                &mut stream_started,
                                &pcm,
                                sample_rate,
                            );
                        }
                        Cmd::StreamEnd => {
                            if let Some(s) = session.take() {
                                s.finish();
                            }
                            if let Some(start) = stream_started.take() {
                                trace_playback_span(
                                    "playback.stream",
                                    start,
                                    serde_json::json!({ "backend": "paplay", "streaming": true }),
                                );
                            }
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

/// Resample one mono `f32` chunk from `sample_rate` to `device_rate`, reusing
/// (or lazily creating) the resampler in `slot`. Returns `None` only if
/// resampler init fails. A matching rate is a passthrough.
#[cfg(feature = "cpal-backend")]
fn cpal_resample(
    slot: &mut Option<(u32, crate::resample::Resampler)>,
    pcm: Vec<f32>,
    sample_rate: u32,
    device_rate: u32,
) -> Option<Vec<f32>> {
    if sample_rate == device_rate {
        return Some(pcm);
    }
    let r = match slot {
        Some((r_in, r)) if *r_in == sample_rate => r,
        _ => match crate::resample::Resampler::new(sample_rate, device_rate) {
            Ok(r) => {
                *slot = Some((sample_rate, r));
                &mut slot.as_mut().unwrap().1
            }
            Err(e) => {
                warn!(target: "fono::audio::playback", error = %e, "resampler init failed");
                return None;
            }
        },
    };
    Some(r.process(&pcm))
}

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
    let supported = device.default_output_config().context("no default cpal output config")?;
    let device_rate = supported.sample_rate().0;
    let channels = supported.channels();

    std::thread::Builder::new()
        .name("fono-playback".into())
        .spawn(move || {
            // The cpal callback drains a producer-consumer ring of
            // f32 samples. Sized for ~2 s of 48 kHz mono audio.
            let ring = Arc::new(std::sync::Mutex::new(
                std::collections::VecDeque::<f32>::with_capacity(96_000),
            ));
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
                // Wall-clock of the first chunk pushed to the always-draining
                // ring for the open streaming session; closes the
                // `playback.stream` trace span on StreamEnd.
                let mut stream_started: Option<std::time::Instant> = None;
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
                            let samples = pcm.len();
                            let play_started = std::time::Instant::now();
                            let resampled = match cpal_resample(
                                &mut resampler,
                                pcm,
                                sample_rate,
                                device_rate,
                            ) {
                                Some(r) => r,
                                None => {
                                    pending.fetch_sub(1, Ordering::SeqCst);
                                    continue;
                                }
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
                            trace_playback_span(
                                "playback.play",
                                play_started,
                                serde_json::json!({ "backend": "cpal", "samples": samples, "sample_rate": sample_rate }),
                            );
                            pending.fetch_sub(1, Ordering::SeqCst);
                        }
                        Cmd::StreamBegin => {
                            // The persistent cpal stream already plays the ring
                            // gaplessly, so a session needs no setup beyond the
                            // pending unit begin_stream() already counted.
                            stream_started = None;
                            trace_playback_instant(
                                "playback.stream_open",
                                serde_json::json!({ "backend": "cpal" }),
                            );
                        }
                        Cmd::StreamChunk { pcm, sample_rate } => {
                            if AudioPlayback::is_stopping(&stop) {
                                continue;
                            }
                            if stream_started.is_none() {
                                stream_started = Some(std::time::Instant::now());
                                trace_playback_instant(
                                    "playback.first_audio",
                                    serde_json::json!({ "backend": "cpal", "sample_rate": sample_rate, "streaming": true }),
                                );
                            }
                            let resampled = match cpal_resample(
                                &mut resampler,
                                pcm,
                                sample_rate,
                                device_rate,
                            ) {
                                Some(r) => r,
                                None => continue,
                            };
                            // Append straight to the ring — the callback drains
                            // it continuously, so chunks of one utterance play
                            // back-to-back with no inter-chunk gap.
                            let mut q = ring.lock().expect("playback ring poisoned");
                            for s in resampled {
                                for _ in 0..channels {
                                    q.push_back(s);
                                }
                            }
                            in_flight.store(q.len(), Ordering::SeqCst);
                        }
                        Cmd::StreamEnd => {
                            // Wait for the ring to drain, mirroring the Play
                            // arm, then clear the session's pending unit.
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
                            if let Some(start) = stream_started.take() {
                                trace_playback_span(
                                    "playback.stream",
                                    start,
                                    serde_json::json!({ "backend": "cpal", "streaming": true }),
                                );
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

    #[test]
    fn lead_in_prepends_expected_silence() {
        // 150 ms at 22.05 kHz = round(22050 * 150 / 1000) = 3307 samples.
        let pcm = vec![1.0_f32, -1.0, 0.5];
        let out = with_lead_in(pcm.clone(), 22_050);
        let lead = 22_050 * 150 / 1000;
        assert_eq!(out.len(), lead + pcm.len());
        assert!(out[..lead].iter().all(|&s| s == 0.0), "head must be silence");
        assert_eq!(&out[lead..], &pcm[..], "original samples follow unchanged");

        // 150 ms at 16 kHz = 2400 samples.
        let out16 = with_lead_in(vec![0.25_f32; 10], 16_000);
        assert_eq!(out16.len(), 16_000 * 150 / 1000 + 10);
    }

    #[test]
    fn lead_in_zero_rate_is_noop() {
        let pcm = vec![1.0_f32, 2.0, 3.0];
        assert_eq!(with_lead_in(pcm.clone(), 0), pcm);
    }
}
