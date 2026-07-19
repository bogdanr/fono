// SPDX-License-Identifier: GPL-3.0-only
//! Voice-assistant pipeline. Orchestrates the F8 (or IPC-triggered)
//! assistant turn:
//!
//! ```text
//!   captured PCM ─▶ STT ─▶ Assistant.reply_stream
//!                              │ token deltas
//!                              ▼
//!                       SentenceSplitter ─▶ TextToSpeech.synthesize
//!                                                │ TtsAudio
//!                                                ▼
//!                                         AudioPlayback (cpal/paplay)
//! ```
//!
//! Lives in a dedicated module so `session.rs` stays focused on the
//! dictation pipeline. The orchestrator (in `session.rs`) owns the
//! [`AssistantSessionState`] and calls into the pump function from
//! `on_assistant_hold_release`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use fono_assistant::{
    Assistant, AssistantContext, ConversationHistory, ScreenCaptureFn, ToolEvent,
};
#[cfg(feature = "realtime")]
use fono_assistant::{RealtimeAssistant, RealtimeEvent, RealtimeMode, RealtimeSession};
use fono_audio::AudioPlayback;
#[cfg(feature = "realtime")]
use fono_audio::{rms_to_dbfs, EnvelopeConfig, EnvelopeFollower};
#[cfg(feature = "realtime")]
use fono_audio::{AudioCapture, CaptureConfig, RecordingBuffer};
use fono_core::turn_trace::TurnTrace;
use fono_hotkey::HotkeyAction;
use fono_stt::SpeechToText;
use fono_tts::{SentenceSplitter, TextToSpeech};
use futures::stream::StreamExt;
use serde_json::json;
use tokio::sync::{mpsc, Mutex, Notify};
use tracing::{debug, info, warn};

use crate::session::{
    format_assistant_summary, AssistantToolMetric, AssistantToolOutcome, AssistantTurnMetrics,
};

/// Per-orchestrator assistant state. Owned by the
/// [`crate::session::SessionOrchestrator`] inside an `Arc<Mutex<…>>`
/// so the IPC handlers and the pump task share a single source of
/// truth.
///
/// `playback` is created lazily on first use (so a daemon that never
/// hits F8 doesn't open an audio output stream / spawn paplay) and
/// reused across turns. `current_turn` is `Some(_)` while a pump
/// task is running; calling [`Self::stop_current_turn`] notifies the
/// pump to abort and drains the audio queue.
pub struct AssistantSessionState {
    pub history: ConversationHistory,
    pub current_turn: Option<Arc<Notify>>,
    pub playback: Option<AudioPlayback>,
    /// Active full-duplex live-conversation session, when one is open.
    /// `Some` between the F8-tap that entered live mode and the teardown
    /// (second tap / Escape / idle / cap / provider-close). Distinct
    /// from `current_turn` (the per-press PTT cancel handle): a live
    /// session spans many turns. `None` for every push-to-talk and
    /// staged configuration.
    #[cfg(feature = "realtime")]
    pub live: Option<LiveSessionHandle>,
}

impl AssistantSessionState {
    #[must_use]
    pub fn new(history: ConversationHistory) -> Self {
        Self {
            history,
            current_turn: None,
            playback: None,
            #[cfg(feature = "realtime")]
            live: None,
        }
    }

    /// Notify the active pump to abort and ask the playback handle to
    /// drain its queue. History is preserved.
    pub fn stop_current_turn(&mut self) {
        if let Some(notify) = self.current_turn.take() {
            notify.notify_waiters();
        }
        if let Some(pb) = &self.playback {
            pb.stop();
        }
    }

    /// Drop the assistant playback handle, releasing the audio device.
    /// Used on full shutdown; routine cancellation should call
    /// [`Self::stop_current_turn`] instead.
    pub fn shutdown(&mut self) {
        self.stop_current_turn();
        #[cfg(feature = "realtime")]
        if let Some(live) = self.live.take() {
            live.cancel.notify_one();
            live.task.abort();
        }
        self.playback = None;
    }
}

/// Handle to a running full-duplex live-conversation session. Held in
/// [`AssistantSessionState::live`] for the session's lifetime.
#[cfg(feature = "realtime")]
pub struct LiveSessionHandle {
    /// Signalled (via `notify_one`, so the wake is never lost) to tear
    /// the live pump down on explicit exit (second tap / Escape).
    pub cancel: Arc<Notify>,
    /// The spawned pump task. Awaited (after `cancel`) or aborted on
    /// teardown.
    pub task: tokio::task::JoinHandle<()>,
    /// When the session opened. Drives the max-session backstop and any
    /// overlay elapsed meter.
    pub started_at: std::time::Instant,
}

/// Inputs for [`run_assistant_turn`]. Cloning is cheap (everything is
/// `Arc`).
pub struct AssistantTurnInputs {
    /// Captured PCM. Ignored when `pre_transcribed` is `Some` — the
    /// caller has already run the STT step (live-streaming F8 path).
    pub pcm: Vec<f32>,
    pub sample_rate: u32,
    /// Batch STT backend. Unused when `pre_transcribed` is `Some`.
    pub stt: Arc<dyn SpeechToText>,
    pub assistant: Arc<dyn Assistant>,
    /// TTS backend, or `None` for a text-only turn. When absent the
    /// reply is streamed to the overlay as on-screen text and held for
    /// a reading-time dwell instead of being synthesised + played back
    pub tts: Option<Arc<dyn TextToSpeech>>,
    pub system_prompt: String,
    /// Verified enrolled speaker for this turn, when speaker verification
    /// is enabled and the captured voice matched (name only, never the
    /// embedding). Annotates the `assistant:` summary line; `None`
    /// otherwise.
    pub speaker: Option<String>,
    pub language: Option<String>,
    /// Channel back into the FSM. The pump sends
    /// [`HotkeyAction::AssistantSpeakingStarted`] the moment the
    /// first synthesised audio chunk is enqueued for playback — i.e.
    /// when the user actually starts hearing the reply. The earlier
    /// "first LLM delta" milestone is reflected on the *overlay*
    /// (THINKING → SYNTHESISING) but not on the FSM: the user
    /// considers the silent synth phase part of "thinking" for
    /// cancel / tray / barge-in purposes. Drives
    /// `AssistantThinking → AssistantSpeaking`. The same channel
    /// also receives `ProcessingDone` from the daemon dispatcher
    /// after the turn ends (we don't fire it from here).
    pub action_tx: mpsc::UnboundedSender<HotkeyAction>,
    /// Optional overlay handle. When present the pump flips it from
    /// `AssistantThinking` → `AssistantSynthesising` (first LLM
    /// delta) → `AssistantSpeaking` (first audio queued) so the user
    /// can read at a glance which sub-phase of the reply is current.
    /// `None` covers test paths and headless / `noop` overlay
    /// backends.
    pub overlay: Option<fono_overlay::OverlayHandle>,
    /// When `Some`, skip the batch STT step entirely and treat this
    /// string as the user's turn. Set by the live-streaming F8 path
    /// (interactive mode + streaming-capable backend) so the same
    /// transcription that drove the realtime overlay preview gets
    /// forwarded to the LLM rather than re-running STT.
    pub pre_transcribed: Option<String>,
    /// Whether to include the `fono_screen` tool in LLM requests (from
    /// `[assistant].prefer_vision`).
    pub prefer_vision: bool,
    /// Optional screen-capture callback. When `Some`, the LLM may
    /// call `fono_screen` to grab a screenshot. Built from a
    /// `GrabberProbe` in the F8 voice loop when `prefer_vision` is
    /// true and the backend is vision-capable.
    pub screen_capture_fn: Option<ScreenCaptureFn>,
    /// Runtime-only active-window context captured at assistant hotkey press.
    /// Local backends can cache this independently from stable system prompts.
    pub active_window_context: Option<String>,
}

/// Inputs for [`run_realtime_turn`] — the speech-to-speech (Gemini
/// Live) path. Unlike the staged turn there is no separate STT / TTS:
/// the model ingests the captured mic PCM and streams reply audio back
/// over one WebSocket session.
#[cfg(feature = "realtime")]
pub struct RealtimeTurnInputs {
    /// Source of captured mic PCM frames (mono f32 at `sample_rate`)
    /// for this push-to-talk turn. Each frame is resampled to the
    /// backend's [`RealtimeAssistant::native_input_rate`] and pushed
    /// into the live session as it arrives; the turn ends when the
    /// stream closes. For the buffered (record-then-send) path the
    /// call site wraps a finished `Vec<f32>` via [`buffered_frame_stream`];
    /// for live mic streaming the capture forwarder feeds it directly.
    pub frames: mpsc::UnboundedReceiver<Vec<f32>>,
    pub sample_rate: u32,
    pub realtime: Arc<dyn RealtimeAssistant>,
    pub system_prompt: String,
    pub language: Option<String>,
    /// Same FSM channel as the staged path: the turn sends
    /// [`HotkeyAction::AssistantSpeakingStarted`] on the first reply
    /// audio frame.
    pub action_tx: mpsc::UnboundedSender<HotkeyAction>,
    pub overlay: Option<fono_overlay::OverlayHandle>,
    /// Whether to send a screenshot frame with the realtime session
    /// (from `[assistant].prefer_vision`). When true and a capture
    /// callback is present, `open_session` grabs the screen once and
    /// sends it as a `realtimeInput.video` frame before the mic audio.
    pub prefer_vision: bool,
    /// Optional screen-capture callback. Built from a `GrabberProbe`
    /// in the F8 voice loop when `prefer_vision` is true and the
    /// backend is vision-capable. Mirrors the staged turn's field.
    pub screen_capture_fn: Option<ScreenCaptureFn>,
    /// Runtime-only active-window context captured at hotkey press.
    pub active_window_context: Option<String>,
}

/// What one realtime reply produced. Mirrors the fields the staged
/// turn tracks so the `assistant:` summary line renders consistently.
#[cfg(feature = "realtime")]
#[derive(Default)]
struct RealtimeReply {
    /// User utterance as transcribed by the model's own input
    /// transcription (pushed as the user history turn).
    user_text: Option<String>,
    /// Incremental transcript of the spoken reply.
    reply_text: String,
    any_audio: bool,
    aborted: bool,
    last_audio_at: Option<std::time::Instant>,
}

/// Run one assistant turn: STT the captured PCM, push the user turn
/// onto history, stream the model's reply, synthesise sentences as
/// they arrive, queue audio for playback. Cancellable via the
/// `notify`: the pump checks it between deltas / sentences and bails
/// out cleanly.
///
/// Errors are logged but don't propagate — the caller resets state
/// regardless. Returns `Ok(true)` if anything was actually played,
/// `Ok(false)` on early-out (empty STT, abort).
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
pub async fn run_assistant_turn(
    state: Arc<Mutex<AssistantSessionState>>,
    inputs: AssistantTurnInputs,
    notify: Arc<Notify>,
) -> Result<bool> {
    let AssistantTurnInputs {
        pcm,
        sample_rate,
        stt,
        assistant,
        tts,
        system_prompt,
        speaker,
        language,
        action_tx,
        overlay,
        pre_transcribed,
        prefer_vision,
        screen_capture_fn,
        active_window_context,
    } = inputs;

    // Turn-wide metrics. Populated as the pump progresses; emitted
    // as a single `assistant:` INFO line on the success path. When
    // `FONO_ASSISTANT_TRACE` is set, also write a Chrome Trace Event JSON
    // waterfall for the turn.
    let trace = TurnTrace::start_from_env();
    let _trace_guard = trace.as_ref().map(TurnTrace::make_current);
    if let Some(t) = &trace {
        t.instant("turn.start", "assistant", "assistant-pump", json!({ "turn_id": t.id() }));
    }
    let turn_started = std::time::Instant::now();
    let mut metrics =
        AssistantTurnMetrics { language: language.clone(), speaker, ..Default::default() };

    // 1. Resolve the user's text. When `pre_transcribed` is set the
    //    caller already ran streaming STT (live-mode F8 path); we
    //    skip the batch call entirely. Otherwise run STT on the
    //    captured PCM.
    let user_text = if let Some(text) = pre_transcribed {
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            debug!(target: "fono::assistant", "skip: empty pre-transcribed text");
            if let Some(t) = &trace {
                t.finish(json!({
                    "aborted": true,
                    "reason": "empty_pre_transcribed",
                    "summary": t.cache_scoreboard(),
                }));
            }
            return Ok(false);
        }
        if let Some(t) = &trace {
            t.instant(
                "stt.pre_transcribed",
                "assistant.stt",
                "stt",
                json!({ "chars": trimmed.chars().count() }),
            );
        }
        debug!(
            target: "fono::assistant",
            "pre-transcribed: {trimmed:?}"
        );
        trimmed
    } else {
        if pcm.is_empty() {
            debug!(target: "fono::assistant", "skip: empty PCM");
            if let Some(t) = &trace {
                t.finish(json!({
                    "aborted": true,
                    "reason": "empty_pcm",
                    "summary": t.cache_scoreboard(),
                }));
            }
            return Ok(false);
        }
        let stt_started = std::time::Instant::now();
        if let Some(t) = &trace {
            // Device-level capture (mic open, first frame) happens before this
            // turn's trace exists, so surface the *recorded input* bounds on the
            // capture lane here — its width is the audio the user actually
            // recorded, making the record→STT→playback boundary obvious.
            let duration_ms = if sample_rate > 0 {
                (pcm.len() as f64 / f64::from(sample_rate) * 1000.0).round() as u64
            } else {
                0
            };
            t.instant(
                "capture.input",
                "capture",
                fono_core::turn_trace::CAPTURE_LANE,
                json!({ "samples": pcm.len(), "sample_rate": sample_rate, "duration_ms": duration_ms }),
            );
        }
        let transcription = tokio::select! {
            biased;
            () = notify.notified() => {
                debug!(target: "fono::assistant", "cancelled before STT");
                if let Some(t) = &trace {
                    t.duration_between(
                        "stt.transcribe",
                        "assistant.stt",
                        "stt",
                        stt_started,
                        std::time::Instant::now(),
                        json!({ "cancelled": true }),
                    );
                    t.finish(json!({
                        "aborted": true,
                        "reason": "cancelled_before_stt",
                        "summary": t.cache_scoreboard(),
                    }));
                }
                return Ok(false);
            }
            r = stt.transcribe(&pcm, sample_rate, language.as_deref()) => match r {
                Ok(t) => t,
                Err(e) => {
                    // STT backend failed before producing a transcript —
                    // typically auth (bad / project-denied key, e.g. a Gemini
                    // 403 PERMISSION_DENIED), payment, network, or terms.
                    // Mirror the LLM-stage handling below so the user gets one
                    // desktop notification instead of a silent `warn!` at the
                    // session level; the global session cap suppresses any
                    // cascading popups.
                    let err_text = format!("{e:#}");
                    let class = fono_core::critical_notify::classify(&err_text);
                    if matches!(
                        class,
                        fono_core::critical_notify::ErrorClass::Auth
                            | fono_core::critical_notify::ErrorClass::PaymentRequired
                            | fono_core::critical_notify::ErrorClass::Network
                            | fono_core::critical_notify::ErrorClass::TermsRequired
                    ) {
                        fono_core::critical_notify::notify(
                            fono_core::critical_notify::Stage::Stt,
                            stt.name(),
                            class,
                            &err_text,
                        );
                    }
                    if let Some(t) = &trace {
                        t.duration_between(
                            "stt.transcribe",
                            "assistant.stt",
                            "stt",
                            stt_started,
                            std::time::Instant::now(),
                            json!({ "error": err_text }),
                        );
                        t.finish(json!({
                            "aborted": true,
                            "reason": "stt_error",
                            "summary": t.cache_scoreboard(),
                        }));
                    }
                    return Err(e);
                }
            },
        };
        let trimmed = transcription.text.trim().to_string();
        if trimmed.is_empty() {
            debug!(target: "fono::assistant", "skip: empty transcript");
            if let Some(t) = &trace {
                t.duration_between(
                    "stt.transcribe",
                    "assistant.stt",
                    "stt",
                    stt_started,
                    std::time::Instant::now(),
                    json!({ "empty": true }),
                );
                t.finish(json!({
                    "aborted": true,
                    "reason": "empty_transcript",
                    "summary": t.cache_scoreboard(),
                }));
            }
            return Ok(false);
        }
        let stt_ms = stt_started.elapsed().as_millis() as u64;
        if let Some(t) = &trace {
            t.duration_between(
                "stt.transcribe",
                "assistant.stt",
                "stt",
                stt_started,
                std::time::Instant::now(),
                json!({
                    "chars_out": trimmed.chars().count(),
                    "language": transcription.language.as_deref().unwrap_or(""),
                    "sample_rate": sample_rate,
                }),
            );
        }
        metrics.stt_ms = Some(stt_ms);
        // Prefer the language the STT engine actually detected over the
        // configured hint (which is `None` in auto-detect mode and would
        // otherwise render as `?` in the summary line).
        if let Some(lang) = transcription.language.as_ref().filter(|s| !s.trim().is_empty()) {
            metrics.language = Some(lang.clone());
        }
        debug!(
            target: "fono::assistant",
            stt_ms,
            "STT: {trimmed:?}"
        );
        trimmed
    };
    metrics.user_chars = user_text.chars().count();
    if let Some(t) = &trace {
        t.instant(
            "user.text",
            "assistant",
            "assistant-pump",
            json!({ "chars": metrics.user_chars }),
        );
    }

    // 2. Build context from the *completed* history, then record the current
    //    user turn so it persists for the NEXT turn. The snapshot MUST exclude
    //    the in-flight user turn: every backend's prompt/message builder treats
    //    `user_text` as the current turn and renders it itself (the local
    //    builder via the trailing turn marker + suffix; the cloud builders via a
    //    final user message). Including the current turn in `ctx.history` too
    //    would (a) duplicate the user's message in the prompt sent to the model
    //    and (b) make each turn's cached prompt-state prefix end in a volatile
    //    `<start_of_turn>user\n` marker that the next turn overwrites with the
    //    model reply, defeating prompt-state cache reuse (only the static system
    //    base could ever be restored, so prefill grew with every turn).
    let history_snapshot = {
        let history_started = std::time::Instant::now();
        let mut s = state.lock().await;
        let snapshot = s.history.snapshot();
        s.history.push_user(user_text.clone());
        drop(s);
        if let Some(t) = &trace {
            t.duration_between(
                "history.snapshot",
                "assistant",
                "assistant-pump",
                history_started,
                std::time::Instant::now(),
                json!({ "turns": snapshot.len() }),
            );
        }
        snapshot
    };
    let ctx = AssistantContext {
        system_prompt,
        language,
        history: history_snapshot,
        active_window_context,
        screen_capture: screen_capture_fn,
        prefer_vision,
        max_new_tokens: None,
        // Local hotkey-triggered turn: this is the one the on-screen
        // overlay is meant to visualize, so allow the brain-capture tap.
        allow_brain_capture: true,
    };

    // 3. Open the LLM stream.
    let llm_started = std::time::Instant::now();
    let mut deltas = tokio::select! {
        biased;
        () = notify.notified() => {
            debug!(target: "fono::assistant", "cancelled before LLM");
            if let Some(t) = &trace {
                t.duration_between(
                    "llm.reply_stream_open",
                    "assistant.llm",
                    "llm",
                    llm_started,
                    std::time::Instant::now(),
                    json!({ "cancelled": true, "provider": assistant.name() }),
                );
                t.finish(json!({
                    "aborted": true,
                    "reason": "cancelled_before_llm",
                    "summary": t.cache_scoreboard(),
                }));
            }
            return Ok(false);
        }
        r = assistant.reply_stream(&user_text, &ctx) => match r {
            Ok(d) => {
                if let Some(t) = &trace {
                    t.duration_between(
                        "llm.reply_stream_open",
                        "assistant.llm",
                        "llm",
                        llm_started,
                        std::time::Instant::now(),
                        json!({ "provider": assistant.name() }),
                    );
                }
                d
            }
            Err(e) => {
                // Assistant backend refused to even open the
                // stream — typically auth (bad key) or network
                // (offline endpoint). Surface once per session;
                // the global cap keeps cascading STT/LLM popups
                // suppressed.
                let err_text = format!("{e:#}");
                let class = fono_core::critical_notify::classify(&err_text);
                if matches!(
                    class,
                    fono_core::critical_notify::ErrorClass::Auth
                        | fono_core::critical_notify::ErrorClass::PaymentRequired
                        | fono_core::critical_notify::ErrorClass::Network
                        | fono_core::critical_notify::ErrorClass::TermsRequired
                ) {
                    fono_core::critical_notify::notify(
                        fono_core::critical_notify::Stage::Assistant,
                        assistant.name(),
                        class,
                        &err_text,
                    );
                }
                if let Some(t) = &trace {
                    t.duration_between(
                        "llm.reply_stream_open",
                        "assistant.llm",
                        "llm",
                        llm_started,
                        std::time::Instant::now(),
                        json!({ "error": err_text, "provider": assistant.name() }),
                    );
                    t.finish(json!({
                        "aborted": true,
                        "reason": "llm_open_error",
                        "summary": t.cache_scoreboard(),
                    }));
                }
                return Err(e);
            }
        },
    };

    // 3b. Text-only turn: no TTS backend. Stream the reply to the
    //     overlay as on-screen text and hold it for a reading dwell
    //     instead of synthesising + playing audio
    let Some(tts) = tts else {
        return drive_text_only_reply(
            &state,
            deltas,
            overlay.as_ref(),
            &action_tx,
            &notify,
            &mut metrics,
            turn_started,
            llm_started,
            trace.as_ref(),
        )
        .await;
    };

    // 4. Lazily ensure a playback handle exists.
    {
        let playback_started = std::time::Instant::now();
        let mut s = state.lock().await;
        if s.playback.is_none() {
            match AudioPlayback::new(None) {
                Ok(pb) => s.playback = Some(pb),
                Err(e) => {
                    warn!(
                        target: "fono::assistant",
                        error = %e,
                        "audio playback init failed; assistant reply will not be audible"
                    );
                }
            }
        }
        if let Some(t) = &trace {
            t.duration_between(
                "playback.ensure",
                "assistant.playback",
                fono_core::turn_trace::PLAYBACK_LANE,
                playback_started,
                std::time::Instant::now(),
                json!({ "available": s.playback.is_some() }),
            );
        }
    }

    // 5. Pump deltas through the SentenceSplitter into TTS+playback.
    let mut splitter = SentenceSplitter::new();
    let mut full_reply = String::new();
    let mut any_audio = false;
    let mut last_audio_at: Option<std::time::Instant> = None;
    let mut synthesising_announced = false;
    // Fired once at the true first audio frame (mid-stream for streaming
    // backends; right after the first enqueue for batch ones): records the
    // honest TTFA and flips the FSM/overlay to SPEAKING.
    let first_audio = FirstAudio::new(llm_started, &action_tx, overlay.as_ref());
    // Tool events observed on the stream. Recorded into history
    // after the turn finishes so subsequent turns can echo the
    // tool-call exchange back to the model. None ⇒ no tool used.
    let mut tool_event_log: Vec<ToolEvent> = Vec::new();
    // Wall-clock timestamps for per-tool exec_ms. The `Called`
    // sentinel marks the start; the matching `Result` (by
    // tool_call_id) marks the end. Used to populate `metrics.tools`.
    let mut tool_started: HashMap<String, (String, std::time::Instant)> = HashMap::new();
    // Set true on cancel / stream-error so the final summary line
    // and history-rebuild can distinguish a clean turn from an
    // aborted one. `notify_triggered` always returns false (Tokio's
    // Notify has no non-await probe), so we track abort explicitly.
    let mut aborted_mid_stream = false;
    let mut delta_index: u64 = 0;
    let mut last_llm_delta_at: Option<std::time::Instant> = None;
    let mut llm_stream_done_at: Option<std::time::Instant> = None;

    // Voice routing hint for the local (and Cartesia) TTS backends: the
    // language the STT engine actually detected for this turn (falling back to
    // the configured hint), so a Romanian reply is spoken by the Romanian
    // voice rather than whichever voice the primary language resolved to.
    //
    // Normalise to an alpha-2 code here: some STT backends surface the full
    // English name Whisper emits ("romanian") rather than the code ("ro"),
    // and the local TTS engines only accept codes. This is the single point
    // every STT path converges on, so normalising once guards them all.
    let tts_lang: Option<String> =
        metrics.language.as_deref().map(fono_stt::lang::whisper_lang_to_code);

    loop {
        let next = tokio::select! {
            biased;
            () = notify.notified() => {
                debug!(target: "fono::assistant", "cancelled mid-stream");
                aborted_mid_stream = true;
                if let Some(t) = &trace {
                    t.instant(
                        "llm.stream_cancelled",
                        "assistant.llm",
                        "llm",
                        json!({ "delta_index": delta_index }),
                    );
                }
                break;
            }
            n = deltas.next() => n,
        };
        let Some(item) = next else {
            llm_stream_done_at = Some(std::time::Instant::now());
            break;
        };
        let delta = match item {
            Ok(d) => d,
            Err(e) => {
                warn!(target: "fono::assistant", error = %e, "assistant stream error");
                // Mid-stream failures usually mean the connection
                // dropped or the backend returned a typed error
                // after auth-handshake; surface once per session.
                let err_text = format!("{e:#}");
                let class = fono_core::critical_notify::classify(&err_text);
                if matches!(
                    class,
                    fono_core::critical_notify::ErrorClass::Auth
                        | fono_core::critical_notify::ErrorClass::PaymentRequired
                        | fono_core::critical_notify::ErrorClass::Network
                        | fono_core::critical_notify::ErrorClass::TermsRequired
                ) {
                    fono_core::critical_notify::notify(
                        fono_core::critical_notify::Stage::Assistant,
                        assistant.name(),
                        class,
                        &err_text,
                    );
                }
                if let Some(t) = &trace {
                    t.instant(
                        "llm.stream_error",
                        "assistant.llm",
                        "llm",
                        json!({ "delta_index": delta_index, "error": err_text }),
                    );
                }
                break;
            }
        };
        let delta_chars = delta.text.chars().count();
        last_llm_delta_at = Some(std::time::Instant::now());
        if let Some(t) = &trace {
            t.instant(
                "llm.delta",
                "assistant.llm",
                "llm",
                json!({
                    "index": delta_index,
                    "chars": delta_chars,
                    "cumulative_chars": full_reply.chars().count() + delta_chars,
                    "tool_event": delta.tool_event.is_some(),
                }),
            );
        }
        delta_index = delta_index.saturating_add(1);
        // First LLM delta — model is generating but the
        // `SentenceSplitter` is still buffering until a full
        // sentence emerges and the TTS HTTP roundtrip hasn't even
        // started yet, so the user will hear silence for a stretch.
        // Reflect that on the overlay (THINKING → SYNTHESISING) only;
        // the FSM stays in `AssistantThinking` because cancel /
        // barge-in / tray semantics treat this silent stretch as
        // part of "thinking".
        if !synthesising_announced {
            synthesising_announced = true;
            metrics.llm_ttfb_ms = llm_started.elapsed().as_millis() as u64;
            debug!(
                target: "fono::assistant",
                llm_ttfb_ms = metrics.llm_ttfb_ms,
                "first LLM delta — overlay: THINKING → SYNTHESISING"
            );
            if let Some(t) = &trace {
                t.instant(
                    "llm.first_delta",
                    "assistant.llm",
                    "llm",
                    json!({ "ttfb_ms": metrics.llm_ttfb_ms }),
                );
            }
            if let Some(o) = overlay.as_ref() {
                o.set_state(fono_overlay::OverlayState::AssistantSynthesising);
            }
        }
        // Tool sentinels carry no spoken text — record them and
        // skip the splitter / TTS path entirely. The assistant
        // client follows up with the real prose reply on the same
        // stream.
        if let Some(event) = delta.tool_event {
            match &event {
                ToolEvent::Called(call) => {
                    tool_started
                        .insert(call.id.clone(), (call.name.clone(), std::time::Instant::now()));
                }
                ToolEvent::Result { tool_call_id, summary } => {
                    if let Some((name, started_at)) = tool_started.remove(tool_call_id) {
                        let exec_ms = started_at.elapsed().as_millis() as u64;
                        let outcome = classify_tool_outcome(summary);
                        metrics.tools.push(AssistantToolMetric { name, exec_ms, outcome });
                    }
                }
            }
            debug!(target: "fono::assistant", ?event, "tool event recorded for history");
            tool_event_log.push(event);
            continue;
        }
        full_reply.push_str(&delta.text);
        let split_started = std::time::Instant::now();
        let sentences = splitter.push(&delta.text);
        if let Some(t) = &trace {
            t.duration_between(
                "splitter.push",
                "assistant.splitter",
                fono_core::turn_trace::SPLITTER_LANE,
                split_started,
                std::time::Instant::now(),
                json!({ "delta_chars": delta_chars, "sentences_ready": sentences.len() }),
            );
        }
        for sentence in sentences {
            let sentence_index = metrics.sentences.saturating_add(1);
            if let Some(t) = &trace {
                t.instant(
                    "splitter.sentence_ready",
                    "assistant.splitter",
                    fono_core::turn_trace::SPLITTER_LANE,
                    json!({ "sentence_index": sentence_index, "chars": sentence.chars().count() }),
                );
            }
            if synth_and_enqueue(
                &state,
                &tts,
                &sentence,
                tts_lang.as_deref(),
                sentence_index,
                &notify,
                &first_audio,
            )
            .await
            {
                any_audio = true;
                metrics.sentences = metrics.sentences.saturating_add(1);
                last_audio_at = Some(std::time::Instant::now());
                // Streaming backends already fired mid-stream (via the
                // `stream_utterance` callback) the moment the first frame
                // reached the device; batch backends fire here. Idempotent.
                first_audio.fire();
            }
            if notify_triggered(&notify) {
                break;
            }
        }
        if notify_triggered(&notify) {
            break;
        }
    }

    metrics.llm_total_ms = last_llm_delta_at.or(llm_stream_done_at).map_or_else(
        || llm_started.elapsed().as_millis() as u64,
        |at| at.duration_since(llm_started).as_millis() as u64,
    );
    if let Some(t) = &trace {
        let stream_ended = llm_stream_done_at.unwrap_or_else(std::time::Instant::now);
        t.duration_between(
            "llm.stream_drain",
            "assistant.llm",
            "llm",
            llm_started,
            stream_ended,
            json!({
                "deltas": delta_index,
                "reply_chars": full_reply.chars().count(),
                "llm_total_ms": metrics.llm_total_ms,
                "aborted": aborted_mid_stream,
            }),
        );
    }

    if !aborted_mid_stream {
        if let Some(tail) = splitter.flush() {
            let flush_started = std::time::Instant::now();
            let sentence_index = metrics.sentences.saturating_add(1);
            if let Some(t) = &trace {
                t.instant(
                    "splitter.flush_tail",
                    "assistant.splitter",
                    fono_core::turn_trace::SPLITTER_LANE,
                    json!({ "sentence_index": sentence_index, "chars": tail.chars().count() }),
                );
                t.duration_between(
                    "splitter.flush",
                    "assistant.splitter",
                    fono_core::turn_trace::SPLITTER_LANE,
                    flush_started,
                    std::time::Instant::now(),
                    json!({ "tail": true }),
                );
            }
            if synth_and_enqueue(
                &state,
                &tts,
                &tail,
                tts_lang.as_deref(),
                sentence_index,
                &notify,
                &first_audio,
            )
            .await
            {
                any_audio = true;
                metrics.sentences = metrics.sentences.saturating_add(1);
                last_audio_at = Some(std::time::Instant::now());
                // Belt-and-braces: a tail flush without any preceding delta is
                // impossible (the splitter is empty until `push`ed), but if a
                // future refactor ever ends up here we still want SPEAKING
                // flipped before audio plays. Idempotent.
                first_audio.fire();
            }
        }
    }

    // 6. Push the assistant turn(s) into history. When the model
    //    used a tool, the rolling log expands to three entries so
    //    subsequent turns can echo the canonical tool sequence back
    //    to the provider:
    //
    //      assistant (tool_calls=...) → tool (result summary) → assistant (text)
    //
    //    All three are appended atomically under a single lock so a
    //    cancelled turn cannot leave history half-rebuilt.
    {
        let mut s = state.lock().await;
        for event in tool_event_log {
            match event {
                ToolEvent::Called(call) => {
                    s.history.push_assistant_tool_calls(String::new(), vec![call]);
                }
                ToolEvent::Result { tool_call_id, summary } => {
                    s.history.push_tool_result(tool_call_id, summary);
                }
            }
        }
        if !full_reply.trim().is_empty() {
            s.history.push_assistant(full_reply.trim().to_string());
        }
    }

    metrics.tts_ttfa_ms = first_audio.ttfa_ms();
    metrics.reply_chars = full_reply.chars().count();
    metrics.aborted = aborted_mid_stream;
    // Total = STT start → last audio queued (drain not included).
    // When no audio was produced we fall back to LLM stream end.
    let total_anchor = last_audio_at.unwrap_or_else(std::time::Instant::now);
    metrics.total_ms = total_anchor.duration_since(turn_started).as_millis() as u64;
    info!(target: "fono::assistant", "{}", format_assistant_summary(&metrics));

    // Cooperative playback-drain wait. The LLM stream is done and
    // every sentence is enqueued, but the `AudioPlayback` worker may
    // still have several seconds of audio in its queue. Block here
    // until the queue actually drains so the caller's cleanup
    // closure (hide overlay, tray → Idle, emit `ProcessingDone`,
    // release the cancel hotkey grab) runs at the moment the user
    // actually stops hearing audio.
    //
    // The `notify` select-arm preserves Escape / barge-in: a cancel
    // press triggers `stop_current_turn` which calls `playback.stop()`
    // (drains the queue) AND `notify.notify_waiters()`, so either
    // path wakes this loop.
    //
    // 60 s belt-and-braces timeout in case `is_idle()` reports a
    // wedged playback handle — warn and fall through rather than
    // soft-locking the FSM in `AssistantSpeaking` forever.
    let playback_handle = {
        let s = state.lock().await;
        s.playback.clone()
    };
    if let Some(pb) = playback_handle {
        let drain_started = std::time::Instant::now();
        let mut drain_reason = "idle";
        if !pb.is_idle() {
            let timeout = std::time::Duration::from_secs(60);
            loop {
                tokio::select! {
                    biased;
                    () = notify.notified() => {
                        debug!(target: "fono::assistant", "drain-poll: cancelled");
                        drain_reason = "cancelled";
                        break;
                    }
                    () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                        if pb.is_idle() {
                            debug!(
                                target: "fono::assistant",
                                ms = drain_started.elapsed().as_millis() as u64,
                                "drain-poll: playback idle"
                            );
                            break;
                        }
                        if drain_started.elapsed() >= timeout {
                            warn!(
                                target: "fono::assistant",
                                "drain-poll exceeded 60 s; forcing ProcessingDone (wedged playback handle?)"
                            );
                            drain_reason = "timeout";
                            break;
                        }
                    }
                }
            }
        }
        // Always record the drain span (zero-width when playback was already
        // idle) so the post-TTS tail between last audio and turn end is never
        // unexplained whitespace on the timeline.
        if let Some(t) = &trace {
            t.duration_between(
                "playback.drain",
                "assistant.playback",
                fono_core::turn_trace::PLAYBACK_LANE,
                drain_started,
                std::time::Instant::now(),
                json!({ "reason": drain_reason }),
            );
        }
    }
    // Tell the Glass Cortex replay engine the audio is over (covers
    // idle, drained, cancelled and timed-out paths alike) so trailing
    // animation wraps up instead of ghost-playing over silence.
    if let Some(o) = &overlay {
        o.push_cortex(fono_overlay::CortexCmd::PlaybackDone);
    }
    if let Some(t) = &trace {
        t.finish(json!({
            "aborted": metrics.aborted,
            "played_audio": any_audio,
            "total_ms": metrics.total_ms,
            "llm_ttfb_ms": metrics.llm_ttfb_ms,
            "llm_total_ms": metrics.llm_total_ms,
            "tts_ttfa_ms": metrics.tts_ttfa_ms,
            "sentences": metrics.sentences,
            "reply_chars": metrics.reply_chars,
            "summary": t.cache_scoreboard(),
        }));
    }
    Ok(any_audio)
}

/// Upper bound on the character count fed to [`read_dwell`] when sizing
/// the post-stream reading *hold*. The reply already scrolled past while
/// streaming, so the hold only needs to cover the last screenful the
/// reader is left looking at (~one panel of wrapped text ≈ 8 lines).
/// Capping here keeps a very long reply from holding its final two
/// lines on screen for the full [`read_dwell`] minute.
const READING_HOLD_CHARS: usize = 320;

/// Reading-time dwell for a text-only reply of `reply_chars` characters.
/// Deliberately slow (~130 wpm, vs the ~200–250 wpm of a fluent reader)
/// so the panel errs on staying up a little too long — Escape is the
/// user's override, and cutting a reply off mid-read is the worse
/// failure. Floored so even a one-word reply is legible, and capped so
/// the overlay can never wedge on screen forever.
fn read_dwell(reply_chars: usize) -> std::time::Duration {
    const WPM: f32 = 130.0;
    // Average English word ≈ 5 letters + 1 separating space.
    const CHARS_PER_WORD: f32 = 6.0;
    const FLOOR: std::time::Duration = std::time::Duration::from_secs(3);
    const CAP: std::time::Duration = std::time::Duration::from_secs(60);
    let words = reply_chars as f32 / CHARS_PER_WORD;
    let secs = words / WPM * 60.0;
    std::time::Duration::from_secs_f32(secs.max(0.0)).clamp(FLOOR, CAP)
}

/// Text-only reply pump consume the already-opened LLM
/// delta stream, stream the growing reply text to the overlay
/// (`AssistantReading`), record tool events + the reply into history,
/// then hold the panel for a reading-time dwell
#[allow(clippy::too_many_arguments, clippy::too_many_lines, clippy::cognitive_complexity)]
async fn drive_text_only_reply(
    state: &Arc<Mutex<AssistantSessionState>>,
    mut deltas: futures::stream::BoxStream<'static, Result<fono_assistant::TokenDelta>>,
    overlay: Option<&fono_overlay::OverlayHandle>,
    action_tx: &mpsc::UnboundedSender<HotkeyAction>,
    notify: &Arc<Notify>,
    metrics: &mut AssistantTurnMetrics,
    turn_started: std::time::Instant,
    llm_started: std::time::Instant,
    trace: Option<&TurnTrace>,
) -> Result<bool> {
    let mut full_reply = String::new();
    let mut tool_event_log: Vec<ToolEvent> = Vec::new();
    let mut tool_started: HashMap<String, (String, std::time::Instant)> = HashMap::new();
    let mut aborted_mid_stream = false;
    let mut reading_announced = false;

    loop {
        let next = tokio::select! {
            biased;
            () = notify.notified() => {
                debug!(target: "fono::assistant", "text-only: cancelled mid-stream");
                aborted_mid_stream = true;
                break;
            }
            n = deltas.next() => n,
        };
        let Some(item) = next else { break };
        let delta = match item {
            Ok(d) => d,
            Err(e) => {
                warn!(target: "fono::assistant", error = %e, "text-only: assistant stream error");
                let err_text = format!("{e:#}");
                let class = fono_core::critical_notify::classify(&err_text);
                if matches!(
                    class,
                    fono_core::critical_notify::ErrorClass::Auth
                        | fono_core::critical_notify::ErrorClass::PaymentRequired
                        | fono_core::critical_notify::ErrorClass::Network
                        | fono_core::critical_notify::ErrorClass::TermsRequired
                ) {
                    fono_core::critical_notify::notify(
                        fono_core::critical_notify::Stage::Assistant,
                        "assistant",
                        class,
                        &err_text,
                    );
                }
                break;
            }
        };
        // Tool sentinels carry no spoken/visible prose — record them for
        // history and skip the text panel.
        if let Some(event) = delta.tool_event {
            match &event {
                ToolEvent::Called(call) => {
                    tool_started
                        .insert(call.id.clone(), (call.name.clone(), std::time::Instant::now()));
                }
                ToolEvent::Result { tool_call_id, summary } => {
                    if let Some((name, started_at)) = tool_started.remove(tool_call_id) {
                        let exec_ms = started_at.elapsed().as_millis() as u64;
                        let outcome = classify_tool_outcome(summary);
                        metrics.tools.push(AssistantToolMetric { name, exec_ms, outcome });
                    }
                }
            }
            tool_event_log.push(event);
            continue;
        }
        if delta.text.is_empty() {
            continue;
        }
        // First visible token: flip the overlay to the reading panel and
        // move the FSM Thinking → Speaking so Escape dismisses cleanly.
        if !reading_announced {
            reading_announced = true;
            metrics.llm_ttfb_ms = llm_started.elapsed().as_millis() as u64;
            let _ = action_tx.send(HotkeyAction::AssistantSpeakingStarted);
            if let Some(o) = overlay {
                o.set_state(fono_overlay::OverlayState::AssistantReading);
            }
        }
        full_reply.push_str(&delta.text);
        if let Some(o) = overlay {
            o.update_text(full_reply.clone());
        }
    }

    metrics.llm_total_ms = llm_started.elapsed().as_millis() as u64;

    // Push tool exchange + the reply into history under a single lock.
    {
        let mut s = state.lock().await;
        for event in tool_event_log {
            match event {
                ToolEvent::Called(call) => {
                    s.history.push_assistant_tool_calls(String::new(), vec![call]);
                }
                ToolEvent::Result { tool_call_id, summary } => {
                    s.history.push_tool_result(tool_call_id, summary);
                }
            }
        }
        if !full_reply.trim().is_empty() {
            s.history.push_assistant(full_reply.trim().to_string());
        }
    }

    metrics.reply_chars = full_reply.chars().count();
    metrics.aborted = aborted_mid_stream;
    metrics.tts_ttfa_ms = None;
    metrics.total_ms = turn_started.elapsed().as_millis() as u64;
    info!(target: "fono::assistant", "{}", format_assistant_summary(metrics));

    // Reading hold: the reply already scrolled past while it streamed
    // (the panel tail-follows the newest line as tokens arrive), so we
    // hold the final screenful on-screen long enough to finish reading
    // it, then dismiss. The hold is sized to a screenful, not the whole
    // reply — a long answer isn't pinned to its last two lines for a
    // full minute. Escape / barge-in (`notify`) ends it early.
    if !aborted_mid_stream && !full_reply.trim().is_empty() {
        let hold = read_dwell(metrics.reply_chars.min(READING_HOLD_CHARS));
        tokio::select! {
            biased;
            () = notify.notified() => {
                debug!(target: "fono::assistant", "text-only: reading hold cancelled");
            }
            () = tokio::time::sleep(hold) => {}
        }
    }

    if let Some(t) = trace {
        t.finish(json!({
            "aborted": aborted_mid_stream,
            "played_audio": false,
            "mode": "text_only",
            "total_ms": metrics.total_ms,
            "llm_ttfb_ms": metrics.llm_ttfb_ms,
            "llm_total_ms": metrics.llm_total_ms,
            "reply_chars": metrics.reply_chars,
            "summary": t.cache_scoreboard(),
        }));
    }
    Ok(false)
}

/// Per-turn first-audio signal. [`Self::fire`] is invoked at the *true* moment
/// the first PCM frame reaches the device — mid-stream for streaming TTS
/// backends (via the `stream_utterance` callback), or right after the first
/// enqueue for batch backends. It is idempotent (fires once per turn), records
/// the time-to-first-audio relative to LLM start, and flips the FSM + overlay
/// to SPEAKING. Borrows `action_tx`/`overlay` for the whole turn.
struct FirstAudio<'a> {
    llm_started: std::time::Instant,
    action_tx: &'a mpsc::UnboundedSender<HotkeyAction>,
    overlay: Option<&'a fono_overlay::OverlayHandle>,
    fired: AtomicBool,
    ttfa_cell: AtomicU64,
    /// Cumulative enqueued reply-audio milliseconds (batch TTS path).
    /// Each addition is forwarded to the Glass Cortex replay engine as
    /// `CortexCmd::AudioTotal` so the brain animation is paced to the
    /// real playback duration. Streaming TTS skips this — the engine
    /// then estimates the timeline from token count.
    audio_ms: AtomicU64,
}

impl<'a> FirstAudio<'a> {
    fn new(
        llm_started: std::time::Instant,
        action_tx: &'a mpsc::UnboundedSender<HotkeyAction>,
        overlay: Option<&'a fono_overlay::OverlayHandle>,
    ) -> Self {
        Self {
            llm_started,
            action_tx,
            overlay,
            fired: AtomicBool::new(false),
            ttfa_cell: AtomicU64::new(u64::MAX),
            audio_ms: AtomicU64::new(0),
        }
    }

    /// Fire once. Safe to call from any sentence / either path; only the first
    /// call records TTFA and announces SPEAKING.
    fn fire(&self) {
        if !self.fired.swap(true, Ordering::SeqCst) {
            let ttfa = self.llm_started.elapsed().as_millis() as u64;
            self.ttfa_cell.store(ttfa, Ordering::SeqCst);
            debug!(target: "fono::assistant", ttfa_ms = ttfa, "first audio");
            let _ = self.action_tx.send(HotkeyAction::AssistantSpeakingStarted);
            if let Some(o) = self.overlay {
                o.set_state(fono_overlay::OverlayState::AssistantSpeaking);
            }
        }
    }

    /// Recorded time-to-first-audio in ms, or `None` if no audio ever played.
    fn ttfa_ms(&self) -> Option<u64> {
        match self.ttfa_cell.load(Ordering::SeqCst) {
            u64::MAX => None,
            v => Some(v),
        }
    }

    /// Account one enqueued audio chunk and push the running total to
    /// the Glass Cortex replay engine (sizes its playback timeline).
    fn add_audio(&self, samples: usize, sample_rate: u32) {
        if sample_rate == 0 || samples == 0 {
            return;
        }
        let ms = samples as u64 * 1000 / u64::from(sample_rate);
        let total = self.audio_ms.fetch_add(ms, Ordering::SeqCst) + ms;
        if let Some(o) = self.overlay {
            o.push_cortex(fono_overlay::CortexCmd::AudioTotal { secs: total as f32 / 1000.0 });
        }
    }

    /// Reply-audio seconds enqueued so far — the timeline offset the
    /// next chunk's spectrum windows are stamped against.
    fn audio_secs_so_far(&self) -> f32 {
        self.audio_ms.load(Ordering::SeqCst) as f32 / 1000.0
    }

    /// Tap the REAL synthesised TTS PCM (plan Task E1): compute a few
    /// frequency bands + amplitude per short window of the audio we
    /// already synthesised, and push them to the overlay as
    /// `CortexCmd::AudioBands` stamped on the reply's audio timeline.
    /// The Glass Cortex speaking scene samples these against its
    /// playback clock to modulate its per-row voice glow from the
    /// genuine spoken spectrum. No model cost — a handful of Goertzel
    /// filters over ~50 ms windows of PCM we already own. `start_secs`
    /// is this chunk's offset within the reply. This replaces the
    /// synthetic FFT frames that used to decorate the speaking scene:
    /// the modulation is now the real voice. Honesty invariant: this
    /// only feeds brightness modulation; the underlying cell value is
    /// still a real weight/activation (see `draw_lattice_scene`).
    fn push_audio_bands(&self, pcm: &[f32], sample_rate: u32, start_secs: f32) {
        let Some(o) = self.overlay else { return };
        tts_audio_band_windows(pcm, sample_rate, start_secs, |at_secs, bands, amp| {
            o.push_cortex(fono_overlay::CortexCmd::AudioBands { at_secs, bands, amp });
        });
    }
}

/// Representative band centre frequencies (Hz) for the real-TTS
/// spectrum tap — log-spaced across the speech range. Kept small so
/// the tap stays cheap and the row lattice reads as a coarse but
/// honest spectrum.
const TTS_BAND_HZ: [f32; 8] = [120.0, 250.0, 450.0, 800.0, 1400.0, 2400.0, 4000.0, 6500.0];

/// Goertzel single-frequency magnitude (0..) of `samples` at `freq`.
/// One multiply-add per sample per band — cheaper than a full FFT for
/// the handful of bands we visualise.
#[allow(clippy::suboptimal_flops)] // keep the recurrence readable as textbook Goertzel
fn goertzel_mag(samples: &[f32], sample_rate: u32, freq: f32) -> f32 {
    let n = samples.len();
    if n == 0 || sample_rate == 0 {
        return 0.0;
    }
    let k = (freq * n as f32 / sample_rate as f32).round();
    let w = 2.0 * std::f32::consts::PI * k / n as f32;
    let coeff = 2.0 * w.cos();
    let mut s1 = 0.0f32;
    let mut s2 = 0.0f32;
    for &x in samples {
        let s0 = x + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = coeff.mul_add(-s1 * s2, s1 * s1 + s2 * s2).max(0.0);
    power.sqrt() / n as f32
}

/// Slice `pcm` into ~50 ms windows and hand each window's normalised
/// band shape + amplitude to `emit(at_secs, bands, amp)`. The band
/// shape is normalised to the window's own peak (so quiet windows
/// still show structure) then scaled by loudness, and amplitude is a
/// gained RMS — so silence stays dark and speech dances. Honest: the
/// numbers come straight from the synthesised PCM.
fn tts_audio_band_windows<F: FnMut(f32, Vec<f32>, f32)>(
    pcm: &[f32],
    sample_rate: u32,
    start_secs: f32,
    mut emit: F,
) {
    if sample_rate == 0 || pcm.is_empty() {
        return;
    }
    // ~50 ms windows ⇒ ~20 spectrum frames/second, matching the
    // overlay animation cadence.
    let win = (sample_rate as usize / 20).max(256);
    let mut off = 0usize;
    while off < pcm.len() {
        let end = (off + win).min(pcm.len());
        let w = &pcm[off..end];
        // Gained RMS amplitude (speech RMS ~0.05..0.2 ⇒ ×6 fills the
        // 0..1 range for typical loud passages, clamped).
        let sum_sq: f32 = w.iter().map(|&x| x * x).sum();
        let rms = (sum_sq / w.len() as f32).sqrt();
        let amp = (rms * 6.0).clamp(0.0, 1.0);
        let mut bands = [0.0f32; TTS_BAND_HZ.len()];
        let mut peak = 0.0f32;
        for (b, &f) in bands.iter_mut().zip(TTS_BAND_HZ.iter()) {
            *b = goertzel_mag(w, sample_rate, f);
            peak = peak.max(*b);
        }
        // Shape from the spectrum, magnitude from loudness: divide by
        // the window peak then scale by amplitude so pauses go dark.
        if peak > 1e-9 {
            for b in &mut bands {
                *b = (*b / peak) * amp;
            }
        }
        let at_secs = start_secs + off as f32 / sample_rate as f32;
        emit(at_secs, bands.to_vec(), amp);
        off += win;
    }
}

/// Single-sentence helper. Synthesises and enqueues into the active
/// playback handle. Returns `true` on success.
#[allow(clippy::too_many_lines)]
async fn synth_and_enqueue(
    state: &Arc<Mutex<AssistantSessionState>>,
    tts: &Arc<dyn TextToSpeech>,
    sentence: &str,
    lang: Option<&str>,
    sentence_index: u32,
    notify: &Arc<Notify>,
    first_audio: &FirstAudio<'_>,
) -> bool {
    if sentence.trim().is_empty() {
        return false;
    }
    // Streaming-capable cloud backends play each sentence as a gapless
    // intra-utterance session, cutting time-to-first-audio. Batch/local
    // backends fall through to the synthesize + enqueue path below.
    if tts.supports_streaming() {
        return synth_and_stream(state, tts, sentence, lang, sentence_index, notify, first_audio)
            .await;
    }
    let synth_started = std::time::Instant::now();
    let audio = tokio::select! {
        biased;
        () = notify.notified() => {
            debug!(target: "fono::assistant", "cancelled before TTS synth");
            if let Some(t) = TurnTrace::current() {
                t.instant(
                    "tts.synthesize_cancelled",
                    "assistant.tts",
                    fono_core::turn_trace::TTS_LANE,
                    json!({ "sentence_index": sentence_index }),
                );
            }
            return false;
        }
        r = tts.synthesize(sentence, None, lang) => match r {
            Ok(a) => {
                if let Some(t) = TurnTrace::current() {
                    t.duration_between(
                        "tts.synthesize",
                        "assistant.tts",
                        "tts",
                        synth_started,
                        std::time::Instant::now(),
                        json!({
                            "sentence_index": sentence_index,
                            "provider": tts.name(),
                            "chars": sentence.chars().count(),
                            "sample_rate": a.sample_rate,
                            "samples": a.pcm.len(),
                        }),
                    );
                }
                a
            }
            Err(e) => {
                warn!(target: "fono::assistant", error = %e, "TTS synth failed");
                // Surface auth/network failures once per session.
                // Other (transient) errors stay silent — a flaky
                // 5xx mid-reply is best handled by retry, not a
                // popup. Global cascade cap keeps this quiet if
                // the assistant or STT stages already notified.
                let err_text = format!("{e:#}");
                let class = fono_core::critical_notify::classify(&err_text);
                if matches!(
                    class,
                    fono_core::critical_notify::ErrorClass::Auth
                        | fono_core::critical_notify::ErrorClass::PaymentRequired
                        | fono_core::critical_notify::ErrorClass::Network
                        | fono_core::critical_notify::ErrorClass::TermsRequired
                ) {
                    fono_core::critical_notify::notify(
                        fono_core::critical_notify::Stage::Tts,
                        tts.name(),
                        class,
                        &err_text,
                    );
                } else if matches!(class, fono_core::critical_notify::ErrorClass::RateLimit) {
                    // Cloud TTS 429s (e.g. Groq Orpheus TPD cap) — surface
                    // via the shared rate-limit notifier so the user sees
                    // exactly one popup per session even if the assistant
                    // emits multiple sentences. critical_notify itself is
                    // a no-op for RateLimit by design (see its module
                    // docs), so we route through rate_limit_notify here.
                    //
                    // The raw error text includes the request id, JSON
                    // envelope, and upsell copy — far too long for a
                    // desktop notification. Extract just the human
                    // `message` field when present.
                    let body = extract_json_message(&err_text)
                        .unwrap_or_else(|| truncate(&err_text, 240).to_string());
                    fono_stt::rate_limit_notify::notify_once(tts.name(), &body);
                }
                if let Some(t) = TurnTrace::current() {
                    t.duration_between(
                        "tts.synthesize",
                        "assistant.tts",
                        "tts",
                        synth_started,
                        std::time::Instant::now(),
                        json!({
                            "sentence_index": sentence_index,
                            "provider": tts.name(),
                            "chars": sentence.chars().count(),
                            "error": err_text,
                        }),
                    );
                }
                return false;
            }
        },
    };
    debug!(
        target: "fono::assistant",
        provider = tts.name(),
        ms = synth_started.elapsed().as_millis() as u64,
        rate = audio.sample_rate,
        samples = audio.pcm.len(),
        "synth ok"
    );
    // Clone the playback handle out so we can release the state lock
    // before the (potentially blocking) enqueue.
    let pb_handle = {
        let s = state.lock().await;
        s.playback.clone()
    };
    let Some(pb) = pb_handle else {
        return false;
    };
    let enqueue_started = std::time::Instant::now();
    let bands_start = first_audio.audio_secs_so_far();
    first_audio.add_audio(audio.pcm.len(), audio.sample_rate);
    // Real-voice spectrum tap for the Glass Cortex speaking scene
    // (plan Task E1) — computed from the PCM before it is moved into
    // the playback queue.
    first_audio.push_audio_bands(&audio.pcm, audio.sample_rate, bands_start);
    if let Err(e) = pb.enqueue(audio.pcm, audio.sample_rate) {
        warn!(target: "fono::assistant", error = %e, "enqueue failed");
        if let Some(t) = TurnTrace::current() {
            t.duration_between(
                "audio.enqueue",
                "assistant.playback",
                fono_core::turn_trace::PLAYBACK_LANE,
                enqueue_started,
                std::time::Instant::now(),
                json!({ "sentence_index": sentence_index, "error": e.to_string() }),
            );
        }
        return false;
    }
    if let Some(t) = TurnTrace::current() {
        t.duration_between(
            "audio.enqueue",
            "assistant.playback",
            fono_core::turn_trace::PLAYBACK_LANE,
            enqueue_started,
            std::time::Instant::now(),
            json!({ "sentence_index": sentence_index }),
        );
    }
    true
}

/// Streaming counterpart of [`synth_and_enqueue`] for backends that report
/// `supports_streaming()`. Drives one sentence through
/// [`fono_tts::stream_utterance`] into a [`LocalPlaybackSink`], so the first
/// audio plays before the whole sentence is synthesised. Cancellation
/// (Escape / barge-in) is handled by racing the stream against `notify`; on
/// cancel the sink is aborted (which stops the playback worker). Returns `true`
/// if any audio was produced.
async fn synth_and_stream(
    state: &Arc<Mutex<AssistantSessionState>>,
    tts: &Arc<dyn TextToSpeech>,
    sentence: &str,
    lang: Option<&str>,
    sentence_index: u32,
    notify: &Arc<Notify>,
    first_audio: &FirstAudio<'_>,
) -> bool {
    use fono_tts::PcmSink as _;

    let pb_handle = {
        let s = state.lock().await;
        s.playback.clone()
    };
    let Some(pb) = pb_handle else {
        return false;
    };
    let synth_started = std::time::Instant::now();
    let mut sink = fono_audio::LocalPlaybackSink::new(pb);
    let result = tokio::select! {
        biased;
        () = notify.notified() => {
            debug!(target: "fono::assistant", "cancelled during TTS stream");
            let _ = sink.abort().await;
            return false;
        }
        r = fono_tts::stream_utterance(
            tts.as_ref(),
            sentence,
            None,
            lang,
            &mut sink,
            || first_audio.fire(),
        ) => r,
    };
    match result {
        Ok(any_audio) => {
            if let Some(t) = TurnTrace::current() {
                t.duration_between(
                    "tts.synthesize_stream",
                    "assistant.tts",
                    fono_core::turn_trace::TTS_LANE,
                    synth_started,
                    std::time::Instant::now(),
                    json!({
                        "sentence_index": sentence_index,
                        "provider": tts.name(),
                        "chars": sentence.chars().count(),
                        "streamed": true,
                        "produced_audio": any_audio,
                    }),
                );
            }
            any_audio
        }
        Err(e) => {
            warn!(target: "fono::assistant", error = %e, "TTS stream failed");
            let err_text = format!("{e:#}");
            let class = fono_core::critical_notify::classify(&err_text);
            if matches!(
                class,
                fono_core::critical_notify::ErrorClass::Auth
                    | fono_core::critical_notify::ErrorClass::PaymentRequired
                    | fono_core::critical_notify::ErrorClass::Network
                    | fono_core::critical_notify::ErrorClass::TermsRequired
            ) {
                fono_core::critical_notify::notify(
                    fono_core::critical_notify::Stage::Tts,
                    tts.name(),
                    class,
                    &err_text,
                );
            } else if matches!(class, fono_core::critical_notify::ErrorClass::RateLimit) {
                let body = extract_json_message(&err_text)
                    .unwrap_or_else(|| truncate(&err_text, 240).to_string());
                fono_stt::rate_limit_notify::notify_once(tts.name(), &body);
            }
            false
        }
    }
}

/// Resample mono `f32` PCM from `from` Hz to `to` Hz via linear
/// interpolation. Returns the input untouched when the rates match or
/// either is zero. Cheap and good enough for speech — the realtime mic
/// path only ever down/up-samples by small integer-ish ratios (e.g.
/// 16 kHz capture → 16 kHz model, or 48 kHz → 16 kHz).
#[cfg(feature = "realtime")]
fn resample_linear(pcm: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == 0 || to == 0 || from == to || pcm.is_empty() {
        return pcm.to_vec();
    }
    let ratio = f64::from(to) / f64::from(from);
    let out_len = ((pcm.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = (i as f64) / ratio;
        let idx = src.floor() as usize;
        let frac = src - src.floor();
        let a = pcm.get(idx).copied().unwrap_or(0.0);
        let b = pcm.get(idx + 1).copied().unwrap_or(a);
        out.push((b - a).mul_add(frac as f32, a));
    }
    out
}

/// Open a realtime session, classifying + notifying on failure and
/// recording the `realtime.open` span on the trace either way. Keeps
/// `run_realtime_turn` under the line limit.
#[cfg(feature = "realtime")]
async fn open_realtime_or_notify(
    realtime: &dyn RealtimeAssistant,
    ctx: &AssistantContext,
    mode: RealtimeMode,
    trace: Option<&TurnTrace>,
) -> Result<RealtimeSession> {
    let open_started = std::time::Instant::now();
    match realtime.open_session(ctx, mode).await {
        Ok(s) => {
            if let Some(t) = trace {
                t.duration_between(
                    "realtime.open",
                    "assistant",
                    "assistant-pump",
                    open_started,
                    std::time::Instant::now(),
                    json!({ "provider": realtime.name() }),
                );
            }
            Ok(s)
        }
        Err(e) => {
            let err_text = format!("{e:#}");
            let class = fono_core::critical_notify::classify(&err_text);
            if matches!(
                class,
                fono_core::critical_notify::ErrorClass::Auth
                    | fono_core::critical_notify::ErrorClass::PaymentRequired
                    | fono_core::critical_notify::ErrorClass::Network
                    | fono_core::critical_notify::ErrorClass::TermsRequired
            ) {
                fono_core::critical_notify::notify(
                    fono_core::critical_notify::Stage::Assistant,
                    realtime.name(),
                    class,
                    &err_text,
                );
            }
            if let Some(t) = trace {
                t.duration_between(
                    "realtime.open",
                    "assistant",
                    "assistant-pump",
                    open_started,
                    std::time::Instant::now(),
                    json!({ "error": err_text }),
                );
                t.finish(json!({ "aborted": true, "reason": "realtime_open_error" }));
            }
            Err(e)
        }
    }
}

/// Forward captured mic frames into the live session as they arrive:
/// pull each frame from `frames`, resample it to the model's native
/// rate (identity for Gemini's 16 kHz), push it into `audio_in`, and
/// drop `audio_in` when the stream closes — the client turns that into
/// an `audioStreamEnd` (end-of-utterance). Works identically for the
/// buffered adapter (all frames already queued) and live capture
/// (frames trickle in during the F8 hold). Records streaming markers
/// on the `capture` lane so a trace waterfall shows upload overlapping
/// the hold.
#[cfg(feature = "realtime")]
async fn forward_mic_stream(
    audio_in: mpsc::Sender<Vec<f32>>,
    first: Vec<f32>,
    mut frames: mpsc::UnboundedReceiver<Vec<f32>>,
    sample_rate: u32,
    native: u32,
    trace: Option<&TurnTrace>,
) {
    let mut total = 0usize;
    let mut first_sent = false;
    // `first` is the already-peeked leading frame (the caller used it to
    // skip empty utterances); process it before draining the rest.
    let mut next = Some(first);
    loop {
        let frame = match next.take() {
            Some(f) => f,
            None => match frames.recv().await {
                Some(f) => f,
                None => break,
            },
        };
        if frame.is_empty() {
            continue;
        }
        let resampled = resample_linear(&frame, sample_rate, native);
        total += resampled.len();
        if !first_sent {
            first_sent = true;
            if let Some(t) = trace {
                t.instant(
                    "realtime.first_frame_sent",
                    "capture",
                    fono_core::turn_trace::CAPTURE_LANE,
                    json!({ "sample_rate": native }),
                );
            }
        }
        if audio_in.send(resampled).await.is_err() {
            break;
        }
    }
    drop(audio_in);
    if let Some(t) = trace {
        t.instant(
            "realtime.input_closed",
            "capture",
            fono_core::turn_trace::CAPTURE_LANE,
            json!({ "samples": total, "sample_rate": native }),
        );
    }
}

/// Pull the first non-empty frame from a mic stream, returning it plus
/// the (still-open) receiver for the remaining frames. Returns `None`
/// if the stream closes before any audio arrives — the orchestrator
/// treats that as an empty utterance and skips the turn (preserving the
/// old `pcm.is_empty()` guard). Frames captured while we await keep
/// queuing in the unbounded channel, so nothing is lost.
#[cfg(feature = "realtime")]
async fn peek_first_frame(
    mut frames: mpsc::UnboundedReceiver<Vec<f32>>,
) -> Option<(Vec<f32>, mpsc::UnboundedReceiver<Vec<f32>>)> {
    while let Some(f) = frames.recv().await {
        if !f.is_empty() {
            return Some((f, frames));
        }
    }
    None
}

/// Adapt a finished `Vec<f32>` (the record-then-send path) into the
/// same frame stream the live path uses: chunk the clip into ~50 ms
/// frames at the capture rate and push them through an unbounded
/// channel, then drop the sender to close the stream. This keeps the
/// buffered turn behaving byte-for-byte as before (a degenerate
/// stream) and is the safe fallback when live capture-on-press is
/// unavailable. Synchronous (no spawn / no runtime needed) thanks to
/// the unbounded channel, so it is unit-testable.
#[cfg(feature = "realtime")]
pub fn buffered_frame_stream(pcm: &[f32], sample_rate: u32) -> mpsc::UnboundedReceiver<Vec<f32>> {
    let (tx, rx) = mpsc::unbounded_channel();
    let frame = (sample_rate as usize / 20).max(1); // ~50 ms chunks
    for chunk in pcm.chunks(frame) {
        if tx.send(chunk.to_vec()).is_err() {
            break;
        }
    }
    // tx dropped here → the stream closes once all frames are drained.
    rx
}

/// One assistant turn over the realtime / speech-to-speech path
/// (Gemini Live). Opens a single WebSocket session, streams the
/// captured mic PCM in (then closes input), and plays the model's
/// reply audio back as one continuous gapless stream — which is what
/// fixes the staged path's per-sentence voice drift and ~6 s/sentence
/// batch-TTS latency. Cancellable via `notify`. Returns `Ok(true)` if
/// any reply audio played.
#[cfg(feature = "realtime")]
pub async fn run_realtime_turn(
    state: Arc<Mutex<AssistantSessionState>>,
    inputs: RealtimeTurnInputs,
    notify: Arc<Notify>,
) -> Result<bool> {
    let RealtimeTurnInputs {
        frames,
        sample_rate,
        realtime,
        system_prompt,
        language,
        action_tx,
        overlay,
        prefer_vision,
        screen_capture_fn,
        active_window_context,
    } = inputs;

    let trace = TurnTrace::start_from_env();
    let _trace_guard = trace.as_ref().map(TurnTrace::make_current);
    if let Some(t) = &trace {
        t.instant(
            "turn.start",
            "assistant",
            "assistant-pump",
            json!({ "turn_id": t.id(), "mode": "realtime" }),
        );
    }
    let turn_started = std::time::Instant::now();
    let mut metrics = AssistantTurnMetrics { language: language.clone(), ..Default::default() };

    // Wait for the first mic frame before doing any work. If the stream
    // closes with no audio (instantaneous press/release), skip the turn
    // without opening a session — preserving the old empty-PCM guard.
    // Frames captured during this await keep queuing in the unbounded
    // channel, so live streaming loses nothing.
    let Some((first_frame, frames)) = peek_first_frame(frames).await else {
        debug!(target: "fono::assistant", "realtime skip: empty mic stream");
        if let Some(t) = &trace {
            t.finish(json!({ "aborted": true, "reason": "empty_pcm" }));
        }
        return Ok(false);
    };

    // Seed the session with the completed history (the current user
    // turn is transcribed by the model and pushed afterwards).
    let history_snapshot = {
        let mut s = state.lock().await;
        s.history.snapshot()
    };
    let ctx = AssistantContext {
        system_prompt,
        language: language.clone(),
        history: history_snapshot,
        active_window_context,
        screen_capture: screen_capture_fn,
        prefer_vision,
        max_new_tokens: None,
        // Local hotkey-triggered turn: this is the one the on-screen
        // overlay is meant to visualize, so allow the brain-capture tap.
        allow_brain_capture: true,
    };

    // Open the live session.
    let RealtimeSession { audio_in, mut events } =
        open_realtime_or_notify(realtime.as_ref(), &ctx, RealtimeMode::PushToTalk, trace.as_ref())
            .await?;

    // Lazily ensure playback exists (mirrors the staged path).
    {
        let mut s = state.lock().await;
        if s.playback.is_none() {
            match AudioPlayback::new(None) {
                Ok(pb) => s.playback = Some(pb),
                Err(e) => warn!(
                    target: "fono::assistant",
                    error = %e,
                    "audio playback init failed; realtime reply will not be audible"
                ),
            }
        }
    }

    // Stream the captured mic audio in (frame-by-frame), then close
    // input. The buffered adapter feeds all frames at once; live
    // capture trickles them during the F8 hold — same code path.
    let native = realtime.native_input_rate();
    forward_mic_stream(audio_in, first_frame, frames, sample_rate, native, trace.as_ref()).await;

    // Drive the reply: play audio, accumulate transcripts.
    let first_audio = FirstAudio::new(turn_started, &action_tx, overlay.as_ref());
    let reply = drive_realtime_reply(&state, &mut events, &notify, &first_audio).await;

    // Record both turns under a single lock.
    {
        let mut s = state.lock().await;
        if let Some(u) = reply.user_text.as_ref().map(|u| u.trim()).filter(|u| !u.is_empty()) {
            s.history.push_user(u.to_string());
        }
        if !reply.reply_text.trim().is_empty() {
            s.history.push_assistant(reply.reply_text.trim().to_string());
        }
    }

    metrics.user_chars = reply.user_text.as_deref().map_or(0, |u| u.trim().chars().count());
    metrics.reply_chars = reply.reply_text.trim().chars().count();
    metrics.tts_ttfa_ms = first_audio.ttfa_ms();
    metrics.aborted = reply.aborted;
    let total_anchor = reply.last_audio_at.unwrap_or_else(std::time::Instant::now);
    metrics.total_ms = total_anchor.duration_since(turn_started).as_millis() as u64;
    info!(target: "fono::assistant", "{}", format_assistant_summary(&metrics));

    if let Some(t) = &trace {
        t.finish(json!({
            "aborted": reply.aborted,
            "played_audio": reply.any_audio,
            "total_ms": metrics.total_ms,
            "tts_ttfa_ms": metrics.tts_ttfa_ms,
            "reply_chars": metrics.reply_chars,
            "mode": "realtime",
        }));
    }
    Ok(reply.any_audio)
}

/// Consume the realtime event stream: play reply audio as one gapless
/// session, accumulate the reply + user transcripts, and honour
/// cancellation (Escape / barge-in). Returns what the reply produced.
#[cfg(feature = "realtime")]
async fn drive_realtime_reply(
    state: &Arc<Mutex<AssistantSessionState>>,
    events: &mut futures::stream::BoxStream<'static, anyhow::Result<RealtimeEvent>>,
    notify: &Arc<Notify>,
    first_audio: &FirstAudio<'_>,
) -> RealtimeReply {
    use fono_tts::PcmSink as _;

    let pb_handle = {
        let s = state.lock().await;
        s.playback.clone()
    };
    let mut reply = RealtimeReply::default();
    let Some(pb) = pb_handle else {
        // No device — still drain events so history/transcripts are
        // captured, but no audio can play.
        while let Some(ev) = events.next().await {
            match ev {
                Ok(RealtimeEvent::AssistantTextDelta(s)) => reply.reply_text.push_str(&s),
                Ok(RealtimeEvent::UserTextFinal(s)) => reply.user_text = Some(s),
                Ok(RealtimeEvent::Done) | Err(_) => break,
                // `EndConversation` is full-duplex live-mode only; never
                // emitted on the push-to-talk path.
                Ok(
                    RealtimeEvent::Audio { .. }
                    | RealtimeEvent::Interrupted
                    | RealtimeEvent::EndConversation,
                ) => {}
            }
        }
        return reply;
    };
    let mut sink = fono_audio::LocalPlaybackSink::new(pb);
    let mut begun = false;
    loop {
        tokio::select! {
            biased;
            () = notify.notified() => {
                debug!(target: "fono::assistant", "realtime cancelled");
                reply.aborted = true;
                if begun {
                    let _ = sink.abort().await;
                }
                break;
            }
            ev = events.next() => {
                match ev {
                    None => break,
                    Some(Err(e)) => {
                        warn!(target: "fono::assistant", error = %e, "realtime stream error");
                        reply.aborted = true;
                        break;
                    }
                    Some(Ok(RealtimeEvent::Audio { pcm, sample_rate })) => {
                        if !begun {
                            if sink.begin().await.is_err() {
                                reply.aborted = true;
                                break;
                            }
                            begun = true;
                        }
                        first_audio.fire();
                        if sink.push(pcm, sample_rate).await.is_ok() {
                            reply.any_audio = true;
                            reply.last_audio_at = Some(std::time::Instant::now());
                        }
                    }
                    Some(Ok(RealtimeEvent::AssistantTextDelta(s))) => reply.reply_text.push_str(&s),
                    Some(Ok(RealtimeEvent::UserTextFinal(s))) => reply.user_text = Some(s),
                    Some(Ok(RealtimeEvent::Interrupted)) => {
                        // Barge-in: the model discarded the rest of this
                        // reply. Drop the queued/playing audio immediately so
                        // we don't talk over the user. A later Audio frame (a
                        // fresh reply) re-opens the gapless session.
                        debug!(target: "fono::assistant", "realtime: barge-in interrupt");
                        if begun {
                            let _ = sink.abort().await;
                            begun = false;
                        }
                    }
                    Some(Ok(RealtimeEvent::Done)) => break,
                    // Full-duplex live-mode only; never emitted in PTT.
                    Some(Ok(RealtimeEvent::EndConversation)) => {}
                }
            }
        }
    }
    if begun && !reply.aborted {
        let _ = sink.end().await;
    }
    reply
}

// ── Full-duplex live conversation mode (F8 tap) ──────────────────────
//
// A single persistent speech-to-speech session that spans many turns,
// distinct from the per-press one-shot [`run_realtime_turn`] (PTT). The
// mic streams continuously, gated by the mute-while-speaking logic
// proven in `examples/smoke_realtime_live.rs`: while the model holds
// the floor (thinking + speaking) the forwarder drops mic frames so the
// open mic cannot pick up — and re-transcribe — the model's own reply
// audio. This is the shipped baseline; AEC + talk-over barge-in are out
// of scope for this slice (see Part E of the design plan).

/// Why a live session's pump loop stopped. Drives teardown: every
/// reason except [`LiveExit::Explicit`] notifies the user and
/// reconciles the FSM, because the explicit second-tap / Escape path is
/// reconciled by the FSM itself and torn down by the orchestrator's
/// exit handler.
#[cfg(feature = "realtime")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveExit {
    /// Second tap / Escape. The orchestrator's exit handler took the
    /// session handle and the FSM is already `Idle`; the pump only
    /// releases its own resources.
    Explicit,
    /// Local silence-watch closed the session: after a completed reply
    /// the user stayed silent for `auto_stop_silence_ms`. Driven by the
    /// capture forwarder's own envelope/VAD, not the provider, so it
    /// fires even when the provider's server VAD is chattering.
    Idle,
    /// The model ended the conversation itself — it invoked the
    /// `end_conversation` tool (full-duplex only), e.g. after the user
    /// said goodbye. Surfaced as [`RealtimeEvent::EndConversation`].
    EndedByModel,
    /// `max_session_secs` wall-clock backstop reached.
    MaxDuration,
    /// The provider closed the socket (or the event stream errored).
    /// No silent reconnect — drop to idle and require a fresh tap.
    ProviderClosed,
    /// The session (or its mic capture) could not be opened at all.
    OpenFailed,
}

/// Inputs for [`run_live_session`]. The history is read from the shared
/// [`AssistantSessionState`] inside the pump (so the live session
/// carries prior conversation context), so it is not duplicated here.
#[cfg(feature = "realtime")]
pub struct LiveSessionInputs {
    pub realtime: Arc<dyn RealtimeAssistant>,
    pub system_prompt: String,
    pub language: Option<String>,
    /// FSM channel. The pump sends [`HotkeyAction::ProcessingDone`] when
    /// it self-terminates (idle / cap / provider-close / open-failure)
    /// to reconcile the FSM back to `Idle`.
    pub action_tx: mpsc::UnboundedSender<HotkeyAction>,
    pub overlay: Option<fono_overlay::OverlayHandle>,
    /// Local-silence auto-close window, in milliseconds: after a reply
    /// completes, this much continuous user silence closes the session
    /// (with the `Pondering` walking-letter animation building up to
    /// it). `0` disables silence-driven close. Sourced from
    /// `[audio].auto_stop_silence_ms`.
    pub auto_stop_silence_ms: u32,
    /// Hard max-session backstop. `Duration::ZERO` disables it.
    pub max_session: std::time::Duration,
    pub active_window_context: Option<String>,
    /// Shared rolling buffer feeding the audio visualisation. The
    /// capture forwarder appends mic frames during the user's turn and
    /// the pump appends reply PCM during the model's turn; the
    /// [`waveform_task`](Self::waveform_task) ticker reads the tail and
    /// pushes style-appropriate primitives to the overlay.
    pub viz_buf: Arc<std::sync::Mutex<RecordingBuffer>>,
    /// Abort handle for the style-aware waveform ticker the orchestrator
    /// spawned against `viz_buf`. Aborted on teardown. `None` when the
    /// overlay is disabled or the `interactive` feature is off.
    pub waveform_task: Option<tokio::task::AbortHandle>,
}

/// Drive one persistent full-duplex live session to completion. Opens
/// the session lazily (connect-on-demand), streams the mic
/// continuously with the mute-while-speaking gate, plays each reply to
/// completion, pushes both transcripts to history per turn, and loops
/// until the session ends (explicit exit, idle, cap, or provider
/// close). Spawned as a detached task by the orchestrator; teardown
/// (including clearing the [`AssistantSessionState::live`] slot on
/// self-termination) happens here.
#[cfg(feature = "realtime")]
#[allow(clippy::too_many_lines)]
pub async fn run_live_session(
    state: Arc<Mutex<AssistantSessionState>>,
    inputs: LiveSessionInputs,
    cancel: Arc<Notify>,
) {
    let LiveSessionInputs {
        realtime,
        system_prompt,
        language,
        action_tx,
        overlay,
        auto_stop_silence_ms,
        max_session,
        active_window_context,
        viz_buf,
        waveform_task,
    } = inputs;

    // Seed the session with the completed history (each user turn is
    // transcribed by the model and pushed afterwards). Vision is
    // intentionally not wired for live mode in this slice — it would
    // only add a one-shot screenshot at open with no clear live-mode
    // story; PTT keeps the vision path.
    let history_snapshot = { state.lock().await.history.snapshot() };
    let ctx = AssistantContext {
        system_prompt,
        language,
        history: history_snapshot,
        active_window_context,
        screen_capture: None,
        prefer_vision: false,
        max_new_tokens: None,
        // Local hotkey-triggered turn: this is the one the on-screen
        // overlay is meant to visualize, so allow the brain-capture tap.
        allow_brain_capture: true,
    };

    // Connect on demand. On failure, classify + notify like the PTT
    // open path, then self-terminate.
    let session = match realtime.open_session(&ctx, RealtimeMode::FullDuplex).await {
        Ok(s) => s,
        Err(e) => {
            let err_text = format!("{e:#}");
            warn!(target: "fono::assistant", error = %err_text, "live: session open failed");
            let class = fono_core::critical_notify::classify(&err_text);
            if matches!(
                class,
                fono_core::critical_notify::ErrorClass::Auth
                    | fono_core::critical_notify::ErrorClass::PaymentRequired
                    | fono_core::critical_notify::ErrorClass::Network
                    | fono_core::critical_notify::ErrorClass::TermsRequired
            ) {
                fono_core::critical_notify::notify(
                    fono_core::critical_notify::Stage::Assistant,
                    realtime.name(),
                    class,
                    &err_text,
                );
            }
            finish_live(&state, &action_tx, overlay.as_ref(), LiveExit::OpenFailed).await;
            return;
        }
    };
    let RealtimeSession { audio_in, mut events } = session;
    let native = realtime.native_input_rate();

    // Lazily ensure playback exists (mirrors the PTT path).
    {
        let mut s = state.lock().await;
        if s.playback.is_none() {
            match AudioPlayback::new(None) {
                Ok(pb) => s.playback = Some(pb),
                Err(e) => warn!(
                    target: "fono::assistant",
                    error = %e,
                    "audio playback init failed; live reply will not be audible"
                ),
            }
        }
    }

    // Continuous mic capture with the mute-while-speaking gate. The
    // capture handle (a cpal stream on some platforms) is `!Send`, so
    // it lives on a dedicated thread and is stopped via a channel —
    // mirroring the assistant push-to-talk capture. The forwarder
    // pushes resampled frames into the session sink unless the gate
    // (model holds the floor) is set, in which case the frame is
    // dropped so the model cannot hear itself.
    let mic_muted = Arc::new(AtomicBool::new(false));
    // Set true only while the pump is waiting for the user *after* a
    // completed reply; gates the local silence auto-close so a fresh
    // session (or a user still formulating their first request) is
    // never closed prematurely. Toggled by the pump's floor handlers.
    let idle_armed = Arc::new(AtomicBool::new(false));
    // Forwarder → pump signal: continuous user silence reached
    // `auto_stop_silence_ms` while `idle_armed`. The pump selects on
    // this to close with `LiveExit::Idle`.
    let silence_commit = Arc::new(Notify::new());
    let capture = match spawn_live_capture(LiveCaptureWiring {
        native,
        mic_sink: audio_in.clone(),
        gate: Arc::clone(&mic_muted),
        armed: Arc::clone(&idle_armed),
        commit: Arc::clone(&silence_commit),
        overlay: overlay.clone(),
        viz_buf: Arc::clone(&viz_buf),
        auto_stop_silence_ms,
    }) {
        Ok(c) => c,
        Err(e) => {
            warn!(target: "fono::assistant", error = %e, "live: mic capture failed to start");
            drop(audio_in);
            finish_live(&state, &action_tx, overlay.as_ref(), LiveExit::OpenFailed).await;
            return;
        }
    };
    let LiveCapture { thread: cap_thread, stop_tx: cap_stop_tx } = capture;

    let session_open = std::time::Instant::now();

    // Realtime-paced visualisation feeder (see `spawn_viz_pacer`): reply
    // audio arrives in bursts far faster than realtime, so it is drained
    // into `viz_buf` at playback pace during the model's turn rather than
    // dumped straight in (which makes the waveform race ahead then stall).
    let (viz_tx, viz_pace_task) = spawn_viz_pacer(Arc::clone(&viz_buf), Arc::clone(&mic_muted));

    // The conversation opens on the user's turn: mic live, overlay green.
    if let Some(o) = overlay.as_ref() {
        o.set_state(fono_overlay::OverlayState::AssistantRecording { db: 0 });
    }

    let mut pump = LivePump {
        state: &state,
        events: &mut events,
        cancel: &cancel,
        overlay: overlay.as_ref(),
        mic_muted: &mic_muted,
        idle_armed: &idle_armed,
        silence_commit: &silence_commit,
        viz_tx: &viz_tx,
        turns: 0,
        max_session,
        started_at: std::time::Instant::now(),
    };
    let exit = pump.run().await;
    let turns = pump.turns;

    info!(
        target: "fono::assistant",
        provider = realtime.name(),
        reason = ?exit,
        turns,
        open_secs = format!("{:.1}", session_open.elapsed().as_secs_f32()),
        "live conversation session closed"
    );

    // Teardown: stop the mic, close the input (client emits
    // audioStreamEnd), drain playback. Dropping `events` (via the
    // borrow ending) plus `audio_in` closes the socket.
    viz_pace_task.abort();
    if let Some(t) = &waveform_task {
        t.abort();
    }
    drop(audio_in);
    let _ = cap_stop_tx.send(());
    let _ = tokio::task::spawn_blocking(move || {
        let _ = cap_thread.join();
    })
    .await;
    {
        let s = state.lock().await;
        if let Some(pb) = &s.playback {
            pb.stop();
        }
    }

    finish_live(&state, &action_tx, overlay.as_ref(), exit).await;
}

/// Live-mode capture thread handle: the dedicated capture thread plus the
/// channel that signals it to stop and unwind on teardown.
#[cfg(feature = "realtime")]
struct LiveCapture {
    thread: std::thread::JoinHandle<()>,
    stop_tx: std::sync::mpsc::Sender<()>,
}

/// Wiring handed to [`spawn_live_capture`]: the model input sink plus the
/// shared gates / overlay / visualisation buffer the per-frame forwarder
/// consults.
#[cfg(feature = "realtime")]
struct LiveCaptureWiring {
    native: u32,
    mic_sink: tokio::sync::mpsc::Sender<Vec<f32>>,
    gate: Arc<AtomicBool>,
    armed: Arc<AtomicBool>,
    commit: Arc<Notify>,
    overlay: Option<fono_overlay::OverlayHandle>,
    viz_buf: Arc<std::sync::Mutex<RecordingBuffer>>,
    auto_stop_silence_ms: u32,
}

/// Per-frame state for the live-mode capture forwarder, pulled out of
/// `run_live_session` so the hot path stays small. Each frame either feeds
/// the model + visualisation (user's turn) or is dropped (model holds the
/// floor), and drives the local-silence auto-close with the `Pondering`
/// animation. The envelope follower persists across turns so the user's
/// own voiced level stays calibrated.
#[cfg(feature = "realtime")]
struct LiveFrameProcessor {
    native: u32,
    auto_stop: f32,
    mic_sink: tokio::sync::mpsc::Sender<Vec<f32>>,
    gate: Arc<AtomicBool>,
    armed: Arc<AtomicBool>,
    commit: Arc<Notify>,
    overlay: Option<fono_overlay::OverlayHandle>,
    viz_buf: Arc<std::sync::Mutex<RecordingBuffer>>,
    viz_max: usize,
    env: EnvelopeFollower,
    silence_ms: f32,
    pondering: bool,
}

#[cfg(feature = "realtime")]
impl LiveFrameProcessor {
    /// Visual lead-in before the silence bar starts filling, in ms.
    const PONDERING_VISUAL_MS: f32 = 1_000.0;
    /// How far below the user's own voiced level counts as silence, in dB.
    const SILENCE_GAP_DB: f32 = 12.0;

    fn on_frame(&mut self, pcm: &[f32]) {
        // Model holds the floor: drop the frame (mute-while-speaking) and
        // reset local silence tracking so it restarts cleanly when the
        // floor returns to the user.
        if self.gate.load(Ordering::Relaxed) {
            self.silence_ms = 0.0;
            self.pondering = false;
            return;
        }
        let _ = self.mic_sink.try_send(pcm.to_vec());
        // Feed the visualisation: the style-aware ticker reads this rolling
        // buffer. (During the model's turn the pump feeds reply PCM.)
        if let Ok(mut b) = self.viz_buf.lock() {
            b.push_rolling(pcm, self.viz_max);
        }
        self.env.push_frame(pcm);
        // Silence-driven auto-close only while waiting for the user after a
        // reply (and only when enabled).
        if self.auto_stop <= 0.0 || !self.armed.load(Ordering::Relaxed) {
            return;
        }
        let frame_ms =
            if self.native > 0 { pcm.len() as f32 * 1000.0 / self.native as f32 } else { 0.0 };
        let snap = self.env.snapshot();
        let has_ref = snap.voiced_frames > 0;
        let is_silent = !has_ref
            || rms_to_dbfs(snap.inst_rms) < rms_to_dbfs(snap.voiced_rms) - Self::SILENCE_GAP_DB;
        if !is_silent {
            self.end_pondering();
            self.silence_ms = 0.0;
            return;
        }
        self.silence_ms += frame_ms;
        if self.silence_ms >= Self::PONDERING_VISUAL_MS {
            self.show_pondering();
        }
        if self.silence_ms >= self.auto_stop {
            self.commit.notify_one();
        }
    }

    /// Drive the `Pondering` walking-letter animation as the post-reply
    /// silence builds toward the auto-close threshold.
    fn show_pondering(&mut self) {
        if let Some(o) = self.overlay.as_ref() {
            let span = (self.auto_stop - Self::PONDERING_VISUAL_MS).max(1.0);
            let progress = (((self.silence_ms - Self::PONDERING_VISUAL_MS) / span).clamp(0.0, 1.0)
                * 10_000.0) as u16;
            o.set_state(fono_overlay::OverlayState::AssistantPondering {
                db: 0,
                walk_progress: progress,
            });
        }
        self.pondering = true;
    }

    /// The user resumed speaking: revert the overlay from `Pondering` back
    /// to the green recording state.
    fn end_pondering(&mut self) {
        if self.pondering {
            if let Some(o) = self.overlay.as_ref() {
                o.set_state(fono_overlay::OverlayState::AssistantRecording { db: 0 });
            }
            self.pondering = false;
        }
    }
}

/// Spawn the continuous live-mode mic-capture thread and block until it
/// confirms the stream started. The capture handle (a cpal stream on some
/// platforms) is `!Send`, so it lives on a dedicated thread and is stopped
/// via the returned [`LiveCapture::stop_tx`]. Returns `Err` if the thread
/// could not be spawned or the stream failed to start.
#[cfg(feature = "realtime")]
fn spawn_live_capture(w: LiveCaptureWiring) -> std::result::Result<LiveCapture, String> {
    let LiveCaptureWiring {
        native,
        mic_sink,
        gate,
        armed,
        commit,
        overlay,
        viz_buf,
        auto_stop_silence_ms,
    } = w;
    let (started_tx, started_rx) = std::sync::mpsc::channel::<std::result::Result<(), String>>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::Builder::new()
        .name("fono-live-capture".into())
        .spawn(move || {
            let cap = AudioCapture::new(CaptureConfig { target_sample_rate: native, source: None });
            let mut proc = LiveFrameProcessor {
                native,
                auto_stop: auto_stop_silence_ms as f32,
                mic_sink,
                gate,
                armed,
                commit,
                overlay,
                viz_buf,
                // Keep ~1 s of recent mic audio for the visualisation ticker.
                viz_max: native.max(1) as usize,
                env: EnvelopeFollower::new(EnvelopeConfig {
                    sample_rate: native,
                    ..Default::default()
                }),
                silence_ms: 0.0,
                pondering: false,
            };
            match cap.start_with_forwarder(move |pcm: &[f32]| proc.on_frame(pcm)) {
                Ok(handle) => {
                    let _ = started_tx.send(Ok(()));
                    let _ = stop_rx.recv();
                    drop(handle);
                }
                Err(e) => {
                    let _ = started_tx.send(Err(format!("{e:#}")));
                }
            }
        })
        .map_err(|e| format!("{e:#}"))?;
    match started_rx.recv() {
        Ok(Ok(())) => Ok(LiveCapture { thread, stop_tx }),
        other => {
            let _ = stop_tx.send(());
            let _ = thread.join();
            Err(match other {
                Ok(Err(e)) => e,
                _ => "live capture thread exited before signalling start".to_string(),
            })
        }
    }
}

/// Spawn the realtime-paced visualisation feeder. Reply audio arrives from
/// the provider in bursts far faster than realtime; pushing it straight
/// into the rolling viz buffer makes the ticker race ahead of the audible
/// playback and then stall. This task drains the pump's chunks into
/// `viz_buf` at playback pace (one quantum per tick), gated by `mic_muted`
/// so it only owns the buffer during the model's turn — the mic forwarder
/// owns it during the user's turn. When the model holds the floor but no
/// audio is queued (thinking, or a pause), it feeds silence so the
/// waveform decays to flat instead of freezing on a stale frame. Returns
/// the sender the pump pushes `(pcm, rate)` chunks to and the task handle
/// to abort on teardown.
#[cfg(feature = "realtime")]
fn spawn_viz_pacer(
    viz_buf: Arc<std::sync::Mutex<RecordingBuffer>>,
    mic_muted: Arc<AtomicBool>,
) -> (tokio::sync::mpsc::UnboundedSender<(Vec<f32>, u32)>, tokio::task::JoinHandle<()>) {
    let (viz_tx, mut viz_rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<f32>, u32)>();
    let task = tokio::spawn(async move {
        const VIZ_TICK_MS: u64 = 33;
        let mut pending: std::collections::VecDeque<f32> = std::collections::VecDeque::new();
        let mut rate = 24_000u32;
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(VIZ_TICK_MS));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                chunk = viz_rx.recv() => match chunk {
                    Some((pcm, r)) => { rate = r.max(1); pending.extend(pcm); }
                    None => break,
                },
                _ = tick.tick() => {
                    // User's turn: the mic forwarder owns the viz buffer.
                    if !mic_muted.load(Ordering::Relaxed) {
                        pending.clear();
                        continue;
                    }
                    let quantum = (rate as usize * VIZ_TICK_MS as usize / 1000).max(1);
                    let frame: Vec<f32> = if pending.is_empty() {
                        vec![0.0; quantum]
                    } else {
                        let take = quantum.min(pending.len());
                        pending.drain(..take).collect()
                    };
                    if let Ok(mut b) = viz_buf.lock() {
                        b.push_rolling(&frame, rate as usize);
                    }
                }
            }
        }
    });
    (viz_tx, task)
}

/// Borrowed context for the live event/timer loop. Kept as a struct so
/// the loop body stays under clippy's argument-count limit.
#[cfg(feature = "realtime")]
struct LivePump<'a> {
    state: &'a Arc<Mutex<AssistantSessionState>>,
    events: &'a mut futures::stream::BoxStream<'static, anyhow::Result<RealtimeEvent>>,
    cancel: &'a Arc<Notify>,
    overlay: Option<&'a fono_overlay::OverlayHandle>,
    /// Mute-while-speaking gate the capture forwarder consults. `true`
    /// whenever the model holds the floor (thinking + speaking).
    mic_muted: &'a Arc<AtomicBool>,
    /// Armed (`true`) only while waiting for the user after a completed
    /// reply; gates the forwarder's local silence auto-close.
    idle_armed: &'a Arc<AtomicBool>,
    /// Forwarder → pump silence-close signal. The forwarder fires it
    /// after `auto_stop_silence_ms` of continuous post-reply silence.
    silence_commit: &'a Arc<Notify>,
    /// Sender to the realtime-paced visualisation feeder. The pump
    /// forwards each reply-audio chunk here (cloned) during the model's
    /// turn; the feeder task drains it into the shared viz buffer at
    /// playback pace so audio-out animates smoothly instead of in
    /// faster-than-realtime network bursts. See [`run_live_session`].
    viz_tx: &'a tokio::sync::mpsc::UnboundedSender<(Vec<f32>, u32)>,
    /// Completed turns (each `Done`), for the session-closed log line.
    turns: u32,
    max_session: std::time::Duration,
    started_at: std::time::Instant,
}

#[cfg(feature = "realtime")]
impl LivePump<'_> {
    /// Give the floor to the model: mute the mic (drop the open-mic
    /// echo of the reply) and paint the "waiting on first audio" amber
    /// state. Idempotent within a turn.
    fn give_floor_to_model(&self) {
        // Stop arming the silence auto-close while the model speaks.
        self.idle_armed.store(false, Ordering::Relaxed);
        if !self.mic_muted.swap(true, Ordering::Relaxed) {
            if let Some(o) = self.overlay {
                o.set_state(fono_overlay::OverlayState::AssistantThinking);
            }
        }
    }

    /// Return the floor to the user: unmute the mic and paint the green
    /// recording state. Called at each turn boundary (`Done`).
    fn give_floor_to_user(&self) {
        self.mic_muted.store(false, Ordering::Relaxed);
        // A reply just completed: arm the local silence auto-close so a
        // trailing silence (the user is done) closes the session.
        self.idle_armed.store(true, Ordering::Relaxed);
        if let Some(o) = self.overlay {
            o.set_state(fono_overlay::OverlayState::AssistantRecording { db: 0 });
        }
    }

    /// Run the multi-turn event/timer loop until the session ends.
    async fn run(&mut self) -> LiveExit {
        use fono_tts::PcmSink as _;

        let pb_handle = { self.state.lock().await.playback.clone() };
        let mut sink = pb_handle.map(fono_audio::LocalPlaybackSink::new);

        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Per-turn accumulators.
        let mut reply_text = String::new();
        let mut user_text: Option<String> = None;
        let mut begun = false;
        // Set when the model invokes `end_conversation`; the session
        // closes (gracefully) once the current reply finishes.
        let mut model_wants_end = false;

        loop {
            tokio::select! {
                biased;
                () = self.cancel.notified() => return LiveExit::Explicit,
                () = self.silence_commit.notified() => return LiveExit::Idle,
                _ = tick.tick() => {
                    if !self.max_session.is_zero() && self.started_at.elapsed() >= self.max_session {
                        return LiveExit::MaxDuration;
                    }
                }
                ev = self.events.next() => {
                    match ev {
                        None => return LiveExit::ProviderClosed,
                        Some(Err(e)) => {
                            warn!(target: "fono::assistant", error = %e, "live: event stream error");
                            return LiveExit::ProviderClosed;
                        }
                        Some(Ok(RealtimeEvent::Audio { pcm, sample_rate })) => {
                            // First audio of this turn → model is speaking.
                            self.give_floor_to_model();
                            // Hand the chunk to the realtime-paced viz
                            // feeder so audio-out animates at playback
                            // pace (not the faster network arrival rate).
                            let _ = self.viz_tx.send((pcm.clone(), sample_rate));
                            if let Some(s) = sink.as_mut() {
                                if !begun && s.begin().await.is_ok() {
                                    begun = true;
                                    if let Some(o) = self.overlay {
                                        o.set_state(
                                            fono_overlay::OverlayState::AssistantSpeaking,
                                        );
                                    }
                                }
                                if begun {
                                    let _ = s.push(pcm, sample_rate).await;
                                }
                            }
                        }
                        Some(Ok(RealtimeEvent::AssistantTextDelta(s))) => reply_text.push_str(&s),
                        Some(Ok(RealtimeEvent::UserTextFinal(s))) => {
                            // The user finished speaking; the model now
                            // owns the floor while it formulates a reply.
                            self.give_floor_to_model();
                            user_text = Some(s);
                        }
                        Some(Ok(RealtimeEvent::EndConversation)) => {
                            // The model decided the conversation is over.
                            // Finish any in-flight reply, then close. If
                            // nothing is playing, close immediately.
                            model_wants_end = true;
                            if !begun {
                                return LiveExit::EndedByModel;
                            }
                        }
                        Some(Ok(RealtimeEvent::Interrupted)) => {
                            // Barge-in (rare under mute-while-speaking):
                            // drop queued reply audio; a later Audio frame
                            // reopens the stream.
                            if begun {
                                if let Some(s) = sink.as_mut() {
                                    let _ = s.abort().await;
                                }
                                begun = false;
                            }
                        }
                        Some(Ok(RealtimeEvent::Done)) => {
                            if begun {
                                if let Some(s) = sink.as_mut() {
                                    let _ = s.end().await;
                                }
                                begun = false;
                            }
                            // Commit both transcripts for the turn.
                            {
                                let mut st = self.state.lock().await;
                                if let Some(u) = user_text
                                    .take()
                                    .map(|u| u.trim().to_string())
                                    .filter(|u| !u.is_empty())
                                {
                                    st.history.push_user(u);
                                }
                                if !reply_text.trim().is_empty() {
                                    st.history.push_assistant(reply_text.trim().to_string());
                                }
                            }
                            reply_text.clear();
                            self.turns = self.turns.saturating_add(1);
                            // If the model asked to end, close now that the
                            // reply has fully drained.
                            if model_wants_end {
                                return LiveExit::EndedByModel;
                            }
                            // Floor returns to the user for the next turn.
                            self.give_floor_to_user();
                        }
                    }
                }
            }
        }
    }
}

/// Finalise a live session: hide the overlay and, for self-termination
/// reasons, clear the persistent session slot, reconcile the FSM to
/// `Idle`, and tell the user why the session ended. The explicit
/// (second-tap / Escape) path is a no-op here — the orchestrator's exit
/// handler owns that teardown.
#[cfg(feature = "realtime")]
async fn finish_live(
    state: &Arc<Mutex<AssistantSessionState>>,
    action_tx: &mpsc::UnboundedSender<HotkeyAction>,
    overlay: Option<&fono_overlay::OverlayHandle>,
    exit: LiveExit,
) {
    if let Some(o) = overlay {
        o.set_state(fono_overlay::OverlayState::Hidden);
    }
    if exit == LiveExit::Explicit {
        return;
    }
    // Self-termination: clear the slot so a fresh tap reopens, and drive
    // the FSM back to Idle. Clearing drops this task's own JoinHandle,
    // which merely detaches it (we are inside that task) — safe.
    {
        let mut s = state.lock().await;
        s.live = None;
    }
    let _ = action_tx.send(HotkeyAction::ProcessingDone);
    // Graceful, user-initiated ends (the user fell silent, or said
    // goodbye and the model closed the conversation) are expected and
    // need no notification — the overlay simply disappears. Only the
    // unexpected / forced ends below are worth a desktop notification.
    let (summary, body) = match exit {
        LiveExit::Idle | LiveExit::EndedByModel => return,
        LiveExit::MaxDuration => (
            "Fono — live mode closed",
            "Live conversation reached its time limit. Tap to start a new session.",
        ),
        LiveExit::ProviderClosed => (
            "Fono — live mode ended",
            "The realtime provider closed the session. Tap to reconnect.",
        ),
        LiveExit::OpenFailed => (
            "Fono — live mode unavailable",
            "Could not open a realtime session. Check your connection and API key, \
             then tap to retry.",
        ),
        LiveExit::Explicit => return,
    };
    fono_core::notify::send(
        summary,
        body,
        "dialog-information",
        5_000,
        fono_core::notify::Urgency::Normal,
    );
}

/// Pull the human-readable `message` out of a provider error blob
/// that wraps a JSON envelope like `{"error":{"message":"...", ...}}`.
///
/// Handles plain unescaped messages (Groq + OpenAI shape) and JSON
/// strings containing escaped quotes. Returns `None` when no
/// `"message":"..."` substring is present so the caller can fall back
/// to the raw text. Output is trimmed to 240 chars to keep desktop
/// notifications readable on every DE — `notify-send` on KDE truncates
/// silently past that, GNOME wraps but loses focus quickly.
fn extract_json_message(blob: &str) -> Option<String> {
    let key = "\"message\":\"";
    let start = blob.find(key)? + key.len();
    let tail = &blob[start..];
    // Scan for the terminating unescaped quote.
    let mut end = 0;
    let mut prev_backslash = false;
    for (i, c) in tail.char_indices() {
        if c == '"' && !prev_backslash {
            end = i;
            break;
        }
        prev_backslash = c == '\\' && !prev_backslash;
    }
    if end == 0 {
        return None;
    }
    let raw = &tail[..end];
    // Unescape the minimal JSON sequences we care about.
    let unescaped = raw.replace("\\\"", "\"").replace("\\n", " ").replace("\\\\", "\\");
    Some(truncate(&unescaped, 240).to_string())
}

/// Clip a string to at most `max` chars on a char boundary, appending
/// an ellipsis if anything was dropped. Borrowed style mirrors the
/// helper in `fono-tts::openai_compat` so call-sites read alike.
fn truncate(s: &str, max: usize) -> std::borrow::Cow<'_, str> {
    if s.chars().count() <= max {
        return std::borrow::Cow::Borrowed(s);
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    std::borrow::Cow::Owned(format!("{cut}…"))
}

/// Map a `ToolEvent::Result.summary` string back to a typed
/// [`AssistantToolOutcome`] for the `assistant:` summary line. The
/// classifier is conservative: it only recognises the canned phrases
/// emitted by the assistant client's tool-dispatch helper; any other
/// summary (including the success case `"Captured 954x564 PNG…"`)
/// falls through to [`AssistantToolOutcome::Ok`].
fn classify_tool_outcome(summary: &str) -> AssistantToolOutcome {
    let lower = summary.to_ascii_lowercase();
    if lower.contains("private window") {
        AssistantToolOutcome::Private
    } else if lower.contains("cancel") {
        AssistantToolOutcome::Cancelled
    } else if lower.contains("no capture tool") || lower.contains("no-tool") {
        AssistantToolOutcome::NoTool
    } else if lower.starts_with("captured ") || lower.contains("png of") {
        AssistantToolOutcome::Ok
    } else if lower.contains("failed") || lower.contains("error") {
        AssistantToolOutcome::Failed
    } else {
        AssistantToolOutcome::Ok
    }
}

/// Non-blocking probe — returns true if the pump should bail. Uses
/// `now_or_never` on a `notified()` future so we don't await.
fn notify_triggered(_notify: &Arc<Notify>) -> bool {
    // tokio::sync::Notify doesn't expose a non-await poll, so we
    // can't cheaply "probe" without consuming a permit. The select
    // arms above already cover cancellation; this helper exists for
    // symmetry but always returns false. Keeping it as a hook makes
    // future swap to `CancellationToken` (which has `is_cancelled`)
    // a one-line change.
    false
}

#[cfg(test)]
mod tests {
    use super::{extract_json_message, read_dwell, truncate};

    #[test]
    fn read_dwell_floors_short_replies() {
        // A one-word reply must still stay legible: never below the 3 s floor.
        assert_eq!(read_dwell(0), std::time::Duration::from_secs(3));
        assert_eq!(read_dwell(1), std::time::Duration::from_secs(3));
        assert_eq!(read_dwell(20), std::time::Duration::from_secs(3));
    }

    #[test]
    fn read_dwell_caps_huge_replies() {
        // A pathologically long reply can't wedge the overlay: 60 s ceiling.
        assert_eq!(read_dwell(1_000_000), std::time::Duration::from_secs(60));
    }

    #[test]
    fn read_dwell_scales_between_floor_and_cap() {
        // ~130 wpm over ~6 chars/word: mid-size replies land strictly
        // between the bounds and grow monotonically with length.
        // 300 chars ≈ 50 words ≈ 23 s; 600 chars ≈ 100 words ≈ 46 s.
        let short = read_dwell(300);
        let long = read_dwell(600);
        assert!(short > std::time::Duration::from_secs(3), "300 chars should exceed floor");
        assert!(long < std::time::Duration::from_secs(60), "600 chars should stay under cap");
        assert!(long > short, "longer replies dwell longer");
    }

    #[test]
    fn extract_json_message_pulls_groq_429_body() {
        // Verbatim shape from the user's Groq TTS 429 report.
        let blob = "groq TTS returned 429 Too Many Requests (request_id=req_01ks4jx1, \
                    text_len=121): {\"error\":{\"message\":\"Rate limit reached for model \
                    `canopylabs/orpheus-v1-english` in organization `org_x` service tier \
                    `on_demand` on tokens per day (TPD): Limit 3600, Used 3571, Requested \
                    121. Please try again in 36m48s.\",\"type\":\"tokens\",\"code\":\
                    \"rate_limit_exceeded\"}}";
        let msg = extract_json_message(blob).expect("message present");
        assert!(msg.starts_with("Rate limit reached for model"), "got: {msg}");
        assert!(msg.contains("try again in 36m48s"));
        // The request_id / code / type fields must not leak in.
        assert!(!msg.contains("request_id"));
        assert!(!msg.contains("rate_limit_exceeded"));
    }

    #[test]
    fn extract_json_message_returns_none_without_envelope() {
        assert!(extract_json_message("just a transport error").is_none());
    }

    #[test]
    fn extract_json_message_handles_escaped_quotes() {
        let blob = r#"{"error":{"message":"key \"X\" was rejected","code":"x"}}"#;
        assert_eq!(extract_json_message(blob).as_deref(), Some(r#"key "X" was rejected"#));
    }

    #[test]
    fn truncate_keeps_short_strings_intact() {
        assert_eq!(truncate("hi", 10), "hi");
    }

    #[test]
    fn truncate_appends_ellipsis_on_overflow() {
        let out = truncate("abcdefghij", 5);
        assert_eq!(out, "abcd…");
    }

    #[cfg(feature = "realtime")]
    #[test]
    fn buffered_frame_stream_chunks_at_50ms_and_closes() {
        use super::buffered_frame_stream;
        // 16 kHz → 50 ms frame = 800 samples. 2000 samples → 800,800,400.
        let pcm = vec![0.5f32; 2000];
        let mut rx = buffered_frame_stream(&pcm, 16_000);
        let mut sizes = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            sizes.push(frame.len());
        }
        assert_eq!(sizes, vec![800, 800, 400]);
        // Sender dropped → stream is closed (next recv would yield None).
        assert!(matches!(rx.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)));
    }

    #[cfg(feature = "realtime")]
    #[test]
    fn buffered_frame_stream_empty_pcm_is_immediately_closed() {
        use super::buffered_frame_stream;
        let mut rx = buffered_frame_stream(&[], 16_000);
        assert!(matches!(rx.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)));
    }

    #[cfg(feature = "realtime")]
    #[tokio::test]
    async fn peek_first_frame_skips_empties_and_preserves_remainder() {
        use super::{buffered_frame_stream, peek_first_frame};
        let pcm = vec![0.25f32; 1600]; // 16 kHz → 800,800
        let rx = buffered_frame_stream(&pcm, 16_000);
        let (first, mut rest) = peek_first_frame(rx).await.expect("first frame present");
        assert_eq!(first.len(), 800);
        // The remaining frame is still queued (nothing lost on peek).
        let next = rest.recv().await.expect("second frame present");
        assert_eq!(next.len(), 800);
        assert!(rest.recv().await.is_none(), "stream closes after all frames");
    }

    #[cfg(feature = "realtime")]
    #[tokio::test]
    async fn peek_first_frame_returns_none_on_empty_stream() {
        use super::{buffered_frame_stream, peek_first_frame};
        let rx = buffered_frame_stream(&[], 16_000);
        assert!(peek_first_frame(rx).await.is_none());
    }

    // PTT (push-to-talk) regression guard: the realtime reply pump must
    // drain the model's reply until `Done` and then STOP — it waits for
    // the model to finish, and does not consume anything past the turn
    // boundary. This pins the contract the live-mode work (Parts C/D of
    // `plans/2026-06-22-realtime-live-conversation-mode-v4.md`) must not
    // silently regress. Uses the device-free drain path (playback None).
    #[cfg(feature = "realtime")]
    #[tokio::test]
    async fn realtime_reply_drains_to_done_then_stops() {
        use std::sync::Arc;

        use fono_assistant::{ConversationHistory, RealtimeEvent};
        use futures::StreamExt as _;
        use tokio::sync::{Mutex, Notify};

        use super::{drive_realtime_reply, AssistantSessionState, FirstAudio};

        let state =
            Arc::new(Mutex::new(AssistantSessionState::new(ConversationHistory::default())));
        let events_vec: Vec<anyhow::Result<RealtimeEvent>> = vec![
            Ok(RealtimeEvent::UserTextFinal("hi".into())),
            Ok(RealtimeEvent::AssistantTextDelta("hel".into())),
            Ok(RealtimeEvent::AssistantTextDelta("lo".into())),
            Ok(RealtimeEvent::Done),
            // Anything after `Done` belongs to no turn we are driving and
            // must be left untouched.
            Ok(RealtimeEvent::AssistantTextDelta("LEAK".into())),
        ];
        let mut events = futures::stream::iter(events_vec).boxed();
        let notify = Arc::new(Notify::new());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let first_audio = FirstAudio::new(std::time::Instant::now(), &tx, None);

        let reply = drive_realtime_reply(&state, &mut events, &notify, &first_audio).await;
        assert_eq!(reply.reply_text, "hello", "reply transcript accumulates across deltas");
        assert_eq!(reply.user_text.as_deref(), Some("hi"), "user transcript captured");
        assert!(!reply.aborted, "a clean Done is not an abort");

        // Stopped exactly at `Done`: the post-Done event is still queued.
        match events.next().await {
            Some(Ok(RealtimeEvent::AssistantTextDelta(s))) => {
                assert_eq!(s, "LEAK", "pump stopped at Done without over-reading");
            }
            other => panic!("expected the leftover delta after Done, got {other:?}"),
        }
    }

    // ── Live-mode pump tests (headless, no network / audio) ──────────
    //
    // The live pump's testable surface: turn-boundary history
    // accumulation across many turns, the mute-while-speaking gate
    // tracking floor ownership, the provider-close exit reason, and the
    // explicit-cancel path. Audio playback is absent (playback `None`)
    // so the `Audio` branch is exercised without a device.

    #[cfg(feature = "realtime")]
    fn live_pump_with<'a>(
        state: &'a std::sync::Arc<tokio::sync::Mutex<super::AssistantSessionState>>,
        events: &'a mut futures::stream::BoxStream<
            'static,
            anyhow::Result<fono_assistant::RealtimeEvent>,
        >,
        cancel: &'a std::sync::Arc<tokio::sync::Notify>,
        mic_muted: &'a std::sync::Arc<std::sync::atomic::AtomicBool>,
        idle_armed: &'a std::sync::Arc<std::sync::atomic::AtomicBool>,
        silence_commit: &'a std::sync::Arc<tokio::sync::Notify>,
        viz_tx: &'a tokio::sync::mpsc::UnboundedSender<(Vec<f32>, u32)>,
    ) -> super::LivePump<'a> {
        super::LivePump {
            state,
            events,
            cancel,
            overlay: None,
            mic_muted,
            idle_armed,
            silence_commit,
            viz_tx,
            turns: 0,
            // Timer disabled: drive the loop purely by the event script.
            max_session: std::time::Duration::ZERO,
            started_at: std::time::Instant::now(),
        }
    }

    /// Multi-turn: each `Done` commits the user + assistant transcripts
    /// for that turn, the loop continues for the next turn, and a closed
    /// stream ends the session with `ProviderClosed`. The gate ends
    /// un-muted (floor back to the user after the last `Done`).
    #[cfg(feature = "realtime")]
    #[tokio::test]
    async fn live_pump_accumulates_history_across_turns() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        use fono_assistant::{ChatRole, ConversationHistory, RealtimeEvent};
        use futures::StreamExt as _;
        use tokio::sync::{Mutex, Notify};

        let state =
            Arc::new(Mutex::new(super::AssistantSessionState::new(ConversationHistory::default())));
        let events_vec: Vec<anyhow::Result<RealtimeEvent>> = vec![
            Ok(RealtimeEvent::UserTextFinal("hi".into())),
            Ok(RealtimeEvent::AssistantTextDelta("hel".into())),
            Ok(RealtimeEvent::AssistantTextDelta("lo".into())),
            Ok(RealtimeEvent::Done),
            Ok(RealtimeEvent::UserTextFinal("bye".into())),
            Ok(RealtimeEvent::AssistantTextDelta("see ya".into())),
            Ok(RealtimeEvent::Done),
            // Stream then closes → provider-side close.
        ];
        let mut events = futures::stream::iter(events_vec).boxed();
        let cancel = Arc::new(Notify::new());
        let mic_muted = Arc::new(AtomicBool::new(false));
        let idle_armed = Arc::new(AtomicBool::new(false));
        let silence_commit = Arc::new(Notify::new());
        let (viz_tx, _viz_rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<f32>, u32)>();

        let exit = {
            let mut pump = live_pump_with(
                &state,
                &mut events,
                &cancel,
                &mic_muted,
                &idle_armed,
                &silence_commit,
                &viz_tx,
            );
            pump.run().await
        };
        assert_eq!(exit, super::LiveExit::ProviderClosed, "closed stream ends the session");
        assert!(!mic_muted.load(Ordering::Relaxed), "floor back to user after the last Done");

        let snap = { state.lock().await.history.snapshot() };
        assert_eq!(snap.len(), 4, "two full turns → four history entries");
        assert_eq!((snap[0].role, snap[0].content.as_str()), (ChatRole::User, "hi"));
        assert_eq!((snap[1].role, snap[1].content.as_str()), (ChatRole::Assistant, "hello"));
        assert_eq!((snap[2].role, snap[2].content.as_str()), (ChatRole::User, "bye"));
        assert_eq!((snap[3].role, snap[3].content.as_str()), (ChatRole::Assistant, "see ya"));
    }

    /// The mute-while-speaking gate engages when the user's turn ends
    /// (model takes the floor) and stays engaged until the turn `Done`.
    /// A stream that closes mid-reply leaves the mic muted.
    #[cfg(feature = "realtime")]
    #[tokio::test]
    async fn live_pump_mutes_mic_while_model_holds_floor() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        use fono_assistant::{ConversationHistory, RealtimeEvent};
        use futures::StreamExt as _;
        use tokio::sync::{Mutex, Notify};

        let state =
            Arc::new(Mutex::new(super::AssistantSessionState::new(ConversationHistory::default())));
        // User finishes; model would start replying — stream closes
        // before `Done`, so the model still holds the floor at exit.
        let events_vec: Vec<anyhow::Result<RealtimeEvent>> =
            vec![Ok(RealtimeEvent::UserTextFinal("hello".into()))];
        let mut events = futures::stream::iter(events_vec).boxed();
        let cancel = Arc::new(Notify::new());
        let mic_muted = Arc::new(AtomicBool::new(false));
        let idle_armed = Arc::new(AtomicBool::new(false));
        let silence_commit = Arc::new(Notify::new());
        let (viz_tx, _viz_rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<f32>, u32)>();

        let exit = {
            let mut pump = live_pump_with(
                &state,
                &mut events,
                &cancel,
                &mic_muted,
                &idle_armed,
                &silence_commit,
                &viz_tx,
            );
            pump.run().await
        };
        assert_eq!(exit, super::LiveExit::ProviderClosed);
        assert!(
            mic_muted.load(Ordering::Relaxed),
            "mic stays muted while the model holds the floor (no Done yet)"
        );
    }

    /// An explicit cancel (second tap / Escape) ends the loop with
    /// `Explicit` and takes priority over a pending event (biased select).
    #[cfg(feature = "realtime")]
    #[tokio::test]
    async fn live_pump_cancel_exits_explicit() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;

        use fono_assistant::{ConversationHistory, RealtimeEvent};
        use futures::StreamExt as _;
        use tokio::sync::{Mutex, Notify};

        let state =
            Arc::new(Mutex::new(super::AssistantSessionState::new(ConversationHistory::default())));
        let events_vec: Vec<anyhow::Result<RealtimeEvent>> =
            vec![Ok(RealtimeEvent::AssistantTextDelta("never read".into()))];
        let mut events = futures::stream::iter(events_vec).boxed();
        let cancel = Arc::new(Notify::new());
        // Pre-arm the cancel permit so the first `notified()` poll wins.
        cancel.notify_one();
        let mic_muted = Arc::new(AtomicBool::new(false));
        let idle_armed = Arc::new(AtomicBool::new(false));
        let silence_commit = Arc::new(Notify::new());
        let (viz_tx, _viz_rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<f32>, u32)>();

        let exit = {
            let mut pump = live_pump_with(
                &state,
                &mut events,
                &cancel,
                &mic_muted,
                &idle_armed,
                &silence_commit,
                &viz_tx,
            );
            pump.run().await
        };
        assert_eq!(exit, super::LiveExit::Explicit, "cancel wins over a queued event");
        // The queued event was not consumed.
        assert!(events.next().await.is_some(), "biased cancel left the event untouched");
    }
}
