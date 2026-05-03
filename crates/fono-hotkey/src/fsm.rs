// SPDX-License-Identifier: GPL-3.0-only
//! Recording-state finite state machine.
//!
//! Two pipelines share one FSM: dictation (`Recording` /
//! `LiveDictating` → `Processing`) and the voice assistant
//! (`AssistantRecording` → `AssistantThinking` → `AssistantSpeaking`).
//! Guards keep them mutually exclusive so a stray F10 mid-dictation —
//! or vice versa — is ignored rather than mixing buffers.

use tokio::sync::mpsc;

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
    /// while speaking, second F10 press, tray "Stop"). The
    /// orchestrator drops the audio queue and aborts the LLM stream.
    /// History is preserved.
    StopAssistantPlayback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingMode {
    Hold,
    Toggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Recording(RecordingMode),
    /// Live (streaming) dictation. The streaming pipeline is consuming
    /// audio frames and emitting preview/finalize updates. Plan R7.4 /
    /// R18.21. Reached from `Idle` via [`HotkeyAction::HoldPressed`] /
    /// [`HotkeyAction::TogglePressed`] **only when the orchestrator's
    /// runtime config has `[interactive].enabled = true`** (the
    /// orchestrator decides which start variant to dispatch).
    LiveDictating(RecordingMode),
    Processing,
    /// Voice assistant: F10 held, audio capture in progress.
    AssistantRecording,
    /// Voice assistant: STT + LLM streaming, before first audio
    /// chunk is queued for playback. Distinguishable from
    /// `AssistantSpeaking` so the tray icon / overlay can show a
    /// different shade for "thinking" vs "speaking".
    AssistantThinking,
    /// Voice assistant: TTS audio is playing back (or queued).
    AssistantSpeaking,
}

/// Input actions the hotkey/ipc layers dispatch to the FSM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Orchestrator signals the first TTS audio chunk has been queued
    /// for playback. Drives `AssistantThinking → AssistantSpeaking`.
    AssistantSpeakingStarted,
}

pub struct RecordingFsm {
    state: State,
    tx: mpsc::UnboundedSender<HotkeyEvent>,
}

impl RecordingFsm {
    #[must_use]
    pub fn new() -> (Self, mpsc::UnboundedReceiver<HotkeyEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                state: State::Idle,
                tx,
            },
            rx,
        )
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
                let _ = self
                    .tx
                    .send(HotkeyEvent::StartRecording(RecordingMode::Hold));
                State::Recording(RecordingMode::Hold)
            }
            (State::Idle, HotkeyAction::TogglePressed) => {
                let _ = self
                    .tx
                    .send(HotkeyEvent::StartRecording(RecordingMode::Toggle));
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
                let _ = self
                    .tx
                    .send(HotkeyEvent::StartLiveDictation(RecordingMode::Hold));
                State::LiveDictating(RecordingMode::Hold)
            }
            (State::Idle, HotkeyAction::LiveTogglePressed) => {
                let _ = self
                    .tx
                    .send(HotkeyEvent::StartLiveDictation(RecordingMode::Toggle));
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
            // Re-press F10 mid-reply: barge-in. Stop the current
            // playback, start a new recording. History is preserved
            // so the next turn carries context.
            (State::AssistantSpeaking, HotkeyAction::AssistantPressed) => {
                let _ = self.tx.send(HotkeyEvent::StopAssistantPlayback);
                let _ = self.tx.send(HotkeyEvent::StartAssistant);
                State::AssistantRecording
            }
            // The pump emits ProcessingDone when both stream and
            // queue are drained; either Thinking or Speaking returns
            // to Idle.
            (
                State::AssistantThinking | State::AssistantSpeaking,
                HotkeyAction::ProcessingDone,
            ) => State::Idle,
            (_, HotkeyAction::ProcessingStarted) => State::Processing,
            (State::Processing, HotkeyAction::ProcessingDone) => State::Idle,
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
        assert_eq!(
            fsm.dispatch(HotkeyAction::HoldPressed),
            State::Recording(RecordingMode::Hold)
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            HotkeyEvent::StartRecording(RecordingMode::Hold)
        );
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
        assert_eq!(
            rx.try_recv().unwrap(),
            HotkeyEvent::StartLiveDictation(RecordingMode::Hold)
        );
        assert_eq!(
            fsm.dispatch(HotkeyAction::LiveHoldReleased),
            State::Processing
        );
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
        assert_eq!(
            fsm.dispatch(HotkeyAction::AssistantPressed),
            State::AssistantRecording
        );
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::StartAssistant);
        assert_eq!(
            fsm.dispatch(HotkeyAction::AssistantReleased),
            State::AssistantThinking
        );
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::StopAssistant);
        assert_eq!(
            fsm.dispatch(HotkeyAction::AssistantSpeakingStarted),
            State::AssistantSpeaking
        );
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
        // A fresh press while speaking → stop playback + start
        // recording (history preserved).
        assert_eq!(
            fsm.dispatch(HotkeyAction::AssistantPressed),
            State::AssistantRecording
        );
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::StopAssistantPlayback);
        assert_eq!(rx.try_recv().unwrap(), HotkeyEvent::StartAssistant);
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
        assert_eq!(
            fsm.dispatch(HotkeyAction::HoldPressed),
            State::AssistantRecording
        );
    }
}
