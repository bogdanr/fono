// SPDX-License-Identifier: GPL-3.0-only
//! Streaming-STT primitives: shared types, the [`StreamingStt`] trait, and a
//! `LocalAgreement` helper for confirming preview-pane tokens that two
//! consecutive decodes agree on.
//!
//! Per `plans/2026-04-27-fono-interactive-v6.md` R1. Compiled only with the
//! `streaming` cargo feature so slim builds stay slim.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;

/// Lane that produced a [`TranscriptUpdate`].
///
/// * `Preview` — speculative low-latency text from the *fast* lane. May
///   change on every emission. Render in a dimmed colour. Not committed.
/// * `Finalize` — text from the *slow* / dual-pass lane that the harness
///   considers authoritative. Once a `Finalize` update fires for a
///   segment, callers should commit the text and never overwrite it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpdateLane {
    Preview,
    Finalize,
}

/// A single emission from a streaming STT decoder.
///
/// Field semantics chosen so that the v6 plan's R12.5 "confidence-aware
/// finalize-skip" extension can be wired without touching this struct
/// later. `mean_logprob` is plumbed from day one; backends that do not
/// expose token-level logprobs leave it as `None`.
#[derive(Debug, Clone)]
pub struct TranscriptUpdate {
    /// 0-based segment index. A segment is a VAD-bounded chunk of audio;
    /// preview/finalize updates for the same segment share an index.
    pub segment_index: u32,
    /// Which lane produced this update.
    pub lane: UpdateLane,
    /// The text emitted *for this segment only*. Callers concatenate
    /// across segments to assemble the full committed transcript.
    pub text: String,
    /// Detected language (best-effort; copied from the underlying STT).
    pub language: Option<String>,
    /// Optional mean per-token log-probability for the segment, for
    /// future R12.5 wiring. `None` when the backend does not expose it.
    pub mean_logprob: Option<f32>,
    /// Wall-clock instant at which this update was constructed,
    /// captured as a `Duration` since the stream started so the harness
    /// can compute TTFF / TTC without depending on `std::time::Instant`.
    pub elapsed_since_start: Duration,
}

impl TranscriptUpdate {
    pub fn preview(
        segment_index: u32,
        text: impl Into<String>,
        elapsed: Duration,
    ) -> Self {
        Self {
            segment_index,
            lane: UpdateLane::Preview,
            text: text.into(),
            language: None,
            mean_logprob: None,
            elapsed_since_start: elapsed,
        }
    }

    pub fn finalize(
        segment_index: u32,
        text: impl Into<String>,
        elapsed: Duration,
    ) -> Self {
        Self {
            segment_index,
            lane: UpdateLane::Finalize,
            text: text.into(),
            language: None,
            mean_logprob: None,
            elapsed_since_start: elapsed,
        }
    }

    #[must_use]
    pub fn with_language(mut self, lang: Option<String>) -> Self {
        self.language = lang;
        self
    }

    #[must_use]
    pub fn with_mean_logprob(mut self, lp: Option<f32>) -> Self {
        self.mean_logprob = lp;
        self
    }
}

/// Streaming variant of [`crate::SpeechToText`]. Implementations consume a
/// stream of f32 PCM frames at `sample_rate` Hz and yield
/// [`TranscriptUpdate`]s as preview and finalize text become available.
///
/// Streaming variant of [`crate::SpeechToText`]. Implementations consume a
/// stream of [`StreamFrame`]s at `sample_rate` Hz and yield
/// [`TranscriptUpdate`]s as preview and finalize text become available.
///
/// On `StreamFrame::Eof` the implementation MUST emit a final `Finalize`
/// update for any unflushed segment before closing the output stream.
#[async_trait]
pub trait StreamingStt: Send + Sync {
    /// Begin a streaming decode.
    ///
    /// Both streams are `'static` so implementations can move them into
    /// detached background tasks without lifetime juggling. Callers
    /// build their input stream from owned values (e.g. an mpsc
    /// receiver) so this is rarely a constraint in practice.
    async fn stream_transcribe(
        &self,
        frames: BoxStream<'static, StreamFrame>,
        sample_rate: u32,
        lang: Option<String>,
    ) -> Result<BoxStream<'static, TranscriptUpdate>>;

    /// Backend identifier for history / logging.
    fn name(&self) -> &'static str;
}

/// One element of the input stream consumed by [`StreamingStt`]. Defined
/// here (rather than re-exported from `fono-audio`) so `fono-stt` does
/// not pick up an audio dependency.
#[derive(Debug, Clone)]
pub enum StreamFrame {
    /// A chunk of mono f32 PCM at the agreed sample rate.
    Pcm(Vec<f32>),
    /// VAD-driven segment boundary. Triggers a finalize-lane decode on
    /// any pending segment audio.
    SegmentBoundary,
    /// End of input. The implementation MUST emit any pending
    /// `Finalize` update before closing the output stream.
    Eof,
}

// ---------------------------------------------------------------------
// LocalAgreement helper.
// ---------------------------------------------------------------------

/// Tracks the longest common token-prefix between two consecutive decode
/// passes ("local agreement"). Tokens that survive two consecutive passes
/// are considered stable enough for preview commit; tokens that change
/// between passes are flagged as in-flux.
///
/// Plan R1.3.
#[derive(Debug, Default, Clone)]
pub struct LocalAgreement {
    previous: Vec<String>,
    /// Longest token prefix that has been agreed on across all decodes
    /// observed so far. Monotonic — we never revoke a token already
    /// stable.
    stable_prefix: Vec<String>,
}

impl LocalAgreement {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a fresh decode (token list). Returns the stable token-prefix
    /// after this update.
    pub fn observe<I, S>(&mut self, tokens: I) -> &[String]
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let current: Vec<String> = tokens.into_iter().map(Into::into).collect();
        // The new stable prefix is the LCP of the previous decode and
        // the current decode, but only insofar as it extends the
        // currently-stable prefix.
        let lcp = lcp_len(&self.previous, &current);
        if lcp > self.stable_prefix.len() {
            self.stable_prefix = current[..lcp].to_vec();
        }
        self.previous = current;
        &self.stable_prefix
    }

    /// The currently agreed-on token prefix.
    #[must_use]
    pub fn stable(&self) -> &[String] {
        &self.stable_prefix
    }

    /// Tokens past the stable prefix from the most-recent decode (the
    /// "tentative" suffix; render as preview).
    #[must_use]
    pub fn tentative(&self) -> &[String] {
        let n = self.stable_prefix.len();
        if n >= self.previous.len() {
            &[]
        } else {
            &self.previous[n..]
        }
    }

    /// Reset for a new segment; clears all agreement state.
    pub fn reset(&mut self) {
        self.previous.clear();
        self.stable_prefix.clear();
    }
}

fn lcp_len(a: &[String], b: &[String]) -> usize {
    a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_agreement_promotes_lcp_after_two_observations() {
        let mut la = LocalAgreement::new();
        // First decode — nothing stable yet (need two for agreement).
        assert!(la.observe(["hello", "world"]).is_empty());
        // Second decode agrees on full prefix.
        let stable = la.observe(["hello", "world", "today"]);
        assert_eq!(stable, &["hello".to_string(), "world".to_string()]);
        // Tentative is the divergent tail.
        assert_eq!(la.tentative(), &["today".to_string()]);
    }

    #[test]
    fn local_agreement_is_monotonic_under_disagreement() {
        let mut la = LocalAgreement::new();
        la.observe(["the", "quick", "brown"]);
        la.observe(["the", "quick", "brown", "fox"]);
        assert_eq!(la.stable().len(), 3);
        // A regression in the next decode does NOT shrink the stable
        // prefix.
        la.observe(["the", "quack"]);
        assert_eq!(la.stable().len(), 3);
    }

    #[test]
    fn local_agreement_reset_clears_state() {
        let mut la = LocalAgreement::new();
        la.observe(["a", "b"]);
        la.observe(["a", "b", "c"]);
        assert_eq!(la.stable().len(), 2);
        la.reset();
        assert!(la.stable().is_empty());
        assert!(la.tentative().is_empty());
    }

    #[test]
    fn transcript_update_preview_and_finalize_lanes() {
        let p = TranscriptUpdate::preview(0, "hi", Duration::from_millis(120));
        assert_eq!(p.lane, UpdateLane::Preview);
        let f = TranscriptUpdate::finalize(0, "hi.", Duration::from_millis(800));
        assert_eq!(f.lane, UpdateLane::Finalize);
        assert_eq!(f.segment_index, 0);
    }
}
