// SPDX-License-Identifier: GPL-3.0-only
//! Voice-assistant pipeline. Orchestrates the F10 (or IPC-triggered)
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

use std::sync::Arc;

use anyhow::Result;
use fono_assistant::{Assistant, AssistantContext, ConversationHistory};
use fono_audio::AudioPlayback;
use fono_hotkey::HotkeyAction;
use fono_stt::SpeechToText;
use fono_tts::{SentenceSplitter, TextToSpeech};
use futures::stream::StreamExt;
use tokio::sync::{mpsc, Mutex, Notify};
use tracing::{debug, info, warn};

/// Per-orchestrator assistant state. Owned by the
/// [`crate::session::SessionOrchestrator`] inside an `Arc<Mutex<…>>`
/// so the IPC handlers and the pump task share a single source of
/// truth.
///
/// `playback` is created lazily on first use (so a daemon that never
/// hits F10 doesn't open an audio output stream / spawn paplay) and
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
    /// caller has already run the STT step (live-streaming F10 path).
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
    /// string as the user's turn. Set by the live-streaming F10 path
    /// (interactive mode + streaming-capable backend) so the same
    /// transcription that drove the realtime overlay preview gets
    /// forwarded to the LLM rather than re-running STT.
    pub pre_transcribed: Option<String>,
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
    } = inputs;

    // 1. Resolve the user's text. When `pre_transcribed` is set the
    //    caller already ran streaming STT (live-mode F10 path); we
    //    skip the batch call entirely. Otherwise run STT on the
    //    captured PCM.
    let user_text = if let Some(text) = pre_transcribed {
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            debug!(target: "fono::assistant", "skip: empty pre-transcribed text");
            return Ok(false);
        }
        info!(
            target: "fono::assistant",
            "pre-transcribed: {trimmed:?}"
        );
        trimmed
    } else {
        if pcm.is_empty() {
            debug!(target: "fono::assistant", "skip: empty PCM");
            return Ok(false);
        }
        let stt_started = std::time::Instant::now();
        let transcription = tokio::select! {
            biased;
            () = notify.notified() => {
                debug!(target: "fono::assistant", "cancelled before STT");
                return Ok(false);
            }
            r = stt.transcribe(&pcm, sample_rate, language.as_deref()) => r?,
        };
        let trimmed = transcription.text.trim().to_string();
        if trimmed.is_empty() {
            debug!(target: "fono::assistant", "skip: empty transcript");
            return Ok(false);
        }
        info!(
            target: "fono::assistant",
            stt_ms = stt_started.elapsed().as_millis() as u64,
            "STT: {trimmed:?}"
        );
        trimmed
    };

    // 2. Build context from history (prune-on-snapshot) + push user turn.
    let history_snapshot = {
        let mut s = state.lock().await;
        s.history.push_user(user_text.clone());
        s.history.snapshot()
    };
    let ctx = AssistantContext { system_prompt, language, history: history_snapshot };

    // 3. Open the LLM stream.
    let llm_started = std::time::Instant::now();
    let mut deltas = tokio::select! {
        biased;
        () = notify.notified() => {
            debug!(target: "fono::assistant", "cancelled before LLM");
            return Ok(false);
        }
        r = assistant.reply_stream(&user_text, &ctx) => match r {
            Ok(d) => d,
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
                return Err(e);
            }
        },
    };

    // 4. Lazily ensure a playback handle exists.
    {
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
    }

    // 5. Pump deltas through the SentenceSplitter into TTS+playback.
    let mut splitter = SentenceSplitter::new();
    let mut full_reply = String::new();
    let mut any_audio = false;
    let mut first_audio_at: Option<std::time::Instant> = None;
    let mut speaking_announced = false;
    let mut synthesising_announced = false;

    loop {
        let next = tokio::select! {
            biased;
            () = notify.notified() => {
                debug!(target: "fono::assistant", "cancelled mid-stream");
                break;
            }
            n = deltas.next() => n,
        };
        let Some(item) = next else {
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
                break;
            }
        };
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
            info!(
                target: "fono::assistant",
                llm_ttfb_ms = llm_started.elapsed().as_millis() as u64,
                "first LLM delta — overlay: THINKING → SYNTHESISING"
            );
            if let Some(o) = overlay.as_ref() {
                o.set_state(fono_overlay::OverlayState::AssistantSynthesising);
            }
        }
        full_reply.push_str(&delta.text);
        for sentence in splitter.push(&delta.text) {
            if synth_and_enqueue(&state, &tts, &sentence, &notify).await {
                any_audio = true;
                if first_audio_at.is_none() {
                    first_audio_at = Some(std::time::Instant::now());
                    info!(
                        target: "fono::assistant",
                        ttfa_ms = llm_started.elapsed().as_millis() as u64,
                        "first audio queued"
                    );
                    // FSM `AssistantThinking → AssistantSpeaking` +
                    // overlay SYNTHESISING → SPEAKING. This is the
                    // moment the user actually starts hearing the
                    // reply.
                    if !speaking_announced {
                        speaking_announced = true;
                        let _ = action_tx.send(HotkeyAction::AssistantSpeakingStarted);
                        if let Some(o) = overlay.as_ref() {
                            o.set_state(fono_overlay::OverlayState::AssistantSpeaking);
                        }
                    }
                }
            }
            if notify_triggered(&notify) {
                break;
            }
        }
        if notify_triggered(&notify) {
            break;
        }
    }

    if !notify_triggered(&notify) {
        if let Some(tail) = splitter.flush() {
            if synth_and_enqueue(&state, &tts, &tail, &notify).await {
                any_audio = true;
                // Belt-and-braces: a tail flush that arrives without
                // any preceding delta is impossible (the splitter is
                // empty until `push`ed), but if a future refactor
                // ever ends up here without having announced
                // SPEAKING, we still want the FSM/overlay flipped
                // before audio actually plays. We're past the delta
                // loop, so no need to flip `speaking_announced` itself.
                if !speaking_announced {
                    let _ = action_tx.send(HotkeyAction::AssistantSpeakingStarted);
                    if let Some(o) = overlay.as_ref() {
                        o.set_state(fono_overlay::OverlayState::AssistantSpeaking);
                    }
                }
            }
        }
    }

    // 6. Push the assistant turn into history (with whatever we got
    //    before cancellation; partial replies still inform the next
    //    turn).
    if !full_reply.trim().is_empty() {
        let mut s = state.lock().await;
        s.history.push_assistant(full_reply.trim().to_string());
    }

    info!(
        target: "fono::assistant",
        total_ms = llm_started.elapsed().as_millis() as u64,
        chars = full_reply.len(),
        "assistant turn done"
    );

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
        if !pb.is_idle() {
            let drain_started = std::time::Instant::now();
            let timeout = std::time::Duration::from_secs(60);
            loop {
                tokio::select! {
                    biased;
                    () = notify.notified() => {
                        debug!(target: "fono::assistant", "drain-poll: cancelled");
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
                            break;
                        }
                    }
                }
            }
        }
    }
    Ok(any_audio)
}

/// Single-sentence helper. Synthesises and enqueues into the active
/// playback handle. Returns `true` on success.
async fn synth_and_enqueue(
    state: &Arc<Mutex<AssistantSessionState>>,
    tts: &Arc<dyn TextToSpeech>,
    sentence: &str,
    notify: &Arc<Notify>,
) -> bool {
    if sentence.trim().is_empty() {
        return false;
    }
    let synth_started = std::time::Instant::now();
    let audio = tokio::select! {
        biased;
        () = notify.notified() => {
            debug!(target: "fono::assistant", "cancelled before TTS synth");
            return false;
        }
        r = tts.synthesize(sentence, None, None) => match r {
            Ok(a) => a,
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
                return false;
            }
        },
    };
    debug!(
        target: "fono::assistant",
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
    if let Err(e) = pb.enqueue(audio.pcm, audio.sample_rate) {
        warn!(target: "fono::assistant", error = %e, "enqueue failed");
        return false;
    }
    true
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
}
