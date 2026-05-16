// SPDX-License-Identifier: GPL-3.0-only
//! Streamed body reader with an inter-chunk watchdog.
//!
//! `reqwest::Response::bytes()` uses the client's *overall* timeout
//! for the entire body. That conflates two failure modes:
//! "connection is hung and producing nothing" with "connection is
//! slowly producing data but progressing." For Fono's voice-assistant
//! TTS path, the first mode is fatal (the user hears silence for a
//! minute) while the second is recoverable (slow but eventually
//! useful audio).
//!
//! This module implements a streamed reader that resets a deadline
//! every time a chunk arrives. A hung connection trips the watchdog
//! in `chunk_timeout` seconds; a slow-but-progressing connection
//! keeps making progress and runs only against the caller's overall
//! `reqwest` timeout.

use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::StreamExt;
use reqwest::Response;
use thiserror::Error;

use crate::timings::RequestTimings;

/// Detailed outcome of a successful body read.
#[derive(Debug, Clone, Copy)]
pub struct BodyStats {
    /// Total body bytes drained.
    pub bytes: u64,
    /// Number of stream chunks observed. Always ≥ 1 on a successful
    /// read — a server returning Content-Length: 0 still produces a
    /// single empty-chunk delivery on most clients.
    pub chunks: u32,
}

/// Failure modes for [`read_body_with_watchdog`]. All include enough
/// state to render a structured `tracing` line without parsing a
/// `Display` string.
#[derive(Debug, Error)]
pub enum BodyError {
    /// No chunk arrived within `chunk_timeout`. The connection is
    /// hung from our side's point of view; the caller may safely
    /// retry the entire request because the server has either
    /// already completed processing (and is sitting on the body) or
    /// has dropped the inflight stream.
    #[error(
        "body stalled after {after_ms} ms (received {partial_bytes} bytes in {chunks} chunks)"
    )]
    Stalled {
        after_ms: u64,
        partial_bytes: u64,
        chunks: u32,
        /// Buffered bytes that arrived before the watchdog fired.
        /// Exposed so callers can dump a hex preview when diagnosing
        /// upstream framing weirdness (e.g. an opaque preamble that
        /// always precedes the actual payload).
        partial: Bytes,
    },
    /// Underlying transport returned an error mid-stream (TLS
    /// failure, connection reset, HTTP/2 GOAWAY, etc.). Caller may
    /// retry — see `BodyError::is_retryable`.
    #[error("body transport error after {partial_bytes} bytes: {source}")]
    Transport {
        partial_bytes: u64,
        chunks: u32,
        /// Buffered bytes that arrived before the transport error.
        partial: Bytes,
        #[source]
        source: reqwest::Error,
    },
}

impl BodyError {
    /// `true` when the error is a stall or a transport-level
    /// connection drop that's safe to retry once. Returns `false` for
    /// anything we'd rather not paper over silently.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Stalled { .. } => true,
            Self::Transport { source, .. } => {
                // reqwest doesn't expose a typed "connection reset"
                // signal, so we use `is_connect()` / `is_timeout()`
                // / `is_request()` as a proxy. Decode errors at the
                // body level surface as `is_decode()` and are NOT
                // retried because they indicate the server sent
                // malformed bytes — retrying just risks the same.
                source.is_timeout() || source.is_connect() || source.is_request()
            }
        }
    }

    /// Partial body bytes that arrived before the error. Useful for
    /// diagnostics ("the stall happened after 0 bytes" vs "after
    /// 90 % of a long synthesis").
    #[must_use]
    pub fn partial_bytes(&self) -> u64 {
        match self {
            Self::Stalled { partial_bytes, .. } | Self::Transport { partial_bytes, .. } => {
                *partial_bytes
            }
        }
    }

    /// Number of stream chunks observed before the error fired. On a
    /// stall this is exactly the number of successful chunk reads
    /// before silence; on a transport error it is the number of
    /// successful chunks before the mid-stream failure. Distinct
    /// from `partial_bytes` because a single 9.6 KB chunk that
    /// arrives intact and is then followed by silence reports
    /// `chunks = 1, partial_bytes = 9600` — useful for telling
    /// "proxy buffered then hung" from "nothing ever arrived" apart.
    #[must_use]
    pub fn chunks(&self) -> u32 {
        match self {
            Self::Stalled { chunks, .. } | Self::Transport { chunks, .. } => *chunks,
        }
    }

    /// Milliseconds the watchdog waited before firing. `0` for
    /// transport errors (no watchdog timeout was involved).
    #[must_use]
    pub fn after_ms(&self) -> u64 {
        match self {
            Self::Stalled { after_ms, .. } => *after_ms,
            Self::Transport { .. } => 0,
        }
    }

    /// Bytes that arrived before the error fired. Cheap clone
    /// (`Bytes` is reference-counted). Useful for one-shot
    /// diagnostic hex dumps when the upstream's framing is
    /// unexpected.
    #[must_use]
    pub fn partial(&self) -> Bytes {
        match self {
            Self::Stalled { partial, .. } | Self::Transport { partial, .. } => partial.clone(),
        }
    }
}

/// Drain `response`'s body, returning the full payload plus a
/// [`BodyStats`] summary. Aborts with [`BodyError::Stalled`] if no
/// chunk arrives within `chunk_timeout`.
///
/// `timings` is updated in place: `mark_first_byte()` fires on the
/// first chunk, `mark_body_done()` on the last. Caller is responsible
/// for emitting the structured `fono.http` log line via
/// [`crate::emit_http_debug`] after this returns.
///
/// `chunk_timeout` is the inter-chunk deadline — i.e. the maximum
/// time we'll wait between successive chunks before declaring the
/// connection stalled. The first-chunk deadline is the same value;
/// `chunk_timeout = 15s` is a reasonable default for TTS / one-shot
/// JSON responses.
pub async fn read_body_with_watchdog(
    response: Response,
    chunk_timeout: Duration,
    timings: &mut RequestTimings,
) -> Result<(Bytes, BodyStats), BodyError> {
    let mut stream = response.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut chunks: u32 = 0;
    let watchdog_started = Instant::now();
    loop {
        match tokio::time::timeout(chunk_timeout, stream.next()).await {
            Err(_elapsed) => {
                return Err(BodyError::Stalled {
                    after_ms: watchdog_started.elapsed().as_millis() as u64,
                    partial_bytes: buf.len() as u64,
                    chunks,
                    partial: Bytes::from(buf),
                });
            }
            Ok(None) => {
                // Stream complete.
                timings.mark_body_done();
                let final_bytes = Bytes::from(buf);
                let len = final_bytes.len() as u64;
                return Ok((final_bytes, BodyStats { bytes: len, chunks }));
            }
            Ok(Some(Err(e))) => {
                return Err(BodyError::Transport {
                    partial_bytes: buf.len() as u64,
                    chunks,
                    partial: Bytes::from(buf),
                    source: e,
                });
            }
            Ok(Some(Ok(chunk))) => {
                if chunks == 0 {
                    timings.mark_first_byte();
                }
                chunks = chunks.saturating_add(1);
                buf.extend_from_slice(&chunk);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalled_error_carries_partials() {
        let e = BodyError::Stalled {
            after_ms: 15_004,
            partial_bytes: 42,
            chunks: 1,
            partial: Bytes::from(vec![0xAB; 42]),
        };
        assert!(e.is_retryable());
        assert_eq!(e.partial_bytes(), 42);
        assert_eq!(e.chunks(), 1);
        assert_eq!(e.after_ms(), 15_004);
        assert_eq!(e.partial().len(), 42);
        let rendered = format!("{e}");
        assert!(rendered.contains("15004"));
        assert!(rendered.contains("42"));
    }
}
