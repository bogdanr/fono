// SPDX-License-Identifier: GPL-3.0-only
//! Shared "summarize and speak" helper used by the `fono.summarize`
//! MCP tool and the `fono summarize` CLI subcommand.
//!
//! External applications (chat clients, monitoring tools, scripts) send a
//! structured notification payload — sender, chat, raw message text,
//! attachment metadata. The helper renders the payload into a one-shot
//! request against the configured `[assistant]` backend with a strict
//! summarization prompt and returns the 1-2 sentence spoken summary.
//! The raw content is never returned for speech verbatim.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use serde::Deserialize;

use fono_assistant::AssistantContext;
use fono_core::config::{default_summarize_prompt, AssistantBackend, Config};
use fono_core::Secrets;

/// Maximum number of `message_text` characters fed to the LLM. Inputs
/// beyond the cap are middle-elided (head + tail preserved) so both the
/// opening intent and the trailing error lines of a long log survive.
/// Sized for the embedded local backend's default 8192-token context
/// with room for the system prompt and the rest of the payload.
pub const MESSAGE_TEXT_CAP: usize = 16_000;

/// Of [`MESSAGE_TEXT_CAP`], how many characters are kept from the head;
/// the remainder is kept from the tail.
const HEAD_KEEP: usize = 12_000;

/// Ceiling on opening the assistant stream (connection + first byte)
/// against a cloud backend. A healthy cloud answers in single-digit
/// seconds for a 1-2 sentence summary; past this it is shedding load and
/// failing fast leaves room for the retry/fallback inside the caller's
/// budget (chat-cli kills the whole process at 180 s).
const CLOUD_OPEN_TIMEOUT: Duration = Duration::from_secs(10);
/// Ceiling on draining the full reply stream from a cloud backend.
const CLOUD_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
/// Ceiling on opening the assistant stream against the local backend
/// (embedded GGUF or self-hosted server). First byte arrives only after
/// model load + CPU prompt evaluation of up to ~4k tokens, which on
/// modest hardware genuinely takes tens of seconds.
const LOCAL_OPEN_TIMEOUT: Duration = Duration::from_secs(60);
/// Ceiling on draining the full reply stream from the local backend.
const LOCAL_DRAIN_TIMEOUT: Duration = Duration::from_secs(120);

/// Per-request generation cap for the summary. One or two spoken
/// sentences never need the local backend's full 384-token budget; a
/// tight cap bounds a degenerate run (e.g. a small model looping on a
/// refusal) to a few seconds instead of the full budget. Cloud
/// backends ignore it.
const SUMMARY_MAX_NEW_TOKENS: u32 = 96;

/// `(open, drain)` timeout pair for the given backend shape.
/// `Ollama` covers both the embedded local model and a manually
/// configured local server; everything else is a cloud provider.
fn llm_timeouts(backend: &AssistantBackend) -> (Duration, Duration) {
    match backend {
        AssistantBackend::Ollama => (LOCAL_OPEN_TIMEOUT, LOCAL_DRAIN_TIMEOUT),
        _ => (CLOUD_OPEN_TIMEOUT, CLOUD_DRAIN_TIMEOUT),
    }
}

/// Fallback preference order: fast cheap clouds first, the local backend
/// last (always "works" offline but is slow and weakest at instruction
/// following). The configured backend is excluded by
/// [`fallback_candidates`].
const FALLBACK_ORDER: [AssistantBackend; 6] = [
    AssistantBackend::Cerebras,
    AssistantBackend::Groq,
    AssistantBackend::OpenAI,
    AssistantBackend::OpenRouter,
    AssistantBackend::Anthropic,
    AssistantBackend::Ollama,
];

/// Fallback candidates in preference order, excluding the configured
/// primary backend.
fn fallback_candidates(primary: &AssistantBackend) -> Vec<AssistantBackend> {
    FALLBACK_ORDER.iter().filter(|b| *b != primary).cloned().collect()
}

/// Attachment metadata. v1 is metadata-only: paths/bytes are neither
/// read nor sent to the model — attachments are described, not analyzed.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AttachmentMeta {
    /// Coarse kind, e.g. `"image"`, `"file"`, `"audio"`, `"video"`.
    pub kind: String,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: Option<u64>,
}

/// Structured notification payload shared by the MCP tool schema and the
/// CLI `--json` mode. Every field except `message_text` is optional.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SummarizePayload {
    /// Originating application, e.g. `"chat-cli"`.
    pub source_app: String,
    /// Event kind, e.g. `"incoming_message"`, `"alert"`.
    pub source_kind: String,
    /// Account / workspace label, e.g. `"Slack / Engineering"`.
    pub account: String,
    pub chat_name: String,
    /// E.g. `"channel"`, `"direct_message"`, `"group"`.
    pub chat_kind: String,
    pub sender_name: String,
    /// The raw content. May be long (logs, pasted dumps); it is capped
    /// via [`MESSAGE_TEXT_CAP`] before reaching the model and is never
    /// spoken verbatim.
    pub message_text: String,
    pub attachments: Vec<AttachmentMeta>,
    /// Optional caller instructions appended to the system prompt.
    pub instructions: String,
}

/// Render the structured payload into the single user-turn string sent
/// to the assistant. Empty fields are omitted; `message_text` is
/// middle-elided beyond [`MESSAGE_TEXT_CAP`].
pub fn render_user_turn(payload: &SummarizePayload) -> String {
    let mut out = String::with_capacity(payload.message_text.len().min(MESSAGE_TEXT_CAP) + 512);
    out.push_str("Incoming notification to summarize for speech.\n");
    let mut field = |label: &str, value: &str| {
        if !value.trim().is_empty() {
            out.push_str(label);
            out.push_str(": ");
            out.push_str(value.trim());
            out.push('\n');
        }
    };
    field("Source application", &payload.source_app);
    field("Event kind", &payload.source_kind);
    field("Account", &payload.account);
    field("Chat", &payload.chat_name);
    field("Chat kind", &payload.chat_kind);
    field("Sender", &payload.sender_name);
    if !payload.attachments.is_empty() {
        let described: Vec<String> = payload.attachments.iter().map(describe_attachment).collect();
        out.push_str("Attachments: ");
        out.push_str(&described.join("; "));
        out.push('\n');
    }
    out.push_str("Message content:\n<<<\n");
    out.push_str(&truncate_middle(&payload.message_text, MESSAGE_TEXT_CAP));
    out.push_str("\n>>>");
    out
}

/// One-line human description of an attachment for the model.
fn describe_attachment(a: &AttachmentMeta) -> String {
    let mut s = String::new();
    if a.kind.trim().is_empty() {
        s.push_str("attachment")
    } else {
        s.push_str(a.kind.trim())
    };
    if !a.filename.trim().is_empty() {
        s.push_str(&format!(" \"{}\"", a.filename.trim()));
    }
    let mut extras: Vec<String> = Vec::new();
    if !a.mime_type.trim().is_empty() {
        extras.push(a.mime_type.trim().to_string());
    }
    if let Some(bytes) = a.size_bytes {
        extras.push(format!("{bytes} bytes"));
    }
    if !extras.is_empty() {
        s.push_str(&format!(" ({})", extras.join(", ")));
    }
    s
}

/// Middle-elide `text` to at most `cap` kept characters (plus a short
/// elision marker). Keeps the head ([`HEAD_KEEP`] of `cap`) and the tail
/// (the remainder), cutting on `char` boundaries.
pub fn truncate_middle(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let head_target = HEAD_KEEP.min(cap.saturating_sub(1));
    let tail_target = cap - head_target;
    let mut head_end = head_target.min(text.len());
    while head_end > 0 && !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = text.len().saturating_sub(tail_target);
    while tail_start < text.len() && !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let elided = tail_start.saturating_sub(head_end);
    format!("{}\n[... {elided} characters elided ...]\n{}", &text[..head_end], &text[tail_start..])
}

/// Effective system prompt: `[mcp].summarize_prompt` override when set,
/// otherwise the built-in default; caller `instructions` appended.
pub fn system_prompt(cfg: &Config, instructions: &str) -> String {
    let base = if cfg.mcp.summarize_prompt.trim().is_empty() {
        default_summarize_prompt()
    } else {
        cfg.mcp.summarize_prompt.trim()
    };
    if instructions.trim().is_empty() {
        base.to_string()
    } else {
        format!("{base}\nCaller instructions: {}", instructions.trim())
    }
}

/// Summarize `payload` into 1-2 spoken sentences using the configured
/// `[assistant]` backend. One-shot: empty history, no tools, no vision.
///
/// Resilience policy (three attempts maximum):
///
/// ```text
/// attempt 1: configured backend
/// attempt 2: same backend, one immediate retry
/// attempt 3: first available fallback backend (single attempt)
/// ```
///
/// Configuration problems (assistant disabled, primary backend fails to
/// build) still fail fast — falling back would mask a setup error.
///
/// `assistant_models_dir` is only consulted by the embedded local
/// backend; pass the polish models dir (the daemon does the same — the
/// local assistant shares the polish GGUF weights).
pub async fn summarize(
    cfg: &Config,
    secrets: &Secrets,
    assistant_models_dir: &Path,
    payload: &SummarizePayload,
) -> Result<String> {
    // Fail fast on an empty payload before paying for an assistant build.
    if payload.message_text.trim().is_empty() {
        return Err(anyhow!("missing or empty `message_text`"));
    }
    let assistant = build_primary_assistant(cfg, secrets, assistant_models_dir)?;
    summarize_with_assistant(assistant.as_ref(), cfg, secrets, assistant_models_dir, payload).await
}

/// Build the configured primary assistant backend, with summarize's
/// error shaping (disabled backend → actionable guidance). Exposed so
/// long-lived callers (the MCP server tool) can build ONCE and reuse the
/// instance across calls — for the embedded local backend that keeps the
/// model loaded and the prompt-state cache warm, so repeat summaries
/// only prefill the per-request payload instead of the whole system
/// prompt.
pub fn build_primary_assistant(
    cfg: &Config,
    secrets: &Secrets,
    assistant_models_dir: &Path,
) -> Result<std::sync::Arc<dyn fono_assistant::Assistant>> {
    fono_assistant::build_assistant(&cfg.assistant, secrets, assistant_models_dir)
        .context("assistant build failed")?
        .ok_or_else(|| {
            anyhow!(
                "The assistant backend is disabled. Run `fono setup` (or set \
                 `[assistant] enabled = true` with a backend in config.toml) before \
                 using summarize."
            )
        })
}

/// Core of [`summarize`] with a caller-supplied (possibly cached)
/// primary assistant: retry once on the same backend, then try the
/// fallback chain. Fallback backends are still built per call — they
/// are the rare path and a successful build doubles as a usability
/// probe.
pub async fn summarize_with_assistant(
    assistant: &dyn fono_assistant::Assistant,
    cfg: &Config,
    secrets: &Secrets,
    assistant_models_dir: &Path,
    payload: &SummarizePayload,
) -> Result<String> {
    if payload.message_text.trim().is_empty() {
        return Err(anyhow!("missing or empty `message_text`"));
    }

    let primary_err = match summarize_with_retry(assistant, cfg, payload).await {
        Ok(summary) => return Ok(summary),
        Err(err) => err,
    };

    // One fallback attempt on the first candidate that actually builds
    // (a successful build implies a usable key / model file). `cloud =
    // None` so the provider-specific `[assistant.cloud]` override block
    // never leaks into a different provider's build — key resolution
    // falls through to the canonical env var and the catalogue's
    // default model.
    for candidate in fallback_candidates(&cfg.assistant.backend) {
        let mut fb_cfg = cfg.clone();
        fb_cfg.assistant.backend = candidate.clone();
        fb_cfg.assistant.cloud = None;
        let Ok(Some(fb)) =
            fono_assistant::build_assistant(&fb_cfg.assistant, secrets, assistant_models_dir)
        else {
            continue;
        };
        tracing::warn!(
            target: "fono_mcp_server::summarize",
            fallback = ?candidate,
            error = format!("{primary_err:#}"),
            "configured backend failed twice; trying fallback backend"
        );
        return match summarize_with(fb.as_ref(), &fb_cfg, payload).await {
            Ok(summary) => {
                tracing::info!(
                    target: "fono_mcp_server::summarize",
                    fallback = ?candidate,
                    "fallback backend produced the summary"
                );
                Ok(summary)
            }
            Err(fb_err) => Err(fb_err.context(format!(
                "fallback backend {candidate:?} also failed (configured backend failed \
                 first: {primary_err:#})"
            ))),
        };
    }

    Err(primary_err.context(
        "no fallback backend available (no other backend has a usable API key or local model)",
    ))
}

/// [`summarize_with`] plus exactly one retry on the same backend. The
/// first failure is logged at warn level; the second is returned with
/// retry context.
pub async fn summarize_with_retry(
    assistant: &dyn fono_assistant::Assistant,
    cfg: &Config,
    payload: &SummarizePayload,
) -> Result<String> {
    let first = match summarize_with(assistant, cfg, payload).await {
        Ok(summary) => return Ok(summary),
        Err(err) => err,
    };
    tracing::warn!(
        target: "fono_mcp_server::summarize",
        backend = assistant.name(),
        error = format!("{first:#}"),
        "summarize attempt failed; retrying once on the same backend"
    );
    summarize_with(assistant, cfg, payload)
        .await
        .with_context(|| format!("retry on the same backend also failed (first: {first:#})"))
}

/// Backend-injectable core of [`summarize`]; used directly by tests with
/// a mock [`fono_assistant::Assistant`]. Timeouts are derived from
/// `cfg.assistant.backend` (cloud short, local long).
pub async fn summarize_with(
    assistant: &dyn fono_assistant::Assistant,
    cfg: &Config,
    payload: &SummarizePayload,
) -> Result<String> {
    summarize_with_timeouts(assistant, cfg, payload, llm_timeouts(&cfg.assistant.backend)).await
}

/// Core with an explicit `(open, drain)` timeout pair — the fallback
/// path must use the *fallback* backend's pair, not the primary's.
async fn summarize_with_timeouts(
    assistant: &dyn fono_assistant::Assistant,
    cfg: &Config,
    payload: &SummarizePayload,
    (open_timeout, drain_timeout): (Duration, Duration),
) -> Result<String> {
    let ctx = AssistantContext {
        system_prompt: system_prompt(cfg, &payload.instructions),
        language: None,
        history: Vec::new(),
        active_window_context: None,
        screen_capture: None,
        prefer_vision: false,
        max_new_tokens: Some(SUMMARY_MAX_NEW_TOKENS),
    };
    let user_turn = render_user_turn(payload);

    let stream = tokio::time::timeout(open_timeout, assistant.reply_stream(&user_turn, &ctx))
        .await
        .map_err(|_| anyhow!("assistant request timed out"))?
        .context("assistant request failed")?;

    let mut full = String::new();
    let mut deltas = stream;
    tokio::time::timeout(drain_timeout, async {
        while let Some(item) = deltas.next().await {
            let delta = item.context("assistant stream error")?;
            if delta.tool_event.is_none() {
                full.push_str(&delta.text);
            }
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .map_err(|_| anyhow!("assistant reply timed out"))??;

    let summary = collapse_repeated_sentences(full.trim());
    if summary.is_empty() {
        return Err(anyhow!("assistant returned an empty summary"));
    }
    if looks_like_refusal(&summary) {
        // Small aligned models occasionally refuse hostile content despite
        // the prompt's explicit no-refusal directive. A refusal is useless
        // spoken aloud; fall back to a deterministic metadata-only summary
        // so the user still learns who pinged them and where.
        tracing::warn!(
            target: "fono_mcp_server::summarize",
            backend = assistant.name(),
            "assistant refused to summarize; using metadata fallback"
        );
        return Ok(metadata_fallback(payload));
    }
    Ok(summary)
}

/// Collapse consecutive duplicate sentences. Greedy decoding on small
/// local models can repeat the same sentence verbatim until the token
/// budget; speaking it once is always correct, so the dedupe is safe
/// for healthy replies too (a real summary never repeats a sentence
/// back-to-back).
fn collapse_repeated_sentences(text: &str) -> String {
    let mut out = String::new();
    let mut prev_norm: Option<String> = None;
    for sentence in split_sentences(text) {
        let norm = sentence.trim().trim_end_matches(['.', '!', '?', '…']).trim().to_lowercase();
        if !norm.is_empty() && prev_norm.as_deref() == Some(norm.as_str()) {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(sentence.trim());
        if !norm.is_empty() {
            prev_norm = Some(norm);
        }
    }
    out
}

/// Split on sentence-final punctuation (`.` `!` `?` `…`), keeping the
/// punctuation with its sentence. A trailing fragment without a closer
/// (token-budget truncation) is returned as the last item.
fn split_sentences(text: &str) -> Vec<&str> {
    let mut sentences = Vec::new();
    let mut start = 0;
    let mut iter = text.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if matches!(c, '.' | '!' | '?' | '…') {
            // Absorb any run of closers ("?!", "...").
            let mut end = i + c.len_utf8();
            while let Some(&(j, c2)) = iter.peek() {
                if matches!(c2, '.' | '!' | '?' | '…') {
                    end = j + c2.len_utf8();
                    iter.next();
                } else {
                    break;
                }
            }
            let sentence = text[start..end].trim();
            if !sentence.is_empty() {
                sentences.push(sentence);
            }
            start = end;
        }
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        sentences.push(tail);
    }
    sentences
}

/// Whether the (already collapsed) reply is a bare refusal rather than
/// a summary. Heuristic of last resort: prompt hardening and decoding
/// guards run first; this only catches the reply when it *opens* with a
/// canonical English refusal formula, so a genuine summary that merely
/// mentions inability mid-sentence is never misclassified.
fn looks_like_refusal(text: &str) -> bool {
    const REFUSAL_OPENERS: [&str; 8] = [
        "i cannot",
        "i can't",
        "i can not",
        "i am unable",
        "i'm unable",
        "i am not able",
        "i'm not able",
        "as an ai",
    ];
    let lowered = text.trim().to_lowercase();
    REFUSAL_OPENERS.iter().any(|opener| lowered.starts_with(opener))
}

/// Deterministic summary built from payload metadata alone — spoken when
/// the model refuses. Intentionally content-free: the message text
/// already failed summarization, so only who/where survives.
fn metadata_fallback(payload: &SummarizePayload) -> String {
    let sender = payload.sender_name.trim();
    let sender = if sender.is_empty() { "Someone" } else { sender };
    let chat = payload.chat_name.trim();
    let app = payload.source_app.trim();
    if !chat.is_empty() {
        format!("{sender} sent a message in {chat}.")
    } else if !app.is_empty() {
        format!("{sender} sent a message via {app}.")
    } else {
        format!("{sender} sent a message.")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use fono_assistant::traits::ToolEvent;
    use fono_assistant::{Assistant, TokenDelta};
    use futures::stream::BoxStream;
    use futures::StreamExt;

    use super::*;
    use fono_core::config::Config;

    fn payload(text: &str) -> SummarizePayload {
        SummarizePayload {
            source_app: "chat-cli".into(),
            source_kind: "incoming_message".into(),
            account: "Slack / Engineering".into(),
            chat_name: "Backend Alerts".into(),
            chat_kind: "channel".into(),
            sender_name: "Mihai".into(),
            message_text: text.into(),
            attachments: vec![AttachmentMeta {
                kind: "image".into(),
                filename: "screenshot.png".into(),
                mime_type: "image/png".into(),
                size_bytes: Some(2048),
            }],
            instructions: String::new(),
        }
    }

    // ── render_user_turn ──────────────────────────────────────────────

    #[test]
    fn render_includes_structured_fields_and_attachments() {
        let turn = render_user_turn(&payload("deploy failed"));
        assert!(turn.contains("Source application: chat-cli"));
        assert!(turn.contains("Account: Slack / Engineering"));
        assert!(turn.contains("Chat: Backend Alerts"));
        assert!(turn.contains("Chat kind: channel"));
        assert!(turn.contains("Sender: Mihai"));
        assert!(turn.contains("image \"screenshot.png\" (image/png, 2048 bytes)"));
        assert!(turn.contains("deploy failed"));
    }

    #[test]
    fn render_omits_empty_fields() {
        let p = SummarizePayload { message_text: "hello".into(), ..Default::default() };
        let turn = render_user_turn(&p);
        assert!(!turn.contains("Sender:"));
        assert!(!turn.contains("Chat:"));
        assert!(!turn.contains("Attachments:"));
        assert!(turn.contains("hello"));
    }

    // ── truncate_middle ───────────────────────────────────────────────

    #[test]
    fn truncate_leaves_short_text_unchanged() {
        assert_eq!(truncate_middle("short", MESSAGE_TEXT_CAP), "short");
    }

    #[test]
    fn truncate_keeps_head_and_tail_with_marker() {
        let text = format!("HEAD-START {} TAIL-END", "x".repeat(3 * MESSAGE_TEXT_CAP));
        let out = truncate_middle(&text, MESSAGE_TEXT_CAP);
        assert!(out.starts_with("HEAD-START"));
        assert!(out.ends_with("TAIL-END"));
        assert!(out.contains("characters elided"));
        // Kept content is bounded by the cap plus the marker line.
        assert!(out.len() < MESSAGE_TEXT_CAP + 100);
    }

    #[test]
    fn truncate_respects_multibyte_char_boundaries() {
        let text = "ăîșț".repeat(2 * MESSAGE_TEXT_CAP);
        let out = truncate_middle(&text, MESSAGE_TEXT_CAP);
        // Must not panic and must remain valid UTF-8 (implicit) with marker.
        assert!(out.contains("characters elided"));
    }

    // ── system_prompt ─────────────────────────────────────────────────

    #[test]
    fn default_prompt_directs_neutral_relay_and_forbids_refusals() {
        let p = default_summarize_prompt();
        assert!(p.contains("neutral relay"));
        assert!(p.contains("do not refuse"));
        assert!(p.contains("Never reply that you cannot process or summarize"));
    }

    #[test]
    fn system_prompt_uses_default_then_override_then_instructions() {
        let mut cfg = Config::default();
        assert_eq!(system_prompt(&cfg, ""), default_summarize_prompt());

        cfg.mcp.summarize_prompt = "Custom prompt.".into();
        assert_eq!(system_prompt(&cfg, ""), "Custom prompt.");

        let with_instr = system_prompt(&cfg, "Speak Romanian.");
        assert!(with_instr.starts_with("Custom prompt."));
        assert!(with_instr.contains("Caller instructions: Speak Romanian."));
    }

    // ── summarize / summarize_with ────────────────────────────────────

    #[tokio::test]
    async fn summarize_rejects_empty_message_text() {
        let cfg = Config::default();
        let secrets = fono_core::Secrets::default();
        let p = SummarizePayload::default();
        let err = summarize(&cfg, &secrets, std::path::Path::new("."), &p)
            .await
            .expect_err("empty message_text must fail");
        assert!(err.to_string().contains("message_text"));
    }

    #[tokio::test]
    async fn summarize_errors_when_assistant_disabled() {
        // Default config: `[assistant] enabled = false`, backend = none.
        let cfg = Config::default();
        let secrets = fono_core::Secrets::default();
        let err = summarize(&cfg, &secrets, std::path::Path::new("."), &payload("hi"))
            .await
            .expect_err("disabled assistant must fail");
        assert!(err.to_string().contains("disabled"), "got: {err:#}");
    }

    /// Mock assistant: records the request, replies with canned deltas.
    struct MockAssistant {
        deltas: Vec<TokenDelta>,
        seen_user: Arc<Mutex<String>>,
        seen_system: Arc<Mutex<String>>,
    }

    #[async_trait::async_trait]
    impl Assistant for MockAssistant {
        async fn reply_stream(
            &self,
            user_text: &str,
            ctx: &AssistantContext,
        ) -> anyhow::Result<BoxStream<'static, anyhow::Result<TokenDelta>>> {
            *self.seen_user.lock().unwrap() = user_text.to_string();
            *self.seen_system.lock().unwrap() = ctx.system_prompt.clone();
            let items: Vec<anyhow::Result<TokenDelta>> =
                self.deltas.clone().into_iter().map(Ok).collect();
            Ok(futures::stream::iter(items).boxed())
        }

        fn name(&self) -> &'static str {
            "mock"
        }
    }

    #[tokio::test]
    async fn summarize_with_collects_text_and_skips_tool_events() {
        let seen_user = Arc::new(Mutex::new(String::new()));
        let seen_system = Arc::new(Mutex::new(String::new()));
        let mock = MockAssistant {
            deltas: vec![
                TokenDelta::text("Mihai reports ".into()),
                TokenDelta::tool(ToolEvent::Result {
                    tool_call_id: "t1".into(),
                    summary: "ignored".into(),
                }),
                TokenDelta::text("a deployment failure.".into()),
            ],
            seen_user: Arc::clone(&seen_user),
            seen_system: Arc::clone(&seen_system),
        };
        let cfg = Config::default();
        let summary = summarize_with(&mock, &cfg, &payload("raw log content"))
            .await
            .expect("mock summarize succeeds");
        assert_eq!(summary, "Mihai reports a deployment failure.");
        assert!(seen_user.lock().unwrap().contains("raw log content"));
        assert_eq!(*seen_system.lock().unwrap(), default_summarize_prompt());
    }

    #[tokio::test]
    async fn summarize_with_empty_reply_errors() {
        let mock = MockAssistant {
            deltas: vec![TokenDelta::text("   ".into())],
            seen_user: Arc::new(Mutex::new(String::new())),
            seen_system: Arc::new(Mutex::new(String::new())),
        };
        let cfg = Config::default();
        let err =
            summarize_with(&mock, &cfg, &payload("hi")).await.expect_err("blank reply must fail");
        assert!(err.to_string().contains("empty summary"));
    }

    // ── collapse / refusal fallback ───────────────────────────────────

    #[test]
    fn collapse_removes_consecutive_duplicate_sentences() {
        let looped = "I cannot process this request. I cannot process this request. \
                      I cannot process this request.";
        assert_eq!(collapse_repeated_sentences(looped), "I cannot process this request.");
        // A trailing truncated duplicate (no closing punctuation) is
        // normalized away too.
        let truncated = "Mihai is angry. Mihai is angry";
        assert_eq!(collapse_repeated_sentences(truncated), "Mihai is angry.");
        // Distinct sentences survive untouched.
        let healthy = "Mihai reports a failure. He asks for a redeploy.";
        assert_eq!(collapse_repeated_sentences(healthy), healthy);
    }

    #[test]
    fn refusal_detection_matches_openers_only() {
        assert!(looks_like_refusal("I cannot process this request because…"));
        assert!(looks_like_refusal("I'm unable to summarize this message."));
        assert!(looks_like_refusal("As an AI, I must decline."));
        // Mid-sentence mention of inability is NOT a refusal.
        assert!(!looks_like_refusal("Mihai says he cannot deploy the service."));
        assert!(!looks_like_refusal("Bogdan sent an angry, insulting message."));
    }

    #[test]
    fn metadata_fallback_prefers_chat_then_app() {
        let p = payload("x");
        assert_eq!(metadata_fallback(&p), "Mihai sent a message in Backend Alerts.");
        let mut no_chat = p.clone();
        no_chat.chat_name = String::new();
        assert_eq!(metadata_fallback(&no_chat), "Mihai sent a message via chat-cli.");
        let bare = SummarizePayload::default();
        assert_eq!(metadata_fallback(&bare), "Someone sent a message.");
    }

    #[tokio::test]
    async fn summarize_with_replaces_looped_refusal_with_metadata_fallback() {
        let mock = MockAssistant {
            deltas: vec![TokenDelta::text(
                "I cannot process this request because the message contains offensive \
                 language. I cannot process this request because the message contains \
                 offensive language."
                    .into(),
            )],
            seen_user: Arc::new(Mutex::new(String::new())),
            seen_system: Arc::new(Mutex::new(String::new())),
        };
        let cfg = Config::default();
        let summary = summarize_with(&mock, &cfg, &payload("hostile content"))
            .await
            .expect("refusal must degrade to metadata fallback, not error");
        assert_eq!(summary, "Mihai sent a message in Backend Alerts.");
    }

    // ── llm_timeouts ──────────────────────────────────────────────────

    #[test]
    fn llm_timeouts_local_vs_cloud() {
        assert_eq!(
            llm_timeouts(&AssistantBackend::Ollama),
            (LOCAL_OPEN_TIMEOUT, LOCAL_DRAIN_TIMEOUT)
        );
        for cloud in [
            AssistantBackend::OpenAI,
            AssistantBackend::Anthropic,
            AssistantBackend::Groq,
            AssistantBackend::Cerebras,
            AssistantBackend::OpenRouter,
            AssistantBackend::None,
        ] {
            assert_eq!(llm_timeouts(&cloud), (CLOUD_OPEN_TIMEOUT, CLOUD_DRAIN_TIMEOUT));
        }
        assert!(CLOUD_OPEN_TIMEOUT < LOCAL_OPEN_TIMEOUT);
        assert!(CLOUD_DRAIN_TIMEOUT < LOCAL_DRAIN_TIMEOUT);
    }

    // ── fallback_candidates ───────────────────────────────────────────

    #[test]
    fn fallback_candidates_exclude_primary_and_keep_order() {
        let candidates = fallback_candidates(&AssistantBackend::Cerebras);
        assert_eq!(candidates.first(), Some(&AssistantBackend::Groq));
        assert!(!candidates.contains(&AssistantBackend::Cerebras));
        assert_eq!(candidates.last(), Some(&AssistantBackend::Ollama));
        assert_eq!(candidates.len(), FALLBACK_ORDER.len() - 1);

        // A non-listed primary (None) keeps the full order.
        let all = fallback_candidates(&AssistantBackend::None);
        assert_eq!(all.first(), Some(&AssistantBackend::Cerebras));
        assert_eq!(all.len(), FALLBACK_ORDER.len());
    }

    // ── summarize_with_retry ──────────────────────────────────────────

    /// Mock that fails the first `fail_first` calls, then succeeds.
    struct FlakyAssistant {
        fail_first: usize,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Assistant for FlakyAssistant {
        async fn reply_stream(
            &self,
            _user_text: &str,
            _ctx: &AssistantContext,
        ) -> anyhow::Result<BoxStream<'static, anyhow::Result<TokenDelta>>> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n < self.fail_first {
                return Err(anyhow!("simulated transient failure"));
            }
            Ok(futures::stream::iter(vec![Ok(TokenDelta::text("Recovered summary.".into()))])
                .boxed())
        }

        fn name(&self) -> &'static str {
            "flaky"
        }
    }

    #[tokio::test]
    async fn retry_succeeds_on_second_attempt() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let flaky = FlakyAssistant { fail_first: 1, calls: Arc::clone(&calls) };
        let cfg = Config::default();
        let summary = summarize_with_retry(&flaky, &cfg, &payload("hi"))
            .await
            .expect("retry must recover from a single transient failure");
        assert_eq!(summary, "Recovered summary.");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn retry_gives_up_after_two_attempts() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let flaky = FlakyAssistant { fail_first: usize::MAX, calls: Arc::clone(&calls) };
        let cfg = Config::default();
        let err = summarize_with_retry(&flaky, &cfg, &payload("hi"))
            .await
            .expect_err("persistent failure must surface after the retry");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(
            format!("{err:#}").contains("retry on the same backend also failed"),
            "got: {err:#}"
        );
    }
}
