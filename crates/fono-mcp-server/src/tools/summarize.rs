// SPDX-License-Identifier: GPL-3.0-only
//! `fono.summarize` — summarize an incoming notification into 1-2
//! spoken sentences via the configured assistant backend, then speak the
//! summary through the configured TTS backend (unless `silent` is set).
//! The raw content (which may be a long log, alert dump, or pasted text)
//! is never read aloud verbatim.

use async_trait::async_trait;
use tracing::info;

use crate::protocol::ToolCallResult;
use crate::summarize::{build_primary_assistant, summarize_with_assistant, SummarizePayload};
use crate::tools::{ClientIdentityHandle, McpContext, Tool};
use crate::voice_io::{resolve_program_voice, speak_text};

/// `fono.summarize` tool.
///
/// Accepts a structured notification payload (see [`SummarizePayload`]);
/// only `message_text` is required. Optional `voice` overrides the TTS
/// voice exactly like `fono.speak`; optional `silent: true` skips TTS
/// playback entirely. Returns `{"spoken": <bool>, "summary": "..."}`
/// so callers can log or display what was (or would have been) said.
pub struct SummarizeTool {
    cfg: fono_core::config::Config,
    secrets: fono_core::Secrets,
    polish_models_dir: std::path::PathBuf,
    daemon_ipc_candidates: Vec<std::path::PathBuf>,
    client_identity: ClientIdentityHandle,
    /// Primary assistant, built lazily on the first call and reused for
    /// the lifetime of the (long-lived) MCP server process. For the
    /// embedded local backend this keeps the model loaded and the
    /// prompt-state cache warm, so repeat summaries restore the cached
    /// system-prompt state and only prefill the per-request payload.
    /// Build failures are not cached — the next call retries.
    assistant: tokio::sync::OnceCell<std::sync::Arc<dyn fono_assistant::Assistant>>,
}

impl SummarizeTool {
    pub fn new(ctx: &McpContext) -> Self {
        Self {
            cfg: ctx.cfg.clone(),
            secrets: ctx.secrets.clone(),
            polish_models_dir: ctx.polish_models_dir.clone(),
            daemon_ipc_candidates: ctx.daemon_ipc_candidates.clone(),
            client_identity: ctx.client_identity.clone(),
            assistant: tokio::sync::OnceCell::new(),
        }
    }
}

#[async_trait]
impl Tool for SummarizeTool {
    fn name(&self) -> &str {
        "fono.summarize"
    }

    fn description(&self) -> &str {
        "Summarize an incoming notification (chat message, log, alert) into one or \
         two spoken sentences using the configured assistant, then speak the summary \
         aloud (set `silent: true` to only return it). Send the raw content — it is \
         summarized, never read verbatim. Returns the summary text."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message_text": {
                    "type": "string",
                    "description": "The raw notification content. May be long (logs, pasted dumps); it is capped and summarized, never spoken verbatim."
                },
                "source_app": {
                    "type": "string",
                    "description": "Originating application, e.g. \"chat-cli\"."
                },
                "source_kind": {
                    "type": "string",
                    "description": "Event kind, e.g. \"incoming_message\", \"alert\"."
                },
                "account": {
                    "type": "string",
                    "description": "Account or workspace label, e.g. \"Slack / Engineering\"."
                },
                "chat_name": {
                    "type": "string",
                    "description": "Conversation or channel name."
                },
                "chat_kind": {
                    "type": "string",
                    "description": "E.g. \"channel\", \"direct_message\", \"group\"."
                },
                "sender_name": {
                    "type": "string",
                    "description": "Display name of the message author."
                },
                "attachments": {
                    "type": "array",
                    "description": "Attachment metadata (described to the model, not analyzed).",
                    "items": {
                        "type": "object",
                        "properties": {
                            "kind": { "type": "string", "description": "E.g. \"image\", \"file\", \"audio\"." },
                            "filename": { "type": "string" },
                            "mime_type": { "type": "string" },
                            "size_bytes": { "type": "integer" }
                        }
                    }
                },
                "instructions": {
                    "type": "string",
                    "description": "Optional extra instructions appended to the summarization prompt."
                },
                "voice": {
                    "type": "string",
                    "description": "Optional TTS voice override (backend-specific)."
                },
                "silent": {
                    "type": "boolean",
                    "description": "When true, skip TTS playback and only return the summary text. Default false."
                }
            },
            "required": ["message_text"]
        })
    }

    async fn call(&self, arguments: serde_json::Value) -> ToolCallResult {
        let payload: SummarizePayload = match serde_json::from_value(arguments.clone()) {
            Ok(p) => p,
            Err(e) => {
                return ToolCallResult::failure(format!("fono.summarize: invalid arguments: {e}"));
            }
        };
        if payload.message_text.trim().is_empty() {
            return ToolCallResult::failure(
                "fono.summarize: missing or empty `message_text` argument",
            );
        }
        let voice = arguments.get("voice").and_then(|v| v.as_str()).map(String::from);
        let silent = arguments.get("silent").and_then(serde_json::Value::as_bool).unwrap_or(false);
        let assistant_backend =
            fono_core::providers::assistant_backend_str(&self.cfg.assistant.backend);
        let tts_backend = fono_core::providers::tts_backend_str(&self.cfg.tts.backend);
        let text_len = payload.message_text.len();

        let assistant = match self
            .assistant
            .get_or_try_init(|| async {
                build_primary_assistant(&self.cfg, &self.secrets, &self.polish_models_dir)
            })
            .await
        {
            Ok(a) => std::sync::Arc::clone(a),
            Err(e) => return ToolCallResult::failure(format!("fono.summarize: {e:#}")),
        };
        let summarize_started = std::time::Instant::now();
        let summary = match summarize_with_assistant(
            assistant.as_ref(),
            &self.cfg,
            &self.secrets,
            &self.polish_models_dir,
            &payload,
        )
        .await
        {
            Ok(s) => s,
            Err(e) => return ToolCallResult::failure(format!("fono.summarize: {e:#}")),
        };
        let summarize_ms = summarize_started.elapsed().as_millis().min(u64::MAX as u128) as u64;

        if silent {
            info!(
                target: "fono_mcp_server::summarize",
                assistant_backend,
                text_len,
                summarize_ms,
                summary_len = summary.len(),
                spoken = false,
                ok = true,
                "fono.summarize completed"
            );
            return ToolCallResult::success(
                serde_json::json!({ "spoken": false, "summary": summary }).to_string(),
            );
        }

        // For notifications the per-program identity is `source_app`
        // (the program that raised the notification), falling back to
        // the MCP client identity when the caller omitted it. An
        // explicit `voice` argument still wins over both.
        let program = if payload.source_app.trim().is_empty() {
            crate::tools::client_program(&self.client_identity)
        } else {
            Some(payload.source_app.trim().to_string())
        };
        let resolved = resolve_program_voice(&self.cfg, program.as_deref(), voice.as_deref());

        match speak_text(
            &self.cfg,
            &self.secrets,
            &summary,
            resolved.as_deref(),
            &self.daemon_ipc_candidates,
        )
        .await
        {
            Ok(timings) => {
                info!(
                    target: "fono_mcp_server::summarize",
                    client = program.as_deref().unwrap_or(""),
                    assistant_backend,
                    tts_backend,
                    voice = resolved.as_deref().unwrap_or(""),
                    text_len,
                    summarize_ms,
                    tts_synth_ms = timings.synth_ms,
                    playback_ms = timings.playback_ms,
                    summary_len = summary.len(),
                    spoken = true,
                    ok = true,
                    "fono.summarize completed"
                );
                ToolCallResult::success(
                    serde_json::json!({ "spoken": true, "summary": summary }).to_string(),
                )
            }
            Err(e) => {
                info!(
                    target: "fono_mcp_server::summarize",
                    client = program.as_deref().unwrap_or(""),
                    assistant_backend,
                    tts_backend,
                    voice = resolved.as_deref().unwrap_or(""),
                    text_len,
                    summarize_ms,
                    summary_len = summary.len(),
                    spoken = false,
                    ok = false,
                    error = %format!("{e:#}"),
                    "fono.summarize completed"
                );
                ToolCallResult::failure(format!("fono.summarize: {e:#}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolRegistry;

    fn ctx() -> McpContext {
        McpContext {
            cfg: fono_core::config::Config::default(),
            secrets: fono_core::Secrets::default(),
            whisper_models_dir: std::path::PathBuf::from("/tmp/fono-test-models"),
            polish_models_dir: std::path::PathBuf::from("/tmp/fono-test-polish"),
            polish_classifier_cache: McpContext::new_classifier_cache(),
            daemon_ipc_candidates: Vec::new(),
            client_identity: McpContext::new_client_identity(),
        }
    }

    fn result_text(result: &ToolCallResult) -> String {
        result.content.first().and_then(|b| b.text.clone()).unwrap_or_default()
    }

    #[test]
    fn schema_requires_message_text() {
        let tool = SummarizeTool::new(&ctx());
        let schema = tool.input_schema();
        assert_eq!(schema["required"], serde_json::json!(["message_text"]));
        assert!(schema["properties"]["attachments"].is_object());
        assert!(schema["properties"]["silent"].is_object());
    }

    #[test]
    fn registry_lists_summarize() {
        let registry = ToolRegistry::default_with_context(&ctx());
        let names: Vec<String> = registry.tool_defs().into_iter().map(|d| d.name).collect();
        assert!(names.contains(&"fono.summarize".to_string()), "got: {names:?}");
    }

    #[tokio::test]
    async fn missing_message_text_fails_fast() {
        let tool = SummarizeTool::new(&ctx());
        let result = tool.call(serde_json::json!({})).await;
        assert!(result.is_error);
        assert!(result_text(&result).contains("message_text"));
    }

    #[tokio::test]
    async fn whitespace_message_text_fails_fast() {
        let tool = SummarizeTool::new(&ctx());
        let result = tool.call(serde_json::json!({ "message_text": "   " })).await;
        assert!(result.is_error);
        assert!(result_text(&result).contains("message_text"));
    }

    #[tokio::test]
    async fn malformed_attachments_fail_as_invalid_arguments() {
        let tool = SummarizeTool::new(&ctx());
        let result = tool
            .call(serde_json::json!({ "message_text": "x", "attachments": "not-an-array" }))
            .await;
        assert!(result.is_error);
        assert!(result_text(&result).contains("invalid arguments"));
    }

    #[tokio::test]
    async fn disabled_assistant_surfaces_clear_error_without_tts() {
        // Default config: assistant disabled — the call must fail with
        // guidance before any TTS construction is attempted.
        let tool = SummarizeTool::new(&ctx());
        let result = tool.call(serde_json::json!({ "message_text": "deploy failed" })).await;
        assert!(result.is_error);
        assert!(result_text(&result).contains("disabled"), "got: {}", result_text(&result));
    }

    #[tokio::test]
    async fn silent_mode_skips_tts_even_when_assistant_fails() {
        // `silent: true` must parse cleanly alongside the payload fields.
        // With the default (disabled) assistant the call still fails at
        // the summarize step — before any TTS path — proving `silent`
        // does not interfere with payload deserialization.
        let tool = SummarizeTool::new(&ctx());
        let result = tool.call(serde_json::json!({ "message_text": "x", "silent": true })).await;
        assert!(result.is_error);
        assert!(result_text(&result).contains("disabled"), "got: {}", result_text(&result));
    }
}
