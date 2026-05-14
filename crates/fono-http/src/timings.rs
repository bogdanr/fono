// SPDX-License-Identifier: GPL-3.0-only
//! Per-request timing instrumentation.
//!
//! All times are captured via [`std::time::Instant::now`], which is
//! single-digit nanoseconds on every platform Fono ships on. The
//! struct is zero-allocation; field-formatting cost is paid only when
//! a subscriber is actually listening at the `fono.http=debug` target.

use std::time::Instant;

/// Five-checkpoint stopwatch used to attribute latency to the
/// individual stages of an HTTP round-trip.
///
/// Order of marks is request-start → headers → first-body-byte →
/// body-done → decode-done. Skipped stages stay `None` and serialise
/// as `0`, which is fine for `tracing`'s integer fields.
#[derive(Debug, Clone, Copy)]
pub struct RequestTimings {
    /// Captured at construction — request kickoff (right before
    /// `client.request().send().await`).
    pub start: Instant,
    /// Captured the instant `send().await` returns. Difference from
    /// `start` is "time to response headers".
    pub headers: Option<Instant>,
    /// Captured when the first byte of the response body arrives —
    /// distinct from `headers` because most providers send headers
    /// immediately and stream the body afterwards.
    pub first_byte: Option<Instant>,
    /// Captured when the full response body has been drained.
    pub body_done: Option<Instant>,
    /// Captured after any post-processing (WAV header strip, JSON
    /// parse, base64 decode, ...) completes.
    pub decode_done: Option<Instant>,
}

impl RequestTimings {
    /// Start the stopwatch. Call immediately before `send().await`.
    #[must_use]
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
            headers: None,
            first_byte: None,
            body_done: None,
            decode_done: None,
        }
    }

    /// Mark the moment response headers arrived (after `send().await`
    /// returns).
    pub fn mark_headers(&mut self) {
        self.headers = Some(Instant::now());
    }

    /// Mark the moment the first response-body byte arrived.
    pub fn mark_first_byte(&mut self) {
        self.first_byte = Some(Instant::now());
    }

    /// Mark the moment the last response-body byte arrived.
    pub fn mark_body_done(&mut self) {
        self.body_done = Some(Instant::now());
    }

    /// Mark the moment post-processing (decode/parse) completed.
    pub fn mark_decode_done(&mut self) {
        self.decode_done = Some(Instant::now());
    }

    /// Milliseconds from `start` to `headers` (0 if unmarked).
    #[must_use]
    pub fn headers_ms(&self) -> u64 {
        self.headers
            .map(|h| h.duration_since(self.start).as_millis() as u64)
            .unwrap_or(0)
    }

    /// Milliseconds from `headers` to `first_byte` (0 if either unmarked).
    #[must_use]
    pub fn ttfb_ms(&self) -> u64 {
        match (self.headers, self.first_byte) {
            (Some(h), Some(b)) => b.duration_since(h).as_millis() as u64,
            _ => 0,
        }
    }

    /// Milliseconds from `first_byte` to `body_done` (0 if either unmarked).
    #[must_use]
    pub fn body_ms(&self) -> u64 {
        match (self.first_byte, self.body_done) {
            (Some(b), Some(d)) => d.duration_since(b).as_millis() as u64,
            _ => 0,
        }
    }

    /// Milliseconds from `body_done` to `decode_done` (0 if either unmarked).
    #[must_use]
    pub fn decode_ms(&self) -> u64 {
        match (self.body_done, self.decode_done) {
            (Some(b), Some(d)) => d.duration_since(b).as_millis() as u64,
            _ => 0,
        }
    }

    /// Milliseconds from `start` to whichever of `decode_done` /
    /// `body_done` / `headers` is latest. Convenience for one-glance
    /// summarisation.
    #[must_use]
    pub fn total_ms(&self) -> u64 {
        let last = self
            .decode_done
            .or(self.body_done)
            .or(self.first_byte)
            .or(self.headers)
            .unwrap_or(self.start);
        last.duration_since(self.start).as_millis() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmarked_stages_report_zero() {
        let t = RequestTimings::start();
        assert_eq!(t.headers_ms(), 0);
        assert_eq!(t.ttfb_ms(), 0);
        assert_eq!(t.body_ms(), 0);
        assert_eq!(t.decode_ms(), 0);
    }

    #[test]
    fn total_ms_picks_latest_set() {
        let mut t = RequestTimings::start();
        std::thread::sleep(std::time::Duration::from_millis(5));
        t.mark_headers();
        std::thread::sleep(std::time::Duration::from_millis(5));
        t.mark_first_byte();
        std::thread::sleep(std::time::Duration::from_millis(5));
        t.mark_body_done();
        assert!(t.total_ms() >= 15, "total_ms = {}", t.total_ms());
        // decode is unset → total still reflects body_done
        assert_eq!(t.decode_ms(), 0);
    }

    #[test]
    fn stage_deltas_are_pairwise_consistent() {
        let mut t = RequestTimings::start();
        std::thread::sleep(std::time::Duration::from_millis(3));
        t.mark_headers();
        std::thread::sleep(std::time::Duration::from_millis(3));
        t.mark_first_byte();
        std::thread::sleep(std::time::Duration::from_millis(3));
        t.mark_body_done();
        std::thread::sleep(std::time::Duration::from_millis(3));
        t.mark_decode_done();
        let sum = t.headers_ms() + t.ttfb_ms() + t.body_ms() + t.decode_ms();
        // Total should equal the sum of the stages within 2 ms slack
        // (instant-to-instant arithmetic is exact; the slack is for
        // millisecond truncation across four boundaries).
        let total = t.total_ms();
        assert!(total.abs_diff(sum) <= 2, "total {total} vs sum {sum}");
    }
}
