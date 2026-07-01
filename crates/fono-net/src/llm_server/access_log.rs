// SPDX-License-Identifier: GPL-3.0-only
//! One human-readable access line per request, at `debug` level.
//!
//! Emitted on the existing `fono::llm::server` target, so it inherits
//! the daemon's `FONO_LOG` filtering with no new machinery. The line is
//! written **at request completion** — for streaming responses that
//! means when the body drains, so it can carry time-to-first-token,
//! total duration, and an output-token count.
//!
//! Layout (fixed field order, so the eye learns the columns even as
//! model names vary in width):
//!
//! ```text
//! <surface>/<op> <status>  <mode>  <model>  [stream]  ttft=… total=…  <N>tok @<tps>/s  via <ua>  [<peer>]
//! ```
//!
//! * `mode` is `proxy→<provider>` when the request passed through to a
//!   cloud upstream, else `adapt` (Fono drove the `Assistant` trait).
//! * `ttft` appears only for streaming responses.
//! * the token/throughput cluster appears only when a count is
//!   available (the adapter path, where each delta is one decoded
//!   token); the proxy path is a byte passthrough and omits it.
//! * `via <ua>` is always shown — on a shared local port it is what
//!   tells one client apart from another.
//! * `<peer>` is shown only for non-loopback callers (a LAN client on a
//!   `0.0.0.0` bind); on loopback it is pure noise and suppressed.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

/// Which path served the request.
#[derive(Clone)]
pub enum Mode {
    /// Not yet determined / not applicable (auth reject, 404, `version`).
    Unknown,
    /// Fono drove the `Assistant` trait (local llama, Anthropic, or the
    /// realtime→text fallback).
    Adapt,
    /// Passed through verbatim to a cloud upstream; the string is a
    /// short provider label derived from the upstream host.
    Proxy(String),
}

impl Mode {
    fn label(&self) -> String {
        match self {
            Self::Unknown => "·".to_string(),
            Self::Adapt => "adapt".to_string(),
            Self::Proxy(p) => format!("proxy→{p}"),
        }
    }
}

/// Derive a short provider label (`gemini`, `openai`, …) from an
/// upstream chat URL, for the `proxy→<provider>` mode tag.
#[must_use]
pub fn provider_label(url: &str) -> String {
    let host = url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or(url);
    let host = host.split(':').next().unwrap_or(host); // strip :port
    for (needle, label) in [
        ("googleapis", "gemini"),
        ("openai.azure", "azure-openai"),
        ("openai", "openai"),
        ("groq", "groq"),
        ("cerebras", "cerebras"),
        ("openrouter", "openrouter"),
        ("anthropic", "anthropic"),
        ("mistral", "mistral"),
    ] {
        if host.contains(needle) {
            return label.to_string();
        }
    }
    host.to_string()
}

/// Collapse a raw `User-Agent` into a compact, readable client label.
/// Recognises common clients for a clean name; otherwise falls back to
/// the first product token, capped. `None`/blank → `?`.
#[must_use]
pub fn compact_ua(raw: Option<&str>) -> String {
    // Ordered: more specific needles first (a browser-style HA UA also
    // contains `aiohttp`, so match `homeassistant` before it).
    const KNOWN: &[(&str, &str)] = &[
        ("homeassistant", "Home Assistant"),
        ("home assistant", "Home Assistant"),
        ("open-webui", "Open WebUI"),
        ("open webui", "Open WebUI"),
        ("openwebui", "Open WebUI"),
        ("ollama", "ollama"),
        ("langchain", "LangChain"),
        ("openai", "OpenAI"),
        ("python-requests", "requests"),
        ("httpx", "httpx"),
        ("aiohttp", "aiohttp"),
        ("node-fetch", "node-fetch"),
        ("undici", "undici"),
        ("go-http-client", "go-http"),
        ("curl", "curl"),
        ("wget", "wget"),
    ];
    let ua = match raw.map(str::trim) {
        Some(s) if !s.is_empty() => s,
        _ => return "?".to_string(),
    };
    let lower = ua.to_ascii_lowercase();
    for (needle, label) in KNOWN {
        if let Some(pos) = lower.find(needle) {
            if let Some(v) = version_after(ua, pos + needle.len()) {
                return format!("{label}/{v}");
            }
            return (*label).to_string();
        }
    }
    // Fallback: first whitespace-delimited token, capped.
    let first = ua.split_whitespace().next().unwrap_or(ua);
    truncate(first, 32)
}

/// Read a `/<version>` immediately following `from` (allowing a short
/// run of non-`/` client-name suffix like `-python` first), capped.
fn version_after(ua: &str, from: usize) -> Option<String> {
    let rest = ua.get(from..)?;
    let slash = rest.find('/')?;
    // Only accept the version if the name suffix before the slash is
    // short (e.g. `-python`), so we don't leap across unrelated tokens.
    if slash > 12 {
        return None;
    }
    let after = &rest[slash + 1..];
    let v: String =
        after.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-').collect();
    if v.is_empty() {
        None
    } else {
        Some(truncate(&v, 12))
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

fn human(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.2}s", d.as_secs_f64())
    }
}

/// All the fields needed to render one line. Shared by the non-stream
/// finaliser and the streaming finaliser so the format lives in one
/// place (and is unit-testable).
pub struct LineFields {
    pub surface: &'static str,
    pub op: String,
    pub status: u16,
    pub mode: Mode,
    pub model: String,
    pub stream: bool,
    pub ttft: Option<Duration>,
    pub total: Duration,
    pub out_tokens: u64,
    pub tps: Option<f64>,
    pub ua: String,
    pub peer: Option<SocketAddr>,
}

#[must_use]
pub fn format_line(f: &LineFields) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(9);
    parts.push(format!("{}/{} {}", f.surface, f.op, f.status));
    parts.push(f.mode.label());
    parts.push(if f.model.is_empty() { "·".to_string() } else { f.model.clone() });
    if f.stream {
        parts.push("stream".to_string());
    }
    let timing = f.ttft.map_or_else(
        || format!("total={}", human(f.total)),
        |t| format!("ttft={} total={}", human(t), human(f.total)),
    );
    parts.push(timing);
    if f.out_tokens > 0 {
        match f.tps {
            Some(tps) => parts.push(format!("{}tok @{:.0}/s", f.out_tokens, tps)),
            None => parts.push(format!("{}tok", f.out_tokens)),
        }
    }
    parts.push(format!("via {}", f.ua));
    if let Some(p) = f.peer {
        parts.push(p.ip().to_string());
    }
    parts.join("  ")
}

fn emit(line: &str) {
    tracing::debug!(target: "fono::llm::server", "{line}");
}

/// Per-request log accumulator, built at dispatch. Non-streaming
/// requests are finalised by `route()` via [`ReqLog::finish`]; streaming
/// requests hand a [`StreamLog`] to the body task via [`ReqLog::defer`],
/// which emits when the stream drains.
pub struct ReqLog {
    surface: &'static str,
    op: String,
    ua: String,
    peer: Option<SocketAddr>,
    mode: Mode,
    model: String,
    start: Instant,
    /// Set once a streaming handler has taken over emission.
    pub deferred: bool,
}

impl ReqLog {
    #[must_use]
    pub fn new(surface: &'static str, op: String, ua: String, peer: Option<SocketAddr>) -> Self {
        Self {
            surface,
            op,
            ua,
            peer,
            mode: Mode::Unknown,
            model: String::new(),
            start: Instant::now(),
            deferred: false,
        }
    }

    /// Record which backend served the request (called by handlers once
    /// they know).
    pub fn set_target(&mut self, mode: Mode, model: String) {
        self.mode = mode;
        self.model = model;
    }

    /// Update just the effective model (the proxy path resolves it from
    /// the client body after the mode is already set).
    pub fn set_model(&mut self, model: String) {
        self.model = model;
    }

    /// Finalise a non-streaming request and emit its line.
    pub fn finish(&self, status: u16) {
        let line = format_line(&LineFields {
            surface: self.surface,
            op: self.op.clone(),
            status,
            mode: self.mode.clone(),
            model: self.model.clone(),
            stream: false,
            ttft: None,
            total: self.start.elapsed(),
            out_tokens: 0,
            tps: None,
            ua: self.ua.clone(),
            peer: self.peer,
        });
        emit(&line);
    }

    /// Hand off to the streaming body task. Marks the request deferred so
    /// `route()` will not also emit. `count_tokens` is true on the
    /// adapter path (deltas ≈ tokens) and false on the proxy passthrough.
    #[must_use]
    pub fn defer(&mut self, count_tokens: bool) -> StreamLog {
        self.deferred = true;
        StreamLog {
            surface: self.surface,
            op: self.op.clone(),
            ua: self.ua.clone(),
            peer: self.peer,
            mode: self.mode.clone(),
            model: self.model.clone(),
            start: self.start,
            first_token: None,
            out_tokens: 0,
            count_tokens,
            status: 200,
        }
    }
}

/// Streaming-request finaliser, moved into the body task. Records
/// time-to-first-frame and (on the adapter path) an output-token count,
/// then emits one line when dropped via [`StreamLog::emit`].
pub struct StreamLog {
    surface: &'static str,
    op: String,
    ua: String,
    peer: Option<SocketAddr>,
    mode: Mode,
    model: String,
    start: Instant,
    first_token: Option<Instant>,
    out_tokens: u64,
    count_tokens: bool,
    status: u16,
}

impl StreamLog {
    /// Record one output token/delta (adapter path).
    pub fn on_token(&mut self) {
        if self.first_token.is_none() {
            self.first_token = Some(Instant::now());
        }
        self.out_tokens += 1;
    }

    /// Record the first relayed frame (proxy path: ttft only, no count).
    pub fn on_frame(&mut self) {
        if self.first_token.is_none() {
            self.first_token = Some(Instant::now());
        }
    }

    /// Override the reported status (proxy path: the upstream status).
    pub fn set_status(&mut self, status: u16) {
        self.status = status;
    }

    /// Emit the completion line.
    pub fn emit(self) {
        let total = self.start.elapsed();
        let ttft = self.first_token.map(|t| t.duration_since(self.start));
        let tps = if self.count_tokens && self.out_tokens > 0 {
            // Throughput over the decode span (first token → now).
            let gen = self.first_token.map_or(total, |t| t.elapsed());
            let secs = gen.as_secs_f64();
            (secs > 0.0).then_some(self.out_tokens as f64 / secs)
        } else {
            None
        };
        let out_tokens = if self.count_tokens { self.out_tokens } else { 0 };
        let line = format_line(&LineFields {
            surface: self.surface,
            op: self.op,
            status: self.status,
            mode: self.mode,
            model: self.model,
            stream: true,
            ttft,
            total,
            out_tokens,
            tps,
            ua: self.ua,
            peer: self.peer,
        });
        emit(&line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ua_known_clients_are_friendly() {
        assert_eq!(compact_ua(Some("ollama-python/0.3.3 (linux)")), "ollama/0.3.3");
        assert_eq!(
            compact_ua(Some("HomeAssistant/2024.12.0 aiohttp/3.11.7 Python/3.12")),
            "Home Assistant/2024.12.0"
        );
        assert_eq!(compact_ua(Some("curl/8.5.0")), "curl/8.5.0");
        assert_eq!(compact_ua(Some("python-httpx/0.27.0")), "httpx/0.27.0");
        assert_eq!(compact_ua(Some("Open WebUI")), "Open WebUI");
    }

    #[test]
    fn ua_missing_or_blank_is_question_mark() {
        assert_eq!(compact_ua(None), "?");
        assert_eq!(compact_ua(Some("   ")), "?");
    }

    #[test]
    fn ua_unknown_falls_back_to_first_token_capped() {
        assert_eq!(compact_ua(Some("weirdclient/9.9 (extra bits)")), "weirdclient/9.9");
        let long = "a".repeat(50);
        assert!(compact_ua(Some(&long)).chars().count() <= 32);
    }

    #[test]
    fn provider_label_maps_known_hosts() {
        assert_eq!(
            provider_label(
                "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
            ),
            "gemini"
        );
        assert_eq!(provider_label("https://api.openai.com/v1/chat/completions"), "openai");
        assert_eq!(provider_label("https://api.groq.com/openai/v1/chat/completions"), "groq");
    }

    #[test]
    fn line_stream_full_shape() {
        let line = format_line(&LineFields {
            surface: "openai",
            op: "chat".to_string(),
            status: 200,
            mode: Mode::Proxy("gemini".to_string()),
            model: "gemini-flash-lite-latest".to_string(),
            stream: true,
            ttft: Some(Duration::from_millis(310)),
            total: Duration::from_millis(1840),
            out_tokens: 214,
            tps: Some(116.0),
            ua: "ollama/0.3.3".to_string(),
            peer: None,
        });
        assert_eq!(
            line,
            "openai/chat 200  proxy→gemini  gemini-flash-lite-latest  stream  ttft=310ms total=1.84s  214tok @116/s  via ollama/0.3.3"
        );
    }

    #[test]
    fn line_nonstream_minimal_and_peer_shown() {
        let peer: SocketAddr = "192.168.1.50:5000".parse().unwrap();
        let line = format_line(&LineFields {
            surface: "openai",
            op: "chat".to_string(),
            status: 401,
            mode: Mode::Unknown,
            model: String::new(),
            stream: false,
            ttft: None,
            total: Duration::from_millis(0),
            out_tokens: 0,
            tps: None,
            ua: "curl/8.5.0".to_string(),
            peer: Some(peer),
        });
        assert_eq!(line, "openai/chat 401  ·  ·  total=0ms  via curl/8.5.0  192.168.1.50");
    }
}
