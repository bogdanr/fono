// SPDX-License-Identifier: GPL-3.0-only
//! Transport layer for the MCP stdio server.
//!
//! **Critical:** all tracing / logging must go to stderr. stdout is the
//! exclusive MCP JSON-RPC channel and must remain clean.

use anyhow::Result;
use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

// ── Trait ────────────────────────────────────────────────────────────────────

/// Async transport over which the MCP server exchanges messages.
///
/// `recv` returns the next raw JSON string (one per line), or `None` when
/// the connection closes. `send` / `send_notification` write a single
/// JSON line to the peer.
#[async_trait]
pub trait ServerTransport: Send + Sync {
    /// Receive the next JSON-RPC message line. Returns `None` on EOF.
    async fn recv(&mut self) -> Result<Option<String>>;

    /// Send a serialised JSON-RPC response line.
    async fn send(&mut self, json: &str) -> Result<()>;
}

// ── Stdio implementation ──────────────────────────────────────────────────────

/// MCP transport that reads from `stdin` and writes to `stdout`.
///
/// Stdout is reserved exclusively for JSON-RPC frames; the global
/// tracing subscriber configured in `crates/fono/src/main.rs` writes
/// to stderr so log output never contaminates the MCP channel.
pub struct StdioTransport {
    stdin: BufReader<tokio::io::Stdin>,
    stdout: tokio::io::Stdout,
}

impl StdioTransport {
    /// Build a new `StdioTransport`.
    pub fn new() -> Self {
        Self { stdin: BufReader::new(tokio::io::stdin()), stdout: tokio::io::stdout() }
    }
}

impl Default for StdioTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ServerTransport for StdioTransport {
    async fn recv(&mut self) -> Result<Option<String>> {
        let mut line = String::new();
        let n = self.stdin.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let trimmed = line.trim_end_matches(['\n', '\r']).to_string();
        Ok(Some(trimmed))
    }

    async fn send(&mut self, json: &str) -> Result<()> {
        self.stdout.write_all(json.as_bytes()).await?;
        self.stdout.write_all(b"\n").await?;
        self.stdout.flush().await?;
        Ok(())
    }
}

// ── In-memory transport (for unit tests) ─────────────────────────────────────

/// In-memory transport for tests. Push messages into `incoming`; sent
/// responses accumulate in `outgoing`.
#[cfg(test)]
pub mod test_transport {
    use std::collections::VecDeque;

    use super::*;
    use crate::protocol::ServerResponse;

    pub struct MemTransport {
        pub incoming: VecDeque<String>,
        pub outgoing: Vec<String>,
    }

    impl Default for MemTransport {
        fn default() -> Self {
            Self::new()
        }
    }

    impl MemTransport {
        pub fn new() -> Self {
            Self { incoming: VecDeque::new(), outgoing: Vec::new() }
        }

        pub fn push_msg(&mut self, msg: &str) {
            self.incoming.push_back(msg.to_string());
        }

        /// Collect the outgoing responses and deserialize them.
        pub fn responses(&self) -> Vec<ServerResponse> {
            self.outgoing.iter().filter_map(|s| serde_json::from_str(s).ok()).collect()
        }
    }

    #[async_trait]
    impl ServerTransport for MemTransport {
        async fn recv(&mut self) -> Result<Option<String>> {
            Ok(self.incoming.pop_front())
        }

        async fn send(&mut self, json: &str) -> Result<()> {
            self.outgoing.push(json.to_string());
            Ok(())
        }
    }
}
