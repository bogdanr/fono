// SPDX-License-Identifier: GPL-3.0-only
//! Length-prefixed bincode frames over a Unix socket at
//! `$XDG_STATE_HOME/fono/fono.sock`. Phase 8 Task 8.4.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

/// Commands from the CLI to the running daemon.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Request {
    /// Start-or-stop recording (toggle).
    Toggle,
    /// Press-and-hold start.
    HoldPress,
    /// Press-and-hold release.
    HoldRelease,
    /// Re-type the last cleaned transcription.
    PasteLast,
    /// Run diagnostics.
    Doctor,
    /// Query daemon status.
    Status,
    /// Re-read config + secrets and rebuild STT/LLM in-place. Used by
    /// `fono use …` and `fono keys …` so the user doesn't need to
    /// restart the daemon to switch providers. Provider-switching plan
    /// task S11.
    Reload,
    /// Snapshot the current LAN-discovery registry. Slice 4 of the
    /// network plan — backs `fono discover` and the tray's
    /// *Discovered on LAN* submenu.
    ListDiscovered,
    /// Voice-assistant push-to-talk start. Mirrors `HoldPress` but
    /// routes to the assistant pipeline (STT → assistant chat → TTS →
    /// playback) instead of dictation. Step 3 of the assistant plan.
    AssistantHoldPress,
    /// Voice-assistant push-to-talk release. Triggers the streaming
    /// assistant pump.
    AssistantHoldRelease,
    /// Stop assistant playback / pump immediately. Used for "shut
    /// up"-style cancellation (Escape, tray "Stop assistant"). The
    /// rolling history is preserved so a follow-up turn can build on
    /// the conversation.
    AssistantStop,
    /// Wipe the assistant's rolling conversation history (and stop
    /// any in-flight playback). Backs the tray "Forget conversation"
    /// entry. Distinct from [`Self::AssistantStop`] so a casual stop
    /// doesn't lose context.
    AssistantForget,
    /// Graceful shutdown.
    Shutdown,
}

/// Serializable, IPC-friendly view of one mDNS-discovered peer. Mirrors
/// `fono_net::discovery::DiscoveredPeer` minus the `Instant`/`IpAddr`
/// types. Slice 4.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveredPeer {
    /// `"wyoming"` | `"fono"`.
    pub kind: String,
    /// mDNS instance fullname (e.g. `fono-A._wyoming._tcp.local.`).
    pub fullname: String,
    /// Friendly instance name (the part before the service type).
    pub name: String,
    /// Resolved hostname (typically `<host>.local.`, trailing dot
    /// stripped before serialisation).
    pub hostname: String,
    /// First resolved address as a string, if any.
    pub address: Option<String>,
    /// Service port.
    pub port: u16,
    /// `proto` TXT key (`wyoming/1` / `fono/1`).
    pub proto: String,
    /// `version` TXT key.
    pub version: String,
    /// `caps` TXT key, comma-split.
    pub caps: Vec<String>,
    /// `model` TXT key — Wyoming-only.
    pub model: Option<String>,
    /// `auth` TXT key — `true` if peer expects a bearer token.
    pub auth_required: bool,
    /// `path` TXT key — WebSocket path for Fono-native peers.
    pub path: Option<String>,
    /// Seconds since the registry last saw a `ServiceResolved` for
    /// this peer (truncated `u64`).
    pub age_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Response {
    Ok,
    /// Textual payload (e.g. status summary, doctor report).
    Text(String),
    /// Snapshot of the LAN-discovery registry. Slice 4.
    Discovered(Vec<DiscoveredPeer>),
    Error(String),
}

/// Write a length-prefixed bincode frame.
pub async fn write_frame<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<()> {
    let bytes = bincode::serialize(value).context("bincode serialize")?;
    let len = u32::try_from(bytes.len()).context("frame too large")?;
    stream.write_all(&len.to_le_bytes()).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

/// Read a length-prefixed bincode frame.
pub async fn read_frame<T: for<'de> Deserialize<'de>>(stream: &mut UnixStream) -> Result<T> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(bincode::deserialize(&buf).context("bincode deserialize")?)
}

/// Bind a Unix-socket listener, removing any stale socket first. Sets mode 0600.
pub fn bind_listener(socket: &Path) -> Result<UnixListener> {
    if socket.exists() {
        let _ = std::fs::remove_file(socket);
    }
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = UnixListener::bind(socket).with_context(|| format!("bind UDS at {socket:?}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600));
    }
    Ok(listener)
}

/// Dial the daemon. Returns a descriptive error when the daemon isn't running.
pub async fn connect(socket: &Path) -> Result<UnixStream> {
    UnixStream::connect(socket).await.with_context(|| {
        format!(
            "fono: daemon not running at {socket:?}; start it with 'fono' or install the \
             autostart unit"
        )
    })
}

/// Try a sequence of socket paths in order and return the first
/// successful connection. Used by the CLI which prefers the
/// system-service socket (`/var/lib/fono/fono.sock` — installed by the
/// headless `fono.service` unit) and falls back to the per-user XDG
/// socket so `systemctl --user` and standalone deployments keep
/// working. The error path reports every path that was tried.
pub async fn connect_any(sockets: &[std::path::PathBuf]) -> Result<UnixStream> {
    let mut last_err: Option<std::io::Error> = None;
    for sock in sockets {
        match UnixStream::connect(sock).await {
            Ok(s) => return Ok(s),
            Err(e) => {
                tracing::debug!(
                    target: "fono::ipc",
                    socket = %sock.display(),
                    error = %e,
                    "ipc connect candidate failed"
                );
                last_err = Some(e);
            }
        }
    }
    let summary = if sockets.is_empty() {
        "<none>".to_string()
    } else {
        sockets
            .iter()
            .map(|p| format!("{}", p.display()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let cause = last_err.map_or_else(
        || "no IPC socket candidates configured".to_string(),
        |e| format!("{e}"),
    );
    Err(anyhow::anyhow!(
        "fono: daemon not running (tried: {summary}; last error: {cause}); start it with \
         'fono' or install the autostart unit"
    ))
}

/// Round-trip a single request on a fresh connection.
pub async fn request(socket: &Path, req: &Request) -> Result<Response> {
    let mut stream = connect(socket).await?;
    write_frame(&mut stream, req).await?;
    let resp: Response = read_frame(&mut stream).await?;
    Ok(resp)
}

/// Like [`request`] but tries each socket in `sockets` in order. The
/// first successful connection is used to send the request.
pub async fn request_any(sockets: &[std::path::PathBuf], req: &Request) -> Result<Response> {
    let mut stream = connect_any(sockets).await?;
    write_frame(&mut stream, req).await?;
    let resp: Response = read_frame(&mut stream).await?;
    Ok(resp)
}
