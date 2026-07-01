// SPDX-License-Identifier: GPL-3.0-only
//! Cloud pass-through proxy for the OpenAI surface (ADR 0036).
//!
//! When the served assistant backend is an OpenAI-compatible cloud
//! provider, the OpenAI-surface handlers forward the client's request
//! **verbatim** to the upstream provider instead of adapting it through
//! the `Assistant` trait. This preserves full wire fidelity — every
//! model, tool/function-calling, vision, and request parameter passes
//! through untouched — for near-zero code, and unlocks cloud
//! tool-calling (the Home Assistant device-control path).
//!
//! The only mutation Fono makes to the request body is defaulting the
//! `model` field when the client omits it; the API key is injected on
//! the outbound leg (never exposed to the client). The adapter remains
//! the universal floor for everything that is not proxyable (local
//! llama.cpp, Anthropic, realtime, and the Ollama-native surface).

use std::convert::Infallible;
use std::sync::OnceLock;

use bytes::Bytes;
use fono_assistant::CloudUpstream;
use futures::StreamExt;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Frame;
use hyper::{Response, StatusCode};

use super::access_log::ReqLog;
use super::{error_response, full, ResBody};

/// Shared outbound client (connection pool reused across requests).
fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// Forward a `/v1/chat/completions` request body to the cloud upstream
/// and relay the response (SSE stream or single JSON) back verbatim.
pub async fn forward_chat(
    upstream: &CloudUpstream,
    body: Bytes,
    log: &mut ReqLog,
) -> Response<ResBody> {
    let mut json: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return error_response(StatusCode::BAD_REQUEST, &format!("invalid JSON body: {e}"));
        }
    };
    let stream = json.get("stream").and_then(serde_json::Value::as_bool).unwrap_or(false);
    // Default the model only when the client omits or blanks it — an
    // explicit client-chosen model is honoured (ADR 0036).
    let client_model =
        json.get("model").and_then(serde_json::Value::as_str).filter(|s| !s.is_empty());
    // Reflect the effective model in the access log.
    log.set_model(client_model.map_or_else(|| upstream.model.clone(), ToOwned::to_owned));
    if client_model.is_none() {
        if let Some(obj) = json.as_object_mut() {
            obj.insert("model".to_string(), serde_json::Value::String(upstream.model.clone()));
        }
    }

    let mut req = client().post(&upstream.chat_url).json(&json);
    if !upstream.api_key.is_empty() {
        req = req.bearer_auth(&upstream.api_key);
    }
    if stream {
        req = req.header(reqwest::header::ACCEPT, "text/event-stream");
    }
    match req.send().await {
        Ok(resp) => relay(resp, stream, log).await,
        Err(e) => {
            tracing::warn!(target: "fono::llm::server", "proxy upstream request failed: {e:#}");
            error_response(StatusCode::BAD_GATEWAY, &format!("upstream request failed: {e}"))
        }
    }
}

/// Proxy `GET /v1/models` to the provider's `/models` endpoint so
/// clients discover the full catalogue. Returns `None` (caller falls
/// back to a single-model list) when no URL is derivable or the upstream
/// call fails.
pub async fn forward_models(upstream: &CloudUpstream) -> Option<Response<ResBody>> {
    let url = upstream.models_url.as_ref()?;
    let mut req = client().get(url);
    if !upstream.api_key.is_empty() {
        req = req.bearer_auth(&upstream.api_key);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.bytes().await.ok()?;
    Some(
        Response::builder()
            .status(StatusCode::OK)
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .body(full(body))
            .expect("proxy models response builder"),
    )
}

/// Relay a `reqwest` response into a hyper response, streaming the body
/// through unchanged (SSE) or buffering it (single JSON). The upstream
/// status code and content-type are preserved. For streaming responses
/// the access line is emitted from the relay task (ttft = first frame);
/// non-streaming responses are finalised by `route()`.
async fn relay(resp: reqwest::Response, stream: bool, log: &mut ReqLog) -> Response<ResBody> {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);

    if stream {
        let mut slog = log.defer(false);
        slog.set_status(status.as_u16());
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Bytes>(32);
        tokio::spawn(async move {
            let mut s = resp.bytes_stream();
            while let Some(item) = s.next().await {
                match item {
                    Ok(b) => {
                        slog.on_frame();
                        if tx.send(b).await.is_err() {
                            break; // client hung up
                        }
                    }
                    Err(e) => {
                        tracing::warn!(target: "fono::llm::server", "proxy stream error: {e:#}");
                        break;
                    }
                }
            }
            slog.emit();
        });
        let body_stream = futures::stream::poll_fn(move |cx| {
            rx.poll_recv(cx).map(|opt| opt.map(|b| Ok::<Frame<Bytes>, Infallible>(Frame::data(b))))
        });
        let body: ResBody = BodyExt::boxed(StreamBody::new(body_stream));
        return Response::builder()
            .status(status)
            .header(
                hyper::header::CONTENT_TYPE,
                content_type.unwrap_or_else(|| "text/event-stream".to_string()),
            )
            .header(hyper::header::CACHE_CONTROL, "no-cache")
            .body(body)
            .expect("proxy stream response builder");
    }

    match resp.bytes().await {
        Ok(b) => Response::builder()
            .status(status)
            .header(
                hyper::header::CONTENT_TYPE,
                content_type.unwrap_or_else(|| "application/json".to_string()),
            )
            .body(full(b))
            .expect("proxy response builder"),
        Err(e) => {
            error_response(StatusCode::BAD_GATEWAY, &format!("reading upstream response: {e}"))
        }
    }
}
