// SPDX-License-Identifier: GPL-3.0-only
//! OpenAI-compatible chat-completions client used by Cerebras, Groq,
//! OpenRouter, Ollama, and OpenAI itself.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::traits::{FormatContext, TextFormatter};

pub struct OpenAiCompat {
    endpoint: String,
    api_key: String,
    model: String,
    backend_name: &'static str,
    client: reqwest::Client,
}

impl OpenAiCompat {
    pub fn new(
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        backend_name: &'static str,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            api_key: api_key.into(),
            model: model.into(),
            backend_name,
            client: reqwest::Client::new(),
        }
    }

    /// Convenience constructor for Cerebras (Phase 5 Task 5.3 — fastest latency).
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

    /// Ollama exposes an OpenAI-compatible endpoint on `/v1/chat/completions`
    /// by default; `endpoint` should point at the local instance.
    pub fn ollama(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(endpoint, "", model, "ollama")
    }
}

#[derive(Serialize)]
struct ChatReq<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    temperature: f32,
    top_p: f32,
    stream: bool,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResp {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: RespMessage,
}

#[derive(Deserialize)]
struct RespMessage {
    content: String,
}

#[async_trait]
impl TextFormatter for OpenAiCompat {
    async fn format(&self, raw: &str, ctx: &FormatContext) -> Result<String> {
        let system = ctx.system_prompt();
        let req = ChatReq {
            model: &self.model,
            messages: vec![
                Message {
                    role: "system",
                    content: &system,
                },
                Message {
                    role: "user",
                    content: raw,
                },
            ],
            temperature: 0.3,
            top_p: 0.9,
            stream: false,
        };
        let mut builder = self.client.post(&self.endpoint).json(&req);
        if !self.api_key.is_empty() {
            builder = builder.bearer_auth(&self.api_key);
        }
        let res = builder.send().await.context("chat POST failed")?;
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("{} LLM {status}: {body}", self.backend_name);
        }
        let parsed: ChatResp = serde_json::from_str(&body)
            .with_context(|| format!("parse {} response: {body}", self.backend_name))?;
        Ok(parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default()
            .trim()
            .to_string())
    }

    fn name(&self) -> &'static str {
        self.backend_name
    }
}
