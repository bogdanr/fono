// SPDX-License-Identifier: GPL-3.0-only
//! fono-mcp-server — MCP server exposing Fono voice tools to coding agents.
//!
//! Serves three tools over stdio JSON-RPC:
//! - `fono.speak` — synthesise and play text via the configured TTS backend.
//! - `fono.listen` — record voice until silence; return transcript.
//! - `fono.confirm` — ask A/B/C question by voice; return matched choice.
//!
//! Start with `fono mcp serve` (requires `fono use mcp-server on` first).

pub mod protocol;
pub mod relevance;
pub mod server;
pub mod summarize;
pub mod tools;
pub mod transport;
pub mod voice_io;

pub use protocol::{ContentBlock, ToolCallResult};
pub use server::McpServer;
pub use tools::{McpContext, PolishClassifierCache, ToolRegistry};
pub use transport::{ServerTransport, StdioTransport};
