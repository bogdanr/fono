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
use fono_assistant::{RealtimeAssistant, RealtimeEvent, RealtimeSession};
use fono_audio::AudioPlayback;
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
}

impl AssistantSessionState {
    #[must_use]
    pub fn new(history: ConversationHistory) -> Self {
        Self { history, current_turn: None, playback: None }
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
        self.playback = None;
    }
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
    pub tts: Arc<dyn TextToSpeech>,
    pub system_prompt: String,
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
    let mut metrics = AssistantTurnMetrics { language: language.clone(), ..Default::default() };

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
    let tts_lang: Option<String> = metrics.language.clone();

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
    trace: Option<&TurnTrace>,
) -> Result<RealtimeSession> {
    let open_started = std::time::Instant::now();
    match realtime.open_session(ctx).await {
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
    };

    // Open the live session.
    let RealtimeSession { audio_in, mut events } =
        open_realtime_or_notify(realtime.as_ref(), &ctx, trace.as_ref()).await?;

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
                Ok(RealtimeEvent::Audio { .. } | RealtimeEvent::Interrupted) => {}
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
                }
            }
        }
    }
    if begun && !reply.aborted {
        let _ = sink.end().await;
    }
    reply
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
    use super::{extract_json_message, truncate};

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
}
