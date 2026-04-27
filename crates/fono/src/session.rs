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
}

impl CaptureSession {
    fn stop_and_drain(mut self) -> (Vec<f32>, Duration) {
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
    /// Stops the capture thread (drops the cpal stream).
    capture_stop_tx: std::sync::mpsc::Sender<()>,
    capture_join: Option<JoinHandle<()>>,
    /// Stops the drain task (signals it to call `pump.finish()`).
    drain_stop_tx: tokio::sync::oneshot::Sender<()>,
    /// JoinHandle for the drain task; awaited during shutdown so we
    /// don't tear down the pump before all captured PCM has been
    /// pushed.
    drain_join: tokio::task::JoinHandle<()>,
    /// JoinHandle for the [`crate::live::LiveSession::run`] task.
    run_join: tokio::task::JoinHandle<anyhow::Result<crate::live::LiveTranscript>>,
    /// Overlay handle (if available) — kept so we can hide the window
    /// when the session ends.
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
                let _ = notify_rust::Notification::new()
                    .summary("Fono — copied to clipboard")
                    .body(&format!(
                        "No key-injection backend was available. The cleaned text \
                         is on the clipboard (via {tool}); press Ctrl+V to paste."
                    ))
                    .timeout(notify_rust::Timeout::Milliseconds(6_000))
                    .show();
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
        let stt = fono_stt::build_stt(&config.stt, secrets, &paths.whisper_models_dir())
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
            input_device: config.audio.input_device.clone(),
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
            match fono_stt::build_streaming_stt(&config.stt, secrets, &paths.whisper_models_dir()) {
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
        let new_stt = fono_stt::build_stt(&cfg.stt, &secrets, &paths.whisper_models_dir())
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
            pipeline_in_flight: Arc::new(AtomicBool::new(false)),
            config: Arc::new(StdRwLock::new(config)),
            paths: None,
            action_tx,
            injector,
            focus,
        }
    }

    /// Begin recording. Refuses if a previous pipeline is still running.
    pub async fn on_start_recording(&self, mode: RecordingMode) -> Result<()> {
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
            "recording started (mode={:?} sample_rate={} device={:?})",
            mode, self.capture_cfg.target_sample_rate, self.capture_cfg.input_device
        );
        *slot = Some(CaptureSession {
            buffer,
            stop_tx,
            join: Some(join),
            started_at: Instant::now(),
            mode,
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
            input_device: self.capture_cfg.input_device.clone(),
            target_sample_rate: sample_rate,
        };

        // ---- Spawn the capture thread ----------------------------
        let (started_tx, started_rx) = std::sync::mpsc::channel::<
            std::result::Result<Arc<StdMutex<RecordingBuffer>>, String>,
        >();
        let (capture_stop_tx, capture_stop_rx) = std::sync::mpsc::channel::<()>();
        let cap_cfg_thread = cap_cfg.clone();
        let capture_join = std::thread::Builder::new()
            .name("fono-live-capture".into())
            .spawn(move || {
                let cap = AudioCapture::new(cap_cfg_thread);
                match cap.start() {
                    Ok(handle) => {
                        let _ = started_tx.send(Ok(Arc::clone(&handle.buffer)));
                        let _ = capture_stop_rx.recv();
                        drop(handle);
                    }
                    Err(e) => {
                        let _ = started_tx.send(Err(format!("{e:#}")));
                    }
                }
            })
            .context("spawn live-capture thread")?;
        let buffer = match started_rx.recv() {
            Ok(Ok(b)) => b,
            Ok(Err(e)) => {
                let _ = capture_join.join();
                return Err(anyhow::anyhow!("live audio capture failed to start: {e}"));
            }
            Err(_) => {
                return Err(anyhow::anyhow!(
                    "live capture thread died before reporting status"
                ))
            }
        };

        let cfg = self.current_config();
        if cfg.general.auto_mute_system {
            fono_audio::mute::set_default_sink_mute(true);
        }

        // ---- Spawn the overlay (best-effort) ---------------------
        let overlay = if cfg.interactive.overlay {
            match fono_overlay::RealOverlay::spawn() {
                Ok(h) => Some(h),
                Err(e) => {
                    warn!("live-dictation: overlay unavailable ({e:#}); continuing without it");
                    None
                }
            }
        } else {
            None
        };

        // ---- Build the pump + LiveSession ------------------------
        let mut pump = crate::live::Pump::new(fono_audio::StreamConfig::default());
        let frame_rx = pump.take_receiver().context("take live frame receiver")?;
        let language = if cfg.general.language == "auto" || cfg.general.language.is_empty() {
            None
        } else {
            Some(cfg.general.language.clone())
        };
        let mut session = crate::live::LiveSession::new(streaming, sample_rate)
            .with_language(language);
        if let Some(o) = overlay.as_ref() {
            session = session.with_overlay(o.clone());
        }
        let quality_floor = crate::live::parse_quality_floor(&cfg.interactive.quality_floor);

        // ---- Spawn the run task ----------------------------------
        let run_join = tokio::spawn(session.run(frame_rx, quality_floor));

        // ---- Spawn the drain task (cpal buffer -> Pump::push) -----
        let (drain_stop_tx, mut drain_stop_rx) = tokio::sync::oneshot::channel::<()>();
        let drain_join = tokio::spawn(async move {
            // Tracks how many samples we've already pushed so we only
            // forward newly-captured PCM. Polls every 30 ms — same
            // cadence the offline replay path in `fono record --live`
            // uses, well below whisper's preview interval.
            let mut offset: usize = 0;
            let mut interval = tokio::time::interval(Duration::from_millis(30));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut pump = pump;
            loop {
                tokio::select! {
                    _ = &mut drain_stop_rx => break,
                    _ = interval.tick() => {
                        let snapshot: Option<Vec<f32>> = {
                            buffer.lock().ok().and_then(|b| {
                                let s = b.samples();
                                if s.len() > offset {
                                    let new = s[offset..].to_vec();
                                    offset = s.len();
                                    Some(new)
                                } else {
                                    None
                                }
                            })
                        };
                        if let Some(chunk) = snapshot {
                            pump.push(&chunk);
                        }
                    }
                }
            }
            // Drain final tail before signalling EOF.
            if let Ok(b) = buffer.lock() {
                let s = b.samples();
                if s.len() > offset {
                    let tail = s[offset..].to_vec();
                    pump.push(&tail);
                }
            }
            pump.finish();
            // Drop pump explicitly so the broadcast sender side closes.
            drop(pump);
        });

        info!(
            "live-dictation started (mode={:?} sample_rate={} device={:?})",
            mode, sample_rate, cap_cfg.input_device
        );
        self.pipeline_in_flight.store(true, Ordering::SeqCst);
        *slot = Some(LiveCaptureSession {
            capture_stop_tx,
            capture_join: Some(capture_join),
            drain_stop_tx,
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
        let taken = self.live_capture.lock().await.take();
        let Some(mut session) = taken else {
            debug!("live-stop with no active live capture");
            let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
            return;
        };
        let cfg = self.current_config();
        if cfg.general.auto_mute_system {
            fono_audio::mute::set_default_sink_mute(false);
        }
        let elapsed = session.started_at.elapsed();
        let capture_ms = elapsed.as_millis() as u64;

        // 1. Tell the drain task to call pump.finish() and drop the pump.
        let _ = session.drain_stop_tx.send(());
        // 2. Stop cpal capture (frees the device promptly).
        let _ = session.capture_stop_tx.send(());
        if let Some(j) = session.capture_join.take() {
            let _ = tokio::task::spawn_blocking(move || {
                let _ = j.join();
            })
            .await;
        }
        // 3. Wait for the drain task itself to exit (it owns the pump).
        let _ = session.drain_join.await;
        // 4. Await the streaming STT run-task. This is the only place
        //    we hear about the final transcript.
        let transcript_res = session.run_join.await;
        if let Some(o) = session.overlay.as_ref() {
            o.set_state(fono_overlay::OverlayState::Hidden);
            o.shutdown();
        }
        let transcript = match transcript_res {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => {
                error!("live-dictation: streaming STT failed: {e:#}");
                self.pipeline_in_flight.store(false, Ordering::SeqCst);
                let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
                return;
            }
            Err(e) => {
                error!("live-dictation: run task join error: {e:#}");
                self.pipeline_in_flight.store(false, Ordering::SeqCst);
                let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
                return;
            }
        };

        let raw = transcript.committed.trim().to_string();
        if raw.is_empty() {
            warn!("live-dictation: empty transcript after {capture_ms} ms");
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
        let cleaned = if cfg.interactive.cleanup_on_finalize {
            if let Some(llm) = self.current_llm() {
                let (app_class, app_title) = self.focus.probe();
                let ctx = build_format_context(
                    &cfg,
                    app_class.as_deref(),
                    app_title.as_deref(),
                    None,
                );
                match llm.format(&raw, &ctx).await {
                    Ok(c) => {
                        let trimmed = c.trim().to_string();
                        if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed)
                        }
                    }
                    Err(e) => {
                        warn!("live-dictation: LLM cleanup failed: {e:#}");
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

        // Inject — best-effort, same as the batch path.
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

        if cfg.general.notify_on_dictation {
            let body = if final_text.chars().count() > 240 {
                let mut s: String = final_text.chars().take(240).collect();
                s.push('…');
                s
            } else {
                final_text.clone()
            };
            let _ = notify_rust::Notification::new()
                .summary("Fono — dictated (live)")
                .body(&body)
                .icon("audio-input-microphone")
                .timeout(notify_rust::Timeout::Milliseconds(4_000))
                .show();
        }

        // History (non-fatal on failure).
        if cfg.history.enabled {
            let stt_label = self
                .current_stt()
                .name()
                .to_string();
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
                info!(
                    "llm: {} {}ms → {} chars",
                    llm_backend.name(),
                    metrics.llm_ms,
                    trimmed.chars().count()
                );
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

    // ---- Desktop notification (always, when enabled) -----------------
    // Gives the user visible feedback even when injection silently
    // failed. Truncated to keep the toast short; the full text is in
    // the clipboard and history db.
    if config.general.notify_on_dictation {
        let body = if final_text.chars().count() > 240 {
            let mut s: String = final_text.chars().take(240).collect();
            s.push('…');
            s
        } else {
            final_text.clone()
        };
        let _ = notify_rust::Notification::new()
            .summary("Fono — dictated")
            .body(&body)
            .icon("audio-input-microphone")
            .timeout(notify_rust::Timeout::Milliseconds(4_000))
            .show();
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
    let l = &config.general.language;
    if l.is_empty() || l == "auto" {
        None
    } else {
        Some(l.clone())
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
        c.general.language = "auto".into();
        assert!(lang_for(&c).is_none());
        c.general.language = "ro".into();
        assert_eq!(lang_for(&c).as_deref(), Some("ro"));
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
