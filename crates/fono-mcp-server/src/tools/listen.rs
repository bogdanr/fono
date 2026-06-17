// SPDX-License-Identifier: GPL-3.0-only
//! `fono.listen` — record voice input and return a transcript.
//!
//! Captures from the configured input device, gates end-of-utterance on
//! the shared silence-watch state machine, and runs the buffered audio
//! through the configured STT backend. The optional `prompt` argument is
//! synthesised via the configured TTS backend before recording starts
//! so the agent can verbally ask the user what to say.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use fono_polish::TextFormatter;
use tracing::debug;

use crate::protocol::ToolCallResult;
use crate::tools::{ClientIdentityHandle, McpContext, PolishClassifierCache, Tool};
use crate::voice_io::{listen_once, resolve_program_voice, speak_text, ListenStopReason};

/// Default cap on a single recording (in seconds) when the caller does
/// not pass `max_seconds`. Mirrors the value advertised in the tool
/// description so agents know what to expect.
const DEFAULT_MAX_SECONDS: u32 = 45;

/// `fono.listen` tool.
pub struct ListenTool {
    cfg: fono_core::config::Config,
    secrets: fono_core::Secrets,
    whisper_models_dir: PathBuf,
    polish_models_dir: PathBuf,
    polish_classifier_cache: PolishClassifierCache,
    daemon_ipc_candidates: Vec<PathBuf>,
    client_identity: ClientIdentityHandle,
}

impl ListenTool {
    pub fn new(ctx: &McpContext) -> Self {
        Self {
            cfg: ctx.cfg.clone(),
            secrets: ctx.secrets.clone(),
            whisper_models_dir: ctx.whisper_models_dir.clone(),
            polish_models_dir: ctx.polish_models_dir.clone(),
            polish_classifier_cache: ctx.polish_classifier_cache.clone(),
            daemon_ipc_candidates: ctx.daemon_ipc_candidates.clone(),
            client_identity: ctx.client_identity.clone(),
        }
    }

    /// Resolve the polish-backed relevance classifier on first use,
    /// then return a cheap `Arc` clone on every subsequent call. The
    /// inner `Option` distinguishes "polish not configured / build
    /// failed" (cached `None` → never retry inside this process)
    /// from "we tried and it worked" (cached `Some`).
    ///
    /// Cheap to call when `relevance_filter != "llm"`: the caller is
    /// responsible for short-circuiting first; this function always
    /// triggers the lazy build.
    fn ensure_classifier(&self) -> Option<Arc<dyn TextFormatter>> {
        self.polish_classifier_cache
            .get_or_init(|| {
                match fono_polish::build_polish(
                    &self.cfg.polish,
                    &self.secrets,
                    &self.polish_models_dir,
                ) {
                    Ok(opt) => opt,
                    Err(e) => {
                        debug!(
                            target: "fono_mcp_server::listen",
                            error = %e,
                            "polish build failed for relevance classifier; falling back \
                             to heuristic gate only",
                        );
                        None
                    }
                }
            })
            .clone()
    }

    /// Return the classifier to hand to `listen_once` for this
    /// call: `Some` only when `relevance_filter = "llm"` and polish
    /// is configured. Other filter modes get `None` so `listen_once`
    /// skips the LLM stage entirely.
    fn classifier_for_call(&self) -> Option<Arc<dyn TextFormatter>> {
        if self.cfg.mcp.relevance_filter.as_str() == "llm" {
            self.ensure_classifier()
        } else {
            None
        }
    }
}

#[async_trait]
impl Tool for ListenTool {
    fn name(&self) -> &str {
        "fono.listen"
    }

    fn description(&self) -> &str {
        "Ask the user a free-form question and capture their spoken answer. Use \
         `fono.confirm` instead when the answer fits a small fixed set of choices."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Optional text to speak aloud before recording starts."
                },
                "context": {
                    "type": "string",
                    "description": "Short description of what kind of answer is expected. \
                                    Used by the relevance filter to discard background \
                                    speech (radio, TV, side conversation) and prompt-TTS \
                                    echo. Pass the question text or a brief intent (e.g. \
                                    \"asking the user for their favourite colour\")."
                },
                "max_seconds": {
                    "type": "number",
                    "description": "Maximum recording duration in seconds (default: 45)."
                }
            }
        })
    }

    async fn call(&self, arguments: serde_json::Value) -> ToolCallResult {
        let max_seconds = arguments
            .get("max_seconds")
            .and_then(|v| v.as_u64())
            .map(|v| v.min(self.cfg.mcp.listen_max_seconds as u64) as u32)
            .unwrap_or(DEFAULT_MAX_SECONDS.min(self.cfg.mcp.listen_max_seconds));
        let prompt = arguments.get("prompt").and_then(|v| v.as_str()).map(str::to_owned);
        let context = arguments.get("context").and_then(|v| v.as_str()).map(str::to_owned);

        debug!(
            target: "fono_mcp_server::listen",
            max_seconds,
            has_prompt = prompt.is_some(),
            has_context = context.is_some(),
            "fono.listen called"
        );

        if let Some(text) = prompt.as_deref() {
            if !text.trim().is_empty() {
                // The overlay is intentionally **not** shown during
                // prompt TTS — Slice 1 of
                // `plans/2026-05-26-mcp-listen-overlay-and-silence-parity-v7.md`
                // scopes the visual indicator to the actual
                // microphone-open phase. If the user can already hear
                // the agent speaking, painting a "RECORDING" panel on
                // top of that is just noise.
                if let Err(e) = speak_text(
                    &self.cfg,
                    &self.secrets,
                    text,
                    resolve_program_voice(
                        &self.cfg,
                        crate::tools::client_program(&self.client_identity).as_deref(),
                        None,
                    )
                    .as_deref(),
                    &self.daemon_ipc_candidates,
                )
                .await
                {
                    return ToolCallResult::failure(format!(
                        "fono.listen: prompt TTS failed: {e:#}"
                    ));
                }
            }
        }

        match listen_once(
            &self.cfg,
            &self.secrets,
            &self.whisper_models_dir,
            max_seconds,
            prompt.as_deref(),
            context.as_deref(),
            self.classifier_for_call(),
            &self.daemon_ipc_candidates,
        )
        .await
        {
            Ok(outcome) => {
                let reason = match outcome.reason {
                    ListenStopReason::Silence => "silence",
                    ListenStopReason::Timeout => "timeout",
                    ListenStopReason::Cancelled => "cancelled",
                };
                let body = serde_json::json!({
                    "transcript": outcome.transcript,
                    "duration_ms": outcome.duration_ms,
                    "reason": reason,
                    "rejected_count": outcome.rejected_count,
                });
                ToolCallResult::success(body.to_string())
            }
            Err(e) => ToolCallResult::failure(format!("fono.listen: {e:#}")),
        }
    }
}
