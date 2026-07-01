// SPDX-License-Identifier: GPL-3.0-only
//! Ollama-native surface: `GET /api/tags`, `POST /api/chat`
//! (NDJSON stream or single JSON), `GET /api/version`.
//!
//! This is the path Home Assistant's Ollama conversation integration
//! and Ollama-hardcoded tools probe. The chat body reuses the same
//! `messages[]` split + `Assistant::reply_stream` as the OpenAI
//! surface; only the framing differs (NDJSON, one JSON object per line,
//! `done: true` on the last).

use bytes::Bytes;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use serde::{Deserialize, Serialize};

use super::access_log::{Mode, ReqLog, StreamLog};
use super::messages::{
    collect_reply, make_context, read_json, rfc3339_now, split_messages, stream_body, WireMessage,
};
use super::{error_response, json_ok, ndjson_response, ResBody, ServerCtx};

// --- GET /api/version ----------------------------------------------------

#[derive(Serialize)]
struct Version {
    version: String,
}

pub fn version(ctx: &ServerCtx) -> Response<ResBody> {
    json_ok(&Version { version: ctx.cfg.server_version.clone() })
}

// --- GET /api/tags -------------------------------------------------------

#[derive(Serialize)]
struct TagList {
    models: Vec<TagEntry>,
}

#[derive(Serialize)]
struct TagEntry {
    name: String,
    model: String,
    modified_at: String,
    size: u64,
    digest: String,
    details: TagDetails,
}

#[derive(Serialize)]
struct TagDetails {
    format: &'static str,
    family: &'static str,
    families: Vec<&'static str>,
    parameter_size: &'static str,
    quantization_level: &'static str,
}

pub fn tags(ctx: &ServerCtx, log: &mut ReqLog) -> Response<ResBody> {
    log.set_target(Mode::Adapt, String::new());
    let name = ctx.cfg.model_name.clone();
    let list = TagList {
        models: vec![TagEntry {
            name: name.clone(),
            model: name,
            modified_at: rfc3339_now(),
            size: 0,
            digest: String::new(),
            details: TagDetails {
                format: "gguf",
                family: "fono",
                families: vec!["fono"],
                parameter_size: "",
                quantization_level: "",
            },
        }],
    };
    json_ok(&list)
}

// --- POST /api/chat ------------------------------------------------------

#[derive(Deserialize)]
struct ChatRequest {
    #[serde(default)]
    messages: Vec<WireMessage>,
    /// Ollama defaults `stream` to `true` when the field is absent.
    #[serde(default = "default_true")]
    stream: bool,
    #[serde(default)]
    options: Option<Options>,
}

#[derive(Deserialize)]
struct Options {
    #[serde(default)]
    num_predict: Option<i64>,
}

fn default_true() -> bool {
    true
}

#[derive(Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct ChatResponse {
    model: String,
    created_at: String,
    message: ChatMessage,
    done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    done_reason: Option<&'static str>,
}

pub async fn chat(req: Request<Incoming>, ctx: &ServerCtx, log: &mut ReqLog) -> Response<ResBody> {
    let body: ChatRequest = match read_json(req).await {
        Ok(b) => b,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, &e),
    };
    let split = match split_messages(&body.messages) {
        Ok(s) => s,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, &e),
    };
    let Some(assistant) = (ctx.assistant)() else {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "no assistant backend configured");
    };
    // Ollama's `num_predict` maps onto the per-turn token cap. Negative
    // means "unbounded" in Ollama; treat that (and 0) as no cap.
    let max_tokens = body
        .options
        .as_ref()
        .and_then(|o| o.num_predict)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0);
    let ctx_obj = make_context(&split, max_tokens);
    let model = ctx.cfg.model_name.clone();
    log.set_target(Mode::Adapt, model.clone());

    if body.stream {
        stream_chat(assistant, split.user_text, ctx_obj, model, log.defer(true))
    } else {
        match collect_reply(assistant, split.user_text, ctx_obj).await {
            Ok(text) => json_ok(&ChatResponse {
                model,
                created_at: rfc3339_now(),
                message: ChatMessage { role: "assistant", content: text },
                done: true,
                done_reason: Some("stop"),
            }),
            Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e),
        }
    }
}

fn ndjson_line<T: Serialize>(value: &T) -> Bytes {
    let mut json = serde_json::to_vec(value).unwrap_or_default();
    json.push(b'\n');
    Bytes::from(json)
}

fn stream_chat(
    assistant: std::sync::Arc<dyn fono_assistant::traits::Assistant>,
    user_text: String,
    ctx_obj: fono_assistant::traits::AssistantContext,
    model: String,
    slog: StreamLog,
) -> Response<ResBody> {
    let enc_model = model.clone();
    let encode = move |text: &str| {
        ndjson_line(&ChatResponse {
            model: enc_model.clone(),
            created_at: rfc3339_now(),
            message: ChatMessage { role: "assistant", content: text.to_owned() },
            done: false,
            done_reason: None,
        })
    };

    // Terminal line: empty message content, `done: true`.
    let final_line = ndjson_line(&ChatResponse {
        model,
        created_at: rfc3339_now(),
        message: ChatMessage { role: "assistant", content: String::new() },
        done: true,
        done_reason: Some("stop"),
    });

    let body =
        stream_body(assistant, user_text, ctx_obj, None, encode, vec![final_line], Some(slog));
    ndjson_response(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ndjson_line_terminated_by_newline() {
        let resp = ChatResponse {
            model: "fono".into(),
            created_at: "2026-07-01T00:00:00Z".into(),
            message: ChatMessage { role: "assistant", content: "hi".into() },
            done: false,
            done_reason: None,
        };
        let line = ndjson_line(&resp);
        let s = String::from_utf8(line.to_vec()).unwrap();
        assert!(s.ends_with('\n'));
        assert!(!s.trim_end().contains('\n'), "exactly one line");
        assert!(s.contains("\"done\":false"));
        // done_reason omitted when None.
        assert!(!s.contains("done_reason"));
    }

    #[test]
    fn stream_defaults_to_true_when_absent() {
        let req: ChatRequest = serde_json::from_str(r#"{"messages":[]}"#).unwrap();
        assert!(req.stream);
    }

    #[test]
    fn stream_can_be_disabled() {
        let req: ChatRequest = serde_json::from_str(r#"{"messages":[],"stream":false}"#).unwrap();
        assert!(!req.stream);
    }
}
