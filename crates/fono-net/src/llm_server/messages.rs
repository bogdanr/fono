// SPDX-License-Identifier: GPL-3.0-only
//! Shared request parsing + reply driving for both wire formats.
//!
//! Both OpenAI `chat/completions` and Ollama `api/chat` send the same
//! `messages: [{role, content}]` shape. This module folds a system
//! message into [`AssistantContext::system_prompt`], maps the completed
//! turns into [`ChatTurn`] history, extracts the trailing user turn as
//! the `user_text` argument, and drives the one
//! [`Assistant::reply_stream`] — collected into a string (non-stream) or
//! encoded frame-by-frame into a hyper streaming body (stream).

use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use fono_assistant::history::{ChatRole, ChatTurn, ToolCall};
use fono_assistant::traits::{Assistant, AssistantContext};
use futures::StreamExt;
use http_body_util::{BodyExt, Limited, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::Request;
use serde::de::DeserializeOwned;
use serde::Deserialize;

use super::access_log::StreamLog;
use super::{ResBody, MAX_BODY_BYTES};

/// One wire message. `content` is a `Value` so the OpenAI vision shape
/// (an array of typed parts) parses without erroring; [`content_text`]
/// flattens it to plain text (non-text parts are ignored for the MVP).
///
/// `tool_calls` and `tool_call_id` carry the two halves of a tool
/// exchange. A client whose conversation is mostly tool traffic — a
/// coding agent — sends the model's call in one message and the result
/// in the next, and without both the replayed history shows the model
/// asking for things and nothing ever happening.
#[derive(Debug, Deserialize)]
pub struct WireMessage {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub content: serde_json::Value,
    #[serde(default)]
    pub tool_calls: Vec<WireToolCall>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

/// One entry of an assistant message's `tool_calls[]`. Only the
/// `function` variant exists on the wire today, and `arguments` stays a
/// raw string per the spec — a model may emit invalid JSON that still
/// has to be echoed back verbatim on the next turn.
#[derive(Debug, Deserialize)]
pub struct WireToolCall {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub function: WireToolFunction,
}

#[derive(Debug, Default, Deserialize)]
pub struct WireToolFunction {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

/// Tool-call arguments as a string. The spec says a JSON-encoded
/// string, but clients differ and some send the object itself, so
/// accept either and re-encode the object form.
fn arguments_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Flatten message content (a string, or an array of `{type, text}`
/// parts) to plain text.
pub fn content_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// The result of splitting a `messages[]` array into the parts the
/// [`Assistant`] trait expects.
pub struct Split {
    pub system_prompt: String,
    pub history: Vec<ChatTurn>,
    pub user_text: String,
}

/// Split `messages[]` into system prompt + prior history + the trailing
/// turn the model has to answer. Errors when there is nothing to answer.
///
/// The trailing turn is the last message that asks for a reply: usually
/// the user's, but an agent that has just run a tool sends the result as
/// the final message and expects the model to carry on from it. Taking
/// the last *user* message in that case would have thrown away the call
/// and its result and re-answered a question already answered.
pub fn split_messages(msgs: &[WireMessage]) -> Result<Split, String> {
    let last = msgs
        .iter()
        .rposition(|m| matches!(m.role.as_str(), "user" | "tool" | "function"))
        .ok_or("no user or tool message in `messages`")?;
    let system_prompt = msgs
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| content_text(&m.content))
        .collect::<Vec<_>>()
        .join("\n\n");
    // A trailing tool result is labelled the same way it is labelled when
    // replayed from history, so the prompt the model reads mid-exchange
    // matches the one rebuilt on the next turn — which is what lets the
    // cached prefix keep matching.
    let trailing = content_text(&msgs[last].content);
    let user_text = if msgs[last].role == "user" {
        trailing
    } else {
        fono_assistant::local_tools::render_result(&trailing)
    };
    let now = Instant::now();
    let mut history = Vec::new();
    for m in msgs.iter().take(last) {
        let role = match m.role.as_str() {
            "user" => ChatRole::User,
            "assistant" => ChatRole::Assistant,
            "tool" | "function" => ChatRole::Tool,
            // System is folded above.
            _ => continue,
        };
        let tool_calls = m
            .tool_calls
            .iter()
            .filter(|c| !c.function.name.is_empty())
            .map(|c| ToolCall {
                id: c.id.clone(),
                name: c.function.name.clone(),
                arguments: arguments_text(&c.function.arguments),
            })
            .collect::<Vec<_>>();
        let content = content_text(&m.content);
        // An assistant message that only called a tool has empty
        // content, and a tool result carries no call — but a message
        // with neither is nothing to replay.
        if content.trim().is_empty() && tool_calls.is_empty() {
            continue;
        }
        history.push(ChatTurn {
            role,
            content,
            at: now,
            tool_calls,
            tool_call_id: m.tool_call_id.clone(),
        });
    }
    Ok(Split { system_prompt, history, user_text })
}

/// Build the per-turn [`AssistantContext`] from a [`Split`].
///
/// `tools` are the descriptors the client offered. They are described in
/// the prompt rather than handed to the backend's own tool machinery,
/// because that machinery *runs* the tool — here the client runs it and
/// wants the call back. Rendering them through
/// [`fono_assistant::local_tools::instructions`] and
/// [`fono_assistant::traits::compose_head`] means a client's tools are
/// described in exactly the words, and exactly the order, the embedded
/// backend uses locally; a second phrasing would ask the same model for a
/// syntax it was never shown, and would sit in front of the whole prompt
/// where any drift costs the cached prefix.
///
/// Note the [`AssistantContext::default`] tail: `allow_brain_capture`
/// stays `false`, so a network client hitting the shared LLM server never
/// drives the local Glas Cortex overlay or pays the brain-capture cost —
/// only local hotkey turns (which set it explicitly) do.
pub fn make_context(
    split: &Split,
    max_new_tokens: Option<u32>,
    tools: &[serde_json::Value],
) -> AssistantContext {
    let system_prompt = if tools.is_empty() {
        split.system_prompt.clone()
    } else {
        fono_assistant::traits::compose_head(
            &split.system_prompt,
            Some(&fono_assistant::local_tools::instructions(tools)),
            None,
        )
    };
    AssistantContext {
        system_prompt,
        history: split.history.clone(),
        max_new_tokens,
        ..AssistantContext::default()
    }
}

/// A finished reply, read as either prose or a call the client must run.
pub struct Reply {
    /// The text to hand back. Empty when the model called a tool.
    pub text: String,
    /// The call the model made, if it made one.
    pub tool_calls: Vec<ToolCall>,
}

impl Reply {
    /// `"tool_calls"` when the model asked for a tool, else `"stop"` —
    /// the `finish_reason` a client keys its loop off.
    #[must_use]
    pub fn finish_reason(&self) -> &'static str {
        if self.tool_calls.is_empty() {
            "stop"
        } else {
            "tool_calls"
        }
    }
}

/// Read a finished reply as prose or as a tool call.
///
/// Only attempted when the client offered tools: the parser is tolerant by
/// design (a model that wandered into a code fence still gets understood),
/// and running it on a plain chat turn would let an answer that merely
/// *discusses* JSON be handed back as a call nobody asked for.
#[must_use]
pub fn read_reply(text: String, tools_offered: bool) -> Reply {
    if !tools_offered {
        return Reply { text, tool_calls: Vec::new() };
    }
    match fono_assistant::local_tools::parse_call(&text) {
        Some((name, arguments)) => Reply {
            text: String::new(),
            tool_calls: vec![ToolCall { id: gen_id("call_"), name, arguments }],
        },
        None => Reply { text, tool_calls: Vec::new() },
    }
}

/// Read a request body into bytes (size-capped). Returns a
/// human-readable error string for a 400 response.
pub async fn read_body_bytes(req: Request<Incoming>) -> Result<Bytes, String> {
    let limited = Limited::new(req.into_body(), MAX_BODY_BYTES);
    let collected = limited.collect().await.map_err(|e| format!("reading request body: {e}"))?;
    Ok(collected.to_bytes())
}

/// Read + JSON-parse a request body (size-capped). Returns a
/// human-readable error string for a 400 response.
pub async fn read_json<T: DeserializeOwned>(req: Request<Incoming>) -> Result<T, String> {
    let bytes = read_body_bytes(req).await?;
    serde_json::from_slice(&bytes).map_err(|e| format!("invalid JSON body: {e}"))
}

/// Drive a full reply to completion, concatenating every text delta.
/// Used by the non-streaming code paths.
pub async fn collect_reply(
    assistant: Arc<dyn Assistant>,
    user_text: String,
    ctx: AssistantContext,
) -> Result<String, String> {
    let mut stream =
        assistant.reply_stream(&user_text, &ctx).await.map_err(|e| format!("{e:#}"))?;
    let mut out = String::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(delta) => out.push_str(&delta.text),
            Err(e) => return Err(format!("{e:#}")),
        }
    }
    Ok(out)
}

/// Build a hyper streaming body that drives `reply_stream` and encodes
/// each text delta with `encode_delta`. `open` (if any) is sent first;
/// `tail` frames are always sent last (final chunk, `[DONE]`, etc.),
/// even if generation errors — so a client always sees a clean end.
///
/// When `slog` is `Some`, it records time-to-first-token and an output
/// token count and emits the access line once the body drains.
pub fn stream_body<D>(
    assistant: Arc<dyn Assistant>,
    user_text: String,
    ctx: AssistantContext,
    open: Option<Bytes>,
    encode_delta: D,
    tail: Vec<Bytes>,
    slog: Option<StreamLog>,
) -> ResBody
where
    D: Fn(&str) -> Bytes + Send + 'static,
{
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Bytes>(32);
    tokio::spawn(async move {
        let mut slog = slog;
        'run: {
            if let Some(b) = open {
                if tx.send(b).await.is_err() {
                    break 'run;
                }
            }
            match assistant.reply_stream(&user_text, &ctx).await {
                Ok(mut stream) => {
                    while let Some(item) = stream.next().await {
                        match item {
                            Ok(delta) if !delta.text.is_empty() => {
                                if let Some(s) = slog.as_mut() {
                                    s.on_token();
                                }
                                if tx.send(encode_delta(&delta.text)).await.is_err() {
                                    break 'run;
                                }
                            }
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!(target: "fono::llm::server", "generation error: {e:#}");
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(target: "fono::llm::server", "reply_stream failed: {e:#}");
                }
            }
            for b in tail {
                if tx.send(b).await.is_err() {
                    break 'run;
                }
            }
        }
        if let Some(s) = slog {
            s.emit();
        }
    });

    let stream = futures::stream::poll_fn(move |cx| {
        rx.poll_recv(cx)
            .map(|opt| opt.map(|b| Ok::<Frame<Bytes>, std::convert::Infallible>(Frame::data(b))))
    });
    BodyExt::boxed(StreamBody::new(stream))
}

/// A body of frames that are already known, for a reply generated whole
/// but still framed as a stream.
#[must_use]
pub fn fixed_body(frames: Vec<Bytes>) -> ResBody {
    let stream = futures::stream::iter(
        frames.into_iter().map(|b| Ok::<Frame<Bytes>, std::convert::Infallible>(Frame::data(b))),
    );
    BodyExt::boxed(StreamBody::new(stream))
}

/// Unix time in whole seconds (0 on a pre-epoch clock).
pub fn unix_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A monotonic-ish unique id for chat completions (`<prefix><nanos>`).
pub fn gen_id(prefix: &str) -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{prefix}{nanos}")
}

/// RFC 3339 UTC timestamp for Ollama's `created_at`, dependency-free
/// (chrono/time are not in the graph). Uses Howard Hinnant's
/// `civil_from_days` algorithm.
pub fn rfc3339_now() -> String {
    let secs = i64::try_from(unix_secs()).unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hh, mm, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> WireMessage {
        WireMessage {
            role: role.into(),
            content: serde_json::json!(content),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// Parse a real OpenAI-shaped body, so the tests below exercise the
    /// wire deserialization and not a hand-built struct.
    fn parse(body: serde_json::Value) -> Vec<WireMessage> {
        serde_json::from_value(body).expect("messages parse")
    }

    #[test]
    fn tool_exchange_survives_the_wire() {
        // The defect: `tool` messages were dropped and assistant
        // `tool_calls` were never parsed, so an agent's history replayed
        // as the model asking for things and nothing ever happening.
        let msgs = parse(serde_json::json!([
            {"role": "user", "content": "what is in the file?"},
            {"role": "assistant", "content": null, "tool_calls": [
                {"id": "call_1", "type": "function",
                 "function": {"name": "read_file", "arguments": "{\"path\":\"a.txt\"}"}}
            ]},
            {"role": "tool", "tool_call_id": "call_1", "content": "hello world"},
            {"role": "user", "content": "and now?"},
        ]));
        let s = split_messages(&msgs).expect("split");
        assert_eq!(s.history.len(), 3);
        assert_eq!(s.history[1].role, ChatRole::Assistant);
        assert_eq!(s.history[1].tool_calls.len(), 1);
        assert_eq!(s.history[1].tool_calls[0].name, "read_file");
        assert_eq!(s.history[1].tool_calls[0].arguments, "{\"path\":\"a.txt\"}");
        assert_eq!(s.history[2].role, ChatRole::Tool);
        assert_eq!(s.history[2].content, "hello world");
        assert_eq!(s.history[2].tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn a_trailing_tool_result_is_what_gets_answered() {
        // An agent that has just run a tool sends the result last and
        // expects the model to continue. Answering the previous user
        // message instead would drop the call and repeat an answer.
        let msgs = parse(serde_json::json!([
            {"role": "user", "content": "what is in the file?"},
            {"role": "assistant", "content": null, "tool_calls": [
                {"id": "c1", "type": "function",
                 "function": {"name": "read_file", "arguments": {"path": "a.txt"}}}
            ]},
            {"role": "tool", "tool_call_id": "c1", "content": "hello world"},
        ]));
        let s = split_messages(&msgs).expect("split");
        assert_eq!(s.user_text, "Tool result: hello world");
        assert_eq!(s.history.len(), 2);
        assert_eq!(s.history[1].tool_calls.len(), 1);
        // Arguments sent as an object rather than the spec's JSON string
        // still round-trip.
        assert_eq!(s.history[1].tool_calls[0].arguments, "{\"path\":\"a.txt\"}");
    }

    #[test]
    fn split_folds_system_and_takes_last_user() {
        let msgs = vec![
            msg("system", "be terse"),
            msg("user", "hi"),
            msg("assistant", "hello"),
            msg("user", "what's 2+2?"),
        ];
        let s = split_messages(&msgs).expect("split");
        assert_eq!(s.system_prompt, "be terse");
        assert_eq!(s.user_text, "what's 2+2?");
        // history = the first user + assistant turns (system folded, last
        // user excluded).
        assert_eq!(s.history.len(), 2);
        assert_eq!(s.history[0].role, ChatRole::User);
        assert_eq!(s.history[0].content, "hi");
        assert_eq!(s.history[1].role, ChatRole::Assistant);
    }

    #[test]
    fn split_errors_without_user() {
        let msgs = vec![msg("system", "hello")];
        assert!(split_messages(&msgs).is_err());
    }

    #[test]
    fn content_text_flattens_array_parts() {
        let v = serde_json::json!([
            { "type": "text", "text": "a" },
            { "type": "image_url", "image_url": { "url": "x" } },
            { "type": "text", "text": "b" }
        ]);
        assert_eq!(content_text(&v), "ab");
    }

    #[test]
    fn rfc3339_epoch_is_well_formed() {
        let s = rfc3339_now();
        assert_eq!(s.len(), 20);
        assert!(s.ends_with('Z'));
        assert_eq!(&s[4..5], "-");
    }

    #[test]
    fn network_context_never_allows_brain_capture() {
        // A turn arriving over the shared LLM server must not drive the
        // local Glas Cortex overlay (or pay its capture cost) — the tap is
        // reserved for local hotkey turns.
        let split =
            Split { system_prompt: "be terse".into(), history: Vec::new(), user_text: "hi".into() };
        assert!(!make_context(&split, None, &[]).allow_brain_capture);
        assert!(!make_context(&split, Some(64), &[]).allow_brain_capture);
    }

    fn split_of(system: &str) -> Split {
        Split { system_prompt: system.into(), history: Vec::new(), user_text: "hi".into() }
    }

    fn one_tool() -> Vec<serde_json::Value> {
        vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a file",
                "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}
            }
        })]
    }

    #[test]
    fn offering_no_tools_leaves_the_prompt_untouched() {
        // Byte-for-byte, or every client that sends no tools pays a
        // fresh prefill for a prompt that only differs by whitespace.
        let split = split_of("be terse");
        assert_eq!(make_context(&split, None, &[]).system_prompt, "be terse");
    }

    #[test]
    fn offered_tools_are_described_after_the_client_prompt() {
        // Order matters beyond taste: the client's own prompt is the
        // steady head a cached prefix is pinned to, so anything added
        // has to go behind it.
        let ctx = make_context(&split_of("be terse"), None, &one_tool());
        assert!(ctx.system_prompt.starts_with("be terse"));
        assert!(ctx.system_prompt.contains("read_file(path)"));
        assert!(ctx.system_prompt.contains("<tool_call>"));
    }

    #[test]
    fn a_call_is_read_back_out_of_the_reply() {
        let reply = read_reply(
            "Reading it now.<tool_call>{\"name\": \"read_file\", \"arguments\": {\"path\": \"a.txt\"}}</tool_call>"
                .into(),
            true,
        );
        assert_eq!(reply.tool_calls.len(), 1);
        assert_eq!(reply.tool_calls[0].name, "read_file");
        assert_eq!(reply.tool_calls[0].arguments, "{\"path\":\"a.txt\"}");
        assert!(reply.text.is_empty(), "a call is not also spoken");
        assert_eq!(reply.finish_reason(), "tool_calls");
        assert!(!reply.tool_calls[0].id.is_empty(), "clients key their result on the id");
    }

    #[test]
    fn prose_stays_prose_even_when_it_talks_about_json() {
        let reply = read_reply("{\"name\": \"read_file\"} is what a call looks like.".into(), true);
        assert!(reply.tool_calls.is_empty());
        assert_eq!(reply.finish_reason(), "stop");
    }

    #[test]
    fn a_reply_is_never_read_as_a_call_when_no_tools_were_offered() {
        // The parser is deliberately tolerant, so a client that offered
        // nothing must never have an answer swallowed as machinery.
        let text = "<tool_call>{\"name\": \"read_file\", \"arguments\": {}}</tool_call>";
        let reply = read_reply(text.into(), false);
        assert!(reply.tool_calls.is_empty());
        assert_eq!(reply.text, text);
    }
}
