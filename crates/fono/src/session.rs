// SPDX-License-Identifier: GPL-3.0-only
//! End-to-end dictation orchestrator.
//!
//! Owns the active capture stream, the STT/polish backends, and the
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
use fono_assistant::{Assistant, ConversationHistory};
use fono_audio::{
    AudioCapture, CaptureConfig, EnvelopeConfig, EnvelopeFollower, RecordingBuffer, SilenceEvent,
    SilenceWatch, SilenceWatchConfig,
};
use fono_core::config::{Config, ContextRule};
use fono_core::history::{HistoryDb, Transcription as HistoryRow};
use fono_core::{Paths, Secrets};
use fono_hotkey::{HotkeyAction, RecordingMode};
use fono_polish::{FormatContext, TextFormatter};
use fono_stt::SpeechToText;
#[cfg(feature = "interactive")]
use fono_stt::StreamingStt;
use fono_tts::TextToSpeech;
use std::sync::Mutex as StdMutex;
use std::thread::JoinHandle;
use tokio::sync::{mpsc, Mutex, Notify};
use tracing::{debug, error, info, warn};

use crate::assistant::{run_assistant_turn, AssistantSessionState, AssistantTurnInputs};

/// Minimum duration of audio that will be passed to STT. Anything
/// shorter is treated as a misfire.
pub const MIN_RECORDING: Duration = Duration::from_millis(300);

/// Amplitude that maps to "full" on every audio-driven visualisation
/// — RMS for bars + the live-dictation VU bar, peak amplitude for the
/// oscilloscope. 0.22 is the value that looks balanced across all
/// three at typical speaking-voice levels.
#[cfg(feature = "interactive")]
const WAVEFORM_AMPLITUDE_CEILING: f32 = 0.22;

/// Which capture pipeline the silence-watch task is driving. Changes
/// the overlay states it emits, which hold-flag it consults, and
/// which (if any) [`HotkeyAction`] it fires on `Committed`.
#[cfg(feature = "interactive")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SilenceWatchFlavor {
    /// F7 / dictation: red `Recording` ↔ `Pondering` overlay states,
    /// dictation hold-flag, `TogglePressed` on commit.
    Dictation,
    /// F8 / assistant: green `AssistantRecording` ↔
    /// `AssistantPondering` overlay states, assistant hold-flag,
    /// `AssistantPressed` on commit (only when triggered by toggle
    /// — hold-to-talk gets the visual but not the commit).
    Assistant {
        /// `true` when the assistant session was started by a
        /// toggle press; the auto-stop commit is gated on this so
        /// hold-to-talk users own the release boundary themselves.
        auto_stop_commit: bool,
    },
}

/// Pre-computed `10^(-12 dB / 20) ≈ 0.2512`. Used by
/// `spawn_silence_watch_task` to derive the silence-threshold tick
/// (Advanced VU bar) from the envelope's `voiced_rms`. Mirrors
/// `SilenceWatchConfig::default().silence_gap_db = 12`.
#[cfg(feature = "interactive")]
const SILENCE_GAIN: f32 = 0.251_188_64;

/// FFT window size used by the `fft` and `heatmap` styles. 4096
/// samples ≈ 256 ms at 16 kHz — gives ~3.9 Hz per source bin so
/// 512 display bins across 0–3 kHz still average 1–2 source bins
/// each.
#[cfg(feature = "interactive")]
const WAVEFORM_FFT_SIZE: usize = 4096;

/// Upper frequency cutoff for the FFT visualisations. Most voice
/// intelligibility (fundamentals + first three formants) sits below
/// 3 kHz — anything higher is sibilance or background noise that
/// clutters the view.
#[cfg(feature = "interactive")]
const WAVEFORM_FFT_MAX_HZ: f32 = 3000.0;

/// Target display-bin count pushed to the overlay per frame. The
/// ticker maps each display bin to a `[start, end)` slice of the
/// source spectrum via integer multiply-divide, so non-integer
/// source-to-display ratios distribute cleanly without rounding all
/// the way down to a single source bin per display. 300 bars across
/// the ~588 px content area lands each at ≈2 px wide.
#[cfg(feature = "interactive")]
const WAVEFORM_FFT_BINS: usize = 300;

/// dB range mapped to `[0.0, 1.0]` on the FFT / heatmap. Bins
/// quieter than the floor read as 0 (so background noise doesn't
/// light up the visualisation); louder than the ceiling saturate.
/// −20 dB floor keeps room noise / breathing dark; +30 dB ceiling
/// reserves the top of the scale for vowel peaks.
#[cfg(feature = "interactive")]
const WAVEFORM_FFT_DB_FLOOR: f32 = -20.0;
#[cfg(feature = "interactive")]
const WAVEFORM_FFT_DB_CEILING: f32 = 30.0;

/// Compute RMS of an f32 slice and normalise against
/// [`WAVEFORM_AMPLITUDE_CEILING`]. Result is clamped to `[0.0, 1.0]`.
#[cfg(feature = "interactive")]
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
    /// AbortHandle for the audio-level ticker that feeds the standalone
    /// waveform overlay. `None` when no overlay is attached.
    #[cfg(feature = "interactive")]
    level_task: Option<tokio::task::AbortHandle>,
    /// AbortHandle for the silence-watch task driving the
    /// `Recording ↔ Pondering` overlay transitions. Only spawned in
    /// toggle dictation mode (slice 2 of the auto-stop plan). `None`
    /// in hold-to-talk mode, when no overlay is attached, or when
    /// the waveform overlay is disabled.
    #[cfg(feature = "interactive")]
    silence_task: Option<tokio::task::AbortHandle>,
}

impl CaptureSession {
    fn stop_and_drain(mut self) -> (Vec<f32>, Duration) {
        #[cfg(feature = "interactive")]
        if let Some(h) = self.level_task.take() {
            h.abort();
        }
        #[cfg(feature = "interactive")]
        if let Some(h) = self.silence_task.take() {
            h.abort();
        }
        let _ = self.stop_tx.send(());
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
        let elapsed = self.started_at.elapsed();
        let pcm = self.buffer.lock().map(|b| b.samples().to_vec()).unwrap_or_default();
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
    /// Silence-watch task driving Pondering overlay transitions (and,
    /// for assistant toggle sessions, the auto-stop commit). `None`
    /// when the overlay is disabled. See
    /// `plans/2026-05-22-assistant-pondering-parity-v1.md`.
    silence_task: Option<tokio::task::AbortHandle>,
    /// Overlay handle (clone of [`SessionOrchestrator::overlay`]) — kept
    /// so we can hide the window when the session ends. The handle is
    /// owned by the orchestrator and reused across sessions; this
    /// field is just a clone for convenience.
    overlay: Option<fono_overlay::OverlayHandle>,
    started_at: Instant,
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
    /// `Polish.skip_if_words_lt` words. Latency plan L9.
    pub llm_skipped_short: bool,
}

/// Outcome of one full dictation pipeline run, returned by the inner
/// pipeline task and consumed by the daemon for tray + tracing.
#[derive(Debug, Clone)]
pub enum PipelineOutcome {
    /// Successfully transcribed and (optionally) cleaned + injected text.
    Completed { raw: String, cleaned: Option<String>, metrics: PipelineMetrics },
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
/// system clipboard with the final text (e.g. the clipboard fallback
/// path when no key-injector worked). The orchestrator uses that
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

/// One-shot guard for the "text copied to clipboard, press Ctrl+V" hint.
/// Flipped to `true` after the first clipboard fallback so subsequent
/// dictations don't spam the user with a notification per utterance.
static CLIPBOARD_HINT_SHOWN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

impl Injector for RealInjector {
    fn inject(&self, text: &str) -> Result<bool> {
        match fono_inject::type_text_with_outcome(text)? {
            fono_inject::InjectOutcome::Typed(backend) => {
                tracing::info!("inject backend: typed via {backend}");
                // All current key-injection backends (wtype/ydotool/
                // xdotool/enigo/xtest-type) deliver keystrokes directly
                // and leave the clipboard untouched, so the orchestrator
                // should still run `also_copy_to_clipboard` if enabled.
                Ok(false)
            }
            fono_inject::InjectOutcome::Clipboard(tool) => {
                tracing::info!("inject backend: clipboard via {tool} (no key-injection worked)");
                // Surface the "press Ctrl+V to paste" hint only once
                // per daemon process. On Wayland without an active
                // virtual-keyboard / RemoteDesktop session this is
                // the steady state (every dictation falls back to
                // clipboard) and firing a notification on every
                // utterance is intolerably noisy. Doctor and the
                // tray cover the persistent-state surface.
                if !CLIPBOARD_HINT_SHOWN.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    let _ = tool;
                    fono_core::notify::send(
                        "Fono — text copied",
                        "Press Ctrl+V or Shift+Insert to paste.",
                        "edit-paste",
                        4_000,
                        fono_core::notify::Urgency::Normal,
                    );
                }
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
/// `stt`, `polish`, and `config` live behind `RwLock<Arc<…>>` so the
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
    polish: Arc<StdRwLock<Option<Arc<dyn TextFormatter>>>>,
    /// TTS backend for the assistant's audio reply path. `None` when
    /// `[tts].backend = none` or the factory failed.
    tts: Arc<StdRwLock<Option<Arc<dyn TextToSpeech>>>>,
    /// Streaming chat backend for the assistant. `None` when
    /// `[assistant]` is disabled or the factory failed.
    assistant_backend: Arc<StdRwLock<Option<Arc<dyn Assistant>>>>,
    /// Per-orchestrator assistant runtime state: rolling history,
    /// cancellation `Notify`, and lazy [`fono_audio::AudioPlayback`]
    /// handle. Shared with the pump task spawned in
    /// [`Self::on_assistant_hold_release`].
    assistant_session: Arc<Mutex<AssistantSessionState>>,
    /// Capture slot dedicated to the assistant push-to-talk path.
    /// Independent of the dictation [`Self::capture`] slot so the two
    /// pipelines can never trample each other (and so a future where
    /// they overlap becomes a config decision rather than a state
    /// hazard).
    assistant_capture: Arc<Mutex<Option<CaptureSession>>>,
    history: Arc<Mutex<HistoryDb>>,
    capture_cfg: CaptureConfig,
    capture: Arc<Mutex<Option<CaptureSession>>>,
    /// Active live-dictation session, parallel to [`Self::capture`] but
    /// holding the streaming pump + run-task instead of the batch
    /// recorder. Wiring fix follow-up to Slice A v7.
    #[cfg(feature = "interactive")]
    live_capture: Arc<Mutex<Option<LiveCaptureSession>>>,
    /// Streaming-STT-backed capture for the assistant push-to-talk
    /// path. Only used when `[interactive].enabled = true` and a
    /// streaming-capable STT backend is loaded; otherwise the batch
    /// path through [`Self::assistant_capture`] is used. Sharing the
    /// `LiveCaptureSession` shape with the dictation slot keeps the
    /// teardown logic uniform — only the press/release wrappers and
    /// the post-stop transcript routing differ.
    #[cfg(feature = "interactive")]
    assistant_live_capture: Arc<Mutex<Option<LiveCaptureSession>>>,
    /// Long-lived overlay handle, spawned **once** at orchestrator
    /// construction and reused across every live-dictation and batch
    /// recording session. winit refuses to construct a second
    /// `EventLoop` in the same process, so we MUST keep this alive
    /// for the daemon's lifetime rather than spawning per session.
    /// `None` means the overlay is disabled in config or failed to
    /// spawn at startup.
    #[cfg(feature = "interactive")]
    overlay: Arc<StdRwLock<Option<fono_overlay::OverlayHandle>>>,
    pipeline_in_flight: Arc<AtomicBool>,
    config: Arc<StdRwLock<Arc<Config>>>,
    /// Resolved XDG paths; used by [`Self::reload`] to re-read config
    /// + secrets from disk.
    paths: Option<Arc<Paths>>,
    action_tx: mpsc::UnboundedSender<HotkeyAction>,
    injector: Arc<dyn Injector>,
    focus: Arc<dyn FocusProbe>,
    /// Per-role key-held flags shared with the hotkey listener. The
    /// silence-watch task reads `dictation` / `assistant` to suppress
    /// the `Pondering` overlay flip (and any auto-stop commit) while
    /// the user is physically holding the dictation/assistant key down
    /// — see `fono_hotkey::KeyHeldFlags`. Populated by [`Self::new`]
    /// from the value passed by `daemon::run`; [`Self::with_parts`]
    /// leaves it at `KeyHeldFlags::default()` for IPC / test callers.
    held_flags: fono_hotkey::KeyHeldFlags,
}

impl SessionOrchestrator {
    /// Construct from a fresh config + secrets, building both backends.
    /// Returns an error if the STT factory fails — the daemon should
    /// still come up but in a "degraded" mode where hotkeys notify the
    /// user. LLM construction failure downgrades to "no cleanup".
    #[allow(clippy::too_many_lines)]
    pub fn new(
        config: Arc<Config>,
        secrets: &Secrets,
        paths: &Paths,
        action_tx: mpsc::UnboundedSender<HotkeyAction>,
        held_flags: fono_hotkey::KeyHeldFlags,
    ) -> Result<Self> {
        let stt =
            fono_stt::build_stt(&config.stt, &config.general, secrets, &paths.whisper_models_dir())
                .context("build STT backend")?;
        let polish =
            match fono_polish::build_polish(&config.polish, secrets, &paths.polish_models_dir()) {
                Ok(opt) => opt,
                Err(e) => {
                    warn!("polish backend unavailable; continuing without cleanup: {e:#}");
                    None
                }
            };
        let tts = match fono_tts::build_tts(&config.tts, secrets, &config.general.languages) {
            Ok(opt) => opt,
            Err(e) => {
                warn!("TTS backend unavailable; assistant replies will be silent: {e:#}");
                None
            }
        };
        let assistant_backend = match fono_assistant::build_assistant(&config.assistant, secrets) {
            Ok(opt) => opt,
            Err(e) => {
                warn!("Assistant backend unavailable; F8 will notify but not respond: {e:#}");
                None
            }
        };
        let history =
            Arc::new(Mutex::new(HistoryDb::open(&paths.history_db()).context("open history db")?));
        let capture_cfg = CaptureConfig { target_sample_rate: config.audio.sample_rate };
        let mut orch = Self::with_parts(
            stt,
            polish,
            history,
            capture_cfg,
            Arc::clone(&config),
            action_tx,
            Arc::new(RealInjector),
            Arc::new(RealFocusProbe),
        );
        orch.paths = Some(Arc::new(paths.clone()));
        orch.held_flags = held_flags;
        // Populate the assistant-side slots. Both are optional —
        // F8 surfaces a notification when either is missing.
        if let Ok(mut g) = orch.tts.write() {
            *g = tts;
        }
        if let Ok(mut g) = orch.assistant_backend.write() {
            *g = assistant_backend;
        }
        // Populate the streaming-STT slot when this build supports
        // interactive mode. Errors are non-fatal — the live path
        // gracefully falls back to batch when the slot is `None`.
        #[cfg(feature = "interactive")]
        {
            match fono_stt::build_streaming_stt(
                &config.stt,
                &config.general,
                config.live_preview(),
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
        // One spawn for the lifetime of the daemon, parameterised by
        // the chosen `WaveformStyle` (Transcript renders the streaming
        // live-preview text panel; the other four styles render their
        // audio visualisations). Style swaps after startup are pushed
        // via `set_waveform_style` from `reload()`.
        #[cfg(feature = "interactive")]
        {
            let spawn_result: Option<std::io::Result<fono_overlay::OverlayHandle>> = {
                if config.overlay.waveform {
                    Some(fono_overlay::RealOverlay::spawn(config.overlay.style))
                } else {
                    None
                }
            };
            match spawn_result {
                Some(Ok(h)) => {
                    h.set_volume_bar(config.overlay.volume_bar);
                    // Surface a clear notification + WARN when the
                    // overlay landed on the noop terminal sink in a
                    // graphical session — the user asked for an
                    // on-screen indicator but won't see one. Common
                    // cause on Ubuntu/X11/Xwayland is a missing
                    // `libxkbcommon-x11`; `fono install` offers to
                    // install it.
                    if h.backend_id() == fono_overlay::BackendId::Noop
                        && (std::env::var_os("DISPLAY").is_some()
                            || std::env::var_os("WAYLAND_DISPLAY").is_some())
                    {
                        warn!(
                            "overlay: no usable backend on this graphical session — \
                             on-screen recording indicator will not be shown. On X11 / \
                             Xwayland this usually means `libxkbcommon-x11` is missing; \
                             on Wayland it means the compositor doesn't speak \
                             `wlr-layer-shell`. Run `fono install` (it offers to install \
                             `libxkbcommon-x11`), or run `fono doctor` for details."
                        );
                        fono_core::notify::send(
                            "Fono — overlay disabled",
                            "No on-screen recording indicator available on this session. \
                             Install libxkbcommon-x11 (X11/Xwayland) or use a wlr-layer-shell \
                             Wayland compositor. Dictation still works.",
                            "dialog-warning",
                            6_000,
                            fono_core::notify::Urgency::Normal,
                        );
                    }
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
    #[allow(clippy::too_many_lines)]
    pub async fn reload(&self) -> Result<String> {
        let paths = self
            .paths
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("orchestrator built without Paths; cannot reload"))?
            .clone();
        let cfg = Config::load(&paths.config_file()).context("reload: read config")?;
        let secrets = Secrets::load(&paths.secrets_file()).context("reload: read secrets")?;
        // Re-apply `[inject].backend` so `fono use inject …` takes
        // effect without a daemon restart. Idempotent on the `"auto"`
        // default path; see `daemon::apply_inject_backend_env`.
        crate::daemon::apply_inject_backend_env(&cfg.inject);
        let new_stt =
            fono_stt::build_stt(&cfg.stt, &cfg.general, &secrets, &paths.whisper_models_dir())
                .context("reload: build STT")?;
        let new_polish = match fono_polish::build_polish(
            &cfg.polish,
            &secrets,
            &paths.polish_models_dir(),
        ) {
            Ok(opt) => opt,
            Err(e) => {
                let err_text = format!("{e:#}");
                let provider = fono_core::providers::polish_backend_str(&cfg.polish.backend);
                fono_core::critical_notify::notify_actionable(
                    fono_core::critical_notify::Stage::Polish,
                    provider,
                    &err_text,
                );
                warn!("reload: polish backend unavailable; continuing without cleanup: {err_text}");
                None
            }
        };
        let new_tts = match fono_tts::build_tts(&cfg.tts, &secrets, &cfg.general.languages) {
            Ok(opt) => opt,
            Err(e) => {
                let err_text = format!("{e:#}");
                let provider = fono_core::providers::tts_backend_str(&cfg.tts.backend);
                fono_core::critical_notify::notify_actionable(
                    fono_core::critical_notify::Stage::Tts,
                    provider,
                    &err_text,
                );
                warn!("reload: TTS unavailable; assistant replies will be silent: {err_text}");
                None
            }
        };
        let new_assistant = match fono_assistant::build_assistant(&cfg.assistant, &secrets) {
            Ok(opt) => opt,
            Err(e) => {
                let err_text = format!("{e:#}");
                let provider = fono_core::providers::assistant_backend_str(&cfg.assistant.backend);
                fono_core::critical_notify::notify_actionable(
                    fono_core::critical_notify::Stage::Assistant,
                    provider,
                    &err_text,
                );
                warn!("reload: assistant unavailable: {err_text}");
                None
            }
        };
        let stt_name = new_stt.name().to_string();
        let llm_name =
            new_polish.as_ref().map_or_else(|| "none".to_string(), |l| l.name().to_string());
        // Lock-write order matches read order in the hot path.
        if let Ok(mut guard) = self.stt.write() {
            *guard = new_stt;
        }
        #[cfg(feature = "interactive")]
        {
            let new_streaming = match fono_stt::build_streaming_stt(
                &cfg.stt,
                &cfg.general,
                cfg.live_preview(),
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
        if let Ok(mut guard) = self.polish.write() {
            *guard = new_polish;
        }
        if let Ok(mut guard) = self.tts.write() {
            *guard = new_tts;
        }
        if let Ok(mut guard) = self.assistant_backend.write() {
            *guard = new_assistant;
        }
        // Re-tune the rolling history window to match the freshly-
        // loaded config.
        {
            let window = Duration::from_secs(60 * u64::from(cfg.assistant.history_window_minutes));
            let max_turns = cfg.assistant.history_max_turns as usize;
            let mut s = self.assistant_session.lock().await;
            s.history = ConversationHistory::new(window, max_turns);
        }
        // Push the (possibly changed) overlay style to the long-
        // lived handle. The overlay is spawned **once** at startup
        // (winit forbids a second EventLoop per process) so we
        // hot-swap between Transcript / Bars / Oscilloscope / FFT /
        // Heatmap via `SetWaveformStyle`. The call is idempotent
        // when nothing changed.
        #[cfg(feature = "interactive")]
        if let Some(o) = self.overlay.read().ok().and_then(|g| g.clone()) {
            o.set_waveform_style(cfg.overlay.style);
            o.set_volume_bar(cfg.overlay.volume_bar);
        }
        if let Ok(mut guard) = self.config.write() {
            *guard = Arc::new(cfg);
        }
        // Re-prewarm the new backends so the first post-switch
        // dictation isn't cold (latency plan L3 still applies).
        self.spawn_warmups();
        info!("reloaded: stt={stt_name} polish={llm_name}");
        Ok(format!("active: stt={stt_name} polish={llm_name}"))
    }

    /// Read-only snapshot of the active backend names. Returns the
    /// **canonical** lowercase identifier from
    /// [`fono_core::providers::stt_backend_str`] /
    /// [`fono_core::providers::polish_backend_str`] (e.g. `"local"`,
    /// `"groq"`, `"none"`) so the tray's active-marker comparison and
    /// the doctor / status output stay in sync. The trait `name()`s
    /// (e.g. `"whisper-local"`, `"llama-local"`) are intentionally
    /// **not** used here — they're an implementation detail.
    #[must_use]
    pub fn active_backends(&self) -> (String, String) {
        let cfg = self.current_config();
        let stt = fono_core::providers::stt_backend_str(&cfg.stt.backend).to_string();
        let polish = fono_core::providers::polish_backend_str(&cfg.polish.backend).to_string();
        (stt, polish)
    }

    /// Same as [`Self::active_backends`] but also reports the
    /// assistant + TTS backend identifiers. The tray's active-marker
    /// uses this; doctor / status output keep using the 2-tuple.
    #[must_use]
    pub fn active_backends_full(&self) -> (String, String, String, String) {
        let cfg = self.current_config();
        let stt = fono_core::providers::stt_backend_str(&cfg.stt.backend).to_string();
        let polish = fono_core::providers::polish_backend_str(&cfg.polish.backend).to_string();
        let assistant =
            fono_core::providers::assistant_backend_str(&cfg.assistant.backend).to_string();
        let tts = fono_core::providers::tts_backend_str(&cfg.tts.backend).to_string();
        (stt, polish, assistant, tts)
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
        self.polish.read().expect("polish lock poisoned").clone()
    }

    fn current_config(&self) -> Arc<Config> {
        Arc::clone(&self.config.read().expect("config lock poisoned"))
    }

    /// Snapshot of the post-reload [`Config::live_preview`] flag. The
    /// daemon's hotkey dispatcher and IPC client handler read this on
    /// every action so a tray-triggered switch into Transcript style
    /// (which calls `reload()` and updates `self.config`) takes effect
    /// on the very next F7 press — without this, those paths captured
    /// the startup `Arc<Config>` and kept routing to the batch
    /// pipeline even after the user picked Transcript, suppressing
    /// the live overlay.
    #[must_use]
    pub fn live_preview(&self) -> bool {
        self.current_config().live_preview()
    }

    /// Spawn the standalone-waveform overlay's level ticker, set the
    /// overlay state to `initial_state`, and return an
    /// [`tokio::task::AbortHandle`] the caller stows in its
    /// [`CaptureSession`] so the ticker stops when capture ends.
    /// Returns `None` when the overlay is disabled in config or the
    /// daemon failed to spawn the overlay handle at startup.
    ///
    /// Shared between the dictation path (`OverlayState::Recording`,
    /// red palette) and the assistant path
    /// (`OverlayState::AssistantRecording`, green palette) so both
    /// pipelines get the same Bars / FFT / Heatmap / Oscilloscope
    /// visualisations the user picked in `[overlay].style`.
    #[cfg(feature = "interactive")]
    #[allow(clippy::too_many_lines, clippy::suboptimal_flops)]
    fn spawn_waveform_level_task(
        &self,
        cfg: &Config,
        initial_state: fono_overlay::OverlayState,
        buffer: &Arc<StdMutex<RecordingBuffer>>,
    ) -> Option<tokio::task::AbortHandle> {
        // The live-preview gate only applies to the batch dictation
        // overlay — when the user picks Transcript style, F7 takes
        // the streaming path and the dictation panel is handled by
        // `LiveSession`. The assistant pipeline is independent and
        // should always show its overlay regardless of style
        // (different state, different colour, different label).
        let is_assistant =
            matches!(initial_state, fono_overlay::OverlayState::AssistantRecording { .. });
        let want_waveform = cfg.overlay.waveform && (is_assistant || !cfg.live_preview());
        let handle = self.overlay.read().ok().and_then(|g| g.clone());
        match (want_waveform, handle) {
            (true, Some(o)) => {
                o.set_state(initial_state);
                let style = cfg.overlay.style;
                let buf = Arc::clone(buffer);
                let sample_rate = self.capture_cfg.target_sample_rate;
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
                                    // Push raw RMS (pre-gain) for the VU bar.
                                    // `push_samples` above is *gained* for the
                                    // oscilloscope display; the bar wants the
                                    // true amplitude so its normalisation
                                    // against `WAVEFORM_AMPLITUDE_CEILING` is
                                    // consistent across styles.
                                    let inv_gain = 1.0 / gain;
                                    let rms = {
                                        let sum_sq: f32 =
                                            snap.iter().map(|v| (v * inv_gain).powi(2)).sum();
                                        (sum_sq / snap.len() as f32).sqrt()
                                    };
                                    o.push_level(
                                        (rms / WAVEFORM_AMPLITUDE_CEILING).clamp(0.0, 1.0),
                                    );
                                    o.push_samples(snap);
                                }
                            }
                        }
                        fono_core::config::WaveformStyle::Fft
                        | fono_core::config::WaveformStyle::Heatmap => {
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
                            let max_source_bin = ((WAVEFORM_FFT_MAX_HZ * WAVEFORM_FFT_SIZE as f32)
                                / sample_rate as f32)
                                as usize;
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
                                    *slot =
                                        ((db - WAVEFORM_FFT_DB_FLOOR) / db_span).clamp(0.0, 1.0);
                                }
                                // Also push a level for the VU bar, derived
                                // from the same windowed samples the FFT
                                // consumed. Reuses the windowed `input_buf`
                                // (already populated above with the last
                                // `take` samples * Hann window), un-windowing
                                // would be expensive so we accept the slight
                                // Hann-energy bias (≈3 dB low) — the bar is
                                // an indicator, not a measurement.
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
                        // Transcript renders the streaming-preview text
                        // panel, not an audio visualisation. The
                        // `want_waveform` gate excludes this branch
                        // when style == Transcript, but the match
                        // must remain exhaustive.
                        fono_core::config::WaveformStyle::Transcript => {}
                    }
                });
                Some(task.abort_handle())
            }
            _ => None,
        }
    }

    /// Drive the silence-watch state machine from the live capture
    /// buffer. Pushes overlay state transitions
    /// `Recording ↔ Pondering` and updates `walk_progress` so the
    /// "Pondering…" label's walking-letter highlight advances in
    /// step with the configured `auto_stop_silence_ms`.
    ///
    /// Slice 2 of `plans/2026-05-22-fono-auto-stop-silence-v1.md` —
    /// visual feedback only; the auto-stop *commit* lands in slice 4.
    /// Returns `None` when the overlay is disabled / failed to
    /// spawn — the silence watchdog has nothing to drive without an
    /// overlay handle (slice 4 will revisit this when the commit
    /// path lands).
    #[cfg(feature = "interactive")]
    fn spawn_silence_watch_task(
        &self,
        cfg: &Config,
        buffer: &Arc<StdMutex<RecordingBuffer>>,
    ) -> Option<tokio::task::AbortHandle> {
        self.spawn_silence_watch_task_for(cfg, buffer, SilenceWatchFlavor::Dictation)
    }

    /// Flavoured variant of [`Self::spawn_silence_watch_task`] —
    /// drives the silence watch for either the dictation pipeline
    /// (red `Recording`/`Pondering` overlay states, dictation
    /// hold-flag, optional `TogglePressed` auto-stop) or the
    /// assistant pipeline (green `AssistantRecording`/
    /// `AssistantPondering` states, assistant hold-flag, optional
    /// `AssistantPressed` auto-stop). See
    /// `plans/2026-05-22-assistant-pondering-parity-v1.md`.
    #[cfg(feature = "interactive")]
    #[allow(clippy::too_many_lines)]
    fn spawn_silence_watch_task_for(
        &self,
        cfg: &Config,
        buffer: &Arc<StdMutex<RecordingBuffer>>,
        flavor: SilenceWatchFlavor,
    ) -> Option<tokio::task::AbortHandle> {
        let overlay = self.overlay.read().ok().and_then(|g| g.clone())?;
        let buf = Arc::clone(buffer);
        let sample_rate = self.capture_cfg.target_sample_rate;
        let auto_stop_ms = cfg.audio.auto_stop_silence_ms;
        let envelope_cfg = EnvelopeConfig { sample_rate, ..EnvelopeConfig::default() };
        // Slice 4: wire the commit timer. Zero means "no auto-stop";
        // the watch still drives the Pondering label so the user
        // sees the state machine even with the feature off, which
        // is what dogfooding from slice 2 already depended on.
        let watch_cfg = SilenceWatchConfig {
            auto_stop_silence_ms: if auto_stop_ms > 0 { Some(auto_stop_ms) } else { None },
            ..SilenceWatchConfig::default()
        };
        let pondering_visual_ms = watch_cfg.pondering_visual_ms;
        // Slice 2 visual default: when the user has auto-stop off
        // (the shipping default), still walk the highlight across
        // a 5 s "what auto-stop *would* feel like" window so the
        // dogfooding signal is meaningful. When auto-stop is set
        // the walk matches the real timer.
        let walk_total_ms = if auto_stop_ms > 0 { auto_stop_ms } else { 5_000 };
        let action_tx = self.action_tx.clone();
        // Push-to-talk suppression: while the user is physically
        // holding the relevant key down, the `Pondering` overlay
        // flip and any auto-stop commit are skipped. The hotkey
        // listener collapses hold-vs-toggle into `RecordingMode::Toggle`
        // at action-emit time (every keyboard press enters Toggle and
        // a long release synthesises a second `TogglePressed`), so the
        // mode argument can't be trusted here — this flag is the only
        // authoritative source of "is the key currently held?". See
        // `fono_hotkey::KeyHeldFlags` and `plans/2026-05-22-assistant-pondering-parity-v1.md`.
        let key_held = match flavor {
            SilenceWatchFlavor::Dictation => Arc::clone(&self.held_flags.dictation),
            SilenceWatchFlavor::Assistant { .. } => Arc::clone(&self.held_flags.assistant),
        };
        let recording_state = match flavor {
            SilenceWatchFlavor::Dictation => fono_overlay::OverlayState::Recording { db: 0 },
            SilenceWatchFlavor::Assistant { .. } => {
                fono_overlay::OverlayState::AssistantRecording { db: 0 }
            }
        };
        let make_pondering_state = move |walk_progress: u16| match flavor {
            SilenceWatchFlavor::Dictation => {
                fono_overlay::OverlayState::Pondering { db: 0, walk_progress }
            }
            SilenceWatchFlavor::Assistant { .. } => {
                fono_overlay::OverlayState::AssistantPondering { db: 0, walk_progress }
            }
        };
        let commit_action: Option<HotkeyAction> = match flavor {
            SilenceWatchFlavor::Dictation => Some(HotkeyAction::TogglePressed),
            SilenceWatchFlavor::Assistant { auto_stop_commit } => {
                auto_stop_commit.then_some(HotkeyAction::AssistantPressed)
            }
        };
        let task = tokio::spawn(async move {
            let mut envelope = EnvelopeFollower::new(envelope_cfg);
            let mut watch = SilenceWatch::new(watch_cfg);
            let frame_samples = (sample_rate as usize / 1000) * 20;
            if frame_samples == 0 {
                return;
            }
            let frame_ms = 20.0_f32;
            let mut last_pos: usize = 0;
            let mut tick = tokio::time::interval(Duration::from_millis(20));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut in_pondering = false;
            let mut last_walk_progress: u16 = 0;
            // Pre-computed for the Advanced VU bar's silence-threshold
            // tick: voiced_rms × 10^(-12 dB / 20) ≈ voiced_rms × 0.2512.
            // Slice-2 `SilenceWatchConfig::default().silence_gap_db` is
            // 12 dB; if that changes here, mirror it.
            let mut metrics_tick_ms: f32 = 0.0;
            loop {
                tick.tick().await;
                let new_samples: Vec<f32> = match buf.lock() {
                    Ok(g) => {
                        let s = g.samples();
                        if s.len() <= last_pos {
                            Vec::new()
                        } else {
                            s[last_pos..].to_vec()
                        }
                    }
                    Err(_) => continue,
                };
                let mut consumed = 0;
                // Push-to-talk gate: while the dictation key is
                // physically held down, suppress the `Pondering`
                // overlay flip and any auto-stop commit. We still
                // advance the envelope follower so the live VU bar
                // keeps animating; we just skip feeding
                // `SilenceWatch`, which freezes its internal state
                // at the moment of the press. On release the watch
                // resumes from where it left off; the daemon will
                // shortly route the long-release synthetic
                // `TogglePressed` into `on_stop_recording`, which
                // aborts this task before any post-release silence
                // accumulation could matter.
                let held = key_held.load(Ordering::Relaxed);
                if held && in_pondering {
                    // The user pressed the key while we were already
                    // in Pondering (rare: a previous toggle session
                    // sitting paused, the user grabs the key again
                    // — but the FSM treats that as a toggle-off, so
                    // by the time this branch fires the task is
                    // usually already aborted). Snap the overlay
                    // back to Recording so the held state never
                    // shows the Pondering label.
                    in_pondering = false;
                    last_walk_progress = 0;
                    overlay.set_state(recording_state);
                }
                while consumed + frame_samples <= new_samples.len() {
                    envelope.push_frame(&new_samples[consumed..consumed + frame_samples]);
                    if !held {
                        let snap = envelope.snapshot();
                        let event = watch.push(snap, frame_ms);
                        match event {
                            SilenceEvent::EnteredPondering => {
                                in_pondering = true;
                                last_walk_progress = 0;
                                overlay.set_state(make_pondering_state(0));
                            }
                            SilenceEvent::ResumedFromPondering => {
                                in_pondering = false;
                                last_walk_progress = 0;
                                overlay.set_state(recording_state);
                            }
                            SilenceEvent::Committed => {
                                // Auto-stop fires. Emit a synthetic toggle so
                                // the stop is observationally identical to
                                // the user pressing the hotkey: same FSM
                                // transition, same on_stop_recording /
                                // on_stop_live call, same overlay
                                // transitions. For the assistant flavour,
                                // `commit_action` may be `None` (hold-to-
                                // talk path) in which case we keep the
                                // visual Pondering label but defer to the
                                // user to release the key.
                                if let Some(action) = commit_action {
                                    tracing::info!(
                                        target: "fono::auto_stop",
                                        "auto-stop committed after {} ms of silence (flavor={:?})",
                                        watch_cfg.auto_stop_silence_ms.unwrap_or(0),
                                        flavor,
                                    );
                                    let _ = action_tx.send(action);
                                    return;
                                }
                            }
                            _ => {}
                        }
                    }
                    consumed += frame_samples;
                }
                last_pos += consumed;
                if in_pondering && !held {
                    let elapsed = watch.pondering_elapsed_ms();
                    // 1 s plain grace, then ramp walk_progress 1..=10_000
                    // over (walk_total_ms - pondering_visual_ms - 1000).
                    let grace_ms = 1000.0_f32;
                    let walk_window_ms =
                        (walk_total_ms.saturating_sub(pondering_visual_ms)) as f32 - grace_ms;
                    let new_progress: u16 = if elapsed < grace_ms || walk_window_ms <= 0.0 {
                        0
                    } else {
                        let frac = ((elapsed - grace_ms) / walk_window_ms).clamp(0.0, 1.0);
                        let p = (frac * 10_000.0) as u32 + 1;
                        p.min(10_000) as u16
                    };
                    // 100-step quantisation keeps overlay set_state
                    // traffic to ~100 transitions across the walk
                    // — plenty smooth, much cheaper than per-frame.
                    if new_progress.abs_diff(last_walk_progress) >= 100 || new_progress == 10_000 {
                        last_walk_progress = new_progress;
                        overlay.set_state(make_pondering_state(new_progress));
                    }
                }
                // Push gate metrics for the `Advanced` VU-bar at ~10 Hz.
                // Skipped silently by the renderer in `Off` / `Simple`
                // modes (cheap message; redraw is gated server-side).
                metrics_tick_ms += 20.0;
                if metrics_tick_ms >= 100.0 {
                    metrics_tick_ms = 0.0;
                    let snap = envelope.snapshot();
                    let silence_rms = snap.voiced_rms * SILENCE_GAIN;
                    overlay.push_gate_metrics(snap.inst_rms, snap.voiced_rms, silence_rms);
                }
            }
        });
        Some(task.abort_handle())
    }

    /// Spawn the assistant-thinking animation: a per-style synthetic
    /// generator that pushes time-evolving frames at 20 fps so the
    /// overlay shows active feedback during the post-release dead
    /// time (STT + LLM streaming + first TTS). All four waveform
    /// styles get a hand-tuned animation distinct from the
    /// real-audio one used during recording.
    ///
    /// Returns `None` when the overlay is disabled in config or
    /// failed to spawn at startup. Callers stow the
    /// [`tokio::task::AbortHandle`] so the animation stops when the
    /// pump completes (or the user cancels).
    /// Spawn the assistant-thinking synthetic visualisation. Each
    /// waveform style gets its own time-evolving generator pushed
    /// at 20 fps; renderer-side, the only state-aware branch is
    /// the Bars draw which reads the per-bar profile from
    /// `fft_frames.back()` during `AssistantThinking` and the FFT
    /// draw which switches to a gapped layout.
    ///
    /// The math is deliberately framed against `time_ms` (f64) to
    /// match the source the user audited; constants are unitless
    /// (per-millisecond rates / per-bin widths) so the same numbers
    /// behave correctly on any panel size.
    #[cfg(feature = "interactive")]
    #[allow(
        clippy::too_many_lines,
        clippy::suboptimal_flops,
        clippy::items_after_statements,
        clippy::cast_precision_loss
    )]
    fn spawn_thinking_animation_task(&self, cfg: &Config) -> Option<tokio::task::AbortHandle> {
        // Assistant-only path — independent of the dictation
        // interactive gate (see `spawn_waveform_level_task`).
        let want_waveform = cfg.overlay.waveform;
        let handle = self.overlay.read().ok().and_then(|g| g.clone());
        if !want_waveform {
            return None;
        }
        let o = handle?;
        let style = cfg.overlay.style;
        let task = tokio::spawn(async move {
            // Renderer-side ring sizes: the synthetic data must
            // fully populate the visible window each tick.
            // Heatmap uses 300 bins (one per row of vertical
            // resolution); FFT in thinking mode uses fewer bins
            // (120) so the 1-pixel gaps between bars round to
            // even slot widths instead of producing
            // unevenly-spaced phantom lines at sub-pixel rates.
            const FFT_BINS_HEATMAP: usize = 300;
            const FFT_BINS_THINKING: usize = 100;
            const OSC_SAMPLES: usize = 5000;
            // Bars draw width — matches LEVELS_CAP so the per-bar
            // profile lines up 1:1 with the existing slot count.
            const BARS: usize = 60;

            let started = Instant::now();
            // Heatmap transition: just push at the steady cadence
            // and let the rolling cache do the work — the new
            // strand columns scroll in from the right while the
            // pre-thinking recording-FFT data scrolls left and out
            // over ~6 s. Seamless and keeps the user's recent voice
            // visible while it fades.
            let mut tick = tokio::time::interval(Duration::from_millis(50));
            loop {
                tick.tick().await;
                let time_ms = started.elapsed().as_secs_f64() * 1000.0;
                match style {
                    // ── FFT: spectrum scanning ────────────────────
                    // Gaussian "scanner" (σ ≈ 20 bins out of 120)
                    // sweeps across the bins; a per-bin breathing
                    // baseline keeps every bar alive even far
                    // from the focus. The two are blended
                    // additively so the scanner blends smoothly
                    // into the surrounding rhythm instead of
                    // looking like a sharp spotlight on top of a
                    // flat field.
                    fono_core::config::WaveformStyle::Fft => {
                        let n = FFT_BINS_THINKING;
                        let scan_phase = ((time_ms * 0.0015).sin() + 1.0) / 2.0;
                        let focus = scan_phase * n as f64;
                        // σ = 8 bins; divisor = 2·σ² for the
                        // unnormalised Gaussian. Visible bell
                        // width is ≈8 % of the panel.
                        let sigma_bins = 8.0_f64;
                        let denom = 2.0 * sigma_bins * sigma_bins;
                        let mut bins = vec![0.0_f32; n];
                        for (i, slot) in bins.iter_mut().enumerate() {
                            let dist = (i as f64) - focus;
                            let scanner = (-(dist * dist) / denom).exp();
                            let breathing =
                                ((time_ms * 0.003 + (i as f64) * 0.2).sin() + 1.0) / 2.0;
                            // Soft "screen" blend: scanner takes
                            // priority near focus (peak = 1.0
                            // exactly, no clipping), breathing
                            // fills in the surrounding bars.
                            // Mathematically: scanner +
                            // (1 − scanner) · breathing · 0.30 —
                            // scanner reaches 1.0 alone at focus
                            // (where the (1 − scanner) factor
                            // multiplies the breathing addition by
                            // 0, eliminating the over-1.0
                            // contribution that was causing the
                            // flat-topped clip).
                            let combined = scanner + (1.0 - scanner) * breathing * 0.30;
                            *slot = combined.clamp(0.0, 1.0) as f32;
                        }
                        o.push_fft_bins(bins);
                    }
                    // ── Heatmap: neural strands ────────────────────
                    // Two wandering paths combined as Gaussian
                    // falloffs vertically. Each tick is one new
                    // column; the rolling 6 s window traces them
                    // out as crossing strands.
                    fono_core::config::WaveformStyle::Heatmap => {
                        // Strand positions in bin-index space.
                        // Amplitudes sized so the pair stays well
                        // inside the panel even at extremes.
                        let mid = FFT_BINS_HEATMAP as f64 / 2.0;
                        let amp_a = FFT_BINS_HEATMAP as f64 * 0.30;
                        let amp_b = FFT_BINS_HEATMAP as f64 * 0.10;
                        let amp_c = FFT_BINS_HEATMAP as f64 * 0.35;
                        let strand1 =
                            mid + (time_ms * 0.001).sin() * amp_a + (time_ms * 0.003).sin() * amp_b;
                        let strand2 = mid + (time_ms * 0.0015).cos() * amp_c;
                        // Falloff scaled to the bin axis so the
                        // strand width reads as ~5 % of the panel
                        // (visually equivalent to the user's
                        // 10 px / 80 px-tall reference).
                        let sigma_a_sq = (FFT_BINS_HEATMAP as f64 * 0.06).powi(2);
                        let sigma_b_sq = (FFT_BINS_HEATMAP as f64 * 0.075).powi(2);
                        let mut bins = vec![0.0_f32; FFT_BINS_HEATMAP];
                        for (i, slot) in bins.iter_mut().enumerate() {
                            let y = i as f64;
                            let d1 = y - strand1;
                            let d2 = y - strand2;
                            let int1 = (-(d1 * d1) / sigma_a_sq).exp();
                            let int2 = (-(d2 * d2) / sigma_b_sq).exp();
                            *slot = (int1 + int2).min(1.0).clamp(0.0, 1.0) as f32;
                        }
                        o.push_fft_bins(bins);
                    }
                    // ── Oscilloscope: harmonic processing ──────────
                    // Two interfering sine waves with edge taper
                    // (sin(π · x/W) so x = 0 and x = 1 stay pinned
                    // at the centerline). Pushing the full
                    // OSC_SAMPLES_CAP each tick replaces the ring,
                    // so the renderer always sees a fresh snapshot.
                    // Amplitude is sized so the wave reaches ±1.0
                    // (full panel height) routinely — partial
                    // cancellation around the edges of the beat
                    // envelope still touches the rails.
                    fono_core::config::WaveformStyle::Oscilloscope => {
                        let mut samples = vec![0.0_f32; OSC_SAMPLES];
                        // Per-pixel constants (`f1=0.015, f2=0.010`)
                        // are in pixel units; the renderer maps the
                        // OSC_SAMPLES_CAP buffer linearly across
                        // ~588 px of panel width.
                        let panel_w = 588.0_f64;
                        let f1_eff = 0.015 * panel_w / (OSC_SAMPLES as f64 - 1.0);
                        let f2_eff = 0.010 * panel_w / (OSC_SAMPLES as f64 - 1.0);
                        let beat_env = (time_ms * 0.001).sin();
                        let t_bg = time_ms * 0.002;
                        let t_fg = time_ms * 0.003;
                        // Peak y_val ≈ 43 when all sines align;
                        // dividing by 44 keeps the central antinode
                        // just shy of ±1.0 across the typical beat
                        // envelope, so the wave routinely touches
                        // the rails without clipping. The renderer
                        // also passes `headroom = 1.0` for the
                        // thinking path, so the panel ceiling /
                        // floor exactly equal ±1.0.
                        let amp_div = 44.0_f64;
                        for (i, slot) in samples.iter_mut().enumerate() {
                            let xi = i as f64;
                            let bg_b1 = (xi * f1_eff + t_bg).sin() * 20.0 * 0.6;
                            let bg_b2 = (xi * f2_eff - t_bg * 1.5).sin() * 15.0 * 0.6 * beat_env;
                            let fg_b1 = (xi * f1_eff + t_fg).sin() * 20.0 * 1.0;
                            let fg_b2 = (xi * f2_eff - t_fg * 1.5).sin() * 15.0 * 1.0 * beat_env;
                            // Foreground dominates; background
                            // adds a softer beating texture on top.
                            let y_val = fg_b1 + fg_b2 + 0.4 * (bg_b1 + bg_b2);
                            // Edge taper sin(π · u) anchors x=0/x=1
                            // back to the centreline.
                            let u = xi / (OSC_SAMPLES as f64 - 1.0);
                            let edge = (u * std::f64::consts::PI).sin();
                            *slot = ((y_val * edge) / amp_div).clamp(-1.0, 1.0) as f32;
                        }
                        o.push_samples(samples);
                    }
                    // ── Bars: symmetric centre-out ─────────────────
                    // Per-bar profile pushed via fft_frames; the
                    // bars renderer reads it directly during
                    // AssistantThinking and skips the levels ring.
                    // Concentric outward-flowing waves with edge
                    // taper produce a peak at the centre that
                    // ripples toward the edges.
                    fono_core::config::WaveformStyle::Bars => {
                        let center = BARS as f64 / 2.0;
                        let mut bins = vec![0.0_f32; BARS];
                        for (i, slot) in bins.iter_mut().enumerate() {
                            let dist = (i as f64) - center + 0.5;
                            let dist = dist.abs();
                            let phase = dist * 0.5 - time_ms * 0.003;
                            let intensity = (phase.sin() + 1.0) / 2.0;
                            let edge_taper = (1.0 - dist / center).max(0.0);
                            // 0.05 floor so silence still reads
                            // as "alive"; max ≈ 1.0 at centre.
                            let h = 0.05 + intensity * edge_taper;
                            *slot = (h.clamp(0.0, 1.0)) as f32;
                        }
                        o.push_fft_bins(bins);
                    }
                    // Transcript renders the streaming text panel, not
                    // an animated visualisation — nothing to push from
                    // this task. The match must remain exhaustive.
                    fono_core::config::WaveformStyle::Transcript => {}
                }
            }
        });
        Some(task.abort_handle())
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
        if let Some(polish) = self.current_llm() {
            tokio::spawn(async move {
                let started = Instant::now();
                match polish.prewarm().await {
                    Ok(()) => debug!(
                        "warmup: polish {} ready in {}ms",
                        polish.name(),
                        started.elapsed().as_millis()
                    ),
                    Err(e) => debug!("warmup: polish {} prewarm skipped: {e:#}", polish.name()),
                }
            });
        }
        // TTS prewarm matters for Cartesia: it pre-resolves a native
        // voice per non-English configured language via `/voices?…`
        // and caches them, so the first synth doesn't pay the HTTP
        // round-trip. Other TTS backends do their own startup probes
        // here too (Deepgram, Wyoming, OpenAI-compat). Errors are
        // non-fatal; the per-call code paths self-heal.
        if let Some(tts) = self.current_tts() {
            tokio::spawn(async move {
                let started = Instant::now();
                match tts.prewarm().await {
                    Ok(()) => debug!(
                        "warmup: tts {} ready in {}ms",
                        tts.name(),
                        started.elapsed().as_millis()
                    ),
                    Err(e) => warn!("warmup: tts {} prewarm failed: {e:#}", tts.name()),
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
        polish: Option<Arc<dyn TextFormatter>>,
        history: Arc<Mutex<HistoryDb>>,
        capture_cfg: CaptureConfig,
        config: Arc<Config>,
        action_tx: mpsc::UnboundedSender<HotkeyAction>,
        injector: Arc<dyn Injector>,
        focus: Arc<dyn FocusProbe>,
    ) -> Self {
        let history_window =
            Duration::from_secs(60 * u64::from(config.assistant.history_window_minutes));
        let history_max = config.assistant.history_max_turns as usize;
        Self {
            stt: Arc::new(StdRwLock::new(stt)),
            #[cfg(feature = "interactive")]
            streaming_stt: Arc::new(StdRwLock::new(None)),
            polish: Arc::new(StdRwLock::new(polish)),
            tts: Arc::new(StdRwLock::new(None)),
            assistant_backend: Arc::new(StdRwLock::new(None)),
            assistant_session: Arc::new(Mutex::new(AssistantSessionState::new(
                ConversationHistory::new(history_window, history_max),
            ))),
            assistant_capture: Arc::new(Mutex::new(None)),
            history,
            capture_cfg,
            capture: Arc::new(Mutex::new(None)),
            #[cfg(feature = "interactive")]
            live_capture: Arc::new(Mutex::new(None)),
            #[cfg(feature = "interactive")]
            assistant_live_capture: Arc::new(Mutex::new(None)),
            #[cfg(feature = "interactive")]
            overlay: Arc::new(StdRwLock::new(None)),
            pipeline_in_flight: Arc::new(AtomicBool::new(false)),
            config: Arc::new(StdRwLock::new(config)),
            paths: None,
            action_tx,
            injector,
            focus,
            held_flags: fono_hotkey::KeyHeldFlags::default(),
        }
    }

    /// Begin recording. Refuses if a previous pipeline is still running.
    #[allow(clippy::too_many_lines, clippy::suboptimal_flops, clippy::many_single_char_names)]
    pub async fn on_start_recording(&self, mode: RecordingMode) -> Result<()> {
        fono_stt::rate_limit_notify::reset_session_flag();
        fono_core::critical_notify::reset_session_flag();
        if self.pipeline_in_flight.load(Ordering::SeqCst) {
            warn!("recording requested while previous pipeline still running; ignoring");
            return Ok(());
        }
        // Dictation and assistant are separate intents; if the user
        // pivots from "ask" to "dictate" mid-conversation we wipe the
        // assistant's rolling context (configurable). Also stops any
        // assistant turn that's still speaking.
        self.maybe_clear_assistant_on_dictation().await;
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
            Err(_) => return Err(anyhow::anyhow!("capture thread died before reporting status")),
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
        #[cfg(feature = "interactive")]
        let level_task = self.spawn_waveform_level_task(
            &cfg,
            fono_overlay::OverlayState::Recording { db: 0 },
            &buffer,
        );

        // Silence-watch: drives the Recording ↔ Pondering overlay
        // transition + walking-letter highlight. Toggle-mode dictation
        // only; hold-to-talk owns its own boundary. Slice 2 of the
        // auto-stop plan — visual feedback only, no auto-stop commit
        // yet.
        #[cfg(feature = "interactive")]
        let silence_task = if matches!(mode, RecordingMode::Toggle) {
            self.spawn_silence_watch_task(&cfg, &buffer)
        } else {
            None
        };

        *slot = Some(CaptureSession {
            buffer,
            stop_tx,
            join: Some(join),
            started_at: Instant::now(),
            #[cfg(feature = "interactive")]
            level_task,
            #[cfg(feature = "interactive")]
            silence_task,
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
        // Standalone-waveform overlay: shift to amber `Polishing`
        // (animated, when STT or LLM is local) or `Processing`
        // (static, for cloud) while STT runs. Live-dictation mode
        // owns its own state transitions; only flip when this is
        // the batch path.
        #[cfg(feature = "interactive")]
        let polish_anim: Option<tokio::task::AbortHandle> = {
            if cfg.overlay.waveform && !cfg.live_preview() {
                let stt_local = self.current_stt().is_local();
                let llm_local = cfg.interactive.cleanup_on_finalize
                    && self.current_llm().is_some_and(|l| l.is_local());
                let animate = stt_local || llm_local;
                if let Some(o) = self.overlay.read().ok().and_then(|g| g.clone()) {
                    o.set_state(if animate {
                        fono_overlay::OverlayState::Polishing
                    } else {
                        fono_overlay::OverlayState::Processing
                    });
                }
                if animate {
                    self.spawn_thinking_animation_task(&cfg)
                } else {
                    None
                }
            } else {
                None
            }
        };
        #[cfg(not(feature = "interactive"))]
        let polish_anim: Option<tokio::task::AbortHandle> = None;
        let (samples, elapsed) =
            tokio::task::spawn_blocking(move || session.stop_and_drain()).await.unwrap_or_default();
        let capture_ms = elapsed.as_millis() as u64;
        info!("recording stopped: {capture_ms} ms / {} samples", samples.len());

        if elapsed < MIN_RECORDING || samples.is_empty() {
            warn!("recording too short ({capture_ms} ms); skipping STT");
            #[cfg(feature = "interactive")]
            if cfg.overlay.waveform && !cfg.live_preview() {
                if let Some(t) = polish_anim {
                    t.abort();
                }
                if let Some(o) = self.overlay.read().ok().and_then(|g| g.clone()) {
                    o.set_state(fono_overlay::OverlayState::Hidden);
                }
            }
            let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
            return;
        }

        self.spawn_pipeline(samples, capture_ms, polish_anim);
    }

    /// Cancel an active recording, dropping the audio without invoking STT.
    /// Tears down both the batch capture slot and the live-dictation
    /// session if either is active — ESC during F7 must stop the
    /// streaming pipeline cleanly, not leave its threads/tasks running
    /// in the background.
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
            #[cfg(feature = "interactive")]
            if cfg.overlay.waveform && !cfg.live_preview() {
                if let Some(o) = self.overlay.read().ok().and_then(|g| g.clone()) {
                    o.set_state(fono_overlay::OverlayState::Hidden);
                }
            }
            info!("recording cancelled by user");
        }
        #[cfg(feature = "interactive")]
        {
            let live_taken = self.live_capture.lock().await.take();
            if let Some(mut session) = live_taken {
                let cfg = self.current_config();
                if cfg.general.auto_mute_system {
                    fono_audio::mute::set_default_sink_mute(false);
                }
                // Same teardown order as on_stop_live_dictation, minus
                // the transcript commit. The grace-sleep is skipped —
                // ESC means "drop this", not "wait for trailing audio".
                if let Some(h) = session.silence_task.take() {
                    h.abort();
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
                // Abort the run task instead of awaiting it — we don't
                // care about the partial transcript.
                session.run_join.abort();
                let _ = session.drain_join.await;
                let _ = session.run_join.await;
                if let Some(o) = session.overlay.as_ref() {
                    o.set_state(fono_overlay::OverlayState::Hidden);
                }
                self.pipeline_in_flight.store(false, Ordering::SeqCst);
                info!("live-dictation cancelled by user");
            }
        }
        let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
    }

    // ---- assistant push-to-talk ---------------------------------------

    /// Begin recording for the voice-assistant path. Mirrors
    /// [`Self::on_start_recording`] but writes into a dedicated
    /// capture slot, skips the dictation overlay / mute path, and
    /// does not flip `pipeline_in_flight` (the assistant pipeline
    /// gates on its own cancellation `Notify`).
    ///
    /// When `[interactive].enabled = true` and a streaming-capable STT
    /// backend is loaded, the press takes the streaming path: same
    /// pipeline as F7 dictation but with the green `AssistantRecording`
    /// panel, so the user sees realtime preview text as they speak.
    /// On release the captured transcript is forwarded to the LLM
    /// (skipping the batch STT step in [`run_assistant_turn`]).
    #[allow(clippy::significant_drop_tightening)]
    pub async fn on_assistant_hold_press(&self) -> Result<()> {
        // If a previous turn's playback is still finishing, stop it
        // — second-press semantics: barge-in.
        {
            let mut s = self.assistant_session.lock().await;
            s.stop_current_turn();
        }

        // Streaming branch: only when interactive mode is on AND a
        // streaming backend is loaded. Falls through to the batch
        // path below otherwise (cloud-only configs, missing feature).
        #[cfg(feature = "interactive")]
        {
            if self.current_config().live_preview() {
                if let Some(streaming) = self.current_streaming_stt() {
                    let mut slot = self.assistant_live_capture.lock().await;
                    if slot.is_some() {
                        warn!(
                            "assistant live capture already in progress; ignoring duplicate start"
                        );
                        return Ok(());
                    }
                    let session = self.build_live_capture_pipeline(
                        streaming,
                        fono_overlay::OverlayState::AssistantRecording { db: 0 },
                        Some(SilenceWatchFlavor::Assistant { auto_stop_commit: true }),
                    )?;
                    *slot = Some(session);
                    if self.current_config().general.auto_mute_system {
                        fono_audio::mute::set_default_sink_mute(true);
                    }
                    info!("assistant recording started (streaming)");
                    return Ok(());
                }
            }
        }

        let mut slot = self.assistant_capture.lock().await;
        if slot.is_some() {
            warn!("assistant capture already in progress; ignoring duplicate start");
            return Ok(());
        }
        let cap_cfg = self.capture_cfg.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel::<
            std::result::Result<Arc<StdMutex<RecordingBuffer>>, String>,
        >();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let join = std::thread::Builder::new()
            .name("fono-assistant-capture".into())
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
            .context("spawn assistant capture thread")?;
        let buffer = match started_rx.recv() {
            Ok(Ok(b)) => b,
            Ok(Err(e)) => {
                let _ = join.join();
                return Err(anyhow::anyhow!("assistant capture failed to start: {e}"));
            }
            Err(_) => {
                return Err(anyhow::anyhow!(
                    "assistant capture thread died before reporting status"
                ))
            }
        };
        // Reuse the dictation-side waveform overlay for the assistant
        // recording. Same Bars / FFT / Heatmap / Oscilloscope styles —
        // only the panel title ("ASSISTANT") and accent colour
        // (saturated green, mirrored by the tray icon) differ.
        let cfg = self.current_config();
        #[cfg(feature = "interactive")]
        let level_task = self.spawn_waveform_level_task(
            &cfg,
            fono_overlay::OverlayState::AssistantRecording { db: 0 },
            &buffer,
        );
        // Pondering parity with dictation: drive the silence-watch
        // state machine so the assistant overlay flips
        // `AssistantRecording → AssistantPondering` on long pauses.
        // The hold-flag inside the watch task suppresses both the
        // flip and the commit while F8 is physically held — see
        // `plans/2026-05-22-assistant-pondering-parity-v1.md`.
        // Auto-stop commits whenever the held flag is false (toggle
        // / quick-tap sessions); hold-to-talk users own the release
        // boundary themselves and the held flag keeps them out of
        // the commit path.
        #[cfg(feature = "interactive")]
        let silence_task = self.spawn_silence_watch_task_for(
            &cfg,
            &buffer,
            SilenceWatchFlavor::Assistant { auto_stop_commit: true },
        );
        let session = CaptureSession {
            buffer,
            stop_tx,
            join: Some(join),
            started_at: Instant::now(),
            #[cfg(feature = "interactive")]
            level_task,
            #[cfg(feature = "interactive")]
            silence_task,
        };
        *slot = Some(session);
        if cfg.general.auto_mute_system {
            fono_audio::mute::set_default_sink_mute(true);
        }
        info!("assistant recording started");
        Ok(())
    }

    /// Stop assistant recording and kick off the streaming pump:
    /// STT → assistant chat → SentenceSplitter → TTS → playback.
    /// Returns immediately; the pump runs on a detached tokio task.
    #[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
    pub async fn on_assistant_hold_release(&self) {
        // Every early-return path MUST emit `ProcessingDone` so the
        // FSM doesn't get stuck in `AssistantThinking` (which would
        // also block subsequent F7/F8 presses). The `_` binding
        // captures cases where there's no buffered session yet (a
        // duplicate release event) — we still kick the FSM to Idle.

        // Streaming branch: consume the live capture, await the final
        // transcript, and forward it as `pre_transcribed` to skip the
        // batch STT step. Mirrors `on_stop_live_dictation`'s teardown
        // shape but routes the transcript to the LLM rather than the
        // text injector.
        let mut pre_transcribed: Option<String> = None;
        let mut elapsed: Option<Duration> = None;
        #[cfg(feature = "interactive")]
        {
            let live_taken = self.assistant_live_capture.lock().await.take();
            if let Some(mut session) = live_taken {
                let captured_for = session.started_at.elapsed();
                // Same trailing-word grace as the F7 path: the cpal
                // callback may still have a few frames in flight when
                // the user releases the key.
                let cfg = self.current_config();
                if cfg.general.auto_mute_system {
                    fono_audio::mute::set_default_sink_mute(false);
                }
                let grace_ms = u64::from(cfg.interactive.hold_release_grace_ms);
                if grace_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(grace_ms)).await;
                }
                if let Some(h) = session.silence_task.take() {
                    h.abort();
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
                let transcript_res = session.run_join.await;
                let transcript = match transcript_res {
                    Ok(Ok(t)) => t,
                    Ok(Err(e)) => {
                        error!("assistant: streaming STT failed: {e:#}");
                        self.hide_assistant_overlay();
                        let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
                        return;
                    }
                    Err(e) => {
                        error!("assistant: streaming run task join error: {e:#}");
                        self.hide_assistant_overlay();
                        let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
                        return;
                    }
                };
                let raw = transcript.committed.trim().to_string();
                if raw.is_empty() {
                    info!("assistant streaming: empty transcript; skipping");
                    self.hide_assistant_overlay();
                    let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
                    return;
                }
                pre_transcribed = Some(raw);
                elapsed = Some(captured_for);
            }
        }

        let (pcm, elapsed) = if pre_transcribed.is_some() {
            (Vec::new(), elapsed.unwrap_or_default())
        } else {
            let session = self.assistant_capture.lock().await.take();
            let Some(session) = session else {
                warn!("assistant release without a matching press; ignoring");
                self.hide_assistant_overlay();
                let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
                return;
            };
            if self.current_config().general.auto_mute_system {
                fono_audio::mute::set_default_sink_mute(false);
            }
            match tokio::task::spawn_blocking(move || session.stop_and_drain()).await {
                Ok(t) => t,
                Err(e) => {
                    warn!("assistant capture join failed: {e:#}");
                    self.hide_assistant_overlay();
                    let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
                    return;
                }
            }
        };
        if pre_transcribed.is_none() && (elapsed < MIN_RECORDING || pcm.is_empty()) {
            info!("assistant recording too short ({}ms); skipping", elapsed.as_millis());
            self.hide_assistant_overlay();
            let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
            return;
        }
        let cfg = self.current_config();
        let stt = self.current_stt();
        let assistant = self.current_assistant();
        let tts = self.current_tts();
        let (Some(assistant), Some(tts)) = (assistant, tts) else {
            // The slots are populated by `build_assistant()` /
            // `build_tts()` in `new()` and `reload()`. If the config
            // flags are on but the slots are empty, the factory
            // errored at startup (missing API key, missing
            // sub-block, missing feature). Run `fono doctor` for the
            // exact reason; the daemon also logged it on startup.
            warn!(
                "assistant turn requested but a runtime backend is missing \
                 (assistant_loaded={} tts_loaded={}; config: assistant.enabled={} \
                 tts.backend={:?}). Run `fono doctor` to see which factory failed.",
                self.current_assistant().is_some(),
                self.current_tts().is_some(),
                cfg.assistant.enabled,
                cfg.tts.backend,
            );
            fono_core::notify::send(
                "Fono — assistant backend missing",
                "The assistant or TTS factory failed at startup (likely a missing API key). \
                 Run `fono doctor` to see which backend errored.",
                "dialog-information",
                6_000,
                fono_core::notify::Urgency::Normal,
            );
            self.hide_assistant_overlay();
            let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
            return;
        };
        // Switch the overlay into "thinking" mode and spawn the
        // synthetic-animation ticker. The renderer paints amber
        // with a "THINKING" title; each waveform style gets a
        // distinct hand-tuned animation (FFT bell sweep, heatmap
        // intersecting paths, oscilloscope standing wave,
        // centre-symmetric bars). The ticker runs until the pump's
        // closure aborts it on completion / cancellation.
        if let Some(o) = self.overlay.read().ok().and_then(|g| g.clone()) {
            o.set_state(fono_overlay::OverlayState::AssistantThinking);
        }
        #[cfg(feature = "interactive")]
        let thinking_task = self.spawn_thinking_animation_task(&cfg);
        #[cfg(not(feature = "interactive"))]
        let thinking_task: Option<tokio::task::AbortHandle> = None;
        let overlay_for_task = self.overlay.read().ok().and_then(|g| g.clone());
        let notify = Arc::new(Notify::new());
        {
            let mut s = self.assistant_session.lock().await;
            s.current_turn = Some(notify.clone());
        }
        let inputs = AssistantTurnInputs {
            pcm,
            sample_rate: self.capture_cfg.target_sample_rate,
            stt,
            assistant,
            tts,
            system_prompt: cfg.assistant.prompt_main.clone(),
            language: cfg.general.language_override().map(str::to_string),
            action_tx: self.action_tx.clone(),
            // Hand the pump a clone of the live overlay handle so it
            // can flip THINKING → SPEAKING the moment the first LLM
            // delta arrives (see `assistant.rs`). Cheap (Arc-wrapped),
            // None when no graphical session is attached.
            overlay: self.overlay.read().ok().and_then(|g| g.clone()),
            pre_transcribed,
        };
        let state_for_task = self.assistant_session.clone();
        let action_tx = self.action_tx.clone();
        let notify_for_task = notify.clone();
        let state_for_clear = state_for_task.clone();
        tokio::spawn(async move {
            if let Err(e) = run_assistant_turn(state_for_task, inputs, notify_for_task).await {
                warn!("assistant turn failed: {e:#}");
            }
            // Clear the current_turn slot so a fresh press doesn't
            // think a stale pump is still running.
            {
                let mut s = state_for_clear.lock().await;
                if let Some(active) = s.current_turn.as_ref() {
                    if Arc::ptr_eq(active, &notify) {
                        s.current_turn = None;
                    }
                }
            }
            // Stop the thinking animation and hide the overlay —
            // the pump is done, the user has heard (or aborted)
            // the reply.
            if let Some(t) = thinking_task {
                t.abort();
            }
            if let Some(o) = overlay_for_task {
                o.set_state(fono_overlay::OverlayState::Hidden);
            }
            // Tell the FSM we're idle.
            let _ = action_tx.send(HotkeyAction::ProcessingDone);
        });
    }

    /// Hide the standalone-waveform overlay. Best-effort — silently
    /// noop'd when the overlay handle isn't present (e.g.
    /// `[overlay].waveform = false` or no graphical session).
    fn hide_assistant_overlay(&self) {
        if let Some(o) = self.overlay.read().ok().and_then(|g| g.clone()) {
            o.set_state(fono_overlay::OverlayState::Hidden);
        }
    }

    /// Stop the active assistant turn immediately. Notifies the pump
    /// to bail out and asks the playback handle to drain its queue.
    /// Conversation history is preserved so a follow-up turn carries
    /// context.
    ///
    /// Also tears down the streaming live-capture session if ESC was
    /// pressed mid-recording (before the user released F8) — without
    /// this the cpal capture thread + bridge + run task would keep
    /// running after the FSM has already returned to Idle.
    pub async fn on_assistant_stop(&self) {
        {
            let mut s = self.assistant_session.lock().await;
            s.stop_current_turn();
        }
        // Best-effort unmute on cancel: covers both an in-flight
        // assistant recording (live or batch) and a turn still in
        // its STT/LLM/TTS phase where we'd already unmuted — the
        // call is idempotent.
        if self.current_config().general.auto_mute_system {
            fono_audio::mute::set_default_sink_mute(false);
        }
        #[cfg(feature = "interactive")]
        {
            let live_taken = self.assistant_live_capture.lock().await.take();
            if let Some(mut session) = live_taken {
                if let Some(h) = session.silence_task.take() {
                    h.abort();
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
                session.run_join.abort();
                let _ = session.drain_join.await;
                let _ = session.run_join.await;
                info!("assistant streaming capture cancelled by user");
            }
        }
        self.hide_assistant_overlay();
        info!("assistant stop requested");
    }

    /// Stop the active assistant turn AND clear the rolling history.
    /// Backs the tray "Forget conversation" entry — a one-step
    /// "fresh start" without changing config.
    pub async fn on_assistant_forget(&self) {
        {
            let mut s = self.assistant_session.lock().await;
            s.stop_current_turn();
            s.history.clear();
        }
        info!("assistant history cleared");
    }

    fn current_assistant(&self) -> Option<Arc<dyn Assistant>> {
        self.assistant_backend.read().expect("assistant lock poisoned").clone()
    }

    fn current_tts(&self) -> Option<Arc<dyn TextToSpeech>> {
        self.tts.read().expect("tts lock poisoned").clone()
    }

    /// Wipe the assistant's rolling history (and stop any in-flight
    /// playback) when the user pivots to dictation. No-op when
    /// `[assistant].auto_clear_on_dictation = false`.
    async fn maybe_clear_assistant_on_dictation(&self) {
        let cfg = self.current_config();
        if !cfg.assistant.auto_clear_on_dictation {
            return;
        }
        let mut s = self.assistant_session.lock().await;
        s.stop_current_turn();
        if !s.history.snapshot().is_empty() || !s.history.is_stale() {
            debug!(target: "fono::assistant", "clearing assistant history (dictation pivot)");
            s.history.clear();
        }
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

    fn spawn_pipeline(
        &self,
        pcm: Vec<f32>,
        capture_ms: u64,
        polish_anim: Option<tokio::task::AbortHandle>,
    ) {
        let stt = self.current_stt();
        let polish = self.current_llm();
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
        #[cfg(feature = "interactive")]
        let overlay = if config.overlay.waveform && !config.live_preview() {
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
                polish.as_deref(),
                &history,
                &config,
                injector.as_ref(),
                focus.as_ref(),
            )
            .await;
            match &outcome {
                PipelineOutcome::Completed { metrics, .. } => {
                    info!(
                        "pipeline ok: capture={}ms trim={}ms ({}→{} samples) stt={}ms polish={}ms{} inject={}ms ({} → {} chars)",
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
            #[cfg(feature = "interactive")]
            if let Some(t) = polish_anim {
                t.abort();
            }
            #[cfg(not(feature = "interactive"))]
            drop(polish_anim);
            #[cfg(feature = "interactive")]
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
        let polish = self.current_llm();
        let config = self.current_config();
        run_pipeline(
            pcm,
            self.capture_cfg.target_sample_rate,
            capture_ms,
            stt.as_ref(),
            polish.as_deref(),
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
        self.streaming_stt.read().expect("streaming_stt lock poisoned").clone()
    }

    /// Build a streaming-STT-backed capture pipeline with the supplied
    /// overlay state. Shared between [`Self::on_start_live_dictation`]
    /// (F7, `LiveDictating` panel) and [`Self::on_assistant_hold_press`]
    /// (F8 with interactive enabled, `AssistantRecording` panel) so
    /// both surfaces get the same realtime preview UX.
    ///
    /// Spawns: the cpal capture thread, the crossbeam→tokio bridge
    /// thread, the pump drain task, and the [`crate::live::LiveSession`]
    /// run task. Returns a [`LiveCaptureSession`] the caller stores in
    /// the appropriate slot.
    #[allow(clippy::significant_drop_tightening, clippy::too_many_lines)]
    fn build_live_capture_pipeline(
        &self,
        streaming: Arc<dyn StreamingStt>,
        active_state: fono_overlay::OverlayState,
        silence_flavor: Option<SilenceWatchFlavor>,
    ) -> Result<LiveCaptureSession> {
        // Slice A: streaming pipeline operates at 16 kHz to keep the
        // pump's broadcast frame budget aligned with whisper. The
        // capture stage resamples for us.
        let sample_rate = 16_000_u32;
        let cap_cfg = CaptureConfig { target_sample_rate: sample_rate };

        // ---- Spawn the capture thread ----------------------------
        // The cpal stream uses the new realtime forwarder API:
        // each data callback resamples to mono f32 @ 16 kHz and
        // pushes the slice into a bounded crossbeam SPSC. The audio
        // thread MUST NOT block on a tokio runtime, so we drop on
        // overflow (logged at warn) rather than queue. The forwarder
        // closure is owned by the cpal `Stream`; dropping the stream
        // (when capture_stop_rx fires) drops the closure and thereby
        // the `audio_tx` Sender, signalling EOF to the bridge thread
        // downstream.
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
                    if forwarder_tx.try_send(pcm.to_vec()).is_err() {
                        warn!("live-capture: realtime SPSC full ({} samples dropped)", pcm.len());
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
                return Err(anyhow::anyhow!("live capture thread died before reporting status"))
            }
        }

        let cfg = self.current_config();

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
        let mut session = crate::live::LiveSession::new(streaming, sample_rate)
            .with_language(language)
            .with_overlay_active_state(active_state);
        if let Some(o) = overlay.as_ref() {
            session = session.with_overlay(o.clone());
        }
        let quality_floor = crate::live::parse_quality_floor(&cfg.interactive.quality_floor);

        // ---- Spawn the run task ----------------------------------
        let run_join = tokio::spawn(session.run(frame_rx, quality_floor));

        // ---- Bridge: realtime crossbeam rx → tokio mpsc ----------
        let (tokio_tx, mut tokio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<f32>>();
        let bridge_join = std::thread::Builder::new()
            .name("fono-live-bridge".into())
            .spawn(move || {
                while let Ok(chunk) = audio_rx.recv() {
                    if tokio_tx.send(chunk).is_err() {
                        break;
                    }
                }
            })
            .context("spawn live-capture bridge thread")?;

        // ---- Spawn the drain task (tokio mpsc -> Pump::push) -----
        // Tap RMS off each chunk to feed the right-side VU bar on the
        // overlay panel during F7. The assistant path uses the same
        // tap so the same VU indicator works for both surfaces.
        // Shadow PCM buffer fed by the drain task in lockstep with
        // the pump. Used by `spawn_silence_watch_task_for` to read
        // recent samples for the envelope follower. Only allocated
        // when a silence flavour is requested — the live-dictation
        // path passes `None` to keep memory flat.
        let shadow_buffer: Option<Arc<StdMutex<RecordingBuffer>>> =
            silence_flavor.map(|_| Arc::new(StdMutex::new(RecordingBuffer::default())));
        let drain_shadow = shadow_buffer.clone();
        let shadow_cap_samples =
            (fono_audio::capture::HARD_CAP.as_secs() as usize) * (sample_rate as usize);
        let vu_overlay = if cfg.overlay.volume_bar.is_on() { overlay.clone() } else { None };
        let drain_join = tokio::spawn(async move {
            let mut pump = pump;
            while let Some(chunk) = tokio_rx.recv().await {
                if let Some(o) = vu_overlay.as_ref() {
                    o.push_level(normalised_rms(&chunk));
                }
                if let Some(sb) = drain_shadow.as_ref() {
                    if let Ok(mut b) = sb.lock() {
                        b.push_slice(&chunk, shadow_cap_samples);
                    }
                }
                pump.push(&chunk);
            }
            pump.finish();
            drop(pump);
        });

        // Drive the silence-watch task off the shadow buffer (Slice 1
        // of `plans/2026-05-22-assistant-pondering-parity-v1.md` for
        // the assistant streaming path; live dictation passes `None`).
        let silence_task = match (silence_flavor, shadow_buffer.as_ref()) {
            (Some(flavor), Some(sb)) => self.spawn_silence_watch_task_for(&cfg, sb, flavor),
            _ => None,
        };

        Ok(LiveCaptureSession {
            capture_stop_tx,
            capture_join: Some(capture_join),
            bridge_join: Some(bridge_join),
            drain_join,
            run_join,
            silence_task,
            overlay,
            started_at: Instant::now(),
        })
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
    #[allow(clippy::significant_drop_tightening)]
    pub async fn on_start_live_dictation(&self, mode: RecordingMode) -> Result<()> {
        fono_stt::rate_limit_notify::reset_session_flag();
        fono_core::critical_notify::reset_session_flag();
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
        self.maybe_clear_assistant_on_dictation().await;
        let mut slot = self.live_capture.lock().await;
        if slot.is_some() {
            warn!("live-dictation already in progress; ignoring duplicate start");
            return Ok(());
        }

        let session = self.build_live_capture_pipeline(
            streaming,
            fono_overlay::OverlayState::LiveDictating,
            None,
        )?;

        let cfg = self.current_config();
        if cfg.general.auto_mute_system {
            fono_audio::mute::set_default_sink_mute(true);
        }

        info!("live-dictation started (mode={:?} sample_rate=16000)", mode);
        self.pipeline_in_flight.store(true, Ordering::SeqCst);
        *slot = Some(session);
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
        //     samples reach the audio bridge — without this, F7 release
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
        // during polish and back to LiveDictating with the final
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
        // a second time. Instead we run an optional polish pass
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
            if let Some(polish) = self.current_llm() {
                // Show "polishing…" so the user knows we haven't
                // hung after the streaming ended.
                if let Some(o) = session.overlay.as_ref() {
                    o.set_state(fono_overlay::OverlayState::Processing);
                    o.update_text(raw.clone());
                }
                let (app_class, app_title) = self.focus.probe();
                let ctx =
                    build_format_context(&cfg, app_class.as_deref(), app_title.as_deref(), None);
                llm_label_for_log = Some(polish.name().to_string());
                match polish.format(&raw, &ctx).await {
                    Ok(c) => {
                        llm_ms = llm_started.elapsed().as_millis() as u64;
                        let trimmed = c.trim().to_string();
                        let raw_chars = raw.chars().count();
                        let new_chars = trimmed.chars().count();
                        // Mirror the batch pipeline's INFO logs at
                        // `session.rs:1253-1265` so live and batch produce
                        // structurally-identical operator output.
                        info!("polish: {} {}ms → {} chars", polish.name(), llm_ms, new_chars);
                        let diff = i64::try_from(new_chars).unwrap_or(0)
                            - i64::try_from(raw_chars).unwrap_or(0);
                        if trimmed == raw {
                            info!("polish: cleanup no-op (input unchanged, {raw_chars} chars)");
                        } else {
                            info!(
                                "polish: cleanup diff {raw_chars} → {new_chars} chars ({diff:+})"
                            );
                        }
                        if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed)
                        }
                    }
                    Err(e) => {
                        llm_ms = llm_started.elapsed().as_millis() as u64;
                        warn!("live-dictation: polish failed after {llm_ms}ms: {e:#}");
                        // Mirror the batch path: surface auth or
                        // network failures once per session so the
                        // user notices the expired key / offline
                        // endpoint. Transient `Other` errors stay
                        // silent (raw transcript still injected).
                        // Global cascade cap prevents piling on top
                        // of an earlier STT notification.
                        let err_text = format!("{e:#}");
                        let class = fono_core::critical_notify::classify(&err_text);
                        if matches!(
                            class,
                            fono_core::critical_notify::ErrorClass::Auth
                                | fono_core::critical_notify::ErrorClass::Network
                                | fono_core::critical_notify::ErrorClass::TermsRequired
                        ) {
                            fono_core::critical_notify::notify(
                                fono_core::critical_notify::Stage::Polish,
                                polish.name(),
                                class,
                                &err_text,
                            );
                        }
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
            "pipeline ok (live): capture={}ms stt=streaming({} segments) polish={} {}ms inject={}ms ({} → {} chars)",
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
                polish_backend: llm_label,
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
    polish: Option<&dyn TextFormatter>,
    history: &Arc<Mutex<HistoryDb>>,
    config: &Config,
    injector: &dyn Injector,
    focus: &dyn FocusProbe,
) -> PipelineOutcome {
    if pcm.is_empty() {
        return PipelineOutcome::EmptyOrTooShort { duration_ms: capture_ms };
    }
    let mut metrics = PipelineMetrics { capture_ms, samples: pcm.len(), ..Default::default() };

    // ---- Trim leading/trailing silence (latency plan L11+L12) -------
    let pcm_for_stt: std::borrow::Cow<'_, [f32]> = if config.audio.trim_silence {
        let trim_started = Instant::now();
        let trim_cfg = fono_audio::TrimConfig { sample_rate, ..Default::default() };
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
    let stt_result = stt.transcribe(&pcm_for_stt, sample_rate, lang.as_deref()).await;
    metrics.stt_ms = stt_started.elapsed().as_millis() as u64;
    let trans = match stt_result {
        Ok(t) => t,
        Err(e) => {
            let err_text = format!("STT {}: {e:#}", stt.name());
            // Critical pipeline failure — also fire a desktop
            // notification (not just `error!`). Dedup is per
            // (stage, provider, class) so a stuck/expired API key
            // pops once per session instead of on every hold.
            let class = fono_core::critical_notify::classify(&err_text);
            fono_core::critical_notify::notify(
                fono_core::critical_notify::Stage::Stt,
                stt.name(),
                class,
                &format!("{e:#}"),
            );
            return PipelineOutcome::Failed(err_text);
        }
    };
    // Strip Whisper-style trailing closer phrases ("thank you", "bye",
    // "you") before the empty-check so silence-tail hallucinations
    // become `EmptyOrTooShort` instead of leaking into the cursor.
    // The streaming/live pipeline already does this in
    // `apply_update`; the batch pipeline needs the same wiring even
    // though `whisper-rs` has hallucination guards enabled — they're
    // probabilistic (no_speech_thold=0.6, logprob_thold=-1.0) and a
    // moderately confident "Thank you." on a silent tail still slips
    // through. See `crates/fono-stt/src/streaming.rs:227`.
    let raw_pre_strip = trans.text.trim().to_string();
    let raw = fono_stt::strip_trailing_hallucinations(&raw_pre_strip);
    metrics.raw_chars = raw.chars().count();
    if raw.is_empty() {
        if raw_pre_strip.is_empty() {
            warn!("STT returned empty text — nothing to inject");
        } else {
            warn!(
                "STT output was a trailing-closer hallucination only ({raw_pre_strip:?}) — skipping injection"
            );
        }
        // Empty-transcript microphone recovery hook. When the user
        // held the hotkey for >= 5 s and the STT still produced
        // nothing, the most likely cause is a silent input device
        // (typically: an external dock with a passive capture
        // endpoint elected as the OS default source). Notify the
        // user and point at the tray Microphone submenu / `fono use
        // input` CLI. Plan v2 Phase 1.
        crate::audio_recovery::notify_empty_capture(capture_ms);
        return PipelineOutcome::EmptyOrTooShort { duration_ms: capture_ms };
    }
    info!("stt: {} {}ms → {} chars", stt.name(), metrics.stt_ms, metrics.raw_chars);

    // ---- polish (optional) -------------------------------------
    let (app_class, app_title) = focus.probe();
    tracing::debug!(
        target: "fono::pipeline",
        "stt.raw lang={:?} app=({:?}, {:?}): {raw:?}",
        trans.language, app_class, app_title,
    );
    let word_count = raw.split_whitespace().count() as u32;
    let skip_short =
        config.polish.skip_if_words_lt > 0 && word_count < config.polish.skip_if_words_lt;
    let cleaned = if skip_short {
        // Latency plan L9 — short utterances rarely need cleanup;
        // skipping the LLM saves 150–800 ms.
        if polish.is_some() {
            info!(
                "polish: skipped (short utterance: {} word(s) < {})",
                word_count, config.polish.skip_if_words_lt
            );
            metrics.llm_skipped_short = true;
        }
        None
    } else if let Some(polish_backend) = polish {
        let ctx = build_format_context(
            config,
            app_class.as_deref(),
            app_title.as_deref(),
            trans.language.as_deref(),
        );
        tracing::debug!(
            target: "fono::pipeline",
            "polish.prompt main={:?} advanced={:?} dictionary={:?}",
            ctx.main_prompt, ctx.advanced_prompt, ctx.dictionary,
        );
        tracing::debug!(target: "fono::pipeline", "polish.input: {raw:?}");
        let llm_started = Instant::now();
        match polish_backend.format(&raw, &ctx).await {
            Ok(c) => {
                metrics.llm_ms = llm_started.elapsed().as_millis() as u64;
                let trimmed = c.trim().to_string();
                let raw_chars = raw.chars().count();
                let new_chars = trimmed.chars().count();
                info!(
                    "polish: {} {}ms → {} chars",
                    polish_backend.name(),
                    metrics.llm_ms,
                    new_chars
                );
                let diff =
                    i64::try_from(new_chars).unwrap_or(0) - i64::try_from(raw_chars).unwrap_or(0);
                if trimmed == raw {
                    info!("polish: cleanup no-op (input unchanged, {raw_chars} chars)");
                } else {
                    info!("polish: cleanup diff {raw_chars} → {new_chars} chars ({diff:+})");
                }
                tracing::debug!(target: "fono::pipeline", "polish.output: {trimmed:?}");
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            }
            Err(e) => {
                metrics.llm_ms = llm_started.elapsed().as_millis() as u64;
                warn!("polish: {} failed after {}ms: {e:#}", polish_backend.name(), metrics.llm_ms);
                // Surface user-actionable failures (auth or network)
                // once per session. Transient `Other` failures stay
                // silent — the raw STT text is still injected below
                // so the dictation isn't lost; a desktop popup on
                // every flaky 5xx would just be noise. The global
                // cascade cap in critical_notify ensures we never
                // pile up notifications when STT already fired.
                let err_text = format!("{e:#}");
                let class = fono_core::critical_notify::classify(&err_text);
                if matches!(
                    class,
                    fono_core::critical_notify::ErrorClass::Auth
                        | fono_core::critical_notify::ErrorClass::Network
                        | fono_core::critical_notify::ErrorClass::TermsRequired
                ) {
                    fono_core::critical_notify::notify(
                        fono_core::critical_notify::Stage::Polish,
                        polish_backend.name(),
                        class,
                        &err_text,
                    );
                }
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
            // Critical: the user just dictated something and now
            // has nothing on screen. Surface this once per session
            // (the cascade cap means it stays silent if STT/LLM
            // already notified).
            let err_text = format!("{e:#}");
            fono_core::critical_notify::notify(
                fono_core::critical_notify::Stage::Inject,
                "injector",
                fono_core::critical_notify::classify(&err_text),
                &err_text,
            );
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
    // clipboard (the clipboard fallback when no key-injector worked) —
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
            polish_backend: polish.map(|l| l.name().to_string()),
            language: trans.language.clone(),
        };
        let redact = config.history.redact_secrets;
        let db = history.lock().await;
        if let Err(e) = db.insert(&row, redact) {
            warn!("history insert failed: {e:#}");
        }
    }

    PipelineOutcome::Completed { raw, cleaned, metrics }
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
        main_prompt: config.polish.prompt.main.clone(),
        advanced_prompt: config.polish.prompt.advanced.clone(),
        dictionary: config.polish.prompt.dictionary.clone(),
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
    hay.to_ascii_lowercase().contains(&needle.to_ascii_lowercase())
}

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Helper used by `fono record` and the integration test to construct an
/// orchestrator backed by a single user-provided STT/LLM pair without
/// touching `Paths` / `Secrets`.
#[must_use]
pub fn orchestrator_for_test(
    stt: Arc<dyn SpeechToText>,
    polish: Option<Arc<dyn TextFormatter>>,
    history_path: &Path,
    config: Arc<Config>,
    injector: Arc<dyn Injector>,
    focus: Arc<dyn FocusProbe>,
) -> (SessionOrchestrator, mpsc::UnboundedReceiver<HotkeyAction>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let history =
        Arc::new(Mutex::new(HistoryDb::open(history_path).expect("open history db (test)")));
    let capture_cfg = CaptureConfig::default();
    let orch = SessionOrchestrator::with_parts(
        stt,
        polish,
        history,
        capture_cfg,
        config,
        tx,
        injector,
        focus,
    );
    (orch, rx)
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
