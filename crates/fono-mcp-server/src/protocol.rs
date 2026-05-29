// SPDX-License-Identifier: GPL-3.0-only
//! JSON-RPC 2.0 wire types for the MCP (Model Context Protocol) server.
//!
//! Covers the subset of MCP needed by `fono mcp serve`:
//! `initialize`, `initialized`, `tools/list`, `tools/call`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Error codes ──────────────────────────────────────────────────────────────

pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

// ── Client → Server ──────────────────────────────────────────────────────────

/// A client message — either a JSON-RPC request (has `id`) or a
/// notification (no `id`). Unified so the dispatcher handles both.
#[derive(Debug, Clone, Deserialize)]
pub struct ClientMessage {
    pub jsonrpc: String,
    /// `None` for notifications (e.g. `notifications/initialized`).
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

// ── Server → Client ──────────────────────────────────────────────────────────

/// JSON-RPC 2.0 response (result or error).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl ServerResponse {
    /// Successful response.
    pub fn ok(id: Value, result: Value) -> Self {
        Self { jsonrpc: "2.0".into(), id, result: Some(result), error: None }
    }

    /// Error response.
    pub fn error(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError { code, message: message.into(), data: None }),
        }
    }
}

/// JSON-RPC 2.0 server-initiated notification (no `id`).
#[derive(Debug, Clone, Serialize)]
pub struct ServerNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl ServerNotification {
    pub fn new(method: impl Into<String>) -> Self {
        Self { jsonrpc: "2.0".into(), method: method.into(), params: None }
    }
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// ── MCP-specific request / response shapes ───────────────────────────────────

/// `initialize` request params.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: String,
    #[serde(default)]
    pub capabilities: Value,
    #[serde(default)]
    pub client_info: Option<ClientInfo>,
}

/// `initialize` result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    pub capabilities: Capabilities,
    pub server_info: ServerInfo,
}

/// Server capabilities advertised during `initialize`.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Capabilities {
    pub tools: ToolsCapability,
}

/// Capabilities object for the `tools` field.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

/// Server identity.
#[derive(Debug, Clone, Serialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// Client identity from `initialize` params.
#[derive(Debug, Clone, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
}

// ── `tools/list` ─────────────────────────────────────────────────────────────

/// Result of a `tools/list` request.
#[derive(Debug, Clone, Serialize)]
pub struct ToolsListResult {
    pub tools: Vec<ToolDef>,
}

/// Definition of a single tool as advertised to the client.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

// ── `tools/call` ─────────────────────────────────────────────────────────────

/// Parameters of a `tools/call` request.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// Result of a `tools/call` request.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    pub content: Vec<ContentBlock>,
    pub is_error: bool,
}

impl ToolCallResult {
    pub fn success(text: impl Into<String>) -> Self {
        Self { content: vec![ContentBlock::text(text)], is_error: false }
    }

    pub fn failure(text: impl Into<String>) -> Self {
        Self { content: vec![ContentBlock::text(text)], is_error: true }
    }

    /// Return a successful result containing an image block followed by a
    /// metadata text block.  Used by `fono.screen`.
    pub fn with_image(b64_data: String, meta_text: String) -> Self {
        Self {
            content: vec![
                ContentBlock::image(b64_data, "image/png".to_string()),
                ContentBlock::text(meta_text),
            ],
            is_error: false,
        }
    }
}

/// A single content block in a tool result.
///
/// Supports both `text` blocks (`{"type":"text","text":"..."}`) and
/// `image` blocks (`{"type":"image","data":"<base64>","mimeType":"image/png"}`).
#[derive(Debug, Clone, Serialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub kind: String,
    /// Present on `text` blocks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Present on `image` blocks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// Present on `image` blocks.
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self { kind: "text".into(), text: Some(text.into()), data: None, mime_type: None }
    }

    pub fn image(data: String, mime_type: String) -> Self {
        Self { kind: "image".into(), text: None, data: Some(data), mime_type: Some(mime_type) }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_response_ok_round_trip() {
        let resp = ServerResponse::ok(Value::from(1), serde_json::json!({"hello": "world"}));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"result\""));
        assert!(!json.contains("\"error\""));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["id"], 1);
    }

    #[test]
    fn server_response_error_round_trip() {
        let resp = ServerResponse::error(Value::from(2), METHOD_NOT_FOUND, "not found");
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"error\""));
        assert!(!json.contains("\"result\""));
    }

    #[test]
    fn client_message_deserialize_request() {
        let raw = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let msg: ClientMessage = serde_json::from_str(raw).unwrap();
        assert_eq!(msg.method, "initialize");
        assert!(msg.id.is_some());
    }

    #[test]
    fn client_message_deserialize_notification() {
        let raw = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let msg: ClientMessage = serde_json::from_str(raw).unwrap();
        assert_eq!(msg.method, "notifications/initialized");
        assert!(msg.id.is_none());
    }

    #[test]
    fn initialize_params_round_trip() {
        let raw = r#"{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}"#;
        let params: InitializeParams = serde_json::from_str(raw).unwrap();
        assert_eq!(params.protocol_version, "2024-11-05");
        assert_eq!(params.client_info.unwrap().name, "test");
    }

    #[test]
    fn initialize_result_serializes_camel_case() {
        let result = InitializeResult {
            protocol_version: "2024-11-05".into(),
            capabilities: Capabilities::default(),
            server_info: ServerInfo { name: "fono".into(), version: "0.1.0".into() },
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("protocolVersion"), "camelCase required: {json}");
        assert!(json.contains("serverInfo"), "camelCase required: {json}");
    }

    #[test]
    fn tool_call_result_success_not_error() {
        let r = ToolCallResult::success("done");
        assert!(!r.is_error);
        assert_eq!(r.content[0].text.as_deref(), Some("done"));
    }

    #[test]
    fn tool_call_result_failure_is_error() {
        let r = ToolCallResult::failure("oops");
        assert!(r.is_error);
    }

    #[test]
    fn tool_call_params_name_and_args() {
        let raw = r#"{"name":"fono.speak","arguments":{"text":"hello"}}"#;
        let p: ToolCallParams = serde_json::from_str(raw).unwrap();
        assert_eq!(p.name, "fono.speak");
        assert_eq!(p.arguments["text"], "hello");
    }

    #[test]
    fn content_block_text_type() {
        let b = ContentBlock::text("hello");
        assert_eq!(b.kind, "text");
        let json = serde_json::to_string(&b).unwrap();
        assert!(json.contains("\"type\""));
    }
}
