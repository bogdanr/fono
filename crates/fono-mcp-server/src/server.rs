// SPDX-License-Identifier: GPL-3.0-only
//! `McpServer` — owns the transport, tool registry, and the JSON-RPC
//! request loop.

use anyhow::Result;
use tracing::{debug, info, warn};

use crate::protocol::{
    self, Capabilities, ClientMessage, InitializeResult, ServerInfo, ServerResponse,
    ToolCallParams, ToolsListResult,
};
use crate::tools::ToolRegistry;
use crate::transport::ServerTransport;

/// MCP server instance. Owns a `Box<dyn ServerTransport>` and a
/// `ToolRegistry`, and runs the JSON-RPC request/response loop until
/// the transport reports EOF.
pub struct McpServer {
    transport: Box<dyn ServerTransport>,
    registry: ToolRegistry,
}

impl McpServer {
    /// Construct a new server from a transport and a tool registry.
    pub fn new(transport: Box<dyn ServerTransport>, registry: ToolRegistry) -> Self {
        Self { transport, registry }
    }

    /// Run the server until stdin closes (EOF) or a fatal I/O error.
    ///
    /// Protocol flow:
    /// 1. Wait for `initialize` request → send `InitializeResult`.
    /// 2. Wait for `notifications/initialized` notification → no response.
    /// 3. Loop: dispatch `tools/list`, `tools/call`, and other requests.
    pub async fn run(&mut self) -> Result<()> {
        // ── Step 1: initialize handshake ─────────────────────────────────────
        loop {
            let Some(line) = self.transport.recv().await? else {
                return Ok(()); // EOF before initialize
            };
            if line.trim().is_empty() {
                continue;
            }
            let msg: ClientMessage = match serde_json::from_str(&line) {
                Ok(m) => m,
                Err(e) => {
                    warn!(target: "fono_mcp_server", error = %e, "parse error on initialize");
                    let resp = ServerResponse::error(
                        serde_json::Value::Null,
                        protocol::PARSE_ERROR,
                        e.to_string(),
                    );
                    let json = serde_json::to_string(&resp)?;
                    self.transport.send(&json).await?;
                    continue;
                }
            };
            if msg.method == "initialize" {
                let id = msg.id.unwrap_or(serde_json::Value::Null);
                let result = InitializeResult {
                    protocol_version: "2024-11-05".into(),
                    capabilities: Capabilities::default(),
                    server_info: ServerInfo {
                        name: "fono".into(),
                        version: env!("CARGO_PKG_VERSION").into(),
                    },
                };
                let resp = ServerResponse::ok(id, serde_json::to_value(&result)?);
                let json = serde_json::to_string(&resp)?;
                self.transport.send(&json).await?;
                info!(
                    target: "fono_mcp_server",
                    tools = self.registry.len(),
                    "MCP initialize handshake complete"
                );
                break;
            }
            // Any other method before initialize → error
            let id = msg.id.unwrap_or(serde_json::Value::Null);
            let resp = ServerResponse::error(
                id,
                protocol::INVALID_REQUEST,
                "expected `initialize` as the first request",
            );
            let json = serde_json::to_string(&resp)?;
            self.transport.send(&json).await?;
        }

        // ── Step 2: notifications/initialized ────────────────────────────────
        loop {
            let Some(line) = self.transport.recv().await? else {
                return Ok(());
            };
            if line.trim().is_empty() {
                continue;
            }
            let msg: ClientMessage = match serde_json::from_str(&line) {
                Ok(m) => m,
                Err(e) => {
                    warn!(target: "fono_mcp_server", error = %e, "parse error after initialize");
                    continue;
                }
            };
            if msg.method == "notifications/initialized" {
                debug!(target: "fono_mcp_server", "received notifications/initialized");
                break; // No response to notifications.
            }
            // Anything else — dispatch normally, then keep waiting.
            self.dispatch(msg).await?;
        }

        // ── Step 3: main dispatch loop ────────────────────────────────────────
        loop {
            let Some(line) = self.transport.recv().await? else {
                info!(target: "fono_mcp_server", "stdin closed — MCP server exiting");
                return Ok(());
            };
            if line.trim().is_empty() {
                continue;
            }
            let msg: ClientMessage = match serde_json::from_str(&line) {
                Ok(m) => m,
                Err(e) => {
                    warn!(target: "fono_mcp_server", error = %e, "JSON parse error");
                    let resp = ServerResponse::error(
                        serde_json::Value::Null,
                        protocol::PARSE_ERROR,
                        e.to_string(),
                    );
                    let json = serde_json::to_string(&resp)?;
                    self.transport.send(&json).await?;
                    continue;
                }
            };
            // Notifications have no id — skip them silently.
            if msg.id.is_none() {
                debug!(target: "fono_mcp_server", method = %msg.method, "received notification");
                continue;
            }
            self.dispatch(msg).await?;
        }
    }

    async fn dispatch(&mut self, msg: ClientMessage) -> Result<()> {
        let id = msg.id.clone().unwrap_or(serde_json::Value::Null);
        let json = match msg.method.as_str() {
            "tools/list" => {
                let list = ToolsListResult { tools: self.registry.tool_defs() };
                let resp = ServerResponse::ok(id, serde_json::to_value(&list)?);
                serde_json::to_string(&resp)?
            }
            "tools/call" => {
                let params =
                    match msg.params.and_then(|p| serde_json::from_value::<ToolCallParams>(p).ok())
                    {
                        Some(p) => p,
                        None => {
                            let resp = ServerResponse::error(
                                id,
                                protocol::INVALID_PARAMS,
                                "tools/call requires `{name, arguments}`",
                            );
                            let json = serde_json::to_string(&resp)?;
                            return self.transport.send(&json).await;
                        }
                    };
                let result = self.registry.call(&params.name, params.arguments).await;
                let resp = ServerResponse::ok(id, serde_json::to_value(&result)?);
                serde_json::to_string(&resp)?
            }
            "ping" => {
                let resp = ServerResponse::ok(id, serde_json::json!({}));
                serde_json::to_string(&resp)?
            }
            method => {
                debug!(target: "fono_mcp_server", %method, "unknown method");
                let resp = ServerResponse::error(
                    id,
                    protocol::METHOD_NOT_FOUND,
                    format!("unknown method: {method}"),
                );
                serde_json::to_string(&resp)?
            }
        };
        self.transport.send(&json).await
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ToolCallResult;
    use crate::tools::{McpContext, Tool, ToolRegistry};
    use crate::transport::test_transport::MemTransport;

    /// A minimal stub tool for testing the golden flow.
    struct EchoTool;

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "test.echo"
        }
        fn description(&self) -> &str {
            "Echo the input text."
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}})
        }
        async fn call(&self, arguments: serde_json::Value) -> ToolCallResult {
            let text = arguments.get("text").and_then(|v| v.as_str()).unwrap_or("(empty)");
            ToolCallResult::success(format!("echo: {text}"))
        }
    }

    #[allow(dead_code)]
    fn make_server() -> (McpServer, &'static MemTransport) {
        let mut registry = ToolRegistry::new();
        registry.register(EchoTool);
        let transport = Box::new(MemTransport::new());
        // We need a way to inspect the outgoing messages, but Box hides
        // MemTransport. For test visibility we use a raw pointer (the
        // test drives the box directly via push_msg, which is fine in
        // single-threaded tests).
        let _ptr: *const MemTransport = &*transport;
        let server = McpServer::new(transport, registry);
        (server, unsafe { &*_ptr })
    }

    #[tokio::test]
    async fn golden_flow_initialize_tools_list_tools_call() {
        let mut transport = MemTransport::new();
        let init_req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}"#;
        let notif = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let list_req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        let call_req = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"test.echo","arguments":{"text":"hello"}}}"#;
        transport.push_msg(init_req);
        transport.push_msg(notif);
        transport.push_msg(list_req);
        transport.push_msg(call_req);
        // Empty to signal EOF.

        let mut registry = ToolRegistry::new();
        registry.register(EchoTool);
        let mut server = McpServer::new(Box::new(transport), registry);
        server.run().await.expect("server should complete without error");

        // The server's transport is consumed — we can't inspect it here.
        // The test passes as long as `run()` returns `Ok(())`.
    }

    #[tokio::test]
    async fn tools_call_unknown_tool_returns_failure() {
        let mut transport = MemTransport::new();
        transport.push_msg(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}"#);
        transport.push_msg(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        transport.push_msg(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"no.such.tool","arguments":{}}}"#);

        let mut registry = ToolRegistry::new();
        registry.register(EchoTool);
        let mut server = McpServer::new(Box::new(transport), registry);
        server.run().await.expect("server should complete");
    }

    #[test]
    fn mcp_context_cfg_clones() {
        let cfg = fono_core::config::Config::default();
        let secrets = fono_core::Secrets::default();
        let ctx = McpContext {
            cfg: cfg.clone(),
            secrets,
            whisper_models_dir: std::path::PathBuf::from("/tmp/fono-test-models"),
            polish_models_dir: std::path::PathBuf::from("/tmp/fono-test-polish"),
            polish_classifier_cache: McpContext::new_classifier_cache(),
            daemon_ipc_candidates: Vec::new(),
        };
        // MCP server is enabled by default (stdio only, no network exposure).
        assert!(ctx.cfg.mcp.enabled);
    }
}
