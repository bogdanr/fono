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
//! HTML/CSS/JS assets are embedded with `include_str!` — no fonts and no
//! external requests; tens of KB total. The only image is the favicon, and
//! it is an inline-text SVG (a glyph, not a bitmap).
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

/// Larger cap for the two routes that carry base64-encoded PCM audio
/// (speaker enrollment and "test my voice" calibration). A handful of
/// several-second 16 kHz mono clips base64-encodes to a few MB, which
/// overflows [`MAX_BODY_BYTES`]; audio uploads are loopback-only so the
/// wider bound is safe.
const MAX_AUDIO_BODY_BYTES: usize = 16 * 1024 * 1024;

/// How many history records a `/api/history/*` list returns when the page
/// doesn't ask for a specific `limit`.
const DEFAULT_HISTORY_LIMIT: usize = 50;
/// Upper bound on `?limit=`, so a hand-crafted URL cannot ask the daemon
/// to serialise the entire database into one response.
const MAX_HISTORY_LIMIT: usize = 500;

/// Embedded page assets for the search-first accordion layout.
pub const INDEX_HTML: &str = include_str!("assets/index.html");
pub const APP_CSS: &str = include_str!("assets/app.css");
pub const APP_JS: &str = include_str!("assets/app.js");
/// Kept byte-identical to `fono-site/favicon.svg` so the settings page and
/// the website show the same φ mark.
pub const FAVICON_SVG: &str = include_str!("assets/favicon.svg");

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

/// List all inbound API keys as JSON (metadata only — never secrets):
/// `{"keys": [{id, name, masked, created_at, expires_at, last_used_at,
/// revoked, usage_day, usage_month}, …]}`.
pub type ListApiKeysFn =
    Arc<dyn Fn() -> std::result::Result<serde_json::Value, String> + Send + Sync>;
/// Create a named key. Args: `(name, expires_at?)`. Returns
/// `{"key": {…metadata…}, "secret": "fono_sk_…"}` — the plaintext secret
/// is present **exactly once**, in this response only.
pub type CreateApiKeyFn =
    Arc<dyn Fn(&str, Option<i64>) -> std::result::Result<serde_json::Value, String> + Send + Sync>;
/// Update a key by id. The JSON body may carry any of `name` (rename),
/// `expires_at` (number or null), or `revoked` (bool). Returns the
/// updated metadata.
pub type UpdateApiKeyFn = Arc<
    dyn Fn(i64, serde_json::Value) -> std::result::Result<serde_json::Value, String> + Send + Sync,
>;
/// Permanently delete a key (and its usage counters) by id.
pub type DeleteApiKeyFn = Arc<dyn Fn(i64) -> std::result::Result<(), String> + Send + Sync>;

/// List enrolled speakers as JSON (metadata only — never voice-print
/// embeddings): `{"speakers": [{id, name, utterance_count, created_at,
/// updated_at, calibrated}, …]}`.
pub type ListSpeakersFn =
    Arc<dyn Fn() -> std::result::Result<serde_json::Value, String> + Send + Sync>;
/// Rename an enrolled speaker by id. Args `(id, new_name)`.
pub type RenameSpeakerFn = Arc<dyn Fn(i64, &str) -> std::result::Result<(), String> + Send + Sync>;
/// Delete an enrolled speaker (and all their voice prints) by id.
pub type DeleteSpeakerFn = Arc<dyn Fn(i64) -> std::result::Result<(), String> + Send + Sync>;
/// Enroll one voice sample for a speaker (create-or-append) from captured
/// audio. The JSON body carries `{name, audio_pcm16 (base64 LE i16 mono),
/// sample_rate, capture_source?}`; the daemon fetches/loads the embedding
/// model, turns the audio into a voice print, and stores it. Returns the
/// updated speaker metadata `{ok, speaker:{…}}`. Async: model fetch + the
/// `ort` embed run happen off the accept loop. Only the derived embedding is
/// persisted — the raw audio never touches disk.
pub type EnrollSpeakerFn = Arc<
    dyn Fn(serde_json::Value) -> BoxFuture<'static, std::result::Result<serde_json::Value, String>>
        + Send
        + Sync,
>;
/// Run "test my voice" calibration for a speaker (`POST
/// /api/speakers/{id}/calibrate`). The JSON body carries held-out genuine
/// clips `{clips: [{audio_pcm16 (base64 LE i16 mono), sample_rate}, …]}`; the
/// daemon embeds them, scores genuine-vs-own-centroid and impostor-vs-cohort,
/// persists the resulting calibration, and returns the score distributions,
/// EER estimate, recommended thresholds, and per-embed latency. Args
/// `(id, body)`. Async: model fetch + the `ort` embed runs happen off the
/// accept loop. No audio is persisted — only the derived calibration stats.
pub type CalibrateSpeakerFn = Arc<
    dyn Fn(
            i64,
            serde_json::Value,
        ) -> BoxFuture<'static, std::result::Result<serde_json::Value, String>>
        + Send
        + Sync,
>;
/// List a speaker's enrolled utterances with their capture-time quality
/// metrics and on-demand consistency scores, plus a suggested prune set that
/// respects the coverage floor (`GET /api/speakers/{id}/utterances`). Never
/// carries the raw voice-print embeddings. Arg `(id)`.
pub type ListUtterancesFn =
    Arc<dyn Fn(i64) -> std::result::Result<serde_json::Value, String> + Send + Sync>;
/// Delete one enrolled utterance (`DELETE /api/speakers/{id}/utterances/{uid}`).
/// Refuses to remove the speaker's last remaining clip. Args
/// `(speaker_id, utterance_id)`.
pub type DeleteUtteranceFn = Arc<dyn Fn(i64, i64) -> std::result::Result<(), String> + Send + Sync>;

/// List the discovered tool catalogue (`GET /api/tools`):
/// `{"servers": [{name, url, configured, last_seen}, …], "tools": [{source,
/// name, description, schema, enabled, available, capability, verify_class,
/// …}, …]}`, plus what the Tools &amp; actions page needs to explain them:
/// `house` (the areas, devices and kinds the servers reported), `slots`
/// (which published field carries an area, a device and a kind, or nulls for a
/// server Fono has no specific knowledge of), `hint` (the literal sentences
/// the model is given about the home), `prompt` (the whole steady head block by
/// block: `house`, `tools`, `behaviour`, plus `chars` for what this backend
/// actually reads), `grammar`, `place_names`, `catalogue_hash` and `offered`.
///
/// Reads the local store only — never contacts a server, so the page renders
/// instantly. Built by the same code that builds the prompt, so what the page
/// shows and what the model was told cannot drift apart.
pub type ListToolsFn =
    Arc<dyn Fn() -> std::result::Result<serde_json::Value, String> + Send + Sync>;
/// Allow or deny one tool (`PATCH /api/tools`). Args `(source, name,
/// enabled)`. Recorded as an explicit user choice, so it survives the tool
/// disappearing and coming back.
pub type SetToolEnabledFn =
    Arc<dyn Fn(&str, &str, bool) -> std::result::Result<(), String> + Send + Sync>;
/// Contact every configured MCP server and fold what it reports into the
/// catalogue (`POST /api/tools/discover`). Returns a per-server summary of
/// what changed. Async: network round-trips happen off the accept loop.
///
/// With a `{name, url, auth_token_ref}` body it instead *probes* that one
/// server and stores nothing, so a URL can be tested before it is saved.
pub type DiscoverToolsFn = Arc<
    dyn Fn(
            Option<serde_json::Value>,
        ) -> BoxFuture<'static, std::result::Result<serde_json::Value, String>>
        + Send
        + Sync,
>;

/// Change one of the phrases Fono has learned. `(phrase, also)`.
///
/// Two edits only, because they are the two that cannot lie: `Some(also)` adds
/// another way of saying what `phrase` already runs (`POST /api/shortcuts`), and
/// `None` forgets `phrase` outright (`DELETE /api/shortcuts?phrase=…`).
///
/// Rewriting which command a phrase runs is deliberately not offered — that
/// mapping is earned by working twice, and letting it be typed in would make the
/// earning decorative. An added phrase starts unpromoted like any other.
pub type EditShortcutFn =
    Arc<dyn Fn(&str, Option<&str>) -> std::result::Result<(), String> + Send + Sync>;

/// Probe a self-hosted OpenAI-compatible LLM server (`POST
/// /api/llm/probe`). The body carries `{url, api_key_ref?}`; the daemon
/// asks that server for its model list and returns `{ok, count, models:
/// [id, …]}`. Nothing is stored, so an address can be tested before it is
/// saved. This runs daemon-side on purpose: the server is typically on the
/// LAN, where the browser cannot reach it (or is blocked by CORS).
pub type ProbeLlmFn = Arc<
    dyn Fn(serde_json::Value) -> BoxFuture<'static, std::result::Result<serde_json::Value, String>>
        + Send
        + Sync,
>;

/// List saved dictation transcripts for the history page (`GET
/// /api/history/dictation?limit=&q=`). Args `(query, limit)`; an empty
/// query returns the most recent entries, otherwise it is a full-text
/// search. Returns `{"entries": [{id, ts, raw, cleaned, app_class,
/// app_title, stt_backend, speaker, …}, …]}`, including the verified
/// speaker when one was detected.
pub type ListDictationFn =
    Arc<dyn Fn(&str, usize) -> std::result::Result<serde_json::Value, String> + Send + Sync>;
/// List assistant conversation threads newest-first (`GET
/// /api/history/conversations?limit=`). Returns
/// `{"threads": [{id, started_at, last_at, ended, turn_count, preview,
/// speakers: [name, …], backend, model}, …]}` — enough to render the list
/// without loading every turn.
pub type ListThreadsFn =
    Arc<dyn Fn(usize) -> std::result::Result<serde_json::Value, String> + Send + Sync>;
/// Load one thread's turns in order (`GET
/// /api/history/conversations/{id}`). Returns `{"turns": [{ordinal, role,
/// text, ts, speaker, latency_ms, partial}, …]}`, where `role` is one of
/// `user` / `assistant` / `tool_call` / `tool_result`.
pub type GetThreadFn =
    Arc<dyn Fn(i64) -> std::result::Result<serde_json::Value, String> + Send + Sync>;
/// Delete history the user no longer wants kept (`DELETE
/// /api/history/dictation[/{id}]`, `DELETE
/// /api/history/conversations[/{id}]`). Args `(kind, id)` where `kind` is
/// `"dictation"` or `"conversations"` and `id` is `None` for "clear all".
/// Returns the number of records removed.
pub type DeleteHistoryFn =
    Arc<dyn Fn(&str, Option<i64>) -> std::result::Result<usize, String> + Send + Sync>;

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
    /// Inbound API-key management (the Groq-style "API Keys" table).
    pub list_api_keys: ListApiKeysFn,
    pub create_api_key: CreateApiKeyFn,
    pub update_api_key: UpdateApiKeyFn,
    pub delete_api_key: DeleteApiKeyFn,
    /// Enrolled-speaker management (local voice biometrics). Metadata
    /// verbs plus audio enrollment; verification runs live in the daemon
    /// pipeline. These never move voice-print embeddings over the wire.
    pub list_speakers: ListSpeakersFn,
    pub rename_speaker: RenameSpeakerFn,
    pub delete_speaker: DeleteSpeakerFn,
    /// Enroll a voice sample from captured audio (`POST /api/speakers`).
    pub enroll_speaker: EnrollSpeakerFn,
    /// Run "test my voice" calibration (`POST /api/speakers/{id}/calibrate`).
    pub calibrate_speaker: CalibrateSpeakerFn,
    /// List a speaker's utterances + quality/consistency + prune suggestion
    /// (`GET /api/speakers/{id}/utterances`).
    pub list_utterances: ListUtterancesFn,
    /// Delete one utterance (`DELETE /api/speakers/{id}/utterances/{uid}`).
    pub delete_utterance: DeleteUtteranceFn,
    /// Voice-triggered actions: the discovered tool catalogue and the
    /// user's allow/deny choices about it.
    pub list_tools: ListToolsFn,
    pub set_tool_enabled: SetToolEnabledFn,
    pub discover_tools: DiscoverToolsFn,
    /// Add another way of saying a learned phrase, or forget one.
    pub edit_shortcut: EditShortcutFn,
    /// Test a self-hosted OpenAI-compatible LLM endpoint and list its
    /// models (`POST /api/llm/probe`).
    pub probe_llm: ProbeLlmFn,
    /// Browse what Fono has saved: dictation transcripts and assistant
    /// conversations, both with the detected speaker where known.
    pub list_dictation: ListDictationFn,
    pub list_threads: ListThreadsFn,
    pub get_thread: GetThreadFn,
    pub delete_history: DeleteHistoryFn,
}

/// Configuration for [`WebSettingsServer::start`]. Built from
/// `[server.web]` at the daemon layer; tests construct it directly.
#[derive(Debug, Clone)]
pub struct WebSettingsConfig {
    /// Bind host. `127.0.0.1` is the safe default.
    pub bind: String,
    /// TCP port. Default [`DEFAULT_PORT`] (`10808`).
    pub port: u16,
    /// Require a valid inbound API key for non-loopback access. When
    /// `true`, non-loopback `/api/*` and `/v1/audio/*` requests must carry
    /// `Authorization: Bearer <key>` (or `?token=<key>`) matching an entry
    /// in the API-key store. Loopback callers are always trusted so the
    /// first key can be created from the local browser without a bootstrap
    /// lockout. Static assets are always served without auth.
    pub auth_enabled: bool,
    /// When `true`, refuses non-loopback peers even if the bind address
    /// would have allowed them. Set when `bind = "127.0.0.1"`.
    pub loopback_only: bool,
}

impl Default for WebSettingsConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1".to_string(),
            port: DEFAULT_PORT,
            auth_enabled: true,
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
    verifier: Option<crate::auth::AuthVerifier>,
    usage: Option<crate::auth::UsageSink>,
}

/// The server itself. Stateless beyond the config + hook closures.
pub struct WebSettingsServer {
    cfg: WebSettingsConfig,
    hooks: WebSettingsHooks,
    verifier: Option<crate::auth::AuthVerifier>,
    usage: Option<crate::auth::UsageSink>,
}

impl WebSettingsServer {
    /// Build a server. Does not bind yet — call [`Self::start`].
    #[must_use]
    pub fn new(cfg: WebSettingsConfig, hooks: WebSettingsHooks) -> Self {
        Self { cfg, hooks, verifier: None, usage: None }
    }

    /// Attach the inbound-auth verifier and usage sink. Required for
    /// `auth_enabled = true` to admit any non-loopback caller.
    #[must_use]
    pub fn with_auth(
        mut self,
        verifier: crate::auth::AuthVerifier,
        usage: crate::auth::UsageSink,
    ) -> Self {
        self.verifier = Some(verifier);
        self.usage = Some(usage);
        self
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
        let ctx = ServerCtx {
            cfg: Arc::new(self.cfg),
            hooks: self.hooks,
            verifier: self.verifier,
            usage: self.usage,
        };
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
                                tokio::spawn(serve_conn(sock, peer, ctx.clone()));
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

async fn serve_conn(sock: TcpStream, peer: SocketAddr, ctx: ServerCtx) {
    let io = TokioIo::new(sock);
    let service = service_fn(move |req: Request<Incoming>| {
        let ctx = ctx.clone();
        async move { Ok::<_, Infallible>(route(req, peer, ctx).await) }
    });
    if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
        tracing::debug!(target: "fono::web::server", "connection ended: {e:#}");
    }
}

/// Serve one of the embedded page assets, or `None` if the request is not
/// for a static asset. Kept separate from [`route`] so the state-bearing
/// dispatch below stays readable.
fn static_asset(method: &Method, path: &str) -> Option<Response<ResBody>> {
    match (method, path) {
        (&Method::GET | &Method::HEAD, "/" | "/index.html") => {
            Some(asset(INDEX_HTML, "text/html; charset=utf-8"))
        }
        (&Method::GET, "/app.css") => Some(asset(APP_CSS, "text/css; charset=utf-8")),
        (&Method::GET, "/app.js") => Some(asset(APP_JS, "text/javascript; charset=utf-8")),
        (&Method::GET | &Method::HEAD, "/favicon.svg" | "/favicon.ico") => {
            Some(asset(FAVICON_SVG, "image/svg+xml"))
        }
        _ => None,
    }
}

/// Decide whether one request may proceed, or the response that turns it
/// away. Kept separate from [`route`] so the dispatch below reads as a table
/// of paths.
///
/// When auth is on, a presented token is always verified — even from loopback
/// — so a wrong `?token=`/bearer is rejected and a valid key's id is recorded
/// against its usage counters. Loopback with *no* token is trusted so the
/// first key can be created locally (bootstrap).
fn admit(req: &Request<Incoming>, peer: SocketAddr, ctx: &ServerCtx) -> Option<Response<ResBody>> {
    if !ctx.cfg.auth_enabled {
        return None;
    }
    let presented = presented_token(req);
    match crate::auth::decide(
        ctx.cfg.auth_enabled,
        is_loopback(&peer),
        presented.as_deref(),
        ctx.verifier.as_ref(),
    ) {
        crate::auth::AuthDecision::Allow(key_id) => {
            if let (Some(id), Some(sink)) = (key_id, ctx.usage.as_ref()) {
                sink(id);
            }
            None
        }
        crate::auth::AuthDecision::Deny => {
            Some(error_response(StatusCode::UNAUTHORIZED, "missing or invalid API key"))
        }
    }
}

/// Dispatch the routes that read and write stored settings — the config
/// document, the vocabulary list, and one named secret — or `None` if the
/// request is for something else.
async fn route_settings(
    method: &Method,
    path: &str,
    req: Request<Incoming>,
    ctx: &ServerCtx,
) -> Option<Response<ResBody>> {
    Some(match (method, path) {
        (&Method::GET, "/api/config") => match (ctx.hooks.get_config)() {
            Ok(v) => json_ok(&v),
            Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e),
        },
        (&Method::PUT, "/api/config") => {
            let Some(body) = read_json_body(req).await else {
                return Some(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid or oversized JSON body",
                ));
            };
            match (ctx.hooks.put_config)(body).await {
                Ok(summary) => json_ok(&serde_json::json!({ "ok": true, "summary": summary })),
                Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e),
            }
        }
        (&Method::GET, "/api/vocabulary") => match (ctx.hooks.get_vocabulary)() {
            Ok(v) => json_ok(&v),
            Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e),
        },
        (&Method::PUT, "/api/vocabulary") => {
            let Some(body) = read_json_body(req).await else {
                return Some(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid or oversized JSON body",
                ));
            };
            match (ctx.hooks.put_vocabulary)(body) {
                Ok(summary) => json_ok(&serde_json::json!({ "ok": true, "summary": summary })),
                Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e),
            }
        }
        (&Method::PUT, p) if p.starts_with("/api/secret/") => {
            let name = p.trim_start_matches("/api/secret/").to_owned();
            if !valid_secret_name(&name) {
                return Some(error_response(StatusCode::BAD_REQUEST, "invalid secret name"));
            }
            let Some(body) = read_json_body(req).await else {
                return Some(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid or oversized JSON body",
                ));
            };
            let Some(value) = body.get("value").and_then(|v| v.as_str()) else {
                return Some(error_response(
                    StatusCode::BAD_REQUEST,
                    "body must be {\"value\": \"…\"}",
                ));
            };
            match (ctx.hooks.set_secret)(&name, value) {
                Ok(()) => json_ok(&serde_json::json!({ "ok": true })),
                Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e),
            }
        }
        _ => return None,
    })
}

/// Dispatch one request. The service layer never fails; every path
/// returns a `Response` (including error responses).
async fn route(req: Request<Incoming>, peer: SocketAddr, ctx: ServerCtx) -> Response<ResBody> {
    let path = req.uri().path().to_owned();
    let method = req.method().clone();

    // Static assets — no auth (no state, no secrets).
    if let Some(res) = static_asset(&method, path.as_str()) {
        return res;
    }

    // Everything else is state-bearing (JSON API + `/v1/audio/*`).
    if let Some(res) = admit(&req, peer, &ctx) {
        return res;
    }
    match (&method, path.as_str()) {
        (&Method::GET, "/api/meta") => json_ok(&(ctx.hooks.meta)()),
        (m, p) if p == "/api/apikeys" || p.starts_with("/api/apikeys/") => {
            route_api_keys(m, p, req, &ctx).await
        }
        (m, p) if p == "/api/speakers" || p.starts_with("/api/speakers/") => {
            route_speakers(m, p, req, &ctx).await
        }
        (m, p) if p == "/api/tools" || p == "/api/tools/discover" || p == "/api/shortcuts" => {
            route_tools(m, p, req, &ctx).await
        }
        (m, p) if p.starts_with("/api/history/") => route_history(m, p, req.uri().query(), &ctx),
        (&Method::POST, "/api/llm/probe") => route_llm_probe(req, &ctx).await,
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
        (m, p) => route_settings(m, p, req, &ctx)
            .await
            .unwrap_or_else(|| error_response(StatusCode::NOT_FOUND, "not found")),
    }
}

/// Dispatch the inbound API-key management routes (`/api/apikeys[/id]`).
/// Split out of [`route`] to keep it under clippy's `too_many_lines`.
async fn route_api_keys(
    method: &Method,
    path: &str,
    req: Request<Incoming>,
    ctx: &ServerCtx,
) -> Response<ResBody> {
    match (method, path) {
        (&Method::GET, "/api/apikeys") => match (ctx.hooks.list_api_keys)() {
            Ok(v) => json_ok(&v),
            Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e),
        },
        (&Method::POST, "/api/apikeys") => {
            let Some(body) = read_json_body(req).await else {
                return error_response(StatusCode::BAD_REQUEST, "invalid or oversized JSON body");
            };
            let Some(name) = body.get("name").and_then(|v| v.as_str()) else {
                return error_response(StatusCode::BAD_REQUEST, "body must include a \"name\"");
            };
            let expires_at = body.get("expires_at").and_then(serde_json::Value::as_i64);
            match (ctx.hooks.create_api_key)(name, expires_at) {
                Ok(v) => json_ok(&v),
                Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e),
            }
        }
        (&Method::PATCH, p) if p.starts_with("/api/apikeys/") => {
            let Some(id) = p.trim_start_matches("/api/apikeys/").parse::<i64>().ok() else {
                return error_response(StatusCode::BAD_REQUEST, "invalid API key id");
            };
            let Some(body) = read_json_body(req).await else {
                return error_response(StatusCode::BAD_REQUEST, "invalid or oversized JSON body");
            };
            match (ctx.hooks.update_api_key)(id, body) {
                Ok(v) => json_ok(&v),
                Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e),
            }
        }
        (&Method::DELETE, p) if p.starts_with("/api/apikeys/") => {
            let Some(id) = p.trim_start_matches("/api/apikeys/").parse::<i64>().ok() else {
                return error_response(StatusCode::BAD_REQUEST, "invalid API key id");
            };
            match (ctx.hooks.delete_api_key)(id) {
                Ok(()) => json_ok(&serde_json::json!({ "ok": true })),
                Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e),
            }
        }
        _ => error_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
    }
}

/// `/api/history/*` — read back what Fono has saved: dictation
/// transcripts and assistant conversations. Read-only apart from the
/// delete verbs, and entirely local: both stores are on this machine.
///
/// Synchronous by design — SQLite reads of a page's worth of rows are
/// sub-millisecond, so there is nothing to move off the accept loop.
fn route_history(
    method: &Method,
    path: &str,
    query: Option<&str>,
    ctx: &ServerCtx,
) -> Response<ResBody> {
    let limit = query_param(query, "limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_HISTORY_LIMIT)
        .clamp(1, MAX_HISTORY_LIMIT);
    match (method, path) {
        (&Method::GET, "/api/history/dictation") => {
            let q = query_param(query, "q").unwrap_or_default();
            match (ctx.hooks.list_dictation)(&q, limit) {
                Ok(v) => json_ok(&v),
                Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e),
            }
        }
        (&Method::GET, "/api/history/conversations") => match (ctx.hooks.list_threads)(limit) {
            Ok(v) => json_ok(&v),
            Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e),
        },
        (&Method::GET, p) if p.starts_with("/api/history/conversations/") => {
            let Ok(id) = p.trim_start_matches("/api/history/conversations/").parse::<i64>() else {
                return error_response(StatusCode::BAD_REQUEST, "invalid conversation id");
            };
            match (ctx.hooks.get_thread)(id) {
                Ok(v) => json_ok(&v),
                Err(e) => error_response(StatusCode::NOT_FOUND, &e),
            }
        }
        (&Method::DELETE, p) => {
            let rest = p.trim_start_matches("/api/history/");
            let (kind, id) = match rest.split_once('/') {
                Some((kind, id_str)) => {
                    let Ok(id) = id_str.parse::<i64>() else {
                        return error_response(StatusCode::BAD_REQUEST, "invalid history id");
                    };
                    (kind, Some(id))
                }
                None => (rest, None),
            };
            if !matches!(kind, "dictation" | "conversations") {
                return error_response(StatusCode::NOT_FOUND, "not found");
            }
            match (ctx.hooks.delete_history)(kind, id) {
                Ok(n) => json_ok(&serde_json::json!({ "ok": true, "deleted": n })),
                Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e),
            }
        }
        _ => error_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
    }
}

/// Pull one percent-decoded parameter out of a raw query string.
fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    query?.split('&').find_map(|kv| kv.strip_prefix(&prefix)).map(percent_decode)
}

/// Minimal `application/x-www-form-urlencoded` decoding for query values:
/// `+` is a space and `%XX` is a byte. Enough for a search box, and it
/// keeps a URL-decoding crate out of the binary.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    out.push(b);
                    i += 3;
                } else {
                    // Not a valid escape — keep the literal `%`.
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `POST /api/llm/probe` — "Test connection" for a self-hosted LLM
/// server. The daemon does the fetching because the endpoint is usually
/// a LAN address the browser cannot reach. A failure here means the
/// user's server did not answer, hence `502` rather than a 4xx.
async fn route_llm_probe(req: Request<Incoming>, ctx: &ServerCtx) -> Response<ResBody> {
    let Some(body) = read_json_body(req).await else {
        return error_response(StatusCode::BAD_REQUEST, "invalid or oversized JSON body");
    };
    match (ctx.hooks.probe_llm)(body).await {
        Ok(v) => json_ok(&v),
        Err(e) => error_response(StatusCode::BAD_GATEWAY, &e),
    }
}

/// `/api/tools*` and `/api/shortcuts` — the discovered tool catalogue the
/// assistant may use, and the phrases Fono has learned to run without it.
///
/// `GET` and `PATCH` read and write the local store only, so the page
/// renders and toggles instantly. Only `POST /api/tools/discover` talks to
/// the configured servers.
async fn route_tools(
    method: &Method,
    path: &str,
    req: Request<Incoming>,
    ctx: &ServerCtx,
) -> Response<ResBody> {
    match (method, path) {
        (&Method::GET, "/api/tools") => match (ctx.hooks.list_tools)() {
            Ok(v) => json_ok(&v),
            Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e),
        },
        (&Method::PATCH, "/api/tools") => {
            let Some(body) = read_json_body(req).await else {
                return error_response(StatusCode::BAD_REQUEST, "invalid or oversized JSON body");
            };
            let source = body.get("source").and_then(|v| v.as_str()).unwrap_or_default();
            let name = body.get("name").and_then(|v| v.as_str()).unwrap_or_default();
            let Some(enabled) = body.get("enabled").and_then(serde_json::Value::as_bool) else {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "body must be {\"source\": \"…\", \"name\": \"…\", \"enabled\": true|false}",
                );
            };
            match (ctx.hooks.set_tool_enabled)(source, name, enabled) {
                Ok(()) => json_ok(&serde_json::json!({ "ok": true })),
                Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e),
            }
        }
        (&Method::POST, "/api/tools/discover") => {
            // An empty body means "refresh everything saved"; a body names a
            // single, possibly unsaved, server to try.
            let probe = read_json_body(req).await.filter(|v| v.get("url").is_some());
            match (ctx.hooks.discover_tools)(probe).await {
                Ok(v) => json_ok(&v),
                Err(e) => error_response(StatusCode::BAD_GATEWAY, &e),
            }
        }
        (&Method::POST, "/api/shortcuts") => {
            let Some(body) = read_json_body(req).await else {
                return error_response(StatusCode::BAD_REQUEST, "invalid or oversized JSON body");
            };
            let like = body.get("like").and_then(|v| v.as_str()).unwrap_or_default();
            let Some(phrase) = body.get("phrase").and_then(|v| v.as_str()) else {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "body must be {\"like\": \"…\", \"phrase\": \"…\"}",
                );
            };
            match (ctx.hooks.edit_shortcut)(like, Some(phrase)) {
                Ok(()) => json_ok(&serde_json::json!({ "ok": true })),
                Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e),
            }
        }
        // A delete rather than a switch: the user saying they do not want a
        // phrase is not the same as the world changing under it, which the row
        // already reports on its own.
        (&Method::DELETE, "/api/shortcuts") => {
            let Some(phrase) = query_param(req.uri().query(), "phrase") else {
                return error_response(StatusCode::BAD_REQUEST, "?phrase=… is required");
            };
            match (ctx.hooks.edit_shortcut)(&phrase, None) {
                Ok(()) => json_ok(&serde_json::json!({ "ok": true })),
                Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e),
            }
        }
        _ => error_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
    }
}

/// Enrolled-speaker endpoints. `GET` lists metadata, `POST` enrolls a voice
/// sample from captured audio, `PATCH` renames, `DELETE` removes. Voice-print
/// embeddings never cross this boundary — only names and counts leave, and
/// only audio (never stored) enters.
async fn route_speakers(
    method: &Method,
    path: &str,
    req: Request<Incoming>,
    ctx: &ServerCtx,
) -> Response<ResBody> {
    match (method, path) {
        (&Method::GET, "/api/speakers") => match (ctx.hooks.list_speakers)() {
            Ok(v) => json_ok(&v),
            Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e),
        },
        (&Method::POST, "/api/speakers") => {
            let Some(body) = read_json_body_limited(req, MAX_AUDIO_BODY_BYTES).await else {
                return error_response(StatusCode::BAD_REQUEST, "invalid or oversized JSON body");
            };
            match (ctx.hooks.enroll_speaker)(body).await {
                Ok(v) => json_ok(&v),
                Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e),
            }
        }
        (&Method::POST, p) if p.starts_with("/api/speakers/") && p.ends_with("/calibrate") => {
            let id_str = p.trim_start_matches("/api/speakers/").trim_end_matches("/calibrate");
            let Some(id) = id_str.parse::<i64>().ok() else {
                return error_response(StatusCode::BAD_REQUEST, "invalid speaker id");
            };
            let Some(body) = read_json_body_limited(req, MAX_AUDIO_BODY_BYTES).await else {
                return error_response(StatusCode::BAD_REQUEST, "invalid or oversized JSON body");
            };
            match (ctx.hooks.calibrate_speaker)(id, body).await {
                Ok(v) => json_ok(&v),
                Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e),
            }
        }
        (&Method::GET, p) if p.starts_with("/api/speakers/") && p.ends_with("/utterances") => {
            let id_str = p.trim_start_matches("/api/speakers/").trim_end_matches("/utterances");
            let Some(id) = id_str.parse::<i64>().ok() else {
                return error_response(StatusCode::BAD_REQUEST, "invalid speaker id");
            };
            match (ctx.hooks.list_utterances)(id) {
                Ok(v) => json_ok(&v),
                Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e),
            }
        }
        (&Method::DELETE, p) if p.contains("/utterances/") && p.starts_with("/api/speakers/") => {
            let rest = p.trim_start_matches("/api/speakers/");
            let Some((sid_str, uid_str)) = rest.split_once("/utterances/") else {
                return error_response(StatusCode::BAD_REQUEST, "invalid utterance path");
            };
            let (Ok(sid), Ok(uid)) = (sid_str.parse::<i64>(), uid_str.parse::<i64>()) else {
                return error_response(StatusCode::BAD_REQUEST, "invalid speaker or utterance id");
            };
            match (ctx.hooks.delete_utterance)(sid, uid) {
                Ok(()) => json_ok(&serde_json::json!({ "ok": true })),
                Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e),
            }
        }
        (&Method::PATCH, p) if p.starts_with("/api/speakers/") => {
            let Some(id) = p.trim_start_matches("/api/speakers/").parse::<i64>().ok() else {
                return error_response(StatusCode::BAD_REQUEST, "invalid speaker id");
            };
            let Some(body) = read_json_body(req).await else {
                return error_response(StatusCode::BAD_REQUEST, "invalid or oversized JSON body");
            };
            let Some(name) = body.get("name").and_then(|v| v.as_str()) else {
                return error_response(StatusCode::BAD_REQUEST, "body must include a \"name\"");
            };
            match (ctx.hooks.rename_speaker)(id, name) {
                Ok(()) => json_ok(&serde_json::json!({ "ok": true })),
                Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e),
            }
        }
        (&Method::DELETE, p) if p.starts_with("/api/speakers/") => {
            let Some(id) = p.trim_start_matches("/api/speakers/").parse::<i64>().ok() else {
                return error_response(StatusCode::BAD_REQUEST, "invalid speaker id");
            };
            match (ctx.hooks.delete_speaker)(id) {
                Ok(()) => json_ok(&serde_json::json!({ "ok": true })),
                Err(e) => error_response(StatusCode::UNPROCESSABLE_ENTITY, &e),
            }
        }
        _ => error_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
    }
}

/// Secret names are env-var shaped: `[A-Z][A-Z0-9_]*`, sane length.
fn valid_secret_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Bearer header or `?token=` query parameter, whichever is present.
fn presented_token(req: &Request<Incoming>) -> Option<String> {
    if let Some(tok) = req
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
    {
        return Some(tok.to_owned());
    }
    req.uri()
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=").map(str::to_owned)))
}

async fn read_json_body(req: Request<Incoming>) -> Option<serde_json::Value> {
    read_json_body_limited(req, MAX_BODY_BYTES).await
}

async fn read_json_body_limited(req: Request<Incoming>, max: usize) -> Option<serde_json::Value> {
    let limited = Limited::new(req.into_body(), max);
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
        assert!(cfg.auth_enabled);
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
        assert!(INDEX_HTML.contains("favicon.svg"));
        assert!(FAVICON_SVG.contains("<svg"));
    }

    /// The tools & actions page is a route of its own, reachable from the
    /// header and from the settings section that used to hold the list. All
    /// three have to exist together: a shell with no view renders blank, and a
    /// view nothing links to cannot be found.
    #[test]
    fn the_tools_and_actions_page_is_reachable() {
        assert!(INDEX_HTML.contains("view-actions"), "the page needs somewhere to render");
        assert!(INDEX_HTML.contains("href=\"#/actions\""), "and a way in from the header");
        assert!(APP_JS.contains("'#/actions'"), "and a route that recognises it");
        assert!(APP_JS.contains("href=\"#/actions\""), "and a link from the settings summary");
        // The panels that make it a debugging instrument rather than a list:
        // what a tool was really asked to do, what each field is narrowed to,
        // the server's own words, and the words the model is given.
        assert!(APP_JS.contains("class=\"uses\""), "the commands that actually reached a tool");
        assert!(APP_JS.contains("held to "), "which fields Fono is narrowing");
        assert!(APP_JS.contains("What the server published, word for word"));
        assert!(APP_JS.contains("The exact words the assistant is given"));
        // And all three blocks of them. The panel showed the house block alone
        // under that heading, which was less than half of what is sent.
        for block in ["Your home", "What it can do", "How to answer"] {
            assert!(APP_JS.contains(block), "the prompt panel must show {block}");
        }
    }

    /// A saved conversation has to answer the same question the tools page
    /// answers — which command worked — so it renders the same block, wearing
    /// the same three verdict states. It opens on conversations because a
    /// dictation transcript is a line the user already watched being typed.
    #[test]
    fn a_saved_conversation_shows_which_commands_worked() {
        assert!(APP_JS.contains("histTab = 'conversations'"), "conversations open first");
        assert!(APP_JS.contains("renderCommandTurn"), "a call and its reply are one block");
        // The verdict comes off the stored flag, never off the reply text: a
        // Home Assistant call that worked comes back saying `"failed": []`.
        assert!(APP_JS.contains("typeof res.ok === 'boolean'"), "three states, not two");
        assert!(APP_CSS.contains(".uses .use.bad"), "and the failing one is coloured");
    }

    /// The settings page auto-picks a backend when a language-model role
    /// is switched on with none chosen, and must reach the same verdict
    /// the daemon would. That means duplicating `LLM_AUTOSELECT_ORDER`
    /// in JavaScript — so assert the two copies still agree, in order.
    /// If they drift, the page proposes one provider and the daemon
    /// silently runs another.
    #[test]
    fn js_autoselect_order_matches_the_rust_one() {
        let line = APP_JS
            .lines()
            .find(|l| l.starts_with("const LLM_AUTOSELECT_ORDER"))
            .expect("app.js must declare LLM_AUTOSELECT_ORDER on one line");
        let js: Vec<&str> = line
            .split_once('[')
            .and_then(|(_, r)| r.split_once(']'))
            .expect("malformed array literal")
            .0
            .split(',')
            .map(|s| s.trim().trim_matches('\''))
            .collect();
        let rust: Vec<&str> = fono_core::providers::llm_autoselect_order()
            .iter()
            .map(fono_core::providers::llm_backend_str)
            .collect();
        assert_eq!(js, rust, "app.js auto-select order has drifted from fono-core");
    }

    /// Every rendered `<button>` must be reachable by a click handler.
    ///
    /// Most of the settings page routes clicks through one delegated
    /// `closest(...)` call with an explicit attribute list; views that
    /// re-render wholesale (the history page) instead bind their buttons
    /// directly with `querySelectorAll('[data-x]')` after each render.
    /// Either wiring counts — what must never happen is a button that
    /// renders perfectly and silently does nothing, with no error and
    /// nothing in the console. That exact bug shipped once; this is the
    /// guard.
    #[test]
    fn every_button_is_reachable_by_the_click_handler() {
        let selector = APP_JS
            .lines()
            .find(|l| l.contains("e.target.closest('[data-"))
            .expect("delegated click listener not found");

        // Attribute names are lower-kebab in the markup; the listener lists
        // them verbatim, so a plain substring check is enough.
        let attrs_near = |s: &str| -> Vec<String> {
            let mut out = Vec::new();
            let bytes = s.as_bytes();
            let mut i = 0;
            while let Some(p) = s[i..].find("data-") {
                let start = i + p;
                let end = bytes[start..]
                    .iter()
                    .position(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-'))
                    .map_or(s.len(), |n| start + n);
                out.push(s[start..end].to_owned());
                i = end;
            }
            out
        };

        // An attribute is handled if the delegated listener selects on it,
        // or if some view binds it directly after rendering.
        let handled = |a: &str| {
            selector.contains(&format!("[{a}]"))
                || APP_JS.contains(&format!("querySelectorAll('[{a}]')"))
        };

        for (at, _) in APP_JS.match_indices("<button") {
            let window = &APP_JS[at..(at + 220).min(APP_JS.len())];
            // Stop at the end of the opening tag where we can find one, so a
            // later button's attributes cannot rescue this one.
            let head = window.split_once('>').map_or(window, |(h, _)| h);
            // `keyIconBtn` builds its attribute name from an argument
            // (`data-' + action`), so there is nothing static to check.
            // Buttons carrying an `id` are wired by direct `addEventListener`
            // instead of delegation, and are checked below.
            if head.contains("data-'") || head.contains("id=\"") {
                continue;
            }
            let found = attrs_near(head);
            assert!(
                found.iter().any(|a| handled(a)),
                "button at byte {at} has no click-handled attribute (saw {found:?})"
            );
        }
    }

    /// The key that identifies a row must never be written into the document.
    ///
    /// It joins the server and the tool name with a NUL, and an HTML attribute
    /// cannot carry one: the parser rewrites U+0000 to U+FFFD, so the key read
    /// back off a click never equals the key the renderer looks up and no row
    /// ever opens. Every automated check passed while the page was in exactly
    /// that state — the buttons existed, the handler was bound, the classes were
    /// styled, and the one thing the page is for did not work. This asserts the
    /// invariant that was actually broken: the separator stays in JavaScript.
    #[test]
    fn the_row_key_never_reaches_the_html() {
        // The quoted literal, so the comment describing the key does not count
        // as a second use of it.
        let sep = APP_JS.match_indices("'\\u0000'").count();
        assert!(sep > 0, "the scan has stopped seeing the key builder");
        assert_eq!(
            sep, 1,
            "the NUL row-key separator appears more than once — it belongs only in `actKey`, \
             never in emitted markup, because the HTML parser silently corrupts it"
        );
        assert!(
            !APP_JS.contains("esc(k)"),
            "a composite row key is being escaped into an attribute; pass its parts as \
             separate `data-` attributes and rebuild the key in JavaScript"
        );
    }

    /// Every class the tools & actions page paints on itself must exist in
    /// the stylesheet.
    ///
    /// The page carries a lot of meaning in colour alone — a failed run, a
    /// tool switched off, a value Fono is holding to your home. A class name
    /// that has drifted does not break anything loudly; it just quietly stops
    /// saying the thing, on the one page whose whole job is to say things
    /// plainly. Scoped to the `act-*` family and the pill variants, which is
    /// where that meaning lives.
    #[test]
    fn every_actions_page_class_is_styled() {
        let mut wanted: Vec<String> = Vec::new();
        for prefix in ["act-", "chip-"] {
            for (at, _) in APP_JS.match_indices(prefix) {
                // `data-act-*` attributes are behaviour, not style, and are
                // covered by the click-handler test above.
                if at >= 5 && &APP_JS[at - 5..at] == "data-" {
                    continue;
                }
                let rest = &APP_JS[at..];
                let end = rest
                    .find(|c: char| !(c.is_ascii_lowercase() || c == '-'))
                    .unwrap_or(rest.len());
                let name = rest[..end].trim_end_matches('-');
                if name.len() > 4 {
                    wanted.push(name.to_owned());
                }
            }
        }
        // Pill variants, read off the `pill(label, kind, …)` call sites rather
        // than by looking for the bare word anywhere in the file — `'strong'`
        // and `'warn'` are also used by the calibration and doctor views for
        // something else entirely.
        for (at, _) in APP_JS.match_indices("pill('") {
            let rest = &APP_JS[at + "pill('".len()..];
            // Past the label, then the kind, when both are plain literals.
            let Some(label_end) = rest.find('\'') else { continue };
            let Some(kind) = rest[label_end + 1..].strip_prefix(", '") else { continue };
            let Some(end) = kind.find('\'') else { continue };
            if !kind[..end].is_empty() {
                wanted.push(format!("pill.{}", &kind[..end]));
            }
        }
        wanted.sort();
        wanted.dedup();
        // The parser going blind is the failure mode that would make this test
        // pass forever while checking nothing — the exact shape of an earlier
        // defect where a harness reported zero of something it could not see.
        assert!(wanted.len() > 10, "found only {wanted:?} — the scan has stopped seeing the page");

        // Boundary-aware: `.act-ran` must not be satisfied by `.act-rans`.
        let styled = |c: &str| {
            APP_CSS.match_indices(&format!(".{c}")).any(|(at, m)| {
                APP_CSS[at + m.len()..]
                    .chars()
                    .next()
                    .is_none_or(|n| !(n.is_ascii_alphanumeric() || n == '-' || n == '_'))
            })
        };
        for c in wanted {
            assert!(
                styled(&c),
                "app.js paints `{c}` on the actions page but app.css never styles it"
            );
        }
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
            // local model plumbing. The model *id* is now a dropdown in
            // the Cleanup / Assistant "Local" panel, so it is bound; these
            // two are picked by the wizard + hardware probe instead.
            "polish.local.quantization",
            "polish.local.context",
            "assistant.local.quantization",
            "assistant.local.context",
            // voice mirror override for forks / self-hosting
            "tts.local.base_url",
            // discovered-palette switch; palette tooling is CLI-driven
            "tts.voice_discovery",
            // MCP per-program voice map + summarize prompt override —
            // driven by `fono mcp` / `fono voices` tooling
            "mcp.voices",
            "mcp.summarize_prompt",
            // Glass Cortex brain-keyframe capture — gets a UI toggle
            // once the overlay style ships
            "overlay.brain_capture",
            // Telling the model the real area names is what makes a command
            // in any language hit the right area, so it is on and stays on.
            // A hand edit exists only to rule it out when diagnosing.
            "assistant.tools.place_names",
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
        cfg.polish.cloud =
            fono_core::config::LlmCloud { api_key_ref: "OPENAI_API_KEY".into(), model: "m".into() };
        cfg.polish.network = fono_core::config::LlmNetwork {
            url: "http://localhost:11434/v1/chat/completions".into(),
            model: "m".into(),
            api_key_ref: "T".into(),
        };
        cfg.polish.prompt.dictionary.push("Fono".into());
        cfg.assistant.cloud =
            fono_core::config::LlmCloud { api_key_ref: "OPENAI_API_KEY".into(), model: "m".into() };
        cfg.assistant.network = fono_core::config::LlmNetwork {
            url: "http://localhost:11434/v1/chat/completions".into(),
            model: "m".into(),
            api_key_ref: "T".into(),
        };
        cfg.context_rules.push(fono_core::config::ContextRule {
            match_: fono_core::config::ContextMatch::default(),
            prompt_suffix: "s".into(),
        });
        cfg.server.wyoming.auth_token_ref = "T".into();
        cfg.server.llm.model = "m".into();
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
