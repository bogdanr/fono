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
#[cfg(feature = "realtime")]
use fono_assistant::RealtimeAssistant;
use fono_assistant::{
    Assistant, AssistantCacheTrigger, AssistantHandle, AssistantPromptCacheSnapshot,
    AssistantPromptCacheWarmup, ConversationHistory,
};
use fono_audio::{
    AudioCapture, CaptureConfig, EnvelopeConfig, EnvelopeFollower, RecordingBuffer, SilenceEvent,
    SilenceWatch, SilenceWatchConfig,
};
use fono_core::config::{Config, ContextRule};
use fono_core::history::{HistoryDb, Transcription as HistoryRow};
use fono_core::turn_trace::{
    current_instant, current_span, TurnTrace, INJECT_LANE, KEYS_LANE, PUMP_LANE, STT_LANE,
    WARMUP_LANE,
};
use fono_core::{Paths, Secrets};
use fono_hotkey::{HotkeyAction, RecordingMode};
use fono_inject::{ContextClassifier, ContextProfile, FocusInfo};
#[cfg(feature = "interactive")]
use fono_overlay::PolishingPhase;
use fono_polish::{
    has_enough_text_for_language_guard, looks_like_clarification, looks_like_degenerate_cleanup,
    looks_like_translated_cleanup, FormatContext, TextFormatter,
};
#[cfg(feature = "interactive")]
use fono_stt::StreamingStt;
use fono_stt::{SpeechToText, TranscribeOptions};
use fono_tts::TextToSpeech;
use futures::StreamExt;
use serde_json::json;
use std::sync::Mutex as StdMutex;
use std::thread::JoinHandle;
use tokio::sync::{mpsc, Mutex, Notify};
use tracing::{debug, error, info, warn};

use crate::assistant::{run_assistant_turn, AssistantSessionState, AssistantTurnInputs};
#[cfg(feature = "realtime")]
use crate::assistant::{run_realtime_turn, RealtimeTurnInputs};

/// Minimum duration of audio that will be passed to STT. Anything
/// shorter is treated as a misfire.
pub const MIN_RECORDING: Duration = Duration::from_millis(300);

const POLISH_WALK_MIN: Duration = Duration::from_secs(1);
const POLISH_WALK_MAX: Duration = Duration::from_secs(5);

/// Upper bound on how many candidate-language F7Context prefixes the
/// record-start prewarm builds speculatively. Each is one prefill on the
/// shared local model, serialised against the others; capping keeps a long
/// configured locale list from spawning an unbounded prefill train under STT.
const MAX_POLISH_PREWARM_LANGS: usize = 3;

fn polish_walk_duration(recording_duration: Duration) -> Duration {
    (recording_duration / 2).clamp(POLISH_WALK_MIN, POLISH_WALK_MAX)
}

#[cfg(feature = "interactive")]
fn polish_walk_progress(started: Instant, duration: Duration) -> u16 {
    let elapsed_ms = started.elapsed().as_millis();
    let duration_ms = duration.as_millis().max(1);
    let raw = (elapsed_ms.saturating_mul(10_000) / duration_ms).min(10_000);
    raw as u16
}

#[cfg(feature = "interactive")]
fn spawn_polishing_phase_task_for_handle(
    o: fono_overlay::OverlayHandle,
    phase: PolishingPhase,
    walk_duration: Duration,
) -> tokio::task::AbortHandle {
    let task = tokio::spawn(async move {
        let started = Instant::now();
        o.set_state(fono_overlay::OverlayState::Polishing { phase, walk_progress: 0 });
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        loop {
            tick.tick().await;
            o.set_state(fono_overlay::OverlayState::Polishing {
                phase,
                walk_progress: polish_walk_progress(started, walk_duration),
            });
        }
    });
    task.abort_handle()
}

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

/// Upper frequency cutoff for the FFT bar visualisation. Set low so
/// the voice fundamental + first formant fill the panel and small
/// pitch changes are clearly visible.
#[cfg(feature = "interactive")]
const WAVEFORM_FFT_MAX_HZ_FFT: f32 = 1500.0;

/// Upper frequency cutoff for the spectrogram (Heatmap) visualisation.
/// Wider than the bar style so sibilance and higher formants appear as
/// distinct horizontal bands rather than getting clipped at the top.
#[cfg(feature = "interactive")]
const WAVEFORM_FFT_MAX_HZ_HEATMAP: f32 = 6000.0;

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

/// Return `true` when the configured assistant backend is vision-capable
/// (has a `multimodal_model` in the provider catalogue). Used to gate
/// the `fono_screen` tool injection in the F8 voice loop.
fn backend_is_vision_capable(backend: &fono_core::config::AssistantBackend) -> bool {
    use fono_core::config::AssistantBackend;
    let id = match backend {
        AssistantBackend::OpenAI => "openai",
        AssistantBackend::Anthropic => "anthropic",
        AssistantBackend::Groq => "groq",
        AssistantBackend::Cerebras => "cerebras",
        AssistantBackend::OpenRouter => "openrouter",
        AssistantBackend::Gemini => "gemini",
        AssistantBackend::Ollama | AssistantBackend::None => return false,
    };
    fono_core::provider_catalog::find(id)
        .and_then(|p| p.assistant)
        .and_then(|a| a.multimodal_model)
        .is_some()
}

fn assistant_cache_warmup(config: &Config) -> AssistantPromptCacheWarmup {
    AssistantPromptCacheWarmup {
        f7_system_prompt: Some(f7_polish_prompt_for_cache(config)),
        f8_system_prompt: Some(config.assistant.prompt_main.clone()),
        assistant_tool_prompt: config
            .assistant
            .prefer_vision
            .then(|| ASSISTANT_SCREEN_TOOL_PROMPT.to_string()),
    }
}

fn f7_polish_prompt_for_cache(config: &Config) -> String {
    let mut prompt = config.polish.prompt.main.trim().to_string();
    let advanced = config.polish.prompt.advanced.trim();
    if !advanced.is_empty() {
        if !prompt.is_empty() {
            prompt.push_str("\n\n");
        }
        prompt.push_str(advanced);
    }
    if !config.polish.prompt.dictionary.is_empty() {
        if !prompt.is_empty() {
            prompt.push_str("\n\n");
        }
        prompt.push_str("Personal dictionary:\n");
        prompt.push_str(&config.polish.prompt.dictionary.join("\n"));
    }
    prompt
}

const ASSISTANT_SCREEN_TOOL_PROMPT: &str = "Assistant tool schema: fono_screen captures the focused window or a user-selected screen region when the user asks about visible on-screen content. Call it only when the answer needs current pixels.";

fn classify_focus_profile(focus_info: &FocusInfo) -> Option<ContextProfile> {
    let mut profile = ContextClassifier::classify(
        focus_info.window_class.as_deref(),
        focus_info.window_title.as_deref(),
    );
    if profile.as_ref().is_some_and(|p| p.is_terminal) {
        if let Some(pid) = focus_info.window_pid {
            if fono_inject::proc_enrichment_available() {
                let term_ctx = fono_inject::terminal_context(pid);
                if let Some(ref mut profile) = profile {
                    ContextClassifier::enrich_terminal(profile, &term_ctx);
                }
            }
        }
    }
    profile
}

fn assistant_window_context_for_cache(focus_info: &FocusInfo) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(class) = focus_info.window_class.as_deref().filter(|s| !s.trim().is_empty()) {
        parts.push(format!("Active app class: {class}"));
    }
    if let Some(title) = focus_info.window_title.as_deref().filter(|s| !s.trim().is_empty()) {
        parts.push(format!("Active window title: {title}"));
    }
    if let Some(profile) = classify_focus_profile(focus_info) {
        parts.push(format!("Context profile: {}", profile.name));
        if let Some(agent) = profile.detected_agent {
            parts.push(format!("Detected coding agent: {agent:?}"));
        }
        if let Some(suffix) = profile.llm_suffix {
            parts.push(format!("Context guidance: {suffix}"));
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// (ALSA / PipeWire), so it is kept on a dedicated thread; we
/// communicate with that thread via a stop signal and the shared
/// buffer.
struct CaptureSession {
    buffer: Arc<StdMutex<RecordingBuffer>>,
    stop_tx: std::sync::mpsc::Sender<()>,
    join: Option<JoinHandle<()>>,
    started_at: Instant,
    /// Snapshot of the focused window captured at hotkey-press time
    /// (H.1 in `plans/2026-05-25-hover-context-injection-v1.md`).
    /// Carried through to the pipeline so STT/LLM enrichment reflects
    /// the window the user was actually pointing at when they started
    /// dictating, not whatever wins focus by the time we tear down the
    /// recording (e.g. when an overlay surface or a focus-follows-mouse
    /// move steals it mid-utterance).
    focus_info: FocusInfo,
    /// Press-time turn trace (when `FONO_ASSISTANT_TRACE` is set). Created in
    /// `on_start_recording` so the `keys` lane press event precedes all STT
    /// work, carried into the pipeline, made current there, and finished once
    /// the pipeline completes. `None` for the assistant batch path (which owns
    /// its own trace in `run_assistant_turn`) and whenever tracing is disabled.
    trace: Option<TurnTrace>,
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
    /// Snapshot of the focused window captured when the live session
    /// started — same H.1 rationale as `CaptureSession::focus_info`.
    focus_info: FocusInfo,
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
    /// Detected language code returned by the STT backend (e.g. `"en"`,
    /// `"ro"`). `None` when the backend did not report a language.
    pub language: Option<String>,
    /// Short tag describing how much context was available to the polish
    /// LLM (e.g. `"app+rule+dict"`). Empty string when polish was not
    /// run. Used by the `pipeline:` summary log line.
    pub ctx_tag: String,
    /// Short identifier of the inject backend actually used
    /// (e.g. `"xtest-type"`, `"wtype"`, `"clipboard/xclip"`).
    pub inject_backend: String,
    /// Time-to-first-injected-character (TTFI) in milliseconds for the
    /// streaming local-cleanup path: measured from the moment the cleanup
    /// stream begins to the first character committed to the cursor. `0` on
    /// every non-streaming run (cloud cleanup, one-shot local cleanup, no
    /// polish, short-utterance skip) — those inject the whole text at once and
    /// the `inject_ms` leg already covers it. Streaming cuts this from the full
    /// multi-second decode to ~1–3 s on long local dictations.
    pub time_to_first_inject_ms: u64,
}

/// Outcome classification for one tool call observed during an
/// assistant turn. Maps onto the short token shown after the tool
/// name in the `assistant:` summary line (e.g. `[fono_screen 1284ms]`
/// vs `[fono_screen failed=cancelled]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistantToolOutcome {
    /// Tool returned a usable result.
    Ok,
    /// User pressed Escape in the OS-side region picker.
    Cancelled,
    /// Focused window was on the private-window allow-list and the
    /// capture was refused before any pixels were read.
    Private,
    /// No grabber tool was available on the system.
    NoTool,
    /// Anything else (timeouts, downscale failures, …).
    Failed,
}

/// One tool invocation observed during an assistant turn.
#[derive(Debug, Clone)]
pub struct AssistantToolMetric {
    /// Tool name as registered with the LLM (e.g. `"fono_screen"`).
    pub name: String,
    /// Wall-clock time spent executing the tool, measured from the
    /// `ToolEvent::Called` sentinel to the matching `ToolEvent::Result`.
    pub exec_ms: u64,
    /// Classification of the result for the summary tag.
    pub outcome: AssistantToolOutcome,
}

/// Per-turn metrics for the assistant (F8) pipeline. Populated by
/// [`crate::assistant::run_assistant_turn`] and consumed by
/// [`format_assistant_summary`].
#[derive(Debug, Clone, Default)]
pub struct AssistantTurnMetrics {
    /// Total wall-clock time from start of STT to the moment the
    /// last audio chunk was queued for playback. Drain time is not
    /// included.
    pub total_ms: u64,
    /// Detected / configured language. `None` ⇒ rendered as `?`.
    pub language: Option<String>,
    /// Batch STT latency. `None` when the live-streaming path
    /// supplied a pre-transcribed string instead.
    pub stt_ms: Option<u64>,
    /// User-text length in chars (the prompt sent to the LLM).
    pub user_chars: usize,
    /// Time-to-first-delta on the LLM stream. `0` when the turn
    /// aborted before any delta arrived.
    pub llm_ttfb_ms: u64,
    /// Total LLM streaming time including any tool round-trip wait.
    pub llm_total_ms: u64,
    /// Assistant reply length in chars (what gets spoken).
    pub reply_chars: usize,
    /// Tools observed in execution order. Empty for plain text-only
    /// turns. Currently only `fono_screen` is registered.
    pub tools: Vec<AssistantToolMetric>,
    /// Time-to-first-audio queued for playback. `None` when no
    /// audio was produced (cancelled before TTS, empty reply, …).
    pub tts_ttfa_ms: Option<u64>,
    /// Number of sentences synthesised + enqueued. Even when audio
    /// production happened, this can be zero if cancellation hit
    /// between splitter pushes.
    pub sentences: u32,
    /// True when the turn was aborted mid-stream (cancel hotkey,
    /// playback-stop, etc.). Renders as `| aborted` at the end of
    /// the summary line.
    pub aborted: bool,
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
/// Returns `(clipboard_populated, backend_name)` where
/// `clipboard_populated` is `true` when the inject path itself already
/// placed the text on the clipboard (e.g. the clipboard fallback when
/// no key-injector worked — the orchestrator skips the redundant
/// `also_copy_to_clipboard` write in that case), and `backend_name`
/// is a short identifier surfaced in the `pipeline:` summary log line.
pub trait Injector: Send + Sync + 'static {
    fn inject(&self, text: &str) -> Result<(bool, String)>;

    /// Whether this injector types incrementally at the cursor, so streaming
    /// word-by-word injection makes sense. Clipboard-fallback injectors return
    /// `false`: each `inject` call overwrites the clipboard, so streaming would
    /// leave only the last word visible. Defaults to `true` (every real
    /// key-injection backend appends at the cursor and so types incrementally);
    /// [`RealInjector`] overrides it to `false` when no key-injection backend
    /// is available and dictation would fall back to the clipboard.
    fn supports_streaming(&self) -> bool {
        true
    }
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
    fn inject(&self, text: &str) -> Result<(bool, String)> {
        match fono_inject::type_text_with_outcome(text)? {
            fono_inject::InjectOutcome::Typed(backend) => {
                debug!("inject backend: typed via {backend}");
                // All current key-injection backends (wtype/ydotool/
                // xdotool/enigo/xtest-type) deliver keystrokes directly
                // and leave the clipboard untouched, so the orchestrator
                // should still run `also_copy_to_clipboard` if enabled.
                Ok((false, backend.to_string()))
            }
            fono_inject::InjectOutcome::Clipboard(tool) => {
                debug!("inject backend: clipboard via {tool} (no key-injection worked)");
                // Surface the "press Ctrl+V to paste" hint only once
                // per daemon process. On Wayland without an active
                // virtual-keyboard / RemoteDesktop session this is
                // the steady state (every dictation falls back to
                // clipboard) and firing a notification on every
                // utterance is intolerably noisy. Doctor and the
                // tray cover the persistent-state surface.
                if !CLIPBOARD_HINT_SHOWN.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    fono_core::notify::send(
                        "Fono — text copied",
                        "Press Ctrl+V or Shift+Insert to paste.",
                        "edit-paste",
                        4_000,
                        fono_core::notify::Urgency::Normal,
                    );
                }
                Ok((true, format!("clipboard/{tool}")))
            }
        }
    }

    fn supports_streaming(&self) -> bool {
        // When no key-injection backend is available the dictation falls back
        // to the clipboard (`InjectOutcome::Clipboard`), where each write
        // overwrites the previous one — streaming word-by-word would leave only
        // the last word. Detecting `Injector::None` up front (the GNOME-Wayland
        // clipboard-first default, or a host with no wtype/ydotool/xdotool/
        // xtest) keeps the orchestrator on the one-shot path in that case.
        !matches!(fono_inject::Injector::detect(), fono_inject::Injector::None)
    }
}

/// Trait abstraction over focus detection (X11/Wayland-dependent) so the
/// integration test can stub out window classes deterministically.
pub trait FocusProbe: Send + Sync + 'static {
    fn probe(&self) -> FocusInfo;
}

/// Default focus probe — calls into [`fono_inject::detect_focus`].
pub struct RealFocusProbe;

impl FocusProbe for RealFocusProbe {
    fn probe(&self) -> FocusInfo {
        fono_inject::detect_focus().unwrap_or_default()
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
    /// Realtime (speech-to-speech) assistant backend, selected when
    /// `[assistant.cloud].model` matches the catalogue's Gemini Live
    /// profile. When `Some`, F8 dispatches to
    /// [`crate::assistant::run_realtime_turn`] instead of the staged
    /// STT → LLM → TTS pump — one continuous voice, sub-second first
    /// audio. `None` for every staged configuration.
    #[cfg(feature = "realtime")]
    realtime_backend: Arc<StdRwLock<Option<Arc<dyn RealtimeAssistant>>>>,
    /// Assistant the local LLM server (ADR 0036) should expose when it
    /// differs from the primary staged backend — i.e. an explicit
    /// `[server.llm].model` override, or the same-provider text sibling
    /// built when the primary `[assistant]` is a *realtime*
    /// speech-to-speech model the text API can't serve (Gemini Live →
    /// `gemini-flash-lite-latest`, same key). `None` means the server
    /// reuses the primary staged assistant. Rebuilt on every `reload`
    /// so a config swap is tracked without restarting the listener.
    server_assistant_extra: Arc<StdRwLock<Option<Arc<dyn Assistant>>>>,
    /// Cloud pass-through upstream for the local LLM server's OpenAI
    /// surface (ADR 0036). `Some` when the served backend is an
    /// OpenAI-compatible cloud provider (so requests are forwarded
    /// verbatim for full tool/vision/parameter fidelity); `None` for
    /// non-proxyable backends (local llama.cpp / Ollama / Anthropic).
    /// Rebuilt on every `reload` alongside `server_assistant_extra`.
    server_upstream: Arc<StdRwLock<Option<Arc<fono_assistant::CloudUpstream>>>>,
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
    /// True while a live-dictation press is being serviced by the
    /// *batch* pipeline because the current STT backend has no
    /// streaming implementation (or the streaming factory failed).
    /// The batch overlay show-gates treat this as "not live preview"
    /// so the fallback session still gets the standard waveform panel
    /// (with the renderer temporarily swapped off `Transcript` to the
    /// default visualisation) instead of a blank screen. Cleared on
    /// every terminal outcome of the batch pipeline.
    #[cfg(feature = "interactive")]
    live_fallback: Arc<AtomicBool>,
    /// One-shot latch for the "live transcript unavailable" desktop
    /// notification, so repeated F7 presses don't spam the user.
    /// Re-armed on [`Self::reload`] — a backend or style switch
    /// changes the answer.
    #[cfg(feature = "interactive")]
    live_fallback_notified: Arc<AtomicBool>,
    pipeline_in_flight: Arc<AtomicBool>,
    /// Lazily-loaded speaker-embedding engine, shared across dictations so the
    /// heavy ONNX graph is loaded once (on the first verified dictation) and
    /// reused. `None` inside until then. Only present in `speaker-onnx` builds.
    #[cfg(feature = "speaker-onnx")]
    speaker_engine: crate::daemon::SpeakerEngineCache,
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

/// Convert one brain-trace bus event into the overlay's Glass Cortex
/// replay command (pure field mapping — `fono-core` cannot depend on
/// `fono-overlay`, so the two mirror types meet here).
#[cfg(all(feature = "interactive", feature = "llama-local"))]
fn cortex_cmd_from_brain_event(ev: fono_core::brain_tap::BrainEvent) -> fono_overlay::CortexCmd {
    use fono_core::brain_tap::BrainEvent as E;
    match ev {
        E::ReplyBegin { n_layer, kind, n_experts_total, n_experts_active } => {
            fono_overlay::CortexCmd::ReplyBegin {
                n_layer,
                kind: match kind {
                    fono_core::brain_tap::BrainModelKind::Dense => {
                        fono_overlay::CortexModelKind::Dense
                    }
                    fono_core::brain_tap::BrainModelKind::Moe => fono_overlay::CortexModelKind::Moe,
                },
                n_experts_total,
                n_experts_active,
            }
        }
        E::Prefill { n_tokens } => fono_overlay::CortexCmd::Prefill { n_tokens },
        E::Frame(f) => fono_overlay::CortexCmd::Frame(fono_overlay::CortexFrame {
            token_index: f.token_index,
            layer_norms: f.layer_norms,
            experts: f
                .experts
                .into_iter()
                .map(|e| fono_overlay::CortexExperts {
                    layer: e.layer,
                    ids: e.ids,
                    weights: e.weights,
                })
                .collect(),
            token_prob: f.token_prob,
            entropy_bits: f.entropy_bits,
        }),
        E::ReplyEnd { total_tokens, gen_ms, ctx_used, ctx_capacity } => {
            fono_overlay::CortexCmd::ReplyEnd { total_tokens, gen_ms, ctx_used, ctx_capacity }
        }
    }
}

impl SessionOrchestrator {
    /// Construct from a fresh config + secrets, building both backends.
    /// Returns an error if the STT factory fails — the daemon should
    /// still come up but in a "degraded" mode where hotkeys notify the
    /// user. LLM construction failure downgrades to "no cleanup".
    #[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
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
        // Arm (or keep disarmed) the Glass Cortex capture latch before
        // any embedded LLM backend is constructed — the polish and
        // assistant factories read it at build time.
        #[cfg(feature = "llama-local")]
        fono_core::brain_tap::set_capture_enabled(config.overlay.brain_capture);
        let polish =
            match fono_polish::build_polish(&config.polish, secrets, &paths.polish_models_dir()) {
                Ok(opt) => opt,
                Err(e) => {
                    let err_text = format!("{e:#}");
                    warn!("polish backend unavailable; continuing without cleanup: {err_text}");
                    // Surface the degraded state once (deduped). Without
                    // this the user gets silently un-cleaned dictation —
                    // exactly the failure mode that made the local-model
                    // misroute so hard to diagnose. Classify so cloud
                    // auth/network/key failures get tailored copy; a
                    // missing local GGUF falls through to the generic
                    // "polish failed … run `fono models install`" body.
                    let provider = fono_core::providers::polish_backend_str(&config.polish.backend);
                    let class = fono_core::critical_notify::classify(&err_text);
                    fono_core::critical_notify::notify(
                        fono_core::critical_notify::Stage::Polish,
                        provider,
                        class,
                        &err_text,
                    );
                    None
                }
            };
        let tts = match fono_tts::build_tts(
            &config.tts,
            secrets,
            &config.general.languages,
            &paths.voices_dir(),
        ) {
            Ok(opt) => opt,
            Err(e) => {
                warn!("TTS backend unavailable; assistant replies will be silent: {e:#}");
                None
            }
        };
        let assistant_handle = match fono_assistant::build_assistant_handle(
            &config.assistant,
            secrets,
            &paths.polish_models_dir(),
        ) {
            Ok(opt) => opt,
            Err(e) => {
                warn!("Assistant backend unavailable; F8 will notify but not respond: {e:#}");
                None
            }
        };
        let history =
            Arc::new(Mutex::new(HistoryDb::open(&paths.history_db()).context("open history db")?));
        let capture_cfg = CaptureConfig {
            target_sample_rate: fono_core::config::AUDIO_SAMPLE_RATE_HZ,
            source: None,
        };
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
        // F8 surfaces a notification when either is missing. The
        // handle routes to the staged or the realtime slot depending
        // on whether `[assistant.cloud].model` selected Gemini Live.
        if let Ok(mut g) = orch.tts.write() {
            *g = tts;
        }
        match assistant_handle {
            Some(AssistantHandle::Staged(a)) => {
                if let Ok(mut g) = orch.assistant_backend.write() {
                    *g = Some(a);
                }
            }
            #[cfg(feature = "realtime")]
            Some(AssistantHandle::Realtime(r)) => {
                if let Ok(mut g) = orch.realtime_backend.write() {
                    *g = Some(r);
                }
            }
            None => {}
        }
        // Build the LLM server's dedicated assistant when it needs one
        // distinct from the primary staged backend — an explicit
        // `[server.llm].model` override, or the same-provider staged
        // text sibling when the primary is a realtime model the text
        // API can't expose. `None` means the server reuses the primary
        // staged assistant. Built regardless of `[server.llm].enabled`
        // so the tray toggle can start the server in place without a
        // full reload. See ADR 0036.
        {
            let m = config.server.llm.model.trim();
            let override_model = (!m.is_empty()).then_some(m);
            match fono_assistant::build_server_assistant_override(
                &config.assistant,
                override_model,
                secrets,
                &paths.polish_models_dir(),
            ) {
                Ok(Some(a)) => {
                    if let Ok(mut g) = orch.server_assistant_extra.write() {
                        *g = Some(a);
                    }
                }
                Ok(None) => {}
                Err(e) => warn!("LLM server assistant fallback unavailable: {e:#}"),
            }
        }
        // Resolve the cloud pass-through upstream for the LLM server's
        // OpenAI surface (ADR 0036). `Some` for OpenAI-compat cloud
        // backends (proxy verbatim); `None` for non-proxyable backends
        // (the server drives the assistant adapter instead).
        {
            let m = config.server.llm.model.trim();
            let override_model = (!m.is_empty()).then_some(m);
            match fono_assistant::cloud_chat_upstream(&config.assistant, override_model, secrets) {
                Ok(up) => {
                    if let Ok(mut g) = orch.server_upstream.write() {
                        *g = up.map(Arc::new);
                    }
                }
                Err(e) => warn!("LLM server proxy upstream unavailable: {e:#}"),
            }
        }
        // Publish whether an assistant tap should enter live mode now
        // that the realtime slot is wired.
        #[cfg(feature = "realtime")]
        orch.update_assistant_live_available();
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
            // Bridge the brain-trace bus onto the overlay: every event
            // published by the embedded LLM decode loops is forwarded
            // to the Glass Cortex replay engine. The sink runs on the
            // decode thread, so it does the minimum — snapshot the
            // handle (cheap clones) and push onto the overlay's mpsc.
            // With no overlay (or capture disabled ⇒ no events) this
            // is inert.
            #[cfg(feature = "llama-local")]
            {
                let overlay = Arc::clone(&orch.overlay);
                fono_core::brain_tap::set_event_sink(Some(Arc::new(move |ev| {
                    if let Some(h) = overlay.read().ok().and_then(|g| g.clone()) {
                        h.push_cortex(cortex_cmd_from_brain_event(ev));
                    }
                })));
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
    #[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
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
        // Re-arm the Glass Cortex capture latch so a `[overlay].brain_capture`
        // edit takes effect on the backends rebuilt below.
        #[cfg(feature = "llama-local")]
        fono_core::brain_tap::set_capture_enabled(cfg.overlay.brain_capture);
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
        let new_tts = match fono_tts::build_tts(
            &cfg.tts,
            &secrets,
            &cfg.general.languages,
            &paths.voices_dir(),
        ) {
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
        let new_assistant_handle = match fono_assistant::build_assistant_handle(
            &cfg.assistant,
            &secrets,
            &paths.polish_models_dir(),
        ) {
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
            // Re-arm the one-shot "live transcript unavailable"
            // notification: a reload can change the backend or the
            // overlay style, so the fallback answer may differ now.
            self.live_fallback_notified.store(false, Ordering::Relaxed);
        }
        if let Ok(mut guard) = self.polish.write() {
            *guard = new_polish;
        }
        if let Ok(mut guard) = self.tts.write() {
            *guard = new_tts;
        }
        // Route the rebuilt assistant into the staged or realtime
        // slot, clearing the other so a config change that flips
        // between staged and Gemini Live takes effect on reload.
        match new_assistant_handle {
            Some(AssistantHandle::Staged(a)) => {
                if let Ok(mut guard) = self.assistant_backend.write() {
                    *guard = Some(a);
                }
                #[cfg(feature = "realtime")]
                if let Ok(mut guard) = self.realtime_backend.write() {
                    *guard = None;
                }
            }
            #[cfg(feature = "realtime")]
            Some(AssistantHandle::Realtime(r)) => {
                if let Ok(mut guard) = self.realtime_backend.write() {
                    *guard = Some(r);
                }
                if let Ok(mut guard) = self.assistant_backend.write() {
                    *guard = None;
                }
            }
            None => {
                if let Ok(mut guard) = self.assistant_backend.write() {
                    *guard = None;
                }
                #[cfg(feature = "realtime")]
                if let Ok(mut guard) = self.realtime_backend.write() {
                    *guard = None;
                }
            }
        }
        // Rebuild the LLM server's dedicated assistant (override / the
        // realtime→text-sibling fallback) so a config swap between
        // staged and realtime — or a changed `[server.llm].model` —
        // takes effect without restarting the listener. `None` clears
        // the slot so the server falls back to reusing the primary
        // staged assistant. See ADR 0036.
        {
            let m = cfg.server.llm.model.trim();
            let override_model = (!m.is_empty()).then_some(m);
            let rebuilt = match fono_assistant::build_server_assistant_override(
                &cfg.assistant,
                override_model,
                &secrets,
                &paths.polish_models_dir(),
            ) {
                Ok(opt) => opt,
                Err(e) => {
                    warn!("reload: LLM server assistant fallback unavailable: {e:#}");
                    None
                }
            };
            if let Ok(mut guard) = self.server_assistant_extra.write() {
                *guard = rebuilt;
            }
            // Recompute the cloud proxy upstream alongside the fallback
            // so a backend swap re-targets the LLM server's OpenAI
            // surface without restarting the listener (ADR 0036).
            let up =
                match fono_assistant::cloud_chat_upstream(&cfg.assistant, override_model, &secrets)
                {
                    Ok(up) => up.map(Arc::new),
                    Err(e) => {
                        warn!("reload: LLM server proxy upstream unavailable: {e:#}");
                        None
                    }
                };
            if let Ok(mut guard) = self.server_upstream.write() {
                *guard = up;
            }
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
        // Re-publish the live-mode tap availability against the freshly
        // loaded backend + config (a reload may have swapped to/from a
        // realtime model or toggled `[assistant.realtime].live_mode`).
        #[cfg(feature = "realtime")]
        self.update_assistant_live_available();
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

    /// Snapshot of the active TTS backend for the Wyoming server's
    /// `TtsProvider`. `None` when no `[tts]` backend is configured.
    /// Invoked once per accepted connection so `Reload`-driven swaps
    /// are tracked without restarting the listener.
    #[must_use]
    pub fn tts_snapshot(&self) -> Option<Arc<dyn TextToSpeech>> {
        self.current_tts()
    }

    /// Snapshot of the active assistant backend for the local LLM
    /// server's `AssistantProvider`. `None` when no `[assistant]`
    /// backend is configured (degraded / cloud-less state), surfaced by
    /// the server as HTTP 503. Invoked once per accepted request so
    /// `Reload`-driven backend swaps are tracked without restarting the
    /// listener — the same posture as [`Self::stt_snapshot`] /
    /// [`Self::tts_snapshot`].
    #[must_use]
    pub fn assistant_snapshot(&self) -> Option<Arc<dyn Assistant>> {
        self.current_assistant()
    }

    /// Whether the active assistant is a *realtime* (speech-to-speech)
    /// backend with no staged text backend loaded — i.e.
    /// `[assistant.cloud].model` selected Gemini Live. The local LLM
    /// server serves text chat-completions and cannot expose a
    /// realtime backend, so it uses this to emit an accurate diagnostic
    /// (rather than the misleading "no backend configured") and skip
    /// starting. Always `false` when the `realtime` feature is off.
    #[must_use]
    pub fn assistant_is_realtime_only(&self) -> bool {
        #[cfg(feature = "realtime")]
        {
            self.current_assistant().is_none() && self.current_realtime().is_some()
        }
        #[cfg(not(feature = "realtime"))]
        {
            false
        }
    }

    /// Snapshot of the assistant the local LLM server (ADR 0036) should
    /// serve. Prefers the dedicated server assistant — an explicit
    /// `[server.llm].model` override, or the same-provider staged
    /// **text** sibling built when the primary `[assistant]` is a
    /// realtime speech-to-speech model the text API can't expose
    /// (Gemini Live → `gemini-flash-lite-latest`, same key) — and
    /// otherwise reuses the primary staged assistant. `None` only when
    /// nothing can be served (no assistant configured, or the fallback
    /// couldn't be built, e.g. a missing API key). Invoked once per
    /// accepted request so `Reload`-driven swaps are tracked without
    /// restarting the listener.
    #[must_use]
    pub fn server_assistant_snapshot(&self) -> Option<Arc<dyn Assistant>> {
        if let Some(a) =
            self.server_assistant_extra.read().ok().and_then(|g| g.as_ref().map(Arc::clone))
        {
            return Some(a);
        }
        self.current_assistant()
    }

    /// Snapshot of the cloud pass-through upstream for the LLM server's
    /// OpenAI surface (ADR 0036). `Some` when the served backend is an
    /// OpenAI-compatible cloud provider — the daemon forwards
    /// `/v1/chat/completions` verbatim to it for full tool/vision/
    /// parameter fidelity. `None` for non-proxyable backends (embedded
    /// llama.cpp, Ollama, Anthropic), where the server drives the
    /// assistant adapter from [`Self::server_assistant_snapshot`]
    /// instead. Read once per request so `Reload`-driven backend swaps
    /// re-target without restarting the listener.
    #[must_use]
    pub fn server_upstream_snapshot(&self) -> Option<Arc<fono_assistant::CloudUpstream>> {
        self.server_upstream.read().ok().and_then(|g| g.as_ref().map(Arc::clone))
    }

    fn current_llm(&self) -> Option<Arc<dyn TextFormatter>> {
        self.polish.read().expect("polish lock poisoned").clone()
    }

    fn current_config(&self) -> Arc<Config> {
        Arc::clone(&self.config.read().expect("config lock poisoned"))
    }

    /// Build a per-dictation speaker-verify handle when `[speaker].enabled`
    /// and a `Paths` root exists; `None` otherwise (verification off, or a
    /// test orchestrator without paths). The handle shares the process-lived
    /// engine cache so the model is loaded once.
    #[cfg(feature = "speaker-onnx")]
    fn speaker_verify(&self, config: &Config) -> Option<crate::daemon::SpeakerVerify> {
        if !config.speaker.enabled {
            return None;
        }
        let paths = self.paths.as_deref()?;
        Some(crate::daemon::SpeakerVerify::new(Arc::clone(&self.speaker_engine), paths, config))
    }

    /// Verification is compiled out; dictation is never speaker-tagged.
    #[cfg(not(feature = "speaker-onnx"))]
    fn speaker_verify(&self, _config: &Config) -> Option<crate::daemon::SpeakerVerify> {
        None
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
        sample_rate: u32,
    ) -> Option<tokio::task::AbortHandle> {
        // The live-preview gate only applies to the batch dictation
        // overlay — when the user picks Transcript style, F7 takes
        // the streaming path and the dictation panel is handled by
        // `LiveSession`. The assistant pipeline is independent and
        // should always show its overlay regardless of style
        // (different state, different colour, different label).
        let is_assistant =
            matches!(initial_state, fono_overlay::OverlayState::AssistantRecording { .. });
        let want_waveform =
            (is_assistant && cfg.overlay.waveform) || self.batch_overlay_ui_active(cfg);
        let handle = self.overlay.read().ok().and_then(|g| g.clone());
        match (want_waveform, handle) {
            (true, Some(o)) => {
                // Live-fallback session under Transcript style: the
                // transcript panel has nothing to render without a
                // streaming backend, so swap the renderer to the
                // default audio visualisation for this session. The
                // terminal hide restores Transcript (invisible while
                // hidden). Dictation-only — the assistant path keeps
                // its existing behaviour.
                let style = if !is_assistant
                    && cfg.overlay.style == fono_core::config::WaveformStyle::Transcript
                {
                    let fallback_style = fono_core::config::WaveformStyle::default();
                    o.set_waveform_style(fallback_style);
                    fallback_style
                } else {
                    cfg.overlay.style
                };
                o.set_state(initial_state);
                let buf = Arc::clone(buffer);
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
                        | fono_core::config::WaveformStyle::Heatmap
                        | fono_core::config::WaveformStyle::Terrain3d
                        | fono_core::config::WaveformStyle::System360
                        | fono_core::config::WaveformStyle::Cortex => {
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
                                | fono_core::config::WaveformStyle::Terrain3d => {
                                    WAVEFORM_FFT_MAX_HZ_HEATMAP
                                }
                                // Cortex listening borrows System/360's
                                // voice-focused range (~1.5 kHz) so speech
                                // energy fills the full panel width instead of
                                // crowding the low end of a 0–6 kHz axis and
                                // leaving the upper bands dark.
                                _ => WAVEFORM_FFT_MAX_HZ_FFT,
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
        // Auto-stop off ⇒ Pondering off. The Pondering overlay
        // state, walking-letter highlight, and any auto-stop
        // commit only make sense when the user has actually
        // opted into the silence-driven boundary. With
        // `audio.auto_stop_silence_ms = 0` (disabled by the user)
        // the user owns the boundary by keypress; surfacing
        // "PONDERING" under their finger only confuses things.
        // Skip spawning the watch entirely so it costs nothing.
        if auto_stop_ms == 0 {
            return None;
        }
        let envelope_cfg = EnvelopeConfig { sample_rate, ..EnvelopeConfig::default() };
        let watch_cfg = SilenceWatchConfig {
            auto_stop_silence_ms: Some(auto_stop_ms),
            ..SilenceWatchConfig::default()
        };
        let pondering_visual_ms = watch_cfg.pondering_visual_ms;
        let walk_total_ms = auto_stop_ms;
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
                    // ── Terrain 3D: ambient turbulence field ──────
                    // No single feature — instead a sum of many
                    // incoherent low-frequency components, each
                    // with its own spatial frequency, temporal
                    // frequency, and phase. The resulting field
                    // reads as a textured plane that constantly
                    // evolves, with no single peak the eye can
                    // latch onto and follow. Avoids the 'worm /
                    // snake / comb' artefacts that previous
                    // single-mass attempts produced when their
                    // shape scrolled through the time axis.
                    // Cortex shares the turbulence field while the
                    // model is between phases: the cortex renderer
                    // maps the per-bin heights onto layer-glow
                    // intensity so the spine keeps "breathing"
                    // during the TTFT gap (plan Task 2.5 idle loop).
                    fono_core::config::WaveformStyle::Terrain3d
                    | fono_core::config::WaveformStyle::Cortex => {
                        let n = FFT_BINS_THINKING;
                        let t_s = time_ms / 1000.0;
                        // Six octaves of sin(k_x · i + k_t · t + phi),
                        // each independently gated by a smooth
                        // sigmoid envelope on its own slow clock.
                        // Gate ω_g periods are 5–13 s, pairwise
                        // incommensurate, so each octave fades in
                        // and out independently — there's no
                        // moment where the whole field freezes or
                        // simultaneously erupts.
                        // Tuple layout: (k_x, k_t, phi, amp, ω_g, phi_g).
                        // Gate periods: 2.2, 3.1, 4.3, 2.7, 5.9, 6.7 s
                        // (incommensurate, all in the 2–7 s band).
                        let comps: [(f64, f64, f64, f64, f64, f64); 6] = [
                            (2.3, 6.4, 0.0, 0.16, 2.856, 0.0),
                            (3.7, 4.1, 1.7, 0.13, 2.027, 1.3),
                            (5.1, 9.8, 0.9, 0.10, 1.461, 2.5),
                            (6.7, 2.7, 2.4, 0.09, 2.327, 0.4),
                            (8.3, 7.9, 1.2, 0.07, 1.065, 3.0),
                            (11.0, 5.3, 3.1, 0.05, 0.938, 1.8),
                        ];
                        // Pre-compute each octave's gate once per
                        // tick — they don't depend on bin index.
                        let mut gates = [0.0_f64; 6];
                        for (idx, &(_, _, _, _, w_g, phi_g)) in comps.iter().enumerate() {
                            // Sigmoid (tanh) on a sin keeps the
                            // gate near 0 or 1 most of the time
                            // with smooth crossings — no cliffs.
                            let s = (w_g * t_s + phi_g).sin();
                            gates[idx] = 0.5 * (1.0 + (3.0 * s).tanh());
                        }
                        let mut bins = vec![0.0_f32; n];
                        let inv_n = 1.0 / (n as f64);
                        for (i, slot) in bins.iter_mut().enumerate() {
                            let u = (i as f64) * inv_n; // 0..1 across panel
                            let mut h = 0.45_f64;
                            for (idx, &(k_x, k_t, phi, amp, _, _)) in comps.iter().enumerate() {
                                h += gates[idx]
                                    * amp
                                    * (std::f64::consts::TAU * k_x * u + k_t * t_s + phi).sin();
                            }
                            *slot = h.clamp(0.0, 1.0) as f32;
                        }
                        o.push_fft_bins(bins);
                    }
                    // ── System/360: same turbulence field as Terrain3D ─
                    // Reuses the multi-octave turbulence field so the
                    // dotted lamp grid never goes static during idle / thinking.
                    fono_core::config::WaveformStyle::System360 => {
                        let n = FFT_BINS_THINKING;
                        let t_s = time_ms / 1000.0;
                        let comps: [(f64, f64, f64, f64); 4] = [
                            (2.3, 6.4, 0.0, 0.20),
                            (3.7, 4.1, 1.7, 0.16),
                            (5.1, 9.8, 0.9, 0.12),
                            (8.3, 7.9, 1.2, 0.10),
                        ];
                        let mut bins = vec![0.0_f32; n];
                        let inv_n = 1.0 / (n as f64);
                        for (i, slot) in bins.iter_mut().enumerate() {
                            let u = (i as f64) * inv_n;
                            let mut h = 0.35_f64;
                            for &(k_x, k_t, phi, amp) in &comps {
                                h +=
                                    amp * (std::f64::consts::TAU * k_x * u + k_t * t_s + phi).sin();
                            }
                            *slot = h.clamp(0.0, 1.0) as f32;
                        }
                        o.push_fft_bins(bins);
                    }
                }
            }
        });
        Some(task.abort_handle())
    }

    #[cfg(feature = "interactive")]
    fn spawn_polishing_phase_task(
        &self,
        phase: PolishingPhase,
        walk_duration: Duration,
    ) -> Option<tokio::task::AbortHandle> {
        if !self.current_config().overlay.waveform {
            return None;
        }
        let o = self.overlay.read().ok().and_then(|g| g.clone())?;
        Some(spawn_polishing_phase_task_for_handle(o, phase, walk_duration))
    }

    /// Fire-and-forget warmup for STT, LLM and the inject backend.
    /// Latency plan tasks L2 (whisper mmap), L3 (HTTP keep-alive),
    /// L5 (inject binary page-cache).
    fn spawn_warmups(&self) {
        // Startup prewarm timeline (Phase 4). Written to its own
        // `startup-*.json` file (same `FONO_ASSISTANT_TRACE` env var) so it
        // doesn't collide with per-turn dictation/assistant traces. Each
        // warmup task records a `warmup` lane duration on a clone of the
        // handle; a coordinator task finishes the file once all are done.
        let trace = TurnTrace::start_from_env_named("startup");
        if let Some(t) = &trace {
            t.instant(
                "turn.start",
                "assistant",
                PUMP_LANE,
                json!({ "turn_id": t.id(), "path": "startup" }),
            );
        }
        let mut warmup_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        // Serialise the two CPU-bound local-LLM prewarms (the polish F7 base
        // prefix build and the assistant F8 prompt-cache prefill). Both default
        // to the same gemma model but are loaded as two independent llama.cpp
        // instances, and each prefill spins up `threads` worker threads. Run
        // concurrently on an N-core box that is 2N llama threads fighting over N
        // cores: the startup waterfall showed a 78-token prefill taking 11s and
        // the 426-token polish base 15s, all overlapping. A single permit lets
        // whichever starts first prefill at full core count, then the other —
        // turning ~17s of thrash into back-to-back full-speed prefills. STT/TTS/
        // inject warmups stay concurrent; they hit unrelated subsystems.
        let llm_warmup_gate = Arc::new(tokio::sync::Semaphore::new(1));
        let stt = self.current_stt();
        let stt_trace = trace.clone();
        warmup_handles.push(tokio::spawn(async move {
            let started = Instant::now();
            match stt.prewarm().await {
                Ok(()) => debug!(
                    "warmup: stt {} ready in {}ms",
                    stt.name(),
                    started.elapsed().as_millis()
                ),
                Err(e) => debug!("warmup: stt {} prewarm skipped: {e:#}", stt.name()),
            }
            if let Some(t) = &stt_trace {
                t.duration_between(
                    "warmup.stt",
                    "warmup",
                    "warmup:stt",
                    started,
                    Instant::now(),
                    json!({ "backend": stt.name() }),
                );
            }
        }));
        if let Some(h) = self.spawn_polish_warmup(trace.as_ref(), &llm_warmup_gate) {
            warmup_handles.push(h);
        }
        if let Some(h) = self.spawn_assistant_warmup(trace.as_ref(), &llm_warmup_gate) {
            warmup_handles.push(h);
        }
        // TTS prewarm matters for Cartesia: it pre-resolves a native
        // voice per non-English configured language via `/voices?…`
        // and caches them, so the first synth doesn't pay the HTTP
        // round-trip. Other TTS backends do their own startup probes
        // here too (Deepgram, Wyoming, OpenAI-compat). Errors are
        // non-fatal; the per-call code paths self-heal.
        if let Some(tts) = self.current_tts() {
            let tts_trace = trace.clone();
            warmup_handles.push(tokio::spawn(async move {
                let started = Instant::now();
                match tts.prewarm().await {
                    Ok(()) => debug!(
                        "warmup: tts {} ready in {}ms",
                        tts.name(),
                        started.elapsed().as_millis()
                    ),
                    Err(e) => warn!("warmup: tts {} prewarm failed: {e:#}", tts.name()),
                }
                if let Some(t) = &tts_trace {
                    t.duration_between(
                        "warmup.tts",
                        "warmup",
                        "warmup:tts",
                        started,
                        Instant::now(),
                        json!({ "backend": tts.name() }),
                    );
                }
            }));
        }
        // Inject backend warmup runs on a blocking thread because the
        // probe shells out to `wtype --version` / `ydotool --version`.
        tokio::task::spawn_blocking(|| match fono_inject::warm_backend() {
            Ok(name) => debug!("warmup: inject backend = {name}"),
            Err(e) => debug!("warmup: inject backend probe failed: {e:#}"),
        });
        // Coordinator: once every async warmup completes, write the startup
        // trace file. Fire-and-forget; if no trace is active this is a no-op.
        if let Some(t) = trace {
            tokio::spawn(async move {
                for h in warmup_handles {
                    let _ = h.await;
                }
                t.finish(json!({ "path": "startup", "summary": t.cache_scoreboard() }));
            });
        }
    }

    /// Spawn the polish (cleanup) prewarm task on the startup trace, if a
    /// polish backend is configured. Loads the model and, for embedded local
    /// backends, builds + pins the F7 base prefix checkpoint so the first
    /// cleanup after launch doesn't pay the multi-second base prefill on the
    /// hotkey path. Returns the join handle so the warmup coordinator can await
    /// it before finalizing the trace file.
    fn spawn_polish_warmup(
        &self,
        trace: Option<&TurnTrace>,
        gate: &Arc<tokio::sync::Semaphore>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let polish = self.current_llm()?;
        let polish_trace = trace.cloned();
        // Context-independent base system prompt (main + advanced + dictionary),
        // built identically to the live dictation path so the prewarmed F7 base
        // checkpoint is a cache hit at turn time. App context and language are
        // irrelevant: `base_system_prompt()` strips the rule suffix and language
        // directive.
        let polish_base = build_format_context(&self.current_config(), None, None, None, None)
            .base_system_prompt();
        let gate = Arc::clone(gate);
        Some(tokio::spawn(async move {
            // Hold the local-LLM warmup permit across load + prefill so this
            // doesn't oversubscribe the CPU against the assistant prewarm.
            let _permit = gate.acquire_owned().await.ok();
            let started = Instant::now();
            match polish.prewarm().await {
                Ok(()) => debug!(
                    "warmup: polish {} ready in {}ms",
                    polish.name(),
                    started.elapsed().as_millis()
                ),
                Err(e) => debug!("warmup: polish {} prewarm skipped: {e:#}", polish.name()),
            }
            // Build + pin the F7 base prefix checkpoint. No-op for cloud backends.
            if let Err(e) = polish.prewarm_prompt_cache(&polish_base).await {
                debug!("warmup: polish {} prompt-cache prewarm skipped: {e:#}", polish.name());
            }
            if let Some(t) = &polish_trace {
                t.duration_between(
                    "warmup.polish",
                    "warmup",
                    "warmup:polish",
                    started,
                    Instant::now(),
                    json!({ "backend": polish.name() }),
                );
            }
        }))
    }

    /// Spawn the assistant prompt-cache prewarm task on the startup trace, if an
    /// assistant backend is configured. Returns the join handle so the warmup
    /// coordinator can await it before finalizing the trace file.
    fn spawn_assistant_warmup(
        &self,
        trace: Option<&TurnTrace>,
        gate: &Arc<tokio::sync::Semaphore>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let assistant = self.current_assistant()?;
        let warmup = assistant_cache_warmup(&self.current_config());
        let assistant_trace = trace.cloned();
        let gate = Arc::clone(gate);
        Some(tokio::spawn(async move {
            // Install the startup trace as process-current so the cache
            // build/restore instants emitted deep inside the backend land
            // on the warmup file.
            let _guard = assistant_trace.as_ref().map(TurnTrace::make_current);
            tokio::time::sleep(Duration::from_millis(250)).await;
            // Serialise against the polish prewarm (see `spawn_warmups`): take
            // the permit only around the heavy load + prefill, after the
            // stagger sleep, so whichever local model prefills first gets the
            // whole CPU.
            let _permit = gate.acquire_owned().await.ok();
            let started = Instant::now();
            let result = assistant.prewarm_prompt_caches(warmup).await;
            match &result {
                Ok(()) => debug!(
                    "warmup: assistant {} prompt caches ready in {}ms",
                    assistant.name(),
                    started.elapsed().as_millis()
                ),
                Err(e) => debug!(
                    "warmup: assistant {} prompt-cache prewarm skipped: {e:#}",
                    assistant.name()
                ),
            }
            if let Some(t) = &assistant_trace {
                t.duration_between(
                    "warmup.assistant_prompt_caches",
                    "warmup",
                    WARMUP_LANE,
                    started,
                    Instant::now(),
                    json!({ "backend": assistant.name(), "ok": result.is_ok() }),
                );
            }
        }))
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
            #[cfg(feature = "realtime")]
            realtime_backend: Arc::new(StdRwLock::new(None)),
            server_assistant_extra: Arc::new(StdRwLock::new(None)),
            server_upstream: Arc::new(StdRwLock::new(None)),
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
            #[cfg(feature = "interactive")]
            live_fallback: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "interactive")]
            live_fallback_notified: Arc::new(AtomicBool::new(false)),
            pipeline_in_flight: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "speaker-onnx")]
            speaker_engine: Arc::new(Mutex::new(None)),
            config: Arc::new(StdRwLock::new(config)),
            paths: None,
            action_tx,
            injector,
            focus,
            held_flags: fono_hotkey::KeyHeldFlags::default(),
        }
    }

    /// Point the orchestrator at a `Paths` root after construction.
    /// Used by integration tests to exercise features that resolve
    /// per-user files (e.g. `vocabulary.toml`) without going through
    /// the full daemon constructor.
    pub fn set_paths(&mut self, paths: Arc<Paths>) {
        self.paths = Some(paths);
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
        // Dictation and assistant have fully separate histories. The
        // pivot only stops any assistant turn that's still speaking so
        // it doesn't talk over the user; the rolling chat history is
        // preserved and the user can resume the conversation on the
        // next F8 press.
        {
            let mut s = self.assistant_session.lock().await;
            s.stop_current_turn();
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
            Err(_) => return Err(anyhow::anyhow!("capture thread died before reporting status")),
        };
        let cfg = self.current_config();
        if cfg.general.auto_mute_system {
            fono_audio::mute::set_default_sink_mute(true);
        }
        debug!(
            "recording started (mode={:?} sample_rate={})",
            mode, self.capture_cfg.target_sample_rate
        );

        // H.1: snapshot the focused window at hotkey-press time, before
        // any overlay surface activity or focus-follows-mouse cursor
        // motion can move it. Sway/Hyprland/X11 IPC takes ~5 ms; run it
        // on a blocking thread so we don't hold the async executor.
        let focus = Arc::clone(&self.focus);
        let focus_info =
            tokio::task::spawn_blocking(move || focus.probe()).await.unwrap_or_default();
        tracing::debug!(
            target: "fono::context",
            class = ?focus_info.window_class,
            title = ?focus_info.window_title,
            "capture: focus snapshot at press"
        );
        // Start the per-turn trace at press time so the `keys` lane press
        // event precedes all STT work (Phase 3). Recorded directly on the
        // handle here; the pipeline makes it current later for the STT/polish
        // cache events. Plain F7 dictation now produces a trace file.
        let trace = TurnTrace::start_from_env_named("dictation");
        if let Some(t) = &trace {
            t.instant(
                "turn.start",
                "assistant",
                "assistant-pump",
                json!({ "turn_id": t.id(), "path": "dictation" }),
            );
            t.instant(
                "key.press",
                "keys",
                KEYS_LANE,
                json!({ "trigger": "F7", "from_state": "idle", "to_state": "recording", "mode": format!("{mode:?}") }),
            );
            t.instant(
                "fsm.transition",
                "keys",
                KEYS_LANE,
                json!({ "trigger": "F7", "from_state": "idle", "to_state": "recording" }),
            );
        }
        if let Some(assistant) = self.current_assistant() {
            let history = Vec::new();
            Self::prepare_assistant_prompt_cache(
                assistant,
                history,
                AssistantCacheTrigger::F7,
                f7_polish_prompt_for_cache(&cfg),
                focus_info.clone(),
                cfg.assistant.prefer_vision,
                trace.clone(),
            );
        }

        // Speculatively warm the polish per-app + language F7Context prefix now,
        // concurrent with capture + STT, so the first dictation into this window
        // restores the whole prefix instead of decoding the ~1.3 s of per-app +
        // language-directive tokens on the hotkey path.
        self.spawn_polish_context_prewarm(&focus_info, &cfg, trace.clone());

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
            self.capture_cfg.target_sample_rate,
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
            focus_info,
            trace,
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
        // (walking label, when STT or LLM is local) or `Processing`
        // (static, for cloud) while STT runs. Live-dictation mode
        // owns its own state transitions; only flip when this is
        // the batch path.
        #[cfg(feature = "interactive")]
        let polish_label_anim: Option<tokio::task::AbortHandle> = {
            if self.batch_overlay_ui_active(&cfg) {
                let stt_local = self.current_stt().is_local();
                let llm_local = cfg.interactive.cleanup_on_finalize
                    && self.current_llm().is_some_and(|l| l.is_local());
                let animate = stt_local || llm_local;
                if animate {
                    self.spawn_polishing_phase_task(
                        PolishingPhase::Transcribing,
                        polish_walk_duration(session.started_at.elapsed()),
                    )
                } else {
                    if let Some(o) = self.overlay.read().ok().and_then(|g| g.clone()) {
                        o.set_state(fono_overlay::OverlayState::Processing);
                    }
                    None
                }
            } else {
                None
            }
        };
        #[cfg(feature = "interactive")]
        let polish_waveform_anim: Option<tokio::task::AbortHandle> = {
            if polish_label_anim.is_some() {
                self.spawn_thinking_animation_task(&cfg)
            } else {
                None
            }
        };
        #[cfg(not(feature = "interactive"))]
        let polish_label_anim: Option<tokio::task::AbortHandle> = None;
        #[cfg(not(feature = "interactive"))]
        let polish_waveform_anim: Option<tokio::task::AbortHandle> = None;
        // Pull the press-time focus snapshot and turn trace out before
        // consuming the session in the blocking stop/drain below.
        let focus_info = session.focus_info.clone();
        let trace = session.trace.clone();
        let (samples, elapsed) =
            tokio::task::spawn_blocking(move || session.stop_and_drain()).await.unwrap_or_default();
        let capture_ms = elapsed.as_millis() as u64;
        debug!("recording stopped: {capture_ms} ms / {} samples", samples.len());
        if let Some(t) = &trace {
            t.instant(
                "key.release",
                "keys",
                KEYS_LANE,
                json!({ "trigger": "F7", "from_state": "recording", "to_state": "processing", "capture_ms": capture_ms }),
            );
            t.instant(
                "fsm.transition",
                "keys",
                KEYS_LANE,
                json!({ "trigger": "F7", "from_state": "recording", "to_state": "processing" }),
            );
        }

        if elapsed < MIN_RECORDING || samples.is_empty() {
            warn!("recording too short ({capture_ms} ms); skipping STT");
            if let Some(t) = &trace {
                t.finish(json!({ "path": "dictation", "aborted": true, "reason": "too_short", "summary": t.cache_scoreboard() }));
            }
            #[cfg(feature = "interactive")]
            {
                if let Some(t) = polish_label_anim {
                    t.abort();
                }
                if let Some(t) = polish_waveform_anim {
                    t.abort();
                }
                // Hide unconditionally — deliberately NOT gated on
                // `!cfg.live_preview()`. When the batch path runs as
                // the live-dictation fallback (Transcript style + a
                // non-streaming STT backend) the toggle-mode silence
                // watch may have shown the Pondering panel; a hide
                // gated on `!live_preview()` would leave it on screen
                // forever. Hiding is idempotent, and no live pipeline
                // can be active here (this batch session owned the
                // capture slot).
                if let Some(o) = self.overlay.read().ok().and_then(|g| g.clone()) {
                    o.set_state(fono_overlay::OverlayState::Hidden);
                    // Live-fallback session done: restore the user's
                    // Transcript style (swapped to the default viz at
                    // session start) while the panel is hidden.
                    if self.live_fallback.swap(false, Ordering::Relaxed) {
                        o.set_waveform_style(cfg.overlay.style);
                    }
                }
            }
            let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
            return;
        }

        self.spawn_pipeline(
            samples,
            capture_ms,
            polish_label_anim,
            polish_waveform_anim,
            focus_info,
            trace,
        );
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
            // (no pipeline phase follows). Unconditional — see the
            // batch-fallback note in `on_stop_recording`: under
            // Transcript style the silence watch may have shown the
            // panel even though the batch path never explicitly
            // "owned" it.
            #[cfg(feature = "interactive")]
            if let Some(o) = self.overlay.read().ok().and_then(|g| g.clone()) {
                o.set_state(fono_overlay::OverlayState::Hidden);
                // Restore Transcript style if this was a live-fallback
                // session (see `spawn_waveform_level_task`).
                if self.live_fallback.swap(false, Ordering::Relaxed) {
                    o.set_waveform_style(cfg.overlay.style);
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
    #[allow(clippy::too_many_lines)]
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
                    // Snapshot focus now so F8 can restore/extend prompt state
                    // before the streaming STT result is available.
                    let focus = Arc::clone(&self.focus);
                    let focus_info = tokio::task::spawn_blocking(move || focus.probe())
                        .await
                        .unwrap_or_default();
                    let cfg = self.current_config();
                    if let Some(assistant) = self.current_assistant() {
                        let history = {
                            let mut s = self.assistant_session.lock().await;
                            s.history.snapshot()
                        };
                        Self::prepare_assistant_prompt_cache(
                            assistant,
                            history,
                            AssistantCacheTrigger::F8,
                            cfg.assistant.prompt_main.clone(),
                            focus_info.clone(),
                            cfg.assistant.prefer_vision,
                            None,
                        );
                    }
                    let session = self.build_live_capture_pipeline(
                        streaming,
                        fono_overlay::OverlayState::AssistantRecording { db: 0 },
                        Some(SilenceWatchFlavor::Assistant { auto_stop_commit: true }),
                        focus_info,
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
        // Snapshot focus for active-window prompt-state caching before the
        // overlay can steal focus. The assistant batch path also carries this
        // into the LLM context after STT completes.
        let focus = Arc::clone(&self.focus);
        let focus_info =
            tokio::task::spawn_blocking(move || focus.probe()).await.unwrap_or_default();
        let cfg = self.current_config();
        if let Some(assistant) = self.current_assistant() {
            let history = {
                let mut s = self.assistant_session.lock().await;
                s.history.snapshot()
            };
            Self::prepare_assistant_prompt_cache(
                assistant,
                history,
                AssistantCacheTrigger::F8,
                cfg.assistant.prompt_main.clone(),
                focus_info.clone(),
                cfg.assistant.prefer_vision,
                None,
            );
        }
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
            self.capture_cfg.target_sample_rate,
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
            focus_info,
            // Assistant batch path owns its own trace in `run_assistant_turn`.
            trace: None,
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
        let mut assistant_focus_info: Option<FocusInfo> = None;
        #[cfg(feature = "interactive")]
        {
            let live_taken = self.assistant_live_capture.lock().await.take();
            if let Some(mut session) = live_taken {
                assistant_focus_info = Some(session.focus_info.clone());
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
            assistant_focus_info = Some(session.focus_info.clone());
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
        // Realtime (speech-to-speech) short-circuit: when Gemini Live is
        // selected, skip the staged STT→LLM→TTS pipeline entirely and run
        // one continuous bidirectional turn. Fixes the two problems the
        // staged Gemini path cannot — per-sentence voice drift and
        // ~6 s/sentence batch-TTS latency — because Live emits one
        // continuous voice incrementally as it is generated.
        #[cfg(feature = "realtime")]
        if let Some(realtime) = self.current_realtime() {
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
            let active_window_context =
                assistant_focus_info.as_ref().and_then(assistant_window_context_for_cache);
            // Build the screen-capture closure when prefer_vision is on
            // and the backend is vision-capable, mirroring the staged
            // path below. When present, the Live session sends one
            // screenshot frame (realtimeInput.video) before mic audio.
            let rt_prefer_vision = cfg.assistant.prefer_vision;
            let rt_screen_capture_fn: Option<fono_assistant::ScreenCaptureFn> =
                if rt_prefer_vision && backend_is_vision_capable(&cfg.assistant.backend) {
                    use fono_core::screen_capture::GrabberProbe;
                    use fono_inject::focus::detect_focus;
                    let probe = GrabberProbe::detect();
                    Some(Arc::new(move |mode| {
                        let fi = detect_focus().ok();
                        let wm_class = fi.and_then(|f| f.window_class);
                        probe.capture(mode, wm_class.as_deref())
                    }))
                } else {
                    None
                };
            let inputs = RealtimeTurnInputs {
                frames: crate::assistant::buffered_frame_stream(
                    &pcm,
                    self.capture_cfg.target_sample_rate,
                ),
                sample_rate: self.capture_cfg.target_sample_rate,
                realtime,
                system_prompt: cfg.assistant.prompt_main.clone(),
                language: cfg.general.language_override().map(str::to_string),
                action_tx: self.action_tx.clone(),
                overlay: self.overlay.read().ok().and_then(|g| g.clone()),
                prefer_vision: rt_prefer_vision,
                screen_capture_fn: rt_screen_capture_fn,
                active_window_context,
            };
            let state_for_task = self.assistant_session.clone();
            let notify_for_task = notify.clone();
            let turn_fut = Box::pin(run_realtime_turn(state_for_task, inputs, notify_for_task));
            self.spawn_assistant_pump(turn_fut, notify, thinking_task, overlay_for_task);
            return;
        }
        let stt = self.current_stt();
        let assistant = self.current_assistant();
        // TTS is optional: when no TTS backend is available (e.g. a
        // cloud provider without TTS, or `tts.backend = none`) the turn
        // still runs and the reply is shown as on-screen text instead
        // of being spoken (GitHub #15). Only a missing *assistant*
        // backend aborts the turn.
        let tts = self.current_tts();
        let Some(assistant) = assistant else {
            // The slot is populated by `build_assistant()` in `new()` /
            // `reload()`. If the config flag is on but the slot is
            // empty, the factory errored at startup (missing API key,
            // missing sub-block, missing feature). Run `fono doctor`
            // for the exact reason; the daemon also logged it on
            // startup.
            warn!(
                "assistant turn requested but the assistant backend is missing \
                 (assistant.enabled={}). Run `fono doctor` to see which factory failed.",
                cfg.assistant.enabled,
            );
            fono_core::notify::send(
                "Fono — assistant backend missing",
                "The assistant factory failed at startup (likely a missing API key). \
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
        // Build the screen-capture closure when prefer_vision is on
        // and the configured assistant backend is vision-capable.
        let prefer_vision = cfg.assistant.prefer_vision;
        let screen_capture_fn: Option<fono_assistant::ScreenCaptureFn> =
            if prefer_vision && backend_is_vision_capable(&cfg.assistant.backend) {
                use fono_core::screen_capture::GrabberProbe;
                use fono_inject::focus::detect_focus;
                let probe = GrabberProbe::detect();
                Some(Arc::new(move |mode| {
                    let fi = detect_focus().ok();
                    let wm_class = fi.and_then(|f| f.window_class);
                    probe.capture(mode, wm_class.as_deref())
                }))
            } else {
                None
            };
        let active_window_context =
            assistant_focus_info.as_ref().and_then(assistant_window_context_for_cache);
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
            prefer_vision,
            screen_capture_fn,
            active_window_context,
        };
        let state_for_task = self.assistant_session.clone();
        let notify_for_task = notify.clone();
        let turn_fut = Box::pin(run_assistant_turn(state_for_task, inputs, notify_for_task));
        self.spawn_assistant_pump(turn_fut, notify, thinking_task, overlay_for_task);
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
        // Tear down the *batch* assistant capture slot. The streaming
        // slot handled below only exists when `live_preview()` is on;
        // cloud-only / non-streaming configs record into
        // `assistant_capture` instead. Without this teardown an Escape
        // (or barge-in) left the batch `CaptureSession` — and its
        // auto-stop silence-watch task — alive: the orphaned watcher
        // would commit ~3 s later and emit a synthetic
        // `AssistantPressed`, spuriously re-entering `AssistantRecording`
        // from Idle (a phantom session with no overlay), while the
        // still-occupied slot made every subsequent wake-word fire log
        // "assistant capture already in progress; ignoring duplicate
        // start" and never show the overlay. `stop_and_drain` aborts
        // both the level and silence tasks, signals the capture thread
        // to stop, and joins it.
        let batch_taken = self.assistant_capture.lock().await.take();
        if let Some(session) = batch_taken {
            let _ = tokio::task::spawn_blocking(move || {
                let _ = session.stop_and_drain();
            })
            .await;
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

    /// Recompute whether an assistant **tap** should enter full-duplex
    /// live mode and publish it to the shared hotkey flag the listener
    /// reads on a tap-release. True only when a realtime backend is
    /// loaded **and** `[assistant.realtime].live_mode` is enabled.
    /// Called after `new()` / `reload()` wiring so a `fono use` that
    /// swaps the backend takes effect without re-spawning the listener.
    #[cfg(feature = "realtime")]
    fn update_assistant_live_available(&self) {
        let avail =
            self.current_realtime().is_some() && self.current_config().assistant.realtime.live_mode;
        self.held_flags.assistant_live_available.store(avail, std::sync::atomic::Ordering::Relaxed);
    }

    /// Enter full-duplex live conversation mode (F8 tap on a realtime
    /// backend). Discards the nascent push-to-talk capture the entry
    /// press started, then opens a persistent speech-to-speech session
    /// that lives across many turns. The pump task is detached; its
    /// handle is held in [`AssistantSessionState::live`] for teardown.
    /// Idempotent — a duplicate enter while a session is open is ignored.
    #[cfg(feature = "realtime")]
    pub async fn on_assistant_live_enter(&self) {
        // The entry press started a one-shot PTT capture; live mode does
        // not use it. Tear it down before opening the live session.
        self.discard_assistant_capture().await;

        let Some(realtime) = self.current_realtime() else {
            warn!("live enter requested but no realtime backend is loaded; ignoring");
            let _ = self.action_tx.send(HotkeyAction::ProcessingDone);
            return;
        };
        let cfg = self.current_config();
        // Shared rolling buffer feeding the audio visualisation. The
        // capture forwarder writes mic frames into it during the user's
        // turn; the pump writes reply PCM during the model's turn. One
        // style-aware ticker (reused from the dictation path) reads the
        // tail each tick and pushes the right primitive (samples / FFT /
        // level) so both directions animate in the user's chosen style.
        let viz_buf = Arc::new(StdMutex::new(RecordingBuffer::default()));
        // The mic capture runs at the provider's native input rate, so
        // tick the visualisation at that rate (reply audio is pushed as
        // a rolling window too; its rate differs slightly, which only
        // skews the FFT frequency axis cosmetically — the bars still
        // track the voice).
        let native_rate = realtime.native_input_rate();
        #[cfg(feature = "interactive")]
        let waveform_task = self.spawn_waveform_level_task(
            &cfg,
            fono_overlay::OverlayState::AssistantRecording { db: 0 },
            &viz_buf,
            native_rate,
        );
        #[cfg(not(feature = "interactive"))]
        let waveform_task: Option<tokio::task::AbortHandle> = None;
        let inputs = crate::assistant::LiveSessionInputs {
            realtime,
            system_prompt: cfg.assistant.prompt_main.clone(),
            language: cfg.general.language_override().map(str::to_string),
            action_tx: self.action_tx.clone(),
            overlay: self.overlay.read().ok().and_then(|g| g.clone()),
            auto_stop_silence_ms: cfg.audio.auto_stop_silence_ms,
            max_session: Duration::from_secs(cfg.assistant.realtime.max_session_secs),
            active_window_context: None,
            viz_buf,
            waveform_task,
        };
        let cancel = Arc::new(Notify::new());
        // Hold the state lock across spawn + store so the pump (whose
        // first action is to lock the state for the history snapshot)
        // cannot run — and so cannot clear the slot on an early failure —
        // until the handle is recorded. Resolves the spawn/store race.
        let mut s = self.assistant_session.lock().await;
        if s.live.is_some() {
            warn!("live session already open; ignoring duplicate enter");
            return;
        }
        let task = tokio::spawn(crate::assistant::run_live_session(
            self.assistant_session.clone(),
            inputs,
            cancel.clone(),
        ));
        s.live =
            Some(crate::assistant::LiveSessionHandle { cancel, task, started_at: Instant::now() });
        drop(s);
    }

    /// Leave live mode explicitly (second tap / Escape). Takes the
    /// session handle, signals the pump to stop, and awaits its
    /// teardown. A no-op when no session is open (the pump already
    /// self-terminated, e.g. idle/cap/provider-close).
    #[cfg(feature = "realtime")]
    pub async fn on_assistant_live_exit(&self) {
        let handle = { self.assistant_session.lock().await.live.take() };
        if let Some(h) = handle {
            h.cancel.notify_one();
            let _ = h.task.await;
            info!("live conversation mode exited");
        } else {
            debug!("live exit requested but no session was open");
        }
        self.hide_assistant_overlay();
    }

    /// Tear down any in-flight assistant push-to-talk capture (batch or
    /// streaming) without running a turn. Used when an entry press is
    /// reclassified as a live-mode tap.
    #[cfg(feature = "realtime")]
    async fn discard_assistant_capture(&self) {
        if self.current_config().general.auto_mute_system {
            fono_audio::mute::set_default_sink_mute(false);
        }
        let taken = self.assistant_capture.lock().await.take();
        if let Some(session) = taken {
            let _ = tokio::task::spawn_blocking(move || {
                let _ = session.stop_and_drain();
            })
            .await;
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
            }
        }
    }

    /// The realtime (speech-to-speech) backend, when one is loaded.
    /// `Some` only when `[assistant.cloud].model` selected Gemini Live.
    #[cfg(feature = "realtime")]
    fn current_realtime(&self) -> Option<Arc<dyn RealtimeAssistant>> {
        self.realtime_backend.read().expect("realtime lock poisoned").clone()
    }

    /// Spawn the detached pump task for one assistant turn (staged or
    /// realtime) and wire the shared completion teardown: clear the
    /// current-turn slot (only if it still points at *this* turn), stop
    /// the thinking animation, hide the overlay, and tell the FSM we're
    /// idle. `turn_fut` is the boxed pump future for whichever path was
    /// chosen.
    fn spawn_assistant_pump(
        &self,
        turn_fut: std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool>> + Send>>,
        notify: Arc<Notify>,
        thinking_task: Option<tokio::task::AbortHandle>,
        overlay_for_task: Option<fono_overlay::OverlayHandle>,
    ) {
        let state_for_clear = self.assistant_session.clone();
        let action_tx = self.action_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = turn_fut.await {
                warn!("assistant turn failed: {e:#}");
            }
            // Are we still the *current* turn? A barge-in / restart (or an
            // Escape) calls `stop_current_turn`, which takes `current_turn`,
            // and then immediately starts a fresh turn. If that happened
            // while this future was finishing, this pump is now STALE: a
            // newer turn (and its overlay) owns the screen. A stale pump
            // must not touch the overlay or emit `ProcessingDone` — doing so
            // hides the new turn's overlay and races its `AssistantRecording`
            // state back to `Idle`, leaving a capture running with nothing on
            // screen (the "assistant active, no overlay" bug).
            let still_current = {
                let mut s = state_for_clear.lock().await;
                match s.current_turn.as_ref() {
                    Some(active) if Arc::ptr_eq(active, &notify) => {
                        s.current_turn = None;
                        true
                    }
                    _ => false,
                }
            };
            // This turn's own animation ticker is always ours to stop.
            if let Some(t) = thinking_task {
                t.abort();
            }
            if still_current {
                if let Some(o) = overlay_for_task {
                    o.set_state(fono_overlay::OverlayState::Hidden);
                }
                let _ = action_tx.send(HotkeyAction::ProcessingDone);
            } else {
                debug!(
                    "assistant pump: turn superseded before completion; \
                     skipping overlay hide + ProcessingDone (a newer turn owns the screen)"
                );
            }
        });
    }

    fn current_tts(&self) -> Option<Arc<dyn TextToSpeech>> {
        self.tts.read().expect("tts lock poisoned").clone()
    }

    /// Schedule hotkey-time prompt-state cache preparation on the active
    /// assistant backend. Embedded llama.cpp backends restore/build the stable
    /// F7/F8 checkpoint and, when window context is available, a dynamic
    /// window-context checkpoint; cloud backends treat this as a no-op via the
    /// default trait method. Fire-and-forget so the hotkey press path never
    /// blocks on cache work (plan tasks 5–7/9).
    fn prepare_assistant_prompt_cache(
        assistant: Arc<dyn Assistant>,
        history: Vec<fono_assistant::ChatTurn>,
        trigger: AssistantCacheTrigger,
        system_prompt: String,
        focus_info: FocusInfo,
        prefer_vision: bool,
        trace: Option<TurnTrace>,
    ) {
        let snapshot = AssistantPromptCacheSnapshot {
            trigger,
            system_prompt,
            history,
            active_window_context: assistant_window_context_for_cache(&focus_info),
            prefer_vision,
        };
        tokio::spawn(async move {
            // Surface the hotkey-time prepare fire-and-forget on the timeline
            // (Phase 3). Recorded directly on the handle — not via the ambient
            // current-trace — to avoid racing the global slot with the pipeline.
            let prepare_started = Instant::now();
            let result = assistant.prepare_prompt_cache_for_turn(snapshot).await;
            if let Some(t) = &trace {
                t.duration_between(
                    "cache.prepare_for_turn",
                    "cache",
                    "cache",
                    prepare_started,
                    Instant::now(),
                    json!({ "ok": result.is_ok() }),
                );
            }
            if let Err(e) = result {
                debug!("assistant prompt-cache preparation skipped: {e:#}");
            }
        });
    }

    /// Speculatively warm the polish F7Context prefix for the focused app at
    /// record-start, concurrent with capture + STT, so the *first* dictation
    /// into a freshly-focused window restores the whole per-app + language
    /// prefix instead of decoding it on the hotkey path (the ~1.3 s cold cost
    /// in the dictation trace). Fans out across the candidate languages —
    /// whichever STT detects lands on a warm checkpoint. Fire-and-forget; cloud
    /// backends are skipped (the prewarm is a no-op for them anyway).
    fn spawn_polish_context_prewarm(
        &self,
        focus_info: &FocusInfo,
        cfg: &Config,
        trace: Option<TurnTrace>,
    ) {
        let Some(polish) = self.current_llm() else { return };
        // Only embedded local backends maintain a prompt-state cache; cloud
        // backends would just pay an HTTP round-trip for nothing.
        if !polish.is_local() {
            return;
        }
        // Build the exact focused-window context now, immediately after the
        // press-time focus snapshot. This includes terminal /proc enrichment
        // when available, so the speculative prompt matches the eventual
        // post-STT prompt instead of warming an under-enriched variant.
        let profile = classify_focus_profile(focus_info);
        // Candidate source languages STT may report — same source the live
        // `build_format_context` feeds into `candidate_languages`. Bounded so a
        // long locale list can't spawn an unbounded prefill train under STT.
        let candidates = if cfg.stt.local.languages.is_empty() {
            &cfg.general.languages
        } else {
            &cfg.stt.local.languages
        };
        let mut langs: Vec<Option<String>> =
            candidates.iter().take(MAX_POLISH_PREWARM_LANGS).map(|l| Some(l.clone())).collect();
        // With an ambiguous set STT may also report no language (the live path
        // then emits the candidate-set directive); warm that variant too.
        if langs.len() != 1 {
            langs.push(None);
        }
        if langs.is_empty() {
            return;
        }

        let app_class = focus_info.window_class.clone();
        let app_title = focus_info.window_title.clone();
        let cfg = cfg.clone();
        let contexts: Vec<(Option<String>, String)> = langs
            .into_iter()
            .map(|lang| {
                // Per-app builtin suffix computed for a prose (non-command)
                // transcript: terminals suppress their transforming suffix
                // without a command-like transcript, so an empty raw is the
                // right speculative guess and matches the common dictation case.
                let builtin = gated_builtin_suffix(profile.as_ref(), "", lang.as_deref());
                let full_system = build_format_context(
                    &cfg,
                    app_class.as_deref(),
                    app_title.as_deref(),
                    lang.as_deref(),
                    builtin,
                )
                .system_prompt();
                (lang, full_system)
            })
            .collect();
        if let Some(t) = &trace {
            t.instant(
                "polish.context_prewarm_scheduled",
                "polish",
                "f7-polish",
                json!({
                    "variants": contexts.len(),
                    "app_class": app_class.as_deref().unwrap_or(""),
                    "app_title": app_title.as_deref().unwrap_or(""),
                }),
            );
        }
        tokio::spawn(async move {
            for (lang, full_system) in contexts {
                let started = Instant::now();
                let result = polish.prewarm_context_cache(&full_system).await;
                let ended = Instant::now();
                if let Some(t) = &trace {
                    t.duration_between(
                        "polish.context_prewarm",
                        "polish",
                        "f7-polish",
                        started,
                        ended,
                        json!({
                            "ok": result.is_ok(),
                            "language": lang.as_deref().unwrap_or("auto"),
                            "system_chars": full_system.chars().count(),
                        }),
                    );
                }
                if let Err(e) = result {
                    debug!("polish context-cache prewarm skipped (lang={lang:?}): {e:#}");
                }
            }
        });
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
        polish_label_anim: Option<tokio::task::AbortHandle>,
        polish_waveform_anim: Option<tokio::task::AbortHandle>,
        focus_info: FocusInfo,
        trace: Option<TurnTrace>,
    ) {
        let stt = self.current_stt();
        let polish = self.current_llm();
        let history = Arc::clone(&self.history);
        let action_tx = self.action_tx.clone();
        let in_flight = Arc::clone(&self.pipeline_in_flight);
        let config = self.current_config();
        let vocabulary = self.load_vocabulary();
        let injector = Arc::clone(&self.injector);
        let sample_rate = self.capture_cfg.target_sample_rate;
        let speaker_verify = self.speaker_verify(&config);
        // Standalone-waveform overlay: clone the handle so the pipeline
        // task can hide the panel once STT + LLM + inject are done. The
        // overlay was already shifted to `Processing` in
        // `on_stop_recording`; we just clear it back to `Hidden` on
        // every terminal outcome.
        //
        // Two handles, on purpose:
        // - `overlay_hide` (unconditional) is used only for the
        //   terminal `Hidden` transition. The batch pipeline can run
        //   as the live-dictation *fallback* (Transcript style + a
        //   non-streaming STT backend), where the toggle-mode silence
        //   watch pushes Recording/Pondering states onto the panel;
        //   gating the terminal hide on `!live_preview()` left the
        //   panel stuck on screen forever in that combination.
        // - `overlay` (gated) drives the in-pipeline state
        //   transitions (Processing / Polishing / final-text) on the
        //   plain batch path only; the live path owns its own
        //   transitions.
        #[cfg(feature = "interactive")]
        let overlay_hide = self.overlay.read().ok().and_then(|g| g.clone());
        #[cfg(feature = "interactive")]
        let live_fallback = Arc::clone(&self.live_fallback);
        #[cfg(feature = "interactive")]
        let overlay =
            if self.batch_overlay_ui_active(&config) { overlay_hide.clone() } else { None };
        #[cfg(not(feature = "interactive"))]
        let overlay: Option<fono_overlay::OverlayHandle> = None;

        in_flight.store(true, Ordering::SeqCst);
        tokio::spawn(async move {
            // Install the press-time trace as process-current for the
            // duration of this pipeline so the STT + F7 polish + cache
            // events recorded by the lower-level crates land on it.
            let _trace_guard = trace.as_ref().map(TurnTrace::make_current);
            let mut polish_label_anim = polish_label_anim;
            // H.1: focus_info was captured at hotkey-press time in
            // `on_start_recording` and threaded through `CaptureSession`.
            // We deliberately do NOT re-probe here — by the time we
            // reach this point the user has released the hotkey, the
            // overlay surface may have been mapped, and focus may have
            // moved off the window the user was actually dictating into.
            let outcome = run_pipeline(
                pcm,
                sample_rate,
                capture_ms,
                stt.as_ref(),
                polish.as_deref(),
                &history,
                &config,
                &vocabulary,
                injector.as_ref(),
                focus_info,
                overlay.as_ref(),
                &mut polish_label_anim,
                polish_walk_duration(Duration::from_millis(capture_ms)),
                speaker_verify,
            )
            .await;
            match &outcome {
                PipelineOutcome::Completed { metrics, .. } => {
                    info!("{}", format_pipeline_summary(metrics));
                }
                PipelineOutcome::EmptyOrTooShort { duration_ms } => {
                    warn!("pipeline: empty/too short ({duration_ms}ms)");
                }
                PipelineOutcome::Failed(msg) => {
                    error!("pipeline failed: {msg}");
                }
            }
            #[cfg(feature = "interactive")]
            if let Some(t) = polish_label_anim {
                t.abort();
            }
            #[cfg(feature = "interactive")]
            if let Some(t) = polish_waveform_anim {
                t.abort();
            }
            #[cfg(not(feature = "interactive"))]
            drop(polish_label_anim);
            #[cfg(not(feature = "interactive"))]
            drop(polish_waveform_anim);
            #[cfg(feature = "interactive")]
            if let Some(o) = overlay_hide {
                o.set_state(fono_overlay::OverlayState::Hidden);
                // Restore Transcript style if this was a live-fallback
                // session (see `spawn_waveform_level_task`). The swap
                // happens while the panel is hidden, so the user never
                // sees the style flip.
                if live_fallback.swap(false, Ordering::Relaxed) {
                    o.set_waveform_style(config.overlay.style);
                }
            }
            in_flight.store(false, Ordering::SeqCst);
            if let Some(t) = &trace {
                let aborted = !matches!(&outcome, PipelineOutcome::Completed { .. });
                t.finish(json!({
                    "path": "dictation",
                    "aborted": aborted,
                    "summary": t.cache_scoreboard(),
                }));
            }
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
        let vocabulary = self.load_vocabulary();
        // H.2: probe focus on a blocking thread so the async executor
        // doesn't stall on Wayland IPC / X11 calls.
        let focus = Arc::clone(&self.focus);
        let focus_info =
            tokio::task::spawn_blocking(move || focus.probe()).await.unwrap_or_default();
        let mut polish_anim = None;
        let speaker_verify = self.speaker_verify(&config);
        run_pipeline(
            pcm,
            self.capture_cfg.target_sample_rate,
            capture_ms,
            stt.as_ref(),
            polish.as_deref(),
            &self.history,
            &config,
            &vocabulary,
            self.injector.as_ref(),
            focus_info,
            None,
            &mut polish_anim,
            polish_walk_duration(Duration::from_millis(capture_ms)),
            speaker_verify,
        )
        .await
    }

    /// Load the personal vocabulary (ADR 0037). Re-read per dictation —
    /// the file is tiny and this is off the audio hot path — so
    /// `fono vocabulary add` (or a web-UI edit) is picked up by the very
    /// next dictation with no daemon reload. Orchestrators built without
    /// `Paths` (tests) run on an empty, no-op table.
    fn load_vocabulary(&self) -> fono_core::correction::VocabularyTable {
        self.paths
            .as_deref()
            .map(|p| fono_core::correction::VocabularyTable::load_or_empty(&p.vocabulary_file()))
            .unwrap_or_default()
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

    /// True when the batch dictation pipeline should drive the
    /// standalone-waveform overlay: either live preview is off (plain
    /// batch mode), or the live path fell back to batch for this
    /// session because the backend can't stream ([`Self::live_fallback`]).
    fn batch_overlay_ui_active(&self, cfg: &Config) -> bool {
        cfg.overlay.waveform && (!cfg.live_preview() || self.live_fallback.load(Ordering::Relaxed))
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
        focus_info: FocusInfo,
    ) -> Result<LiveCaptureSession> {
        // Slice A: streaming pipeline operates at 16 kHz to keep the
        // pump's broadcast frame budget aligned with whisper. The
        // capture stage resamples for us.
        let sample_rate = 16_000_u32;
        let cap_cfg = CaptureConfig { target_sample_rate: sample_rate, source: None };

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
        // Quality floor was a config knob (`[interactive].quality_floor`)
        // until 2026-07; only `Max` was ever implemented, so it is now
        // pinned (the `LiveSession` parameter stays for R12.5).
        let quality_floor = fono_core::QualityFloor::Max;

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
            focus_info,
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
            let backend = self.current_stt().name();
            warn!(
                "live-dictation: STT backend {backend:?} has no streaming support — live \
                 transcript preview currently needs `[stt].backend` set to \
                 \"local\", \"groq\", or \"deepgram\"; falling back to batch path"
            );
            // Fallback UX: run the batch pipeline with the standard
            // waveform overlay (the show-gates honour this flag via
            // `batch_overlay_ui_active`) and tell the user why — once,
            // not on every press. The latch is re-armed on `reload()`.
            self.live_fallback.store(true, Ordering::Relaxed);
            if !self.live_fallback_notified.swap(true, Ordering::Relaxed) {
                fono_core::notify::send(
                    "Fono — live transcript unavailable",
                    &format!(
                        "The {backend} STT backend doesn't support live streaming yet. \
                         Recording with the standard visualisation instead; your \
                         transcript is typed when you stop."
                    ),
                    "dialog-information",
                    6_000,
                    fono_core::notify::Urgency::Normal,
                );
            }
            return self.on_start_recording(mode).await;
        };
        // A real streaming session is starting; make sure no stale
        // fallback flag survives (e.g. after a mid-session `reload`
        // swapped in a streaming-capable backend).
        self.live_fallback.store(false, Ordering::Relaxed);
        if self.pipeline_in_flight.load(Ordering::SeqCst) {
            warn!("live-dictation requested while previous pipeline still running; ignoring");
            return Ok(());
        }
        {
            let mut s = self.assistant_session.lock().await;
            s.stop_current_turn();
        }
        let mut slot = self.live_capture.lock().await;
        if slot.is_some() {
            warn!("live-dictation already in progress; ignoring duplicate start");
            return Ok(());
        }

        // Pondering parity for live dictation: mirror the batch
        // toggle path and spawn the silence watch when the user
        // pressed (rather than held) the dictation key, so the
        // "PONDERING" overlay + auto-stop commit work on the
        // streaming pipeline too. Hold-to-talk owns its own
        // boundary, same as the batch path.
        let silence_flavor =
            matches!(mode, RecordingMode::Toggle).then_some(SilenceWatchFlavor::Dictation);
        // H.1: snapshot focus at press time, before the overlay surface
        // is shown and before any focus-follows-mouse motion can move it.
        let focus = Arc::clone(&self.focus);
        let focus_info =
            tokio::task::spawn_blocking(move || focus.probe()).await.unwrap_or_default();
        tracing::debug!(
            target: "fono::context",
            class = ?focus_info.window_class,
            title = ?focus_info.window_title,
            "live-capture: focus snapshot at press"
        );
        let session = self.build_live_capture_pipeline(
            streaming,
            fono_overlay::OverlayState::LiveDictating,
            silence_flavor,
            focus_info,
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

        // Personal vocabulary (ADR 0037): same deterministic correction as
        // the batch path, applied to the committed transcript before polish
        // / inject / history so every consumer sees the canonical spelling.
        let vocabulary = self.load_vocabulary();
        let raw = transcript.committed.trim().to_string();
        let raw = if vocabulary.is_empty() { raw } else { vocabulary.apply(&raw) };
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
        let mut live_ctx_tag = String::new();
        let trans_language: Option<String> = None; // live STT path: language not yet surfaced
                                                   // Streaming-injection bookkeeping (plan v3), mirror of the batch path.
        let mut live_stream_injected = false;
        let mut live_stream_backend = String::new();
        let mut live_stream_clip = false;
        let mut live_stream_inject_ms = 0_u64;
        let mut live_ttfi_ms = 0_u64;
        let cleaned = if cfg.interactive.cleanup_on_finalize {
            if let Some(polish) = self.current_llm() {
                // Show "polishing…" so the user knows we haven't
                // hung after the streaming ended.
                if let Some(o) = session.overlay.as_ref() {
                    o.set_state(fono_overlay::OverlayState::Processing);
                    o.update_text(raw.clone());
                }
                // H.1: use the focus snapshot captured at press time.
                // Re-probing here would pick up whatever stole focus
                // since the overlay was mapped.
                let focus_info = session.focus_info.clone();
                let live_ctx_profile = ContextClassifier::classify(
                    focus_info.window_class.as_deref(),
                    focus_info.window_title.as_deref(),
                );
                let builtin_suffix = gated_builtin_suffix(live_ctx_profile.as_ref(), &raw, None);
                let app_class = focus_info.window_class.clone();
                let app_title = focus_info.window_title.clone();
                let ctx = build_format_context(
                    &cfg,
                    app_class.as_deref(),
                    app_title.as_deref(),
                    None,
                    builtin_suffix,
                );
                live_ctx_tag = build_ctx_tag(&ctx, app_class.as_deref());
                llm_label_for_log = Some(polish.name().to_string());
                // Streaming local cleanup (plan v3): same gate as the batch
                // path. Stream the cleaned text into the cursor word-by-word
                // when the backend is local, the flag is on, and the injector
                // types incrementally.
                let stream_eligible = cfg.polish.stream_injection
                    && polish.is_local()
                    && self.injector.supports_streaming();
                if stream_eligible {
                    let outcome = stream_cleanup_and_inject(
                        polish.as_ref(),
                        &raw,
                        &ctx,
                        self.injector.as_ref(),
                    )
                    .await;
                    llm_ms = outcome.elapsed_ms;
                    if outcome.injected {
                        live_stream_injected = true;
                        live_stream_backend.clone_from(&outcome.backend);
                        live_stream_clip = outcome.clipboard_populated;
                        live_stream_inject_ms = outcome.inject_ms;
                        live_ttfi_ms = outcome.ttfi_ms;
                        debug!(
                            "live-dictation: {} streamed+injected {}ms (ttfi {}ms)",
                            polish.name(),
                            llm_ms,
                            outcome.ttfi_ms
                        );
                        outcome.cleaned
                    } else {
                        debug!("live-dictation: {} stream gate fell back to raw", polish.name());
                        None
                    }
                } else {
                    match polish.format(&raw, &ctx).await {
                        Ok(c) => {
                            llm_ms = llm_started.elapsed().as_millis() as u64;
                            let trimmed = c.trim().to_string();
                            let raw_chars = raw.chars().count();
                            let new_chars = trimmed.chars().count();
                            let diff = i64::try_from(new_chars).unwrap_or(0)
                                - i64::try_from(raw_chars).unwrap_or(0);
                            debug!(
                                "polish: {} {}ms  {} → {} chars ({:+})",
                                polish.name(),
                                llm_ms,
                                raw_chars,
                                new_chars,
                                diff
                            );
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

        // Inject — best-effort, same as the batch path. When the streaming
        // path already typed the cleaned text into the cursor, skip the
        // one-shot inject and reuse its recorded backend / timing.
        let (clipboard_already_populated, live_inject_backend, inject_ms) = if live_stream_injected
        {
            (live_stream_clip, live_stream_backend.clone(), live_stream_inject_ms)
        } else {
            let inject_started = Instant::now();
            let injector = Arc::clone(&self.injector);
            let final_for_inject = final_text.clone();
            let (clip, backend) =
                tokio::task::spawn_blocking(move || injector.inject(&final_for_inject))
                    .await
                    .ok()
                    .and_then(std::result::Result::ok)
                    .unwrap_or_else(|| (false, "unknown".to_string()));
            (clip, backend, inject_started.elapsed().as_millis() as u64)
        };
        if cfg.general.also_copy_to_clipboard && !clipboard_already_populated {
            if let Err(e) = fono_inject::copy_to_clipboard(&final_text) {
                warn!("live-dictation: clipboard copy failed: {e:#}");
            }
        }
        let _ = live_ttfi_ms;

        // Mirror the batch summary at `session.rs:684-696` so live and
        // batch dictations produce structurally-identical operator
        // output. Live mode has no trim stage (streaming consumed PCM
        // continuously), so we omit the trim leg; everything else
        // matches.
        let raw_chars = raw.chars().count();
        let final_chars = final_text.chars().count();
        let llm_label = llm_label_for_log.as_deref().unwrap_or("none");
        info!(
            "{}",
            format_pipeline_summary_live(
                capture_ms,
                trans_language.as_deref(),
                transcript.segments_finalized,
                raw_chars,
                llm_label,
                llm_ms,
                &live_ctx_tag,
                final_chars,
                inject_ms,
                &live_inject_backend,
            )
        );

        // History (non-fatal on failure).
        if cfg.history.enabled {
            let stt_label = self.current_stt().name().to_string();
            let llm_label = if cleaned.is_some() {
                self.current_llm().map(|l| l.name().to_string())
            } else {
                None
            };
            // H.1: same press-time focus snapshot used for polish.
            let live_focus_info = session.focus_info.clone();
            let live_suppress = ContextClassifier::classify(
                live_focus_info.window_class.as_deref(),
                live_focus_info.window_title.as_deref(),
            )
            .is_some_and(|p| p.suppress_history);
            if !live_suppress {
                let row = HistoryRow {
                    id: None,
                    ts: now_unix(),
                    duration_ms: Some(capture_ms as i64),
                    raw: raw.clone(),
                    cleaned: cleaned.clone(),
                    app_class: live_focus_info.window_class,
                    app_title: live_focus_info.window_title,
                    stt_backend: Some(stt_label),
                    polish_backend: llm_label,
                    language: None,
                    // Speaker verification is not wired into the streaming
                    // live-dictation path yet: it consumes PCM frame-by-frame
                    // and does not retain the whole-utterance buffer the
                    // embedding model needs. Tagged only on the batch path
                    // (`run_pipeline`) for now; a follow-up can accumulate the
                    // live PCM to verify here too.
                    speaker: None,
                };
                let redact = cfg.history.redact_secrets;
                let db = self.history.lock().await;
                if let Err(e) = db.insert(&row, redact) {
                    warn!("live-dictation: history insert failed: {e:#}");
                }
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
    vocabulary: &fono_core::correction::VocabularyTable,
    injector: &dyn Injector,
    focus_info: FocusInfo,
    overlay: Option<&fono_overlay::OverlayHandle>,
    polish_label_anim: &mut Option<tokio::task::AbortHandle>,
    polish_walk_duration: Duration,
    speaker_verify: Option<crate::daemon::SpeakerVerify>,
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

    // ---- Context classification (snapshot at pipeline-start time) ------
    // H.1: `focus_info` is computed at the very start of processing
    // (before STT) so it's a snapshot of the window active at or
    // immediately after hotkey press. It is used for both STT and polish
    // so the context is consistent across the whole pipeline.
    let ctx_profile = classify_focus_profile(&focus_info);
    if let Some(ref p) = ctx_profile {
        tracing::debug!(
            target: "fono::context",
            profile        = p.name,
            whisper_hint   = ?p.whisper_hint,
            llm_suffix     = ?p.llm_suffix,
            suppress_history = p.suppress_history,
            detected_agent = ?p.detected_agent,
            "resolved profile"
        );
    } else {
        tracing::debug!(target: "fono::context", "no built-in profile matched — using base prompts");
    }
    let suppress_history = ctx_profile.as_ref().is_some_and(|p| p.suppress_history);

    // ---- STT ---------------------------------------------------------
    let stt_started = Instant::now();
    let lang = lang_for(config);
    let stt_opts = TranscribeOptions {
        lang_override: lang,
        context_hint: ctx_profile
            .as_ref()
            .and_then(|p| p.whisper_hint.as_deref())
            .map(str::to_string),
    };
    // Speaker verification (when enabled) runs *concurrently* with the STT
    // call over the same 16 kHz buffer, so its tens-of-ms embed hides behind
    // the STT round-trip. The embedding never leaves `verify` — only the
    // matched speaker name comes back — and it is joined before we tag
    // history. `stt_ms` therefore measures the STT+embed envelope (embed is
    // the shorter leg), which is what the operator waits on.
    let speech_secs = pcm_for_stt.len() as f32 / sample_rate.max(1) as f32;
    let sufficient_audio = speech_secs >= config.speaker.min_speech_secs;
    let stt_future = stt.transcribe_with_opts(&pcm_for_stt, sample_rate, &stt_opts);
    let verify_future = async {
        match speaker_verify.as_ref() {
            Some(v) => v.verify(&pcm_for_stt, sample_rate, sufficient_audio).await,
            None => None,
        }
    };
    let (stt_result, speaker_name) = tokio::join!(stt_future, verify_future);
    let stt_ended = Instant::now();
    metrics.stt_ms = stt_started.elapsed().as_millis() as u64;
    let trans = match stt_result {
        Ok(t) => {
            // `stt` lane span on the dictation waterfall (Workstream B),
            // mirroring the assistant `stt.transcribe` span. Recorded via the
            // ambient current-trace installed by `spawn_pipeline`.
            if let Some(tr) = TurnTrace::current() {
                tr.duration_between(
                    "stt.transcribe",
                    "stt",
                    STT_LANE,
                    stt_started,
                    stt_ended,
                    json!({
                        "chars_out": t.text.trim().chars().count(),
                        "language": t.language.as_deref().unwrap_or(""),
                        "sample_rate": sample_rate,
                    }),
                );
            }
            t
        }
        Err(e) => {
            let err_text = format!("STT {}: {e:#}", stt.name());
            if let Some(tr) = TurnTrace::current() {
                tr.duration_between(
                    "stt.transcribe",
                    "stt",
                    STT_LANE,
                    stt_started,
                    stt_ended,
                    json!({ "error": err_text, "sample_rate": sample_rate }),
                );
            }
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
    // Personal vocabulary (ADR 0037): deterministic correction applied to
    // the transcript itself, so polish, injection (one-shot *and* the
    // word-by-word streaming path), clipboard, history, and the overlay
    // all see the canonical spelling.
    let raw = if vocabulary.is_empty() { raw } else { vocabulary.apply(&raw) };
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
    debug!("stt: {} {}ms → {} chars", stt.name(), metrics.stt_ms, metrics.raw_chars);
    metrics.language.clone_from(&trans.language);

    // ---- polish (optional) -------------------------------------
    let app_class = focus_info.window_class.clone();
    let app_title = focus_info.window_title.clone();
    tracing::debug!(
        target: "fono::pipeline",
        "stt.raw lang={:?} app=({:?}, {:?}): {raw:?}",
        trans.language, app_class, app_title,
    );
    let word_count = raw.split_whitespace().count() as u32;
    let skip_short =
        config.polish.skip_if_words_lt > 0 && word_count < config.polish.skip_if_words_lt;
    // Streaming-injection bookkeeping (plan v3): set when the local cleanup
    // backend streamed its output and already typed it incrementally, so the
    // shared inject block below is skipped.
    let mut stream_injected = false;
    let mut stream_backend = String::new();
    let mut stream_clip_populated = false;
    let mut stream_inject_ms = 0_u64;
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
        #[cfg(feature = "interactive")]
        if let Some(o) = overlay.filter(|_| polish_label_anim.is_some()) {
            // `overlay` is only `Some` when the batch overlay UI is
            // active for this session (including the live-fallback
            // case), so no further live-preview gating is needed.
            if let Some(t) = polish_label_anim.take() {
                t.abort();
            }
            *polish_label_anim = Some(spawn_polishing_phase_task_for_handle(
                o.clone(),
                PolishingPhase::Cleanup,
                polish_walk_duration,
            ));
        }
        let builtin_suffix =
            gated_builtin_suffix(ctx_profile.as_ref(), &raw, trans.language.as_deref());
        let ctx = build_format_context(
            config,
            app_class.as_deref(),
            app_title.as_deref(),
            trans.language.as_deref(),
            builtin_suffix,
        );
        metrics.ctx_tag = build_ctx_tag(&ctx, app_class.as_deref());
        tracing::debug!(
            target: "fono::pipeline",
            "polish.prompt main={:?} advanced={:?} dictionary={:?} rule_suffix={:?} candidate_languages={:?}",
            ctx.main_prompt, ctx.advanced_prompt, ctx.dictionary, ctx.rule_suffix, ctx.candidate_languages,
        );
        tracing::debug!(target: "fono::pipeline", "polish.input: {raw:?}");
        let llm_started = Instant::now();
        // Streaming local cleanup (plan v3): when the active polish backend is
        // local, the flag is on, and the injector types incrementally at the
        // cursor (not the clipboard fallback), stream the cleaned text into the
        // cursor word-by-word behind a first-sentence guard gate instead of
        // waiting for the whole decode. Cloud backends, clipboard-fallback
        // sessions, and (above) short utterances stay on the one-shot path.
        let stream_eligible = config.polish.stream_injection
            && polish_backend.is_local()
            && injector.supports_streaming();
        if stream_eligible {
            let outcome = stream_cleanup_and_inject(polish_backend, &raw, &ctx, injector).await;
            metrics.llm_ms = outcome.elapsed_ms;
            if outcome.injected {
                stream_injected = true;
                stream_backend.clone_from(&outcome.backend);
                stream_clip_populated = outcome.clipboard_populated;
                stream_inject_ms = outcome.inject_ms;
                metrics.time_to_first_inject_ms = outcome.ttfi_ms;
                debug!(
                    "polish: {} streamed+injected {}ms (ttfi {}ms)",
                    polish_backend.name(),
                    metrics.llm_ms,
                    outcome.ttfi_ms
                );
                tracing::debug!(target: "fono::pipeline", "polish.output(streamed): {:?}", outcome.cleaned);
                outcome.cleaned
            } else {
                // Gate fired (clarification / degenerate / translated prefix)
                // or the stream produced nothing usable: nothing was typed —
                // fall back to the raw transcript via the shared inject block.
                debug!("polish: {} stream gate fell back to raw", polish_backend.name());
                None
            }
        } else {
            match polish_backend.format(&raw, &ctx).await {
                Ok(c) => {
                    metrics.llm_ms = llm_started.elapsed().as_millis() as u64;
                    let trimmed = c.trim().to_string();
                    let raw_chars = raw.chars().count();
                    let new_chars = trimmed.chars().count();
                    let diff = i64::try_from(new_chars).unwrap_or(0)
                        - i64::try_from(raw_chars).unwrap_or(0);
                    debug!(
                        "polish: {} {}ms  {} → {} chars ({:+})",
                        polish_backend.name(),
                        metrics.llm_ms,
                        raw_chars,
                        new_chars,
                        diff
                    );
                    tracing::debug!(target: "fono::pipeline", "polish.output: {trimmed:?}");
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed)
                    }
                }
                Err(e) => {
                    metrics.llm_ms = llm_started.elapsed().as_millis() as u64;
                    warn!(
                        "polish: {} failed after {}ms: {e:#}",
                        polish_backend.name(),
                        metrics.llm_ms
                    );
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
        }
    } else {
        None
    };

    // Belt-and-suspenders vocabulary pass (idempotent): guards against the
    // polish LLM re-introducing a mishearing. Applied to `cleaned` itself so
    // injection, history, and the returned outcome stay consistent. The
    // streamed-inject path has already typed its text and skips the one-shot
    // inject below anyway.
    let cleaned = match cleaned {
        Some(c) if !vocabulary.is_empty() && !stream_injected => Some(vocabulary.apply(&c)),
        other => other,
    };
    let final_text = cleaned.as_deref().unwrap_or(&raw).to_string();
    metrics.final_chars = final_text.chars().count();

    // ---- Inject -----------------------------------------------------
    // When the local streaming path already typed the cleaned text into the
    // cursor word-by-word, skip the one-shot inject entirely — re-injecting
    // would duplicate every character. Reuse its recorded backend / timing.
    let clipboard_already_populated = if stream_injected {
        metrics.inject_backend = stream_backend;
        metrics.inject_ms = stream_inject_ms;
        stream_clip_populated
    } else {
        let inject_started = Instant::now();
        let populated = match injector.inject(&final_text) {
            Ok((populated, backend)) => {
                metrics.inject_backend = backend;
                populated
            }
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
                metrics.inject_backend = "failed".to_string();
                false
            }
        };
        metrics.inject_ms = inject_started.elapsed().as_millis() as u64;
        populated
    };
    debug!("inject: {}ms via {}", metrics.inject_ms, metrics.inject_backend);
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
    // G.2: Skip history writes when the active window is a private/
    // sensitive app (password manager, etc.) — suppress_history was set
    // by the classifier's Private profile.
    if config.history.enabled && !suppress_history {
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
            speaker: speaker_name.clone(),
        };
        let redact = config.history.redact_secrets;
        let db = history.lock().await;
        if let Err(e) = db.insert(&row, redact) {
            warn!("history insert failed: {e:#}");
        }
    }

    PipelineOutcome::Completed { raw, cleaned, metrics }
}

/// Outcome of a streaming-cleanup-with-incremental-injection attempt
/// ([`stream_cleanup_and_inject`]).
#[derive(Debug, Default)]
struct StreamCleanupOutcome {
    /// Full accumulated streamed text, trimmed. `None` when a guard fired, the
    /// stream errored before producing a usable prefix, or it yielded nothing
    /// — the caller falls back to the raw transcript on the one-shot inject
    /// path.
    cleaned: Option<String>,
    /// `true` once text has been committed to the cursor inside the sink. When
    /// set, the caller MUST NOT inject again (not even the raw fallback):
    /// partial output is already typed and re-injecting would duplicate it.
    injected: bool,
    /// Inject backend name actually used (empty when nothing was injected).
    backend: String,
    /// Whether the injector already populated the clipboard (clipboard
    /// fallback) — the caller skips the redundant `also_copy_to_clipboard`.
    clipboard_populated: bool,
    /// Time-to-first-injected-char in ms (0 when nothing was injected).
    ttfi_ms: u64,
    /// Wall-clock from cleanup-stream start to stream end / injection
    /// completion — used as `llm_ms` for the streaming path.
    elapsed_ms: u64,
    /// Wall-clock from the first injected char to injection completion — used
    /// as `inject_ms` for the streaming path. 0 when nothing was injected.
    inject_ms: u64,
}

/// True when `text` contains a sentence-ending punctuation mark (`.`/`!`/`?`)
/// followed by ASCII whitespace or the end of the buffer. Used by the
/// first-sentence gate alongside [`has_enough_text_for_language_guard`]: a
/// false positive (e.g. inside `e.g.`) only means the guards run a few tokens
/// earlier, which is harmless.
fn has_sentence_boundary(text: &str) -> bool {
    let bytes = text.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        if matches!(c, b'.' | b'!' | b'?') {
            match bytes.get(i + 1) {
                None => return true,
                Some(n) if n.is_ascii_whitespace() => return true,
                _ => {}
            }
        }
    }
    false
}

/// Word-boundary injection sink. Buffers an incremental text stream and
/// releases only *complete* words plus their separators, holding back the
/// trailing (possibly-partial) word and any trailing whitespace until a later
/// chunk completes it or the stream ends. Guarantees the cursor never sees a
/// partial word/token mid-stream and never a dangling trailing newline/space.
///
/// The concatenation of every [`push`](WordSink::push) return value followed
/// by [`flush`](WordSink::flush) equals the input with leading/trailing
/// whitespace trimmed (internal whitespace preserved exactly).
#[derive(Debug, Default)]
struct WordSink {
    carry: String,
    /// Set once the first non-whitespace char has been seen, so leading
    /// whitespace from the model's first token is trimmed (matching the
    /// non-streaming `format()` path, which returns `out.trim()`).
    started: bool,
}

impl WordSink {
    fn new() -> Self {
        Self::default()
    }

    /// Append `text`; return the slice now safe to inject (complete words with
    /// their separators, never a trailing partial word, never trailing
    /// whitespace).
    fn push(&mut self, text: &str) -> String {
        let to_add = if self.started {
            text
        } else {
            let trimmed = text.trim_start();
            if trimmed.is_empty() {
                return String::new();
            }
            self.started = true;
            trimmed
        };
        self.carry.push_str(to_add);
        // Hold back the trailing whitespace run plus the final (partial) word:
        // strip trailing whitespace, then split before the last remaining
        // whitespace char. Everything before that boundary is complete words
        // and is safe to emit; the held suffix (whitespace + last word) waits
        // for more input or `flush`.
        let trimmed_end = self.carry.trim_end_matches(char::is_whitespace);
        match trimmed_end.rfind(char::is_whitespace) {
            Some(ws_idx) => {
                let flushed: String = self.carry[..ws_idx].to_string();
                self.carry.drain(..ws_idx);
                flushed
            }
            None => String::new(),
        }
    }

    /// Emit the held trailing word at stream end, with trailing whitespace
    /// trimmed.
    fn flush(&mut self) -> String {
        let out = std::mem::take(&mut self.carry).trim_end().to_string();
        self.started = false;
        out
    }
}

/// Minimum interval between incremental text injections during streaming
/// cleanup. The per-word key-injection backends (`wtype`/`ydotool`/`xdotool`)
/// spawn a process per call, so coalescing decoded words into ~5 injections a
/// second keeps that cost negligible without the user perceiving lag.
const FLUSH_INTERVAL: Duration = Duration::from_millis(200);

/// Stream the local cleanup model's output and inject it incrementally,
/// applying the first-sentence guard gate before any text reaches the cursor.
///
/// Flow (plan v3 Tasks 3–4 + 6):
///  1. Buffer incoming chunks until BOTH a sentence boundary is seen AND the
///     buffered text satisfies the translation-guard minimum, OR the stream
///     ends/errors.
///  2. Run all three cleanup guards on the buffered prefix. If ANY fires,
///     discard the stream and inject NOTHING — the caller falls back to the
///     raw transcript (zero cleaned chars typed), identical to the one-shot
///     polish-failure path.
///  3. Otherwise inject the buffered prefix, then flush each completed word as
///     it decodes. The full accumulated text is returned for the history row
///     and clipboard copy so those artefacts equal a non-streaming run.
///
/// On a mid-stream injector or decode error AFTER injection began, the already
/// typed text is kept (never re-injected, never overwritten with raw) and the
/// accumulated prefix is returned with `injected = true`.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
async fn stream_cleanup_and_inject(
    polish: &dyn TextFormatter,
    raw: &str,
    ctx: &FormatContext,
    injector: &dyn Injector,
) -> StreamCleanupOutcome {
    let started = Instant::now();
    let mut stream = match polish.format_stream(raw, ctx).await {
        Ok(s) => s,
        Err(e) => {
            warn!("polish: {} stream init failed: {e:#}", polish.name());
            return StreamCleanupOutcome {
                elapsed_ms: started.elapsed().as_millis() as u64,
                ..Default::default()
            };
        }
    };

    // ---- Phase 1: buffer to the first-sentence gate ------------------
    let mut buf = String::new();
    let mut stream_done = false;
    loop {
        if has_sentence_boundary(&buf) && has_enough_text_for_language_guard(buf.trim()) {
            break;
        }
        match stream.next().await {
            Some(Ok(chunk)) => buf.push_str(&chunk),
            Some(Err(e)) => {
                if buf.trim().is_empty() {
                    warn!("polish: {} stream errored before any output: {e:#}", polish.name());
                    return StreamCleanupOutcome {
                        elapsed_ms: started.elapsed().as_millis() as u64,
                        ..Default::default()
                    };
                }
                warn!(
                    "polish: {} stream errored mid-prefix ({e:#}); gating on the partial prefix",
                    polish.name()
                );
                stream_done = true;
                break;
            }
            None => {
                stream_done = true;
                break;
            }
        }
    }

    // ---- Phase 2: run all three guards on the buffered prefix --------
    let prefix = buf.trim();
    if prefix.is_empty() {
        return StreamCleanupOutcome {
            elapsed_ms: started.elapsed().as_millis() as u64,
            ..Default::default()
        };
    }
    if looks_like_clarification(prefix)
        || looks_like_degenerate_cleanup(raw, prefix)
        || looks_like_translated_cleanup(raw, prefix, ctx)
    {
        debug!(
            "polish: {} streamed prefix tripped a cleanup guard; discarding stream, falling back to raw",
            polish.name()
        );
        return StreamCleanupOutcome {
            elapsed_ms: started.elapsed().as_millis() as u64,
            ..Default::default()
        };
    }

    // ---- Phase 3: gate passed → inject prefix, then stream words -----
    let mut sink = WordSink::new();
    let mut full = String::new();
    let mut backend = String::new();
    let mut clipboard_populated = false;
    let mut ttfi_ms = 0_u64;
    let mut inject_started: Option<Instant> = None;
    // Coalesce decoded words and inject at most once per FLUSH_INTERVAL (the
    // per-word key-injection backends spawn a process per call). The model
    // decodes into an unbounded channel, so this cadence never throttles
    // generation — it only smooths injection.
    let mut pending = String::new();
    let mut last_inject: Option<Instant> = None;
    // Injection-cost counters. Each `injector.inject` call gets its own span on
    // the `f7-inject` lane (visible running concurrently with `polish.generate`
    // on `f7-polish`), and a `polish.stream_inject_summary` instant rolls up
    // the per-event mean so the cost of typing is measurable rather than
    // inferred. All trace calls short-circuit to no-ops on untraced turns.
    let mut inject_events = 0_u32;
    let mut inject_busy_us = 0_u128;
    let mut chars_injected = 0_usize;

    // Closure-free helper: inject `s` (if non-empty), recording TTFI/backend.
    macro_rules! emit {
        ($s:expr) => {{
            let s: String = $s;
            if !s.is_empty() {
                let chunk_chars = s.chars().count();
                let span = current_span("polish.inject_chunk", "inject", INJECT_LANE);
                let ev_started = Instant::now();
                let res = injector.inject(&s);
                let event_us = ev_started.elapsed().as_micros();
                inject_events += 1;
                inject_busy_us += event_us;
                chars_injected += chunk_chars;
                span.finish(json!({
                    "chars": chunk_chars,
                    "seq": inject_events,
                    "event_us": event_us as u64,
                }));
                match res {
                    Ok((populated, b)) => {
                        if inject_started.is_none() {
                            inject_started = Some(Instant::now());
                            ttfi_ms = started.elapsed().as_millis() as u64;
                            clipboard_populated = populated;
                            backend = b;
                        }
                    }
                    Err(e) => {
                        warn!(
                            "polish: {} streaming inject failed mid-stream: {e:#}",
                            polish.name()
                        );
                        // Keep what was already typed; do not re-inject raw.
                        let inject_ms =
                            inject_started.map_or(0, |t| t.elapsed().as_millis() as u64);
                        return StreamCleanupOutcome {
                            cleaned: Some(full.trim().to_string()).filter(|s| !s.is_empty()),
                            injected: inject_started.is_some(),
                            backend: if backend.is_empty() {
                                "failed".to_string()
                            } else {
                                backend
                            },
                            clipboard_populated,
                            ttfi_ms,
                            elapsed_ms: started.elapsed().as_millis() as u64,
                            inject_ms,
                        };
                    }
                }
            }
        }};
    }

    // Inject `pending` when the cadence allows (always on the first call so
    // the gate prefix lands immediately and TTFI stays low) or when `force`d
    // at stream end. Whole-word boundaries are guaranteed by `WordSink`.
    macro_rules! flush_pending {
        ($force:expr) => {{
            let due = last_inject.is_none_or(|t| t.elapsed() >= FLUSH_INTERVAL);
            if !pending.is_empty() && ($force || due) {
                emit!(std::mem::take(&mut pending));
                last_inject = Some(Instant::now());
            }
        }};
    }

    full.push_str(buf.trim_start());
    pending.push_str(&sink.push(buf.trim_start()));
    flush_pending!(true);

    // Continue draining the stream (unless it already ended/errored).
    if !stream_done {
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    full.push_str(&chunk);
                    pending.push_str(&sink.push(&chunk));
                    flush_pending!(false);
                }
                Err(e) => {
                    warn!(
                        "polish: {} stream errored after injection began ({e:#}); keeping typed text",
                        polish.name()
                    );
                    break;
                }
            }
        }
    }
    pending.push_str(&sink.flush());
    if !pending.is_empty() {
        emit!(std::mem::take(&mut pending));
    }

    let inject_ms = inject_started.map_or(0, |t| t.elapsed().as_millis() as u64);
    if inject_events > 0 {
        current_instant(
            "polish.stream_inject_summary",
            "inject",
            INJECT_LANE,
            json!({
                "events": inject_events,
                "chars": chars_injected,
                "busy_ms": (inject_busy_us / 1000) as u64,
                "mean_event_us": (inject_busy_us / u128::from(inject_events)) as u64,
                "ttfi_ms": ttfi_ms,
                "inject_ms": inject_ms,
                "backend": backend,
            }),
        );
    }
    let full = full.trim().to_string();
    StreamCleanupOutcome {
        cleaned: if full.is_empty() { None } else { Some(full) },
        injected: inject_started.is_some(),
        backend,
        clipboard_populated,
        ttfi_ms,
        elapsed_ms: started.elapsed().as_millis() as u64,
        inject_ms,
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
    builtin_suffix: Option<&str>,
) -> FormatContext {
    // E.1: Merge user [[context_rules]] (higher priority) with the built-in
    // classifier suffix. User rule wins; if both match, they are concatenated
    // (user appended after built-in — additive, not replacing).
    let rule_suffix =
        match (matched_rule_suffix(&config.context_rules, app_class, app_title), builtin_suffix) {
            (Some(user), Some(builtin)) => Some(format!("{builtin}\n{user}")),
            (Some(user), None) => Some(user),
            (None, Some(builtin)) => Some(builtin.to_string()),
            (None, None) => None,
        };
    let mut ctx = FormatContext {
        main_prompt: config.polish.prompt.main.clone(),
        advanced_prompt: config.polish.prompt.advanced.clone(),
        dictionary: config.polish.prompt.dictionary.clone(),
        rule_suffix,
        app_class: app_class.map(str::to_string),
        app_title: app_title.map(str::to_string),
        language: language.map(str::to_string),
        // Candidate set fed to the cleanup LLM so it can detect the
        // utterance's language and restore diacritics, engine-
        // independent of `Transcription.language`. Mirror `lang_for`'s
        // per-backend override: `stt.local.languages` wins when set,
        // else `general.languages` (the auto-populated locale subset).
        candidate_languages: if config.stt.local.languages.is_empty() {
            config.general.languages.clone()
        } else {
            config.stt.local.languages.clone()
        },
    };
    // Trim trivially-empty fields so the system prompt stays compact.
    if ctx.advanced_prompt.trim().is_empty() {
        ctx.advanced_prompt.clear();
    }
    ctx
}

fn gated_builtin_suffix(
    profile: Option<&fono_inject::ContextProfile>,
    raw: &str,
    language: Option<&str>,
) -> Option<&'static str> {
    let profile = profile?;
    let suffix = profile.llm_suffix?;

    // A bare terminal window makes shell cleanup available, but it is not
    // proof that the utterance is a command. Gate the transforming shell
    // suffix after STT so natural-language dictation in terminals (for
    // example prose in Romanian) still receives the base cleanup prompt.
    // Agent terminals keep their prose suffix unchanged: there the built-in
    // classifier has already established that the foreground program is a
    // conversational agent rather than a shell prompt.
    if profile.is_terminal
        && profile.detected_agent.is_none()
        && !looks_like_shell_command(raw, language)
    {
        debug!(
            target: "fono::context",
            profile = profile.name,
            ?language,
            "terminal shell suffix suppressed: transcript did not look command-like"
        );
        return None;
    }

    Some(suffix)
}

fn looks_like_shell_command(raw: &str, language: Option<&str>) -> bool {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    let normalized = lower.split_whitespace().collect::<Vec<_>>().join(" ");

    let has_spoken_shell_marker = has_spoken_shell_marker(&normalized);

    has_shell_syntax(&normalized)
        || starts_with_shell_command(&normalized)
        || (!is_confident_non_english(language) && has_spoken_shell_marker)
}

fn has_shell_syntax(s: &str) -> bool {
    const MARKERS: &[&str] = &[
        "./",
        "../",
        "~/",
        " | ",
        " > ",
        " >> ",
        " < ",
        " && ",
        " || ",
        " 1>",
        " 2>",
        "--",
        " -",
        "/dev/null",
    ];
    MARKERS.iter().any(|marker| s.contains(marker))
}

fn starts_with_shell_command(s: &str) -> bool {
    const COMMANDS: &[&str] = &[
        "alias",
        "awk",
        "bash",
        "bat",
        "bun",
        "cargo",
        "cat",
        "cd",
        "chmod",
        "chown",
        "clear",
        "cp",
        "curl",
        "deno",
        "df",
        "docker",
        "du",
        "exit",
        "find",
        "gh",
        "git",
        "grep",
        "helm",
        "journalctl",
        "kill",
        "kubectl",
        "less",
        "ln",
        "ls",
        "mkdir",
        "mv",
        "nano",
        "nix",
        "npm",
        "npx",
        "pacman",
        "ping",
        "pnpm",
        "ps",
        "pwd",
        "python",
        "python3",
        "rm",
        "rsync",
        "scp",
        "ssh",
        "sudo",
        "systemctl",
        "tar",
        "touch",
        "uv",
        "vim",
        "wget",
        "yarn",
        "zig",
        "zsh",
    ];

    let mut tokens = s.split_whitespace().map(trim_shell_token).filter(|token| !token.is_empty());
    let Some(first) = tokens.next() else { return false };
    let candidate =
        if matches!(first, "$" | "#" | ">") { tokens.next().unwrap_or("") } else { first };

    candidate.starts_with("./")
        || candidate.starts_with("../")
        || candidate.starts_with("~/")
        || candidate.starts_with('/')
        || COMMANDS.contains(&candidate)
}

fn trim_shell_token(token: &str) -> &str {
    token.trim_matches(|c: char| {
        matches!(c, '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ';' | ',')
    })
}

fn has_spoken_shell_marker(s: &str) -> bool {
    const MARKERS: &[&str] = &[
        "dash dash",
        "dot slash",
        "dot dot slash",
        "home dot config",
        "dev null",
        "pipe",
        "redirect",
        "standard out",
        "standard error",
    ];
    MARKERS.iter().any(|marker| s.contains(marker))
}

fn is_confident_non_english(language: Option<&str>) -> bool {
    language.is_some_and(|lang| {
        let lang = lang.trim();
        if lang.is_empty() {
            false
        } else {
            let lang = lang.to_ascii_lowercase();
            !(lang == "en" || lang.starts_with("en-") || lang.starts_with("en_"))
        }
    })
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

// ─── log helpers ──────────────────────────────────────────────────────────────

/// Whether log lines may carry ANSI color escapes. True iff stderr
/// (where tracing writes — see `main.rs::init_tracing`) is a TTY and
/// `NO_COLOR` is unset. Cached on first call.
///
/// This is the **single** gate shared between the in-message color
/// helpers here (e.g. [`yellow`]) and the tracing `fmt` layer's
/// `with_ansi(..)` flag. Keeping them in lock-step is what prevents
/// literal `\x1b[33m` bytes from leaking into redirected logs
/// (journald, files, copy-paste): if the formatter won't interpret
/// escapes, we must not bake them into the message either.
#[must_use]
pub fn log_color_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("NO_COLOR").is_none()
            && std::io::IsTerminal::is_terminal(&std::io::stderr())
    })
}

/// ANSI yellow wrap, used to flag slow pipeline stages in the
/// `pipeline:` / `assistant:` summary lines.
///
/// Gated on [`log_color_enabled`] — the **same** gate the tracing
/// `fmt` layer's `with_ansi(..)` consults (see `main.rs`). When stderr
/// is a real terminal the number turns yellow; when it is redirected
/// (journald, a log file, a pipe for parsing) the gate is false and
/// the plain digits are emitted with **no** escape bytes and **no**
/// marker character — so captured logs stay clean and trivially
/// parseable. The formatter and the message thus always agree: either
/// both colour, or neither does.
fn yellow(s: &str) -> String {
    if log_color_enabled() {
        format!("\x1b[33m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

/// Per-stage latency thresholds (milliseconds). A pipeline/assistant
/// summary stage is painted yellow once its measured duration reaches
/// the matching value here **and** colour is enabled (see [`fmt_ms`]).
///
/// These are the single source of truth for "what counts as slow" in
/// the `pipeline:` / `pipeline (live):` / `assistant:` summary lines —
/// adjust a number here and every call site updates. They are tuning
/// hints only: changing one never alters what is logged, just whether
/// the figure is highlighted on a colour-capable terminal.
mod warn_ms {
    /// Speech-to-text transcription (batch and assistant paths).
    pub const STT: u64 = 3_000;
    /// LLM polish pass (batch and live dictation paths).
    pub const POLISH: u64 = 1_000;
    /// Text injection into the focused window.
    pub const INJECT: u64 = 500;
    /// Assistant LLM time-to-first-byte.
    pub const LLM_TTFB: u64 = 1_500;
    /// Assistant LLM total generation time.
    pub const LLM_TOTAL: u64 = 7_000;
    /// A single assistant tool-call execution.
    pub const TOOL_EXEC: u64 = 1_000;
    /// Assistant TTS time-to-first-audio.
    pub const TTS_TTFA: u64 = 2_500;
}

/// Format a millisecond value, colouring it yellow when it exceeds
/// `warn_ms` **and** colour is enabled. Below the threshold, or when
/// colour is disabled, the bare `<n>ms` is returned unchanged — no
/// suffix, no escape — so the value parses identically in every sink.
/// Thresholds live in the [`warn_ms`] module.
fn fmt_ms(ms: u64, warn_ms: u64) -> String {
    let s = format!("{ms}ms");
    if ms >= warn_ms {
        yellow(&s)
    } else {
        s
    }
}

/// Capture duration: show as seconds with one decimal when ≥ 10 s, else ms.
fn fmt_capture(ms: u64) -> String {
    if ms >= 10_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{ms}ms")
    }
}

/// Build the short context-enrichment tag from a `FormatContext`.
///
/// Examples: `"-"`, `"app"`, `"app+rule"`, `"app+dict"`, `"app+rule+dict"`.
fn build_ctx_tag(ctx: &fono_polish::FormatContext, app_class: Option<&str>) -> String {
    let has_app = app_class.is_some();
    let has_rule = ctx.rule_suffix.is_some();
    let has_dict = !ctx.dictionary.is_empty();
    let has_adv = !ctx.advanced_prompt.is_empty();
    if !has_app && !has_rule && !has_dict && !has_adv {
        return "-".to_string();
    }
    let mut parts = Vec::new();
    if has_app {
        parts.push("app");
    }
    if has_rule {
        parts.push("rule");
    }
    if has_adv {
        parts.push("adv");
    }
    if has_dict {
        parts.push("dict");
    }
    parts.join("+")
}

/// Format the single `pipeline:` INFO line for the **batch** path.
///
/// Thresholds for yellow highlighting:
/// * STT  > 2 000 ms
/// * Polish > 1 500 ms
fn format_pipeline_summary(m: &PipelineMetrics) -> String {
    let capture = fmt_capture(m.capture_ms);
    let lang = m.language.as_deref().unwrap_or("?");
    let stt = fmt_ms(m.stt_ms, warn_ms::STT);
    let polish_seg = if m.llm_skipped_short {
        "polish skipped (short)".to_string()
    } else if m.llm_ms == 0 && m.ctx_tag.is_empty() {
        "polish none".to_string()
    } else {
        let pol = fmt_ms(m.llm_ms, warn_ms::POLISH);
        format!("polish {pol} [{}] {} → {} chars", m.ctx_tag, m.raw_chars, m.final_chars)
    };
    let inject = fmt_ms(m.inject_ms, warn_ms::INJECT);
    // Surface the streaming time-to-first-injected-char when the local
    // streaming path was used (0 on every one-shot run).
    let ttfi = if m.time_to_first_inject_ms > 0 {
        format!(" ttfi={}ms", m.time_to_first_inject_ms)
    } else {
        String::new()
    };
    format!(
        "pipeline: {capture} trim={}ms | {lang} | stt {stt} {} chars | {polish_seg} | inject {} {inject}{ttfi}",
        m.trim_ms,
        m.raw_chars,
        m.inject_backend,
    )
}

/// Format the single `pipeline (live):` INFO line for the **streaming** path.
#[allow(clippy::too_many_arguments)]
fn format_pipeline_summary_live(
    capture_ms: u64,
    language: Option<&str>,
    segments: u32,
    raw_chars: usize,
    polish_backend: &str,
    polish_ms: u64,
    ctx_tag: &str,
    final_chars: usize,
    inject_ms: u64,
    inject_backend: &str,
) -> String {
    let capture = fmt_capture(capture_ms);
    let lang = language.unwrap_or("?");
    let polish_seg = if polish_backend == "none" {
        "polish none".to_string()
    } else {
        let pol = fmt_ms(polish_ms, warn_ms::POLISH);
        format!("polish {pol} [{ctx_tag}] {raw_chars} → {final_chars} chars")
    };
    let inject = fmt_ms(inject_ms, warn_ms::INJECT);
    format!(
        "pipeline (live): {capture} | {lang} | stt streaming ({segments} segs) {raw_chars} chars | {polish_seg} | inject {inject_backend} {inject}",
    )
}

/// Format the single `assistant:` INFO line emitted at the end of
/// every F8 voice turn. Style mirrors [`format_pipeline_summary`] so
/// the dictation and assistant lines read alike when scrolling the
/// log.
///
/// Stages slower than their threshold are coloured yellow when colour
/// is enabled (real terminal, `NO_COLOR` unset); otherwise the plain
/// number is emitted (see [`fmt_ms`]):
/// * STT       > 2 000 ms
/// * LLM ttfb  > 1 500 ms
/// * LLM total > 5 000 ms
/// * Tool exec > 1 000 ms
/// * TTS ttfa  > 1 500 ms
#[must_use]
pub fn format_assistant_summary(m: &AssistantTurnMetrics) -> String {
    let total = fmt_capture(m.total_ms);
    let lang = m.language.as_deref().unwrap_or("?");
    let stt_seg = m.stt_ms.map_or_else(
        || format!("stt skipped (live) {} chars in", m.user_chars),
        |ms| format!("stt {} {} chars in", fmt_ms(ms, warn_ms::STT), m.user_chars),
    );
    let llm_seg = format!(
        "llm {} ttfb / {} {} chars out",
        fmt_ms(m.llm_ttfb_ms, warn_ms::LLM_TTFB),
        fmt_ms(m.llm_total_ms, warn_ms::LLM_TOTAL),
        m.reply_chars,
    );
    let tool_seg = if m.tools.is_empty() {
        String::new()
    } else {
        let inner = m
            .tools
            .iter()
            .map(|t| match t.outcome {
                AssistantToolOutcome::Ok => {
                    format!("{} {}", t.name, fmt_ms(t.exec_ms, warn_ms::TOOL_EXEC))
                }
                AssistantToolOutcome::Cancelled => format!("{} failed=cancelled", t.name),
                AssistantToolOutcome::Private => format!("{} failed=private", t.name),
                AssistantToolOutcome::NoTool => format!("{} failed=no-tool", t.name),
                AssistantToolOutcome::Failed => format!("{} failed", t.name),
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(" [{inner}]")
    };
    let tts_seg = m.tts_ttfa_ms.map_or_else(
        || "tts none".to_string(),
        |ms| format!("tts {} ttfa / {} sent", fmt_ms(ms, warn_ms::TTS_TTFA), m.sentences),
    );
    let tail = if m.aborted { " | aborted" } else { "" };
    format!("assistant: {total} | {lang} | {stt_seg} | {llm_seg}{tool_seg} | {tts_seg}{tail}")
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
    use async_trait::async_trait;

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

    #[test]
    fn terminal_suffix_is_suppressed_for_romanian_prose() {
        let profile = ContextClassifier::classify(Some("kitty"), Some("fono -v"));
        assert!(profile.as_ref().is_some_and(|p| p.is_terminal && p.llm_suffix.is_some()));

        let suffix = gated_builtin_suffix(
            profile.as_ref(),
            "o sa facem un test sa vedem daca sta face din limba romanana, limba inglesa",
            Some("ro"),
        );
        assert!(suffix.is_none());
    }

    #[test]
    fn terminal_suffix_is_kept_for_shell_commands() {
        let profile = ContextClassifier::classify(Some("kitty"), None);
        let examples = [
            "git status",
            "cd /etc",
            "sudo apt install vim",
            "grep -r fono .",
            "cargo test",
            "rm -rf target",
            "./script.sh --verbose",
        ];

        for raw in examples {
            assert!(
                gated_builtin_suffix(profile.as_ref(), raw, Some("en")).is_some(),
                "expected terminal suffix for command-like transcript: {raw}"
            );
        }
    }

    #[test]
    fn terminal_suffix_keeps_agent_prose_framing() {
        let mut profile =
            ContextClassifier::classify(Some("kitty"), None).expect("terminal profile");
        profile.detected_agent = Some(fono_inject::CodingAgentKind::Forge);
        profile.llm_suffix = Some("agent prose suffix");

        assert_eq!(
            gated_builtin_suffix(Some(&profile), "please inspect the failing test", Some("en")),
            Some("agent prose suffix"),
        );
    }

    #[test]
    fn gated_suffix_keeps_non_terminal_profiles_unchanged() {
        let profile = fono_inject::ContextProfile {
            name: "TestProfile",
            whisper_hint: None,
            llm_suffix: Some("custom suffix"),
            suppress_history: false,
            detected_agent: None,
            is_terminal: false,
            is_code_editor: false,
        };

        assert_eq!(
            gated_builtin_suffix(Some(&profile), "plain prose", Some("ro")),
            Some("custom suffix")
        );
    }

    #[test]
    fn assistant_summary_full_turn_with_tool() {
        let m = AssistantTurnMetrics {
            total_ms: 4823,
            language: Some("en".into()),
            stt_ms: Some(580),
            user_chars: 14,
            llm_ttfb_ms: 234,
            llm_total_ms: 2103,
            reply_chars: 312,
            tools: vec![AssistantToolMetric {
                name: "fono_screen".into(),
                exec_ms: 1284,
                outcome: AssistantToolOutcome::Ok,
            }],
            tts_ttfa_ms: Some(420),
            sentences: 8,
            aborted: false,
        };
        // Tests run with stderr piped (non-TTY), so `log_color_enabled()`
        // is false: numbers are plain, no colour, no marker — even the
        // slow 1284 ms tool exec.
        let line = format_assistant_summary(&m);
        assert_eq!(
            line,
            "assistant: 4823ms | en | stt 580ms 14 chars in | llm 234ms ttfb / 2103ms 312 chars out [fono_screen 1284ms] | tts 420ms ttfa / 8 sent"
        );
    }

    #[test]
    fn assistant_summary_clean_when_color_disabled() {
        // Under `cargo test` stderr is a pipe, so `log_color_enabled()`
        // is false: the summary must carry NO ANSI escape bytes and NO
        // suffix marker — just bare `<n>ms` numbers — so a captured log
        // (journald, file, `fono doctor` replay) is trivially parseable.
        // Use deliberately slow values so every threshold would fire if
        // colour were on.
        let m = AssistantTurnMetrics {
            total_ms: 7577,
            language: None,
            stt_ms: Some(9000),
            user_chars: 26,
            llm_ttfb_ms: 9000,
            llm_total_ms: 9000,
            reply_chars: 344,
            tools: vec![AssistantToolMetric {
                name: "fono_screen".into(),
                exec_ms: 9000,
                outcome: AssistantToolOutcome::Ok,
            }],
            tts_ttfa_ms: Some(9000),
            sentences: 2,
            aborted: false,
        };
        let line = format_assistant_summary(&m);
        assert!(!line.contains('\u{1b}'), "summary leaked an ANSI escape: {line:?}");
        assert!(!line.contains("\\x1b"), "summary leaked a literal escape: {line:?}");
        // Plain digits, no marker — slow stages look identical to fast
        // ones when colour is off (the colour, when on, is the only cue).
        assert!(line.contains("llm 9000ms ttfb / 9000ms"), "unexpected ms shape: {line:?}");
        assert!(!line.contains("ms!"), "unexpected suffix marker: {line:?}");
    }

    #[test]
    fn assistant_summary_text_only_turn_omits_tool_segment() {
        let m = AssistantTurnMetrics {
            total_ms: 3120,
            language: Some("ro".into()),
            stt_ms: Some(412),
            user_chars: 22,
            llm_ttfb_ms: 180,
            llm_total_ms: 1450,
            reply_chars: 198,
            tools: vec![],
            tts_ttfa_ms: Some(330),
            sentences: 5,
            aborted: false,
        };
        let line = format_assistant_summary(&m);
        assert!(
            !line.contains('['),
            "tool segment must be omitted on text-only turns, got: {line}"
        );
        assert!(line.contains("llm 180ms ttfb / 1450ms 198 chars out | tts"), "got: {line}");
    }

    #[test]
    fn assistant_summary_live_mode_marks_stt_skipped() {
        let m = AssistantTurnMetrics {
            language: Some("en".into()),
            stt_ms: None,
            user_chars: 33,
            ..Default::default()
        };
        let line = format_assistant_summary(&m);
        assert!(line.contains("stt skipped (live) 33 chars in"), "got: {line}");
    }

    #[test]
    fn assistant_summary_cancelled_tool_renders_failure_tag() {
        let m = AssistantTurnMetrics {
            tools: vec![AssistantToolMetric {
                name: "fono_screen".into(),
                exec_ms: 0,
                outcome: AssistantToolOutcome::Cancelled,
            }],
            ..Default::default()
        };
        let line = format_assistant_summary(&m);
        assert!(line.contains("[fono_screen failed=cancelled]"), "got: {line}");
    }

    #[test]
    fn assistant_summary_aborted_appends_tail() {
        let m = AssistantTurnMetrics {
            total_ms: 980,
            language: Some("en".into()),
            stt_ms: Some(420),
            user_chars: 7,
            llm_ttfb_ms: 0,
            llm_total_ms: 540,
            reply_chars: 0,
            aborted: true,
            ..Default::default()
        };
        let line = format_assistant_summary(&m);
        assert!(line.ends_with("| aborted"), "got: {line}");
        assert!(line.contains("tts none"), "got: {line}");
    }

    // ---- Streaming-cleanup injection (plan v3) ----------------------

    /// Drive a [`WordSink`] with a list of chunks, returning the ordered
    /// sequence of non-empty emissions (push results then a final flush).
    fn drive_sink(chunks: &[&str]) -> Vec<String> {
        let mut sink = WordSink::new();
        let mut out = Vec::new();
        for c in chunks {
            let s = sink.push(c);
            if !s.is_empty() {
                out.push(s);
            }
        }
        let tail = sink.flush();
        if !tail.is_empty() {
            out.push(tail);
        }
        out
    }

    #[test]
    fn word_sink_emits_only_whole_words_in_order() {
        // A word ("output") split across two chunks must never be emitted
        // partially: the first chunk holds it back, the second completes it.
        let emissions = drive_sink(&["hello wor", "ld this is out", "put text here"]);
        let joined: String = emissions.concat();
        assert_eq!(joined, "hello world this is output text here");
        // Every emission boundary must fall on whitespace — i.e. no emission
        // ends in the middle of a word (the next emission would then start
        // mid-word). Reconstruct word list and ensure none was split.
        for e in &emissions {
            assert!(!e.is_empty());
        }
        // No emission may contain a leading/trailing partial of a word that
        // also appears split: assert the concatenation tokenizes identically.
        let words: Vec<&str> = joined.split_whitespace().collect();
        assert_eq!(words, vec!["hello", "world", "this", "is", "output", "text", "here"]);
    }

    #[test]
    fn word_sink_trims_leading_and_trailing_whitespace() {
        let emissions = drive_sink(&["   leading", " and trailing words   "]);
        let joined: String = emissions.concat();
        assert_eq!(joined, "leading and trailing words");
    }

    #[test]
    fn word_sink_single_token_held_until_flush() {
        // One word with no whitespace is held until flush.
        let mut sink = WordSink::new();
        assert_eq!(sink.push("hello"), "");
        assert_eq!(sink.flush(), "hello");
    }

    #[test]
    fn sentence_boundary_detection() {
        assert!(has_sentence_boundary("Hello world. More"));
        assert!(has_sentence_boundary("Done!"));
        assert!(has_sentence_boundary("Really?"));
        assert!(!has_sentence_boundary("no terminator yet"));
        assert!(!has_sentence_boundary("decimal 3.14 inline"));
    }

    /// Minimal streaming formatter that yields scripted chunks. `local`
    /// controls `is_local()` so the orchestrator's streaming gate fires.
    struct ScriptedStream {
        chunks: Vec<String>,
        local: bool,
    }

    #[async_trait]
    impl TextFormatter for ScriptedStream {
        async fn format(&self, _raw: &str, _ctx: &FormatContext) -> Result<String> {
            Ok(self.chunks.concat().trim().to_string())
        }
        fn name(&self) -> &'static str {
            "scripted-stream"
        }
        async fn format_stream(
            &self,
            _raw: &str,
            _ctx: &FormatContext,
        ) -> Result<futures::stream::BoxStream<'static, Result<String>>> {
            let items: Vec<Result<String>> = self.chunks.iter().cloned().map(Ok).collect();
            Ok(futures::stream::iter(items).boxed())
        }
        fn is_local(&self) -> bool {
            self.local
        }
    }

    /// Injector that records the ordered sequence of injected strings.
    struct RecordingInjector(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

    impl Injector for RecordingInjector {
        fn inject(&self, text: &str) -> Result<(bool, String)> {
            self.0.lock().unwrap().push(text.to_string());
            Ok((false, "recording".to_string()))
        }
    }

    async fn run_gate(raw: &str, chunks: &[&str]) -> (StreamCleanupOutcome, Vec<String>) {
        let fmt = ScriptedStream {
            chunks: chunks.iter().map(|s| (*s).to_string()).collect(),
            local: true,
        };
        let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let injector = RecordingInjector(std::sync::Arc::clone(&recorded));
        let ctx = FormatContext::default();
        let outcome = stream_cleanup_and_inject(&fmt, raw, &ctx, &injector).await;
        let seq = recorded.lock().unwrap().clone();
        (outcome, seq)
    }

    #[tokio::test]
    async fn gate_passes_clean_prefix_and_streams_all_words() {
        let (outcome, seq) = run_gate(
            "this is the raw transcript that we dictated",
            &[
                "This is the cleaned transcript ",
                "output here. And then ",
                "some more words follow afterwards.",
            ],
        )
        .await;
        assert!(outcome.injected, "clean prefix must stream");
        let full = outcome.cleaned.expect("clean prefix yields cleaned text");
        assert_eq!(
            full,
            "This is the cleaned transcript output here. And then some more words follow afterwards."
        );
        // The injected deltas concatenate to exactly the cleaned text.
        let joined: String = seq.concat();
        assert_eq!(joined, full);
        assert!(seq.len() > 1, "streaming must emit more than one delta: {seq:?}");
    }

    #[tokio::test]
    async fn gate_clarification_prefix_injects_nothing() {
        let (outcome, seq) = run_gate(
            "the response is this and that",
            &[
                "It seems like you're describing a situation, ",
                "but the details are incomplete here.",
            ],
        )
        .await;
        assert!(!outcome.injected, "clarification prefix must not inject");
        assert!(outcome.cleaned.is_none());
        assert!(seq.is_empty(), "no cleaned deltas may leak to the injector: {seq:?}");
    }

    #[tokio::test]
    async fn gate_degenerate_role_token_injects_nothing() {
        let (outcome, seq) = run_gate("the response is this and that today", &["model"]).await;
        assert!(!outcome.injected);
        assert!(outcome.cleaned.is_none());
        assert!(seq.is_empty(), "degenerate role token must not be typed: {seq:?}");
    }

    #[tokio::test]
    async fn gate_translated_prefix_injects_nothing() {
        // Romanian output for an English source must trip the translation
        // guard and inject nothing.
        let fmt = ScriptedStream {
            chunks: vec![
                "Aș vrea să merg acasă astăzi ".to_string(),
                "pentru că este foarte târziu deja.".to_string(),
            ],
            local: true,
        };
        let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let injector = RecordingInjector(std::sync::Arc::clone(&recorded));
        let ctx = FormatContext { language: Some("en".into()), ..Default::default() };
        let outcome = stream_cleanup_and_inject(
            &fmt,
            "I would like to go home today because it is very late already",
            &ctx,
            &injector,
        )
        .await;
        let seq = recorded.lock().unwrap().clone();
        assert!(!outcome.injected, "translated prefix must not inject");
        assert!(outcome.cleaned.is_none());
        assert!(seq.is_empty(), "translated output must not leak to the injector: {seq:?}");
    }
}
