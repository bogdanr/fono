// SPDX-License-Identifier: GPL-3.0-only
//! Streaming chat completions client for OpenAI-compatible providers
//! (Cerebras, Groq, OpenAI, OpenRouter, Ollama).
//!
//! Sends a `chat/completions` POST with `stream: true`, parses the
//! resulting SSE stream into [`TokenDelta`]s. The wire shape is the
//! same minimal subset every modern OpenAI-compatible vendor honours.
//!
//! ## Tool calling — `fono_screen`
//!
//! When [`AssistantContext::prefer_vision`] is true and a screen-
//! capture callback is attached, the request body grows a `tools`
//! array advertising the `fono_screen` function. If the model chooses
//! to call it, this client orchestrates the canonical two-turn flow:
//!
//! 1. First turn — the model emits `tool_calls` with no spoken
//!    content (rare lax models may emit a stray thought first;
//!    that text is buffered and discarded when the call lands).
//! 2. Local capture runs via the supplied callback.
//! 3. Second turn — a fresh `chat/completions` POST whose `messages`
//!    array continues the conversation with the captured image:
//!
//!    ```text
//!    ...history...
//!    user: "what's on my screen?"
//!    assistant: { tool_calls: [{ id, function: fono_screen(...) }] }
//!    tool: { tool_call_id, content: "Captured 800x600 PNG of ..." }
//!    user: [{ type: "image_url", image_url: { url: "data:image/png;base64,..." } }]
//!    ```
//!
//!    The second request omits `tools` so the model can't loop.
//!
//! `TokenDelta::tool_event` carries [`ToolEvent::Called`] /
//! [`ToolEvent::Result`] sentinels so the caller can record the
//! exchange in [`crate::ConversationHistory`] for subsequent turns.

use std::sync::Once;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use fono_core::screen_capture::{CaptureError, CaptureMode, CapturedImage};
use fono_http::{emit_http_debug, provider_request_id, Outcome, RequestTimings};
use futures::stream::{BoxStream, StreamExt};
use serde::{Deserialize, Serialize};

use crate::history::{ChatRole, ChatTurn, ToolCall};
use crate::sse::SseBuffer;
use crate::traits::{Assistant, AssistantContext, TokenDelta, ToolEvent};

fn is_local_backend(backend_name: &str) -> bool {
    backend_name == "ollama"
}

/// Inter-chunk watchdog for streaming chat. SSE replies tick at most
/// every few seconds even on slow providers (Cerebras / Groq deliver
/// one chunk per token roughly every 30-50 ms); 20 s of silence
/// between chunks signals a stall well before users assume Fono has
/// crashed.
const SSE_CHUNK_TIMEOUT: Duration = Duration::from_secs(20);

/// Per-provider OpenAI-compatible `/chat/completions` endpoints live in
/// [`crate::factory`] (always compiled) so the local LLM server's
/// pass-through proxy can share them regardless of feature gates
/// (ADR 0036).
use crate::factory::{
    CEREBRAS_CHAT_ENDPOINT, GEMINI_CHAT_ENDPOINT, GROQ_CHAT_ENDPOINT, OPENAI_CHAT_ENDPOINT,
    OPENROUTER_CHAT_ENDPOINT,
};

pub struct OpenAiCompatChat {
    endpoint: String,
    models_endpoint: Option<String>,
    api_key: String,
    model: String,
    backend_name: &'static str,
    /// Phase E5 — when `Some`, the request body grows a `tools`
    /// array carrying this provider's native web-search tool. Only
    /// OpenAI populates this in practice (the catalogue gates Groq /
    /// Cerebras / OpenRouter / Ollama as `WebSearchSupport::None`).
    web_search_tool: Option<&'static str>,
    client: reqwest::Client,
}

impl OpenAiCompatChat {
    pub fn new(
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        backend_name: &'static str,
    ) -> Self {
        let endpoint_s = endpoint.into();
        let models_endpoint = derive_models_endpoint(&endpoint_s);
        Self {
            endpoint: endpoint_s,
            models_endpoint,
            api_key: api_key.into(),
            model: model.into(),
            backend_name,
            web_search_tool: None,
            client: warm_client(),
        }
    }

    /// Attach a web-search tool descriptor to every request.
    ///
    /// **Currently a no-op.** OpenAI's `chat/completions` API hard-
    /// rejects unknown tool types with a 400. `web_search_preview`
    /// is a Responses-API descriptor, not chat/completions. Until
    /// the OpenAI client migrates, any non-`None` tool id is dropped
    /// at request time with a one-shot `tracing::warn!`.
    #[must_use]
    pub fn with_web_search(mut self, tool_id: Option<&'static str>) -> Self {
        self.web_search_tool = tool_id;
        self
    }

    pub fn cerebras(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(CEREBRAS_CHAT_ENDPOINT, api_key, model, "cerebras")
    }

    pub fn groq(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(GROQ_CHAT_ENDPOINT, api_key, model, "groq")
    }

    pub fn openai(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(OPENAI_CHAT_ENDPOINT, api_key, model, "openai")
    }

    pub fn openrouter(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(OPENROUTER_CHAT_ENDPOINT, api_key, model, "openrouter")
    }

    /// Gemini via its OpenAI-compatible surface, single `GEMINI_API_KEY`
    /// (`Authorization: Bearer <key>`), free tier (ADR 0034).
    pub fn gemini(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(GEMINI_CHAT_ENDPOINT, api_key, model, "gemini")
    }

    pub fn ollama(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(endpoint, "", model, "ollama")
    }
}

fn derive_models_endpoint(chat: &str) -> Option<String> {
    chat.strip_suffix("/chat/completions").map(|root| format!("{root}/models"))
}

fn warm_client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(4)
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .http2_keep_alive_interval(Some(Duration::from_secs(20)))
        .http2_keep_alive_timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default()
}

// ── Wire types ───────────────────────────────────────────────────────────────

/// Top-level chat request body. `tools` is omitted when empty.
#[derive(Serialize)]
struct ChatReq {
    model: String,
    messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    /// OpenAI's gpt-5 / o-series reject the legacy `max_tokens` and
    /// demand `max_completion_tokens`. Older OpenAI models plus
    /// every Cerebras / Groq / OpenRouter / Ollama deployment
    /// accept either. We send only the new name.
    #[serde(rename = "max_completion_tokens")]
    max_tokens: u32,
    stream: bool,
    /// Gemini 3.x Flash enables "thinking" by default, which inflates
    /// time-to-first-token (the assistant's dominant latency). On the
    /// OpenAI-compat surface `reasoning_effort: "low"` pins it to the lowest
    /// level (3.x can't disable thinking entirely). Sent only for Gemini;
    /// other cloud backends keep their default and local Ollama uses
    /// `think: false` below instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
    /// Local OpenAI-compatible servers default thinking-capable models
    /// (Qwen3.x, DeepSeek-R1, etc.) to hidden reasoning. The assistant
    /// should speak the final answer only, so local endpoints get both
    /// Ollama's native `think: false` and llama.cpp's Jinja override.
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_template_kwargs: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
}

/// One message in the request. Supports text content, multipart
/// content arrays (text + image_url), `tool_calls` (assistant turns
/// that invoke functions), and `tool_call_id` (tool-result turns).
#[derive(Serialize, Clone)]
struct WireMessage {
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<WireContent>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<WireToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(untagged)]
enum WireContent {
    Text(String),
    Parts(Vec<WireContentPart>),
}

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireContentPart {
    /// Plain-text content part. Currently unused — the assistant
    /// only emits `ImageUrl` parts in the second-turn user message —
    /// but the variant exists so the wire shape mirrors the OpenAI
    /// docs and future tools can attach a text block too.
    #[allow(dead_code)]
    Text {
        text: String,
    },
    ImageUrl {
        image_url: WireImageUrl,
    },
}

#[derive(Serialize, Clone)]
struct WireImageUrl {
    url: String,
}

#[derive(Serialize, Clone)]
struct WireToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireToolFunction,
}

#[derive(Serialize, Clone)]
struct WireToolFunction {
    name: String,
    arguments: String,
}

// ── Stream-chunk parsing ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: ChunkDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Deserialize, Default)]
struct ToolCallDelta {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ToolFunctionDelta>,
}

#[derive(Deserialize, Default)]
struct ToolFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Build the `fono_screen` OpenAI function-calling tool descriptor.
pub(crate) fn build_screen_tool() -> Vec<serde_json::Value> {
    vec![serde_json::json!({
        "type": "function",
        "function": {
            "name": "fono_screen",
            "description": "Capture a screenshot to see what's on the user's screen. \
                mode=automatic grabs the focused window instantly. \
                mode=interactive lets the user frame a region. \
                Only call when user references something visible.",
            "parameters": {
                "type": "object",
                "required": ["mode"],
                "properties": {
                    "mode": {"type": "string", "enum": ["automatic", "interactive"]}
                }
            }
        }
    })]
}

/// Parse a `{"mode": "..."}` JSON arguments string from a tool call
/// into a [`CaptureMode`]. Defaults to [`CaptureMode::Automatic`] on
/// missing / unknown values.
pub(crate) fn parse_tool_call_mode(args: &str) -> CaptureMode {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return CaptureMode::Automatic;
    };
    match v.get("mode").and_then(|m| m.as_str()) {
        Some("interactive") => CaptureMode::Interactive,
        _ => CaptureMode::Automatic,
    }
}

/// Convert one [`ChatTurn`] from the rolling history into the wire
/// message it should serialise as.
fn turn_to_wire(turn: &ChatTurn) -> WireMessage {
    match turn.role {
        ChatRole::User | ChatRole::System => WireMessage {
            role: turn.role.as_str(),
            content: Some(WireContent::Text(turn.content.clone())),
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        ChatRole::Assistant => {
            let content = if turn.content.is_empty() {
                None
            } else {
                Some(WireContent::Text(turn.content.clone()))
            };
            let tool_calls = turn
                .tool_calls
                .iter()
                .map(|c| WireToolCall {
                    id: c.id.clone(),
                    kind: "function",
                    function: WireToolFunction {
                        name: c.name.clone(),
                        arguments: c.arguments.clone(),
                    },
                })
                .collect();
            WireMessage { role: "assistant", content, tool_calls, tool_call_id: None }
        }
        ChatRole::Tool => WireMessage {
            role: "tool",
            content: Some(WireContent::Text(turn.content.clone())),
            tool_calls: Vec::new(),
            tool_call_id: turn.tool_call_id.clone(),
        },
    }
}

/// Build the messages array for a chat request: system prompt +
/// rolling history + the current user turn.
fn build_initial_messages(ctx: &AssistantContext, user_text: &str) -> Vec<WireMessage> {
    let mut messages: Vec<WireMessage> = Vec::with_capacity(ctx.history.len() + 2);
    if !ctx.system_prompt.is_empty() {
        messages.push(WireMessage {
            role: ChatRole::System.as_str(),
            content: Some(WireContent::Text(ctx.system_prompt.clone())),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
    for turn in &ctx.history {
        messages.push(turn_to_wire(turn));
    }
    messages.push(WireMessage {
        role: ChatRole::User.as_str(),
        content: Some(WireContent::Text(user_text.to_string())),
        tool_calls: Vec::new(),
        tool_call_id: None,
    });
    messages
}

/// Append the canonical post-tool-call triplet to `messages`:
///
/// 1. assistant message echoing the tool call
/// 2. tool message with a short text summary
/// 3. user message carrying the captured image as a content part
fn append_tool_result_with_image(
    messages: &mut Vec<WireMessage>,
    call: &ToolCall,
    summary: &str,
    image: &CapturedImage,
) {
    messages.push(WireMessage {
        role: ChatRole::Assistant.as_str(),
        content: None,
        tool_calls: vec![WireToolCall {
            id: call.id.clone(),
            kind: "function",
            function: WireToolFunction {
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            },
        }],
        tool_call_id: None,
    });
    messages.push(WireMessage {
        role: ChatRole::Tool.as_str(),
        content: Some(WireContent::Text(summary.to_string())),
        tool_calls: Vec::new(),
        tool_call_id: Some(call.id.clone()),
    });
    messages.push(WireMessage {
        role: ChatRole::User.as_str(),
        content: Some(WireContent::Parts(vec![WireContentPart::ImageUrl {
            image_url: WireImageUrl { url: data_url_for_png(&image.png_bytes) },
        }])),
        tool_calls: Vec::new(),
        tool_call_id: None,
    });
}

/// Build a `data:image/png;base64,...` URL for the captured PNG.
fn data_url_for_png(png_bytes: &[u8]) -> String {
    let mut url = String::from("data:image/png;base64,");
    url.push_str(&BASE64.encode(png_bytes));
    url
}

/// Produce a short, prose summary of the captured image suitable for
/// the tool-result text block and for retention in conversation
/// history.
fn capture_summary(image: &CapturedImage) -> String {
    use fono_core::screen_capture::CaptureSource;
    let source = match &image.source {
        CaptureSource::Window { wm_class, .. } if !wm_class.is_empty() => {
            format!("focused window ({wm_class})")
        }
        CaptureSource::Window { .. } => "focused window".to_string(),
        CaptureSource::Region => "user-selected region".to_string(),
    };
    format!("Captured {}x{} PNG of {} via {}.", image.width, image.height, source, image.tool)
}

/// One-line spoken fallback when a tool error short-circuits the
/// turn. Localising this would require either a translation layer in
/// Rust or a second LLM round-trip; for now we hand-write English
/// strings the user can override via `[assistant].prompt_main`.
fn capture_error_sentence(err: &CaptureError) -> &'static str {
    match err {
        CaptureError::Cancelled => "OK, never mind.",
        CaptureError::PrivateWindow => {
            "That window is marked private, so I can't take a screenshot of it."
        }
        CaptureError::NoToolAvailable => {
            "I can't take a screenshot — no capture tool is installed."
        }
        CaptureError::Timeout => "The screenshot timed out, sorry.",
        CaptureError::Io(_) => "I couldn't take a screenshot just now.",
    }
}

#[async_trait]
impl Assistant for OpenAiCompatChat {
    fn name(&self) -> &'static str {
        self.backend_name
    }

    #[allow(clippy::too_many_lines)]
    async fn reply_stream(
        &self,
        user_text: &str,
        ctx: &AssistantContext,
    ) -> Result<BoxStream<'static, Result<TokenDelta>>> {
        // Defensive: chat/completions hard-rejects unknown tool types
        // with a 400, so we drop the descriptor and warn once per
        // process. Only the (unsupported-but-misconfigured) web-search
        // tool path lands here today.
        if self.web_search_tool.is_some() {
            static WARN_ONCE: Once = Once::new();
            WARN_ONCE.call_once(|| {
                tracing::warn!(
                    target: "fono.assistant",
                    "web_search tool requested but OpenAI chat/completions \
                     doesn't accept it; ignoring"
                );
            });
        }

        let initial_messages = build_initial_messages(ctx, user_text);
        let tools = if ctx.prefer_vision && ctx.screen_capture.is_some() {
            Some(build_screen_tool())
        } else {
            None
        };
        let buffer_first_turn = tools.is_some();
        let screen_cap_fn = ctx.screen_capture.clone();

        let runner = ChatRunner {
            endpoint: self.endpoint.clone(),
            api_key: self.api_key.clone(),
            model: self.model.clone(),
            backend_name: self.backend_name,
            client: self.client.clone(),
            is_openrouter: self.backend_name == "openrouter",
        };

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<TokenDelta>>(32);
        tokio::spawn(async move {
            // ── First turn ───────────────────────────────────────────
            let first = match runner
                .run_chat_pump(initial_messages.clone(), tools, buffer_first_turn, &tx)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            };

            // Did the model want a tool call?
            let Some(call) = first.tool_call else {
                // No tool call: flush buffered content if any and we're done.
                if buffer_first_turn {
                    let buffered = first.buffered_content;
                    if !buffered.is_empty() {
                        let _ = tx.send(Ok(TokenDelta::text(buffered))).await;
                    }
                }
                return;
            };

            // Buffered content from the first turn is now discarded.
            // It's a stray pre-tool-call thought from a lax model and
            // would otherwise be spoken aloud as "I'll take a look at
            // your screen now" — annoying and misleading because the
            // real answer comes after the second turn.
            drop(first.buffered_content);

            // Only fono_screen is wired today.
            if call.name != "fono_screen" {
                tracing::warn!(
                    target: "fono.assistant",
                    "unknown tool call from model: {}; ignoring",
                    call.name
                );
                return;
            }

            // Record the tool call in history (caller appends).
            let _ = tx.send(Ok(TokenDelta::tool(ToolEvent::Called(call.clone())))).await;

            // Run the capture on a blocking thread.
            let mode = parse_tool_call_mode(&call.arguments);
            let Some(cap_fn) = screen_cap_fn else {
                let _ = tx
                    .send(Err(anyhow!("fono_screen requested but no capture callback attached")))
                    .await;
                return;
            };
            let cb = std::sync::Arc::clone(&cap_fn);
            let capture_result =
                tokio::task::spawn_blocking(move || cb(mode)).await.map_err(|e| anyhow!("{e}"));

            let image = match capture_result {
                Ok(Ok(img)) => img,
                Ok(Err(cap_err)) => {
                    let sentence = capture_error_sentence(&cap_err);
                    let summary = format!("Capture failed: {cap_err}");
                    // Record the failed tool result and speak a short fallback.
                    let _ = tx
                        .send(Ok(TokenDelta::tool(ToolEvent::Result {
                            tool_call_id: call.id.clone(),
                            summary,
                        })))
                        .await;
                    let _ = tx.send(Ok(TokenDelta::text(sentence.to_string()))).await;
                    return;
                }
                Err(e) => {
                    let _ = tx.send(Err(anyhow!("fono_screen task error: {e}"))).await;
                    return;
                }
            };

            let summary = capture_summary(&image);
            let _ = tx
                .send(Ok(TokenDelta::tool(ToolEvent::Result {
                    tool_call_id: call.id.clone(),
                    summary: summary.clone(),
                })))
                .await;

            // ── Second turn: feed the image back to the model ───────
            let mut second_messages = initial_messages;
            append_tool_result_with_image(&mut second_messages, &call, &summary, &image);

            // No `tools` field in the follow-up — the model must
            // answer in prose now. Streaming is unbuffered.
            if let Err(e) = runner.run_chat_pump(second_messages, None, false, &tx).await {
                let _ = tx.send(Err(e)).await;
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(Box::pin(stream))
    }

    async fn prewarm(&self) -> Result<()> {
        let Some(url) = self.models_endpoint.as_ref() else {
            return Ok(());
        };
        let mut req = self.client.get(url);
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        if self.backend_name == "openrouter" {
            for (name, value) in fono_core::openrouter_attribution::headers() {
                req = req.header(name, value);
            }
        }
        let res =
            req.send().await.with_context(|| format!("{} chat prewarm GET", self.backend_name))?;
        let _ = res.bytes().await;
        Ok(())
    }
}

// ── ChatRunner ───────────────────────────────────────────────────────────────

/// Per-task copy of the runtime bits needed to fire a chat/completions
/// request. Decouples the spawned task from `&self` lifetimes.
#[derive(Clone)]
struct ChatRunner {
    endpoint: String,
    api_key: String,
    model: String,
    backend_name: &'static str,
    client: reqwest::Client,
    is_openrouter: bool,
}

fn assistant_token_budget(backend_name: &str) -> u32 {
    if is_local_backend(backend_name) {
        256
    } else {
        1024
    }
}

/// Reasoning effort to send for `backend_name`, if any.
///
/// Gemini 3.x Flash thinks by default and that dominates assistant
/// time-to-first-token; `reasoning_effort: "low"` pins it to the lowest level
/// the model allows (thinking can't be disabled entirely on 3.x). Other cloud
/// backends are left at their server default, and local Ollama uses
/// `think: false` instead, so they get `None` here.
fn reasoning_effort_for(backend_name: &str) -> Option<&'static str> {
    (backend_name == "gemini").then_some("low")
}

/// Outcome of one pump: the buffered content (when buffering was
/// enabled) and the accumulated tool call (if any).
struct PumpResult {
    buffered_content: String,
    tool_call: Option<ToolCall>,
}

impl ChatRunner {
    /// Fire one chat/completions POST and pump SSE events. When
    /// `buffer_content` is true, content deltas are accumulated into
    /// the returned [`PumpResult::buffered_content`] instead of being
    /// forwarded through `tx` — the caller flushes or discards them
    /// based on whether a tool call materialised.
    #[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
    async fn run_chat_pump(
        &self,
        messages: Vec<WireMessage>,
        tools: Option<Vec<serde_json::Value>>,
        buffer_content: bool,
        tx: &tokio::sync::mpsc::Sender<Result<TokenDelta>>,
    ) -> Result<PumpResult> {
        let req = ChatReq {
            model: self.model.clone(),
            messages,
            temperature: (!uses_default_sampling_only(self.backend_name, &self.model))
                .then_some(0.5),
            top_p: (!uses_default_sampling_only(self.backend_name, &self.model)).then_some(0.9),
            max_tokens: assistant_token_budget(self.backend_name),
            stream: true,
            reasoning_effort: reasoning_effort_for(self.backend_name),
            think: is_local_backend(self.backend_name).then_some(false),
            chat_template_kwargs: is_local_backend(self.backend_name).then_some(
                serde_json::json!({
                    "enable_thinking": false,
                }),
            ),
            tools,
        };

        let mut builder =
            self.client.post(&self.endpoint).header("accept", "text/event-stream").json(&req);
        if !self.api_key.is_empty() {
            builder = builder.bearer_auth(&self.api_key);
        }
        if self.is_openrouter {
            for (name, value) in fono_core::openrouter_attribution::headers() {
                builder = builder.header(name, value);
            }
        }

        let mut timings = RequestTimings::start();
        let resp = match builder.send().await {
            Ok(r) => {
                timings.mark_headers();
                r
            }
            Err(e) => {
                emit_http_debug(
                    "assistant",
                    self.backend_name,
                    "chat/completions",
                    0,
                    &timings,
                    0,
                    None,
                    0,
                    "<none>",
                    1,
                    Outcome::ConnectError,
                );
                return Err(anyhow::Error::new(e)
                    .context(format!("{} chat POST failed", self.backend_name)));
            }
        };

        let status = resp.status();
        let request_id = provider_request_id(resp.headers())
            .map(str::to_owned)
            .unwrap_or_else(|| "<none>".to_string());
        let content_length = resp.content_length();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            emit_http_debug(
                "assistant",
                self.backend_name,
                "chat/completions",
                status.as_u16(),
                &timings,
                0,
                content_length,
                0,
                &request_id,
                1,
                Outcome::HttpError,
            );
            return Err(anyhow!(
                "{} chat returned {status} (request_id={request_id}): {}",
                self.backend_name,
                truncate(&body, 400)
            ));
        }

        let backend_name = self.backend_name;
        let mut bytes_stream = resp.bytes_stream();
        let mut parser = SseBuffer::new();
        let mut buffered = String::new();
        let mut total_bytes: u64 = 0;
        let mut chunk_count: u32 = 0;
        let mut outcome = Outcome::Ok;
        let mut tc_id = String::new();
        let mut tc_name = String::new();
        let mut tc_args = String::new();
        let mut finish_is_tool_calls = false;
        // Set when any finish_reason fires inside a choice; checked
        // after the per-choice loop so we don't break mid-iteration.
        let mut should_break_outer = false;

        'outer: loop {
            let next = tokio::time::timeout(SSE_CHUNK_TIMEOUT, bytes_stream.next()).await;
            let chunk = match next {
                Err(_elapsed) => {
                    outcome = Outcome::Stalled;
                    let _ = tx
                        .send(Err(anyhow!(
                            "{backend_name} stream stalled after {}ms (request_id={request_id}, \
                             {chunk_count} chunks, {total_bytes} bytes)",
                            SSE_CHUNK_TIMEOUT.as_millis()
                        )))
                        .await;
                    break 'outer;
                }
                Ok(None) => break 'outer,
                Ok(Some(Err(e))) => {
                    // No need to update `outcome` — we return before
                    // emit_http_debug runs.
                    return Err(anyhow!(
                        "{backend_name} stream chunk error (request_id={request_id}): {e}"
                    ));
                }
                Ok(Some(Ok(b))) => b,
            };
            if chunk_count == 0 {
                timings.mark_first_byte();
            }
            chunk_count = chunk_count.saturating_add(1);
            total_bytes = total_bytes.saturating_add(chunk.len() as u64);
            parser.push(&chunk);

            while let Some(ev) = parser.next_event() {
                let data = ev.data.trim();
                if data == "[DONE]" {
                    break 'outer;
                }
                if data.is_empty() {
                    continue;
                }
                let parsed = match serde_json::from_str::<StreamChunk>(data) {
                    Ok(p) => p,
                    Err(e) => {
                        // No need to update `outcome` — we return before
                        // emit_http_debug runs.
                        return Err(anyhow!(
                            "{backend_name} stream chunk parse error \
                             (request_id={request_id}): {e}; payload: {data}"
                        ));
                    }
                };
                for choice in parsed.choices {
                    if let Some(content) = choice.delta.content {
                        if !content.is_empty() {
                            if buffer_content {
                                buffered.push_str(&content);
                            } else if tx.send(Ok(TokenDelta::text(content))).await.is_err() {
                                // Receiver dropped — bail.
                                break 'outer;
                            }
                        }
                    }
                    if let Some(tcs) = choice.delta.tool_calls {
                        for tc in tcs {
                            if let Some(id) = tc.id {
                                tc_id = id;
                            }
                            if let Some(func) = tc.function {
                                if let Some(name) = func.name {
                                    tc_name = name;
                                }
                                if let Some(args) = func.arguments {
                                    tc_args.push_str(&args);
                                }
                            }
                        }
                    }
                    if let Some(reason) = choice.finish_reason.as_deref() {
                        if reason == "tool_calls" {
                            finish_is_tool_calls = true;
                        }
                        should_break_outer = true;
                    }
                }
                if should_break_outer {
                    break 'outer;
                }
            }
        }

        timings.mark_body_done();
        emit_http_debug(
            "assistant",
            backend_name,
            "chat/completions",
            status.as_u16(),
            &timings,
            total_bytes,
            content_length,
            chunk_count,
            &request_id,
            1,
            outcome,
        );

        let tool_call = if finish_is_tool_calls && !tc_id.is_empty() && !tc_name.is_empty() {
            Some(ToolCall { id: tc_id, name: tc_name, arguments: tc_args })
        } else {
            None
        };
        Ok(PumpResult { buffered_content: buffered, tool_call })
    }
}

fn uses_default_sampling_only(backend_name: &str, model: &str) -> bool {
    backend_name == "openai" && model.to_ascii_lowercase().contains("gpt-5")
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out = s[..max].to_string();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{ChatRole, ChatTurn};
    use std::time::Instant;

    #[test]
    fn derive_models_endpoint_works_for_canonical_path() {
        assert_eq!(
            derive_models_endpoint("https://api.openai.com/v1/chat/completions"),
            Some("https://api.openai.com/v1/models".to_string())
        );
    }

    #[test]
    fn derive_models_endpoint_returns_none_for_unknown_shape() {
        assert!(derive_models_endpoint("https://example.com/v2/respond").is_none());
    }

    #[test]
    fn truncate_long_strings() {
        assert_eq!(truncate("hello world", 5), "hello…");
    }

    #[test]
    fn truncate_short_strings() {
        assert_eq!(truncate("hi", 5), "hi");
    }

    fn turn(role: ChatRole, content: &str) -> ChatTurn {
        ChatTurn {
            role,
            content: content.to_string(),
            at: Instant::now(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    #[test]
    fn initial_messages_serialise_history_in_order() {
        let ctx = AssistantContext {
            system_prompt: "be brief".into(),
            language: None,
            history: vec![turn(ChatRole::User, "hi"), turn(ChatRole::Assistant, "hello")],
            active_window_context: None,
            screen_capture: None,
            prefer_vision: false,
            max_new_tokens: None,
            allow_brain_capture: false,
        };
        let msgs = build_initial_messages(&ctx, "what now?");
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[1].role, "user");
        assert_eq!(msgs[3].role, "user");
        // Round-trip via JSON to confirm the wire shape.
        let body = serde_json::to_string(&msgs).unwrap();
        assert!(body.contains("\"system\""));
        assert!(body.contains("\"what now?\""));
        assert!(!body.contains("\"tools\""));
    }

    #[test]
    fn history_with_tool_calls_round_trips_to_wire() {
        let mut assistant_turn = turn(ChatRole::Assistant, "");
        assistant_turn.tool_calls = vec![ToolCall {
            id: "call_abc".into(),
            name: "fono_screen".into(),
            arguments: "{\"mode\":\"automatic\"}".into(),
        }];
        let mut tool_turn = turn(ChatRole::Tool, "Captured 800x600 PNG of focused window");
        tool_turn.tool_call_id = Some("call_abc".into());

        let ctx = AssistantContext {
            system_prompt: String::new(),
            language: None,
            history: vec![
                turn(ChatRole::User, "what's on screen?"),
                assistant_turn,
                tool_turn,
                turn(ChatRole::Assistant, "I see a terminal."),
            ],
            active_window_context: None,
            screen_capture: None,
            prefer_vision: false,
            max_new_tokens: None,
            allow_brain_capture: false,
        };
        let msgs = build_initial_messages(&ctx, "and now?");
        // 4 history turns + new user.
        assert_eq!(msgs.len(), 5);
        let body = serde_json::to_string(&msgs).unwrap();
        assert!(body.contains("\"tool_calls\""), "body: {body}");
        assert!(body.contains("\"fono_screen\""), "body: {body}");
        assert!(body.contains("\"tool_call_id\":\"call_abc\""), "body: {body}");
        assert!(body.contains("\"role\":\"tool\""), "body: {body}");
    }

    #[test]
    fn append_tool_result_appends_three_canonical_messages() {
        use fono_core::screen_capture::{CaptureSource, CapturedImage};

        let mut msgs: Vec<WireMessage> = vec![WireMessage {
            role: "user",
            content: Some(WireContent::Text("hi".into())),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }];
        let call = ToolCall {
            id: "call_xyz".into(),
            name: "fono_screen".into(),
            arguments: "{\"mode\":\"automatic\"}".into(),
        };
        let img = CapturedImage {
            png_bytes: vec![0x89, 0x50, 0x4E, 0x47],
            source: CaptureSource::Window { wm_class: "kitty".into(), title: "shell".into() },
            width: 800,
            height: 600,
            tool: "scrot".into(),
        };
        let summary = capture_summary(&img);
        append_tool_result_with_image(&mut msgs, &call, &summary, &img);
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].tool_calls.len(), 1);
        assert_eq!(msgs[1].tool_calls[0].id, "call_xyz");
        assert_eq!(msgs[2].role, "tool");
        assert_eq!(msgs[2].tool_call_id.as_deref(), Some("call_xyz"));
        assert_eq!(msgs[3].role, "user");
        let body = serde_json::to_string(&msgs[3]).unwrap();
        assert!(body.contains("\"image_url\""), "body: {body}");
        assert!(body.contains("data:image/png;base64"), "body: {body}");
    }

    #[test]
    fn data_url_encodes_png() {
        let url = data_url_for_png(&[0x89, 0x50, 0x4E, 0x47]);
        assert!(url.starts_with("data:image/png;base64,"));
        assert!(url.len() > "data:image/png;base64,".len());
    }

    #[test]
    fn capture_summary_includes_dims_and_tool() {
        use fono_core::screen_capture::{CaptureSource, CapturedImage};
        let img = CapturedImage {
            png_bytes: Vec::new(),
            source: CaptureSource::Window { wm_class: "kitty".into(), title: String::new() },
            width: 1280,
            height: 720,
            tool: "import".into(),
        };
        let s = capture_summary(&img);
        assert!(s.contains("1280x720"), "{s}");
        assert!(s.contains("kitty"), "{s}");
        assert!(s.contains("import"), "{s}");
    }

    #[test]
    fn capture_error_sentences_are_user_friendly() {
        assert!(capture_error_sentence(&CaptureError::Cancelled).contains("never mind"));
        assert!(capture_error_sentence(&CaptureError::PrivateWindow).contains("private"));
        assert!(capture_error_sentence(&CaptureError::NoToolAvailable).contains("no capture tool"));
    }

    #[test]
    fn local_backends_disable_thinking() {
        assert!(is_local_backend("ollama"));
        assert!(!is_local_backend("groq"));
        assert!(!is_local_backend("openai"));
    }

    #[test]
    fn gemini_sends_low_reasoning_effort_others_omit_it() {
        assert_eq!(reasoning_effort_for("gemini"), Some("low"));
        assert_eq!(reasoning_effort_for("groq"), None);
        assert_eq!(reasoning_effort_for("ollama"), None);

        let req = ChatReq {
            model: "gemini-flash-lite-latest".into(),
            messages: Vec::new(),
            temperature: Some(0.5),
            top_p: Some(0.9),
            max_tokens: 1024,
            stream: true,
            reasoning_effort: reasoning_effort_for("gemini"),
            think: None,
            chat_template_kwargs: None,
            tools: None,
        };
        let body = serde_json::to_string(&req).unwrap();
        assert!(body.contains("\"reasoning_effort\":\"low\""), "body: {body}");

        let plain = ChatReq {
            model: "llama-3.3-70b".into(),
            messages: Vec::new(),
            temperature: Some(0.5),
            top_p: Some(0.9),
            max_tokens: 1024,
            stream: true,
            reasoning_effort: reasoning_effort_for("groq"),
            think: None,
            chat_template_kwargs: None,
            tools: None,
        };
        let plain_body = serde_json::to_string(&plain).unwrap();
        assert!(!plain_body.contains("reasoning_effort"), "body: {plain_body}");
    }

    #[test]
    fn local_request_serializes_thinking_disabled() {
        let req = ChatReq {
            model: "qwen3.5-4b".into(),
            messages: Vec::new(),
            temperature: Some(0.5),
            top_p: Some(0.9),
            max_tokens: assistant_token_budget("ollama"),
            stream: true,
            reasoning_effort: None,
            think: Some(false),
            chat_template_kwargs: Some(serde_json::json!({ "enable_thinking": false })),
            tools: None,
        };
        let body = serde_json::to_string(&req).unwrap();
        assert!(body.contains("\"think\":false"), "body: {body}");
        assert!(body.contains("\"enable_thinking\":false"), "body: {body}");
    }

    #[test]
    fn prefer_vision_false_omits_tools() {
        // With prefer_vision=false the reply_stream MUST NOT include
        // the `tools` field in the body.
        let req = ChatReq {
            model: "gpt-5-mini".into(),
            messages: Vec::new(),
            temperature: Some(0.5),
            top_p: Some(0.9),
            max_tokens: 1024,
            stream: true,
            reasoning_effort: None,
            think: None,
            chat_template_kwargs: None,
            tools: None,
        };
        let body = serde_json::to_string(&req).unwrap();
        assert!(!body.contains("\"tools\""), "body: {body}");
    }

    #[test]
    fn prefer_vision_true_includes_screen_tool() {
        let req = ChatReq {
            model: "gpt-5-mini".into(),
            messages: Vec::new(),
            temperature: Some(0.5),
            top_p: Some(0.9),
            max_tokens: 1024,
            stream: true,
            reasoning_effort: None,
            think: None,
            chat_template_kwargs: None,
            tools: Some(build_screen_tool()),
        };
        let body = serde_json::to_string(&req).unwrap();
        assert!(body.contains("fono_screen"), "body: {body}");
        assert!(body.contains("\"type\":\"function\""), "body: {body}");
    }

    #[test]
    fn parse_tool_call_mode_handles_automatic_and_interactive() {
        assert!(matches!(parse_tool_call_mode(r#"{"mode":"automatic"}"#), CaptureMode::Automatic));
        assert!(matches!(
            parse_tool_call_mode(r#"{"mode":"interactive"}"#),
            CaptureMode::Interactive
        ));
        // Unknown / missing falls back to Automatic.
        assert!(matches!(parse_tool_call_mode("{}"), CaptureMode::Automatic));
        assert!(matches!(parse_tool_call_mode("garbage"), CaptureMode::Automatic));
    }
}
