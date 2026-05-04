// SPDX-License-Identifier: GPL-3.0-only
//! Streaming chat completions client for OpenAI-compatible providers
//! (Cerebras, Groq, OpenAI, OpenRouter, Ollama).
//!
//! Sends a `chat/completions` POST with `stream: true`, parses the
//! resulting SSE stream into [`TokenDelta`]s. The wire shape is the
//! same minimal subset every modern OpenAI-compatible vendor honours;
//! provider-specific knobs (logprobs, function calls, etc.) live one
//! plan slice down.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use serde::{Deserialize, Serialize};

use crate::history::ChatRole;
use crate::sse::SseBuffer;
use crate::traits::{Assistant, AssistantContext, TokenDelta};

pub struct OpenAiCompatChat {
    endpoint: String,
    models_endpoint: Option<String>,
    api_key: String,
    model: String,
    backend_name: &'static str,
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
            client: warm_client(),
        }
    }

    pub fn cerebras(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(
            "https://api.cerebras.ai/v1/chat/completions",
            api_key,
            model,
            "cerebras",
        )
    }

    pub fn groq(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(
            "https://api.groq.com/openai/v1/chat/completions",
            api_key,
            model,
            "groq",
        )
    }

    pub fn openai(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(
            "https://api.openai.com/v1/chat/completions",
            api_key,
            model,
            "openai",
        )
    }

    pub fn openrouter(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(
            "https://openrouter.ai/api/v1/chat/completions",
            api_key,
            model,
            "openrouter",
        )
    }

    pub fn ollama(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(endpoint, "", model, "ollama")
    }
}

fn derive_models_endpoint(chat: &str) -> Option<String> {
    chat.strip_suffix("/chat/completions")
        .map(|root| format!("{root}/models"))
}

/// Warm reqwest client tuned for chat streaming. Longer overall
/// timeout than the cleanup client because chat replies can run for
/// 5-15 s on cloud providers; connect timeout stays short.
fn warm_client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(4)
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .http2_keep_alive_interval(Some(Duration::from_secs(20)))
        .http2_keep_alive_timeout(Duration::from_secs(10))
        // No top-level timeout: streamed responses can legitimately
        // run for the full duration of the user's question. Per-chunk
        // stalls are detected by the caller via `select!` on the
        // cancellation `Notify`.
        .connect_timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default()
}

#[derive(Serialize)]
struct ChatReq<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    temperature: f32,
    top_p: f32,
    // OpenAI's gpt-5 / o-series models reject the legacy
    // `max_tokens` field with a 400 and demand
    // `max_completion_tokens` instead. Older OpenAI models, plus
    // every Cerebras / Groq / OpenRouter / Ollama deployment I've
    // tested, accept either. Sending only the new name keeps the
    // newer OpenAI models happy without breaking anyone else.
    #[serde(rename = "max_completion_tokens")]
    max_tokens: u32,
    stream: bool,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

/// One chunk of an OpenAI-compatible streaming response.
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
}

#[async_trait]
impl Assistant for OpenAiCompatChat {
    fn name(&self) -> &'static str {
        self.backend_name
    }

    // The streaming body assembles request, headers, error path, and
    // the SSE pump in one place; splitting them makes the control flow
    // harder to follow rather than easier.
    #[allow(clippy::too_many_lines)]
    async fn reply_stream(
        &self,
        user_text: &str,
        ctx: &AssistantContext,
    ) -> Result<BoxStream<'static, Result<TokenDelta>>> {
        let mut messages: Vec<Message> = Vec::with_capacity(ctx.history.len() + 2);
        if !ctx.system_prompt.is_empty() {
            messages.push(Message {
                role: ChatRole::System.as_str(),
                content: &ctx.system_prompt,
            });
        }
        for turn in &ctx.history {
            messages.push(Message {
                role: turn.role.as_str(),
                content: &turn.content,
            });
        }
        messages.push(Message {
            role: ChatRole::User.as_str(),
            content: user_text,
        });

        let req = ChatReq {
            model: &self.model,
            messages,
            // Voice-assistant defaults: short, conversational, low
            // randomness. `max_tokens` is generous because the user
            // can interrupt with another F10 press at any time.
            temperature: 0.5,
            top_p: 0.9,
            max_tokens: 512,
            stream: true,
        };
        let mut builder = self
            .client
            .post(&self.endpoint)
            .header("accept", "text/event-stream")
            .json(&req);
        if !self.api_key.is_empty() {
            builder = builder.bearer_auth(&self.api_key);
        }
        let resp = builder
            .send()
            .await
            .with_context(|| format!("{} chat POST failed", self.backend_name))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "{} chat returned {status}: {}",
                self.backend_name,
                truncate(&body, 400)
            ));
        }

        let backend_name: &'static str = self.backend_name;
        let bytes_stream = resp.bytes_stream();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<TokenDelta>>(32);

        // Pump the byte stream → SSE → JSON → TokenDelta in a
        // detached task. Dropping the receiver (e.g. via cancellation)
        // closes the channel and the task exits on the next send.
        tokio::spawn(async move {
            let mut bytes_stream = bytes_stream;
            let mut parser = SseBuffer::new();
            let mut done = false;
            'outer: while let Some(chunk) = bytes_stream.next().await {
                let chunk = match chunk {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = tx
                            .send(Err(anyhow!("{backend_name} stream chunk error: {e}")))
                            .await;
                        return;
                    }
                };
                parser.push(&chunk);
                while let Some(ev) = parser.next_event() {
                    let data = ev.data.trim();
                    if data == "[DONE]" {
                        done = true;
                        break 'outer;
                    }
                    if data.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<StreamChunk>(data) {
                        Ok(parsed) => {
                            for choice in parsed.choices {
                                if let Some(content) = choice.delta.content {
                                    if !content.is_empty()
                                        && tx.send(Ok(TokenDelta { text: content })).await.is_err()
                                    {
                                        return;
                                    }
                                }
                                if choice.finish_reason.is_some() {
                                    done = true;
                                }
                            }
                            if done {
                                break 'outer;
                            }
                        }
                        Err(e) => {
                            let _ = tx
                                .send(Err(anyhow!(
                                    "{backend_name} stream chunk parse error: {e}; payload: {data}"
                                )))
                                .await;
                            return;
                        }
                    }
                }
            }
            let _ = done;
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
        let res = req
            .send()
            .await
            .with_context(|| format!("{} chat prewarm GET", self.backend_name))?;
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
}
