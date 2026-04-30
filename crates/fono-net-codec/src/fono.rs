// SPDX-License-Identifier: GPL-3.0-only
//! Fono-native protocol event types.
//!
//! Carried over WebSocket binary messages (one [`crate::Frame`] per
//! message) so the same protocol is usable from a browser tab without
//! a redesign. The Frame body is identical to Wyoming's, only the I/O
//! glue differs.
//!
//! Event tags are namespaced under `fono.*` so a stray Fono event sent
//! to a strict Wyoming peer is rejected at the connection-arm allow-
//! list rather than confusing the peer.
//!
//! For STT-equivalent traffic (audio + transcribe + transcript) we
//! reuse the Wyoming event types directly — see
//! [`crate::wyoming`] — so a Fono-native server can host both LLM
//! cleanup and STT on the same WebSocket connection.

use serde::{Deserialize, Serialize};

pub const HELLO: &str = "fono.hello";
pub const HELLO_ACK: &str = "fono.hello-ack";
pub const BYE: &str = "fono.bye";
pub const CLEANUP_REQUEST: &str = "fono.cleanup-request";
pub const CLEANUP_RESPONSE: &str = "fono.cleanup-response";
pub const CLEANUP_CHUNK: &str = "fono.cleanup-chunk";
pub const HISTORY_APPEND: &str = "fono.history-append";
pub const CONTEXT: &str = "fono.context";
pub const ERROR: &str = "fono.error";
pub const PING: &str = "fono.ping";
pub const PONG: &str = "fono.pong";

/// First event a client sends after the WebSocket upgrade. Carries
/// the auth token (if any) and the capabilities the client wants to
/// use; server replies with [`HelloAck`] or closes with an [`Error`]
/// of code `AUTH`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hello {
    pub client_version: String,
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
}

/// Server's reply to [`Hello`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelloAck {
    pub server_version: String,
    pub capabilities: Vec<String>,
    pub session_id: String,
}

/// Graceful close.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Bye {
    pub reason: String,
}

/// Window / app metadata used by hover-context cleanup rules.
/// Optional: clients that don't track focus simply omit the
/// [`CleanupRequest::app_context`] field.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
}

/// Ask the server's configured LLM to clean a raw transcript.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CleanupRequest {
    /// Client-chosen correlation id. Echoed in [`CleanupResponse::id`]
    /// and any [`CleanupChunk::id`].
    pub id: String,
    pub raw_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_context: Option<AppContext>,
}

/// Final cleaned-text reply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CleanupResponse {
    pub id: String,
    pub cleaned_text: String,
    /// Backend the server actually ran (e.g. `"local"`, `"groq"`).
    pub source_backend: String,
}

/// Streaming partial — reserved for a follow-up slice. Servers that
/// don't stream just emit a single [`CleanupResponse`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CleanupChunk {
    pub id: String,
    pub delta: String,
}

/// Mirror a history row from client to server. Only honoured when the
/// server has `mirror_history = true`; otherwise dropped silently.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryAppend {
    pub id: String,
    pub raw: String,
    pub cleaned: String,
    /// Unix epoch seconds.
    pub ts: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_context: Option<AppContext>,
}

/// Update the server's view of the client's current focused app.
/// Sent in addition to (not instead of)
/// [`CleanupRequest::app_context`] when the cleanup pipeline is
/// pre-warmed without a request in flight.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Context {
    pub app_context: AppContext,
}

/// Generic error carrier. `code` is a short uppercase tag so clients
/// can branch on it; `message` is a human-readable diagnostic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Error {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

/// Liveness probe. Servers MUST echo the same nonce in [`Pong`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ping {
    pub nonce: u64,
}

/// Reply to [`Ping`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Pong {
    pub nonce: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Frame;
    use serde_json::to_value;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn hello_and_cleanup_round_trip() {
        let hello = Hello {
            client_version: "0.4.0".into(),
            capabilities: vec!["cleanup".into(), "history-mirror".into()],
            auth_token: Some("REDACTED".into()),
        };
        let f = Frame::new(HELLO).with_data(to_value(&hello).unwrap());
        let mut buf: Vec<u8> = Vec::new();
        f.write_async(&mut buf).await.unwrap();
        let mut reader = BufReader::new(buf.as_slice());
        let parsed = Frame::read_async(&mut reader).await.unwrap();
        assert_eq!(parsed.kind, HELLO);
        let back: Hello = serde_json::from_value(parsed.data).unwrap();
        assert_eq!(back, hello);
    }

    #[test]
    fn cleanup_request_omits_optional_fields() {
        let req = CleanupRequest {
            id: "req-1".into(),
            raw_text: "hello world".into(),
            language: None,
            app_context: None,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert!(v.get("language").is_none());
        assert!(v.get("app_context").is_none());
    }

    #[test]
    fn error_default_retryable_is_false() {
        let json = serde_json::json!({
            "code": "AUTH",
            "message": "bad token"
        });
        let err: Error = serde_json::from_value(json).unwrap();
        assert!(!err.retryable);
    }
}
