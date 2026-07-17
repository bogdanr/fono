// SPDX-License-Identifier: GPL-3.0-only
//! End-to-end integration test for the local LLM server (ADR 0036).
//!
//! Stands up a real [`LlmServer`] on a loopback ephemeral port, backed
//! by a mock `Arc<dyn Assistant>` that yields scripted token deltas,
//! and drives it with a real `reqwest` client over both wire formats:
//!
//! * OpenAI `GET /v1/models`, `POST /v1/chat/completions` (stream + non-stream),
//! * Ollama `GET /api/tags`, `POST /api/chat` (stream + non-stream),
//! * auth (401) + not-found (404) fallbacks.
//!
//! This is the "real client ↔ real server ↔ mock assistant" gate for
//! Phase 1 of
//! `plans/2026-07-01-local-llm-openai-ollama-server-v1.md`.

#![cfg(feature = "llm-server")]

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use fono_assistant::traits::{Assistant, AssistantContext, TokenDelta};
use fono_net::{LlmServer, LlmServerConfig};
use futures::stream::BoxStream;
use serde_json::Value;

/// Mock assistant that streams a fixed sequence of text deltas,
/// regardless of the prompt.
struct MockAssistant {
    deltas: Vec<String>,
}

#[async_trait]
impl Assistant for MockAssistant {
    async fn reply_stream(
        &self,
        _user_text: &str,
        _ctx: &AssistantContext,
    ) -> Result<BoxStream<'static, Result<TokenDelta>>> {
        let deltas = self.deltas.clone();
        let stream = futures::stream::iter(deltas.into_iter().map(|t| Ok(TokenDelta::text(t))));
        Ok(Box::pin(stream))
    }

    fn name(&self) -> &'static str {
        "mock"
    }
}

async fn start_server() -> (fono_net::LlmServerHandle, String) {
    let mock =
        Arc::new(MockAssistant { deltas: vec!["Hello".into(), ", ".into(), "world".into()] });
    let cfg = LlmServerConfig { port: 0, ..LlmServerConfig::default() };
    let handle = LlmServer::with_fixed(cfg, mock).start().await.expect("server starts");
    let base = format!("http://{}", handle.local_addr());
    (handle, base)
}

#[tokio::test]
async fn openai_models_lists_the_configured_model() {
    let (handle, base) = start_server().await;
    let client = reqwest::Client::new();

    let resp = client.get(format!("{base}/v1/models")).send().await.expect("request");
    assert_eq!(resp.status(), 200);
    let body: Value = serde_json::from_str(&resp.text().await.unwrap()).unwrap();
    assert_eq!(body["object"], "list");
    assert_eq!(body["data"][0]["id"], "fono");
    assert_eq!(body["data"][0]["object"], "model");

    handle.shutdown().await;
}

#[tokio::test]
async fn openai_chat_non_stream_concatenates_deltas() {
    let (handle, base) = start_server().await;
    let client = reqwest::Client::new();

    let req = serde_json::json!({
        "model": "fono",
        "stream": false,
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(req.to_string())
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    let body: Value = serde_json::from_str(&resp.text().await.unwrap()).unwrap();
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["message"]["role"], "assistant");
    assert_eq!(body["choices"][0]["message"]["content"], "Hello, world");
    assert_eq!(body["choices"][0]["finish_reason"], "stop");

    handle.shutdown().await;
}

#[tokio::test]
async fn openai_chat_stream_emits_sse_chunks_then_done() {
    let (handle, base) = start_server().await;
    let client = reqwest::Client::new();

    let req = serde_json::json!({
        "model": "fono",
        "stream": true,
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(req.to_string())
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
    let text = resp.text().await.unwrap();

    // Every frame is `data: …`; the terminal frame is `data: [DONE]`.
    assert!(text.contains("data: "));
    assert!(text.trim_end().ends_with("data: [DONE]"));
    // Concatenating the delta contents reproduces the full reply.
    let mut assembled = String::new();
    for line in text.lines() {
        let Some(payload) = line.strip_prefix("data: ") else { continue };
        if payload == "[DONE]" {
            continue;
        }
        let chunk: Value = serde_json::from_str(payload).unwrap();
        if let Some(c) = chunk["choices"][0]["delta"]["content"].as_str() {
            assembled.push_str(c);
        }
    }
    assert_eq!(assembled, "Hello, world");

    handle.shutdown().await;
}

#[tokio::test]
async fn ollama_tags_lists_the_model() {
    let (handle, base) = start_server().await;
    let client = reqwest::Client::new();

    let resp = client.get(format!("{base}/api/tags")).send().await.expect("request");
    assert_eq!(resp.status(), 200);
    let body: Value = serde_json::from_str(&resp.text().await.unwrap()).unwrap();
    assert_eq!(body["models"][0]["name"], "fono");
    assert_eq!(body["models"][0]["details"]["format"], "gguf");

    handle.shutdown().await;
}

#[tokio::test]
async fn ollama_chat_stream_emits_ndjson_then_done() {
    let (handle, base) = start_server().await;
    let client = reqwest::Client::new();

    let req = serde_json::json!({
        "model": "fono",
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let resp = client
        .post(format!("{base}/api/chat"))
        .header("content-type", "application/json")
        .body(req.to_string())
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();

    // NDJSON: one JSON object per line; last carries `done: true`.
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut assembled = String::new();
    for (i, line) in lines.iter().enumerate() {
        let obj: Value = serde_json::from_str(line).unwrap();
        assert_eq!(obj["message"]["role"], "assistant");
        if i == lines.len() - 1 {
            assert_eq!(obj["done"], true);
            assert_eq!(obj["done_reason"], "stop");
        } else {
            assert_eq!(obj["done"], false);
            assembled.push_str(obj["message"]["content"].as_str().unwrap_or(""));
        }
    }
    assert_eq!(assembled, "Hello, world");

    handle.shutdown().await;
}

#[tokio::test]
async fn ollama_chat_non_stream_returns_single_object() {
    let (handle, base) = start_server().await;
    let client = reqwest::Client::new();

    let req = serde_json::json!({
        "model": "fono",
        "stream": false,
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let resp = client
        .post(format!("{base}/api/chat"))
        .header("content-type", "application/json")
        .body(req.to_string())
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    let body: Value = serde_json::from_str(&resp.text().await.unwrap()).unwrap();
    assert_eq!(body["message"]["content"], "Hello, world");
    assert_eq!(body["done"], true);

    handle.shutdown().await;
}

#[tokio::test]
async fn unknown_route_is_404() {
    let (handle, base) = start_server().await;
    let client = reqwest::Client::new();

    let resp = client.get(format!("{base}/nope")).send().await.expect("request");
    assert_eq!(resp.status(), 404);

    handle.shutdown().await;
}

#[tokio::test]
async fn bad_json_body_is_400() {
    let (handle, base) = start_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body("{ not json".to_string())
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400);

    handle.shutdown().await;
}

#[tokio::test]
async fn missing_user_message_is_400() {
    let (handle, base) = start_server().await;
    let client = reqwest::Client::new();

    let req = serde_json::json!({ "model": "fono", "messages": [] });
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(req.to_string())
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400);

    handle.shutdown().await;
}

#[tokio::test]
async fn loopback_is_trusted_even_with_auth_on() {
    // With auth enabled and a verifier that rejects everything, a loopback
    // caller (the local owner) is still admitted without a token — this is
    // the deliberate no-bootstrap-lockout rule. The non-loopback 401 path
    // is unit-tested exhaustively in `fono_net::auth::tests` (loopback is
    // untestable over a real socket, where the peer is always 127.0.0.1).
    let mock = Arc::new(MockAssistant { deltas: vec!["ok".into()] });
    let cfg = LlmServerConfig { port: 0, auth_enabled: true, ..LlmServerConfig::default() };
    let reject_all: fono_net::AuthVerifier = Arc::new(|_tok: &str| None);
    let usage: fono_net::UsageSink = Arc::new(|_id| {});
    let handle = LlmServer::with_fixed(cfg, mock)
        .with_auth(reject_all, usage)
        .start()
        .await
        .expect("server starts");
    let base = format!("http://{}", handle.local_addr());
    let client = reqwest::Client::new();

    // No token, but loopback → admitted.
    let resp = client.get(format!("{base}/v1/models")).send().await.expect("request");
    assert_eq!(resp.status(), 200);

    handle.shutdown().await;
}

// --- cloud pass-through proxy (ADR 0036) ---------------------------------

use std::convert::Infallible;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;

/// A minimal mock upstream that mimics an OpenAI-compatible cloud
/// provider. `/chat/completions` echoes back the `model` it received (so
/// tests can assert verbatim forwarding + default injection) and honours
/// `Authorization`. `/models` returns a two-entry catalogue.
async fn start_mock_upstream() -> (tokio::task::JoinHandle<()>, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let join = tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { break };
            tokio::spawn(async move {
                let io = TokioIo::new(sock);
                let svc = service_fn(handle_upstream);
                let _ = http1::Builder::new().serve_connection(io, svc).await;
            });
        }
    });
    (join, format!("http://{addr}"))
}

async fn handle_upstream(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let path = req.uri().path().to_owned();
    let auth = req
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    match path.as_str() {
        "/models" => {
            let body = serde_json::json!({
                "object": "list",
                "data": [
                    { "id": "up-model-a", "object": "model" },
                    { "id": "up-model-b", "object": "model" },
                ],
            });
            Ok(json_resp(&body, &auth))
        }
        "/chat/completions" => {
            let bytes = req.into_body().collect().await.unwrap().to_bytes();
            let sent: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
            let model = sent["model"].as_str().unwrap_or("<none>").to_owned();
            // Echo the received model back so the test can assert what
            // Fono forwarded (verbatim vs. default-injected).
            let body = serde_json::json!({
                "id": "chatcmpl-upstream",
                "object": "chat.completion",
                "model": model,
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "from-upstream" },
                    "finish_reason": "stop",
                }],
                "echo_auth": auth,
            });
            Ok(json_resp(&body, &auth))
        }
        _ => {
            let mut resp = Response::new(Full::new(Bytes::from_static(b"not found")));
            *resp.status_mut() = hyper::StatusCode::NOT_FOUND;
            Ok(resp)
        }
    }
}

fn json_resp(value: &serde_json::Value, _auth: &str) -> Response<Full<Bytes>> {
    let bytes = serde_json::to_vec(value).unwrap();
    Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(bytes)))
        .unwrap()
}

/// Build an [`LlmServer`] whose OpenAI surface proxies to `upstream_base`.
async fn start_server_with_upstream(
    upstream_base: &str,
    api_key: &str,
) -> (fono_net::LlmServerHandle, String) {
    let up = Arc::new(fono_assistant::CloudUpstream {
        chat_url: format!("{upstream_base}/chat/completions"),
        models_url: Some(format!("{upstream_base}/models")),
        api_key: api_key.to_string(),
        model: "default-model".to_string(),
    });
    let mock = Arc::new(MockAssistant { deltas: vec!["adapter".into()] });
    let cfg = LlmServerConfig { port: 0, ..LlmServerConfig::default() };
    let provider: fono_net::UpstreamProvider = {
        let up = Arc::clone(&up);
        Arc::new(move || Some(Arc::clone(&up)))
    };
    let handle = LlmServer::with_fixed(cfg, mock)
        .with_upstream(provider)
        .start()
        .await
        .expect("server starts");
    let base = format!("http://{}", handle.local_addr());
    (handle, base)
}

#[tokio::test]
async fn proxy_chat_forwards_client_model_verbatim() {
    let (up_join, up_base) = start_mock_upstream().await;
    let (handle, base) = start_server_with_upstream(&up_base, "sk-key").await;
    let client = reqwest::Client::new();

    let req = serde_json::json!({
        "model": "client-chosen",
        "stream": false,
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(req.to_string())
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    let body: Value = serde_json::from_str(&resp.text().await.unwrap()).unwrap();
    // The upstream echoed the model it received: the client's choice was
    // honoured verbatim (not overridden by the server default).
    assert_eq!(body["model"], "client-chosen");
    assert_eq!(body["choices"][0]["message"]["content"], "from-upstream");
    // The provider key was injected on the outbound leg.
    assert_eq!(body["echo_auth"], "Bearer sk-key");

    handle.shutdown().await;
    up_join.abort();
}

#[tokio::test]
async fn proxy_chat_injects_default_model_when_omitted() {
    let (up_join, up_base) = start_mock_upstream().await;
    let (handle, base) = start_server_with_upstream(&up_base, "sk-key").await;
    let client = reqwest::Client::new();

    // No `model` field → the server injects its resolved default.
    let req = serde_json::json!({
        "stream": false,
        "messages": [{ "role": "user", "content": "hi" }],
    });
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(req.to_string())
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    let body: Value = serde_json::from_str(&resp.text().await.unwrap()).unwrap();
    assert_eq!(body["model"], "default-model");

    handle.shutdown().await;
    up_join.abort();
}

#[tokio::test]
async fn proxy_models_surfaces_upstream_catalogue() {
    let (up_join, up_base) = start_mock_upstream().await;
    let (handle, base) = start_server_with_upstream(&up_base, "sk-key").await;
    let client = reqwest::Client::new();

    let resp = client.get(format!("{base}/v1/models")).send().await.expect("request");
    assert_eq!(resp.status(), 200);
    let body: Value = serde_json::from_str(&resp.text().await.unwrap()).unwrap();
    // The upstream's full two-model catalogue is surfaced, not Fono's
    // single served model.
    assert_eq!(body["data"][0]["id"], "up-model-a");
    assert_eq!(body["data"][1]["id"], "up-model-b");

    handle.shutdown().await;
    up_join.abort();
}
