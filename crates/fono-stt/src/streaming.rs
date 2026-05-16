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
    pub fn preview(segment_index: u32, text: impl Into<String>, elapsed: Duration) -> Self {
        Self {
            segment_index,
            lane: UpdateLane::Preview,
            text: text.into(),
            language: None,
            mean_logprob: None,
            elapsed_since_start: elapsed,
        }
    }

    pub fn finalize(segment_index: u32, text: impl Into<String>, elapsed: Duration) -> Self {
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

    /// True for backends that run entirely on the local machine.
    /// Mirror of [`crate::SpeechToText::is_local`]; see that doc
    /// comment for the orchestrator-side rationale (drives the
    /// post-release "polishing" overlay animation gate).
    fn is_local(&self) -> bool {
        false
    }
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

/// Phrases Whisper-family STT (Groq, OpenAI, etc.) tends to hallucinate
/// at the trailing edge of streaming finalize text after the user stops
/// talking. Kept narrow on purpose — only phrases we've actually
/// observed leak through the per-backend confidence filter belong here.
/// Add a phrase only after the user reports it; speculative entries
/// risk eating legitimate content.
///
/// Order matters: longer phrases come **first** so a more specific
/// match wins over a shorter substring. In particular `"thank you"`
/// must precede `"you"` — otherwise the bare `"you"` entry would peel
/// off the tail of a real "Thank you" before the longer phrase ever
/// gets checked, leaving a stranded "thank" in the committed text.
///
/// Trade-off note for `"you"`: this is a much more common legitimate
/// English closer than `"thank you"` or `"bye"`. Trailing uses like
/// "I'll send it to you" will be stripped to "I'll send it to". The
/// per-segment confidence filter (Groq) handles the silence-tail
/// "You" hallucination on its own; this entry is the fallback for
/// when Groq returns "You" with normal-looking scores.
const TRAILING_HALLUCINATIONS: &[&str] = &["thank you", "bye", "you"];

/// Strip Whisper-style closer phrases (see [`TRAILING_HALLUCINATIONS`])
/// from the *trailing tail* of a streaming finalize text.
///
/// Properties:
/// - **Tail-anchored**: a phrase is stripped only when it's at the end
///   (after trimming trailing punctuation/whitespace), so legitimate
///   uses like `"Thank you for the report"` survive.
/// - **Word-boundary**: the character preceding the matched tail must
///   not be a letter — guards against false matches inside compounds.
/// - **Looped**: handles repeated hallucinations like
///   `"thank you, thank you, thank you"`.
/// - **Case-insensitive** for ASCII phrases.
/// - **Idempotent** — if no phrase matches, the original text is
///   returned unchanged (including any user-typed trailing
///   punctuation).
///
/// Universal across streaming providers: invoked from the single
/// consumer chokepoint (`apply_update` in `crates/fono/src/live.rs`)
/// so any current or future streaming backend is covered without
/// per-backend wiring.
#[must_use]
pub fn strip_trailing_hallucinations(text: &str) -> String {
    let punct = |c: char| c.is_whitespace() || matches!(c, '.' | ',' | '!' | '?' | ';' | ':');
    let mut s = text.to_string();
    let mut changed = false;
    loop {
        let trimmed_len = s.trim_end_matches(punct).len();
        let mut matched_at: Option<usize> = None;
        for phrase in TRAILING_HALLUCINATIONS {
            if trimmed_len < phrase.len() {
                continue;
            }
            let tail_start = trimmed_len - phrase.len();
            if !s.is_char_boundary(tail_start) {
                continue;
            }
            let tail = &s[tail_start..trimmed_len];
            if !tail.eq_ignore_ascii_case(phrase) {
                continue;
            }
            let preceded_by_letter =
                tail_start > 0 && s[..tail_start].chars().last().is_some_and(char::is_alphabetic);
            if preceded_by_letter {
                continue;
            }
            matched_at = Some(tail_start);
            break;
        }
        match matched_at {
            Some(cut) => {
                s.truncate(cut);
                changed = true;
            }
            None => break,
        }
    }
    if !changed {
        return text.to_string();
    }
    let cleaned = s.trim_end_matches(punct).to_string();
    tracing::info!(
        "streaming finalize: stripped trailing hallucination from {:?} -> {:?}",
        text,
        cleaned,
    );
    cleaned
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

    #[test]
    fn strip_trailing_thank_you_after_real_speech() {
        assert_eq!(
            strip_trailing_hallucinations("Send him the report. Thank you."),
            "Send him the report"
        );
    }

    #[test]
    fn strip_trailing_thank_you_when_only_phrase() {
        assert_eq!(strip_trailing_hallucinations("Thank you."), "");
        assert_eq!(strip_trailing_hallucinations("thank you"), "");
    }

    #[test]
    fn strip_trailing_thank_you_loops_on_repeats() {
        assert_eq!(
            strip_trailing_hallucinations("Send the email, thank you, thank you, thank you."),
            "Send the email"
        );
    }

    #[test]
    fn keeps_thank_you_when_not_trailing() {
        // Real "thank you" mid-sentence must survive.
        assert_eq!(
            strip_trailing_hallucinations("Thank you for the report"),
            "Thank you for the report"
        );
    }

    #[test]
    fn keeps_thank_you_with_trailing_real_speech() {
        // "Thank you" is at the start, not the trailing tail.
        assert_eq!(
            strip_trailing_hallucinations("Thank you for the report."),
            "Thank you for the report."
        );
    }

    #[test]
    fn case_insensitive_match() {
        assert_eq!(strip_trailing_hallucinations("All done. THANK YOU."), "All done");
        assert_eq!(strip_trailing_hallucinations("Filed it. tHaNk YoU"), "Filed it");
    }

    #[test]
    fn no_change_returns_input_verbatim_with_punct() {
        // Idempotency: when no phrase matches, the original trailing
        // punctuation must be preserved. Important so this filter
        // doesn't silently strip the user's sentence-ending period.
        assert_eq!(
            strip_trailing_hallucinations("Just a normal sentence."),
            "Just a normal sentence."
        );
    }

    #[test]
    fn does_not_match_partial_word() {
        // Word-boundary check: phrases appearing as the *suffix* of a
        // longer word (no space before the match) must not be
        // stripped. "Bayou" ends in the letters "you" but the
        // preceding 'a' is alphabetic, so the boundary check rejects
        // the match.
        assert_eq!(strip_trailing_hallucinations("Bayou"), "Bayou");
        assert_eq!(strip_trailing_hallucinations("On the bayou"), "On the bayou");
    }

    #[test]
    fn handles_empty_and_whitespace() {
        assert_eq!(strip_trailing_hallucinations(""), "");
        assert_eq!(strip_trailing_hallucinations("   "), "   ");
    }

    #[test]
    fn handles_unicode_text_around_phrase() {
        // Multi-byte characters before the trailing English
        // hallucination — boundary checks must not panic.
        assert_eq!(
            strip_trailing_hallucinations("Mulțumesc pentru raport. Thank you."),
            "Mulțumesc pentru raport"
        );
        // Non-English content alone — no match, returned unchanged.
        assert_eq!(strip_trailing_hallucinations("Mulțumesc."), "Mulțumesc.");
    }

    #[test]
    fn strip_trailing_bye_after_real_speech() {
        assert_eq!(
            strip_trailing_hallucinations("Send him the report. Bye."),
            "Send him the report"
        );
    }

    #[test]
    fn strip_trailing_bye_when_only_phrase() {
        assert_eq!(strip_trailing_hallucinations("Bye."), "");
        assert_eq!(strip_trailing_hallucinations("BYE!"), "");
        assert_eq!(strip_trailing_hallucinations("bye"), "");
    }

    #[test]
    fn strip_trailing_bye_loops_with_thank_you() {
        // Mixed closer hallucinations — both kinds peeled off in order.
        assert_eq!(
            strip_trailing_hallucinations("Filed the doc. Thank you. Bye."),
            "Filed the doc"
        );
        assert_eq!(
            strip_trailing_hallucinations("Filed the doc. Bye. Thank you."),
            "Filed the doc"
        );
    }

    #[test]
    fn keeps_goodbye_intact() {
        // Word-boundary check: "goodbye" must not be stripped to "good".
        assert_eq!(strip_trailing_hallucinations("She said goodbye."), "She said goodbye.");
        assert_eq!(strip_trailing_hallucinations("goodbye"), "goodbye");
    }

    #[test]
    fn keeps_bye_when_not_trailing() {
        // "bye" mid-sentence must survive.
        assert_eq!(strip_trailing_hallucinations("He said bye to her"), "He said bye to her");
    }

    #[test]
    fn strip_trailing_you_after_real_speech() {
        // The "You." closer Whisper emits on silence — fallback for
        // when Groq returns it with normal-looking scores.
        assert_eq!(strip_trailing_hallucinations("So that's the plan. You."), "So that's the plan");
    }

    #[test]
    fn strip_trailing_you_when_only_phrase() {
        assert_eq!(strip_trailing_hallucinations("You."), "");
        assert_eq!(strip_trailing_hallucinations("you"), "");
        assert_eq!(strip_trailing_hallucinations("YOU!"), "");
    }

    #[test]
    fn thank_you_wins_over_bare_you_entry() {
        // Order in TRAILING_HALLUCINATIONS matters: "thank you" must
        // strip as a whole rather than leaving a stranded "thank".
        assert_eq!(strip_trailing_hallucinations("Filed the doc. Thank you."), "Filed the doc");
    }

    #[test]
    fn keeps_pronoun_inside_word() {
        // Word-boundary check — "your"/"young" must not be stripped.
        assert_eq!(strip_trailing_hallucinations("It's your turn"), "It's your turn");
        assert_eq!(strip_trailing_hallucinations("They are young"), "They are young");
    }

    #[test]
    fn known_false_positive_legitimate_trailing_you_gets_stripped() {
        // Documents the known trade-off: a legitimate sentence ending
        // with "you" is indistinguishable from the hallucination at
        // the text level. Per-segment confidence usually catches the
        // hallucination first; this entry only fires when Groq
        // returned "you" with normal scores. If false positives
        // become annoying in practice, reconsider the entry.
        assert_eq!(strip_trailing_hallucinations("I'll send it to you"), "I'll send it to");
        assert_eq!(strip_trailing_hallucinations("What about you?"), "What about");
    }
}
