// SPDX-License-Identifier: GPL-3.0-only
//! `fono.speak` — synthesise text through the configured TTS backend
//! and play it back on the configured audio device.

use async_trait::async_trait;
use tracing::debug;

use crate::protocol::ToolCallResult;
use crate::tools::{ClientIdentityHandle, McpContext, Tool};
use crate::voice_io::{resolve_program_voice, speak_text};

/// `fono.speak` tool.
///
/// Accepts `{ "text": string }` and optionally `{ "voice": string }`.
/// Synthesises the text through the configured TTS backend and blocks
/// until the audio finishes draining through the playback queue.
pub struct SpeakTool {
    cfg: fono_core::config::Config,
    secrets: fono_core::Secrets,
    daemon_ipc_candidates: Vec<std::path::PathBuf>,
    client_identity: ClientIdentityHandle,
}

impl SpeakTool {
    pub fn new(ctx: &McpContext) -> Self {
        Self {
            cfg: ctx.cfg.clone(),
            secrets: ctx.secrets.clone(),
            daemon_ipc_candidates: ctx.daemon_ipc_candidates.clone(),
            client_identity: ctx.client_identity.clone(),
        }
    }
}

#[async_trait]
impl Tool for SpeakTool {
    fn name(&self) -> &str {
        "fono.speak"
    }

    fn description(&self) -> &str {
        "Speak a declarative line to the user via TTS. Use `fono.confirm` (bounded \
         choices) or `fono.listen` (free-form) to ask questions — `fono.speak` does \
         not capture a reply."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The text to speak. Not for questions — see tool description."
                },
                "voice": {
                    "type": "string",
                    "description": "Optional voice override (backend-specific)."
                }
            },
            "required": ["text"]
        })
    }

    async fn call(&self, arguments: serde_json::Value) -> ToolCallResult {
        let text = match arguments.get("text").and_then(|v| v.as_str()) {
            Some(t) if !t.trim().is_empty() => t.to_string(),
            _ => return ToolCallResult::failure("fono.speak: missing or empty `text` argument"),
        };
        let voice = arguments.get("voice").and_then(|v| v.as_str()).map(String::from);
        let program = crate::tools::client_program(&self.client_identity);
        let resolved = resolve_program_voice(&self.cfg, program.as_deref(), voice.as_deref());

        debug!(
            target: "fono_mcp_server::speak",
            text_len = text.len(),
            program = program.as_deref().unwrap_or(""),
            voice = resolved.as_deref().unwrap_or(""),
            "fono.speak called"
        );

        match speak_text(
            &self.cfg,
            &self.secrets,
            &text,
            resolved.as_deref(),
            &self.daemon_ipc_candidates,
        )
        .await
        {
            Ok(()) => ToolCallResult::success("{\"spoken\": true}"),
            Err(e) => ToolCallResult::failure(format!("fono.speak: {e:#}")),
        }
    }
}
