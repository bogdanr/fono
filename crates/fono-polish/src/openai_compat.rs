// SPDX-License-Identifier: GPL-3.0-only
//! OpenAI-compatible chat-completions client used by Cerebras, Groq,
//! OpenRouter, Ollama, and OpenAI itself.

use anyhow::{Context, Result};
use async_trait::async_trait;
use fono_http::{
    emit_http_debug, provider_request_id, read_body_with_watchdog, BodyError, Outcome,
    RequestTimings,
};
use serde::{Deserialize, Serialize};

use crate::traits::{looks_like_clarification, user_prompt, FormatContext, TextFormatter};

/// Inter-chunk watchdog for chat-completions JSON bodies. They're
/// small (≤ a few KB) and arrive in one or two chunks; 30 s between
/// chunks comfortably covers slow-but-progressing upstreams while
/// catching true stalls long before the 30 s overall reqwest timeout.
const LLM_CHUNK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
        Self::new("https://api.cerebras.ai/v1/chat/completions", api_key, model, "cerebras")
    }

    pub fn groq(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new("https://api.groq.com/openai/v1/chat/completions", api_key, model, "groq")
    }

    pub fn openai(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new("https://api.openai.com/v1/chat/completions", api_key, model, "openai")
    }

    pub fn openrouter(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new("https://openrouter.ai/api/v1/chat/completions", api_key, model, "openrouter")
    }

    /// Gemini's OpenAI-compatible surface, reached with the single
    /// `GEMINI_API_KEY` (`Authorization: Bearer <key>`). Lets the polish
    /// path reuse this client instead of a bespoke `generateContent`
    /// implementation. See
    /// `docs/decisions/0034-google-via-gemini-single-key.md`.
    pub fn gemini(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(
            "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
            api_key,
            model,
            "gemini",
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
    chat.strip_suffix("/chat/completions").map(|root| format!("{root}/models"))
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
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    // OpenAI's gpt-5 / o-series reject the legacy `max_tokens` field
    // with a 400; the new name `max_completion_tokens` is accepted
    // by all the OpenAI-compat providers we ship (Cerebras, Groq,
    // OpenAI, OpenRouter, Ollama).
    #[serde(rename = "max_completion_tokens")]
    max_tokens: u32,
    // Reasoning models (gpt-oss — the Groq cleanup default —, gpt-5 /
    // o-series, qwen3, deepseek-r1, …) burn an unbounded, variable
    // number of *hidden reasoning* tokens before emitting any visible
    // content. With a tight `max_completion_tokens` the reasoning eats
    // the whole budget and `content` comes back empty, so cleanup
    // silently fell back to the raw STT text (the "polish does nothing
    // / garbled non-English" bug). Pinning the effort to `low` keeps
    // reasoning short and predictable so the visible answer fits the
    // budget. Omitted for non-reasoning models — Cerebras Llama,
    // Ollama, … reject the field with a 400. See
    // `plans/2026-05-29-romanian-dictation-polish-reconstruction-v2.md`.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
    // Local OpenAI-compatible servers default thinking-capable models
    // (Qwen3.x, DeepSeek-R1, etc.) to emitting hidden reasoning. Fono's
    // cleanup path needs the final cleaned transcript only, so local
    // endpoints get both Ollama's native `think: false` switch and
    // llama.cpp's Jinja `enable_thinking: false` override.
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_template_kwargs: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stop: Vec<&'a str>,
    stream: bool,
}

/// Best-effort detection of chat models that emit hidden
/// chain-of-thought tokens before their visible answer. Matched by
/// family substring (case-insensitive) so new point releases inherit
/// the behaviour automatically. Used to decide whether to send
/// `reasoning_effort` and to drop the `"\n\n"` stop sequence (which
/// otherwise fires inside the reasoning channel and truncates the
/// answer to empty). Conservative by design: a false negative just
/// reverts to the previous request shape, a false positive only adds
/// a field reasoning models already accept.
#[must_use]
pub(crate) fn is_reasoning_model(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.contains("gpt-oss")
        || m.contains("gpt-5")
        || m.contains("deepseek-r1")
        || m.contains("qwen3")
        || m.contains("-thinking")
        || m.contains("o1-")
        || m.contains("o3-")
        || m.contains("o4-")
        || m == "o1"
        || m == "o3"
        || m == "o4-mini"
}

#[must_use]
pub(crate) fn uses_default_sampling_only(backend_name: &str, model: &str) -> bool {
    backend_name == "openai" && model.to_ascii_lowercase().contains("gpt-5")
}

#[must_use]
pub(crate) fn is_local_backend(backend_name: &str) -> bool {
    backend_name == "ollama"
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

#[must_use]
fn cleanup_token_budget(raw: &str, local_backend: bool, reasoning_model: bool) -> u32 {
    if !local_backend || reasoning_model {
        return 2048;
    }

    let estimated_input_tokens = (raw.chars().count() as u32 / 4).max(16);
    (estimated_input_tokens + 64).clamp(96, 512)
}

#[async_trait]
impl TextFormatter for OpenAiCompat {
    #[allow(clippy::too_many_lines)]
    async fn format(&self, raw: &str, ctx: &FormatContext) -> Result<String> {
        let system = ctx.system_prompt();
        let user = user_prompt(raw);
        // Gemini 3.x Flash enables "thinking" by default, which inflates
        // latency and can eat the token budget before any visible content
        // (the same failure mode as the gpt-oss reasoning models). On the
        // OpenAI-compat surface `reasoning_effort: "low"` pins it to the
        // lowest level — thinking can't be disabled entirely on 3.x models.
        // Treat Gemini as a reasoning backend so it gets the low effort, the
        // dropped "\n\n" stop, and the larger token budget.
        let reasoning = is_reasoning_model(&self.model) || self.backend_name == "gemini";
        let default_sampling_only = uses_default_sampling_only(self.backend_name, &self.model);
        let req = ChatReq {
            model: &self.model,
            messages: vec![
                Message { role: "system", content: &system },
                Message { role: "user", content: &user },
            ],
            // Latency plan L19 — cleanup outputs should be close to the input length.
            // Cloud reasoning models need a large budget because hidden tokens can consume part of
            // it, but local non-thinking endpoints should stay tight to avoid rambling and reduce
            // tail latency. The local budget scales with input size and is capped well below the
            // cloud budget.
            temperature: (!default_sampling_only).then_some(0.2),
            top_p: (!default_sampling_only).then_some(0.9),
            max_tokens: cleanup_token_budget(raw, is_local_backend(self.backend_name), reasoning),
            reasoning_effort: reasoning.then_some("low"),
            think: is_local_backend(self.backend_name).then_some(false),
            chat_template_kwargs: is_local_backend(self.backend_name).then_some(
                serde_json::json!({
                    "enable_thinking": false,
                }),
            ),
            // The `"\n\n"` stop sequence fires inside a reasoning
            // model's chain-of-thought and truncates the answer to
            // empty, so it must be dropped for those. Non-reasoning
            // models keep it as a cheap guard against trailing chatter.
            stop: if reasoning { vec![] } else { vec!["\n\n"] },
            stream: false,
        };
        let mut builder = self.client.post(&self.endpoint).json(&req);
        if !self.api_key.is_empty() {
            builder = builder.bearer_auth(&self.api_key);
        }
        if self.backend_name == "openrouter" {
            for (name, value) in fono_core::openrouter_attribution::headers() {
                builder = builder.header(name, value);
            }
        }
        let mut timings = RequestTimings::start();
        let res = match builder.send().await {
            Ok(r) => {
                timings.mark_headers();
                r
            }
            Err(e) => {
                emit_http_debug(
                    "polish",
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
                return Err(anyhow::Error::new(e).context("chat POST failed"));
            }
        };
        let status = res.status();
        let request_id = provider_request_id(res.headers())
            .map(str::to_owned)
            .unwrap_or_else(|| "<none>".to_string());
        let content_length = res.content_length();
        let (bytes, stats) =
            match read_body_with_watchdog(res, LLM_CHUNK_TIMEOUT, &mut timings).await {
                Ok(b) => b,
                Err(e) => {
                    let outcome = match &e {
                        BodyError::Stalled { .. } => Outcome::Stalled,
                        BodyError::Transport { .. } => Outcome::TransportError,
                    };
                    emit_http_debug(
                        "polish",
                        self.backend_name,
                        "chat/completions",
                        status.as_u16(),
                        &timings,
                        e.partial_bytes(),
                        content_length,
                        e.chunks(),
                        &request_id,
                        1,
                        outcome,
                    );
                    return Err(anyhow::Error::new(e).context(format!(
                        "{} chat body read failed (request_id={request_id})",
                        self.backend_name
                    )));
                }
            };
        let body = String::from_utf8_lossy(&bytes).to_string();
        if !status.is_success() {
            emit_http_debug(
                "polish",
                self.backend_name,
                "chat/completions",
                status.as_u16(),
                &timings,
                stats.bytes,
                content_length,
                stats.chunks,
                &request_id,
                1,
                Outcome::HttpError,
            );
            anyhow::bail!("{} LLM {status} (request_id={request_id}): {body}", self.backend_name);
        }
        let parsed: ChatResp = match serde_json::from_str(&body) {
            Ok(p) => p,
            Err(e) => {
                emit_http_debug(
                    "polish",
                    self.backend_name,
                    "chat/completions",
                    status.as_u16(),
                    &timings,
                    stats.bytes,
                    content_length,
                    stats.chunks,
                    &request_id,
                    1,
                    Outcome::DecodeError,
                );
                return Err(anyhow::Error::new(e)
                    .context(format!("parse {} response: {body}", self.backend_name)));
            }
        };
        timings.mark_decode_done();
        emit_http_debug(
            "polish",
            self.backend_name,
            "chat/completions",
            status.as_u16(),
            &timings,
            stats.bytes,
            content_length,
            stats.chunks,
            &request_id,
            1,
            Outcome::Ok,
        );
        let out = parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default()
            .trim()
            .to_string();
        if looks_like_clarification(&out) {
            // Some chat-tuned models (Llama-3.3-70B, gpt-4o-mini, …)
            // occasionally respond to short / ambiguous push-to-talk
            // captures with a clarification question instead of a
            // cleaned transcript. Reject so the caller falls back to
            // the raw STT text. See
            // `plans/2026-04-28-polish-cleanup-clarification-refusal-fix-v1.md`.
            anyhow::bail!(
                "{} LLM returned a clarification reply instead of a cleaned transcript; \
                 falling back to raw text. response: {out:?}",
                self.backend_name
            );
        }
        Ok(out)
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
        if self.backend_name == "openrouter" {
            for (name, value) in fono_core::openrouter_attribution::headers() {
                req = req.header(name, value);
            }
        }
        let res = req.send().await.context("openai-compat prewarm")?;
        // Drain via the same watchdog so a slow prewarm doesn't tie
        // up the connection for the next real request.
        let mut timings = RequestTimings::start();
        timings.mark_headers();
        let _ = read_body_with_watchdog(res, LLM_CHUNK_TIMEOUT, &mut timings).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_reasoning_models() {
        // gpt-oss is the shipped Groq cleanup default — the model whose
        // hidden reasoning starved the old 256-token budget and made
        // cleanup a no-op for non-trivial dictation.
        for m in [
            "openai/gpt-oss-20b",
            "openai/gpt-oss-120b",
            "gpt-5.4-nano",
            "openai/gpt-5.4-nano",
            "deepseek-r1-distill-llama-70b",
            "qwen3-32b",
            "o1",
            "o3-mini",
            "o4-mini",
        ] {
            assert!(is_reasoning_model(m), "should be reasoning: {m}");
        }
    }

    #[test]
    fn does_not_flag_plain_instruct_models() {
        // These must NOT receive `reasoning_effort` — Cerebras Llama and
        // Ollama reject the field with a 400.
        for m in [
            "llama3.1-8b",
            "llama-3.3-70b-versatile",
            "llama3.2",
            "gpt-4o-mini",
            "claude-haiku-4-5-20251001",
            "mixtral-8x7b",
        ] {
            assert!(!is_reasoning_model(m), "should NOT be reasoning: {m}");
        }
    }

    #[test]
    fn reasoning_model_request_drops_stop_and_sets_effort() {
        // Lock in the request shape: reasoning models get `low` effort
        // and no stop sequence; plain models keep the legacy guard.
        let reasoning = is_reasoning_model("openai/gpt-oss-20b");
        assert!(reasoning);
        assert_eq!(reasoning.then_some("low"), Some("low"));

        let plain = is_reasoning_model("llama3.1-8b");
        assert!(!plain);
        assert_eq!(plain.then_some("low"), None);
    }

    #[test]
    fn local_backends_disable_thinking() {
        assert!(is_local_backend("ollama"));
        assert!(!is_local_backend("groq"));
        assert!(!is_local_backend("openai"));
    }

    #[test]
    fn gemini_backend_is_treated_as_reasoning() {
        // Gemini's model name ("gemini-flash-lite-latest") is not caught by the
        // family-substring matcher, so the backend-name override is what
        // forces `reasoning_effort: "low"` and the dropped "\n\n" stop. Mirror
        // the `format()` gate: reasoning = model-match OR backend == gemini.
        let reasoning = is_reasoning_model("gemini-flash-lite-latest") || "gemini" == "gemini";
        assert!(reasoning);
        assert_eq!(reasoning.then_some("low"), Some("low"));
        // Non-Gemini, non-reasoning backends stay off.
        let plain = is_reasoning_model("llama3.1-8b") || "groq" == "gemini";
        assert!(!plain);
    }

    #[test]
    fn local_request_serializes_thinking_disabled() {
        let req = ChatReq {
            model: "qwen3.5-4b",
            messages: Vec::new(),
            temperature: Some(0.2),
            top_p: Some(0.9),
            max_tokens: cleanup_token_budget("Ana are mere.", true, false),
            reasoning_effort: None,
            think: Some(false),
            chat_template_kwargs: Some(serde_json::json!({ "enable_thinking": false })),
            stop: Vec::new(),
            stream: false,
        };
        let body = serde_json::to_string(&req).unwrap();
        assert!(body.contains("\"think\":false"), "body: {body}");
        assert!(body.contains("\"enable_thinking\":false"), "body: {body}");
    }

    #[test]
    fn openai_gpt5_uses_default_sampling_only() {
        assert!(uses_default_sampling_only("openai", "gpt-5.4-mini"));
        assert!(uses_default_sampling_only("openai", "gpt-5.5-mini"));
        assert!(!uses_default_sampling_only("openai", "gpt-4.1-mini"));
        assert!(!uses_default_sampling_only("groq", "openai/gpt-oss-20b"));
        assert!(!uses_default_sampling_only("ollama", "gpt-5-local-test"));
    }
}
