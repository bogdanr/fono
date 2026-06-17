// SPDX-License-Identifier: GPL-3.0-only
//! Tool trait, registry, and context for the Fono MCP server.

use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;
use fono_polish::TextFormatter;

use crate::protocol::{ClientInfo, ToolCallResult, ToolDef};

pub mod confirm;
pub mod listen;
pub mod screen;
pub mod speak;
pub mod summarize;

// ── Context ───────────────────────────────────────────────────────────────────

/// Identity of the MCP client (the program driving Fono), captured
/// from `clientInfo` in the `initialize` handshake. Used by the voice
/// resolver to give each program its own palette voice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIdentity {
    /// Client program name, e.g. `"chat-cli"` or `"coach"`.
    pub name: String,
    /// Optional client version string.
    pub version: Option<String>,
}

/// Shared, interior-mutable slot holding the [`ClientIdentity`].
///
/// Constructed empty before the `initialize` handshake; the server
/// fills it once `clientInfo` arrives, and every tool reads it at call
/// time to know which program is speaking. Cloned (Arc) into each tool
/// and into the [`ToolRegistry`] so a single write is visible everywhere.
pub type ClientIdentityHandle = Arc<RwLock<Option<ClientIdentity>>>;

impl From<ClientInfo> for ClientIdentity {
    fn from(info: ClientInfo) -> Self {
        Self { name: info.name, version: info.version }
    }
}

/// Process-wide lazy cache of the polish-backed LLM used by the
/// `relevance_filter = "llm"` mode. `None` after init means polish is
/// not configured (or its build failed) — every subsequent listen
/// call short-circuits the LLM stage and falls back to the heuristic
/// gate alone.
pub type PolishClassifierCache = Arc<OnceLock<Option<Arc<dyn TextFormatter>>>>;

/// Shared context passed to every tool implementation.
pub struct McpContext {
    /// Full Fono config (TTS backend, audio device, MCP settings, …).
    pub cfg: fono_core::config::Config,
    /// API keys / secrets needed by cloud TTS and STT backends.
    pub secrets: fono_core::Secrets,
    /// Directory holding local whisper.cpp model blobs. Consulted only
    /// when `[stt].backend = "local"`; cloud backends ignore it.
    pub whisper_models_dir: std::path::PathBuf,
    /// Directory holding local polish (llama-local) GGUF weights.
    /// Consulted only when the LLM relevance classifier — or any
    /// future tool that needs the polish backend — actually
    /// constructs one. Cloud polish backends ignore it.
    pub polish_models_dir: std::path::PathBuf,
    /// Lazy, process-wide cache of the polish-backed classifier used
    /// by the relevance filter. Populated on first `fono.listen` call
    /// when `[mcp].relevance_filter = "llm"`; shared between
    /// `ListenTool` and `ConfirmTool` so both pay the construction
    /// cost only once.
    pub polish_classifier_cache: PolishClassifierCache,
    /// Socket paths to probe when checking whether a Fono daemon is
    /// running alongside this MCP server. Empty disables the probe
    /// (used in tests). Tried in order on every `fono.listen` call;
    /// the first successful `Status` round-trip flips the listen
    /// loop to "daemon present — don't spawn a second overlay".
    /// Slice 6 of plan v7.
    pub daemon_ipc_candidates: Vec<std::path::PathBuf>,
    /// Identity of the connected MCP client, populated at the
    /// `initialize` handshake. Empty until then (and in non-MCP call
    /// paths such as the CLI). The voice resolver reads this to assign
    /// a per-program voice.
    pub client_identity: ClientIdentityHandle,
}

impl McpContext {
    /// Build a fresh, empty classifier cache. Use when constructing
    /// an `McpContext` from outside the daemon (CLI, tests).
    #[must_use]
    pub fn new_classifier_cache() -> PolishClassifierCache {
        Arc::new(OnceLock::new())
    }

    /// Build a fresh, empty client-identity handle. Use when
    /// constructing an `McpContext` from outside the MCP server (CLI,
    /// tests), where no `initialize` handshake will populate it.
    #[must_use]
    pub fn new_client_identity() -> ClientIdentityHandle {
        Arc::new(RwLock::new(None))
    }
}

/// Read the connected client's program name from `handle`, for use as
/// the voice-resolver `program` key. Returns `None` when no identity was
/// captured (pre-`initialize`, CLI path) or the name is blank.
#[must_use]
pub fn client_program(handle: &ClientIdentityHandle) -> Option<String> {
    handle
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(|i| i.name.trim().to_string()))
        .filter(|s| !s.is_empty())
}

// ── Tool trait ────────────────────────────────────────────────────────────────

/// A single MCP tool exposed by the Fono server.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The dot-namespaced tool name, e.g. `"fono.speak"`.
    fn name(&self) -> &str;

    /// One-sentence description shown to the agent.
    fn description(&self) -> &str;

    /// JSON Schema describing the tool's `arguments` object.
    fn input_schema(&self) -> serde_json::Value;

    /// Execute the tool. Never panics — errors are returned as
    /// `ToolCallResult::failure(...)`.
    async fn call(&self, arguments: serde_json::Value) -> ToolCallResult;
}

// ── Registry ─────────────────────────────────────────────────────────────────

/// Owns all registered tools and dispatches `tools/call` requests.
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
    /// Shared client identity, written by the server at `initialize`
    /// and shared (Arc) with every registered tool.
    client_identity: ClientIdentityHandle,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    /// Empty registry. Use [`Self::default_with_context`] for a
    /// production server with the three standard voice tools.
    pub fn new() -> Self {
        Self { tools: Vec::new(), client_identity: McpContext::new_client_identity() }
    }

    /// Register a tool (moved into a `Box<dyn Tool>`).
    pub fn register(&mut self, tool: impl Tool + 'static) {
        self.tools.push(Box::new(tool));
    }

    /// Build the standard Fono tool set: `fono.speak`, `fono.listen`,
    /// `fono.confirm`, `fono.screen`, `fono.summarize`. Takes a
    /// reference to the context so each tool can clone the parts it
    /// needs.
    pub fn default_with_context(ctx: &McpContext) -> Self {
        let mut reg = Self { tools: Vec::new(), client_identity: ctx.client_identity.clone() };
        reg.register(speak::SpeakTool::new(ctx));
        reg.register(listen::ListenTool::new(ctx));
        reg.register(confirm::ConfirmTool::new(ctx));
        reg.register(screen::ScreenTool::new(ctx));
        reg.register(summarize::SummarizeTool::new(ctx));
        reg
    }

    /// Tool definitions for `tools/list`.
    pub fn tool_defs(&self) -> Vec<ToolDef> {
        self.tools
            .iter()
            .map(|t| ToolDef {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect()
    }

    /// Dispatch a `tools/call` by name. Returns an error result when
    /// the name is unknown rather than propagating an `Err`.
    pub async fn call(&self, name: &str, arguments: serde_json::Value) -> ToolCallResult {
        for tool in &self.tools {
            if tool.name() == name {
                return tool.call(arguments).await;
            }
        }
        ToolCallResult::failure(format!("unknown tool: {name}"))
    }

    /// Record the connecting client's identity (from the `initialize`
    /// handshake). Visible to every tool through the shared handle.
    pub fn set_client_identity(&self, identity: ClientIdentity) {
        if let Ok(mut guard) = self.client_identity.write() {
            *guard = Some(identity);
        }
    }

    /// Read the currently recorded client identity, if any.
    #[must_use]
    pub fn client_identity(&self) -> Option<ClientIdentity> {
        self.client_identity.read().ok().and_then(|g| g.clone())
    }

    /// Clone the shared client-identity handle (for tests / inspection).
    #[must_use]
    pub fn client_identity_handle(&self) -> ClientIdentityHandle {
        self.client_identity.clone()
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// True when no tools are registered.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}
