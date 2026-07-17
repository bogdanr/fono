// SPDX-License-Identifier: GPL-3.0-only
//! Local LLM inference HTTP server — OpenAI- and Ollama-compatible.
//!
//! Serves the daemon's active `Arc<dyn Assistant>` (embedded llama.cpp
//! or a cloud backend) over two wire formats on one listener:
//!
//! * **OpenAI-compatible** — `GET /v1/models`, `POST /v1/chat/completions`
//!   (SSE stream or single JSON).
//! * **Ollama-native** — `GET /api/tags`, `POST /api/chat` (NDJSON stream
//!   or single JSON), `GET /api/version`.
//!
//! Both surfaces map their `messages[]` onto the same
//! [`fono_assistant::traits::AssistantContext`] and drive the one
//! [`fono_assistant::traits::Assistant::reply_stream`]. The server is a
//! thin wire adapter — no inference logic lives here.
//!
//! ## Why raw hyper (no axum)
//!
//! `hyper`/`hyper-util`/`http-body-util`/`bytes` are already in Fono's
//! dependency graph via `reqwest`'s client stack, so enabling hyper's
//! `server` + `http1` features and hand-rolling a small route `match`
//! adds **no new crate** to the shipped binary. `axum`/`matchit` would
//! be net-new dependencies for ergonomics the ~6-route surface does not
//! need. See ADR 0036.
//!
//! Mirrors the `wyoming::server` pattern: one accept loop, one task per
//! connection, a provider closure invoked per request so `Reload`-driven
//! backend swaps are tracked without restarting the listener, and a
//! loopback-only guard for defence in depth.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use fono_assistant::traits::Assistant;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Serialize;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

mod access_log;
mod audio;
mod messages;
mod ollama;
mod openai;
mod proxy;

use access_log::ReqLog;
pub use audio::{TranscribeProvider, TranscribeRequest};

use crate::auth::{AuthVerifier, UsageSink};

/// Default port — Ollama's, so Ollama/OpenAI clients (and Home
/// Assistant's Ollama conversation agent) point at Fono unchanged.
pub const DEFAULT_PORT: u16 = 11_434;

/// Defensive cap on request-body size. Chat payloads are tiny; this only
/// stops a hostile peer streaming an unbounded body.
pub(crate) const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Response body type used across the server.
pub(crate) type ResBody = BoxBody<Bytes, Infallible>;

/// Provider closure invoked per request to obtain the currently-active
/// assistant backend. Returns `None` when no assistant is configured
/// (degraded / cloud-less daemon state), surfaced as HTTP 503. Returning
/// a fresh `Arc` per call tracks `Reload`-driven swaps without a restart.
pub type AssistantProvider = Arc<dyn Fn() -> Option<Arc<dyn Assistant>> + Send + Sync>;

/// Provider closure invoked per request to obtain the cloud upstream to
/// pass through to, when the served backend is an OpenAI-compatible
/// cloud provider. `None` means "not proxyable" — the OpenAI handlers
/// fall back to driving the [`AssistantProvider`] adapter. Returning a
/// fresh value per call tracks `Reload`-driven backend swaps without a
/// restart. See ADR 0036.
pub type UpstreamProvider =
    Arc<dyn Fn() -> Option<Arc<fono_assistant::CloudUpstream>> + Send + Sync>;

/// Speech-synthesis handler for `POST /v1/audio/speech` — the same closure
/// type the settings server mounts, so routing/synthesis logic is shared.
pub type SpeechProvider = crate::web_settings::SpeechFn;

/// Configuration for [`LlmServer::start`]. Built from `[server.llm]` at
/// the daemon layer; tests construct it directly.
#[derive(Debug, Clone)]
pub struct LlmServerConfig {
    /// Bind host. `127.0.0.1` is the safe default.
    pub bind: String,
    /// TCP port. Default [`DEFAULT_PORT`] (`11434`).
    pub port: u16,
    /// Require a valid inbound API key. When `true`, non-loopback callers
    /// must present `Authorization: Bearer <key>` matching an entry in the
    /// API-key store (see [`LlmServer::with_auth`]). Loopback callers are
    /// always trusted so a local client is never locked out. When `false`
    /// the surface is open (loopback-only deployments that opt out).
    pub auth_enabled: bool,
    /// Model id surfaced by `/v1/models` and `/api/tags`. Cosmetic — the
    /// server always drives the one configured assistant regardless of
    /// the `model` field a client sends.
    pub model_name: String,
    /// Version string surfaced by `/api/version`.
    pub server_version: String,
    /// When `true`, refuses non-loopback peers even if the bind address
    /// would have allowed them. Set when `bind = "127.0.0.1"`.
    pub loopback_only: bool,
}

impl Default for LlmServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1".to_string(),
            port: DEFAULT_PORT,
            auth_enabled: true,
            model_name: "fono".to_string(),
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            loopback_only: true,
        }
    }
}

/// Shared per-request context handed to the format handlers.
#[derive(Clone)]
pub(crate) struct ServerCtx {
    pub cfg: Arc<LlmServerConfig>,
    pub assistant: AssistantProvider,
    /// Cloud pass-through upstream (OpenAI surface only). `|| None` when
    /// the backend is not proxyable. See ADR 0036.
    pub upstream: UpstreamProvider,
    /// Optional `POST /v1/audio/speech` handler. `None` = route 404s.
    pub speech: Option<SpeechProvider>,
    /// Optional `POST /v1/audio/transcriptions` handler. `None` = route 404s.
    pub transcribe: Option<TranscribeProvider>,
    /// Verifier mapping a presented bearer token to a key id. `None` with
    /// `auth_enabled = true` fails closed (all non-loopback callers 401).
    pub verifier: Option<AuthVerifier>,
    /// Records one authenticated hit against the matched key id.
    pub usage: Option<UsageSink>,
}

/// Handle returned by [`LlmServer::start`]. Drop or call
/// [`LlmServerHandle::shutdown`] to stop the listener.
pub struct LlmServerHandle {
    pub local_addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl LlmServerHandle {
    /// Bound socket address (useful in tests with `port = 0`).
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Politely stop the listener; in-flight connections finish.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.join.take() {
            let _ = h.await;
        }
    }
}

impl Drop for LlmServerHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// The server itself. Stateless beyond the config + provider closures.
pub struct LlmServer {
    cfg: LlmServerConfig,
    assistant: AssistantProvider,
    upstream: UpstreamProvider,
    speech: Option<SpeechProvider>,
    transcribe: Option<TranscribeProvider>,
    verifier: Option<AuthVerifier>,
    usage: Option<UsageSink>,
}

impl LlmServer {
    /// Build a server. Does not bind yet — call [`Self::start`].
    #[must_use]
    pub fn new(cfg: LlmServerConfig, assistant: AssistantProvider) -> Self {
        Self {
            cfg,
            assistant,
            upstream: Arc::new(|| None),
            speech: None,
            transcribe: None,
            verifier: None,
            usage: None,
        }
    }

    /// Attach the inbound-auth verifier and usage sink. Required for
    /// `auth_enabled = true` to admit any non-loopback caller.
    #[must_use]
    pub fn with_auth(mut self, verifier: AuthVerifier, usage: UsageSink) -> Self {
        self.verifier = Some(verifier);
        self.usage = Some(usage);
        self
    }

    /// Attach the OpenAI-compatible audio handlers (`/v1/audio/speech`,
    /// `/v1/audio/transcriptions`). Absent handlers make their routes 404.
    #[must_use]
    pub fn with_audio(
        mut self,
        speech: Option<SpeechProvider>,
        transcribe: Option<TranscribeProvider>,
    ) -> Self {
        self.speech = speech;
        self.transcribe = transcribe;
        self
    }

    /// Attach the cloud pass-through upstream provider (ADR 0036). When
    /// it yields `Some`, the OpenAI surface proxies verbatim; otherwise
    /// it drives the assistant adapter.
    #[must_use]
    pub fn with_upstream(mut self, upstream: UpstreamProvider) -> Self {
        self.upstream = upstream;
        self
    }

    /// Convenience: pin a single assistant backend for the listener's
    /// lifetime (no `Reload` tracking). Tests use this.
    #[must_use]
    pub fn with_fixed(cfg: LlmServerConfig, assistant: Arc<dyn Assistant>) -> Self {
        Self::new(cfg, Arc::new(move || Some(Arc::clone(&assistant))))
    }

    /// Bind the listener and spawn the accept loop. Returns once the
    /// socket is listening so callers can `.local_addr()` immediately.
    pub async fn start(self) -> Result<LlmServerHandle> {
        let addr = format!("{}:{}", self.cfg.bind, self.cfg.port);
        let listener = TcpListener::bind(&addr)
            .await
            .with_context(|| format!("binding llm server to {addr}"))?;
        let local_addr = listener.local_addr().context("listener.local_addr")?;
        tracing::info!(
            target: "fono::llm::server",
            %local_addr,
            loopback_only = self.cfg.loopback_only,
            model = %self.cfg.model_name,
            "llm server listening"
        );

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let ctx = ServerCtx {
            cfg: Arc::new(self.cfg),
            assistant: self.assistant,
            upstream: self.upstream,
            speech: self.speech,
            transcribe: self.transcribe,
            verifier: self.verifier,
            usage: self.usage,
        };
        let join = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => {
                        tracing::debug!(target: "fono::llm::server", "shutdown signal received");
                        break;
                    }
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((sock, peer)) => {
                                if ctx.cfg.loopback_only && !is_loopback(&peer) {
                                    tracing::warn!(
                                        target: "fono::llm::server",
                                        %peer,
                                        "rejecting non-loopback peer (bind is loopback-only)"
                                    );
                                    drop(sock);
                                    continue;
                                }
                                tokio::spawn(serve_conn(sock, peer, ctx.clone()));
                            }
                            Err(e) => {
                                tracing::warn!(target: "fono::llm::server", "accept failed: {e:#}");
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                        }
                    }
                }
            }
        });

        Ok(LlmServerHandle { local_addr, shutdown_tx: Some(shutdown_tx), join: Some(join) })
    }
}

async fn serve_conn(sock: TcpStream, peer: SocketAddr, ctx: ServerCtx) {
    let io = TokioIo::new(sock);
    let service = service_fn(move |req: Request<Incoming>| {
        let ctx = ctx.clone();
        async move { Ok::<_, Infallible>(route(req, peer, ctx).await) }
    });
    if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
        tracing::debug!(target: "fono::llm::server", "connection ended: {e:#}");
    }
}

/// Classify a request into a `(surface, op)` pair for the access log.
fn classify(path: &str) -> (&'static str, String) {
    match path {
        "/v1/models" => ("openai", "models".to_string()),
        "/v1/chat/completions" => ("openai", "chat".to_string()),
        "/v1/audio/speech" => ("openai", "speech".to_string()),
        "/v1/audio/transcriptions" => ("openai", "transcribe".to_string()),
        "/api/tags" => ("ollama", "tags".to_string()),
        "/api/chat" => ("ollama", "chat".to_string()),
        "/api/version" => ("ollama", "version".to_string()),
        "/" => ("http", "root".to_string()),
        other => ("http", other.trim_start_matches('/').to_string()),
    }
}

/// Dispatch one request. The service layer never fails; every path
/// returns a `Response` (including error responses). Emits one
/// `debug`-level access line per request (streaming handlers defer the
/// line to their body task so it carries ttft/tokens).
async fn route(req: Request<Incoming>, peer: SocketAddr, ctx: ServerCtx) -> Response<ResBody> {
    let (surface, op) = classify(req.uri().path());
    let ua = access_log::compact_ua(
        req.headers().get(hyper::header::USER_AGENT).and_then(|v| v.to_str().ok()),
    );
    // Suppress the peer on loopback (pure noise); show it for LAN callers.
    let peer_field = (!is_loopback(&peer)).then_some(peer);
    let mut log = ReqLog::new(surface, op, ua, peer_field);

    // Auth: when enabled, a presented bearer token is always verified —
    // even from loopback — so a wrong key is rejected and a valid key's id
    // is recorded against its bounded usage counters (last-used, per-day/
    // month). Loopback with *no* token is trusted (local owner; avoids a
    // bootstrap lockout). Fails closed when the verifier is absent.
    if ctx.cfg.auth_enabled {
        match crate::auth::decide(
            ctx.cfg.auth_enabled,
            is_loopback(&peer),
            presented_bearer(&req),
            ctx.verifier.as_ref(),
        ) {
            crate::auth::AuthDecision::Allow(key_id) => {
                if let (Some(id), Some(sink)) = (key_id, ctx.usage.as_ref()) {
                    sink(id);
                }
            }
            crate::auth::AuthDecision::Deny => {
                let resp = error_response(StatusCode::UNAUTHORIZED, "missing or invalid API key");
                log.finish(resp.status().as_u16());
                return resp;
            }
        }
    }
    let method = req.method().as_str().to_owned();
    let path = req.uri().path().to_owned();
    let resp = match (method.as_str(), path.as_str()) {
        ("GET", "/v1/models") => openai::models(&ctx, &mut log).await,
        ("POST", "/v1/chat/completions") => openai::chat(req, &ctx, &mut log).await,
        ("POST", "/v1/audio/speech") => audio::speech(req, &ctx, &mut log).await,
        ("POST", "/v1/audio/transcriptions") => audio::transcriptions(req, &ctx, &mut log).await,
        ("GET", "/api/tags") => ollama::tags(&ctx, &mut log),
        ("POST", "/api/chat") => ollama::chat(req, &ctx, &mut log).await,
        ("GET", "/api/version") => ollama::version(&ctx),
        ("GET" | "HEAD", "/") => text_response(StatusCode::OK, "Fono LLM server\n"),
        _ => error_response(StatusCode::NOT_FOUND, "not found"),
    };
    // Streaming handlers emit from their body task once it drains.
    if !log.deferred {
        log.finish(resp.status().as_u16());
    }
    resp
}

fn is_loopback(addr: &SocketAddr) -> bool {
    match addr.ip() {
        std::net::IpAddr::V4(v) => v.is_loopback(),
        std::net::IpAddr::V6(v) => v.is_loopback(),
    }
}

/// Extract the presented bearer token (`Authorization: Bearer <tok>`).
fn presented_bearer(req: &Request<Incoming>) -> Option<&str> {
    req.headers()
        .get(hyper::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
}

// --- response builders (shared by the format modules) --------------------

pub(crate) fn full(bytes: Bytes) -> ResBody {
    Full::new(bytes).boxed()
}

pub(crate) fn json_ok<T: Serialize>(value: &T) -> Response<ResBody> {
    let Ok(bytes) = serde_json::to_vec(value) else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "serialization error");
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(full(Bytes::from(bytes)))
        .expect("static response builder")
}

pub(crate) fn error_response(status: StatusCode, msg: &str) -> Response<ResBody> {
    let body = serde_json::to_vec(&serde_json::json!({ "error": msg })).unwrap_or_default();
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(full(Bytes::from(body)))
        .expect("static response builder")
}

/// OpenAI-shaped error body (`{"error": {"message", "type"}}`) for the
/// `/v1/audio/*` routes so off-the-shelf OpenAI clients parse failures.
pub(crate) fn openai_error(status: StatusCode, msg: &str) -> Response<ResBody> {
    let body = serde_json::to_vec(&serde_json::json!({
        "error": { "message": msg, "type": "invalid_request_error" }
    }))
    .unwrap_or_default();
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(full(Bytes::from(body)))
        .expect("static response builder")
}

/// Binary audio response (WAV or raw PCM) for `/v1/audio/speech`.
pub(crate) fn audio_response(content_type: String, bytes: Vec<u8>) -> Response<ResBody> {
    Response::builder()
        .status(StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, content_type)
        .header(hyper::header::CACHE_CONTROL, "no-store")
        .body(full(Bytes::from(bytes)))
        .expect("static response builder")
}

pub(crate) fn text_response(status: StatusCode, body: &str) -> Response<ResBody> {
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(full(Bytes::from(body.to_owned())))
        .expect("static response builder")
}

pub(crate) fn sse_response(body: ResBody) -> Response<ResBody> {
    Response::builder()
        .status(StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, "text/event-stream")
        .header(hyper::header::CACHE_CONTROL, "no-cache")
        .body(body)
        .expect("static response builder")
}

pub(crate) fn ndjson_response(body: ResBody) -> Response<ResBody> {
    Response::builder()
        .status(StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, "application/x-ndjson")
        .body(body)
        .expect("static response builder")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_loopback_ollama_port() {
        let cfg = LlmServerConfig::default();
        assert_eq!(cfg.bind, "127.0.0.1");
        assert_eq!(cfg.port, 11_434);
        assert!(cfg.loopback_only);
        assert!(cfg.auth_enabled);
    }

    #[test]
    fn json_ok_sets_content_type() {
        let resp = json_ok(&serde_json::json!({ "ok": true }));
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[hyper::header::CONTENT_TYPE], "application/json");
    }

    #[test]
    fn error_response_carries_status_and_message() {
        let resp = error_response(StatusCode::NOT_FOUND, "not found");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
