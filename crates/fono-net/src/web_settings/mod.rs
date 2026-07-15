// SPDX-License-Identifier: GPL-3.0-only
//! Web settings UI server — embedded browser configuration screen.
//!
//! Serves three embedded static assets (`/`, `/app.css`, `/app.js`) and a
//! small JSON API:
//!
//! * `GET /api/config` — the full config as JSON (secret *references* only;
//!   never secret values).
//! * `PUT /api/config` — replace the config. The daemon-side hook validates,
//!   persists the TOML atomically, and hot-reloads the orchestrator.
//! * `GET /api/meta` — version, config path, which secret names are set
//!   (booleans only), and baked-in prompt defaults for "Reset to default".
//! * `PUT /api/secret/{NAME}` — write-only secret update (`{"value": "…"}`;
//!   empty value clears). Responses never echo stored values.
//! * `GET /api/doctor` — run the daemon-side doctor checks and return the
//!   structured report as JSON (sections → checks with severities plus an
//!   aggregate). Token-gated like every other `/api/*` route — the report
//!   describes system topology.
//!
//! ## Why raw hyper (no axum)
//!
//! Same rationale as `llm_server` (ADR 0036): the hyper stack is already in
//! the shipped binary via reqwest, so this module adds **no new crate**. The
//! HTML/CSS/JS assets are embedded with `include_str!` — no images, fonts, or
//! external requests; tens of KB total.
//!
//! Mirrors the `llm_server` pattern: one accept loop, one task per
//! connection, hook closures invoked per request so config state is always
//! fresh, and a loopback-only guard for defence in depth.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use futures::future::BoxFuture;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Serialize;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// Default port for the web settings UI.
pub const DEFAULT_PORT: u16 = 10_808;

/// Defensive cap on request-body size. A full config JSON is a few KB;
/// this only stops a hostile peer streaming an unbounded body.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Embedded page assets. Design source: the 2026-07-02 search-first
/// accordion handoff (see `plans/2026-07-02-web-config-ui-v2.md`).
pub const INDEX_HTML: &str = include_str!("assets/index.html");
pub const APP_CSS: &str = include_str!("assets/app.css");
pub const APP_JS: &str = include_str!("assets/app.js");

/// Response body type used across the server.
type ResBody = BoxBody<Bytes, Infallible>;

/// Read the current config as JSON. Secret *references* only.
pub type GetConfigFn =
    Arc<dyn Fn() -> std::result::Result<serde_json::Value, String> + Send + Sync>;
/// Validate + persist a replacement config, then hot-reload. Returns a
/// short human-readable summary on success.
pub type PutConfigFn = Arc<
    dyn Fn(serde_json::Value) -> BoxFuture<'static, std::result::Result<String, String>>
        + Send
        + Sync,
>;
/// Write-only secret update: `(name, value)`. Empty value clears the entry.
pub type SetSecretFn = Arc<dyn Fn(&str, &str) -> std::result::Result<(), String> + Send + Sync>;
/// Read the personal vocabulary (`vocabulary.toml`) as JSON:
/// `{"vocabulary": [{"from": […], "to": "…"}, …]}`.
pub type GetVocabularyFn =
    Arc<dyn Fn() -> std::result::Result<serde_json::Value, String> + Send + Sync>;
/// Validate + persist a replacement vocabulary. Same shape as the getter.
pub type PutVocabularyFn =
    Arc<dyn Fn(serde_json::Value) -> std::result::Result<String, String> + Send + Sync>;
/// Metadata for the page chrome: version, config path, secret statuses,
/// prompt defaults.
pub type MetaFn = Arc<dyn Fn() -> serde_json::Value + Send + Sync>;
/// Run the doctor checks and return the structured report as JSON.
/// Async: the daemon side runs the probes on a blocking-friendly task.
pub type DoctorFn = Arc<
    dyn Fn() -> BoxFuture<'static, std::result::Result<serde_json::Value, String>> + Send + Sync,
>;
/// OpenAI-compatible `POST /v1/audio/speech` handler. Takes the parsed
/// request body (`{model, input, voice, response_format?}`) and returns the
/// synthesized audio as `(content_type, bytes)` — WAV or raw PCM. Async: the
/// daemon side builds the requested engine and runs synthesis off the accept
/// loop. Errors are surfaced as an OpenAI-shaped 4xx/5xx by the caller.
pub type SpeechFn = Arc<
    dyn Fn(serde_json::Value) -> BoxFuture<'static, std::result::Result<(String, Vec<u8>), String>>
        + Send
        + Sync,
>;

/// Hook closures supplied by the daemon layer. The server itself is a thin
/// wire adapter with no config semantics.
#[derive(Clone)]
pub struct WebSettingsHooks {
    pub get_config: GetConfigFn,
    pub put_config: PutConfigFn,
    pub set_secret: SetSecretFn,
    pub get_vocabulary: GetVocabularyFn,
    pub put_vocabulary: PutVocabularyFn,
    pub meta: MetaFn,
    pub doctor: DoctorFn,
    /// OpenAI-compatible speech synthesis handler for `POST /v1/audio/speech`.
    pub speak: SpeechFn,
}

/// Configuration for [`WebSettingsServer::start`]. Built from
/// `[server.web]` at the daemon layer; tests construct it directly.
#[derive(Debug, Clone)]
pub struct WebSettingsConfig {
    /// Bind host. `127.0.0.1` is the safe default.
    pub bind: String,
    /// TCP port. Default [`DEFAULT_PORT`] (`10808`).
    pub port: u16,
    /// Optional pre-shared bearer token. When `Some`, `/api/*` requests
    /// must carry `Authorization: Bearer <token>` or `?token=<token>`.
    /// The static assets are served without auth (they contain no state).
    pub auth_token: Option<String>,
    /// When `true`, refuses non-loopback peers even if the bind address
    /// would have allowed them. Set when `bind = "127.0.0.1"`.
    pub loopback_only: bool,
}

impl Default for WebSettingsConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1".to_string(),
            port: DEFAULT_PORT,
            auth_token: None,
            loopback_only: true,
        }
    }
}

/// Handle returned by [`WebSettingsServer::start`]. Drop or call
/// [`WebSettingsHandle::shutdown`] to stop the listener.
pub struct WebSettingsHandle {
    pub local_addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl WebSettingsHandle {
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

impl Drop for WebSettingsHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

#[derive(Clone)]
struct ServerCtx {
    cfg: Arc<WebSettingsConfig>,
    hooks: WebSettingsHooks,
}

/// The server itself. Stateless beyond the config + hook closures.
pub struct WebSettingsServer {
    cfg: WebSettingsConfig,
    hooks: WebSettingsHooks,
}

impl WebSettingsServer {
    /// Build a server. Does not bind yet — call [`Self::start`].
    #[must_use]
    pub fn new(cfg: WebSettingsConfig, hooks: WebSettingsHooks) -> Self {
        Self { cfg, hooks }
    }

    /// Bind the listener and spawn the accept loop. Returns once the
    /// socket is listening so callers can `.local_addr()` immediately.
    pub async fn start(self) -> Result<WebSettingsHandle> {
        let addr = format!("{}:{}", self.cfg.bind, self.cfg.port);
        let listener = TcpListener::bind(&addr)
            .await
            .with_context(|| format!("binding web settings server to {addr}"))?;
        let local_addr = listener.local_addr().context("listener.local_addr")?;
        tracing::info!(
            target: "fono::web::server",
            %local_addr,
            loopback_only = self.cfg.loopback_only,
            "web settings server listening"
        );

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let ctx = ServerCtx { cfg: Arc::new(self.cfg), hooks: self.hooks };
        let join = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => {
                        tracing::debug!(target: "fono::web::server", "shutdown signal received");
                        break;
                    }
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((sock, peer)) => {
                                if ctx.cfg.loopback_only && !is_loopback(&peer) {
                                    tracing::warn!(
                                        target: "fono::web::server",
                                        %peer,
                                        "rejecting non-loopback peer (bind is loopback-only)"
                                    );
                                    drop(sock);
                                    continue;
                                }
                                tokio::spawn(serve_conn(sock, ctx.clone()));
                            }
                            Err(e) => {
                                tracing::warn!(target: "fono::web::server", "accept failed: {e:#}");
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                        }
                    }
                }
            }
        });

        Ok(WebSettingsHandle { local_addr, shutdown_tx: Some(shutdown_tx), join: Some(join) })
    }
}

async fn serve_conn(sock: TcpStream, ctx: ServerCtx) {
    let io = TokioIo::new(sock);
    let service = service_fn(move |req: Request<Incoming>| {
        let ctx = ctx.clone();
        async move { Ok::<_, Infallible>(route(req, ctx).await) }
    });
    if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
        tracing::debug!(target: "fono::web::server", "connection ended: {e:#}");
    }
}

/// Dispatch one request. The service layer never fails; every path
/// returns a `Response` (including error responses).
async fn route(req: Request<Incoming>, ctx: ServerCtx) -> Response<ResBody> {
    let path = req.uri().path().to_owned();
    let method = req.method().clone();

    // Static assets — no auth (no state, no secrets).
    match (&method, path.as_str()) {
        (&Method::GET | &Method::HEAD, "/" | "/index.html") => {
            return asset(INDEX_HTML, "text/html; charset=utf-8");
        }
        (&Method::GET, "/app.css") => return asset(APP_CSS, "text/css; charset=utf-8"),
        (&Method::GET, "/app.js") => {
            return asset(APP_JS, "text/javascript; charset=utf-8");
        }
        _ => {}
    }

    // Everything else is the JSON API — token-gated when configured.
    if let Some(expected) = ctx.cfg.auth_token.as_deref() {
        if !token_ok(&req, expected) {
            return error_response(StatusCode::UNAUTHORIZED, "missing or invalid token");
        }
    }
    match (&method, path.as_str()) {
        (&Method::GET, "/api/config") => match (ctx.hooks.get_config)() {
            Ok(v) => json_ok(&v),
            Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e),
        },
        (&Method::PUT, "/api/config") => {
            let Some(body) = read_json_body(req).await else {
                return error_response(StatusCode::BAD_REQUEST, "invalid or oversized JSON body");
            };
            match (ctx.hooks.put_config)(body).await {
                Ok(summary) => json_ok(&serde_json::json!({ "ok": true, "summary": summary })),
                Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e),
            }
        }
        (&Method::GET, "/api/meta") => json_ok(&(ctx.hooks.meta)()),
        (&Method::POST, "/v1/audio/speech") => {
            let Some(body) = read_json_body(req).await else {
                return openai_error(StatusCode::BAD_REQUEST, "invalid or oversized JSON body");
            };
            match (ctx.hooks.speak)(body).await {
                Ok((content_type, bytes)) => audio_response(content_type, bytes),
                Err(e) => openai_error(StatusCode::BAD_REQUEST, &e),
            }
        }
        (&Method::GET, "/api/doctor") => match (ctx.hooks.doctor)().await {
            Ok(v) => json_ok(&v),
            Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e),
        },
        (&Method::GET, "/api/vocabulary") => match (ctx.hooks.get_vocabulary)() {
            Ok(v) => json_ok(&v),
            Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e),
        },
        (&Method::PUT, "/api/vocabulary") => {
            let Some(body) = read_json_body(req).await else {
                return error_response(StatusCode::BAD_REQUEST, "invalid or oversized JSON body");
            };
            match (ctx.hooks.put_vocabulary)(body) {
                Ok(summary) => json_ok(&serde_json::json!({ "ok": true, "summary": summary })),
                Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e),
            }
        }
        (&Method::PUT, p) if p.starts_with("/api/secret/") => {
            let name = p.trim_start_matches("/api/secret/").to_owned();
            if !valid_secret_name(&name) {
                return error_response(StatusCode::BAD_REQUEST, "invalid secret name");
            }
            let Some(body) = read_json_body(req).await else {
                return error_response(StatusCode::BAD_REQUEST, "invalid or oversized JSON body");
            };
            let Some(value) = body.get("value").and_then(|v| v.as_str()) else {
                return error_response(StatusCode::BAD_REQUEST, "body must be {\"value\": \"…\"}");
            };
            match (ctx.hooks.set_secret)(&name, value) {
                Ok(()) => json_ok(&serde_json::json!({ "ok": true })),
                Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e),
            }
        }
        _ => error_response(StatusCode::NOT_FOUND, "not found"),
    }
}

/// Secret names are env-var shaped: `[A-Z][A-Z0-9_]*`, sane length.
fn valid_secret_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Bearer header or `?token=` query parameter.
fn token_ok(req: &Request<Incoming>, expected: &str) -> bool {
    let bearer = req
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .is_some_and(|tok| tok == expected);
    if bearer {
        return true;
    }
    req.uri()
        .query()
        .is_some_and(|q| q.split('&').any(|kv| kv.strip_prefix("token=") == Some(expected)))
}

async fn read_json_body(req: Request<Incoming>) -> Option<serde_json::Value> {
    let limited = Limited::new(req.into_body(), MAX_BODY_BYTES);
    let bytes = limited.collect().await.ok()?.to_bytes();
    serde_json::from_slice(&bytes).ok()
}

fn is_loopback(addr: &SocketAddr) -> bool {
    match addr.ip() {
        std::net::IpAddr::V4(v) => v.is_loopback(),
        std::net::IpAddr::V6(v) => v.is_loopback(),
    }
}

// --- response builders ----------------------------------------------------

fn full(bytes: Bytes) -> ResBody {
    Full::new(bytes).boxed()
}

fn asset(body: &'static str, content_type: &str) -> Response<ResBody> {
    Response::builder()
        .status(StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, content_type)
        .header(hyper::header::CACHE_CONTROL, "no-cache")
        .body(full(Bytes::from_static(body.as_bytes())))
        .expect("static response builder")
}

fn json_ok<T: Serialize>(value: &T) -> Response<ResBody> {
    let Ok(bytes) = serde_json::to_vec(value) else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "serialization error");
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(full(Bytes::from(bytes)))
        .expect("static response builder")
}

fn error_response(status: StatusCode, msg: &str) -> Response<ResBody> {
    let body = serde_json::to_vec(&serde_json::json!({ "error": msg })).unwrap_or_default();
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(full(Bytes::from(body)))
        .expect("static response builder")
}

/// OpenAI-shaped error body: `{"error": {"message", "type"}}`. Used by the
/// `/v1/audio/*` gateway routes so off-the-shelf OpenAI clients parse it.
fn openai_error(status: StatusCode, msg: &str) -> Response<ResBody> {
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
fn audio_response(content_type: String, bytes: Vec<u8>) -> Response<ResBody> {
    Response::builder()
        .status(StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, content_type)
        .header(hyper::header::CACHE_CONTROL, "no-store")
        .body(full(Bytes::from(bytes)))
        .expect("static response builder")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_loopback() {
        let cfg = WebSettingsConfig::default();
        assert_eq!(cfg.bind, "127.0.0.1");
        assert_eq!(cfg.port, 10_808);
        assert!(cfg.loopback_only);
        assert!(cfg.auth_token.is_none());
    }

    #[test]
    fn secret_name_validation() {
        assert!(valid_secret_name("GROQ_API_KEY"));
        assert!(valid_secret_name("OPENAI_API_KEY"));
        assert!(!valid_secret_name(""));
        assert!(!valid_secret_name("lowercase"));
        assert!(!valid_secret_name("1LEADING_DIGIT"));
        assert!(!valid_secret_name("HAS-DASH"));
        assert!(!valid_secret_name(&"X".repeat(65)));
    }

    #[test]
    fn assets_are_nonempty_and_linked() {
        assert!(INDEX_HTML.contains("app.css"));
        assert!(INDEX_HTML.contains("app.js"));
        assert!(INDEX_HTML.contains("view-doctor"));
        assert!(APP_CSS.contains("--accent"));
        assert!(APP_JS.contains("FONO_SECTIONS"));
        assert!(APP_JS.contains("/api/doctor"));
    }

    /// Every leaf key of a fully-populated `Config` must either be bound
    /// in the web UI (`app.js` references its dotted path) or appear on
    /// the explicit config-file-only allow-list below. Guards against a
    /// new config key silently never surfacing in the settings UI.
    #[test]
    fn config_coverage_ui_or_allowlist() {
        // Keys deliberately NOT exposed in the web UI. Each entry is a
        // dotted-path prefix. Keep this list justified:
        const FILE_ONLY: &[&str] = &[
            // schema bookkeeping, never user-facing
            "version",
            // power-user niche: per-app prompt suffixes
            "context_rules",
            // per-language whisper prompt map — hand-tuned, free-form keys
            "stt.prompts",
            // per-backend language override for mixed STT setups
            "stt.local.languages",
            // privacy-breaking Wyoming wake CLIENT mode stays a deliberate
            // hand edit (see WakeWyoming::CLIENT_PRIVACY_WARNING)
            "wakeword.wyoming",
            // local model plumbing (model ids / quant / ctx picked by the
            // wizard + hardware probe, not casually editable)
            "polish.local",
            "assistant.local",
            // voice mirror override for forks / self-hosting
            "tts.local.base_url",
            // discovered-palette switch; palette tooling is CLI-driven
            "tts.voice_discovery",
            // MCP per-program voice map + summarize prompt override —
            // driven by `fono mcp` / `fono voices` tooling
            "mcp.voices",
            "mcp.summarize_prompt",
            // Glass Cortex brain-keyframe capture (Phase 1 of the
            // brain-visualization plan) — gets a UI toggle when the
            // overlay style ships (plan Task 4.1)
            "overlay.brain_capture",
        ];

        // Fully populate the optional sub-tables so their leaves count.
        let mut cfg = fono_core::Config::default();
        cfg.stt.cloud = Some(fono_core::config::SttCloud {
            provider: "groq".into(),
            api_key_ref: "GROQ_API_KEY".into(),
            model: String::new(),
        });
        cfg.stt.wyoming = Some(fono_core::config::SttWyoming::default());
        cfg.stt.prompts.insert("en".into(), "x".into());
        cfg.wakeword.phrases.push(fono_core::config::WakePhrase::default());
        cfg.wakeword.wyoming = Some(fono_core::config::WakeWyoming {
            enabled: true,
            uri: Some("tcp://x:10400".into()),
        });
        cfg.tts.cloud = Some(fono_core::config::TtsCloud {
            provider: "openai".into(),
            api_key_ref: "OPENAI_API_KEY".into(),
            model: "m".into(),
        });
        cfg.tts.wyoming = Some(fono_core::config::TtsWyoming {
            uri: "tcp://x:10200".into(),
            auth_token_ref: "T".into(),
        });
        cfg.tts.local.voice = "v".into();
        cfg.tts.local.base_url = "u".into();
        cfg.tts.voice = "v".into();
        cfg.tts.output_device = "d".into();
        cfg.polish.cloud = Some(fono_core::config::PolishCloud {
            provider: "openai".into(),
            api_key_ref: "OPENAI_API_KEY".into(),
            model: "m".into(),
        });
        cfg.polish.prompt.dictionary.push("Fono".into());
        cfg.assistant.cloud = Some(fono_core::config::AssistantCloud {
            provider: "openai".into(),
            api_key_ref: "OPENAI_API_KEY".into(),
            model: "m".into(),
        });
        cfg.context_rules.push(fono_core::config::ContextRule {
            match_: fono_core::config::ContextMatch::default(),
            prompt_suffix: "s".into(),
        });
        cfg.server.wyoming.auth_token_ref = "T".into();
        cfg.server.llm.auth_token_ref = "T".into();
        cfg.server.llm.model = "m".into();
        cfg.server.web.auth_token_ref = "T".into();
        cfg.network.instance_name = "n".into();
        cfg.mcp.summarize_prompt = "p".into();
        cfg.mcp.voices.insert("app".into(), "auto".into());
        cfg.mcp.voice_gender = "any".into();
        // Bools with `skip_serializing_if` on their default value must be
        // flipped so they appear in the JSON walk at all.
        cfg.mcp.auto_assign_voices = false;
        cfg.tts.voice_discovery = false;

        let json = serde_json::to_value(&cfg).expect("config to json");
        let mut missing = Vec::new();
        walk_leaves(&json, String::new(), &mut |path| {
            let allowed =
                FILE_ONLY.iter().any(|p| path == *p || path.starts_with(&format!("{p}.")));
            if !allowed && !APP_JS.contains(path) {
                missing.push(path.to_owned());
            }
        });
        assert!(
            missing.is_empty(),
            "config keys neither bound in app.js nor on the file-only allow-list: {missing:#?}"
        );
    }

    /// Depth-first walk emitting dotted paths for every leaf. Arrays are
    /// treated as leaves (the UI binds the array itself, e.g. tag inputs
    /// and wake-phrase lists) except arrays of objects, whose element
    /// fields are walked once via index 0 with the index elided.
    fn walk_leaves(v: &serde_json::Value, prefix: String, f: &mut impl FnMut(&str)) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, val) in map {
                    let p = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                    walk_leaves(val, p, f);
                }
            }
            serde_json::Value::Array(items) => {
                if let Some(first @ serde_json::Value::Object(_)) = items.first() {
                    walk_leaves(first, prefix, f);
                } else {
                    f(&prefix);
                }
            }
            _ => f(&prefix),
        }
    }
}
