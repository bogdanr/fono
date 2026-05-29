// SPDX-License-Identifier: GPL-3.0-only
//! Streaming chat client for Anthropic's Messages API.
//!
//! Uses `/v1/messages` with `stream: true`. Anthropic's SSE schema
//! emits typed events (`message_start`, `content_block_delta`,
//! `message_stop`, etc.) — we care about the `text_delta` payloads
//! inside `content_block_delta` and the terminating `message_stop`
//! event.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use serde::{Deserialize, Serialize};

use crate::history::ChatRole;
use crate::sse::SseBuffer;
use crate::traits::{Assistant, AssistantContext, TokenDelta};

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const MODELS_ENDPOINT: &str = "https://api.anthropic.com/v1/models";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicChat {
    api_key: String,
    model: String,
    /// Phase E5 — when `Some`, every `messages` request grows a
    /// `tools` array with Anthropic's native web-search tool. The
    /// catalogue currently emits `"web_search_20250305"`.
    web_search_tool: Option<&'static str>,
    client: reqwest::Client,
}

impl AnthropicChat {
    #[must_use]
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            web_search_tool: None,
            client: warm_client(),
        }
    }

    /// Attach Anthropic's native web-search tool descriptor. The
    /// expected tool id is `"web_search_20250305"`. Shape:
    /// `[{"type": "<id>", "name": "web_search", "max_uses": 3}]`.
    /// See <https://docs.anthropic.com/en/docs/agents-and-tools/tool-use/web-search-tool>.
    #[must_use]
    pub fn with_web_search(mut self, tool_id: Option<&'static str>) -> Self {
        self.web_search_tool = tool_id;
        self
    }
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

#[derive(Serialize)]
struct MessagesReq<'a> {
    model: &'a str,
    max_tokens: u32,
    temperature: f32,
    system: &'a str,
    messages: Vec<Message<'a>>,
    stream: bool,
    /// Phase E5 — Anthropic's native web-search tool when opted in.
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
}

/// Build Anthropic's `tools` array for the web-search tool. Shape:
/// `[{"type": "<tool_id>", "name": "web_search", "max_uses": 3}]`.
/// `max_uses=3` is the Phase-E default — caps cost per turn while
/// still letting Claude chain a couple of follow-up queries.
#[must_use]
pub(crate) fn build_web_search_tools(tool_id: &str) -> Vec<serde_json::Value> {
    vec![serde_json::json!({
        "type": tool_id,
        "name": "web_search",
        "max_uses": 3,
    })]
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

/// `content_block_delta` payload. Other event types ride past us.
#[derive(Deserialize)]
struct ContentBlockDelta {
    #[serde(default)]
    delta: AnthropicDelta,
}

#[derive(Deserialize, Default)]
struct AnthropicDelta {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

#[async_trait]
impl Assistant for AnthropicChat {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    #[allow(clippy::too_many_lines)]
    async fn reply_stream(
        &self,
        user_text: &str,
        ctx: &AssistantContext,
    ) -> Result<BoxStream<'static, Result<TokenDelta>>> {
        // Anthropic doesn't accept a `system` role inside `messages`
        // — it lives at the top level. We translate any system turns
        // already present in history into a `system` prefix string,
        // appended after the configured prompt_main.
        let mut history_system_extra = String::new();
        let mut messages: Vec<Message> = Vec::with_capacity(ctx.history.len() + 1);
        for turn in &ctx.history {
            match turn.role {
                ChatRole::System => {
                    if !history_system_extra.is_empty() {
                        history_system_extra.push_str("\n\n");
                    }
                    history_system_extra.push_str(&turn.content);
                }
                ChatRole::User => {
                    messages.push(Message { role: "user", content: &turn.content });
                }
                ChatRole::Assistant => {
                    // Anthropic Messages API rejects empty assistant
                    // turns. When the model only emitted tool_calls
                    // (no text), the rolling history records an
                    // empty-content assistant turn; collapse it to a
                    // short narration so the API accepts the request.
                    if turn.content.is_empty() && !turn.tool_calls.is_empty() {
                        // Skip — the following `Tool` turn will
                        // carry the prose summary.
                        continue;
                    }
                    messages.push(Message { role: "assistant", content: &turn.content });
                }
                ChatRole::Tool => {
                    // Anthropic's `messages` array doesn't have a
                    // tool role on the OpenAI-style wire (Anthropic's
                    // own tool-use shape lives inside content blocks
                    // and is not wired here yet). Downgrade the
                    // result to a brief user-channel narration so
                    // subsequent turns still have the context. The
                    // actual image is _not_ resent.
                    messages.push(Message { role: "user", content: &turn.content });
                }
            }
        }
        messages.push(Message { role: "user", content: user_text });
        let system_full = if history_system_extra.is_empty() {
            ctx.system_prompt.clone()
        } else if ctx.system_prompt.is_empty() {
            history_system_extra.clone()
        } else {
            format!("{}\n\n{}", ctx.system_prompt, history_system_extra)
        };

        let tools = self.web_search_tool.map(build_web_search_tools);
        if let Some(tool_id) = self.web_search_tool {
            tracing::info!(
                target: "fono.assistant",
                "web search tool active: {tool_id}"
            );
        }
        let req = MessagesReq {
            model: &self.model,
            max_tokens: 1024,
            temperature: 0.5,
            system: &system_full,
            messages,
            stream: true,
            tools,
        };
        let resp = self
            .client
            .post(ENDPOINT)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("accept", "text/event-stream")
            .json(&req)
            .send()
            .await
            .context("anthropic chat POST failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("anthropic chat returned {status}: {}", truncate(&body, 400)));
        }

        let bytes_stream = resp.bytes_stream();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<TokenDelta>>(32);

        tokio::spawn(async move {
            let mut bytes_stream = bytes_stream;
            let mut parser = SseBuffer::new();
            'outer: while let Some(chunk) = bytes_stream.next().await {
                let chunk = match chunk {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = tx.send(Err(anyhow!("anthropic stream chunk error: {e}"))).await;
                        return;
                    }
                };
                parser.push(&chunk);
                while let Some(ev) = parser.next_event() {
                    let event_kind = ev.event.as_deref().unwrap_or("");
                    match event_kind {
                        "content_block_delta" => {
                            let parsed = match serde_json::from_str::<ContentBlockDelta>(&ev.data) {
                                Ok(p) => p,
                                Err(e) => {
                                    let _ = tx
                                        .send(Err(anyhow!(
                                            "anthropic stream parse error: {e}; payload: {}",
                                            ev.data
                                        )))
                                        .await;
                                    return;
                                }
                            };
                            // Only `text_delta` carries spoken
                            // content; other kinds (input_json_delta,
                            // signature_delta) belong to tool-use
                            // flows we ignore for now.
                            let is_text = parsed
                                .delta
                                .kind
                                .as_deref()
                                .map(|k| k == "text_delta")
                                .unwrap_or(true);
                            if is_text {
                                if let Some(text) = parsed.delta.text {
                                    if !text.is_empty()
                                        && tx.send(Ok(TokenDelta::text(text))).await.is_err()
                                    {
                                        return;
                                    }
                                }
                            }
                        }
                        "message_stop" | "" => {
                            // `message_stop` ends the stream; an
                            // empty event tag with sentinel `[DONE]`-
                            // shaped data also signals end on some
                            // proxies. Either way, exit cleanly.
                            if event_kind == "message_stop" {
                                break 'outer;
                            }
                        }
                        "error" => {
                            let _ = tx
                                .send(Err(anyhow!(
                                    "anthropic stream error event: {}",
                                    truncate(&ev.data, 400)
                                )))
                                .await;
                            return;
                        }
                        // message_start / content_block_start /
                        // content_block_stop / message_delta /
                        // ping — informational, ignore.
                        _ => {}
                    }
                }
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(Box::pin(stream))
    }

    async fn prewarm(&self) -> Result<()> {
        let res = self
            .client
            .get(MODELS_ENDPOINT)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .send()
            .await
            .context("anthropic chat prewarm")?;
        let _ = res.bytes().await;
        Ok(())
    }
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

    #[test]
    fn truncate_long_strings() {
        assert_eq!(truncate("hello world", 5), "hello…");
    }

    #[test]
    fn truncate_short_strings() {
        assert_eq!(truncate("hi", 5), "hi");
    }

    #[test]
    fn build_web_search_tools_shape() {
        let tools = build_web_search_tools("web_search_20250305");
        let json = serde_json::to_value(&tools).unwrap();
        assert_eq!(
            json,
            serde_json::json!([{
                "type": "web_search_20250305",
                "name": "web_search",
                "max_uses": 3
            }])
        );
    }
}
