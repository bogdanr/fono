// SPDX-License-Identifier: GPL-3.0-only
//! Server-Sent Events stream reader with an inter-chunk watchdog.
//!
//! Companion to [`crate::body::read_body_with_watchdog`] for endpoints
//! that stream their response as raw bytes the caller parses
//! incrementally (chat completions). Rather than try to embed an SSE
//! parser here, this helper just wraps the raw `bytes_stream()` in a
//! per-chunk timeout and hands each chunk to a user-supplied callback
//! — typically `parser.push(&chunk)` against the existing
//! `SseBuffer` in `fono-assistant`. That keeps the consumer's parse
//! logic untouched and the helper transport-only.

use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::StreamExt;
use reqwest::Response;
use thiserror::Error;

use crate::timings::RequestTimings;

/// Summary of a streamed-response read.
#[derive(Debug, Clone, Copy, Default)]
pub struct SseStats {
    /// Total raw bytes received over the stream.
    pub bytes: u64,
    /// Number of HTTP/transport chunks (not necessarily SSE events —
    /// providers chunk at the transport level independent of event
    /// boundaries).
    pub chunks: u32,
}

/// Failure modes for [`read_sse_with_watchdog`].
#[derive(Debug, Error)]
pub enum SseError {
    /// No chunk arrived within `chunk_timeout`. The caller's parser
    /// may already have produced partial output; the helper only
    /// detects the stall on the transport.
    #[error("sse stream stalled after {after_ms} ms ({chunks} chunks, {bytes} bytes received)")]
    Stalled {
        after_ms: u64,
        chunks: u32,
        bytes: u64,
    },
    /// Underlying transport returned an error mid-stream.
    #[error("sse transport error after {bytes} bytes: {source}")]
    Transport {
        chunks: u32,
        bytes: u64,
        #[source]
        source: reqwest::Error,
    },
    /// Caller-side parsing logic asked to abort. The helper does not
    /// try to interpret the wrapped error; it just propagates.
    #[error("sse consumer reported error: {0}")]
    Consumer(#[source] anyhow::Error),
}

impl SseError {
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Stalled { .. } => true,
            Self::Transport { source, .. } => {
                source.is_timeout() || source.is_connect() || source.is_request()
            }
            Self::Consumer(_) => false,
        }
    }
}

/// Drain `response`'s body chunk-by-chunk, invoking `on_chunk` for
/// every successfully-arrived chunk. Returns when:
///   * the upstream stream completes naturally (`on_chunk` reported
///     `Ok(false)` then the stream ended, or just the stream ended);
///   * `on_chunk` returns `Ok(true)`, indicating it's seen everything
///     it cares about (e.g. an SSE `data: [DONE]` line) — the helper
///     drops the rest of the stream;
///   * the watchdog or transport fails — returns the corresponding
///     [`SseError`].
///
/// `on_chunk(chunk) -> anyhow::Result<bool>`: return `Ok(true)` to
/// signal "done, stop reading", `Ok(false)` to continue, `Err` to
/// abort.
pub async fn read_sse_with_watchdog<F>(
    response: Response,
    chunk_timeout: Duration,
    timings: &mut RequestTimings,
    mut on_chunk: F,
) -> Result<SseStats, SseError>
where
    F: FnMut(&Bytes) -> anyhow::Result<bool>,
{
    let mut stream = response.bytes_stream();
    let mut bytes: u64 = 0;
    let mut chunks: u32 = 0;
    let watchdog_started = Instant::now();
    loop {
        match tokio::time::timeout(chunk_timeout, stream.next()).await {
            Err(_elapsed) => {
                return Err(SseError::Stalled {
                    after_ms: watchdog_started.elapsed().as_millis() as u64,
                    chunks,
                    bytes,
                });
            }
            Ok(None) => {
                timings.mark_body_done();
                return Ok(SseStats { bytes, chunks });
            }
            Ok(Some(Err(e))) => {
                return Err(SseError::Transport {
                    chunks,
                    bytes,
                    source: e,
                });
            }
            Ok(Some(Ok(chunk))) => {
                if chunks == 0 {
                    timings.mark_first_byte();
                }
                chunks = chunks.saturating_add(1);
                bytes += chunk.len() as u64;
                match on_chunk(&chunk) {
                    Ok(true) => {
                        timings.mark_body_done();
                        return Ok(SseStats { bytes, chunks });
                    }
                    Ok(false) => {}
                    Err(e) => return Err(SseError::Consumer(e)),
                }
            }
        }
    }
}
