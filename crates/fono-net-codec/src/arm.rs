// SPDX-License-Identifier: GPL-3.0-only
//! Connection-arm allow-lists.
//!
//! Each connection plays one of four roles. The role determines which
//! event tags are *expected to be received* on that wire. A stray
//! event from the other protocol (e.g. a Fono-native `fono.cleanup-
//! request` arriving over a Wyoming connection) gets rejected at parse
//! time before reaching application code, eliminating a class of
//! cross-protocol confusion bugs.
//!
//! The allow-list is a receive-side contract; senders are responsible
//! for only emitting events the peer's arm will accept.

use crate::{fono, wyoming};

/// Role of a connection at one of its endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arm {
    /// Fono is acting as a Wyoming **client** (we open the connection
    /// to a Wyoming-protocol server somewhere on the LAN).
    WyomingClient,
    /// Fono is acting as a Wyoming **server** (we accept inbound
    /// connections from Wyoming-protocol clients, e.g. Home Assistant).
    WyomingServer,
    /// Fono is acting as a Fono-native **client** (we open a WebSocket
    /// upgrade to another Fono daemon's `/fono/v1` endpoint).
    FonoClient,
    /// Fono is acting as a Fono-native **server** (we accept inbound
    /// WebSocket connections at `/fono/v1`).
    FonoServer,
}

impl Arm {
    /// Returns true if `kind` is a frame this arm should accept on
    /// the wire. The check is intentionally exhaustive on the
    /// upstream Wyoming spec subset we support; unknown event tags
    /// are rejected.
    #[must_use]
    pub fn accepts(self, kind: &str) -> bool {
        match self {
            // Wyoming clients receive describe responses + transcripts.
            Self::WyomingClient => matches!(
                kind,
                wyoming::INFO
                    | wyoming::TRANSCRIPT
                    | wyoming::TRANSCRIPT_START
                    | wyoming::TRANSCRIPT_CHUNK
                    | wyoming::TRANSCRIPT_STOP
            ),
            // Wyoming servers receive describe requests + audio + a
            // transcribe trigger.
            Self::WyomingServer => matches!(
                kind,
                wyoming::DESCRIBE
                    | wyoming::AUDIO_START
                    | wyoming::AUDIO_CHUNK
                    | wyoming::AUDIO_STOP
                    | wyoming::TRANSCRIBE
            ),
            // Fono-native clients receive the cleanup half of the
            // protocol *and* the STT-equivalent transcript stream
            // when the server is hosting both.
            Self::FonoClient => matches!(
                kind,
                fono::HELLO_ACK
                    | fono::BYE
                    | fono::CLEANUP_RESPONSE
                    | fono::CLEANUP_CHUNK
                    | fono::ERROR
                    | fono::PONG
                    | wyoming::TRANSCRIPT
                    | wyoming::TRANSCRIPT_START
                    | wyoming::TRANSCRIPT_CHUNK
                    | wyoming::TRANSCRIPT_STOP
            ),
            // Fono-native servers receive the request half of the
            // protocol *plus* the STT inputs (audio + transcribe)
            // since one connection covers both modalities.
            Self::FonoServer => matches!(
                kind,
                fono::HELLO
                    | fono::BYE
                    | fono::CLEANUP_REQUEST
                    | fono::HISTORY_APPEND
                    | fono::CONTEXT
                    | fono::PING
                    | wyoming::AUDIO_START
                    | wyoming::AUDIO_CHUNK
                    | wyoming::AUDIO_STOP
                    | wyoming::TRANSCRIBE
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wyoming_client_rejects_fono_events() {
        assert!(!Arm::WyomingClient.accepts(fono::CLEANUP_RESPONSE));
        assert!(!Arm::WyomingClient.accepts(fono::HELLO_ACK));
        assert!(Arm::WyomingClient.accepts(wyoming::TRANSCRIPT));
    }

    #[test]
    fn wyoming_server_rejects_fono_events() {
        assert!(!Arm::WyomingServer.accepts(fono::CLEANUP_REQUEST));
        assert!(Arm::WyomingServer.accepts(wyoming::AUDIO_CHUNK));
        assert!(Arm::WyomingServer.accepts(wyoming::TRANSCRIBE));
    }

    #[test]
    fn fono_client_accepts_both_cleanup_and_transcript() {
        assert!(Arm::FonoClient.accepts(fono::CLEANUP_RESPONSE));
        assert!(Arm::FonoClient.accepts(wyoming::TRANSCRIPT_CHUNK));
        // But never the request half.
        assert!(!Arm::FonoClient.accepts(fono::CLEANUP_REQUEST));
        assert!(!Arm::FonoClient.accepts(wyoming::AUDIO_CHUNK));
    }

    #[test]
    fn fono_server_accepts_request_half_and_audio() {
        assert!(Arm::FonoServer.accepts(fono::CLEANUP_REQUEST));
        assert!(Arm::FonoServer.accepts(fono::HELLO));
        assert!(Arm::FonoServer.accepts(wyoming::AUDIO_START));
        // But never reply-side events.
        assert!(!Arm::FonoServer.accepts(fono::CLEANUP_RESPONSE));
        assert!(!Arm::FonoServer.accepts(wyoming::TRANSCRIPT));
    }

    #[test]
    fn unknown_tags_are_rejected_by_every_arm() {
        for arm in [
            Arm::WyomingClient,
            Arm::WyomingServer,
            Arm::FonoClient,
            Arm::FonoServer,
        ] {
            assert!(!arm.accepts("not-a-real-event"));
            assert!(!arm.accepts("fono.future-event"));
        }
    }
}
