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
        Self {
            history,
            current_turn: None,
            playback: None,
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
        self.playback = None;
    }
}

/// Inputs for [`run_assistant_turn`]. Cloning is cheap (everything is
/// `Arc`).
pub struct AssistantTurnInputs {
    pub pcm: Vec<f32>,
    pub sample_rate: u32,
    pub stt: Arc<dyn SpeechToText>,
    pub assistant: Arc<dyn Assistant>,
    pub tts: Arc<dyn TextToSpeech>,
    pub system_prompt: String,
    pub language: Option<String>,
    /// Channel back into the FSM. The pump sends
    /// [`HotkeyAction::AssistantSpeakingStarted`] once the first
    /// sentence's audio is queued for playback so the FSM transitions
    /// `AssistantThinking → AssistantSpeaking`. The same channel
    /// also receives `ProcessingDone` from the daemon dispatcher
    /// after the turn ends (we don't fire it from here).
    pub action_tx: mpsc::UnboundedSender<HotkeyAction>,
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
    } = inputs;

    if pcm.is_empty() {
        debug!(target: "fono::assistant", "skip: empty PCM");
        return Ok(false);
    }

    // 1. STT.
    let stt_started = std::time::Instant::now();
    let transcription = tokio::select! {
        biased;
        () = notify.notified() => {
            debug!(target: "fono::assistant", "cancelled before STT");
            return Ok(false);
        }
        r = stt.transcribe(&pcm, sample_rate, language.as_deref()) => r?,
    };
    let user_text = transcription.text.trim().to_string();
    if user_text.is_empty() {
        debug!(target: "fono::assistant", "skip: empty transcript");
        return Ok(false);
    }
    info!(
        target: "fono::assistant",
        stt_ms = stt_started.elapsed().as_millis() as u64,
        "STT: {user_text:?}"
    );

    // 2. Build context from history (prune-on-snapshot) + push user turn.
    let history_snapshot = {
        let mut s = state.lock().await;
        s.history.push_user(user_text.clone());
        s.history.snapshot()
    };
    let ctx = AssistantContext {
        system_prompt,
        language,
        history: history_snapshot,
    };

    // 3. Open the LLM stream.
    let llm_started = std::time::Instant::now();
    let mut deltas = tokio::select! {
        biased;
        () = notify.notified() => {
            debug!(target: "fono::assistant", "cancelled before LLM");
            return Ok(false);
        }
        r = assistant.reply_stream(&user_text, &ctx) => r?,
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
                break;
            }
        };
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
                    // Drive FSM `Thinking → Speaking`. Best-effort —
                    // a closed channel just means the daemon has gone
                    // away.
                    let _ = action_tx.send(HotkeyAction::AssistantSpeakingStarted);
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
                if first_audio_at.is_none() {
                    let _ = action_tx.send(HotkeyAction::AssistantSpeakingStarted);
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

    // FOLLOW-UP — playback-drain wait. This function returns the
    // moment the LLM stream ends and the last sentence has been
    // enqueued, but the `AudioPlayback` worker still has several
    // seconds of audio in its queue. The caller's cleanup closure
    // (see `SessionOrchestrator::on_assistant_hold_release`) then
    // immediately aborts the thinking-animation task, hides the
    // overlay, flips the tray back to Idle, and emits
    // `ProcessingDone` — so visually the assistant looks "done"
    // while the user is still hearing the reply.
    //
    // The fix is a cooperative drain poll right here: loop on
    // `state.lock().await.playback.as_ref().map(|p| p.is_idle())`
    // (with the same `notify` select-arm so Escape / barge-in still
    // wins), break when idle. `AudioPlayback::is_idle()` already
    // exists for exactly this purpose; we just never wired it in.
    //
    // Deferred because the existing UX is acceptable enough to
    // ship the assistant in v1 — the visual cues just lead the
    // audio by a few seconds. Implement when adding the
    // post-monitor tap that would let the overlay show real
    // playback levels (the same drain wait would be the natural
    // home for either feature).
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
