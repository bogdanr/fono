// SPDX-License-Identifier: GPL-3.0-only
//! Minimal MCP **client** — enough to discover what a tool server offers.
//!
//! Distinct from `fono-mcp-server`, which exposes *Fono* to coding agents.
//! This is the other direction: Fono asking a server (Home Assistant's
//! `mcp_server` integration, and anything else speaking MCP) "what tools do
//! you have?" so the catalogue store can record them.
//!
//! ## Transports
//!
//! * **SSE**, for a URL ending in `/sse` (Home Assistant's
//!   `…/mcp_server/sse`): `GET` opens an event stream whose first event
//!   carries a POST endpoint, JSON-RPC requests go to that endpoint, and the
//!   *responses arrive back on the stream*.
//! * **Streamable HTTP**, for any other URL (Home Assistant's `…/api/mcp`):
//!   each JSON-RPC request is a POST whose response carries the reply, as
//!   JSON or as a short event stream. A Home Assistant app can only reach
//!   this one, because the Supervisor proxies `/api/…` and nothing else.
//!
//! Discovery is a short-lived, one-shot exchange — open, `initialize`,
//! `tools/list`, drop the stream. Nothing here holds a session open.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use fono_core::tool_catalog::Device;
use futures::StreamExt;
use serde_json::{json, Value};

use crate::sse::SseBuffer;

/// Who answered, for the UI and the logs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub protocol_version: String,
}

/// One tool exactly as the server advertised it. No interpretation happens
/// here — classification is the catalogue's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTool {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// The result of one discovery pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    pub server: ServerInfo,
    pub tools: Vec<RawTool>,
    /// The areas this server knows about, when it can say. Empty otherwise.
    pub places: Vec<String>,
    /// The names of things that can actually be operated — lights, switches,
    /// blinds and so on, each with the kind of thing it is. Sensors are left
    /// out: they cannot be commanded, and they are the bulk of a typical home.
    pub devices: Vec<Device>,
}

/// Everything needed to reach one MCP server.
#[derive(Debug, Clone)]
pub struct McpEndpoint {
    /// Full SSE URL, e.g. `http://homeassistant.local:8123/mcp_server/sse`.
    pub url: String,
    /// Bearer token, when the server wants one.
    pub token: Option<String>,
    /// Ceiling on the whole exchange.
    pub timeout: Duration,
}

/// Scheme + authority of `url` (`http://host:port`), used to resolve the
/// relative POST endpoint the server hands back.
fn origin_of(url: &str) -> Result<&str> {
    let after_scheme =
        url.find("://").map(|i| i + 3).ok_or_else(|| anyhow!("not an absolute URL: {url}"))?;
    Ok(url[after_scheme..].find('/').map_or(url, |i| &url[..after_scheme + i]))
}

/// What a server said when we ran one of its tools.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolResult {
    /// The text blocks of the result, joined. This is what the model sees.
    pub text: String,
    /// The server's own verdict. Note that `false` means only "the server
    /// did not object" — Home Assistant returns a perfectly successful
    /// result for a command that matched nothing and did nothing, so this
    /// is never on its own proof that anything happened.
    pub is_error: bool,
}

type ByteStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = reqwest::Result<bytes::Bytes>> + Send + Sync>>;

/// How replies come back.
enum Transport {
    /// Replies arrive on the event stream opened at connect time.
    Sse { stream: ByteStream, buf: SseBuffer },
    /// Replies arrive in each POST's response. The server may hand out a
    /// session id at `initialize`, which every later request must carry.
    Http { session_id: Option<String> },
}

/// `true` when `url` names an SSE endpoint rather than a Streamable HTTP one.
fn is_sse_url(url: &str) -> bool {
    url.trim_end_matches('/').ends_with("/sse")
}

/// An open exchange with one server: where requests go, how replies come
/// back, and the handshake already done.
struct Session {
    http: reqwest::Client,
    token: Option<String>,
    endpoint: String,
    transport: Transport,
    next_id: u64,
    server: ServerInfo,
}

impl Session {
    async fn open(ep: &McpEndpoint) -> Result<Self> {
        let mut s = if is_sse_url(&ep.url) {
            Self::open_sse(ep).await?
        } else {
            Self {
                http: reqwest::Client::new(),
                token: ep.token.clone(),
                endpoint: ep.url.clone(),
                transport: Transport::Http { session_id: None },
                next_id: 1,
                server: ServerInfo::default(),
            }
        };
        // Handshake. Over SSE the response comes back over the stream, not
        // the POST.
        let init = s.request("initialize", init_params()).await?;
        s.server = server_info(&init);
        // Servers may refuse to answer anything before this notification.
        s.notify("notifications/initialized").await?;
        Ok(s)
    }

    async fn open_sse(ep: &McpEndpoint) -> Result<Self> {
        let http = reqwest::Client::builder()
            // The SSE stream idles between our requests; no total-response
            // timeout, only the outer deadline.
            .build()
            .context("build http client")?;

        let mut req = http.get(&ep.url).header("Accept", "text/event-stream");
        if let Some(tok) = &ep.token {
            req = req.bearer_auth(tok);
        }
        let resp = req.send().await.with_context(|| format!("connect to {}", ep.url))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            bail!("{} rejected the access token ({status})", ep.url);
        }
        if !status.is_success() {
            bail!("MCP server returned {status} for {}", ep.url);
        }
        // Catch the common mistake early: pointing at a web UI rather than at
        // the MCP endpoint. That answers 200 with HTML and then just closes,
        // which would otherwise surface much later as a baffling "stream
        // closed".
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        if !content_type.starts_with("text/event-stream") {
            bail!(
                "{} answered with {} instead of an event stream — is this the MCP \
                 address? (Home Assistant uses …:8123/mcp_server/sse)",
                ep.url,
                if content_type.is_empty() { "no content type" } else { &content_type }
            );
        }

        let mut stream: ByteStream = Box::pin(resp.bytes_stream());
        let mut buf = SseBuffer::new();

        // The server's first event names the POST endpoint.
        let endpoint = read_endpoint(&mut stream, &mut buf, origin_of(&ep.url)?).await?;

        Ok(Self {
            http,
            token: ep.token.clone(),
            endpoint,
            transport: Transport::Sse { stream, buf },
            next_id: 1,
            server: ServerInfo::default(),
        })
    }

    /// Post a JSON-RPC request and wait for its reply. Returns the whole
    /// message, so callers can read `result` or `error`.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let resp = self.post(&rpc(id, method, params)).await?;
        match &mut self.transport {
            Transport::Sse { stream, buf } => read_response(stream, buf, id).await,
            Transport::Http { session_id } => {
                if let Some(sid) = resp.headers().get(SESSION_HEADER) {
                    *session_id = sid.to_str().ok().map(ToOwned::to_owned);
                }
                read_http_response(resp, id).await
            }
        }
    }

    async fn notify(&self, method: &str) -> Result<()> {
        self.post(&json!({"jsonrpc": "2.0", "method": method})).await.map(drop)
    }

    async fn post(&self, body: &Value) -> Result<reqwest::Response> {
        let mut req = self.http.post(&self.endpoint).json(body);
        if let Transport::Http { session_id } = &self.transport {
            req = req.header("Accept", "application/json, text/event-stream");
            if let Some(sid) = session_id {
                req = req.header(SESSION_HEADER, sid);
            }
        }
        if let Some(tok) = &self.token {
            req = req.bearer_auth(tok);
        }
        let resp = req.send().await.with_context(|| format!("POST {}", self.endpoint))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            bail!("{} rejected the access token ({status})", self.endpoint);
        }
        if !status.is_success() {
            bail!("MCP server returned {status} for POST {}", self.endpoint);
        }
        Ok(resp)
    }
}

/// The header a Streamable HTTP server uses to name the session.
const SESSION_HEADER: &str = "mcp-session-id";

/// The reply to request `id` in a Streamable HTTP response, which is either
/// the JSON-RPC message itself or an event stream that carries it.
async fn read_http_response(resp: reqwest::Response, id: u64) -> Result<Value> {
    let is_stream = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/event-stream"));
    if is_stream {
        let mut stream: ByteStream = Box::pin(resp.bytes_stream());
        return read_response(&mut stream, &mut SseBuffer::new(), id).await;
    }
    let msg: Value = resp.json().await.context("read MCP response")?;
    if let Some(err) = msg.get("error") {
        bail!("MCP server error: {err}");
    }
    Ok(msg)
}

/// Open the stream, `initialize`, `tools/list`, close.
///
/// Returns the tools sorted by name: the catalogue render is byte-stable
/// (which keeps a pinned prompt-cache prefix valid), so discovery must not
/// leak server-side ordering into it.
pub async fn discover(ep: &McpEndpoint) -> Result<Discovery> {
    tokio::time::timeout(ep.timeout, discover_inner(ep))
        .await
        .map_err(|_| anyhow!("MCP discovery timed out after {:?}", ep.timeout))?
}

async fn discover_inner(ep: &McpEndpoint) -> Result<Discovery> {
    let mut s = Session::open(ep).await?;
    let listed = s.request("tools/list", json!({})).await?;
    let mut tools = parse_tools(&listed)?;
    remember_wire_names(&ep.url, &tools);
    for t in &mut tools {
        t.name = short_name(&t.name).to_owned();
    }
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    tools.dedup_by(|a, b| a.name == b.name);
    // While we are here, ask the server what areas it has. Doing it now
    // rather than per command is the whole point: the model can be told the
    // real names without anybody waiting for a round-trip to find them.
    let mut places = Vec::new();
    let mut devices = Vec::new();
    if tools.iter().any(|t| t.name == LIVE_CONTEXT) {
        let wire = wire_name(&ep.url, LIVE_CONTEXT);
        match s.request("tools/call", json!({"name": wire, "arguments": {}})).await {
            Ok(msg) => {
                let text = parse_tool_result(&msg).text;
                places = parse_places(&text);
                devices = parse_devices(&text);
            }
            // A server that will not describe itself is not a broken server.
            // Discovery still succeeded; we simply have no area names.
            Err(e) => tracing::debug!(target: "fono.assistant", "no place names: {e}"),
        }
    }
    Ok(Discovery { server: s.server, tools, places, devices })
}

/// The tool a Home Assistant offers for describing itself.
const LIVE_CONTEXT: &str = "GetLiveContext";

/// Pull area names out of a Home Assistant live-context dump.
///
/// Deliberately a text scrape rather than a schema: the dump is prose-ish
/// YAML meant for a model to read, and every entity carries the area it is
/// in as `areas: <name>`. Anything unrecognised yields nothing, which
/// simply means the model is not told any names — the state we were in
/// before, never a wrong answer.
fn parse_places(text: &str) -> Vec<String> {
    let inner = inner_dump(text);
    let mut names: Vec<String> = inner
        .lines()
        .filter_map(|l| l.trim().strip_prefix("areas:").map(str::trim))
        .map(canonical_name)
        .filter(|n| !n.is_empty() && n.len() < 64)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Unwrap a live-context payload.
///
/// The dump arrives as JSON with the text under `result`, but a bare dump is
/// accepted too so a differently-shaped server still works. Getting this
/// wrong is invisible: nothing errors, we simply find no names.
fn inner_dump(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.get("result").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| text.to_string())
}

/// Domains worth naming to the model: things a command can act on.
///
/// Sensors are excluded deliberately — they are the bulk of a house (74 of
/// 155 on the home this was built against) and no command can switch one, so
/// listing them would triple the cost for nothing.
const ACTIONABLE: &[&str] = &[
    "light",
    "switch",
    "cover",
    "climate",
    "media_player",
    "lock",
    "vacuum",
    "fan",
    "button",
    "scene",
    "script",
    "todo",
];

/// The first of the names on one dump line, without its quotes.
///
/// A thing with aliases lists them all on one line — `areas: Kitchen,
/// bucătărie`, `- names: Kitchen display, boxa bucatarie` — and the first is
/// the one the house is really called. Taking the line whole offered the model
/// a name no house ever answers to; taking every part offered a second room and
/// a second speaker that do not exist. The server matches an alias itself, so
/// nothing is lost by naming each thing once.
fn canonical_name(line: &str) -> String {
    let first = line.split(',').next().unwrap_or(line);
    first.trim().trim_matches(['\'', '"']).trim().to_string()
}

/// Pull the actionable devices out of a live-context dump, each with the kind
/// of thing it is.
///
/// Same scrape as [`parse_places`], but it walks entity blocks rather than
/// filtering lines because the kind follows the name. A block looks like:
///
/// ```text
/// - names: Office outdoor light
///   domain: light
///   state: 'on'
///   areas: Yard
/// ```
///
/// The kind is kept rather than discarded because it is what lets Fono offer
/// the model the kinds this home actually contains.
fn parse_devices(text: &str) -> Vec<Device> {
    let inner = inner_dump(text);
    let mut found = Vec::new();
    let mut pending: Option<String> = None;
    for line in inner.lines() {
        let t = line.trim();
        if let Some(n) = t.strip_prefix("- names:") {
            // A block without a domain line is dropped, not guessed at.
            pending = Some(canonical_name(n));
        } else if let Some(d) = t.strip_prefix("domain:") {
            let d = d.trim().trim_matches(['\'', '"']).trim();
            if let Some(n) = pending.take() {
                if ACTIONABLE.contains(&d) && !n.is_empty() && n.len() < 64 {
                    found.push(Device::new(n, d));
                }
            }
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}

/// Run one tool and return what the server said.
///
/// Each call is its own short-lived session. Holding a stream open between
/// utterances would be faster, but a stale session fails at the worst
/// possible moment — mid-command — and reconnecting costs a fraction of a
/// second against a model turn measured in seconds.
pub async fn call_tool(ep: &McpEndpoint, name: &str, args: &Value) -> Result<ToolResult> {
    tokio::time::timeout(ep.timeout, call_tool_inner(ep, name, args))
        .await
        .map_err(|_| anyhow!("{name} did not finish within {:?}", ep.timeout))?
}

async fn call_tool_inner(ep: &McpEndpoint, name: &str, args: &Value) -> Result<ToolResult> {
    let mut s = Session::open(ep).await?;
    if !knows_wire_names(&ep.url) {
        let listed = s.request("tools/list", json!({})).await?;
        remember_wire_names(&ep.url, &parse_tools(&listed)?);
    }
    let wire = wire_name(&ep.url, name);
    let msg = s.request("tools/call", json!({"name": wire, "arguments": args})).await?;
    Ok(parse_tool_result(&msg))
}

/// A tool's name without the namespace a server may put in front of it.
///
/// Home Assistant namespaces tools by the API that provides them on some
/// endpoints (`intent__HassTurnOn`, `homeassistant__GetLiveContext`) and
/// not on others. Fono's catalogue, rules and verification all know the
/// plain names, so every name is stored plain and turned back into the
/// server's own spelling only when a tool is called.
fn short_name(wire: &str) -> &str {
    wire.rsplit_once("__").map_or(wire, |(_, name)| name)
}

type WireNames = std::collections::HashMap<String, std::collections::HashMap<String, String>>;

/// Plain tool name → the server's own name, per server URL.
fn wire_names() -> &'static std::sync::Mutex<WireNames> {
    static NAMES: std::sync::OnceLock<std::sync::Mutex<WireNames>> = std::sync::OnceLock::new();
    NAMES.get_or_init(std::sync::Mutex::default)
}

fn remember_wire_names(url: &str, tools: &[RawTool]) {
    let mut map = std::collections::HashMap::new();
    for t in tools {
        // Two namespaces offering the same plain name: the first one wins,
        // as it does in the deduplicated catalogue.
        map.entry(short_name(&t.name).to_owned()).or_insert_with(|| t.name.clone());
    }
    if let Ok(mut names) = wire_names().lock() {
        names.insert(url.to_owned(), map);
    }
}

fn knows_wire_names(url: &str) -> bool {
    wire_names().lock().is_ok_and(|n| n.contains_key(url))
}

/// The name `url`'s server knows `name` by, or `name` itself when unknown.
fn wire_name(url: &str, name: &str) -> String {
    wire_names()
        .lock()
        .ok()
        .and_then(|n| n.get(url).and_then(|m| m.get(name)).cloned())
        .unwrap_or_else(|| name.to_owned())
}

fn parse_tool_result(msg: &Value) -> ToolResult {
    let result = msg.get("result");
    let text = result
        .and_then(|r| r.get("content"))
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    ToolResult {
        text,
        is_error: result.and_then(|r| r.get("isError")).and_then(Value::as_bool).unwrap_or(false),
    }
}

fn rpc(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn init_params() -> Value {
    json!({
        "protocolVersion": "2025-03-26",
        "capabilities": {},
        "clientInfo": {"name": "fono", "version": env!("CARGO_PKG_VERSION")},
    })
}

/// Pull events until the `endpoint` event arrives, resolving it against
/// `origin` when the server sends a bare path (Home Assistant does).
async fn read_endpoint(
    stream: &mut (impl futures::Stream<Item = reqwest::Result<bytes::Bytes>> + Unpin),
    buf: &mut SseBuffer,
    origin: &str,
) -> Result<String> {
    loop {
        if let Some(ev) = buf.next_event() {
            if ev.event.as_deref() == Some("endpoint") {
                let d = ev.data.trim();
                return Ok(if d.starts_with("http") {
                    d.to_owned()
                } else {
                    format!("{origin}{d}")
                });
            }
            continue;
        }
        let chunk = stream
            .next()
            .await
            .ok_or_else(|| anyhow!("MCP stream closed before naming its endpoint"))?
            .context("read MCP stream")?;
        buf.push(&chunk);
    }
}

/// Pull events until a JSON-RPC message with `id` arrives.
async fn read_response(
    stream: &mut (impl futures::Stream<Item = reqwest::Result<bytes::Bytes>> + Unpin),
    buf: &mut SseBuffer,
    id: u64,
) -> Result<Value> {
    loop {
        if let Some(ev) = buf.next_event() {
            let Ok(msg) = serde_json::from_str::<Value>(&ev.data) else {
                continue;
            };
            if msg.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(err) = msg.get("error") {
                bail!("MCP server error: {err}");
            }
            return Ok(msg);
        }
        let chunk = stream
            .next()
            .await
            .ok_or_else(|| anyhow!("MCP stream closed while awaiting response {id}"))?
            .context("read MCP stream")?;
        buf.push(&chunk);
    }
}

fn server_info(init: &Value) -> ServerInfo {
    let result = init.get("result");
    let info = result.and_then(|r| r.get("serverInfo"));
    let s = |v: Option<&Value>, k: &str| {
        v.and_then(|v| v.get(k)).and_then(Value::as_str).unwrap_or_default().to_owned()
    };
    ServerInfo {
        name: s(info, "name"),
        version: s(info, "version"),
        protocol_version: s(result, "protocolVersion"),
    }
}

fn parse_tools(listed: &Value) -> Result<Vec<RawTool>> {
    let arr = listed
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("tools/list response had no result.tools array"))?;
    Ok(arr
        .iter()
        .filter_map(|t| {
            let name = t.get("name")?.as_str()?.to_owned();
            Some(RawTool {
                name,
                description: t
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                schema: t.get("inputSchema").cloned().unwrap_or_else(|| json!({})),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_strips_path() {
        assert_eq!(
            origin_of("http://homeassistant.local:8123/mcp_server/sse").unwrap(),
            "http://homeassistant.local:8123"
        );
        assert_eq!(origin_of("https://example.org").unwrap(), "https://example.org");
        assert!(origin_of("example.org/sse").is_err());
    }

    #[test]
    fn parses_the_home_assistant_shape() {
        let listed = json!({"result": {"tools": [
            {"name": "HassTurnOn", "description": "Turns on a device",
             "inputSchema": {"type": "object", "properties": {"area": {"type": "string"}}}},
            {"name": "GetLiveContext"},
        ]}});
        let tools = parse_tools(&listed).unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "HassTurnOn");
        // A tool with no schema still parses — it just takes no arguments.
        assert_eq!(tools[1].schema, json!({}));
    }

    #[test]
    fn rejects_a_response_without_tools() {
        assert!(parse_tools(&json!({"result": {}})).is_err());
    }

    /// The real dump shape: JSON envelope, prose-ish YAML inside, one
    /// `areas:` line per entity — so the same area appears many times.
    #[test]
    fn scrapes_area_names_from_a_live_context_dump() {
        let dump = "Live Context:\n- names: Kitchen lights\n  domain: light\n  \
                    areas: Kitchen\n  state: 'off'\n- names: Kitchen lights (2)\n  \
                    domain: light\n  areas: Kitchen\n- names: Hall lamp\n  \
                    domain: light\n  areas: Hall\n- names: Some sensor\n  \
                    domain: sensor\n  areas: 'Yard'\n";
        let text = serde_json::to_string(&json!({"result": dump})).unwrap();
        // Deduplicated and sorted, so the same house always renders the same
        // bytes — a shifting list would invalidate the cached prompt prefix.
        assert_eq!(parse_places(&text), vec!["Hall", "Kitchen", "Yard"]);
        // A bare dump works too, for a server that does not wrap it.
        assert_eq!(parse_places(dump), vec!["Hall", "Kitchen", "Yard"]);
    }

    /// A thing with aliases lists them all on one line, and only the first is
    /// what the house is called. Offered whole, "Kitchen, bucătărie" is an area
    /// that does not exist; offered as two, the model is told of a room that
    /// does not exist either. The server matches an alias on its own.
    #[test]
    fn an_alias_is_not_a_second_room_or_device() {
        let dump = "- names: Kitchen display, boxa bucatarie\n  domain: media_player\n  \
                    areas: Kitchen, bucătărie\n- names: Hall light\n  domain: light\n  \
                    areas: 'Hallway , hol'\n";
        assert_eq!(parse_places(dump), vec!["Hallway", "Kitchen"]);
        assert_eq!(
            parse_devices(dump),
            vec![
                Device::new("Hall light", "light"),
                Device::new("Kitchen display", "media_player"),
            ]
        );
    }

    /// Anything we do not recognise must yield no names at all. Telling the
    /// model a wrong area name is worse than telling it none: it would aim
    /// confidently at something that does not exist.
    #[test]
    fn an_unrecognised_dump_yields_no_area_names() {
        assert!(parse_places("").is_empty());
        assert!(parse_places("{\"result\": \"nothing useful here\"}").is_empty());
        assert!(parse_places("areas:").is_empty());
    }

    /// Only things that can be operated are worth naming. Sensors and their
    /// binary cousins are two thirds of a real house and nothing can be done
    /// to them, so listing them would spend tokens teaching the model names
    /// it must never act on.
    #[test]
    fn scrapes_only_operable_device_names() {
        let dump = "Live Context:\n- names: Office outdoor light\n  domain: light\n  \
                    areas: Yard\n  state: 'on'\n- names: Alocasia Soil humidity\n  \
                    domain: sensor\n  areas: Living room\n- names: Front door\n  \
                    domain: lock\n  areas: Hallway\n- names: Back door open\n  \
                    domain: binary_sensor\n  areas: Hallway\n";
        let text = serde_json::to_string(&json!({"result": dump})).unwrap();
        let want =
            vec![Device::new("Front door", "lock"), Device::new("Office outdoor light", "light")];
        assert_eq!(parse_devices(&text), want);
        // A bare dump works too, for a server that does not wrap it.
        assert_eq!(parse_devices(dump), want);
    }

    /// The exact shape that failed against a real house: the lamp is named
    /// after the area it lights, not the one it sits in. Its name must reach
    /// the model verbatim, because the house matches names exactly — neither
    /// "outdoor light" nor "outdoor office light" finds it.
    #[test]
    fn keeps_a_device_name_that_disagrees_with_its_area() {
        let dump = "- names: Office outdoor light\n  domain: light\n  areas: Yard\n";
        assert_eq!(parse_devices(dump), vec![Device::new("Office outdoor light", "light")]);
    }

    /// The kind of each device is carried through discovery, because it is what
    /// lets Fono offer the model only the kinds this home actually has.
    #[test]
    fn keeps_the_kind_of_each_device() {
        let dump = "- names: Kitchen lights\n  domain: light\n- names: Living room blind\n  \
                    domain: cover\n- names: Kitchen speaker\n  domain: media_player\n";
        let found = parse_devices(dump);
        let kinds: Vec<&str> = found.iter().map(|d| d.domain.as_str()).collect();
        // Sorted by name, so: Kitchen lights, Kitchen speaker, Living room blind.
        assert_eq!(kinds, vec!["light", "media_player", "cover"]);
    }

    /// A block with no domain line is dropped rather than guessed at, and an
    /// unrecognised dump yields nothing: a wrong name is worse than no name,
    /// because the model would aim confidently at something absent.
    #[test]
    fn an_unrecognised_dump_yields_no_device_names() {
        assert!(parse_devices("").is_empty());
        assert!(parse_devices("- names: Mystery lamp\n  areas: Yard\n").is_empty());
        assert!(parse_devices("{\"result\": \"nothing useful here\"}").is_empty());
    }

    #[test]
    fn reads_server_info() {
        let init = json!({"result": {
            "protocolVersion": "2025-03-26",
            "serverInfo": {"name": "Home Assistant", "version": "2026.7.3"},
        }});
        let info = server_info(&init);
        assert_eq!(info.name, "Home Assistant");
        assert_eq!(info.version, "2026.7.3");
        assert_eq!(info.protocol_version, "2025-03-26");
    }

    #[test]
    fn namespaced_tool_names_are_stored_plain() {
        assert_eq!(short_name("intent__HassTurnOn"), "HassTurnOn");
        assert_eq!(short_name("homeassistant__GetLiveContext"), "GetLiveContext");
        assert_eq!(short_name("HassTurnOn"), "HassTurnOn");
    }

    #[test]
    fn picks_the_transport_from_the_url() {
        assert!(is_sse_url("http://homeassistant.local:8123/mcp_server/sse"));
        assert!(is_sse_url("http://ha:8123/mcp_server/sse/"));
        assert!(!is_sse_url("http://supervisor/core/api/mcp"));
    }

    /// A Streamable HTTP server, the way Home Assistant's `/api/mcp`
    /// behaves: every reply is the POST's own JSON body. The mock also
    /// hands out a session id and refuses requests that don't carry it.
    #[tokio::test]
    async fn calls_a_tool_over_streamable_http() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let mut raw = Vec::new();
                let mut chunk = [0u8; 4096];
                // Read the head, then exactly Content-Length body bytes.
                let (head, body) = loop {
                    let n = sock.read(&mut chunk).await.unwrap();
                    raw.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&raw).to_string();
                    if let Some(i) = text.find("\r\n\r\n") {
                        let head = text[..i].to_lowercase();
                        let len: usize = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .map_or(0, |v| v.trim().parse().unwrap());
                        if raw.len() >= i + 4 + len {
                            break (head, text[i + 4..i + 4 + len].to_string());
                        }
                    }
                };
                let msg: Value = serde_json::from_str(&body).unwrap();
                let method = msg["method"].as_str().unwrap_or_default();
                let has_session = head.contains("mcp-session-id: s1");
                let reply = match (method, has_session) {
                    ("initialize", _) => json!({"jsonrpc": "2.0", "id": msg["id"], "result": {
                        "protocolVersion": "2025-03-26",
                        "serverInfo": {"name": "Home Assistant", "version": "2026.9"}}}),
                    (_, false) => Value::Null,
                    ("tools/list", true) => json!({"jsonrpc": "2.0", "id": msg["id"], "result": {
                        "tools": [{"name": "intent__HassTurnOn", "inputSchema": {}}]}}),
                    // Only the server's own, namespaced name runs the tool.
                    ("tools/call", true) if msg["params"]["name"] == "intent__HassTurnOn" => {
                        json!({"jsonrpc": "2.0", "id": msg["id"], "result": {
                            "content": [{"type": "text", "text": "turned on"}], "isError": false}})
                    }
                    ("tools/call", true) => json!({"jsonrpc": "2.0", "id": msg["id"], "result": {
                        "content": [{"type": "text", "text": "not found"}], "isError": true}}),
                    _ => json!({}),
                };
                let (status, body) = if reply.is_null() {
                    ("400 Bad Request", String::new())
                } else {
                    ("200 OK", reply.to_string())
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                     mcp-session-id: s1\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        });
        let ep = McpEndpoint {
            url: format!("http://{addr}/api/mcp"),
            token: Some("t".into()),
            timeout: Duration::from_secs(5),
        };
        let out = call_tool(&ep, "HassTurnOn", &json!({"name": "Kitchen"})).await.unwrap();
        assert_eq!(out.text, "turned on");
        assert!(!out.is_error);
    }

    /// Live check against a real MCP server — the only thing that proves the
    /// SSE handshake is right, since the transport quirk (responses arrive on
    /// the stream, not the POST) is exactly what a mocked test would get
    /// wrong. Set `FONO_TEST_MCP_URL` and, if the server wants one,
    /// `FONO_TEST_MCP_TOKEN`.
    #[tokio::test]
    #[ignore = "needs a reachable MCP server"]
    async fn discovers_a_live_server() {
        let Ok(url) = std::env::var("FONO_TEST_MCP_URL") else { return };
        let ep = McpEndpoint {
            url,
            token: std::env::var("FONO_TEST_MCP_TOKEN").ok(),
            timeout: Duration::from_secs(30),
        };
        let d = discover(&ep).await.expect("discovery failed");
        eprintln!("server: {} {}", d.server.name, d.server.version);
        for t in &d.tools {
            eprintln!("  {}", t.name);
        }
        assert!(!d.server.name.is_empty(), "server did not identify itself");
        assert!(!d.tools.is_empty(), "server advertised no tools");
        // Sorted, so the catalogue render stays byte-stable.
        assert!(d.tools.windows(2).all(|w| w[0].name <= w[1].name));
    }
}
