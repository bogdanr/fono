// SPDX-License-Identifier: GPL-3.0-only
//! Single chokepoint for emitting the structured `fono.http` log
//! line. All consumers funnel through [`emit_http_debug`] so the
//! schema cannot drift.

use tracing::debug;

use crate::timings::RequestTimings;

/// Outcome of an instrumented HTTP request — drives the `outcome`
/// field of the structured log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 2xx + body fully drained + decoded successfully.
    Ok,
    /// Body read aborted by the inter-chunk watchdog.
    Stalled,
    /// HTTP status was not 2xx.
    HttpError,
    /// Body bytes arrived but decoding (WAV strip, JSON parse, ...)
    /// failed.
    DecodeError,
    /// `send().await` itself failed — DNS / connect / TLS error
    /// before any response was observed.
    ConnectError,
    /// Mid-stream transport failure during body read.
    TransportError,
}

impl Outcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Stalled => "stalled",
            Self::HttpError => "http_error",
            Self::DecodeError => "decode_error",
            Self::ConnectError => "connect_error",
            Self::TransportError => "transport_error",
        }
    }
}

/// Emit one `debug!` line under `target: "fono.http"` with the full
/// per-request schema. Cheap when filtered out (tracing skips field
/// formatting) and never allocates strings on the hot path beyond
/// what the caller passes in.
///
/// `request_id` is the upstream provider's correlation id (typically
/// obtained via [`crate::provider_request_id`]). Pass `"<none>"` if
/// the response carried none — keeps grepping single-token rather
/// than absence-detecting.
#[allow(clippy::too_many_arguments)]
pub fn emit_http_debug(
    stage: &'static str,
    provider: &str,
    endpoint: &str,
    status: u16,
    timings: &RequestTimings,
    body_bytes: u64,
    content_length: Option<u64>,
    chunks: u32,
    request_id: &str,
    attempt: u8,
    outcome: Outcome,
) {
    debug!(
        target: "fono.http",
        stage,
        provider = provider,
        endpoint = endpoint,
        status,
        headers_ms = timings.headers_ms(),
        ttfb_ms = timings.ttfb_ms(),
        body_ms = timings.body_ms(),
        decode_ms = timings.decode_ms(),
        total_ms = timings.total_ms(),
        body_bytes,
        content_length = content_length.unwrap_or(0),
        chunks,
        request_id = request_id,
        attempt,
        outcome = outcome.as_str(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_strings_are_stable() {
        // The grep targets in user runbooks depend on these literals.
        assert_eq!(Outcome::Ok.as_str(), "ok");
        assert_eq!(Outcome::Stalled.as_str(), "stalled");
        assert_eq!(Outcome::HttpError.as_str(), "http_error");
        assert_eq!(Outcome::DecodeError.as_str(), "decode_error");
        assert_eq!(Outcome::ConnectError.as_str(), "connect_error");
        assert_eq!(Outcome::TransportError.as_str(), "transport_error");
    }
}
