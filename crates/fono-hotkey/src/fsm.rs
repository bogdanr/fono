// SPDX-License-Identifier: GPL-3.0-only
//! Recording-state finite state machine.
//!
//! Three pipelines share one FSM: dictation (`Recording` /
//! `LiveDictating` → `Processing`), the voice assistant
//! (`AssistantRecording` → `AssistantThinking` → `AssistantSpeaking`),
//! and MCP-driven tool calls (`McpDriven`).
//! Guards keep them mutually exclusive so a stray assistant press
//! mid-dictation — or vice versa — is ignored rather than mixing
//! buffers.

use tokio::sync::mpsc;

/// Which MCP tool is currently active. Used by the `McpDriven` FSM
/// state to let the orchestrator know what to cancel on barge-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Speak,
    Listen,
    Confirm,
}

/// Events emitted to the orchestrator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    StartRecording(RecordingMode),
    StopRecording,
    Cancel,
    /// Live dictation started — orchestrator should start the streaming
    /// pipeline. Plan R7.4. Distinct from `StartRecording` so the
    /// orchestrator can branch on its config without re-deriving the
    /// mode from FSM state. The `mode` field carries the trigger
    /// (Hold/Toggle) for symmetry with [`HotkeyEvent::StartRecording`].
    StartLiveDictation(RecordingMode),
    /// Live dictation finished — orchestrator commits accumulated text
    /// and tears down the streaming pipeline.
    StopLiveDictation,
    /// Voice-assistant push-to-talk pressed. Mirrors
    /// [`HotkeyEvent::StartRecording`] but routes to the assistant
    /// pipeline (STT → chat → TTS → playback) instead of dictation.
    StartAssistant,
    /// Voice-assistant push-to-talk released. The orchestrator drains
    /// captured audio and kicks off the streaming pump.
    StopAssistant,
    /// User asked to interrupt an in-flight assistant reply (Escape
    /// while speaking, second assistant press, tray "Stop"). The
    /// orchestrator drops the audio queue and aborts the LLM stream.
    /// History is preserved.
    StopAssistantPlayback,
    /// Barge-in: the user pressed the assistant hotkey while a reply
    /// was thinking or speaking. The orchestrator must stop the
    /// in-flight reply AND immediately start a fresh assistant
    /// recording — atomically, as one step. This is deliberately a
    /// *single* event rather than `StopAssistantPlayback` followed by
    /// `StartAssistant`: the stop path emits `ProcessingDone`, which
    /// would race the new recording's `AssistantRecording` state and
    /// flip the FSM back to `Idle`, leaving the capture running with
    /// no overlay and rejecting the next press. History is preserved
    /// so the new turn carries conversation context.
    RestartAssistant,
    /// A **tap** of the assistant hotkey, while a realtime model is
    /// loaded and live mode is enabled, asked to ENTER full-duplex live
    /// conversation mode. The orchestrator tears down the nascent
    /// push-to-talk capture started on the press, opens a persistent
    /// speech-to-speech session, and begins continuous mic capture with
    /// the mute-while-speaking gate. Distinct from `StartAssistant`
    /// (the hold/PTT entry).
    EnterAssistantLive,
    /// A second tap / Escape asked to LEAVE live mode. The orchestrator
    /// closes the persistent session and stops the mic. Also reconciled
    /// by the FSM's `ProcessingDone` arm when the live pump
    /// self-terminates (idle timeout, max-duration cap, provider-side
    /// close).
    ExitAssistantLive,
    /// An MCP tool call started; the tray should show the active badge.
    McpToolStarted(ToolKind),
    /// The user barged in (F7 / F8 / Escape) while an MCP tool was
    /// active; the orchestrator should cancel the in-flight tool call.
    McpToolCancelled,
    /// The MCP tool call completed normally; return to Idle.
    McpToolDone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingMode {
    Hold,
    Toggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    /// An MCP tool call (`fono.speak`, `fono.listen`, `fono.confirm`)
    /// is in progress. The hotkey F7/F8/Escape barge-in cancels the
    /// tool call instead of starting a new dictation or assistant turn.
    McpDriven {
        tool: ToolKind,
    },
    Recording(RecordingMode),
    /// Live (streaming) dictation. The streaming pipeline is consuming
    /// audio frames and emitting preview/finalize updates. Plan R7.4 /
    /// R18.21. Reached from `Idle` via [`HotkeyAction::HoldPressed`] /
    /// [`HotkeyAction::TogglePressed`] **only when the orchestrator's
    /// runtime config has `[interactive].enabled = true`** (the
    /// orchestrator decides which start variant to dispatch).
    LiveDictating(RecordingMode),
    Processing,
    /// Voice assistant: assistant hotkey held, audio capture in progress.
    AssistantRecording,
    /// Voice assistant: STT + LLM streaming, before first audio
    /// chunk is queued for playback. Distinguishable from
    /// `AssistantSpeaking` so the tray icon / overlay can show a
    /// different shade for "thinking" vs "speaking".
    AssistantThinking,
    /// Voice assistant: TTS audio is playing back (or queued).
    AssistantSpeaking,
    /// Full-duplex live conversation mode is active: a persistent
    /// speech-to-speech session is open, the mic is streaming
    /// continuously (gated by the mute-while-speaking logic), and the
    /// model owns turn boundaries. Entered from `AssistantRecording`
    /// via [`HotkeyAction::AssistantTapped`] (the tap-release that
    /// follows the entry press) and left via a second tap, Escape, or
    /// the live pump self-terminating. The per-turn overlay colour
    /// (green/amber/blue) is driven by the orchestrator, not the FSM.
    AssistantLive,
}

/// Input actions the hotkey/ipc layers dispatch to the FSM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HotkeyAction {
    HoldPressed,
    HoldReleased,
    TogglePressed,
    CancelPressed,
    /// Orchestrator signals STT/LLM pipeline finished.
    ProcessingDone,
    /// Orchestrator signals STT/LLM pipeline started.
    ProcessingStarted,
    /// Live-dictation variants. Plan R7.4. The hotkey listener and IPC
    /// surface dispatch these instead of `HoldPressed` / `TogglePressed`
    /// when the orchestrator has enabled live mode.
    LiveHoldPressed,
    LiveHoldReleased,
    LiveTogglePressed,
    /// Voice-assistant push-to-talk pressed.
    AssistantPressed,
    /// Voice-assistant push-to-talk released.
    AssistantReleased,
    /// A **tap** (short press) of the assistant hotkey was completed.
    /// The hotkey listener emits this on the tap-release **only** when
    /// `KeyHeldFlags::assistant_live_available` is set (a realtime model
    /// is loaded and live mode is enabled); otherwise a tap keeps its
    /// legacy no-op-on-release behaviour. Drives entering/leaving the
    /// full-duplex live conversation mode.
    AssistantTapped,
    /// Orchestrator signals that the first LLM delta has arrived (not
    /// the first synthesised audio chunk — TTS roundtrip can add
    /// hundreds of ms on top of LLM TTFB, and the overlay/tray should
    /// reflect "the model has started replying" as soon as the model
    /// has actually started replying). Drives `AssistantThinking →
    /// AssistantSpeaking`.
    AssistantSpeakingStarted,
    /// MCP server started a tool call. Drives `Idle → McpDriven`.
    McpToolStarted(ToolKind),
    /// MCP tool call completed normally. Drives `McpDriven → Idle`.
    McpToolDone,
}

pub struct RecordingFsm {
    state: State,
    tx: mpsc::UnboundedSender<HotkeyEvent>,
}

impl RecordingFsm {
    #[must_use]
    pub fn new() -> (Self, mpsc::UnboundedReceiver<HotkeyEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { state: State::Idle, tx }, rx)
    }

    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    /// Apply an action; returns the new state.
    #[allow(clippy::too_many_lines)]
    pub fn dispatch(&mut self, action: HotkeyAction) -> State {
        let next = match (self.state, action) {
            (State::Idle, HotkeyAction::HoldPressed) => {
                let _ = self.tx.send(HotkeyEvent::StartRecording(RecordingMode::Hold));
                State::Recording(RecordingMode::Hold)
            }
            (State::Idle, HotkeyAction::TogglePressed) => {
                let _ = self.tx.send(HotkeyEvent::StartRecording(RecordingMode::Toggle));
                State::Recording(RecordingMode::Toggle)
            }
            (State::Recording(RecordingMode::Hold), HotkeyAction::HoldReleased) => {
                let _ = self.tx.send(HotkeyEvent::StopRecording);
                State::Processing
            }
            (State::Recording(RecordingMode::Toggle), HotkeyAction::TogglePressed) => {
                let _ = self.tx.send(HotkeyEvent::StopRecording);
                State::Processing
            }
            (State::Recording(_), HotkeyAction::CancelPressed) => {
                let _ = self.tx.send(HotkeyEvent::Cancel);
                State::Idle
            }
            (State::Idle, HotkeyAction::LiveHoldPressed) => {
                let _ = self.tx.send(HotkeyEvent::StartLiveDictation(RecordingMode::Hold));
                State::LiveDictating(RecordingMode::Hold)
            }
            (State::Idle, HotkeyAction::LiveTogglePressed) => {
                let _ = self.tx.send(HotkeyEvent::StartLiveDictation(RecordingMode::Toggle));
                State::LiveDictating(RecordingMode::Toggle)
            }
            (State::LiveDictating(RecordingMode::Hold), HotkeyAction::LiveHoldReleased) => {
                let _ = self.tx.send(HotkeyEvent::StopLiveDictation);
                State::Processing
            }
            (State::LiveDictating(RecordingMode::Toggle), HotkeyAction::LiveTogglePressed) => {
                let _ = self.tx.send(HotkeyEvent::StopLiveDictation);
                State::Processing
            }
            (State::LiveDictating(_), HotkeyAction::CancelPressed) => {
                let _ = self.tx.send(HotkeyEvent::Cancel);
                State::Idle
            }
            // Assistant entry: only from Idle. A press during dictation
            // is ignored — let the user finish that flow first.
            (State::Idle, HotkeyAction::AssistantPressed) => {
                let _ = self.tx.send(HotkeyEvent::StartAssistant);
                State::AssistantRecording
            }
            (State::AssistantRecording, HotkeyAction::AssistantReleased) => {
                let _ = self.tx.send(HotkeyEvent::StopAssistant);
                State::AssistantThinking
            }
            // Toggle-mode stop: a second AssistantPressed while
            // recording ends capture and runs the pipeline. Mirrors the
            // dictation `(Recording(Toggle), TogglePressed)` arm.
            (State::AssistantRecording, HotkeyAction::AssistantPressed) => {
                let _ = self.tx.send(HotkeyEvent::StopAssistant);
                State::AssistantThinking
            }
            // Cancel during recording: drop the buffer, return to Idle.
            (State::AssistantRecording, HotkeyAction::CancelPressed) => {
                let _ = self.tx.send(HotkeyEvent::StopAssistantPlayback);
                State::Idle
            }
            // Thinking → Speaking when first audio is queued.
            (State::AssistantThinking, HotkeyAction::AssistantSpeakingStarted) => {
                State::AssistantSpeaking
            }
            // Cancel while thinking: abort the pump, return to Idle.
            (State::AssistantThinking, HotkeyAction::CancelPressed) => {
                let _ = self.tx.send(HotkeyEvent::StopAssistantPlayback);
                State::Idle
            }
            // Cancel while speaking: drain audio, return to Idle.
            // History is preserved (Escape = "shut up", not "forget").
            (State::AssistantSpeaking, HotkeyAction::CancelPressed) => {
                let _ = self.tx.send(HotkeyEvent::StopAssistantPlayback);
                State::Idle
            }
            // Re-press the assistant hotkey mid-reply: barge-in. Stop the in-flight
            // reply (thinking or speaking) and start a new recording
            // in one atomic step. History is preserved so the next
            // turn carries context. Routing through the single
            // `RestartAssistant` event avoids the stop-path's
            // `ProcessingDone` racing the new `AssistantRecording`
            // state.
            (
                State::AssistantThinking | State::AssistantSpeaking,
                HotkeyAction::AssistantPressed,
            ) => {
                let _ = self.tx.send(HotkeyEvent::RestartAssistant);
                State::AssistantRecording
            }
            // The pump emits ProcessingDone when both stream and
            // queue are drained; either Thinking or Speaking returns
            // to Idle. AssistantRecording is also covered as a
            // safety net for the rare case where the orchestrator's
            // `on_assistant_hold_press` errored after the FSM had
            // already entered AssistantRecording.
            (
                State::AssistantRecording | State::AssistantThinking | State::AssistantSpeaking,
                HotkeyAction::ProcessingDone,
            ) => State::Idle,
            // ── Full-duplex live mode ──────────────────────────────────────
            // The entry press put us in AssistantRecording (and started a
            // nascent PTT capture). The matching tap-release converts it
            // to live mode: the orchestrator discards that capture and
            // opens the persistent session.
            (State::AssistantRecording, HotkeyAction::AssistantTapped) => {
                let _ = self.tx.send(HotkeyEvent::EnterAssistantLive);
                State::AssistantLive
            }
            // Second tap leaves live mode. The exit press is a no-op
            // (stays AssistantLive); the tap-release that follows does
            // the exit, so the gesture is symmetric with entry.
            (State::AssistantLive, HotkeyAction::AssistantTapped) => {
                let _ = self.tx.send(HotkeyEvent::ExitAssistantLive);
                State::Idle
            }
            // The exit-tap's press (and any hold while live) is a no-op:
            // live mode only leaves via the tap-release, Escape, or the
            // pump self-terminating.
            (State::AssistantLive, HotkeyAction::AssistantPressed) => State::AssistantLive,
            // Escape leaves live mode.
            (State::AssistantLive, HotkeyAction::CancelPressed) => {
                let _ = self.tx.send(HotkeyEvent::ExitAssistantLive);
                State::Idle
            }
            // The live pump self-terminated (idle timeout, max-duration
            // cap, or provider-side close): the orchestrator emits
            // ProcessingDone to return the FSM to Idle. (Teardown +
            // notify is the orchestrator's job; this is just state
            // reconciliation.)
            (State::AssistantLive, HotkeyAction::ProcessingDone) => State::Idle,
            (_, HotkeyAction::ProcessingStarted) => State::Processing,
            (State::Processing, HotkeyAction::ProcessingDone) => State::Idle,
            // ── MCP-driven tool call ──────────────────────────────────────
            // Entry: any in-flight MCP tool call signals itself via this
            // action. Only from Idle — don't interrupt dictation/assistant.
            (State::Idle, HotkeyAction::McpToolStarted(tool)) => {
                let _ = self.tx.send(HotkeyEvent::McpToolStarted(tool));
                State::McpDriven { tool }
            }
            // Barge-in: F7 / F8 / AssistantPressed / CancelPressed while an
            // MCP tool is running → cancel and return to Idle.
            (
                State::McpDriven { .. },
                HotkeyAction::CancelPressed
                | HotkeyAction::HoldPressed
                | HotkeyAction::AssistantPressed,
            ) => {
                let _ = self.tx.send(HotkeyEvent::McpToolCancelled);
                State::Idle
            }
            // Normal completion of the tool call.
            (State::McpDriven { .. }, HotkeyAction::McpToolDone) => {
                let _ = self.tx.send(HotkeyEvent::McpToolDone);
                State::Idle
            }
            (current, _) => current, // ignore invalid transitions
        };
        self.state = next;
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_flow() {
        let (mut fsm, mut rx) = RecordingFsm::new();
        assert_eq!(fsm.dispatch(HotkeyAction::HoldPressed), State::Recording(RecordingMode::Hold));
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::StartRecording(RecordingMode::Hold));
        assert_eq!(fsm.dispatch(HotkeyAction::HoldReleased), State::Processing);
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::StopRecording);
        assert_eq!(fsm.dispatch(HotkeyAction::ProcessingDone), State::Idle);
    }

    #[test]
    fn toggle_flow() {
        let (mut fsm, _rx) = RecordingFsm::new();
        assert_eq!(
            fsm.dispatch(HotkeyAction::TogglePressed),
            State::Recording(RecordingMode::Toggle)
        );
        assert_eq!(fsm.dispatch(HotkeyAction::TogglePressed), State::Processing);
    }

    #[test]
    fn cancel_while_recording() {
        let (mut fsm, _rx) = RecordingFsm::new();
        fsm.dispatch(HotkeyAction::HoldPressed);
        assert_eq!(fsm.dispatch(HotkeyAction::CancelPressed), State::Idle);
    }

    #[test]
    fn processing_ignores_new_records() {
        let (mut fsm, _rx) = RecordingFsm::new();
        fsm.dispatch(HotkeyAction::HoldPressed);
        fsm.dispatch(HotkeyAction::HoldReleased);
        assert_eq!(fsm.state(), State::Processing);
        // A fresh hold press while Processing is ignored.
        assert_eq!(fsm.dispatch(HotkeyAction::HoldPressed), State::Processing);
    }

    #[test]
    fn live_hold_flow() {
        let (mut fsm, mut rx) = RecordingFsm::new();
        assert_eq!(
            fsm.dispatch(HotkeyAction::LiveHoldPressed),
            State::LiveDictating(RecordingMode::Hold)
        );
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::StartLiveDictation(RecordingMode::Hold));
        assert_eq!(fsm.dispatch(HotkeyAction::LiveHoldReleased), State::Processing);
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::StopLiveDictation);
        assert_eq!(fsm.dispatch(HotkeyAction::ProcessingDone), State::Idle);
    }

    #[test]
    fn live_toggle_flow_and_cancel() {
        let (mut fsm, _rx) = RecordingFsm::new();
        assert_eq!(
            fsm.dispatch(HotkeyAction::LiveTogglePressed),
            State::LiveDictating(RecordingMode::Toggle)
        );
        // Cancel from live state goes back to Idle without Processing.
        assert_eq!(fsm.dispatch(HotkeyAction::CancelPressed), State::Idle);
    }

    #[test]
    fn assistant_flow_press_release_speak_done() {
        let (mut fsm, mut rx) = RecordingFsm::new();
        assert_eq!(fsm.dispatch(HotkeyAction::AssistantPressed), State::AssistantRecording);
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::StartAssistant);
        assert_eq!(fsm.dispatch(HotkeyAction::AssistantReleased), State::AssistantThinking);
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::StopAssistant);
        assert_eq!(fsm.dispatch(HotkeyAction::AssistantSpeakingStarted), State::AssistantSpeaking);
        assert_eq!(fsm.dispatch(HotkeyAction::ProcessingDone), State::Idle);
        assert!(rx.try_recv().is_err()); // no extra events
    }

    #[test]
    fn cancel_while_assistant_recording() {
        let (mut fsm, mut rx) = RecordingFsm::new();
        fsm.dispatch(HotkeyAction::AssistantPressed);
        assert_eq!(fsm.dispatch(HotkeyAction::CancelPressed), State::Idle);
        // Drain StartAssistant.
        let _ = rx.try_recv();
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::StopAssistantPlayback);
    }

    #[test]
    fn cancel_while_assistant_speaking_preserves_history() {
        let (mut fsm, _rx) = RecordingFsm::new();
        fsm.dispatch(HotkeyAction::AssistantPressed);
        fsm.dispatch(HotkeyAction::AssistantReleased);
        fsm.dispatch(HotkeyAction::AssistantSpeakingStarted);
        assert_eq!(fsm.state(), State::AssistantSpeaking);
        // Escape during speaking returns to Idle. The orchestrator
        // (not the FSM) is responsible for keeping the history.
        assert_eq!(fsm.dispatch(HotkeyAction::CancelPressed), State::Idle);
    }

    #[test]
    fn assistant_press_during_speaking_barges_in() {
        let (mut fsm, mut rx) = RecordingFsm::new();
        fsm.dispatch(HotkeyAction::AssistantPressed);
        fsm.dispatch(HotkeyAction::AssistantReleased);
        fsm.dispatch(HotkeyAction::AssistantSpeakingStarted);
        // Drain the prior events.
        while rx.try_recv().is_ok() {}
        // A fresh press while speaking → single atomic restart
        // (stop in-flight reply + start a new recording). History
        // preserved. A single event avoids the stop-path's
        // ProcessingDone racing the new AssistantRecording state.
        assert_eq!(fsm.dispatch(HotkeyAction::AssistantPressed), State::AssistantRecording);
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::RestartAssistant);
        assert!(rx.try_recv().is_err()); // no second event
    }

    #[test]
    fn assistant_press_during_thinking_barges_in() {
        let (mut fsm, mut rx) = RecordingFsm::new();
        fsm.dispatch(HotkeyAction::AssistantPressed);
        fsm.dispatch(HotkeyAction::AssistantReleased);
        assert_eq!(fsm.state(), State::AssistantThinking);
        while rx.try_recv().is_ok() {}
        // Pressing the assistant hotkey while the reply is still
        // thinking (no audio yet) also barges in.
        assert_eq!(fsm.dispatch(HotkeyAction::AssistantPressed), State::AssistantRecording);
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::RestartAssistant);
        assert!(rx.try_recv().is_err());
    }

    /// Toggle-mode assistant flow: two `AssistantPressed` events drive
    /// the FSM through the same Recording → Thinking → Speaking arc as
    /// the hold-mode press/release pair, without an `AssistantReleased`.
    #[test]
    fn assistant_toggle_flow_two_presses() {
        let (mut fsm, mut rx) = RecordingFsm::new();
        assert_eq!(fsm.dispatch(HotkeyAction::AssistantPressed), State::AssistantRecording);
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::StartAssistant);
        // Second press in toggle mode stops capture (no Released event
        // is dispatched by the listener when mode = Toggle).
        assert_eq!(fsm.dispatch(HotkeyAction::AssistantPressed), State::AssistantThinking);
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::StopAssistant);
        assert_eq!(fsm.dispatch(HotkeyAction::AssistantSpeakingStarted), State::AssistantSpeaking);
        assert_eq!(fsm.dispatch(HotkeyAction::ProcessingDone), State::Idle);
    }

    // ── Full-duplex live mode tests ──────────────────────────────────

    /// Entry gesture: the press starts a (nascent) assistant recording,
    /// and the tap-release converts it into live mode, emitting
    /// `EnterAssistantLive` exactly once.
    #[test]
    fn assistant_tap_enters_live_mode() {
        let (mut fsm, mut rx) = RecordingFsm::new();
        assert_eq!(fsm.dispatch(HotkeyAction::AssistantPressed), State::AssistantRecording);
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::StartAssistant);
        assert_eq!(fsm.dispatch(HotkeyAction::AssistantTapped), State::AssistantLive);
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::EnterAssistantLive);
        assert!(rx.try_recv().is_err());
    }

    /// Exit gesture: while live, the press is a no-op and the
    /// tap-release leaves live mode with a single `ExitAssistantLive`.
    #[test]
    fn second_tap_leaves_live_mode() {
        let (mut fsm, mut rx) = RecordingFsm::new();
        fsm.dispatch(HotkeyAction::AssistantPressed);
        fsm.dispatch(HotkeyAction::AssistantTapped);
        while rx.try_recv().is_ok() {}
        // The exit-tap press must NOT change state or emit anything.
        assert_eq!(fsm.dispatch(HotkeyAction::AssistantPressed), State::AssistantLive);
        assert!(rx.try_recv().is_err());
        // The matching tap-release exits.
        assert_eq!(fsm.dispatch(HotkeyAction::AssistantTapped), State::Idle);
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::ExitAssistantLive);
    }

    /// Escape leaves live mode.
    #[test]
    fn escape_leaves_live_mode() {
        let (mut fsm, mut rx) = RecordingFsm::new();
        fsm.dispatch(HotkeyAction::AssistantPressed);
        fsm.dispatch(HotkeyAction::AssistantTapped);
        while rx.try_recv().is_ok() {}
        assert_eq!(fsm.dispatch(HotkeyAction::CancelPressed), State::Idle);
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::ExitAssistantLive);
    }

    /// The live pump self-terminating (idle/cap/provider-close) drives
    /// the FSM back to Idle via `ProcessingDone`.
    #[test]
    fn live_processing_done_returns_to_idle() {
        let (mut fsm, mut rx) = RecordingFsm::new();
        fsm.dispatch(HotkeyAction::AssistantPressed);
        fsm.dispatch(HotkeyAction::AssistantTapped);
        while rx.try_recv().is_ok() {}
        assert_eq!(fsm.dispatch(HotkeyAction::ProcessingDone), State::Idle);
        assert!(rx.try_recv().is_err(), "ProcessingDone reconciliation emits no event");
    }

    /// A hold (PTT) is unaffected by the live transitions: without an
    /// `AssistantTapped`, press → release runs the normal PTT arc.
    #[test]
    fn assistant_hold_release_is_unchanged_by_live_arms() {
        let (mut fsm, mut rx) = RecordingFsm::new();
        assert_eq!(fsm.dispatch(HotkeyAction::AssistantPressed), State::AssistantRecording);
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::StartAssistant);
        // PTT hold-release synthesises a second AssistantPressed (the
        // listener's behaviour), which stops capture — NOT AssistantTapped.
        assert_eq!(fsm.dispatch(HotkeyAction::AssistantPressed), State::AssistantThinking);
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::StopAssistant);
    }

    #[test]
    fn assistant_pressed_during_dictation_is_ignored() {
        // Mixing the two pipelines is a hazard; the FSM rejects it
        // and the user gets the dictation flow they started.
        let (mut fsm, _rx) = RecordingFsm::new();
        fsm.dispatch(HotkeyAction::HoldPressed);
        assert_eq!(
            fsm.dispatch(HotkeyAction::AssistantPressed),
            State::Recording(RecordingMode::Hold)
        );
    }

    #[test]
    fn dictation_pressed_during_assistant_is_ignored() {
        let (mut fsm, _rx) = RecordingFsm::new();
        fsm.dispatch(HotkeyAction::AssistantPressed);
        assert_eq!(fsm.dispatch(HotkeyAction::HoldPressed), State::AssistantRecording);
    }

    // ── McpDriven tests ──────────────────────────────────────────────

    #[test]
    fn mcp_tool_start_and_done() {
        let (mut fsm, mut rx) = RecordingFsm::new();
        assert_eq!(
            fsm.dispatch(HotkeyAction::McpToolStarted(ToolKind::Speak)),
            State::McpDriven { tool: ToolKind::Speak }
        );
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::McpToolStarted(ToolKind::Speak));
        assert_eq!(fsm.dispatch(HotkeyAction::McpToolDone), State::Idle);
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::McpToolDone);
    }

    #[test]
    fn mcp_barge_in_via_cancel() {
        let (mut fsm, mut rx) = RecordingFsm::new();
        fsm.dispatch(HotkeyAction::McpToolStarted(ToolKind::Listen));
        let _ = rx.try_recv(); // drain McpToolStarted
        assert_eq!(fsm.dispatch(HotkeyAction::CancelPressed), State::Idle);
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::McpToolCancelled);
    }

    #[test]
    fn mcp_barge_in_via_hold() {
        let (mut fsm, mut rx) = RecordingFsm::new();
        fsm.dispatch(HotkeyAction::McpToolStarted(ToolKind::Confirm));
        let _ = rx.try_recv();
        // F7 (HoldPressed) while MCP is active → barge-in, not dictation
        assert_eq!(fsm.dispatch(HotkeyAction::HoldPressed), State::Idle);
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::McpToolCancelled);
    }

    #[test]
    fn mcp_does_not_interrupt_dictation() {
        // An MCP start while dictation is in progress is ignored.
        let (mut fsm, _rx) = RecordingFsm::new();
        fsm.dispatch(HotkeyAction::HoldPressed);
        assert_eq!(
            fsm.dispatch(HotkeyAction::McpToolStarted(ToolKind::Speak)),
            State::Recording(RecordingMode::Hold)
        );
    }
}
