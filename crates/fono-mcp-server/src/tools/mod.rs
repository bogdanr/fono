// SPDX-License-Identifier: GPL-3.0-only
//! Tool trait, registry, and context for the Fono MCP server.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use fono_polish::TextFormatter;

use crate::protocol::{ToolCallResult, ToolDef};

pub mod confirm;
pub mod listen;
pub mod screen;
pub mod speak;
pub mod summarize;

// ── Context ───────────────────────────────────────────────────────────────────

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
}

impl McpContext {
    /// Build a fresh, empty classifier cache. Use when constructing
    /// an `McpContext` from outside the daemon (CLI, tests).
    #[must_use]
    pub fn new_classifier_cache() -> PolishClassifierCache {
        Arc::new(OnceLock::new())
    }
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
        Self { tools: Vec::new() }
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
        let mut reg = Self::new();
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

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// True when no tools are registered.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}
