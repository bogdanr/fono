// SPDX-License-Identifier: GPL-3.0-only
//! OpenAI-compatible chat-completions client used by Cerebras, Groq,
//! OpenRouter, Ollama, and OpenAI itself.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::traits::{FormatContext, TextFormatter};

pub struct OpenAiCompat {
    endpoint: String,
    models_endpoint: Option<String>,
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

/// Best-effort: rewrite `…/chat/completions` → `…/models` so we can prewarm
/// the connection with a cheap GET. Returns `None` if the endpoint shape is
/// unfamiliar (then prewarm becomes a no-op).
fn derive_models_endpoint(chat: &str) -> Option<String> {
    chat.strip_suffix("/chat/completions")
        .map(|root| format!("{root}/models"))
}

/// Warm `reqwest::Client` shared by all OpenAI-compatible backends.
/// HTTP/2 keep-alive + bounded timeouts; latency plan L3.
pub(crate) fn warm_client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_idle_timeout(std::time::Duration::from_secs(60))
        .pool_max_idle_per_host(4)
        .tcp_keepalive(Some(std::time::Duration::from_secs(30)))
        .http2_keep_alive_interval(Some(std::time::Duration::from_secs(20)))
        .http2_keep_alive_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default()
}

#[derive(Serialize)]
struct ChatReq<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    temperature: f32,
    top_p: f32,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stop: Vec<&'a str>,
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
            // Latency plan L19 — short cleanup outputs, deterministic
            // tone. Bounded `max_tokens` is critical on cloud providers
            // that meter wall-clock time.
            temperature: 0.2,
            top_p: 0.9,
            max_tokens: 256,
            stop: vec!["\n\n"],
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

    async fn prewarm(&self) -> Result<()> {
        let Some(url) = self.models_endpoint.as_ref() else {
            return Ok(());
        };
        let mut req = self.client.get(url);
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        let res = req.send().await.context("openai-compat prewarm")?;
        let _ = res.bytes().await;
        Ok(())
    }
}
