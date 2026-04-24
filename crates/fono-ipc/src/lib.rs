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
    /// Graceful shutdown.
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Response {
    Ok,
    /// Textual payload (e.g. status summary, doctor report).
    Text(String),
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

/// Round-trip a single request on a fresh connection.
pub async fn request(socket: &Path, req: &Request) -> Result<Response> {
    let mut stream = connect(socket).await?;
    write_frame(&mut stream, req).await?;
    let resp: Response = read_frame(&mut stream).await?;
    Ok(resp)
}
