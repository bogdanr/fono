// SPDX-License-Identifier: GPL-3.0-only
//! Recording-state finite state machine.
//!
//! States: `Idle` -> `Recording(hold|toggle)` -> `Processing` -> `Idle`.
//! Guards prevent re-entry while processing. Hotkey events map to actions.

use tokio::sync::mpsc;

/// Events emitted to the orchestrator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    StartRecording(RecordingMode),
    StopRecording,
    Cancel,
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
    Processing,
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
}
