// SPDX-License-Identifier: GPL-3.0-only
//! Shared voice I/O helpers used by the `fono.speak`, `fono.listen`, and
//! `fono.confirm` MCP tools.
//!
//! These functions are kept deliberately self-contained: they take the
//! relevant slices of `McpContext` and return `Result` so each tool can
//! map errors to its own `ToolCallResult::failure` text. No async
//! cancellation tokens, no shared singletons — every call constructs its
//! own backend, runs to completion, and tears down.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use fono_audio::{
    AudioCapture, AudioPlayback, CaptureConfig, EnvelopeConfig, EnvelopeFollower, RecordingBuffer,
    SilenceEvent, SilenceWatch, SilenceWatchConfig,
};
use fono_core::{config::Config, Secrets};
use fono_ipc::{McpPhase, Request, Response};
use fono_overlay::{
    spawn_overlay, IgnoreReason as OverlayIgnoreReason, OverlayHandle, OverlayState,
};
use fono_polish::TextFormatter;
use tracing::{debug, warn};

/// Frame size used when feeding samples to the envelope follower. 20 ms
/// at 16 kHz mono = 320 f32 samples — matches the capture backend's
/// `--latency=20ms` configuration so each backend chunk usually lines up
/// with one frame.
const ENVELOPE_FRAME_SAMPLES: usize = 320;

/// Amplitude that maps to "full" on every audio-driven visualisation.
/// Mirrors `crates/fono/src/session.rs:51`; duplicated here so the
/// MCP listen overlay matches the F7 dictation visualisation exactly.
const WAVEFORM_AMPLITUDE_CEILING: f32 = 0.22;

/// FFT window size for the `Fft` / `Heatmap` overlay styles. Mirrors
/// `crates/fono/src/session.rs:86`.
const WAVEFORM_FFT_SIZE: usize = 4096;

/// Upper frequency cutoff for the FFT and Fft-style visualisations.
/// Mirrors `crates/fono/src/session.rs:92`.
const WAVEFORM_FFT_MAX_HZ: f32 = 3000.0;

/// Upper frequency cutoff for the heatmap and 3D terrain
/// visualisations. Mirrors
/// `crates/fono/src/session.rs:98`.
const WAVEFORM_FFT_MAX_HZ_WIDE: f32 = 6000.0;

/// Display-bin count pushed to the overlay per FFT frame. Mirrors
/// `crates/fono/src/session.rs:102`.
const WAVEFORM_FFT_BINS: usize = 300;

/// dB range mapped to `[0, 1]` on the FFT / heatmap. Mirrors
/// `crates/fono/src/session.rs:110-112`.
const WAVEFORM_FFT_DB_FLOOR: f32 = -20.0;
const WAVEFORM_FFT_DB_CEILING: f32 = 30.0;

/// RMS of an f32 slice, normalised against
/// [`WAVEFORM_AMPLITUDE_CEILING`] and clamped to `[0, 1]`. Mirrors
/// `crates/fono/src/session.rs:117`.
fn normalised_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    let rms = (sum_sq / samples.len() as f32).sqrt();
    (rms / WAVEFORM_AMPLITUDE_CEILING).clamp(0.0, 1.0)
}

/// Default total-silence window before `fono.listen` auto-stops, in
/// milliseconds. Used when the user has not set
/// `[audio].auto_stop_silence_ms`.
///
/// 10_000 ms is a deliberately generous floor: the MCP listen loop runs
/// inside a coding-agent turn where the user may need a beat to think
/// before answering. Cutting them off after a couple of seconds turns
/// the agent into a stenographer rather than a partner. The
/// `silence-watch` state machine paints the visible `Pondering`
/// indicator from `pondering_visual_ms` (~1 s) onwards, so the user
/// still sees that Fono has noticed the pause; the 10 s ceiling only
/// drives the actual commit.
const MCP_LISTEN_DEFAULT_AUTO_STOP_MS: u32 = 10_000;

/// Hard ceiling on a single `listen` capture, in seconds. Belt-and-
/// braces alongside the caller-supplied `max_seconds`.
const LISTEN_HARD_CEILING_SECS: u64 = 300;

/// `voiced_rms × SILENCE_GAIN` is the dynamic silence threshold used
/// by the `Advanced` VU bar (matches `crates/fono/src/session.rs:79`).
/// 10^(-12 dB / 20) ≈ 0.2511886; mirrored here so the MCP listen
/// path produces the same diagnostic overlay metrics as F7 dictation
/// without pulling in the daemon's session crate.
const SILENCE_GAIN: f32 = 0.251_188_64;

/// RAII wrapper around an optional [`OverlayHandle`]. While alive the
/// overlay shows whatever state callers have driven it into; on
/// `Drop` it flips to `OverlayState::Hidden` and shuts down the
/// backend worker thread so the panel disappears on every exit path
/// (silence commit, timeout, STT error, panic).
///
/// Construction is best-effort: a `None` handle (either because the
/// spawn failed or because the caller decided not to drive the
/// overlay) makes every method a cheap no-op. This keeps the call
/// sites in `listen_once` straight-line — no `if let Some(o)`
/// noise — while the overlay stays optional.
pub(crate) struct OverlayGuard {
    handle: Option<OverlayHandle>,
}

impl OverlayGuard {
    /// Return a guard wrapping the process-wide shared
    /// [`OverlayHandle`]. The handle is spawned lazily on first call
    /// and reused for the lifetime of the MCP server process —
    /// winit's X11 `EventLoop` cannot be recreated after the first
    /// one is destroyed, so spawning per listen worked once and then
    /// every subsequent listen silently fell through to the `noop`
    /// backend. The persistent handle keeps the X11 window hidden
    /// between listens (cheap) and visible whenever a listen is
    /// active.
    ///
    /// Silently degrades to a no-op guard if `spawn_overlay` itself
    /// reports an error (in practice unreachable — the `noop`
    /// backend is a terminal sink).
    pub(crate) fn spawn(cfg: &Config) -> Self {
        static SHARED: OnceLock<Option<OverlayHandle>> = OnceLock::new();
        // MCP listen never drives the streaming-STT preview, so the
        // `Transcript` style has nothing to paint — fall back to an
        // audio visualisation (see `effective_overlay_style`).
        let style = effective_overlay_style(cfg);
        let handle = SHARED
            .get_or_init(|| match spawn_overlay(style) {
                Ok(h) => {
                    h.set_waveform_style(style);
                    h.set_volume_bar(cfg.overlay.volume_bar);
                    Some(h)
                }
                Err(e) => {
                    debug!(
                        target: "fono_mcp_server::voice_io",
                        error = %e,
                        "overlay spawn failed; continuing without visual indicator",
                    );
                    None
                }
            })
            .clone();
        // Re-apply per-call style/volume-bar configuration in case the
        // user edited config between listens. Cheap; both paths are
        // simple channel sends.
        if let Some(h) = handle.as_ref() {
            h.set_waveform_style(style);
            h.set_volume_bar(cfg.overlay.volume_bar);
        }
        Self { handle }
    }

    /// Cheap clone of the underlying handle. Returned `None` when the
    /// guard is a no-op so callers can short-circuit per-frame
    /// pushes.
    pub(crate) fn handle(&self) -> Option<OverlayHandle> {
        self.handle.clone()
    }

    pub(crate) fn set_state(&self, state: OverlayState) {
        if let Some(h) = self.handle.as_ref() {
            h.set_state(state);
        }
    }
}

impl Drop for OverlayGuard {
    fn drop(&mut self) {
        // The handle is process-shared (see `spawn` above); only flip
        // it to `Hidden`. Calling `shutdown` would destroy winit's
        // EventLoop and X11 won't let us create another one in the
        // same process, so every subsequent listen would fall through
        // to the noop backend. Hiding is cheap and reversible.
        if let Some(h) = self.handle.as_ref() {
            h.set_state(OverlayState::Hidden);
        }
    }
}

/// RAII guard that signals an MCP voice interaction span to the
/// running Fono daemon over IPC. On construction it fires a best-
/// effort [`Request::McpActivityStart`] carrying the active
/// [`McpPhase`]; on `Drop` it fires the matching
/// [`Request::McpActivityEnd`]. The daemon's depth counter handles
/// nested spans correctly — multiple guards on the same call stack
/// stack up cleanly.
///
/// **Best-effort.** If the daemon isn't reachable (no candidate
/// sockets, dead socket, malformed reply), construction debug-logs
/// the failure and produces a no-op guard. The voice loop must keep
/// working when the daemon is absent (e.g. `fono mcp serve` running
/// standalone from a developer shell).
///
/// **Drop semantics.** `Drop` cannot be async, so the End frame is
/// dispatched via `tokio::spawn` from within a `Handle::try_current`
/// guard. If the runtime has already shut down (rare — only happens
/// in test harnesses tearing down on panic) the End frame is silently
/// skipped; the daemon's depth counter will still re-balance when the
/// next Start lands on the 0→1 transition because the guard's
/// invariant ("Start before End") was upheld up to runtime tear-down.
///
/// Slice 7 of plan v7.
pub(crate) struct McpActivityGuard {
    candidates: Vec<std::path::PathBuf>,
}

impl McpActivityGuard {
    /// Create a guard for `phase`, dispatching a fire-and-forget
    /// `McpActivityStart` over IPC. Returns a no-op guard when
    /// `candidates` is empty.
    pub(crate) fn new(phase: McpPhase, candidates: &[std::path::PathBuf]) -> Self {
        let candidates_vec = candidates.to_vec();
        if !candidates_vec.is_empty() {
            let snapshot = candidates_vec.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    if let Err(e) =
                        fono_ipc::request_any(&snapshot, &Request::McpActivityStart { phase }).await
                    {
                        debug!(
                            target: "fono_mcp_server::voice_io",
                            error = %e,
                            ?phase,
                            "McpActivityStart ipc unreachable",
                        );
                    }
                });
            } else {
                debug!(
                    target: "fono_mcp_server::voice_io",
                    ?phase,
                    "McpActivityStart skipped: no tokio runtime available",
                );
            }
        }
        Self { candidates: candidates_vec }
    }
}

impl Drop for McpActivityGuard {
    fn drop(&mut self) {
        if self.candidates.is_empty() {
            return;
        }
        let snapshot = std::mem::take(&mut self.candidates);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Err(e) = fono_ipc::request_any(&snapshot, &Request::McpActivityEnd).await {
                    debug!(
                        target: "fono_mcp_server::voice_io",
                        error = %e,
                        "McpActivityEnd ipc unreachable",
                    );
                }
            });
        }
    }
}

/// Connection-scoped guard for an MCP listen/confirm/speak span.
///
/// Unlike [`McpActivityGuard`] (fire-and-forget `McpActivityStart` /
/// `McpActivityEnd`), this guard holds an IPC connection open for the
/// entire duration of the span. Two properties follow from that:
///
/// 1. **Tray is always restored** — if the client process is killed
///    mid-span (Ctrl-C or crash) *or exits immediately after the span*
///    (short-lived CLI entry points like `fono summarize`, where a
///    fire-and-forget End frame spawned during `Drop` would race
///    runtime shutdown and lose), kernel-level socket cleanup closes
///    the connection and the daemon automatically decrements its depth
///    counter and restores the previous tray state.
///
/// 2. **Escape cancels the listen** — when the user presses Escape, the
///    daemon writes `Response::McpListenCancelled` to the open
///    connection. A background read task sets `cancelled` to `true`;
///    `capture_pcm_once` checks this flag every 50 ms and exits early.
///    (Speaking spans don't poll the flag today.)
///
/// Construction is best-effort: if no daemon is reachable the guard
/// becomes a no-op and `cancelled` is always `false`.
pub(crate) struct McpActivityHoldGuard {
    /// Set to `true` when the daemon sends `Response::McpListenCancelled`
    /// (Escape key pressed while listening). `capture_pcm_once` polls
    /// this at 50 ms cadence.
    pub cancelled: Arc<AtomicBool>,
    /// Write half kept alive so that dropping the guard closes the
    /// connection, signalling EOF to the daemon which then decrements
    /// the activity depth and restores the tray.
    _write_half: Option<tokio::net::unix::OwnedWriteHalf>,
    /// Background task reading for `McpListenCancelled` signals from
    /// the daemon. Aborted on drop so the read half is cleaned up.
    _read_task: Option<tokio::task::JoinHandle<()>>,
}

impl McpActivityHoldGuard {
    /// Connect to the daemon, send `McpActivityHold`, wait for `Ok`,
    /// then hand the read half to a background watcher task. Returns a
    /// no-op guard when no daemon is reachable.
    pub(crate) async fn acquire(phase: McpPhase, candidates: &[std::path::PathBuf]) -> Self {
        let cancelled = Arc::new(AtomicBool::new(false));
        if candidates.is_empty() {
            return Self { cancelled, _write_half: None, _read_task: None };
        }
        let mut stream = match fono_ipc::connect_any(candidates).await {
            Ok(s) => s,
            Err(e) => {
                debug!(
                    target: "fono_mcp_server::voice_io",
                    error = %e,
                    "McpActivityHoldGuard: daemon unreachable; continuing without hold",
                );
                return Self { cancelled, _write_half: None, _read_task: None };
            }
        };
        if let Err(e) =
            fono_ipc::write_frame(&mut stream, &Request::McpActivityHold { phase }).await
        {
            debug!(
                target: "fono_mcp_server::voice_io",
                error = %e,
                "McpActivityHoldGuard: send failed",
            );
            return Self { cancelled, _write_half: None, _read_task: None };
        }
        match fono_ipc::read_frame::<Response, _>(&mut stream).await {
            Ok(Response::Ok) => {}
            other => {
                debug!(
                    target: "fono_mcp_server::voice_io",
                    response = ?other.as_ref().ok(),
                    "McpActivityHoldGuard: unexpected ack; continuing without hold",
                );
                return Self { cancelled, _write_half: None, _read_task: None };
            }
        }
        // Split: write half stays in the guard (its drop closes the
        // connection). Read half goes to the watcher task.
        let (mut read_half, write_half) = stream.into_split();
        let cancelled_clone = Arc::clone(&cancelled);
        let task = tokio::spawn(async move {
            loop {
                match fono_ipc::read_frame::<Response, _>(&mut read_half).await {
                    Ok(Response::McpListenCancelled) => {
                        cancelled_clone.store(true, Ordering::Release);
                        debug!(
                            target: "fono_mcp_server::voice_io",
                            "McpActivityHoldGuard: received cancel from daemon",
                        );
                        break;
                    }
                    Ok(_) => {}      // unexpected message — keep reading
                    Err(_) => break, // connection closed
                }
            }
        });
        Self { cancelled, _write_half: Some(write_half), _read_task: Some(task) }
    }
}

impl Drop for McpActivityHoldGuard {
    fn drop(&mut self) {
        if let Some(task) = self._read_task.take() {
            task.abort();
        }
        // Dropping _write_half sends FIN to the daemon → daemon EOF
        // handler decrements the activity depth and restores the tray.
    }
}

/// Cross-process serialisation guard for `fono.speak`. Each coding
/// agent (Claude Code, Forge, Cursor, …) spawns its own
/// `fono mcp serve` process, so concurrent `fono.speak` calls would
/// otherwise mix overlapping TTS audio on the shared output device.
/// This guard asks the running Fono daemon for the exclusive
/// "speak slot" mutex via `Request::McpSpeakAcquire` and holds the
/// IPC connection alive for the duration of playback. Dropping the
/// guard closes the connection; the daemon sees EOF and releases
/// the mutex so the next waiter can proceed.
///
/// **Best-effort.** If no daemon is reachable the guard is a no-op
/// — short-circuiting back to "no coordination, possible overlap"
/// matches the rest of the IPC story (see `McpActivityGuard`). The
/// user explicitly accepted this fallback when there's no daemon.
pub(crate) struct SpeakSlotGuard {
    // `Option` so we can mem-take on Drop without unsafe. While
    // `Some`, the daemon is holding the global mutex on our behalf;
    // dropping the inner `UnixStream` closes the socket which
    // triggers EOF on the daemon side → mutex released.
    _stream: Option<tokio::net::UnixStream>,
}

impl SpeakSlotGuard {
    /// Try to acquire the daemon's speak slot. Awaits until the slot
    /// is granted (i.e. blocks the calling task while another MCP
    /// server is currently speaking). Returns a no-op guard when the
    /// daemon isn't reachable.
    pub(crate) async fn acquire(candidates: &[std::path::PathBuf]) -> Self {
        if candidates.is_empty() {
            return Self { _stream: None };
        }
        let mut stream = match fono_ipc::connect_any(candidates).await {
            Ok(s) => s,
            Err(e) => {
                debug!(
                    target: "fono_mcp_server::voice_io",
                    error = %e,
                    "speak-slot acquire skipped: daemon unreachable",
                );
                return Self { _stream: None };
            }
        };
        if let Err(e) = fono_ipc::write_frame(&mut stream, &Request::McpSpeakAcquire).await {
            debug!(
                target: "fono_mcp_server::voice_io",
                error = %e,
                "speak-slot acquire send failed",
            );
            return Self { _stream: None };
        }
        match fono_ipc::read_frame::<Response, _>(&mut stream).await {
            Ok(Response::Ok) => Self { _stream: Some(stream) },
            Ok(other) => {
                debug!(
                    target: "fono_mcp_server::voice_io",
                    ?other,
                    "speak-slot acquire received unexpected reply",
                );
                Self { _stream: None }
            }
            Err(e) => {
                debug!(
                    target: "fono_mcp_server::voice_io",
                    error = %e,
                    "speak-slot acquire read failed",
                );
                Self { _stream: None }
            }
        }
    }
}

/// Build the active TTS backend's voice palette (curated, gender-tagged
/// voices). Cloud backends prefer a locally cached autodiscovered palette
/// (when `[tts].voice_discovery` is on and a cache exists), falling back to
/// the curated catalogue list; the local backend derives its palette from
/// the on-device voice catalog for the user's configured languages. Returns
/// an empty palette when TTS is disabled or the backend has no voices — the
/// resolver then falls back to the backend default voice.
#[must_use]
pub fn active_palette(cfg: &Config) -> fono_core::voice_palette::Palette {
    active_palette_in(cfg, discovered_cache_dir().as_deref())
}

/// Resolve the discovered-voices cache root (`<cache_dir>`), or `None` when
/// XDG paths can't be resolved. Kept best-effort so palette resolution never
/// fails on a path error.
fn discovered_cache_dir() -> Option<std::path::PathBuf> {
    fono_core::paths::Paths::resolve().ok().map(|p| p.cache_dir)
}

/// [`active_palette`] with an explicit discovered-voices cache root (or
/// `None` to skip the cache). Exposed for tests so the cache-preference and
/// fallback behaviour can be exercised against a temp dir.
#[must_use]
pub fn active_palette_in(
    cfg: &Config,
    cache_dir: Option<&Path>,
) -> fono_core::voice_palette::Palette {
    use fono_core::config::TtsBackend;
    match cfg.tts.backend {
        TtsBackend::None => fono_core::voice_palette::Palette::default(),
        TtsBackend::Local => local_palette(cfg),
        ref other => {
            let id = fono_core::providers::tts_backend_str(other);
            cloud_palette(cfg, id, cache_dir)
        }
    }
}

/// Cloud-backend palette: a fresh discovered cache (when enabled and present)
/// wins, otherwise the curated catalogue palette. A missing/empty/corrupt
/// cache transparently degrades to the curated list.
fn cloud_palette(
    cfg: &Config,
    id: &str,
    cache_dir: Option<&Path>,
) -> fono_core::voice_palette::Palette {
    if cfg.tts.voice_discovery {
        if let Some(dir) = cache_dir {
            if let Some(cached) = fono_core::voice_discovery::DiscoveredVoices::load(dir, id) {
                let palette = cached.to_palette();
                if !palette.is_empty() {
                    return palette;
                }
            }
        }
    }
    fono_core::provider_catalog::tts_palette(id)
}

#[cfg(feature = "tts-local")]
fn local_palette(cfg: &Config) -> fono_core::voice_palette::Palette {
    // Normalise configured languages to base codes (the catalog keys on
    // `en`, `ro`, …) before asking for the gendered local palette.
    let langs: Vec<String> =
        cfg.general.languages.iter().map(|l| fono_tts::local_router::base_lang(l)).collect();
    let refs: Vec<&str> = langs.iter().map(String::as_str).collect();
    fono_tts::voices::local_palette(&refs).unwrap_or_default()
}

#[cfg(not(feature = "tts-local"))]
fn local_palette(_cfg: &Config) -> fono_core::voice_palette::Palette {
    fono_core::voice_palette::Palette::default()
}

/// Resolve which backend voice a given program should speak with, per
/// the shared precedence (explicit per-call voice → manual `[mcp.voices]`
/// pin → stable automatic assignment → backend default). `program` is
/// the normalised caller identity (MCP `clientInfo.name`, or
/// `source_app` for `fono.summarize`); `explicit` is the per-call
/// `voice` argument. Returns `None` to use the backend default voice.
#[must_use]
pub fn resolve_program_voice(
    cfg: &Config,
    program: Option<&str>,
    explicit: Option<&str>,
) -> Option<String> {
    let palette = active_palette(cfg);
    fono_core::voice_resolver::resolve_voice(&fono_core::voice_resolver::VoiceQuery {
        palette: &palette,
        program: program.map(str::trim).filter(|s| !s.is_empty()),
        explicit: explicit.map(str::trim).filter(|s| !s.is_empty()),
        pins: &cfg.mcp.voices,
        voice_gender: &cfg.mcp.voice_gender,
        auto_assign: cfg.mcp.auto_assign_voices,
    })
}

/// Wall-clock breakdown of a single [`speak_text`] call, in
/// milliseconds. Surfaced so callers can log how long synthesis vs
/// playback took without re-instrumenting the path. Both are
/// monotonic-clock measurements; `synth_ms` covers the TTS backend
/// round-trip, `playback_ms` covers draining the audio through the
/// output device.
#[derive(Debug, Clone, Copy, Default)]
pub struct SpeakTimings {
    /// Milliseconds spent in `tts.synthesize` (cloud round-trip or
    /// local inference).
    pub synth_ms: u64,
    /// Milliseconds spent draining the synthesised audio through the
    /// playback queue (0 when the synthesised PCM was empty).
    pub playback_ms: u64,
}

/// Synthesise `text` through the configured TTS backend and block until
/// playback drains (or `120 s` elapses). Returns an error string suitable
/// for `ToolCallResult::failure` when anything goes wrong. On success
/// returns a [`SpeakTimings`] breakdown for logging.
///
/// `voice` is an optional, already-resolved backend-specific voice id
/// (see [`resolve_program_voice`]). `None` uses the backend default.
/// `daemon_ipc_candidates` enables the tray-feedback channel: if any
/// of the listed sockets accepts an IPC connection, the daemon flips
/// its tray to amber for the duration of playback. The guard is
/// gated on synthesised audio length ≥ 1 s so trivially short
/// prompts (e.g. "yes?") don't flash the tray. Slice 7 of plan v7.
pub async fn speak_text(
    cfg: &Config,
    secrets: &Secrets,
    text: &str,
    voice: Option<&str>,
    daemon_ipc_candidates: &[std::path::PathBuf],
) -> Result<SpeakTimings> {
    let voices_dir = fono_core::Paths::resolve().map(|p| p.voices_dir()).unwrap_or_default();
    let tts = fono_tts::build_tts(&cfg.tts, secrets, &cfg.general.languages, &voices_dir)
        .context("TTS build failed")?
        .ok_or_else(|| {
            anyhow!(
                "TTS backend is disabled. Run `fono use tts <backend>` to enable TTS before \
                 using this tool."
            )
        })?;

    let device =
        if cfg.tts.output_device.is_empty() { None } else { Some(cfg.tts.output_device.as_str()) };
    let playback = AudioPlayback::new(device).context("audio device open failed")?;

    // Streaming-capable cloud backends: pull PCM chunks and play them gaplessly
    // as they arrive, cutting time-to-first-audio. Local and batch backends fall
    // through to the synthesize + enqueue path below. We can't size the utterance
    // up front here (the audio arrives incrementally), so the amber activity flash
    // is taken unconditionally rather than gated on a >= 1 s length.
    if tts.supports_streaming() {
        let _activity_guard =
            McpActivityHoldGuard::acquire(McpPhase::Speaking, daemon_ipc_candidates).await;
        let _slot_guard = SpeakSlotGuard::acquire(daemon_ipc_candidates).await;
        let synth_started = Instant::now();
        let mut sink = fono_audio::LocalPlaybackSink::new(playback.clone());
        let produced =
            fono_tts::stream_utterance(tts.as_ref(), text, voice, None, &mut sink, || {})
                .await
                .context("streaming TTS failed")?;
        // Synth and playback overlap when streaming, so `synth_ms` here measures
        // the whole stream-and-push span rather than an isolated synth round-trip.
        let synth_ms = synth_started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let playback_started = Instant::now();
        if produced {
            let deadline = Instant::now() + Duration::from_secs(120);
            while !playback.is_idle() {
                if Instant::now() >= deadline {
                    warn!(target: "fono_mcp_server::voice_io", "playback drain timeout");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        let playback_ms = if produced {
            playback_started.elapsed().as_millis().min(u64::MAX as u128) as u64
        } else {
            0
        };
        return Ok(SpeakTimings { synth_ms, playback_ms });
    }

    let synth_started = Instant::now();
    let audio = tts.synthesize(text, voice, None).await.context("TTS synthesis failed")?;
    let synth_ms = synth_started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    // Gate the tray guard on audio length: short prompts (< 1 s) skip
    // the amber flash to avoid flicker. The guard lives across the
    // playback drain loop and is dropped when this function returns.
    //
    // Connection-scoped (`McpActivityHold`) rather than fire-and-forget
    // Start/End frames: short-lived CLI callers (`fono summarize`) exit
    // right after this function returns, and an End frame spawned from
    // `Drop` would race runtime shutdown and lose — leaving the daemon
    // tray stuck amber. Closing the socket is handled by the kernel
    // even on abrupt exit, so the daemon always restores the tray.
    let audio_secs =
        if audio.sample_rate > 0 { audio.pcm.len() as f64 / audio.sample_rate as f64 } else { 0.0 };
    let _activity_guard = if audio_secs >= 1.0 {
        Some(McpActivityHoldGuard::acquire(McpPhase::Speaking, daemon_ipc_candidates).await)
    } else {
        None
    };
    // Serialise audio output across concurrent `fono mcp serve`
    // processes by asking the daemon for the global speak slot. The
    // guard awaits if another agent is already speaking. Dropped at
    // function return → daemon releases the mutex. Best-effort: when
    // the daemon isn't reachable we accept overlapping playback (the
    // user has signed off on this fallback).
    let _slot_guard = SpeakSlotGuard::acquire(daemon_ipc_candidates).await;
    let playback_started = Instant::now();
    let had_audio = !audio.pcm.is_empty();
    if had_audio {
        playback.enqueue(audio.pcm, audio.sample_rate).context("playback enqueue failed")?;
    }

    let deadline = Instant::now() + Duration::from_secs(120);
    while !playback.is_idle() {
        if Instant::now() >= deadline {
            warn!(target: "fono_mcp_server::voice_io", "playback drain timeout");
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let playback_ms = if had_audio {
        playback_started.elapsed().as_millis().min(u64::MAX as u128) as u64
    } else {
        0
    };
    Ok(SpeakTimings { synth_ms, playback_ms })
}

/// Outcome of a single `listen_once` call.
#[derive(Debug, Clone)]
pub struct ListenOutcome {
    /// Trimmed transcript produced by the configured STT backend. May be
    /// empty (e.g. user stayed silent the whole time).
    pub transcript: String,
    /// Wall-clock duration of the capture in milliseconds (capture only,
    /// not STT inference). Cumulative across rejected utterances in the
    /// multi-utterance loop (Slice 3 of plan v7).
    pub duration_ms: u64,
    /// Why the capture loop ended.
    pub reason: ListenStopReason,
    /// Number of utterances the relevance filter discarded before the
    /// returned one was accepted (or the loop bailed out on its
    /// rejection / wall-clock guard). `0` for a clean single-shot
    /// answer; surfaces in the `fono.listen` tool reply as
    /// `rejected_count` so coding agents can spot pathological
    /// environments.
    pub rejected_count: u32,
    /// Cumulative milliseconds spent capturing audio (microphone open
    /// → silence-commit / timeout) across every utterance in the
    /// multi-utterance loop. Logging only; not surfaced to agents.
    pub capture_ms: u64,
    /// Cumulative milliseconds spent in `stt.transcribe` across every
    /// utterance. Isolates STT latency from the time the user spent
    /// speaking or thinking.
    pub stt_ms: u64,
    /// Cumulative milliseconds spent in the relevance filter
    /// (heuristic + optional LLM classifier). `0` when the filter is
    /// disabled.
    pub relevance_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenStopReason {
    /// `SilenceWatch::Committed` fired — the user paused long enough to
    /// signal end-of-utterance.
    Silence,
    /// `max_seconds` elapsed (or the hard ceiling did).
    Timeout,
    /// The daemon sent `Response::McpListenCancelled` (Escape key
    /// pressed) so the listen was aborted early.
    Cancelled,
}

/// Open the configured input device and run the multi-utterance
/// listen loop until the relevance filter accepts an answer, the
/// rejection ceiling is hit, or the cumulative wall-clock budget
/// exceeds `max_seconds × 1.5`.
///
/// Each iteration captures PCM until either the caller's
/// `max_seconds` per-utterance budget elapses or the silence-watch
/// state machine commits; the buffered audio is transcribed; the
/// relevance heuristic (Slice 3 of plan v7) decides whether to
/// return it or drop it and re-arm.
///
/// `prompt` is the agent-supplied prompt text (used only for the
/// echo check); `context` is reserved for the LLM classifier added
/// in Slice 4. The overlay handle is held across iterations so the
/// panel stays visible while we wait for a real answer.
///
/// This function spends most of its time in the asynchronous polling
/// loop; the capture thread itself is owned by the platform backend
/// (`pw-cat` / `parec` / `cpal`). Tearing down the per-iteration
/// `CaptureHandle` joins that thread, so on return the microphone is
/// no longer hot.
#[allow(clippy::too_many_arguments)]
pub async fn listen_once(
    cfg: &Config,
    secrets: &Secrets,
    whisper_models_dir: &Path,
    max_seconds: u32,
    prompt: Option<&str>,
    context: Option<&str>,
    classifier: Option<Arc<dyn TextFormatter>>,
    daemon_ipc_candidates: &[std::path::PathBuf],
) -> Result<ListenOutcome> {
    // Build STT up-front so the user gets an immediate, clear error if
    // their config is broken rather than after recording an utterance
    // they will lose.
    let stt = fono_stt::build_stt(&cfg.stt, &cfg.general, secrets, whisper_models_dir)
        .context("STT build failed")?;
    // Best-effort prewarm — fail open.
    if let Err(e) = stt.prewarm().await {
        debug!(target: "fono_mcp_server::voice_io", error = %e, "STT prewarm failed (continuing)");
    }

    // Tray feedback: flip the daemon's tray icon to amber for the
    // duration of the listen span (across all utterances in the
    // multi-utterance loop). The persistent `McpActivityHoldGuard`
    // ensures the tray is always restored even when the MCP server
    // is killed (Ctrl-C) — the kernel closes the IPC connection and
    // the daemon's EOF handler decrements the depth counter. The guard
    // also carries the `cancelled` flag that flips to `true` when the
    // daemon forwards a `Response::McpListenCancelled` (Escape key).
    // Slice 7 of plan v7; upgraded to hold-based in the cancel-fix pass.
    let hold_guard =
        McpActivityHoldGuard::acquire(McpPhase::Listening, daemon_ipc_candidates).await;
    let cancelled = Arc::clone(&hold_guard.cancelled);

    // Visual feedback for the duration of the (potentially multi-
    // utterance) listen loop. RAII-managed: dropped at the end of
    // this function (or on panic / early return) which flips the
    // panel to `Hidden`. Spawned only while the microphone is open —
    // Slice 1 of plan v7 deliberately keeps the prompt-TTS phase
    // silent overlay-wise.
    //
    // The Slice 6 "skip when a daemon is detected" branch has been
    // removed: the daemon only renders its overlay during F7
    // dictation / F8 assistant turns, so deferring to it during an
    // MCP listen left the user with no panel at all. Two overlapping
    // panels are a worse-case-theoretical concern; the practical
    // case is zero panels, which is what the probe produced. A
    // proper fix would be a new IPC channel asking the daemon to
    // paint on the MCP server's behalf — deferred until the
    // double-paint actually shows up as a complaint in practice.
    let overlay = OverlayGuard::spawn(cfg);
    overlay.set_state(OverlayState::Recording { db: 0 });

    let max_rejections = cfg.mcp.relevance_max_rejections;
    let filter_enabled = cfg.mcp.relevance_filter.as_str() != "off";
    // Cumulative wall-clock ceiling across iterations. `max_seconds
    // × 1.5` matches the spec in Slice 3 of plan v7 — gives the
    // loop one full retry budget on top of a single-shot capture.
    let wall_clock_budget = Duration::from_secs((max_seconds.max(1) as u64 * 3).div_ceil(2));
    let loop_started = Instant::now();

    let mut rejected_count: u32 = 0;
    let mut last_outcome: Option<(String, ListenStopReason, u64)> = None;
    // Cumulative per-step timings across the multi-utterance loop,
    // surfaced on the `fono.listen` / `fono.confirm` completion log line
    // so a slow turn can be attributed to capture, STT, or the filter.
    let mut capture_ms_acc: u64 = 0;
    let mut stt_ms_acc: u64 = 0;
    let mut relevance_ms_acc: u64 = 0;

    loop {
        // Bound the per-utterance capture by both the caller's
        // `max_seconds` and the wall-clock budget still remaining.
        let elapsed = loop_started.elapsed();
        let remaining = wall_clock_budget.saturating_sub(elapsed);
        if remaining.is_zero() {
            // Out of budget — return the most recent outcome we have,
            // or an empty one if every iteration so far rejected.
            let (transcript, reason, ms) = last_outcome.unwrap_or_else(|| {
                (
                    String::new(),
                    ListenStopReason::Timeout,
                    elapsed.as_millis().min(u64::MAX as u128) as u64,
                )
            });
            return Ok(ListenOutcome {
                transcript,
                duration_ms: ms,
                reason,
                rejected_count,
                capture_ms: capture_ms_acc,
                stt_ms: stt_ms_acc,
                relevance_ms: relevance_ms_acc,
            });
        }
        let per_iter_secs = max_seconds.max(1).min(remaining.as_secs().max(1) as u32);

        let (pcm, reason, iter_ms) =
            capture_pcm_once(cfg, &overlay, per_iter_secs, &cancelled).await?;
        capture_ms_acc = capture_ms_acc.saturating_add(iter_ms);

        // Propagate cancellation immediately — don't transcribe or filter,
        // just surface the early exit to the caller.
        if matches!(reason, ListenStopReason::Cancelled) {
            let total_ms = loop_started.elapsed().as_millis().min(u64::MAX as u128) as u64;
            return Ok(ListenOutcome {
                transcript: String::new(),
                duration_ms: total_ms,
                reason,
                rejected_count,
                capture_ms: capture_ms_acc,
                stt_ms: stt_ms_acc,
                relevance_ms: relevance_ms_acc,
            });
        }

        let stt_started = Instant::now();
        let transcript = if pcm.is_empty() {
            String::new()
        } else {
            match stt.transcribe(&pcm, 16_000, None).await {
                Ok(t) => t.text.trim().to_string(),
                Err(e) => {
                    warn!(target: "fono_mcp_server::voice_io", error = %e, "STT transcribe failed");
                    return Err(e.context("STT transcribe failed"));
                }
            }
        };
        stt_ms_acc = stt_ms_acc
            .saturating_add(stt_started.elapsed().as_millis().min(u64::MAX as u128) as u64);

        // Total wall-clock so far (capture + STT for this iteration
        // plus everything that came before). Reported back to the
        // agent in `duration_ms`.
        let total_ms = loop_started.elapsed().as_millis().min(u64::MAX as u128) as u64;

        if !filter_enabled {
            return Ok(ListenOutcome {
                transcript,
                duration_ms: total_ms,
                reason,
                rejected_count,
                capture_ms: capture_ms_acc,
                stt_ms: stt_ms_acc,
                relevance_ms: relevance_ms_acc,
            });
        }

        // Empty transcript + timeout = the user genuinely stayed
        // silent. Don't penalise that by looping forever; return
        // immediately so the agent can decide what to do.
        if transcript.is_empty() && matches!(reason, ListenStopReason::Timeout) {
            return Ok(ListenOutcome {
                transcript,
                duration_ms: total_ms,
                reason,
                rejected_count,
                capture_ms: capture_ms_acc,
                stt_ms: stt_ms_acc,
                relevance_ms: relevance_ms_acc,
            });
        }

        let relevance_started = Instant::now();
        let heuristic_verdict = crate::relevance::evaluate_heuristic(&transcript, prompt);
        let verdict = match heuristic_verdict {
            // Heuristic accepted — escalate to the LLM classifier when
            // the user has opted into `relevance_filter = "llm"` AND a
            // polish-backed classifier is wired in. Anything else
            // short-circuits to Accept.
            crate::relevance::RelevanceVerdict::Accept => {
                if cfg.mcp.relevance_filter.as_str() == "llm" {
                    if let Some(c) = classifier.as_ref() {
                        let ctx_text = context.unwrap_or("");
                        crate::relevance::evaluate_llm(c.as_ref(), &transcript, ctx_text).await
                    } else {
                        crate::relevance::RelevanceVerdict::Accept
                    }
                } else {
                    crate::relevance::RelevanceVerdict::Accept
                }
            }
            v => v,
        };
        relevance_ms_acc = relevance_ms_acc
            .saturating_add(relevance_started.elapsed().as_millis().min(u64::MAX as u128) as u64);
        match verdict {
            crate::relevance::RelevanceVerdict::Accept => {
                return Ok(ListenOutcome {
                    transcript,
                    duration_ms: total_ms,
                    reason,
                    rejected_count,
                    capture_ms: capture_ms_acc,
                    stt_ms: stt_ms_acc,
                    relevance_ms: relevance_ms_acc,
                });
            }
            crate::relevance::RelevanceVerdict::Reject(why) => {
                debug!(
                    target: "fono_mcp_server::voice_io",
                    rejected_count = rejected_count + 1,
                    reason = ?why,
                    transcript = %transcript,
                    "relevance filter dropped utterance",
                );
                rejected_count += 1;
                last_outcome = Some((transcript, reason, total_ms));
                if rejected_count > max_rejections {
                    // Ceiling reached — return the most recent
                    // utterance even though it didn't pass the
                    // filter so the agent isn't stranded.
                    let (transcript, reason, ms) = last_outcome.unwrap();
                    return Ok(ListenOutcome {
                        transcript,
                        duration_ms: ms,
                        reason,
                        rejected_count,
                        capture_ms: capture_ms_acc,
                        stt_ms: stt_ms_acc,
                        relevance_ms: relevance_ms_acc,
                    });
                }
                // Flash the `Ignoring` overlay state for ~700 ms so
                // the user gets a discriminable visual ack ("Fono
                // heard you but is still waiting for a real answer")
                // before the next iteration re-arms the panel into
                // `Recording`. Slice 5 of plan v7.
                overlay.set_state(OverlayState::Ignoring { reason: map_ignore_reason(why) });
                tokio::time::sleep(Duration::from_millis(700)).await;
                overlay.set_state(OverlayState::Recording { db: 0 });
            }
        }
    }
}

/// Capture a single utterance: open the input device, accumulate PCM
/// until silence-commit, the per-iteration budget, or a cancellation
/// from the daemon, return the buffered samples. The overlay (held
/// across iterations by `listen_once`) is driven via the
/// `forwarder`-side cb plus the 50 ms polling loop.
///
/// `cancelled` is the [`McpActivityHoldGuard::cancelled`] flag; the
/// polling loop checks it every 50 ms. When `true` the function
/// returns early with [`ListenStopReason::Cancelled`] so the caller
/// can abort without transcribing the partial buffer.
async fn capture_pcm_once(
    cfg: &Config,
    overlay: &OverlayGuard,
    max_seconds: u32,
    cancelled: &Arc<AtomicBool>,
) -> Result<(Vec<f32>, ListenStopReason, u64)> {
    let capture = AudioCapture::new(CaptureConfig::default());
    let buffer = Arc::new(Mutex::new(RecordingBuffer::default()));
    let envelope = Arc::new(Mutex::new(EnvelopeFollower::new(EnvelopeConfig::default())));
    let effective_silence_ms = resolve_auto_stop_ms(cfg);
    let watch_cfg = SilenceWatchConfig {
        auto_stop_silence_ms: Some(effective_silence_ms),
        ..Default::default()
    };
    let pondering_visual_ms = watch_cfg.pondering_visual_ms;
    let watch = Arc::new(Mutex::new(SilenceWatch::new(watch_cfg)));
    let committed = Arc::new(Mutex::new(false));
    let pondering = Arc::new(Mutex::new(false));

    // Make sure the overlay is showing the live "Recording" label —
    // a previous iteration may have parked it in `Pondering` before
    // the silence commit.
    overlay.set_state(OverlayState::Recording { db: 0 });
    let overlay_for_cb = overlay.handle();

    let cap_samples = (LISTEN_HARD_CEILING_SECS as usize) * 16_000;

    let buffer_cb = Arc::clone(&buffer);
    let envelope_cb = Arc::clone(&envelope);
    let watch_cb = Arc::clone(&watch);
    let committed_cb = Arc::clone(&committed);
    let pondering_cb = Arc::clone(&pondering);

    let mut metrics_tick: u8 = 0;

    let started = Instant::now();
    let handle = capture
        .start_with_forwarder(move |pcm: &[f32]| {
            if let Ok(mut b) = buffer_cb.lock() {
                b.push_slice(pcm, cap_samples);
            }
            // Raw PCM is *not* pushed straight to the overlay here
            // — the visualizer task spawned below reads the shared
            // `buffer` on a 50 ms tick and emits style-appropriate
            // frames (gained samples for Oscilloscope, FFT bins for
            // Fft/Heatmap, level only for Bars). Pushing raw PCM from
            // the capture callback would race the task and bypass
            // the gain/FFT path the renderer expects.
            if let (Ok(mut env), Ok(mut sw), Ok(mut done), Ok(mut pnd)) =
                (envelope_cb.lock(), watch_cb.lock(), committed_cb.lock(), pondering_cb.lock())
            {
                if *done {
                    return;
                }
                for chunk in pcm.chunks(ENVELOPE_FRAME_SAMPLES) {
                    env.push_frame(chunk);
                    let frame_ms = (chunk.len() as f32 * 1000.0) / 16_000.0;
                    let snap = env.snapshot();
                    metrics_tick = metrics_tick.wrapping_add(1);
                    if metrics_tick % 5 == 0 {
                        if let Some(o) = overlay_for_cb.as_ref() {
                            let silence_rms = snap.voiced_rms * SILENCE_GAIN;
                            o.push_gate_metrics(snap.inst_rms, snap.voiced_rms, silence_rms);
                        }
                    }
                    match sw.push(snap, frame_ms) {
                        SilenceEvent::EnteredPondering => {
                            *pnd = true;
                            if let Some(o) = overlay_for_cb.as_ref() {
                                o.set_state(OverlayState::Pondering { db: 0, walk_progress: 0 });
                            }
                        }
                        SilenceEvent::ResumedFromPondering => {
                            *pnd = false;
                            if let Some(o) = overlay_for_cb.as_ref() {
                                o.set_state(OverlayState::Recording { db: 0 });
                            }
                        }
                        SilenceEvent::Committed => {
                            *done = true;
                            break;
                        }
                        _ => {}
                    }
                }
            }
        })
        .context("audio capture start failed")?;

    // Spawn the style-aware visualization ticker. Mirrors the
    // daemon's `spawn_waveform_level_task` (`crates/fono/src/session.rs:778-940`).
    // Duplicated here to avoid coupling `fono-overlay` to `fono-audio`;
    // follow-up to extract into a shared helper (likely a new
    // `fono-overlay-driver` crate, or a feature-gated module on
    // `fono-overlay` that depends on `fono-audio`).
    let visualizer_handle =
        spawn_visualizer_task(cfg, overlay.handle(), Arc::clone(&buffer), 16_000);

    let bounded_secs = max_seconds.max(1) as u64;
    let bounded_secs = bounded_secs.min(LISTEN_HARD_CEILING_SECS);
    let deadline = started + Duration::from_secs(bounded_secs);
    let mut last_walk_progress: u16 = 0;

    let reason = loop {
        if *committed.lock().expect("silence-commit mutex poisoned") {
            break ListenStopReason::Silence;
        }
        if Instant::now() >= deadline {
            break ListenStopReason::Timeout;
        }
        // Daemon signalled Escape → abort the listen immediately.
        if cancelled.load(Ordering::Acquire) {
            break ListenStopReason::Cancelled;
        }
        let in_pondering =
            pondering.lock().map(|g| *g).unwrap_or(false) && overlay.handle().is_some();
        if in_pondering {
            let elapsed_ms = watch.lock().map(|g| g.pondering_elapsed_ms()).unwrap_or(0.0);
            let progress =
                compute_walk_progress(elapsed_ms, effective_silence_ms, pondering_visual_ms);
            if progress.abs_diff(last_walk_progress) >= 100 || progress == 10_000 {
                last_walk_progress = progress;
                overlay.set_state(OverlayState::Pondering { db: 0, walk_progress: progress });
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    let duration_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    // Stop the visualization ticker before dropping the capture
    // handle so it doesn't keep pumping frames to a torn-down
    // overlay between iterations.
    if let Some(h) = visualizer_handle {
        h.abort();
    }
    // Dropping the handle joins the capture thread.
    drop(handle);

    let pcm: Vec<f32> = {
        let b = buffer.lock().expect("recording buffer mutex poisoned");
        b.samples().to_vec()
    };

    Ok((pcm, reason, duration_ms))
}

/// Spawn the style-aware visualization ticker that feeds the overlay
/// with `push_level` / `push_samples` / `push_fft_bins` frames at
/// 50 ms cadence. Returns `None` when the user disabled the
/// waveform (`[overlay].waveform = false`) or when there's no
/// overlay handle to drive. Caller aborts the returned handle at
/// the end of the capture iteration to avoid stale frames painting
/// onto a torn-down overlay.
///
/// Mirrors `crates/fono/src/session.rs:778-940`. The two paths
/// share the same constants (`WAVEFORM_*`), the same renderer
/// methods (`push_level`, `push_samples`, `push_fft_bins`), and
/// the same 50 ms tick cadence, so the F7 dictation and
/// `fono.listen` overlays paint identically for a given
/// `[overlay].style`. Live-preview / Transcript is intentionally
/// ignored here: the MCP listen path always uses one of the
/// audio-driven styles (`Bars` / `Oscilloscope` / `Fft` / `Heatmap`)
/// and falls through to a no-op task for `Transcript`.
fn spawn_visualizer_task(
    cfg: &Config,
    handle: Option<OverlayHandle>,
    buffer: Arc<Mutex<RecordingBuffer>>,
    sample_rate: u32,
) -> Option<tokio::task::AbortHandle> {
    if !cfg.overlay.waveform {
        return None;
    }
    let o = handle?;
    // MCP listen has no streaming transcript to show, so a configured
    // `Transcript` style degrades to the default audio visualisation
    // rather than a static empty panel (see `effective_overlay_style`).
    let style = effective_overlay_style(cfg);
    let buf = buffer;
    let task = tokio::spawn(async move {
        match style {
            fono_core::config::WaveformStyle::Oscilloscope => {
                let snap_len = (sample_rate as usize / 1000) * 50;
                let gain = 1.0 / WAVEFORM_AMPLITUDE_CEILING;
                let mut tick = tokio::time::interval(Duration::from_millis(50));
                loop {
                    tick.tick().await;
                    let snap = buf
                        .lock()
                        .map(|b| {
                            let s = b.samples();
                            s[s.len().saturating_sub(snap_len)..]
                                .iter()
                                .map(|v| v * gain)
                                .collect::<Vec<f32>>()
                        })
                        .unwrap_or_default();
                    if !snap.is_empty() {
                        let inv_gain = 1.0 / gain;
                        let rms = {
                            let sum_sq: f32 = snap.iter().map(|v| (v * inv_gain).powi(2)).sum();
                            (sum_sq / snap.len() as f32).sqrt()
                        };
                        o.push_level((rms / WAVEFORM_AMPLITUDE_CEILING).clamp(0.0, 1.0));
                        o.push_samples(snap);
                    }
                }
            }
            fono_core::config::WaveformStyle::Fft
            | fono_core::config::WaveformStyle::Heatmap
            | fono_core::config::WaveformStyle::Terrain3d
            | fono_core::config::WaveformStyle::System360 => {
                let mut planner = realfft::RealFftPlanner::<f32>::new();
                let r2c = planner.plan_fft_forward(WAVEFORM_FFT_SIZE);
                let mut input_buf = r2c.make_input_vec();
                let mut output_buf = r2c.make_output_vec();
                let window: Vec<f32> = (0..WAVEFORM_FFT_SIZE)
                    .map(|i| {
                        let phase = std::f32::consts::PI * 2.0 * (i as f32)
                            / (WAVEFORM_FFT_SIZE as f32 - 1.0);
                        0.5 - 0.5 * phase.cos()
                    })
                    .collect();
                let max_hz = match style {
                    fono_core::config::WaveformStyle::Heatmap
                    | fono_core::config::WaveformStyle::Terrain3d
                    | fono_core::config::WaveformStyle::System360 => WAVEFORM_FFT_MAX_HZ_WIDE,
                    _ => WAVEFORM_FFT_MAX_HZ,
                };
                let max_source_bin =
                    ((max_hz * WAVEFORM_FFT_SIZE as f32) / sample_rate as f32) as usize;
                let display_bins = WAVEFORM_FFT_BINS.max(1);
                let db_span = WAVEFORM_FFT_DB_CEILING - WAVEFORM_FFT_DB_FLOOR;
                let mut tick = tokio::time::interval(Duration::from_millis(50));
                loop {
                    tick.tick().await;
                    let filled = buf
                        .lock()
                        .map(|b| {
                            let s = b.samples();
                            let take = s.len().min(WAVEFORM_FFT_SIZE);
                            let head = WAVEFORM_FFT_SIZE - take;
                            for v in &mut input_buf[..head] {
                                *v = 0.0;
                            }
                            let tail = &s[s.len() - take..];
                            for (i, v) in tail.iter().enumerate() {
                                input_buf[head + i] = *v * window[head + i];
                            }
                            take
                        })
                        .unwrap_or(0);
                    if filled == 0 {
                        continue;
                    }
                    if r2c.process(&mut input_buf, &mut output_buf).is_err() {
                        continue;
                    }
                    let mut bins = vec![0.0_f32; display_bins];
                    for (display_i, slot) in bins.iter_mut().enumerate() {
                        let start = (display_i * max_source_bin) / display_bins;
                        let end_raw = ((display_i + 1) * max_source_bin) / display_bins;
                        let end = end_raw.max(start + 1).min(max_source_bin);
                        let mut sum = 0.0_f32;
                        for c in &output_buf[start..end] {
                            sum += c.re.hypot(c.im);
                        }
                        let mag = sum / (end - start) as f32;
                        let db = 20.0 * mag.max(1e-6).log10();
                        *slot = ((db - WAVEFORM_FFT_DB_FLOOR) / db_span).clamp(0.0, 1.0);
                    }
                    let win_len = (sample_rate as usize / 1000) * 50;
                    let level = buf
                        .lock()
                        .map(|b| {
                            let s = b.samples();
                            normalised_rms(&s[s.len().saturating_sub(win_len)..])
                        })
                        .unwrap_or(0.0);
                    o.push_level(level);
                    o.push_fft_bins(bins);
                }
            }
            fono_core::config::WaveformStyle::Bars => {
                let win_len = (sample_rate as usize / 1000) * 50;
                let mut tick = tokio::time::interval(Duration::from_millis(50));
                loop {
                    tick.tick().await;
                    let level = buf
                        .lock()
                        .map(|b| {
                            let s = b.samples();
                            normalised_rms(&s[s.len().saturating_sub(win_len)..])
                        })
                        .unwrap_or(0.0);
                    o.push_level(level);
                }
            }
            // Transcript renders the streaming-preview text panel,
            // not an audio visualisation. MCP listen doesn't drive
            // the streaming-STT pipeline, so this style degrades to
            // a no-op task. The match must remain exhaustive.
            fono_core::config::WaveformStyle::Transcript => {}
        }
    });
    Some(task.abort_handle())
}

/// Resolve the overlay waveform style to actually drive during an MCP
/// listen. The `Transcript` style renders the streaming live-preview
/// text panel, but `fono.listen` never runs the streaming-STT
/// pipeline — so a literal `Transcript` would leave the user staring
/// at a static, empty panel with no animation. Degrade it to the
/// default audio visualisation ([`WaveformStyle::default`], currently
/// `Fft`), mirroring the graceful fallback F7 dictation applies for
/// non-streaming STT backends (commit `7bdbdd6`). Every other style
/// is audio-driven and passes through unchanged.
fn effective_overlay_style(cfg: &Config) -> fono_core::config::WaveformStyle {
    let style = cfg.overlay.style;
    if style.requires_streaming() {
        fono_core::config::WaveformStyle::default()
    } else {
        style
    }
}

/// Reconcile the user's `[audio].auto_stop_silence_ms` setting with the
/// MCP-listen default. The MCP listen path is opinionated: if the user
/// has explicitly set the dictation auto-stop, honour it; otherwise pick
/// a value that produces a responsive end-of-turn detection without
/// cutting off mid-sentence.
fn resolve_auto_stop_ms(cfg: &Config) -> u32 {
    if cfg.audio.auto_stop_silence_ms > 0 {
        cfg.audio.auto_stop_silence_ms
    } else {
        MCP_LISTEN_DEFAULT_AUTO_STOP_MS
    }
}

/// Map current `Pondering` elapsed time to a 0..=10_000 walk-progress
/// fixed-point. Mirrors the daemon's per-tick calculation at
/// `crates/fono/src/session.rs:1139-1160`.
///
/// Visual contract:
/// - 0..`pondering_visual_ms` — the FSM hasn't entered `Pondering`
///   yet (caller shouldn't be invoking this), value is `0`.
/// - `pondering_visual_ms`..`pondering_visual_ms + 1 s` — plain grace
///   so the label appears stable before the highlight starts walking.
///   Returns `0`.
/// - `pondering_visual_ms + 1 s`..`auto_stop_silence_ms` — linear ramp
///   from `1` to `10_000`.
/// - beyond `auto_stop_silence_ms` — clamps to `10_000`.
fn compute_walk_progress(
    elapsed_ms: f32,
    auto_stop_silence_ms: u32,
    pondering_visual_ms: u32,
) -> u16 {
    let grace_ms = 1000.0_f32;
    let walk_window_ms =
        (auto_stop_silence_ms.saturating_sub(pondering_visual_ms)) as f32 - grace_ms;
    if elapsed_ms < grace_ms || walk_window_ms <= 0.0 {
        return 0;
    }
    let frac = ((elapsed_ms - grace_ms) / walk_window_ms).clamp(0.0, 1.0);
    let p = (frac * 10_000.0) as u32 + 1;
    p.min(10_000) as u16
}

/// Match a free-form spoken transcript against an ordered list of
/// choices. Returns the choice as the agent sees it (one of the entries
/// in `choices`) or `None` if no choice matched confidently.
///
/// Matching rules, in order:
/// 1. Strip punctuation, lowercase, trim.
/// 2. If the transcript is exactly one of the choices (case-insensitive),
///    return it. This handles single-letter answers like `"A"` and
///    short words like `"yes"` / `"no"`.
/// 3. If the choices are single letters (`A`/`B`/`C`/...) and the
///    transcript starts with a phrase like `"a"`, `"option a"`,
///    `"choice a"`, `"the first one"`, `"first"`, return the matching
///    choice. Index aliases handle the common "first / second / third"
///    case the user may default to.
/// 4. Otherwise, if the transcript contains exactly one of the choice
///    strings as a substring, return that.
/// 5. Otherwise return `None`.
pub fn match_choice(transcript: &str, choices: &[String]) -> Option<String> {
    let norm = normalise(transcript);
    if norm.is_empty() || choices.is_empty() {
        return None;
    }

    // Rule 2 — exact match.
    for c in choices {
        if normalise(c) == norm {
            return Some(c.clone());
        }
    }

    // Rule 3 — single-letter or ordinal phrasing.
    let all_single_letter = choices.iter().all(|c| {
        let n = normalise(c);
        n.chars().count() == 1 && n.chars().next().unwrap().is_ascii_alphabetic()
    });
    if all_single_letter {
        // "option a", "choice a", "letter a", "a"
        for c in choices {
            let letter = normalise(c);
            for prefix in ["", "option ", "choice ", "letter ", "answer ", "pick "] {
                if norm == format!("{prefix}{letter}") {
                    return Some(c.clone());
                }
            }
        }
    }
    let ordinals = ["first", "second", "third", "fourth", "fifth", "sixth"];
    for (i, ordinal) in ordinals.iter().enumerate().take(choices.len()) {
        if norm == *ordinal || norm == format!("the {ordinal}") || norm == format!("{ordinal} one")
        {
            return Some(choices[i].clone());
        }
    }

    // Rule 4 — substring uniqueness.
    let hits: Vec<&String> = choices
        .iter()
        .filter(|c| !normalise(c).is_empty() && norm.contains(&normalise(c)))
        .collect();
    if hits.len() == 1 {
        return Some(hits[0].clone());
    }

    None
}

/// Map an internal [`crate::relevance::IgnoreReason`] to the public
/// overlay enum. Lives here (rather than in `relevance`) so that
/// module stays free of `fono-overlay` types — the renderer crate
/// must not pull in the MCP server's protocol types and vice-versa.
fn map_ignore_reason(reason: crate::relevance::IgnoreReason) -> OverlayIgnoreReason {
    use crate::relevance::IgnoreReason as R;
    match reason {
        R::Background => OverlayIgnoreReason::BackgroundSpeech,
        R::PromptEcho => OverlayIgnoreReason::EchoFromPrompt,
        R::TooShort | R::FillerOnly => OverlayIgnoreReason::LowConfidence,
    }
}

/// Lowercase, strip ASCII punctuation, collapse whitespace.
pub(crate) fn normalise(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = true;
    for ch in s.chars() {
        if ch.is_ascii_punctuation() {
            continue;
        }
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
            continue;
        }
        for low in ch.to_lowercase() {
            out.push(low);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn normalise_strips_punct_and_lowercases() {
        assert_eq!(normalise("  Hello, World! "), "hello world");
        assert_eq!(normalise("A."), "a");
        assert_eq!(normalise("YES!!!"), "yes");
    }

    #[test]
    fn exact_match_single_letter() {
        let choices = ch(&["A", "B", "C"]);
        assert_eq!(match_choice("A", &choices).as_deref(), Some("A"));
        assert_eq!(match_choice("b", &choices).as_deref(), Some("B"));
        assert_eq!(match_choice("C.", &choices).as_deref(), Some("C"));
    }

    #[test]
    fn exact_match_word_choices() {
        let choices = ch(&["yes", "no"]);
        assert_eq!(match_choice("Yes", &choices).as_deref(), Some("yes"));
        assert_eq!(match_choice("NO.", &choices).as_deref(), Some("no"));
    }

    #[test]
    fn option_phrasing_picks_letter() {
        let choices = ch(&["A", "B", "C"]);
        assert_eq!(match_choice("option A", &choices).as_deref(), Some("A"));
        assert_eq!(match_choice("Choice B", &choices).as_deref(), Some("B"));
        assert_eq!(match_choice("pick c", &choices).as_deref(), Some("C"));
    }

    #[test]
    fn ordinal_phrasing_picks_index() {
        let choices = ch(&["A", "B", "C"]);
        assert_eq!(match_choice("first", &choices).as_deref(), Some("A"));
        assert_eq!(match_choice("the second", &choices).as_deref(), Some("B"));
        assert_eq!(match_choice("third one", &choices).as_deref(), Some("C"));
    }

    #[test]
    fn substring_unique_match() {
        let choices = ch(&["enable", "disable", "skip"]);
        assert_eq!(match_choice("please skip it", &choices).as_deref(), Some("skip"));
    }

    #[test]
    fn substring_ambiguous_returns_none() {
        // "enable" is a substring of "disable" so the user saying
        // "enable" is unambiguous; but a transcript containing both
        // strings must not pick one over the other.
        let choices = ch(&["enable", "disable"]);
        assert_eq!(match_choice("disable", &choices).as_deref(), Some("disable"));
        assert_eq!(match_choice("enable disable", &choices), None);
    }

    #[test]
    fn empty_inputs_return_none() {
        assert_eq!(match_choice("", &ch(&["A"])), None);
        assert_eq!(match_choice("a", &[]), None);
        assert_eq!(match_choice("...", &ch(&["A"])), None);
    }

    #[test]
    fn effective_style_downgrades_transcript_to_default() {
        use fono_core::config::WaveformStyle;
        // Transcript needs the streaming-STT preview MCP listen never
        // runs, so it must fall back to the default audio visualisation.
        let mut cfg = Config::default();
        cfg.overlay.style = WaveformStyle::Transcript;
        assert_eq!(effective_overlay_style(&cfg), WaveformStyle::default());
    }

    #[test]
    fn effective_style_passes_audio_styles_through() {
        use fono_core::config::WaveformStyle;
        for style in [
            WaveformStyle::Bars,
            WaveformStyle::Oscilloscope,
            WaveformStyle::Fft,
            WaveformStyle::Heatmap,
            WaveformStyle::Terrain3d,
            WaveformStyle::System360,
        ] {
            let mut cfg = Config::default();
            cfg.overlay.style = style;
            assert_eq!(effective_overlay_style(&cfg), style, "{style:?} must pass through");
        }
    }

    #[test]
    fn resolve_auto_stop_respects_user_override() {
        let mut cfg = Config::default();
        cfg.audio.auto_stop_silence_ms = 5_000;
        assert_eq!(resolve_auto_stop_ms(&cfg), 5_000);
    }

    #[test]
    fn resolve_auto_stop_falls_back_to_default() {
        // auto_stop_silence_ms = 0 means "not set by user" → fall back to
        // the MCP-listen default. Config::default() sets it to 5_000 (the
        // dictation default), so we must explicitly zero it here to exercise
        // the fallback branch.
        let mut cfg = Config::default();
        cfg.audio.auto_stop_silence_ms = 0;
        assert_eq!(resolve_auto_stop_ms(&cfg), MCP_LISTEN_DEFAULT_AUTO_STOP_MS);
        assert_eq!(MCP_LISTEN_DEFAULT_AUTO_STOP_MS, 10_000);
    }

    #[test]
    fn walk_progress_holds_at_zero_during_grace() {
        // 10 s auto-stop, 250 ms visual threshold (the default), 1 s
        // grace. Anything in the first second after entering
        // `Pondering` must paint a flat highlight at progress 0.
        assert_eq!(compute_walk_progress(0.0, 10_000, 250), 0);
        assert_eq!(compute_walk_progress(500.0, 10_000, 250), 0);
        assert_eq!(compute_walk_progress(999.0, 10_000, 250), 0);
    }

    #[test]
    fn walk_progress_ramps_linearly_through_window() {
        // Window = 10_000 - 250 - 1000 = 8_750 ms. Midpoint sits
        // around the 5_000 / 10_000 mark; the integer mapping picks
        // up the +1 offset from the implementation.
        let mid = compute_walk_progress(1000.0 + 4_375.0, 10_000, 250);
        assert!(mid > 4_900 && mid < 5_100, "midpoint progress = {mid}");
    }

    #[test]
    fn walk_progress_clamps_at_ceiling() {
        assert_eq!(compute_walk_progress(60_000.0, 10_000, 250), 10_000);
    }

    #[test]
    fn walk_progress_handles_degenerate_windows() {
        // If `auto_stop_silence_ms` is shorter than the grace window
        // we never have room to walk — return 0 rather than panic.
        assert_eq!(compute_walk_progress(500.0, 500, 250), 0);
        assert_eq!(compute_walk_progress(2000.0, 1000, 250), 0);
    }

    mod palette_discovery {
        use fono_core::config::{Config, TtsBackend};
        use fono_core::voice_discovery::DiscoveredVoices;
        use fono_core::voice_palette::{Gender, Palette, PaletteVoice};

        use super::super::active_palette_in;

        fn elevenlabs_cfg(voice_discovery: bool) -> Config {
            let mut cfg = Config::default();
            cfg.tts.backend = TtsBackend::ElevenLabs;
            cfg.tts.voice_discovery = voice_discovery;
            cfg
        }

        fn write_cache(dir: &std::path::Path, backend: &str, ids: &[(&str, Gender)]) {
            let palette =
                Palette::new(ids.iter().map(|(id, g)| PaletteVoice::new(*id, *g)).collect());
            DiscoveredVoices::from_palette(backend, &palette, 1)
                .save(dir)
                .expect("save discovered cache");
        }

        #[test]
        fn prefers_a_fresh_discovered_cache() {
            let dir = tempfile::tempdir().unwrap();
            write_cache(
                dir.path(),
                "elevenlabs",
                &[("disc-female", Gender::Female), ("disc-male", Gender::Male)],
            );
            let palette = active_palette_in(&elevenlabs_cfg(true), Some(dir.path()));
            let ids: Vec<_> = palette.voices().iter().map(|v| v.backend_id.clone()).collect();
            assert_eq!(ids, vec!["disc-female", "disc-male"], "cache should win over curated");
        }

        #[test]
        fn falls_back_to_curated_when_cache_absent() {
            let dir = tempfile::tempdir().unwrap();
            let palette = active_palette_in(&elevenlabs_cfg(true), Some(dir.path()));
            // The curated ElevenLabs palette (6 premade voices) is used.
            assert_eq!(palette.voices().len(), 6);
            assert!(palette.by_backend_id("EXAVITQu4vr4xnSDxMaL").is_some());
        }

        #[test]
        fn falls_back_to_curated_on_corrupt_cache() {
            let dir = tempfile::tempdir().unwrap();
            let path = DiscoveredVoices::cache_path(dir.path(), "elevenlabs");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"{ not valid json").unwrap();
            let palette = active_palette_in(&elevenlabs_cfg(true), Some(dir.path()));
            assert_eq!(palette.voices().len(), 6, "corrupt cache must degrade to curated");
        }

        #[test]
        fn disabled_toggle_ignores_cache() {
            let dir = tempfile::tempdir().unwrap();
            write_cache(dir.path(), "elevenlabs", &[("disc-female", Gender::Female)]);
            let palette = active_palette_in(&elevenlabs_cfg(false), Some(dir.path()));
            assert_eq!(palette.voices().len(), 6, "voice_discovery=false uses curated");
        }
    }
}
