// SPDX-License-Identifier: GPL-3.0-only
//! Anthropic Messages API client.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::traits::{FormatContext, TextFormatter};

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";

pub struct AnthropicLlm {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl AnthropicLlm {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            client: reqwest::Client::new(),
        }
    }
}

#[derive(Serialize)]
struct Req<'a> {
    model: &'a str,
    max_tokens: u32,
    temperature: f32,
    system: &'a str,
    messages: Vec<Message<'a>>,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct Resp {
    content: Vec<Block>,
}

#[derive(Deserialize)]
struct Block {
    #[serde(default)]
    text: String,
}

#[async_trait]
impl TextFormatter for AnthropicLlm {
    async fn format(&self, raw: &str, ctx: &FormatContext) -> Result<String> {
        let system = ctx.system_prompt();
        let req = Req {
            model: &self.model,
            max_tokens: 2048,
            temperature: 0.3,
            system: &system,
            messages: vec![Message {
                role: "user",
                content: raw,
            }],
        };
        let res = self
            .client
            .post(ENDPOINT)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&req)
            .send()
            .await
            .context("anthropic POST failed")?;
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("anthropic LLM {status}: {body}");
        }
        let parsed: Resp = serde_json::from_str(&body)
            .with_context(|| format!("parse anthropic response: {body}"))?;
        Ok(parsed
            .content
            .into_iter()
            .map(|b| b.text)
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_string())
    }

    fn name(&self) -> &'static str {
        "anthropic"
    }
}
