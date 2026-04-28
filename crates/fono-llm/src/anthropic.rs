// SPDX-License-Identifier: GPL-3.0-only
//! Anthropic Messages API client.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::traits::{looks_like_clarification, user_prompt, FormatContext, TextFormatter};

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
            client: crate::openai_compat::warm_client(),
        }
    }
}

#[derive(Serialize)]
struct Req<'a> {
    model: &'a str,
    max_tokens: u32,
    temperature: f32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stop_sequences: Vec<&'a str>,
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
        let user = user_prompt(raw);
        let req = Req {
            model: &self.model,
            // Latency plan L19 — short cleanup outputs.
            max_tokens: 512,
            temperature: 0.2,
            stop_sequences: vec!["\n\n"],
            system: &system,
            messages: vec![Message {
                role: "user",
                content: &user,
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
        let out = parsed
            .content
            .into_iter()
            .map(|b| b.text)
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_string();
        if looks_like_clarification(&out) {
            anyhow::bail!(
                "anthropic LLM returned a clarification reply instead of a cleaned transcript; \
                 falling back to raw text. response: {out:?}"
            );
        }
        Ok(out)
    }

    fn name(&self) -> &'static str {
        "anthropic"
    }

    async fn prewarm(&self) -> Result<()> {
        // Anthropic's `/v1/models` endpoint exists; cheap GET warms the
        // TLS+HTTP/2 connection. Failures are non-fatal.
        let res = self
            .client
            .get("https://api.anthropic.com/v1/models")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
            .context("anthropic prewarm")?;
        let _ = res.bytes().await;
        Ok(())
    }
}
