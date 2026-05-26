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
    /// Cancel any in-flight activity: aborts an active recording
    /// (batch or live dictation) AND stops in-flight assistant
    /// playback / pump. Idempotent — no-op when nothing is active.
    /// Backs `fono cancel` (replacing the older `fono assistant stop`
    /// CLI verb) and the Wayland Escape fallback.
    Cancel,
    /// Graceful shutdown.
    Shutdown,
    /// MCP server is starting a voice interaction with the user
    /// (`fono.listen`, `fono.speak`, or `fono.confirm`). The daemon
    /// increments an internal depth counter and — on the 0→1
    /// transition — snapshots the current tray state and flips the
    /// tray to [`fono_tray::TrayState::Processing`] (amber). The
    /// `phase` field is logged for observability but, per v7 of the
    /// MCP overlay plan, all three phases map to the same amber tint
    /// today. Slice 7 of
    /// `plans/2026-05-26-mcp-listen-overlay-and-silence-parity-v7.md`.
    McpActivityStart { phase: McpPhase },
    /// Companion to [`Self::McpActivityStart`]. Decrements the
    /// daemon's depth counter; on the →0 transition the previously
    /// snapshotted tray state is restored (unless another writer has
    /// taken over in the meantime — last-writer-wins).
    McpActivityEnd,
    /// MCP server asks the daemon for exclusive access to the audio
    /// output device for the duration of a `fono.speak` playback.
    /// The daemon serialises these requests through a single
    /// `tokio::sync::Mutex` so concurrent `fono mcp serve` processes
    /// (one per coding agent — Claude Code, Forge, Cursor, …) never
    /// produce overlapping TTS audio on the same speakers.
    ///
    /// **Connection-scoped lock.** The daemon writes the matching
    /// `Response::Ok` *only* after acquiring the mutex, and keeps the
    /// connection open while the lock is held. The client signals
    /// release by closing its end of the socket (drop the
    /// `UnixStream`); the daemon's read loop sees EOF and releases
    /// the mutex. This makes the lock robust against MCP-server
    /// crashes — kernel-level socket cleanup always unblocks the
    /// next waiter.
    ///
    /// **Best-effort.** Clients fall back to no-coordination
    /// (potential overlap) when the daemon is unreachable; the
    /// guarantee is "no overlap *when the daemon is running*".
    McpSpeakAcquire,
}

/// Sub-state of an MCP voice interaction. Logged on the daemon side
/// for observability; all three variants map to the same
/// [`fono_tray::TrayState::Processing`] (amber) tint in v7. Future
/// versions may map them to distinct tints; the wire format already
/// carries the discriminator so the change is daemon-local.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum McpPhase {
    /// `fono.listen` — microphone is open.
    Listening,
    /// `fono.speak` — TTS audio is playing back (only emitted for
    /// utterances ≥ 1 s so short prompts don't flash the tray).
    Speaking,
    /// `fono.confirm` — listening for an A/B/C answer.
    Confirming,
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
        sockets.iter().map(|p| format!("{}", p.display())).collect::<Vec<_>>().join(", ")
    };
    let cause = last_err
        .map_or_else(|| "no IPC socket candidates configured".to_string(), |e| format!("{e}"));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip a `Request` through bincode. Mirrors the wire-level
    /// `write_frame` / `read_frame` codec used at runtime so a future
    /// breaking change to the encoding shows up here.
    fn bincode_roundtrip(req: &Request) -> Request {
        let bytes = bincode::serialize(req).expect("serialize");
        bincode::deserialize(&bytes).expect("deserialize")
    }

    #[test]
    fn mcp_activity_start_roundtrips() {
        for phase in [McpPhase::Listening, McpPhase::Speaking, McpPhase::Confirming] {
            let req = Request::McpActivityStart { phase };
            assert_eq!(bincode_roundtrip(&req), req);
        }
    }

    #[test]
    fn mcp_activity_end_roundtrips() {
        let req = Request::McpActivityEnd;
        assert_eq!(bincode_roundtrip(&req), req);
    }

    #[test]
    fn mcp_speak_acquire_roundtrips() {
        let req = Request::McpSpeakAcquire;
        assert_eq!(bincode_roundtrip(&req), req);
    }
}
