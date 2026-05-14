// SPDX-License-Identifier: GPL-3.0-only
//! Shared HTTP instrumentation, body watchdog, and upstream
//! request-id helpers for Fono's cloud backends.
//!
//! ## Goals
//!
//! - **Silent by default.** Every routine event emits at `debug!`
//!   under `target: "fono.http"`. Default subscriber is `info`, so
//!   users see nothing extra; turn it on per-session via
//!   `RUST_LOG=info,fono.http=debug`.
//! - **Zero hot-path cost.** Only [`std::time::Instant`] calls
//!   (single-digit nanoseconds) and one [`reqwest::header::HeaderMap`]
//!   lookup. `tracing` skips field formatting entirely when the target
//!   is filtered out, so structured fields cost nothing in production.
//! - **One schema for every provider.** Every consumer call site
//!   funnels through [`emit_http_debug`] so the field names + stage +
//!   provider taxonomy can never drift.
//! - **Detect stalled bodies before the global timeout.** [`read_body_with_watchdog`]
//!   and [`read_sse_with_watchdog`] both reset an inter-chunk deadline
//!   on every arriving chunk, surfacing a typed [`BodyError::Stalled`]
//!   with partial-byte count instead of a generic 60 s connection-wide
//!   timeout error.
//!
//! ## Schema (the structured fields)
//!
//! Each `emit_http_debug` call produces one `debug!` line with these
//! named fields:
//!
//! | Field             | Type    | Meaning                                                |
//! |-------------------|---------|--------------------------------------------------------|
//! | `stage`           | str     | `stt` / `llm` / `assistant` / `tts` / `wizard`         |
//! | `provider`        | str     | `openrouter` / `openai` / `groq` / `cerebras` / ...    |
//! | `endpoint`        | str     | last URL segment, e.g. `audio/speech`                  |
//! | `status`          | u16     | HTTP status                                            |
//! | `headers_ms`      | u64     | time to response headers                               |
//! | `ttfb_ms`         | u64     | time from headers to first body byte                   |
//! | `body_ms`         | u64     | time from first byte to last byte                      |
//! | `decode_ms`       | u64     | post-processing (WAV strip, JSON parse, etc.)          |
//! | `total_ms`        | u64     | request-start → decode-done                            |
//! | `body_bytes`      | u64     | actual bytes read                                      |
//! | `content_length`  | u64?    | server-advertised length (when present)                |
//! | `chunks`          | u32     | stream chunk count (1 for one-shot bodies)             |
//! | `request_id`      | str?    | upstream `x-request-id` / `request-id`                 |
//! | `attempt`         | u8      | 1 on first try, 2 on retried                           |
//! | `outcome`         | str     | `ok` / `stalled` / `http_error` / `decode_error`       |
//!
//! Reading those values out of a log file:
//!
//! ```text
//! RUST_LOG=info,fono.http=debug fono daemon 2>&1 \
//!   | grep 'fono.http' \
//!   | awk '{ for (i=1;i<=NF;i++) if ($i ~ /^total_ms=/) print $i }'
//! ```

#![forbid(unsafe_code)]

pub mod body;
pub mod emit;
pub mod request_id;
pub mod sse;
pub mod timings;

pub use body::{read_body_with_watchdog, BodyError, BodyStats};
pub use emit::{emit_http_debug, Outcome};
pub use request_id::provider_request_id;
pub use sse::{read_sse_with_watchdog, SseError, SseStats};
pub use timings::RequestTimings;
