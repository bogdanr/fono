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
    client: reqwest::Client,
}

impl AnthropicChat {
    #[must_use]
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            client: warm_client(),
        }
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
                ChatRole::User | ChatRole::Assistant => {
                    messages.push(Message {
                        role: turn.role.as_str(),
                        content: &turn.content,
                    });
                }
            }
        }
        messages.push(Message {
            role: "user",
            content: user_text,
        });
        let system_full = if history_system_extra.is_empty() {
            ctx.system_prompt.clone()
        } else if ctx.system_prompt.is_empty() {
            history_system_extra.clone()
        } else {
            format!("{}\n\n{}", ctx.system_prompt, history_system_extra)
        };

        let req = MessagesReq {
            model: &self.model,
            max_tokens: 1024,
            temperature: 0.5,
            system: &system_full,
            messages,
            stream: true,
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
            return Err(anyhow!(
                "anthropic chat returned {status}: {}",
                truncate(&body, 400)
            ));
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
                        let _ = tx
                            .send(Err(anyhow!("anthropic stream chunk error: {e}")))
                            .await;
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
                                        && tx.send(Ok(TokenDelta { text })).await.is_err()
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
}
