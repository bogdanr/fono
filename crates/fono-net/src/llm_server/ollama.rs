// SPDX-License-Identifier: GPL-3.0-only
//! Ollama-native surface: `GET /api/tags`, `POST /api/show`, `POST /api/chat`
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
use sha2::{Digest, Sha256};

use super::access_log::{Mode, ReqLog, StreamLog};
use super::messages::{
    collect_reply, fixed_body, make_context, read_json, read_reply, rfc3339_now, split_messages,
    stream_body, WireMessage,
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
    let facts = &ctx.cfg.model_facts;
    let list = TagList {
        models: vec![TagEntry {
            name: name.clone(),
            model: name,
            modified_at: rfc3339_now(),
            size: facts.size_bytes,
            digest: digest(ctx),
            details: details(),
        }],
    };
    json_ok(&list)
}

/// A digest that is never empty, because several clients read an empty one as
/// "this model is not installed" and refuse the endpoint without asking it
/// anything. Falls back to hashing the served model's name, which identifies it
/// as well as a name can when there are no weights on disk to measure.
fn digest(ctx: &ServerCtx) -> String {
    if !ctx.cfg.model_facts.digest.is_empty() {
        return ctx.cfg.model_facts.digest.clone();
    }
    let mut hasher = Sha256::new();
    hasher.update(ctx.cfg.model_name.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn details() -> TagDetails {
    TagDetails {
        format: "gguf",
        family: "fono",
        families: vec!["fono"],
        parameter_size: "",
        quantization_level: "",
    }
}

// --- POST /api/show ------------------------------------------------------

/// What a client learns about the served model without sending it a prompt.
///
/// `capabilities` is the field worth the endpoint: a client that wants to call
/// tools looks for `"tools"` here, and a client that cannot find the endpoint at
/// all concludes tools are unavailable and silently drops them from its
/// requests. Fono reads a tool call out of any model's finished reply, so the
/// capability holds whichever backend is serving.
#[derive(Serialize)]
struct ShowResponse {
    /// Ollama returns the recipe that built the model. Fono has no such recipe,
    /// and an empty string is the honest answer — clients display it.
    modelfile: String,
    parameters: String,
    template: String,
    details: TagDetails,
    model_info: serde_json::Value,
    capabilities: Vec<&'static str>,
    modified_at: String,
}

pub fn show(ctx: &ServerCtx, log: &mut ReqLog) -> Response<ResBody> {
    log.set_target(Mode::Adapt, ctx.cfg.model_name.clone());
    let facts = &ctx.cfg.model_facts;
    let mut model_info = serde_json::json!({
        "general.basename": ctx.cfg.model_name,
        "general.digest": digest(ctx),
        "general.size": facts.size_bytes,
    });
    // Omitted rather than zeroed when unknown: a client that reads a context
    // length of zero has been told something false, while one that finds no
    // key falls back to its own default.
    if facts.context_length > 0 {
        model_info["fono.context_length"] = facts.context_length.into();
    }
    json_ok(&ShowResponse {
        modelfile: String::new(),
        parameters: String::new(),
        template: String::new(),
        details: details(),
        model_info,
        capabilities: vec!["completion", "tools"],
        modified_at: rfc3339_now(),
    })
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
    /// Tool descriptors, same OpenAI-shaped objects Ollama accepts.
    #[serde(default)]
    tools: Vec<serde_json::Value>,
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<OutToolCall>,
}

impl ChatMessage {
    fn text(content: String) -> Self {
        Self { role: "assistant", content, tool_calls: Vec::new() }
    }
}

/// A tool call in Ollama's spelling: `arguments` is a JSON *object*, not
/// the JSON-encoded string OpenAI uses.
#[derive(Serialize)]
struct OutToolCall {
    function: OutToolFunction,
}

#[derive(Serialize)]
struct OutToolFunction {
    name: String,
    arguments: serde_json::Value,
}

fn out_calls(calls: &[fono_assistant::history::ToolCall]) -> Vec<OutToolCall> {
    calls
        .iter()
        .map(|c| OutToolCall {
            function: OutToolFunction {
                name: c.name.clone(),
                // A model may emit arguments that are not valid JSON.
                // An empty object keeps the response well-formed; the
                // client then sees a call it can refuse rather than a
                // body it cannot parse.
                arguments: serde_json::from_str(&c.arguments)
                    .unwrap_or_else(|_| serde_json::json!({})),
            },
        })
        .collect()
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
    let ctx_obj = make_context(&split, max_tokens, &body.tools);
    let model = ctx.cfg.model_name.clone();
    log.set_target(Mode::Adapt, model.clone());

    // See the OpenAI surface: a tool call is read out of the finished
    // reply, so offering tools means generating whole.
    if body.stream && body.tools.is_empty() {
        return stream_chat(assistant, split.user_text, ctx_obj, model, log.defer(true));
    }
    let text = match collect_reply(assistant, split.user_text, ctx_obj).await {
        Ok(t) => t,
        Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    let reply = read_reply(text, !body.tools.is_empty());
    let message = ChatMessage {
        role: "assistant",
        content: reply.text.clone(),
        tool_calls: out_calls(&reply.tool_calls),
    };
    if body.stream {
        return ndjson_response(fixed_body(vec![ndjson_line(&ChatResponse {
            model,
            created_at: rfc3339_now(),
            message,
            done: true,
            done_reason: Some(reply.finish_reason()),
        })]));
    }
    json_ok(&ChatResponse {
        model,
        created_at: rfc3339_now(),
        message,
        done: true,
        done_reason: Some(reply.finish_reason()),
    })
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
            message: ChatMessage::text(text.to_owned()),
            done: false,
            done_reason: None,
        })
    };

    // Terminal line: empty message content, `done: true`.
    let final_line = ndjson_line(&ChatResponse {
        model,
        created_at: rfc3339_now(),
        message: ChatMessage::text(String::new()),
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
            message: ChatMessage::text("hi".into()),
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

    #[test]
    fn ollama_spells_arguments_as_an_object() {
        // Ollama's shape differs from OpenAI's here, and Home Assistant
        // reads the object form.
        let calls = out_calls(&[fono_assistant::history::ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: "{\"path\":\"a.txt\"}".into(),
        }]);
        let s = serde_json::to_string(&calls[0]).unwrap();
        assert!(s.contains(r#""arguments":{"path":"a.txt"}"#), "{s}");
    }

    #[test]
    fn unparseable_arguments_still_produce_a_readable_body() {
        let calls = out_calls(&[fono_assistant::history::ToolCall {
            id: "c".into(),
            name: "read_file".into(),
            arguments: "not json".into(),
        }]);
        let s = serde_json::to_string(&calls[0]).unwrap();
        assert!(s.contains(r#""arguments":{}"#), "{s}");
    }
}
