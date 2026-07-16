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
use fono_assistant::history::{ChatRole, ChatTurn};
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
#[derive(Debug, Deserialize)]
pub struct WireMessage {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub content: serde_json::Value,
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

/// Split `messages[]` into system prompt + prior history + trailing user
/// turn. Errors when there is no user message to answer.
pub fn split_messages(msgs: &[WireMessage]) -> Result<Split, String> {
    let last_user =
        msgs.iter().rposition(|m| m.role == "user").ok_or("no user message in `messages`")?;
    let system_prompt = msgs
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| content_text(&m.content))
        .collect::<Vec<_>>()
        .join("\n\n");
    let user_text = content_text(&msgs[last_user].content);
    let now = Instant::now();
    let mut history = Vec::new();
    for m in msgs.iter().take(last_user) {
        let role = match m.role.as_str() {
            "user" => ChatRole::User,
            "assistant" => ChatRole::Assistant,
            // System is folded above; tool turns are out of MVP scope.
            _ => continue,
        };
        history.push(ChatTurn {
            role,
            content: content_text(&m.content),
            at: now,
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
    Ok(Split { system_prompt, history, user_text })
}

/// Build the per-turn [`AssistantContext`] from a [`Split`].
///
/// Note the [`AssistantContext::default`] tail: `allow_brain_capture`
/// stays `false`, so a network client hitting the shared LLM server never
/// drives the local Glas Cortex overlay or pays the brain-capture cost —
/// only local hotkey turns (which set it explicitly) do.
pub fn make_context(split: &Split, max_new_tokens: Option<u32>) -> AssistantContext {
    AssistantContext {
        system_prompt: split.system_prompt.clone(),
        history: split.history.clone(),
        max_new_tokens,
        ..AssistantContext::default()
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
        WireMessage { role: role.into(), content: serde_json::json!(content) }
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
        assert!(!make_context(&split, None).allow_brain_capture);
        assert!(!make_context(&split, Some(64)).allow_brain_capture);
    }
}
