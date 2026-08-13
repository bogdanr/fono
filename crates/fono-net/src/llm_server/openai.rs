// SPDX-License-Identifier: GPL-3.0-only
//! OpenAI-compatible surface: `GET /v1/models`,
//! `POST /v1/chat/completions` (SSE stream or single JSON).
//!
//! The wire shapes mirror the subset of the OpenAI API that editors,
//! Open WebUI, LangChain, `llm`, and Fono's own `openai_compat_chat`
//! client exercise. Fields Fono does not honour (temperature, top_p, …)
//! are accepted and ignored so clients that always send them work.

use bytes::Bytes;
use hyper::body::Incoming;
use hyper::{Request, StatusCode};
use serde::{Deserialize, Serialize};

use super::access_log::{provider_label, Mode, ReqLog, StreamLog};
use super::messages::{
    collect_reply, gen_id, make_context, read_body_bytes, read_reply, split_messages, stream_body,
    unix_secs, Reply, WireMessage,
};
use super::{error_response, json_ok, sse_response, ResBody, ServerCtx};
use hyper::Response;

// --- GET /v1/models ------------------------------------------------------

#[derive(Serialize)]
struct ModelList {
    object: &'static str,
    data: Vec<ModelEntry>,
}

#[derive(Serialize)]
struct ModelEntry {
    id: String,
    object: &'static str,
    created: u64,
    owned_by: &'static str,
}

pub async fn models(ctx: &ServerCtx, log: &mut ReqLog) -> Response<ResBody> {
    // Proxy mode: surface the provider's full model catalogue so clients
    // discover every model they can request (ADR 0036). Falls back to
    // the single served model if the upstream call fails.
    if let Some(upstream) = (ctx.upstream)() {
        log.set_target(Mode::Proxy(provider_label(&upstream.chat_url)), String::new());
        if let Some(resp) = super::proxy::forward_models(&upstream).await {
            return resp;
        }
    } else {
        log.set_target(Mode::Adapt, ctx.cfg.model_name.clone());
    }
    let list = ModelList {
        object: "list",
        data: vec![ModelEntry {
            id: ctx.cfg.model_name.clone(),
            object: "model",
            created: unix_secs(),
            owned_by: "fono",
        }],
    };
    json_ok(&list)
}

// --- POST /v1/chat/completions -------------------------------------------

#[derive(Deserialize)]
struct ChatRequest {
    #[serde(default)]
    messages: Vec<WireMessage>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    max_tokens: Option<u32>,
    /// Tool descriptors the client offers. Kept as raw JSON: the shape is
    /// the OpenAI one the prompt renderer already reads, and a client may
    /// send schema fields Fono neither needs nor should reject.
    #[serde(default)]
    tools: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct ChatCompletion {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: Vec<Choice>,
}

#[derive(Serialize)]
struct Choice {
    index: u32,
    message: Message,
    finish_reason: &'static str,
}

#[derive(Serialize)]
struct Message {
    role: &'static str,
    /// `null` when the model called a tool instead of speaking — clients
    /// distinguish the two cases by this field being absent, not empty.
    content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<OutToolCall>,
}

/// A tool call on the way out. `arguments` is a JSON-encoded *string* per
/// the spec, which is also what the parser hands back.
#[derive(Serialize, Clone)]
struct OutToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: OutToolFunction,
}

#[derive(Serialize, Clone)]
struct OutToolFunction {
    name: String,
    arguments: String,
}

fn out_calls(calls: &[fono_assistant::history::ToolCall]) -> Vec<OutToolCall> {
    calls
        .iter()
        .map(|c| OutToolCall {
            id: c.id.clone(),
            kind: "function",
            function: OutToolFunction { name: c.name.clone(), arguments: c.arguments.clone() },
        })
        .collect()
}

pub async fn chat(req: Request<Incoming>, ctx: &ServerCtx, log: &mut ReqLog) -> Response<ResBody> {
    let bytes = match read_body_bytes(req).await {
        Ok(b) => b,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, &e),
    };
    // Proxy fast-lane: forward verbatim to the OpenAI-compat cloud
    // upstream (full tool/vision/parameter fidelity, ADR 0036).
    if let Some(upstream) = (ctx.upstream)() {
        log.set_target(Mode::Proxy(provider_label(&upstream.chat_url)), upstream.model.clone());
        return super::proxy::forward_chat(&upstream, bytes, log).await;
    }
    // Adapter path: drive the `Assistant` trait.
    let body: ChatRequest = match serde_json::from_slice(&bytes) {
        Ok(b) => b,
        Err(e) => {
            return error_response(StatusCode::BAD_REQUEST, &format!("invalid JSON body: {e}"))
        }
    };
    let split = match split_messages(&body.messages) {
        Ok(s) => s,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, &e),
    };
    let Some(assistant) = (ctx.assistant)() else {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "no assistant backend configured");
    };
    let ctx_obj = make_context(&split, body.max_tokens, &body.tools);
    let model = ctx.cfg.model_name.clone();
    log.set_target(Mode::Adapt, model.clone());

    // A tool call has to be read out of the finished reply, so a request
    // that offers tools is generated whole even when the client asked to
    // stream. Nothing is lost: a call is a single small object, and
    // trickling it out word by word would only let a client act on half
    // an argument list.
    if body.stream && body.tools.is_empty() {
        return stream_chat(assistant, split.user_text, ctx_obj, model, log.defer(true));
    }
    let text = match collect_reply(assistant, split.user_text, ctx_obj).await {
        Ok(t) => t,
        Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    let reply = read_reply(text, !body.tools.is_empty());
    if body.stream {
        return stream_one_shot(&reply, model);
    }
    json_ok(&ChatCompletion {
        id: gen_id("chatcmpl-"),
        object: "chat.completion",
        created: unix_secs(),
        model,
        choices: vec![Choice {
            index: 0,
            finish_reason: reply.finish_reason(),
            message: Message {
                role: "assistant",
                content: reply.tool_calls.is_empty().then(|| reply.text.clone()),
                tool_calls: out_calls(&reply.tool_calls),
            },
        }],
    })
}

/// Frame an already-finished reply as a short SSE stream, for the client
/// that asked to stream and also offered tools.
fn stream_one_shot(reply: &Reply, model: String) -> Response<ResBody> {
    let id = gen_id("chatcmpl-");
    let created = unix_secs();
    let chunk = |delta: Delta, finish: Option<&'static str>| {
        sse_line(&ChatChunk {
            id: id.clone(),
            object: "chat.completion.chunk",
            created,
            model: model.clone(),
            choices: vec![ChunkChoice { index: 0, delta, finish_reason: finish }],
        })
    };
    let frames = vec![
        chunk(Delta { role: Some("assistant"), ..Delta::default() }, None),
        chunk(
            Delta {
                content: reply.tool_calls.is_empty().then(|| reply.text.clone()),
                tool_calls: out_calls(&reply.tool_calls),
                ..Delta::default()
            },
            None,
        ),
        chunk(Delta::default(), Some(reply.finish_reason())),
        Bytes::from_static(b"data: [DONE]\n\n"),
    ];
    sse_response(super::messages::fixed_body(frames))
}

/// SSE chunk shapes (`object: "chat.completion.chunk"`).
#[derive(Serialize)]
struct ChatChunk {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: Vec<ChunkChoice>,
}

#[derive(Serialize)]
struct ChunkChoice {
    index: u32,
    delta: Delta,
    finish_reason: Option<&'static str>,
}

#[derive(Serialize, Default)]
struct Delta {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<OutToolCall>,
}

fn sse_line<T: Serialize>(value: &T) -> Bytes {
    let json = serde_json::to_string(value).unwrap_or_default();
    Bytes::from(format!("data: {json}\n\n"))
}

fn stream_chat(
    assistant: std::sync::Arc<dyn fono_assistant::traits::Assistant>,
    user_text: String,
    ctx_obj: fono_assistant::traits::AssistantContext,
    model: String,
    slog: StreamLog,
) -> Response<ResBody> {
    let id = gen_id("chatcmpl-");
    let created = unix_secs();

    // Opening chunk announces the assistant role with empty content.
    let open = sse_line(&ChatChunk {
        id: id.clone(),
        object: "chat.completion.chunk",
        created,
        model: model.clone(),
        choices: vec![ChunkChoice {
            index: 0,
            delta: Delta { role: Some("assistant"), ..Delta::default() },
            finish_reason: None,
        }],
    });

    let enc_id = id.clone();
    let enc_model = model.clone();
    let encode = move |text: &str| {
        sse_line(&ChatChunk {
            id: enc_id.clone(),
            object: "chat.completion.chunk",
            created,
            model: enc_model.clone(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: Delta { content: Some(text.to_owned()), ..Delta::default() },
                finish_reason: None,
            }],
        })
    };

    // Final chunk carries finish_reason then the SSE terminator.
    let final_chunk = sse_line(&ChatChunk {
        id,
        object: "chat.completion.chunk",
        created,
        model,
        choices: vec![ChunkChoice {
            index: 0,
            delta: Delta::default(),
            finish_reason: Some("stop"),
        }],
    });
    let tail = vec![final_chunk, Bytes::from_static(b"data: [DONE]\n\n")];

    let body = stream_body(assistant, user_text, ctx_obj, Some(open), encode, tail, Some(slog));
    sse_response(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_line_is_data_framed() {
        let chunk = ChatChunk {
            id: "x".into(),
            object: "chat.completion.chunk",
            created: 0,
            model: "fono".into(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: Delta { content: Some("hi".into()), ..Delta::default() },
                finish_reason: None,
            }],
        };
        let line = sse_line(&chunk);
        let s = String::from_utf8(line.to_vec()).unwrap();
        assert!(s.starts_with("data: "));
        assert!(s.ends_with("\n\n"));
        assert!(s.contains("\"content\":\"hi\""));
        // Role is omitted when None.
        assert!(!s.contains("\"role\""));
    }

    #[test]
    fn delta_skips_none_fields() {
        let d = Delta::default();
        let s = serde_json::to_string(&d).unwrap();
        assert_eq!(s, "{}");
    }

    #[test]
    fn a_tool_call_is_spelled_the_way_the_spec_says() {
        // `content` must be null rather than empty, `arguments` a
        // JSON-encoded string rather than an object, and `type` present —
        // clients branch on all three.
        let calls = out_calls(&[fono_assistant::history::ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: "{\"path\":\"a.txt\"}".into(),
        }]);
        let msg = Message { role: "assistant", content: None, tool_calls: calls };
        let s = serde_json::to_string(&msg).unwrap();
        assert!(s.contains("\"content\":null"));
        assert!(s.contains("\"type\":\"function\""));
        assert!(s.contains(r#""arguments":"{\"path\":\"a.txt\"}""#));
    }

    #[test]
    fn a_plain_reply_carries_no_tool_calls_field() {
        let msg = Message { role: "assistant", content: Some("hi".into()), tool_calls: Vec::new() };
        let s = serde_json::to_string(&msg).unwrap();
        assert!(!s.contains("tool_calls"));
    }
}
