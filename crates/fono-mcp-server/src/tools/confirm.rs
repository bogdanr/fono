// SPDX-License-Identifier: GPL-3.0-only
//! `fono.confirm` — speak a question with choices and listen for a voice answer.
//!
//! Composes a single TTS utterance from the question + choices, speaks
//! it, then listens for at most `timeout_seconds` and matches the
//! transcript against the choice list. Returns
//! `{"choice": "<label>"}` on a successful match, `{"choice": "timeout"}`
//! when nothing matched within the timeout, or
//! `{"choice": "unmatched", "transcript": "..."}` when the user spoke
//! but the answer did not map to a listed option.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use fono_ipc::McpPhase;
use fono_polish::TextFormatter;
use tracing::{debug, info};

use crate::protocol::ToolCallResult;
use crate::tools::{ClientIdentityHandle, McpContext, PolishClassifierCache, Tool};
use crate::voice_io::{
    listen_once, match_choice, resolve_program_voice, speak_text, ListenStopReason,
    McpActivityGuard,
};

/// `fono.confirm` tool.
pub struct ConfirmTool {
    cfg: fono_core::config::Config,
    secrets: fono_core::Secrets,
    whisper_models_dir: PathBuf,
    polish_models_dir: PathBuf,
    polish_classifier_cache: PolishClassifierCache,
    daemon_ipc_candidates: Vec<PathBuf>,
    client_identity: ClientIdentityHandle,
}

impl ConfirmTool {
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

    /// Mirror of `ListenTool::classifier_for_call` — see that for
    /// the rationale. Lives here separately because the two tools
    /// don't otherwise share state, and duplicating four lines is
    /// cheaper than refactoring both onto a shared helper struct.
    fn classifier_for_call(&self) -> Option<Arc<dyn TextFormatter>> {
        if self.cfg.mcp.relevance_filter.as_str() != "llm" {
            return None;
        }
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
                            target: "fono_mcp_server::confirm",
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
}

#[async_trait]
impl Tool for ConfirmTool {
    fn name(&self) -> &str {
        "fono.confirm"
    }

    fn description(&self) -> &str {
        "Ask the user a question with bounded choices and capture their spoken answer. \
         Use `fono.listen` instead when the answer is free-form."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question to ask."
                },
                "choices": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "List of choices (e.g. [\"A\", \"B\", \"C\"])."
                },
                "timeout_seconds": {
                    "type": "number",
                    "description": "Seconds to wait for an answer (default: 10)."
                }
            },
            "required": ["question", "choices"]
        })
    }

    async fn call(&self, arguments: serde_json::Value) -> ToolCallResult {
        let question = match arguments.get("question").and_then(|v| v.as_str()) {
            Some(q) if !q.trim().is_empty() => q.trim().to_string(),
            _ => return ToolCallResult::failure("fono.confirm: missing or empty `question`"),
        };
        let choices: Vec<String> = match arguments.get("choices").and_then(|v| v.as_array()) {
            Some(arr) if !arr.is_empty() => arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .filter(|s| !s.trim().is_empty())
                .collect(),
            _ => {
                return ToolCallResult::failure(
                    "fono.confirm: `choices` must be a non-empty array of strings",
                );
            }
        };
        if choices.is_empty() {
            return ToolCallResult::failure(
                "fono.confirm: `choices` must contain at least one non-empty entry",
            );
        }
        let configured_timeout = self.cfg.mcp.confirm_timeout_seconds.max(1);
        let timeout = arguments
            .get("timeout_seconds")
            .and_then(|v| v.as_u64())
            .map(|v| v.min(configured_timeout as u64) as u32)
            .unwrap_or(configured_timeout);

        let stt_backend = fono_core::providers::stt_backend_str(&self.cfg.stt.backend);
        let tts_backend = fono_core::providers::tts_backend_str(&self.cfg.tts.backend);

        let utterance = compose_utterance(&question, &choices);
        let program = crate::tools::client_program(&self.client_identity);
        let voice = resolve_program_voice(&self.cfg, program.as_deref(), None);
        let speak_timings = match speak_text(
            &self.cfg,
            &self.secrets,
            &utterance,
            voice.as_deref(),
            &self.daemon_ipc_candidates,
        )
        .await
        {
            Ok(t) => t,
            Err(e) => return ToolCallResult::failure(format!("fono.confirm: TTS failed: {e:#}")),
        };

        // Tray feedback: paint the "Confirming" phase across the
        // listen-and-match span. listen_once itself adds a nested
        // Listening guard; the daemon's depth counter keeps the tray
        // amber throughout and only restores on the outermost Drop.
        // Slice 7 of plan v7.
        let _confirm_guard =
            McpActivityGuard::new(McpPhase::Confirming, &self.daemon_ipc_candidates);

        let outcome = match listen_once(
            &self.cfg,
            &self.secrets,
            &self.whisper_models_dir,
            timeout,
            Some(&utterance),
            Some(&utterance),
            self.classifier_for_call(),
            &self.daemon_ipc_candidates,
        )
        .await
        {
            Ok(o) => o,
            Err(e) => {
                info!(
                    target: "fono_mcp_server::confirm",
                    client = program.as_deref().unwrap_or(""),
                    tts_backend,
                    voice = voice.as_deref().unwrap_or(""),
                    stt_backend,
                    n_choices = choices.len(),
                    tts_synth_ms = speak_timings.synth_ms,
                    ok = false,
                    error = %format!("{e:#}"),
                    "fono.confirm completed"
                );
                return ToolCallResult::failure(format!("fono.confirm: listen failed: {e:#}"));
            }
        };

        let (choice, result) =
            if outcome.transcript.is_empty() && outcome.reason == ListenStopReason::Cancelled {
                ("cancelled".to_string(), ToolCallResult::success("{\"choice\":\"cancelled\"}"))
            } else if outcome.transcript.is_empty() && outcome.reason == ListenStopReason::Timeout {
                ("timeout".to_string(), ToolCallResult::success("{\"choice\":\"timeout\"}"))
            } else {
                match match_choice(&outcome.transcript, &choices) {
                    Some(c) => (
                        c.clone(),
                        ToolCallResult::success(
                            serde_json::json!({ "choice": c, "transcript": outcome.transcript })
                                .to_string(),
                        ),
                    ),
                    None => (
                        "unmatched".to_string(),
                        ToolCallResult::success(
                            serde_json::json!({
                                "choice": "unmatched",
                                "transcript": outcome.transcript,
                            })
                            .to_string(),
                        ),
                    ),
                }
            };

        info!(
            target: "fono_mcp_server::confirm",
            client = program.as_deref().unwrap_or(""),
            tts_backend,
            voice = voice.as_deref().unwrap_or(""),
            stt_backend,
            n_choices = choices.len(),
            tts_synth_ms = speak_timings.synth_ms,
            capture_ms = outcome.capture_ms,
            stt_ms = outcome.stt_ms,
            relevance_ms = outcome.relevance_ms,
            duration_ms = outcome.duration_ms,
            choice = %choice,
            ok = true,
            "fono.confirm completed"
        );
        result
    }
}

/// Build a single, naturally-flowing TTS utterance from the question
/// and the choice list. The choices are read out joined with commas so
/// short single-letter labels like `A/B/C` get a beat between them
/// rather than being run together.
fn compose_utterance(question: &str, choices: &[String]) -> String {
    let q = question.trim();
    let q = if q.ends_with('?') || q.ends_with('.') { q.to_string() } else { format!("{q}?") };
    let joined = choices.join(", ");
    format!("{q} Choices: {joined}.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utterance_appends_question_mark() {
        let u = compose_utterance("ready", &["A".into(), "B".into()]);
        assert!(u.starts_with("ready?"));
        assert!(u.contains("A, B"));
    }

    #[test]
    fn utterance_preserves_existing_punctuation() {
        let u = compose_utterance("Ready.", &["yes".into(), "no".into()]);
        assert!(u.starts_with("Ready."));
    }
}
