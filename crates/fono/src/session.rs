// SPDX-License-Identifier: GPL-3.0-only
//! End-to-end dictation orchestrator.
//!
//! Owns the active capture stream, the STT/LLM backends, and the
//! history-DB handle. Plumbs the FSM events from `fono-hotkey` through
//! the pipeline and emits `ProcessingDone` once the pipeline task has
//! finished (or failed).
//!
//! Per-stage timings are emitted at `info` level so users can diagnose
//! latency issues without enabling debug logging — see
//! `docs/plans/2026-04-25-fono-latency-v1.md` task L26.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fono_audio::{AudioCapture, CaptureConfig, RecordingBuffer};
use fono_core::config::{Config, ContextRule};
use fono_core::history::{HistoryDb, Transcription as HistoryRow};
use fono_core::{Paths, Secrets};
use fono_hotkey::{HotkeyAction, RecordingMode};
use fono_llm::{FormatContext, TextFormatter};
use fono_stt::SpeechToText;
#[cfg(feature = "interactive")]
use fono_stt::StreamingStt;
use std::sync::Mutex as StdMutex;
use std::thread::JoinHandle;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, warn};

/// Minimum duration of audio that will be passed to STT. Anything
/// shorter is treated as a misfire.
pub const MIN_RECORDING: Duration = Duration::from_millis(300);

/// Amplitude that maps to "full" on every audio-driven visualisation
/// — RMS for bars + the live-dictation VU bar, peak amplitude for the
/// oscilloscope. 0.22 is the value that looks balanced across all
/// three at typical speaking-voice levels.
#[cfg(any(feature = "interactive", feature = "waveform"))]
const WAVEFORM_AMPLITUDE_CEILING: f32 = 0.22;

/// FFT window size used by the `fft` and `heatmap` styles. 4096
/// samples ≈ 256 ms at 16 kHz — gives ~3.9 Hz per source bin so
/// 512 display bins across 0–3 kHz still average 1–2 source bins
/// each.
#[cfg(any(feature = "interactive", feature = "waveform"))]
const WAVEFORM_FFT_SIZE: usize = 4096;

/// Upper frequency cutoff for the FFT visualisations. Most voice
/// intelligibility (fundamentals + first three formants) sits below
/// 3 kHz — anything higher is sibilance or background noise that
/// clutters the view.
#[cfg(any(feature = "interactive", feature = "waveform"))]
const WAVEFORM_FFT_MAX_HZ: f32 = 3000.0;

/// Target display-bin count pushed to the overlay per frame. The
/// ticker maps each display bin to a `[start, end)` slice of the
/// source spectrum via integer multiply-divide, so non-integer
/// source-to-display ratios distribute cleanly without rounding all
/// the way down to a single source bin per display. 300 bars across
/// the ~588 px content area lands each at ≈2 px wide.
#[cfg(any(feature = "interactive", feature = "waveform"))]
const WAVEFORM_FFT_BINS: usize = 300;

/// dB range mapped to `[0.0, 1.0]` on the FFT / heatmap. Bins
/// quieter than the floor read as 0 (so background noise doesn't
/// light up the visualisation); louder than the ceiling saturate.
/// −20 dB floor keeps room noise / breathing dark; +30 dB ceiling
/// reserves the top of the scale for vowel peaks.
#[cfg(any(feature = "interactive", feature = "waveform"))]
const WAVEFORM_FFT_DB_FLOOR: f32 = -20.0;
#[cfg(any(feature = "interactive", feature = "waveform"))]
const WAVEFORM_FFT_DB_CEILING: f32 = 30.0;

/// Compute RMS of an f32 slice and normalise against
/// [`WAVEFORM_AMPLITUDE_CEILING`]. Result is clamped to `[0.0, 1.0]`.
#[cfg(any(feature = "interactive", feature = "waveform"))]
fn normalised_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    let rms = (sum_sq / samples.len() as f32).sqrt();
    (rms / WAVEFORM_AMPLITUDE_CEILING).clamp(0.0, 1.0)
}

/// Active capture session. The cpal stream itself is `!Send` on Linux
/// (ALSA / PipeWire), so it is kept on a dedicated thread; we
/// communicate with that thread via a stop signal and the shared
/// buffer.
struct CaptureSession {
    buffer: Arc<StdMutex<RecordingBuffer>>,
    stop_tx: std::sync::mpsc::Sender<()>,
    join: Option<JoinHandle<()>>,
    started_at: Instant,
    #[allow(dead_code)]
    mode: RecordingMode,
    /// AbortHandle for the audio-level ticker that feeds the standalone
    /// waveform overlay. `None` when no overlay is attached or the
    /// `waveform` feature is not compiled in.
    #[cfg(any(feature = "interactive", feature = "waveform"))]
    level_task: Option<tokio::task::AbortHandle>,
}

impl CaptureSession {
    fn stop_and_drain(mut self) -> (Vec<f32>, Duration) {
        #[cfg(any(feature = "interactive", feature = "waveform"))]
        if let Some(h) = self.level_task.take() {
            h.abort();
        }
        let _ = self.stop_tx.send(());
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
        let elapsed = self.started_at.elapsed();
        let pcm = self
            .buffer
            .lock()
            .map(|b| b.samples().to_vec())
            .unwrap_or_default();
        (pcm, elapsed)
    }
}

/// Active live-dictation session, parallel to [`CaptureSession`] but
/// driving the streaming pump + run-task. The capture thread owns the
/// cpal stream (which is `!Send` on Linux); the drain task ferries
/// freshly-captured PCM from the shared buffer into the [`crate::live::Pump`]
/// at a fixed cadence; the run task awaits the streaming STT and
/// produces the final [`crate::live::LiveTranscript`].
#[cfg(feature = "interactive")]
struct LiveCaptureSession {
    /// Stops the capture thread (drops the cpal stream, which in turn
    /// drops the forwarder closure and the realtime SPSC `Sender`,
    /// signalling EOF to the bridge thread).
    capture_stop_tx: std::sync::mpsc::Sender<()>,
    capture_join: Option<JoinHandle<()>>,
    /// Bridge thread that ferries PCM from the realtime crossbeam
    /// channel to the async drain task. Joined during shutdown so we
    /// don't tear down the pump while audio is still in flight.
    bridge_join: Option<JoinHandle<()>>,
    /// JoinHandle for the drain task; awaited during shutdown so we
    /// don't tear down the pump before all captured PCM has been
    /// pushed.
    drain_join: tokio::task::JoinHandle<()>,
    /// JoinHandle for the [`crate::live::LiveSession::run`] task.
    run_join: tokio::task::JoinHandle<anyhow::Result<crate::live::LiveTranscript>>,
    /// Overlay handle (clone of [`SessionOrchestrator::overlay`]) — kept
    /// so we can hide the window when the session ends. The handle is
    /// owned by the orchestrator and reused across sessions; this
    /// field is just a clone for convenience.
    overlay: Option<fono_overlay::OverlayHandle>,
    started_at: Instant,
    #[allow(dead_code)]
    mode: RecordingMode,
}

/// Snapshot of the per-stage latencies for one dictation. Logged at
/// `info` and surfaced via `fono history --json` in a follow-up phase.
#[derive(Debug, Default, Clone)]
pub struct PipelineMetrics {
    pub capture_ms: u64,
    pub samples: usize,
    pub trim_ms: u64,
    pub trimmed_samples: usize,
    pub stt_ms: u64,
    pub llm_ms: u64,
    pub inject_ms: u64,
    pub raw_chars: usize,
    pub final_chars: usize,
    /// True if the LLM was skipped because the raw transcript was below
    /// `Llm.skip_if_words_lt` words. Latency plan L9.
    pub llm_skipped_short: bool,
}

/// Outcome of one full dictation pipeline run, returned by the inner
/// pipeline task and consumed by the daemon for tray + tracing.
#[derive(Debug, Clone)]
pub enum PipelineOutcome {
    /// Successfully transcribed and (optionally) cleaned + injected text.
    Completed {
        raw: String,
        cleaned: Option<String>,
        metrics: PipelineMetrics,
    },
    /// Recording was empty or shorter than [`MIN_RECORDING`]. No history
    /// row was written.
    EmptyOrTooShort { duration_ms: u64 },
    /// Pipeline error after audio was captured. Logged but not fatal.
    Failed(String),
}

/// Trait abstraction over text injection so the integration test can
/// substitute a buffer-collector and skip the real keyboard backend.
///
/// Returns `true` when the inject path itself has already populated the
/// system clipboard with the final text (e.g. the X11 `xtest-paste`
/// backend, or the clipboard fallback). The orchestrator uses that
/// signal to skip the redundant belt‑and‑suspenders copy in
/// `run_pipeline`, which otherwise duplicates clipboard writes (and
/// log lines) on every dictation.
pub trait Injector: Send + Sync + 'static {
    fn inject(&self, text: &str) -> Result<bool>;
}

/// Default injector — calls into [`fono_inject::type_text_with_outcome`]
/// so it can surface a desktop notification when no key-injection
/// backend is available and the cleaned text was instead copied to the
/// clipboard. Without this fallback fono "appears to do nothing" on
/// hosts that have neither `wtype`/`ydotool` (Wayland) nor an X11
/// session for `enigo` to talk to.
pub struct RealInjector;

impl Injector for RealInjector {
    fn inject(&self, text: &str) -> Result<bool> {
        match fono_inject::type_text_with_outcome(text)? {
            fono_inject::InjectOutcome::Typed(backend) => {
                tracing::info!("inject backend: typed via {backend}");
                // `xtest-paste` pastes by populating the X CLIPBOARD
                // and synthesising Shift+Insert, so the clipboard
                // already holds `text` — no belt-and-suspenders copy
                // needed afterwards. All other typed backends
                // (`wtype`/`ydotool`/`xdotool`/`enigo`) inject keys
                // directly and leave the clipboard untouched.
                Ok(backend == "xtest-paste")
            }
            fono_inject::InjectOutcome::Clipboard(tool) => {
                tracing::info!("inject backend: clipboard via {tool} (no key-injection worked)");
                fono_core::notify::send(
                    "Fono — copied to clipboard",
                    &format!(
                        "No key-injection backend was available. The cleaned text \
                         is on the clipboard (via {tool}); press Ctrl+V to paste."
                    ),
                    "edit-paste",
                    6_000,
                    fono_core::notify::Urgency::Normal,
                );
                Ok(true)
            }
        }
    }
}

/// Trait abstraction over focus detection (X11/Wayland-dependent) so the
/// integration test can stub out window classes deterministically.
pub trait FocusProbe: Send + Sync + 'static {
    fn probe(&self) -> (Option<String>, Option<String>);
}

/// Default focus probe — calls into [`fono_inject::detect_focus`].
pub struct RealFocusProbe;

impl FocusProbe for RealFocusProbe {
    fn probe(&self) -> (Option<String>, Option<String>) {
        match fono_inject::detect_focus() {
            Ok(f) => (f.window_class, f.window_title),
            Err(_) => (None, None),
        }
    }
}

/// The orchestrator. One per running daemon.
///
/// `stt`, `llm`, and `config` live behind `RwLock<Arc<…>>` so the
/// daemon can hot-swap them when the user runs `fono use …` or
/// `fono keys …` without a restart. The recording hot path takes a
/// single `read()` on each lock and clones the inner `Arc`, so the
/// writer (Reload) only blocks the negligibly-short clone — never an
/// in-flight pipeline. Provider-switching plan task S12.
pub struct SessionOrchestrator {
    stt: Arc<StdRwLock<Arc<dyn SpeechToText>>>,
    /// Optional streaming variant of the active STT backend. Populated
    /// when `[interactive]` is enabled and the backend supports
    /// streaming (Slice A: local only). `None` means the live path
    /// must gracefully fall back to the batch path.
    #[cfg(feature = "interactive")]
    streaming_stt: Arc<StdRwLock<Option<Arc<dyn StreamingStt>>>>,
    llm: Arc<StdRwLock<Option<Arc<dyn TextFormatter>>>>,
    history: Arc<Mutex<HistoryDb>>,
    capture_cfg: CaptureConfig,
    capture: Arc<Mutex<Option<CaptureSession>>>,
    /// Active live-dictation session, parallel to [`Self::capture`] but
    /// holding the streaming pump + run-task instead of the batch
    /// recorder. Wiring fix follow-up to Slice A v7.
    #[cfg(feature = "interactive")]
    live_capture: Arc<Mutex<Option<LiveCaptureSession>>>,
    /// Long-lived overlay handle, spawned **once** at orchestrator
    /// construction and reused across every live-dictation and batch
    /// recording session. winit refuses to construct a second
    /// `EventLoop` in the same process, so we MUST keep this alive
    /// for the daemon's lifetime rather than spawning per session.
    /// `None` means the overlay is disabled in config or failed to
    /// spawn at startup.
    #[cfg(any(feature = "interactive", feature = "waveform"))]
    overlay: Arc<StdRwLock<Option<fono_overlay::OverlayHandle>>>,
    pipeline_in_flight: Arc<AtomicBool>,
    config: Arc<StdRwLock<Arc<Config>>>,
    /// Resolved XDG paths; used by [`Self::reload`] to re-read config
    /// + secrets from disk.
    paths: Option<Arc<Paths>>,
    action_tx: mpsc::UnboundedSender<HotkeyAction>,
    injector: Arc<dyn Injector>,
    focus: Arc<dyn FocusProbe>,
}

impl SessionOrchestrator {
    /// Construct from a fresh config + secrets, building both backends.
    /// Returns an error if the STT factory fails — the daemon should
    /// still come up but in a "degraded" mode where hotkeys notify the
    /// user. LLM construction failure downgrades to "no cleanup".
    pub fn new(
        config: Arc<Config>,
        secrets: &Secrets,
        paths: &Paths,
        action_tx: mpsc::UnboundedSender<HotkeyAction>,
    ) -> Result<Self> {
        let stt = fono_stt::build_stt(
            &config.stt,
            &config.general,
            secrets,
            &paths.whisper_models_dir(),
        )
        .context("build STT backend")?;
        let llm = match fono_llm::build_llm(&config.llm, secrets, &paths.llm_models_dir()) {
            Ok(opt) => opt,
            Err(e) => {
                warn!("LLM backend unavailable; continuing without cleanup: {e:#}");
                None
            }
        };
        let history = Arc::new(Mutex::new(
            HistoryDb::open(&paths.history_db()).context("open history db")?,
        ));
        let capture_cfg = CaptureConfig {
            target_sample_rate: config.audio.sample_rate,
        };
        let config_for_env = Arc::clone(&config);
        let mut orch = Self::with_parts(
            stt,
            llm,
            history,
            capture_cfg,
            Arc::clone(&config),
            action_tx,
            Arc::new(RealInjector),
            Arc::new(RealFocusProbe),
        );
        orch.paths = Some(Arc::new(paths.clone()));
        // Populate the streaming-STT slot when this build supports
        // interactive mode. Errors are non-fatal — the live path
        // gracefully falls back to batch when the slot is `None`.
        #[cfg(feature = "interactive")]
        {
            match fono_stt::build_streaming_stt(
                &config.stt,
                &config.general,
                &config.interactive,
                secrets,
                &paths.whisper_models_dir(),
            ) {
                Ok(opt) => {
                    if let Ok(mut g) = orch.streaming_stt.write() {
                        *g = opt;
                    }
                }
                Err(e) => {
                    warn!(
                        "streaming STT factory failed; live dictation will fall back \
                         to batch: {e:#}"
                    );
                }
            }
        }
        // Spawn the overlay event loop **once**. winit forbids
        // creating a second `EventLoop` per process, so we cannot
        // tear this down between sessions. We keep the handle alive
        // for the daemon's lifetime and just toggle visibility via
        // `set_state` on each session start/stop. Best-effort: a
        // failure here just disables the overlay, dictation still
        // works.
        //
        // Two branches share the same long-lived handle:
        //   * `[interactive].enabled` → the text-rendering overlay
        //     used by live dictation. The VU bar config flag is
        //     pushed once the handle is up.
        //   * else if `[overlay].waveform` (and the `waveform`
        //     feature is compiled in) → the standalone audio
        //     visualisation overlay used during batch recording.
        // Only one branch fires; live dictation takes precedence.
        #[cfg(any(feature = "interactive", feature = "waveform"))]
        {
            let spawn_result: Option<std::io::Result<fono_overlay::OverlayHandle>> = {
                #[cfg(feature = "interactive")]
                {
                    if config.interactive.enabled {
                        Some(fono_overlay::RealOverlay::spawn())
                    } else if cfg!(feature = "waveform") && config.overlay.waveform {
                        Some(fono_overlay::RealOverlay::spawn_waveform(
                            config.overlay.style,
                        ))
                    } else {
                        None
                    }
                }
                #[cfg(all(feature = "waveform", not(feature = "interactive")))]
                {
                    if config.overlay.waveform {
                        Some(fono_overlay::RealOverlay::spawn_waveform(
                            config.overlay.style,
                        ))
                    } else {
                        None
                    }
                }
            };
            match spawn_result {
                Some(Ok(h)) => {
                    h.set_volume_bar(config.overlay.volume_bar);
                    if let Ok(mut g) = orch.overlay.write() {
                        *g = Some(h);
                    }
                }
                Some(Err(e)) => {
                    warn!(
                        "overlay: spawn failed at orchestrator startup ({e:#}); \
                         dictation will run without an overlay window"
                    );
                }
                None => {}
            }
        }
        // Apply [inject].paste_shortcut to the FONO_PASTE_SHORTCUT env
        // var so xtest-paste picks up the configured combo without
        // plumbing it through the Injector trait.
        apply_paste_shortcut_env(&config_for_env);
        // Latency plan L2/L3/L5 — pay TLS handshake, mmap, and inject
        // backend page-cache costs at daemon startup so the first
        // dictation is fast. Failures are logged but non-fatal.
        orch.spawn_warmups();
        Ok(orch)
    }

    /// Hot-reload: re-read config + secrets, rebuild STT + LLM, and
    /// atomically swap the orchestrator's handles. In-flight pipelines
    /// finish on the old backends (they cloned the `Arc` at spawn);
    /// the next `StartRecording` picks up the new ones.
    /// Provider-switching plan task S11/S13.
    ///
    /// Returns a short human-readable summary (active backends).
    pub async fn reload(&self) -> Result<String> {
        let paths = self
            .paths
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("orchestrator built without Paths; cannot reload"))?
            .clone();
        let cfg = Config::load(&paths.config_file()).context("reload: read config")?;
        let secrets = Secrets::load(&paths.secrets_file()).context("reload: read secrets")?;
        let new_stt = fono_stt::build_stt(
            &cfg.stt,
            &cfg.general,
            &secrets,
            &paths.whisper_models_dir(),
        )
        .context("reload: build STT")?;
        let new_llm = match fono_llm::build_llm(&cfg.llm, &secrets, &paths.llm_models_dir()) {
            Ok(opt) => opt,
            Err(e) => {
                warn!("reload: LLM backend unavailable; continuing without cleanup: {e:#}");
                None
            }
        };
        let stt_name = new_stt.name().to_string();
        let llm_name = new_llm
            .as_ref()
            .map_or_else(|| "none".to_string(), |l| l.name().to_string());
        // Lock-write order matches read order in the hot path.
        if let Ok(mut guard) = self.stt.write() {
            *guard = new_stt;
        }
        #[cfg(feature = "interactive")]
        {
            let new_streaming = match fono_stt::build_streaming_stt(
                &cfg.stt,
                &cfg.general,
                &cfg.interactive,
                &secrets,
                &paths.whisper_models_dir(),
            ) {
                Ok(opt) => opt,
                Err(e) => {
                    warn!(
                        "reload: streaming STT factory failed; live dictation will fall \
                         back to batch: {e:#}"
                    );
                    None
                }
            };
            if let Ok(mut guard) = self.streaming_stt.write() {
                *guard = new_streaming;
            }
        }
        if let Ok(mut guard) = self.llm.write() {
            *guard = new_llm;
        }
        if let Ok(mut guard) = self.config.write() {
            *guard = Arc::new(cfg);
        }
        if let Ok(guard) = self.config.read() {
            apply_paste_shortcut_env(&guard);
        }
        // Re-prewarm the new backends so the first post-switch
        // dictation isn't cold (latency plan L3 still applies).
        self.spawn_warmups();
        info!("reloaded: stt={stt_name} llm={llm_name}");
        Ok(format!("active: stt={stt_name} llm={llm_name}"))
    }

    /// Read-only snapshot of the active backend names. Returns the
    /// **canonical** lowercase identifier from
    /// [`fono_core::providers::stt_backend_str`] /
    /// [`fono_core::providers::llm_backend_str`] (e.g. `"local"`,
    /// `"groq"`, `"none"`) so the tray's active-marker comparison and
    /// the doctor / status output stay in sync. The trait `name()`s
    /// (e.g. `"whisper-local"`, `"llama-local"`) are intentionally
    /// **not** used here — they're an implementation detail.
    #[must_use]
    pub fn active_backends(&self) -> (String, String) {
        let cfg = self.current_config();
        let stt = fono_core::providers::stt_backend_str(&cfg.stt.backend).to_string();
        let llm = fono_core::providers::llm_backend_str(&cfg.llm.backend).to_string();
        (stt, llm)
    }

    fn current_stt(&self) -> Arc<dyn SpeechToText> {
        Arc::clone(&self.stt.read().expect("stt lock poisoned"))
    }

    /// Public snapshot of the active STT backend. Used by the LAN
    /// Wyoming server (Slice 3 of the network plan) to obtain a fresh
    /// `Arc` per accepted connection so `Reload`-driven backend swaps
    /// are tracked without restarting the listener.
    #[must_use]
    pub fn stt_snapshot(&self) -> Arc<dyn SpeechToText> {
        self.current_stt()
    }

    fn current_llm(&self) -> Option<Arc<dyn TextFormatter>> {
        self.llm.read().expect("llm lock poisoned").clone()
    }

    fn current_config(&self) -> Arc<Config> {
        Arc::clone(&self.config.read().expect("config lock poisoned"))
    }

    /// Fire-and-forget warmup for STT, LLM and the inject backend.
    /// Latency plan tasks L2 (whisper mmap), L3 (HTTP keep-alive),
    /// L5 (inject binary page-cache).
    fn spawn_warmups(&self) {
        let stt = self.current_stt();
        tokio::spawn(async move {
            let started = Instant::now();
            match stt.prewarm().await {
                Ok(()) => debug!(
                    "warmup: stt {} ready in {}ms",
                    stt.name(),
                    started.elapsed().as_millis()
                ),
                Err(e) => debug!("warmup: stt {} prewarm skipped: {e:#}", stt.name()),
            }
        });
        if let Some(llm) = self.current_llm() {
            tokio::spawn(async move {
                let started = Instant::now();
                match llm.prewarm().await {
                    Ok(()) => debug!(
                        "warmup: llm {} ready in {}ms",
                        llm.name(),
                        started.elapsed().as_millis()
                    ),
                    Err(e) => debug!("warmup: llm {} prewarm skipped: {e:#}", llm.name()),
                }
            });
        }
        // Inject backend warmup runs on a blocking thread because the
        // probe shells out to `wtype --version` / `ydotool --version`.
        tokio::task::spawn_blocking(|| match fono_inject::warm_backend() {
            Ok(name) => debug!("warmup: inject backend = {name}"),
            Err(e) => debug!("warmup: inject backend probe failed: {e:#}"),
        });
    }

    /// Wire pre-built components together. Used by both [`Self::new`]
    /// and the integration test.
    #[allow(clippy::too_many_arguments)]
    pub fn with_parts(
        stt: Arc<dyn SpeechToText>,
        llm: Option<Arc<dyn TextFormatter>>,
        history: Arc<Mutex<HistoryDb>>,
        capture_cfg: CaptureConfig,
        config: Arc<Config>,
        action_tx: mpsc::UnboundedSender<HotkeyAction>,
        injector: Arc<dyn Injector>,
        focus: Arc<dyn FocusProbe>,
    ) -> Self {
        Self {
            stt: Arc::new(StdRwLock::new(stt)),
            #[cfg(feature = "interactive")]
            streaming_stt: Arc::new(StdRwLock::new(None)),
            llm: Arc::new(StdRwLock::new(llm)),
            history,
            capture_cfg,
            capture: Arc::new(Mutex::new(None)),
            #[cfg(feature = "interactive")]
            live_capture: Arc::new(Mutex::new(None)),
            #[cfg(any(feature = "interactive", feature = "waveform"))]
            overlay: Arc::new(StdRwLock::new(None)),
            pipeline_in_flight: Arc::new(AtomicBool::new(false)),
            config: Arc::new(StdRwLock::new(config)),
            paths: None,
            action_tx,
            injector,
            focus,
        }
    }

    /// Begin recording. Refuses if a previous pipeline is still running.
    #[allow(
        clippy::too_many_lines,
        clippy::suboptimal_flops,
        clippy::many_single_char_names
    )]
    pub async fn on_start_recording(&self, mode: RecordingMode) -> Result<()> {
        fono_stt::rate_limit_notify::reset_session_flag();
        if self.pipeline_in_flight.load(Ordering::SeqCst) {
            warn!("recording requested while previous pipeline still running; ignoring");
            return Ok(());
        }
        let mut slot = self.capture.lock().await;
        if slot.is_some() {
            warn!("capture already in progress; ignoring duplicate start");
            return Ok(());
        }
        let cap_cfg = self.capture_cfg.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel::<
            std::result::Result<Arc<StdMutex<RecordingBuffer>>, String>,
        >();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let join = std::thread::Builder::new()
            .name("fono-capture".into())
            .spawn(move || {
                let cap = AudioCapture::new(cap_cfg);
                match cap.start() {
                    Ok(handle) => {
                        let _ = started_tx.send(Ok(Arc::clone(&handle.buffer)));
                        let _ = stop_rx.recv();
                        drop(handle);
                    }
                    Err(e) => {
                        let _ = started_tx.send(Err(format!("{e:#}")));
                    }
                }
            })
            .context("spawn capture thread")?;
        let buffer = match started_rx.recv() {
            Ok(Ok(b)) => b,
            Ok(Err(e)) => {
                let _ = join.join();
                return Err(anyhow::anyhow!("audio capture failed to start: {e}"));
            }
            Err(_) => {
                return Err(anyhow::anyhow!(
                    "capture thread died before reporting status"
                ))
            }
        };
        let cfg = self.current_config();
        if cfg.general.auto_mute_system {
            fono_audio::mute::set_default_sink_mute(true);
        }
        info!(
            "recording started (mode={:?} sample_rate={})",
            mode, self.capture_cfg.target_sample_rate
        );

        // Standalone-waveform overlay: spawn the level ticker that
        // feeds bars/pulse RMS or oscilloscope sample snapshots, and
        // toggle the overlay to `Recording` so the panel shows up. Only
        // fires when `[overlay].waveform = true` produced a live
        // overlay handle at orchestrator startup. Live-dictation mode
        // owns its own visibility transitions further down.
        #[cfg(any(feature = "interactive", feature = "waveform"))]
        let level_task = {
            let want_waveform = cfg.overlay.waveform && !cfg.interactive.enabled;
            let handle = self.overlay.read().ok().and_then(|g| g.clone());
            match (want_waveform, handle) {
                (true, Some(o)) => {
                    o.set_state(fono_overlay::OverlayState::Recording { db: 0 });
                    let style = cfg.overlay.style;
                    let buf = Arc::clone(&buffer);
                    let sample_rate = self.capture_cfg.target_sample_rate;
                    let task = tokio::spawn(async move {
                        match style {
                            fono_core::config::WaveformStyle::Oscilloscope => {
                                // 20 fps snapshots of the last 50 ms.
                                // Samples are pre-scaled by
                                // `1.0 / WAVEFORM_AMPLITUDE_CEILING`
                                // so the trace fills a comfortable
                                // chunk of the panel at typical
                                // speaking-voice amplitude. The
                                // overlay's 5000-sample ring buffer
                                // (≈300 ms) keeps the trace scrolling
                                // slowly enough for individual cycles
                                // to be visible. 50 ms snapshots
                                // (matching the tick rate) keep the
                                // ring buffer gap-free as new chunks
                                // are accumulated.
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
                                        o.push_samples(snap);
                                    }
                                }
                            }
                            fono_core::config::WaveformStyle::Fft
                            | fono_core::config::WaveformStyle::Heatmap => {
                                // 20 fps real-input FFT. Hann window +
                                // 1024-point R2C, then aggregate the
                                // bottom of the spectrum (DC …
                                // WAVEFORM_FFT_MAX_HZ) into display
                                // bins via mean. Convert to dB and
                                // normalise to [0, 1] so the colour /
                                // bar mapping has a sane dynamic
                                // range across both quiet and loud
                                // speech.
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
                                // How many source bins cover [0,
                                // WAVEFORM_FFT_MAX_HZ]? At 16 kHz /
                                // 4096 that's 3000 / 3.9 ≈ 768 bins.
                                // We then carve them into `display_bins`
                                // slices via integer multiply-divide
                                // (see the per-bin loop below) which
                                // handles any source-to-display ratio.
                                let max_source_bin = ((WAVEFORM_FFT_MAX_HZ
                                    * WAVEFORM_FFT_SIZE as f32)
                                    / sample_rate as f32)
                                    as usize;
                                let display_bins = WAVEFORM_FFT_BINS.max(1);
                                let db_span = WAVEFORM_FFT_DB_CEILING - WAVEFORM_FFT_DB_FLOOR;
                                // 20 fps — matches the bars / oscilloscope
                                // tick rate and halves the per-second
                                // render cost vs the previous 30 fps.
                                let mut tick = tokio::time::interval(Duration::from_millis(50));
                                loop {
                                    tick.tick().await;
                                    // Copy the most recent FFT_SIZE
                                    // samples into the FFT input,
                                    // applying the Hann window. If the
                                    // buffer is shorter than the FFT,
                                    // zero-pad on the front.
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
                                        // Integer multiply-divide
                                        // distributes [0, max_source_bin)
                                        // evenly across `display_bins`
                                        // even when the ratio isn't a
                                        // clean integer.
                                        let start = (display_i * max_source_bin) / display_bins;
                                        let end_raw =
                                            ((display_i + 1) * max_source_bin) / display_bins;
                                        let end = end_raw.max(start + 1).min(max_source_bin);
                                        let mut sum = 0.0_f32;
                                        for c in &output_buf[start..end] {
                                            sum += c.re.hypot(c.im);
                                        }
                                        let mag = sum / (end - start) as f32;
                                        let db = 20.0 * mag.max(1e-6).log10();
                                        *slot = ((db - WAVEFORM_FFT_DB_FLOOR) / db_span)
                                            .clamp(0.0, 1.0);
                                    }
                                    o.push_fft_bins(bins);
                                }
                            }
                            fono_core::config::WaveformStyle::Bars => {
                                // 20 fps RMS for bars — fluid enough
                                // for voice activity, half the redraw
                                // cost of a 30 fps tick.
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
                        }
                    });
                    Some(task.abort_handle())
                }
                _ => None,
            }
        };

        *slot = Some(CaptureSession {
            buffer,
            stop_tx,
            join: Some(join),
            started_at: Instant::now(),
            mode,
            #[cfg(any(feature = "interactive", feature = "waveform"))]
            level_task,
        });
        drop(slot);
        Ok(())
    }

    /// Stop recording and spawn the pipeline task. Returns immediately.
    pub async fn on_stop_recording(&self) {
        let taken = self.capture.lock().await.take();
        let Some(session) = taken else {
            debug!("stop with no active capture");
            let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
            return;
        };
        let cfg = self.current_config();
        if cfg.general.auto_mute_system {
            fono_audio::mute::set_default_sink_mute(false);
        }
        // Standalone-waveform overlay: shift to amber `Processing`
        // while STT runs. Live-dictation mode owns its own state
        // transitions; only flip when this is the batch path.
        #[cfg(any(feature = "interactive", feature = "waveform"))]
        if cfg.overlay.waveform && !cfg.interactive.enabled {
            if let Some(o) = self.overlay.read().ok().and_then(|g| g.clone()) {
                o.set_state(fono_overlay::OverlayState::Processing);
            }
        }
        let (samples, elapsed) = tokio::task::spawn_blocking(move || session.stop_and_drain())
            .await
            .unwrap_or_default();
        let capture_ms = elapsed.as_millis() as u64;
        info!(
            "recording stopped: {capture_ms} ms / {} samples",
            samples.len()
        );

        if elapsed < MIN_RECORDING || samples.is_empty() {
            warn!("recording too short ({capture_ms} ms); skipping STT");
            #[cfg(any(feature = "interactive", feature = "waveform"))]
            if cfg.overlay.waveform && !cfg.interactive.enabled {
                if let Some(o) = self.overlay.read().ok().and_then(|g| g.clone()) {
                    o.set_state(fono_overlay::OverlayState::Hidden);
                }
            }
            let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
            return;
        }

        self.spawn_pipeline(samples, capture_ms);
    }

    /// Cancel an active recording, dropping the audio without invoking STT.
    pub async fn on_cancel(&self) {
        let taken = self.capture.lock().await.take();
        if let Some(session) = taken {
            let _ = tokio::task::spawn_blocking(move || session.stop_and_drain()).await;
            let cfg = self.current_config();
            if cfg.general.auto_mute_system {
                fono_audio::mute::set_default_sink_mute(false);
            }
            // Standalone-waveform overlay: hide immediately on cancel
            // (no pipeline phase follows).
            #[cfg(any(feature = "interactive", feature = "waveform"))]
            if cfg.overlay.waveform && !cfg.interactive.enabled {
                if let Some(o) = self.overlay.read().ok().and_then(|g| g.clone()) {
                    o.set_state(fono_overlay::OverlayState::Hidden);
                }
            }
            info!("recording cancelled by user");
        }
        let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
    }

    /// Re-inject the most recent cleaned (or raw) transcription.
    pub async fn on_paste_last(&self) {
        let last = {
            let db = self.history.lock().await;
            match db.last_text() {
                Ok(opt) => opt,
                Err(e) => {
                    warn!("paste-last: history lookup failed: {e:#}");
                    return;
                }
            }
        };
        if let Some(text) = last {
            info!("paste-last: injecting {} bytes", text.len());
            if let Err(e) = self.injector.inject(&text) {
                warn!("paste-last: inject failed: {e:#}");
            }
        } else {
            warn!("paste-last: no history yet");
        }
    }

    fn spawn_pipeline(&self, pcm: Vec<f32>, capture_ms: u64) {
        let stt = self.current_stt();
        let llm = self.current_llm();
        let history = Arc::clone(&self.history);
        let action_tx = self.action_tx.clone();
        let in_flight = Arc::clone(&self.pipeline_in_flight);
        let config = self.current_config();
        let injector = Arc::clone(&self.injector);
        let focus = Arc::clone(&self.focus);
        let sample_rate = self.capture_cfg.target_sample_rate;
        // Standalone-waveform overlay: clone the handle so the pipeline
        // task can hide the panel once STT + LLM + inject are done. The
        // overlay was already shifted to `Processing` in
        // `on_stop_recording`; we just clear it back to `Hidden` on
        // every terminal outcome.
        #[cfg(any(feature = "interactive", feature = "waveform"))]
        let overlay = if config.overlay.waveform && !config.interactive.enabled {
            self.overlay.read().ok().and_then(|g| g.clone())
        } else {
            None
        };

        in_flight.store(true, Ordering::SeqCst);
        tokio::spawn(async move {
            let outcome = run_pipeline(
                pcm,
                sample_rate,
                capture_ms,
                stt.as_ref(),
                llm.as_deref(),
                &history,
                &config,
                injector.as_ref(),
                focus.as_ref(),
            )
            .await;
            match &outcome {
                PipelineOutcome::Completed { metrics, .. } => {
                    info!(
                        "pipeline ok: capture={}ms trim={}ms ({}→{} samples) stt={}ms llm={}ms{} inject={}ms ({} → {} chars)",
                        metrics.capture_ms,
                        metrics.trim_ms,
                        metrics.samples,
                        metrics.trimmed_samples,
                        metrics.stt_ms,
                        metrics.llm_ms,
                        if metrics.llm_skipped_short { " (skipped:short)" } else { "" },
                        metrics.inject_ms,
                        metrics.raw_chars,
                        metrics.final_chars,
                    );
                }
                PipelineOutcome::EmptyOrTooShort { duration_ms } => {
                    warn!("pipeline: empty/too short ({duration_ms}ms)");
                }
                PipelineOutcome::Failed(msg) => {
                    error!("pipeline failed: {msg}");
                }
            }
            #[cfg(any(feature = "interactive", feature = "waveform"))]
            if let Some(o) = overlay {
                o.set_state(fono_overlay::OverlayState::Hidden);
            }
            in_flight.store(false, Ordering::SeqCst);
            let _ = action_tx.send(HotkeyAction::ProcessingDone);
        });
    }

    /// Synchronous pipeline entrypoint exposed for the integration test
    /// and `fono record`. Drives capture-already-done audio through the
    /// orchestrator's STT/LLM/inject/history.
    pub async fn run_oneshot(&self, pcm: Vec<f32>, capture_ms: u64) -> PipelineOutcome {
        let stt = self.current_stt();
        let llm = self.current_llm();
        let config = self.current_config();
        run_pipeline(
            pcm,
            self.capture_cfg.target_sample_rate,
            capture_ms,
            stt.as_ref(),
            llm.as_deref(),
            &self.history,
            &config,
            self.injector.as_ref(),
            self.focus.as_ref(),
        )
        .await
    }
}

#[cfg(feature = "interactive")]
impl SessionOrchestrator {
    /// Snapshot of the live (streaming) STT slot, mirroring
    /// [`Self::current_stt`]. Returns `None` when no streaming-capable
    /// backend is currently loaded — the caller MUST then fall back to
    /// the batch path. Slice A wiring follow-up.
    fn current_streaming_stt(&self) -> Option<Arc<dyn StreamingStt>> {
        self.streaming_stt
            .read()
            .expect("streaming_stt lock poisoned")
            .clone()
    }

    /// Begin a live (streaming) dictation session. Same in-flight
    /// guarantees as [`Self::on_start_recording`]: refuses if a
    /// previous pipeline is still running.
    ///
    /// Falls back to [`Self::on_start_recording`] when no streaming
    /// backend is available — currently true for every cloud STT in
    /// Slice A. The fallback keeps `[interactive].enabled = true`
    /// from breaking dictation entirely on a Groq-configured user's
    /// machine; the daemon logs a `warn!` so the diagnosis is obvious.
    #[allow(clippy::too_many_lines, clippy::significant_drop_tightening)]
    pub async fn on_start_live_dictation(&self, mode: RecordingMode) -> Result<()> {
        fono_stt::rate_limit_notify::reset_session_flag();
        tracing::debug!("live dictation: starting capture (mode={mode:?})");
        let Some(streaming) = self.current_streaming_stt() else {
            warn!(
                "live-dictation: no streaming-capable STT backend currently loaded \
                 (set `[stt].backend = \"local\"` or wait for Slice B); \
                 falling back to batch path"
            );
            return self.on_start_recording(mode).await;
        };
        if self.pipeline_in_flight.load(Ordering::SeqCst) {
            warn!("live-dictation requested while previous pipeline still running; ignoring");
            return Ok(());
        }
        let mut slot = self.live_capture.lock().await;
        if slot.is_some() {
            warn!("live-dictation already in progress; ignoring duplicate start");
            return Ok(());
        }

        // Slice A: streaming pipeline operates at 16 kHz to keep the
        // pump's broadcast frame budget aligned with whisper. The
        // capture stage resamples for us.
        let sample_rate = 16_000_u32;
        let cap_cfg = CaptureConfig {
            target_sample_rate: sample_rate,
        };

        // ---- Spawn the capture thread ----------------------------
        // The cpal stream uses the new realtime forwarder API:
        // each data callback resamples to mono f32 @ 16 kHz and
        // pushes the slice into a bounded crossbeam SPSC. The audio
        // thread MUST NOT block on a tokio runtime, so we drop on
        // overflow (logged at warn) rather than queue. The forwarder
        // closure is owned by the cpal `Stream`; dropping the stream
        // (when capture_stop_rx fires) drops the closure and thereby
        // the `audio_tx` Sender, signalling EOF to the bridge thread
        // downstream. Slice B1 / R10.x — replaces the prior 30 ms
        // RecordingBuffer-poll drain.
        let (audio_tx, audio_rx) = crossbeam_channel::bounded::<Vec<f32>>(64);
        let (started_tx, started_rx) =
            std::sync::mpsc::channel::<std::result::Result<(), String>>();
        let (capture_stop_tx, capture_stop_rx) = std::sync::mpsc::channel::<()>();
        let cap_cfg_thread = cap_cfg;
        let capture_join = std::thread::Builder::new()
            .name("fono-live-capture".into())
            .spawn(move || {
                let cap = AudioCapture::new(cap_cfg_thread);
                let forwarder_tx = audio_tx;
                let result = cap.start_with_forwarder(move |pcm: &[f32]| {
                    // Bounded try_send: drop on overflow (a frame
                    // dropped on the audio thread is preferable to a
                    // glitch caused by allocator pressure under
                    // pump backpressure).
                    if forwarder_tx.try_send(pcm.to_vec()).is_err() {
                        warn!(
                            "live-capture: realtime SPSC full ({} samples dropped)",
                            pcm.len()
                        );
                    }
                });
                match result {
                    Ok(handle) => {
                        let _ = started_tx.send(Ok(()));
                        let _ = capture_stop_rx.recv();
                        drop(handle);
                    }
                    Err(e) => {
                        let _ = started_tx.send(Err(format!("{e:#}")));
                    }
                }
            })
            .context("spawn live-capture thread")?;
        match started_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = capture_join.join();
                return Err(anyhow::anyhow!("live audio capture failed to start: {e}"));
            }
            Err(_) => {
                return Err(anyhow::anyhow!(
                    "live capture thread died before reporting status"
                ))
            }
        }

        let cfg = self.current_config();
        if cfg.general.auto_mute_system {
            fono_audio::mute::set_default_sink_mute(true);
        }

        // ---- Reuse the long-lived overlay handle -----------------
        // winit forbids a second EventLoop per process; the handle
        // was spawned once in `Self::new` and lives for the daemon's
        // lifetime. Per-session we just clone the handle and toggle
        // visibility via `set_state`.
        let overlay = self.overlay.read().ok().and_then(|g| g.clone());

        // ---- Build the pump + LiveSession ------------------------
        let mut pump = crate::live::Pump::new(fono_audio::StreamConfig::default());
        let frame_rx = pump.take_receiver().context("take live frame receiver")?;
        let language = match cfg.general.languages.as_slice() {
            [] => None,
            [single] => Some(single.clone()),
            _ => None,
        };
        let mut session =
            crate::live::LiveSession::new(streaming, sample_rate).with_language(language);
        if let Some(o) = overlay.as_ref() {
            session = session.with_overlay(o.clone());
        }
        let quality_floor = crate::live::parse_quality_floor(&cfg.interactive.quality_floor);

        // ---- Spawn the run task ----------------------------------
        let run_join = tokio::spawn(session.run(frame_rx, quality_floor));

        // ---- Bridge: realtime crossbeam rx → tokio mpsc ----------
        // A dedicated std::thread blocks on `audio_rx.recv()` and
        // forwards into a tokio unbounded mpsc; the tokio drain task
        // awaits that side. We avoid a `spawn_blocking` per recv
        // (which would defeat the latency win this thread is chasing)
        // by spending exactly one OS thread on the bridge.
        let (tokio_tx, mut tokio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<f32>>();
        let bridge_join = std::thread::Builder::new()
            .name("fono-live-bridge".into())
            .spawn(move || {
                while let Ok(chunk) = audio_rx.recv() {
                    if tokio_tx.send(chunk).is_err() {
                        break;
                    }
                }
                // audio_rx returned Err(Disconnected) — capture stream
                // has shut down; drop tokio_tx implicitly to signal
                // EOF to the drain task.
            })
            .context("spawn live-capture bridge thread")?;

        // ---- Spawn the drain task (tokio mpsc -> Pump::push) -----
        // Tap RMS off each chunk to feed the right-side VU bar on the
        // overlay panel. Cheap (one pass over already-resident PCM)
        // and keeps the pump / broadcast channel untouched. The
        // boolean snapshot is stable for the session — config reload
        // doesn't retroactively change running sessions.
        let vu_overlay = if cfg.overlay.volume_bar {
            overlay.clone()
        } else {
            None
        };
        let drain_join = tokio::spawn(async move {
            let mut pump = pump;
            while let Some(chunk) = tokio_rx.recv().await {
                if let Some(o) = vu_overlay.as_ref() {
                    o.push_level(normalised_rms(&chunk));
                }
                pump.push(&chunk);
            }
            pump.finish();
            // Drop pump explicitly so the broadcast sender side closes.
            drop(pump);
        });

        info!(
            "live-dictation started (mode={:?} sample_rate={})",
            mode, sample_rate
        );
        self.pipeline_in_flight.store(true, Ordering::SeqCst);
        *slot = Some(LiveCaptureSession {
            capture_stop_tx,
            capture_join: Some(capture_join),
            bridge_join: Some(bridge_join),
            drain_join,
            run_join,
            overlay,
            started_at: Instant::now(),
            mode,
        });
        Ok(())
    }

    /// Stop the active live-dictation session, await the streaming STT
    /// to drain, then commit the assembled transcript through the
    /// inject + history path. Mirrors [`Self::on_stop_recording`] in
    /// shape but with a pre-existing transcript text instead of a
    /// blob of PCM.
    #[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
    pub async fn on_stop_live_dictation(&self) {
        tracing::debug!("live dictation: stopping capture");
        let taken = self.live_capture.lock().await.take();
        let Some(mut session) = taken else {
            debug!("live-stop with no active live capture; checking batch fallback capture");
            self.on_stop_recording().await;
            return;
        };
        let cfg = self.current_config();
        if cfg.general.auto_mute_system {
            fono_audio::mute::set_default_sink_mute(false);
        }
        let elapsed = session.started_at.elapsed();
        let capture_ms = elapsed.as_millis() as u64;

        // Realtime push teardown order:
        //  0. Sleep `hold_release_grace_ms` so cpal's pending callback
        //     samples reach the audio bridge — without this, F8 release
        //     mid-pause drops the trailing word.
        //  1. Stop cpal capture — drops the forwarder closure (owned
        //     by the Stream), which drops the realtime SPSC `Sender`,
        //     signalling EOF to the bridge thread.
        //  2. Wait for the capture thread to exit (releases the
        //     audio device promptly).
        //  3. Wait for the bridge thread — it observes the disconnected
        //     `audio_rx` and drops the tokio mpsc Sender.
        //  4. Wait for the drain task — `tokio_rx.recv()` returns
        //     None, so it calls `pump.finish()` and exits.
        //  5. Existing post-drain logic (`run_join.await`, transcript
        //     handling) is unchanged.
        let grace_ms = u64::from(cfg.interactive.hold_release_grace_ms);
        if grace_ms > 0 {
            tracing::debug!("live dictation: stopping capture (grace={grace_ms}ms)");
            tokio::time::sleep(Duration::from_millis(grace_ms)).await;
        }
        let _ = session.capture_stop_tx.send(());
        if let Some(j) = session.capture_join.take() {
            let _ = tokio::task::spawn_blocking(move || {
                let _ = j.join();
            })
            .await;
        }
        if let Some(j) = session.bridge_join.take() {
            let _ = tokio::task::spawn_blocking(move || {
                let _ = j.join();
            })
            .await;
        }
        let _ = session.drain_join.await;
        // Await the streaming STT run-task. This is the only place
        // we hear about the final transcript.
        let transcript_res = session.run_join.await;
        // Keep the overlay visible — we'll switch it to Processing
        // during LLM cleanup and back to LiveDictating with the final
        // text just before injection so the user can actually read
        // what's about to be typed.
        let transcript = match transcript_res {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => {
                error!("live-dictation: streaming STT failed: {e:#}");
                if let Some(o) = session.overlay.as_ref() {
                    o.set_state(fono_overlay::OverlayState::Hidden);
                }
                self.pipeline_in_flight.store(false, Ordering::SeqCst);
                let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
                return;
            }
            Err(e) => {
                error!("live-dictation: run task join error: {e:#}");
                if let Some(o) = session.overlay.as_ref() {
                    o.set_state(fono_overlay::OverlayState::Hidden);
                }
                self.pipeline_in_flight.store(false, Ordering::SeqCst);
                let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
                return;
            }
        };

        let raw = transcript.committed.trim().to_string();
        if raw.is_empty() {
            warn!("live-dictation: empty transcript after {capture_ms} ms");
            // Empty-transcript microphone recovery hook (live path).
            // Mirror of the batch hook at the `raw.is_empty()` site
            // in `run_pipeline` — same dock-with-no-mic failure mode,
            // same toast. Plan v2 Phase 1.
            crate::audio_recovery::notify_empty_capture(capture_ms);
            if let Some(o) = session.overlay.as_ref() {
                o.set_state(fono_overlay::OverlayState::Hidden);
            }
            self.pipeline_in_flight.store(false, Ordering::SeqCst);
            let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
            return;
        }
        info!(
            "live-dictation: committed {} chars in {} ms ({} segments)",
            raw.chars().count(),
            capture_ms,
            transcript.segments_finalized
        );

        // Slice A simplification (documented inline): we do NOT pipe
        // the live-committed text through the full `run_pipeline`
        // function — that path expects raw PCM and runs the batch STT
        // a second time. Instead we run an optional LLM cleanup pass
        // and inject directly. History gets the raw + cleaned pair so
        // `fono history` and the tray's "Recent transcriptions" menu
        // surface live dictations identically to batch ones. The
        // boundary-heuristic flags from the LiveTranscript are
        // intentionally not persisted yet — Slice B telemetry work
        // adds dedicated columns.
        let llm_started = Instant::now();
        let mut llm_ms: u64 = 0;
        let mut llm_label_for_log: Option<String> = None;
        let cleaned = if cfg.interactive.cleanup_on_finalize {
            if let Some(llm) = self.current_llm() {
                // Show "polishing…" so the user knows we haven't
                // hung after the streaming ended.
                if let Some(o) = session.overlay.as_ref() {
                    o.set_state(fono_overlay::OverlayState::Processing);
                    o.update_text(raw.clone());
                }
                let (app_class, app_title) = self.focus.probe();
                let ctx =
                    build_format_context(&cfg, app_class.as_deref(), app_title.as_deref(), None);
                llm_label_for_log = Some(llm.name().to_string());
                match llm.format(&raw, &ctx).await {
                    Ok(c) => {
                        llm_ms = llm_started.elapsed().as_millis() as u64;
                        let trimmed = c.trim().to_string();
                        let raw_chars = raw.chars().count();
                        let new_chars = trimmed.chars().count();
                        // Mirror the batch pipeline's INFO logs at
                        // `session.rs:1253-1265` so live and batch produce
                        // structurally-identical operator output.
                        info!("llm: {} {}ms → {} chars", llm.name(), llm_ms, new_chars);
                        let diff = i64::try_from(new_chars).unwrap_or(0)
                            - i64::try_from(raw_chars).unwrap_or(0);
                        if trimmed == raw {
                            info!("llm: cleanup no-op (input unchanged, {raw_chars} chars)");
                        } else {
                            info!("llm: cleanup diff {raw_chars} → {new_chars} chars ({diff:+})");
                        }
                        if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed)
                        }
                    }
                    Err(e) => {
                        llm_ms = llm_started.elapsed().as_millis() as u64;
                        warn!("live-dictation: LLM cleanup failed after {llm_ms}ms: {e:#}");
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        let final_text = cleaned.as_deref().unwrap_or(&raw).to_string();

        // Show the user what we're about to inject. Switch back to
        // LiveDictating so they see the polished text in the same
        // visual lane as the streaming preview, with the cleaned
        // content. Held briefly *before* injection so the eye can
        // catch the final wording.
        if let Some(o) = session.overlay.as_ref() {
            o.set_state(fono_overlay::OverlayState::LiveDictating);
            o.update_text(final_text.clone());
        }

        // Inject — best-effort, same as the batch path.
        let inject_started = Instant::now();
        let injector = Arc::clone(&self.injector);
        let final_for_inject = final_text.clone();
        let clipboard_already_populated =
            tokio::task::spawn_blocking(move || injector.inject(&final_for_inject))
                .await
                .ok()
                .and_then(std::result::Result::ok)
                .unwrap_or(false);
        if cfg.general.also_copy_to_clipboard && !clipboard_already_populated {
            if let Err(e) = fono_inject::copy_to_clipboard(&final_text) {
                warn!("live-dictation: clipboard copy failed: {e:#}");
            }
        }
        let inject_ms = inject_started.elapsed().as_millis() as u64;

        // Mirror the batch summary at `session.rs:684-696` so live and
        // batch dictations produce structurally-identical operator
        // output. Live mode has no trim stage (streaming consumed PCM
        // continuously), so we omit the trim leg; everything else
        // matches.
        let raw_chars = raw.chars().count();
        let final_chars = final_text.chars().count();
        let llm_label = llm_label_for_log.as_deref().unwrap_or("none");
        info!(
            "pipeline ok (live): capture={}ms stt=streaming({} segments) llm={} {}ms inject={}ms ({} → {} chars)",
            capture_ms,
            transcript.segments_finalized,
            llm_label,
            llm_ms,
            inject_ms,
            raw_chars,
            final_chars,
        );

        // History (non-fatal on failure).
        if cfg.history.enabled {
            let stt_label = self.current_stt().name().to_string();
            let llm_label = if cleaned.is_some() {
                self.current_llm().map(|l| l.name().to_string())
            } else {
                None
            };
            let (app_class, app_title) = self.focus.probe();
            let row = HistoryRow {
                id: None,
                ts: now_unix(),
                duration_ms: Some(capture_ms as i64),
                raw: raw.clone(),
                cleaned: cleaned.clone(),
                app_class,
                app_title,
                stt_backend: Some(stt_label),
                llm_backend: llm_label,
                language: None,
            };
            let redact = cfg.history.redact_secrets;
            let db = self.history.lock().await;
            if let Err(e) = db.insert(&row, redact) {
                warn!("live-dictation: history insert failed: {e:#}");
            }
        }

        // Hold the final text on screen briefly so the user can read
        // it, then fade out. 1.2 s is long enough for a glance, short
        // enough not to feel sticky. The hold is on the orchestrator
        // task — sleeping here only delays `ProcessingDone` and the
        // hotkey FSM transition back to Idle, which is fine.
        if let Some(o) = session.overlay.as_ref() {
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            o.set_state(fono_overlay::OverlayState::Hidden);
        }

        self.pipeline_in_flight.store(false, Ordering::SeqCst);
        let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
#[allow(clippy::cognitive_complexity)]
async fn run_pipeline(
    pcm: Vec<f32>,
    sample_rate: u32,
    capture_ms: u64,
    stt: &dyn SpeechToText,
    llm: Option<&dyn TextFormatter>,
    history: &Arc<Mutex<HistoryDb>>,
    config: &Config,
    injector: &dyn Injector,
    focus: &dyn FocusProbe,
) -> PipelineOutcome {
    if pcm.is_empty() {
        return PipelineOutcome::EmptyOrTooShort {
            duration_ms: capture_ms,
        };
    }
    let mut metrics = PipelineMetrics {
        capture_ms,
        samples: pcm.len(),
        ..Default::default()
    };

    // ---- Trim leading/trailing silence (latency plan L11+L12) -------
    let pcm_for_stt: std::borrow::Cow<'_, [f32]> = if config.audio.trim_silence {
        let trim_started = Instant::now();
        let trim_cfg = fono_audio::TrimConfig {
            sample_rate,
            ..Default::default()
        };
        let (s, e) = fono_audio::trim_silence(&pcm, trim_cfg);
        metrics.trim_ms = trim_started.elapsed().as_millis() as u64;
        if s == 0 && e == pcm.len() {
            metrics.trimmed_samples = pcm.len();
            std::borrow::Cow::Borrowed(&pcm[..])
        } else {
            metrics.trimmed_samples = e - s;
            debug!(
                "trim: {} → {} samples in {} ms",
                pcm.len(),
                metrics.trimmed_samples,
                metrics.trim_ms
            );
            std::borrow::Cow::Owned(pcm[s..e].to_vec())
        }
    } else {
        metrics.trimmed_samples = pcm.len();
        std::borrow::Cow::Borrowed(&pcm[..])
    };

    // ---- STT ---------------------------------------------------------
    let stt_started = Instant::now();
    let lang = lang_for(config);
    let stt_result = stt
        .transcribe(&pcm_for_stt, sample_rate, lang.as_deref())
        .await;
    metrics.stt_ms = stt_started.elapsed().as_millis() as u64;
    let trans = match stt_result {
        Ok(t) => t,
        Err(e) => return PipelineOutcome::Failed(format!("STT {}: {e:#}", stt.name())),
    };
    let raw = trans.text.trim().to_string();
    metrics.raw_chars = raw.chars().count();
    if raw.is_empty() {
        warn!("STT returned empty text — nothing to inject");
        // Empty-transcript microphone recovery hook. When the user
        // held the hotkey for >= 5 s and the STT still produced
        // nothing, the most likely cause is a silent input device
        // (typically: an external dock with a passive capture
        // endpoint elected as the OS default source). Notify the
        // user and point at the tray Microphone submenu / `fono use
        // input` CLI. Plan v2 Phase 1.
        crate::audio_recovery::notify_empty_capture(capture_ms);
        return PipelineOutcome::EmptyOrTooShort {
            duration_ms: capture_ms,
        };
    }
    info!(
        "stt: {} {}ms → {} chars",
        stt.name(),
        metrics.stt_ms,
        metrics.raw_chars
    );

    // ---- LLM cleanup (optional) -------------------------------------
    let (app_class, app_title) = focus.probe();
    tracing::debug!(
        target: "fono::pipeline",
        "stt.raw lang={:?} app=({:?}, {:?}): {raw:?}",
        trans.language, app_class, app_title,
    );
    let word_count = raw.split_whitespace().count() as u32;
    let skip_short = config.llm.skip_if_words_lt > 0 && word_count < config.llm.skip_if_words_lt;
    let cleaned = if skip_short {
        // Latency plan L9 — short utterances rarely need cleanup;
        // skipping the LLM saves 150–800 ms.
        if llm.is_some() {
            info!(
                "llm: skipped (short utterance: {} word(s) < {})",
                word_count, config.llm.skip_if_words_lt
            );
            metrics.llm_skipped_short = true;
        }
        None
    } else if let Some(llm_backend) = llm {
        let ctx = build_format_context(
            config,
            app_class.as_deref(),
            app_title.as_deref(),
            trans.language.as_deref(),
        );
        tracing::debug!(
            target: "fono::pipeline",
            "llm.prompt main={:?} advanced={:?} dictionary={:?}",
            ctx.main_prompt, ctx.advanced_prompt, ctx.dictionary,
        );
        tracing::debug!(target: "fono::pipeline", "llm.input: {raw:?}");
        let llm_started = Instant::now();
        match llm_backend.format(&raw, &ctx).await {
            Ok(c) => {
                metrics.llm_ms = llm_started.elapsed().as_millis() as u64;
                let trimmed = c.trim().to_string();
                let raw_chars = raw.chars().count();
                let new_chars = trimmed.chars().count();
                info!(
                    "llm: {} {}ms → {} chars",
                    llm_backend.name(),
                    metrics.llm_ms,
                    new_chars
                );
                let diff =
                    i64::try_from(new_chars).unwrap_or(0) - i64::try_from(raw_chars).unwrap_or(0);
                if trimmed == raw {
                    info!("llm: cleanup no-op (input unchanged, {raw_chars} chars)");
                } else {
                    info!("llm: cleanup diff {raw_chars} → {new_chars} chars ({diff:+})");
                }
                tracing::debug!(target: "fono::pipeline", "llm.output: {trimmed:?}");
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            }
            Err(e) => {
                metrics.llm_ms = llm_started.elapsed().as_millis() as u64;
                warn!(
                    "llm: {} failed after {}ms: {e:#}",
                    llm_backend.name(),
                    metrics.llm_ms
                );
                None
            }
        }
    } else {
        None
    };

    let final_text = cleaned.as_deref().unwrap_or(&raw).to_string();
    metrics.final_chars = final_text.chars().count();

    // ---- Inject -----------------------------------------------------
    let inject_started = Instant::now();
    let clipboard_already_populated = match injector.inject(&final_text) {
        Ok(populated) => populated,
        Err(e) => {
            warn!("inject failed: {e:#}");
            false
        }
    };
    metrics.inject_ms = inject_started.elapsed().as_millis() as u64;
    debug!("inject: {}ms", metrics.inject_ms);
    tracing::debug!(target: "fono::pipeline", "inject.text: {final_text:?}");

    // ---- Belt-and-suspenders: also copy to clipboard --------------
    // KDE Wayland's KWin doesn't implement the wlroots virtual-keyboard
    // protocol that `wtype` uses, so wtype exits 0 but no keys reach
    // the focused window. Always also placing the cleaned text on the
    // system clipboard means the user can press Ctrl+V to recover even
    // when the inject silently no-op'd. Best-effort; never fatal.
    //
    // Skipped when the inject path itself already populated the
    // clipboard (`xtest-paste` backend, or the clipboard fallback) —
    // re-copying the same text would just duplicate log lines and
    // clipboard-manager history entries on every dictation.
    if config.general.also_copy_to_clipboard && !clipboard_already_populated {
        match fono_inject::copy_to_clipboard(&final_text) {
            Ok(tool) => {
                info!("clipboard: copied via {tool} (paste with Ctrl+V if inject didn't land)");
            }
            Err(e) => warn!("clipboard copy failed: {e:#}"),
        }
    }

    // ---- History (off the hot path; failure is non-fatal) -----------
    if config.history.enabled {
        let row = HistoryRow {
            id: None,
            ts: now_unix(),
            duration_ms: Some(capture_ms as i64),
            raw: raw.clone(),
            cleaned: cleaned.clone(),
            app_class,
            app_title,
            stt_backend: Some(stt.name().to_string()),
            llm_backend: llm.map(|l| l.name().to_string()),
            language: trans.language.clone(),
        };
        let redact = config.history.redact_secrets;
        let db = history.lock().await;
        if let Err(e) = db.insert(&row, redact) {
            warn!("history insert failed: {e:#}");
        }
    }

    PipelineOutcome::Completed {
        raw,
        cleaned,
        metrics,
    }
}

fn lang_for(config: &Config) -> Option<String> {
    match config.general.languages.as_slice() {
        [] => None,
        [single] => Some(single.clone()),
        _ => None,
    }
}

fn build_format_context(
    config: &Config,
    app_class: Option<&str>,
    app_title: Option<&str>,
    language: Option<&str>,
) -> FormatContext {
    let mut ctx = FormatContext {
        main_prompt: config.llm.prompt.main.clone(),
        advanced_prompt: config.llm.prompt.advanced.clone(),
        dictionary: config.llm.prompt.dictionary.clone(),
        rule_suffix: matched_rule_suffix(&config.context_rules, app_class, app_title),
        app_class: app_class.map(str::to_string),
        app_title: app_title.map(str::to_string),
        language: language.map(str::to_string),
    };
    // Trim trivially-empty fields so the system prompt stays compact.
    if ctx.advanced_prompt.trim().is_empty() {
        ctx.advanced_prompt.clear();
    }
    ctx
}

fn matched_rule_suffix(
    rules: &[ContextRule],
    app_class: Option<&str>,
    app_title: Option<&str>,
) -> Option<String> {
    for rule in rules {
        let class_ok = match (rule.match_.window_class.as_deref(), app_class) {
            (Some(want), Some(got)) => want.eq_ignore_ascii_case(got),
            (Some(_), None) => false,
            (None, _) => true,
        };
        let title_ok = match (rule.match_.window_title_regex.as_deref(), app_title) {
            (Some(re), Some(t)) => regex_lite_match(re, t),
            (Some(_), None) => false,
            (None, _) => true,
        };
        if class_ok && title_ok && !rule.prompt_suffix.trim().is_empty() {
            return Some(rule.prompt_suffix.clone());
        }
    }
    None
}

/// Minimal substring fallback matcher.  We keep `regex` out of `fono`
/// itself (it's already pulled in by `fono-core` for history); for v0.1
/// the simple `contains` semantics are sufficient and avoid a hot-path
/// dependency.
fn regex_lite_match(needle: &str, hay: &str) -> bool {
    hay.to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Helper used by `fono record` and the integration test to construct an
/// orchestrator backed by a single user-provided STT/LLM pair without
/// touching `Paths` / `Secrets`.
#[must_use]
pub fn orchestrator_for_test(
    stt: Arc<dyn SpeechToText>,
    llm: Option<Arc<dyn TextFormatter>>,
    history_path: &Path,
    config: Arc<Config>,
    injector: Arc<dyn Injector>,
    focus: Arc<dyn FocusProbe>,
) -> (SessionOrchestrator, mpsc::UnboundedReceiver<HotkeyAction>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let history = Arc::new(Mutex::new(
        HistoryDb::open(history_path).expect("open history db (test)"),
    ));
    let capture_cfg = CaptureConfig::default();
    let orch = SessionOrchestrator::with_parts(
        stt,
        llm,
        history,
        capture_cfg,
        config,
        tx,
        injector,
        focus,
    );
    (orch, rx)
}

/// Translate `[inject].paste_shortcut` into the `FONO_PASTE_SHORTCUT`
/// env var that `fono_inject::xtest_paste` reads at inject time. Logged
/// at `debug`; invalid configured shortcuts still warn loudly.
fn apply_paste_shortcut_env(config: &Config) {
    let raw = config.inject.paste_shortcut.trim();
    if raw.is_empty() {
        std::env::remove_var("FONO_PASTE_SHORTCUT");
        debug!("inject paste shortcut: default (Shift+Insert)");
        return;
    }
    // Validate so a typo surfaces as a warning instead of silently
    // falling back. `PasteShortcut` is re-exported from `fono-inject`
    // when its `x11-paste` feature is on (default).
    if fono_inject::PasteShortcut::parse(raw).is_none() {
        warn!(
            "[inject].paste_shortcut={raw:?} is not recognised; \
             xtest-paste will fall back to Shift+Insert"
        );
    }
    std::env::set_var("FONO_PASTE_SHORTCUT", raw);
    debug!("inject paste shortcut: {raw}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_auto_returns_none() {
        let mut c = Config::default();
        c.general.languages.clear();
        assert!(lang_for(&c).is_none());
        c.general.languages = vec!["ro".into()];
        assert_eq!(lang_for(&c).as_deref(), Some("ro"));
        c.general.languages = vec!["en".into(), "ro".into()];
        assert!(lang_for(&c).is_none());
    }

    #[test]
    fn rule_matches_class() {
        let mut c = Config::default();
        c.context_rules.push(ContextRule {
            match_: fono_core::config::ContextMatch {
                window_class: Some("Slack".into()),
                window_title_regex: None,
            },
            prompt_suffix: "use casual tone".into(),
        });
        assert_eq!(
            matched_rule_suffix(&c.context_rules, Some("slack"), None).as_deref(),
            Some("use casual tone"),
        );
        assert!(matched_rule_suffix(&c.context_rules, Some("Firefox"), None).is_none());
    }
}
