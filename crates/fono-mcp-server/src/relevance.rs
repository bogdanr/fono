// SPDX-License-Identifier: GPL-3.0-only
//! Relevance filter for `fono.listen`.
//!
//! Two-stage gate that discards utterances which aren't a direct
//! answer to the agent's question (background TV / radio, side
//! conversations, prompt-TTS echo that AEC didn't cancel) so the
//! listen loop keeps waiting instead of returning noise to the agent.
//!
//! Stage 1 — **heuristic** (Slice 3 of plan v7) — always runs when
//! the filter is enabled. Cheap on-device checks: empty transcripts,
//! filler-only utterances, and prompt-echo via Jaro-Winkler
//! similarity. No network calls; safe to run on every utterance.
//!
//! Stage 2 — **LLM classifier** (Slice 4 of plan v7, separate module
//! extension) — opt-in via `[mcp].relevance_filter = "llm"`. Reuses
//! the configured polish backend. Bounded by a hardcoded 1.5 s
//! timeout (see Slice 4) and fails open on timeout / error so the
//! agent never hangs on a slow classifier.
//!
//! ## Privacy
//!
//! Stage 1 runs entirely on the user's machine. Stage 2 sends the
//! transcript + agent-supplied `context` to whichever polish
//! backend the user has configured; users who don't want that ship
//! `relevance_filter = "heuristic"` (the default).

use std::time::Duration;

use fono_polish::{FormatContext, TextFormatter};
use tracing::debug;

use crate::voice_io;

/// Outcome of a single relevance evaluation. The accompanying
/// `IgnoreReason` lets the overlay paint a discriminable label
/// (Slice 5) and the agent surface a debug signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelevanceVerdict {
    /// The transcript is plausibly a direct answer to the agent's
    /// question. Return it to the caller.
    Accept,
    /// The transcript looks like background noise / unrelated
    /// speech / a prompt-TTS echo. Drop it and keep listening.
    Reject(IgnoreReason),
}

/// Why the heuristic / LLM gate rejected a transcript. Surfaces in
/// the overlay's `Ignoring` label and in debug logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoreReason {
    /// Transcript was empty or fewer than 2 alphanumeric characters
    /// after normalisation. STT picked up breath / lip noise.
    TooShort,
    /// Transcript contained only filler tokens (`uh`, `um`, `hmm`,
    /// `mm`, `eh`, …). User cleared their throat but didn't speak.
    FillerOnly,
    /// Transcript closely matches the agent's most recent prompt
    /// (Jaro-Winkler ≥ `ECHO_THRESHOLD`). AEC didn't fully cancel
    /// the TTS playback.
    PromptEcho,
    /// LLM classifier returned `BACKGROUND`. Slice 4.
    Background,
}

/// Jaro-Winkler similarity threshold above which a transcript is
/// considered a prompt-TTS echo. Conservative: a real user repeating
/// the prompt verbatim is rare; setting this too low would reject
/// users who say "yes" to a "yes-or-no" question.
const ECHO_THRESHOLD: f32 = 0.85;

/// Minimum number of alphanumeric characters a transcript must
/// contain to clear the "too short" check. Empirically tuned: 2 is
/// enough to keep "ok" / "no" / "si" / "da" / numerals; 1 would
/// admit single-letter STT artefacts.
const MIN_ALNUM_CHARS: usize = 2;

/// Filler tokens that, on their own, indicate the user didn't
/// actually answer. Match after `voice_io::normalise` so casing /
/// punctuation are already stripped.
const FILLERS: &[&str] = &["uh", "um", "uhm", "hmm", "mm", "mmm", "eh", "ah", "er", "erm"];

/// Hard ceiling on a single LLM relevance classifier call, in
/// milliseconds. Hardcoded by design — the relevance gate exists to
/// keep the listen loop responsive, so it must never block the
/// coding-agent turn on a slow polish backend. Anything above this
/// budget fails open (returns `Accept`) so the user's utterance is
/// still surfaced to the agent.
///
/// 1.5 s leaves room for one round-trip to a cloud polish backend
/// over a healthy network (typical p50 < 800 ms) while still feeling
/// near-instant to the user. Larger budgets would let a stalled
/// classifier silently degrade the listen experience.
pub const RELEVANCE_LLM_TIMEOUT_MS: u64 = 1_500;

/// Run the on-device heuristic gate. Returns
/// [`RelevanceVerdict::Accept`] when no heuristic fires; otherwise
/// the first matching [`IgnoreReason`].
///
/// `prompt` is the text the agent passed to `fono.listen` as the
/// optional `prompt` argument — used only for the echo check. When
/// `None`, the echo check is skipped.
pub fn evaluate_heuristic(transcript: &str, prompt: Option<&str>) -> RelevanceVerdict {
    let norm = voice_io::normalise(transcript);
    let alnum_count = norm.chars().filter(|c| c.is_ascii_alphanumeric()).count();
    if alnum_count < MIN_ALNUM_CHARS {
        return RelevanceVerdict::Reject(IgnoreReason::TooShort);
    }
    if is_filler_only(&norm) {
        return RelevanceVerdict::Reject(IgnoreReason::FillerOnly);
    }
    if let Some(p) = prompt {
        let p_norm = voice_io::normalise(p);
        if !p_norm.is_empty() && jaro_winkler(&norm, &p_norm) >= ECHO_THRESHOLD {
            return RelevanceVerdict::Reject(IgnoreReason::PromptEcho);
        }
    }
    RelevanceVerdict::Accept
}

/// `true` when every whitespace-separated token in `norm` is one of
/// the filler words in [`FILLERS`].
fn is_filler_only(norm: &str) -> bool {
    let mut saw_token = false;
    for tok in norm.split_whitespace() {
        saw_token = true;
        if !FILLERS.contains(&tok) {
            return false;
        }
    }
    saw_token
}

/// Jaro similarity for two normalised strings. Returns a value in
/// `0.0..=1.0` where `1.0` means identical. Pure Rust to keep the
/// dep tree minimal — the strings we compare are short (the agent's
/// prompt and the user's reply, both <2 KB) so the O(n*m) inner loop
/// is cheap.
fn jaro(a: &str, b: &str) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let match_distance = (a.len().max(b.len()) / 2).saturating_sub(1);
    let mut a_matches = vec![false; a.len()];
    let mut b_matches = vec![false; b.len()];
    let mut matches = 0usize;
    for (i, ca) in a.iter().enumerate() {
        let start = i.saturating_sub(match_distance);
        let end = (i + match_distance + 1).min(b.len());
        for j in start..end {
            if b_matches[j] || b[j] != *ca {
                continue;
            }
            a_matches[i] = true;
            b_matches[j] = true;
            matches += 1;
            break;
        }
    }
    if matches == 0 {
        return 0.0;
    }
    let mut transpositions = 0usize;
    let mut k = 0usize;
    for i in 0..a.len() {
        if !a_matches[i] {
            continue;
        }
        while !b_matches[k] {
            k += 1;
        }
        if a[i] != b[k] {
            transpositions += 1;
        }
        k += 1;
    }
    let m = matches as f32;
    (m / a.len() as f32 + m / b.len() as f32 + (m - transpositions as f32 / 2.0) / m) / 3.0
}

/// Jaro-Winkler similarity: Jaro plus a bonus for matching prefixes
/// up to 4 chars. Bonus weight `p = 0.1` is the standard value.
fn jaro_winkler(a: &str, b: &str) -> f32 {
    let j = jaro(a, b);
    let prefix = a.chars().zip(b.chars()).take(4).take_while(|(x, y)| x == y).count();
    j + (prefix as f32) * 0.1 * (1.0 - j)
}

/// Build the system prompt sent to the polish-backed LLM classifier.
/// `context` is the agent-supplied `context` argument from
/// `fono.listen` — typically the question text or a short intent
/// blurb ("asking the user for their favourite colour"). Kept terse
/// so cloud providers with a low `max_tokens` cap on the *prompt*
/// side stay well under their limit.
fn build_classifier_prompt(context: &str) -> String {
    let ctx = context.trim();
    let ctx_block = if ctx.is_empty() {
        String::new()
    } else {
        format!("\n\nAgent context (what the agent is asking for):\n{ctx}")
    };
    format!(
        "You are classifying voice input in a coding-assistant session. The user is wearing a \
         microphone; the agent has just asked them something and is now listening. Decide \
         whether the user's utterance (delimited by <<< >>>) is a direct ANSWER to the agent's \
         question, BACKGROUND speech (radio, TV, side conversation, room noise, or a leaked echo \
         of the agent's own prompt), or you're UNSURE.\n\n\
         Respond with EXACTLY one word, uppercase, no punctuation, no commentary: ANSWER, \
         BACKGROUND, or UNSURE. Any other output is treated as UNSURE.{ctx_block}"
    )
}

/// Parse the polish backend's reply into a verdict. Takes the first
/// whitespace-trimmed token, strips surrounding non-alphabetic
/// punctuation, uppercases it, and matches against the three known
/// labels. `BACKGROUND` rejects; everything else (`ANSWER`,
/// `UNSURE`, parse failure, gibberish) fails open to `Accept` so a
/// misbehaving classifier never strands the user.
fn parse_verdict(text: &str) -> RelevanceVerdict {
    let first = text
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_ascii_alphabetic())
        .to_ascii_uppercase();
    match first.as_str() {
        "BACKGROUND" => RelevanceVerdict::Reject(IgnoreReason::Background),
        _ => RelevanceVerdict::Accept,
    }
}

/// Run the LLM relevance classifier against `transcript`. Wraps the
/// call in a [`RELEVANCE_LLM_TIMEOUT_MS`]-bounded `tokio::time::timeout`;
/// **fails open** on timeout, transport error, or unparseable output.
///
/// Intentionally takes a `&dyn TextFormatter` rather than an
/// `Arc<...>` so unit tests can stub the classifier with a tiny
/// in-memory fake without touching the polish factory.
pub async fn evaluate_llm(
    classifier: &dyn TextFormatter,
    transcript: &str,
    context: &str,
) -> RelevanceVerdict {
    let ctx =
        FormatContext { main_prompt: build_classifier_prompt(context), ..FormatContext::default() };
    let fut = classifier.format(transcript, &ctx);
    match tokio::time::timeout(Duration::from_millis(RELEVANCE_LLM_TIMEOUT_MS), fut).await {
        Ok(Ok(reply)) => {
            let verdict = parse_verdict(&reply);
            debug!(
                target: "fono_mcp_server::relevance",
                reply = %reply.trim(),
                ?verdict,
                "llm classifier verdict",
            );
            verdict
        }
        Ok(Err(e)) => {
            debug!(
                target: "fono_mcp_server::relevance",
                error = %e,
                "llm classifier failed; failing open to Accept",
            );
            RelevanceVerdict::Accept
        }
        Err(_) => {
            debug!(
                target: "fono_mcp_server::relevance",
                timeout_ms = RELEVANCE_LLM_TIMEOUT_MS,
                "llm classifier timed out; failing open to Accept",
            );
            RelevanceVerdict::Accept
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_accepts_substantive_answer() {
        let v = evaluate_heuristic("yes I would like that", Some("Do you want it?"));
        assert_eq!(v, RelevanceVerdict::Accept);
    }

    #[test]
    fn heuristic_rejects_empty_transcript() {
        assert_eq!(evaluate_heuristic("", None), RelevanceVerdict::Reject(IgnoreReason::TooShort));
        assert_eq!(
            evaluate_heuristic("   ", None),
            RelevanceVerdict::Reject(IgnoreReason::TooShort)
        );
    }

    #[test]
    fn heuristic_rejects_single_char_transcript() {
        assert_eq!(evaluate_heuristic("a", None), RelevanceVerdict::Reject(IgnoreReason::TooShort));
        // Punctuation doesn't count towards the alnum budget.
        assert_eq!(
            evaluate_heuristic("a.", None),
            RelevanceVerdict::Reject(IgnoreReason::TooShort)
        );
    }

    #[test]
    fn heuristic_rejects_filler_only() {
        for filler in ["uh", "um", "hmm", "uh um", "mm hmm", "Uh, um."] {
            assert_eq!(
                evaluate_heuristic(filler, None),
                RelevanceVerdict::Reject(IgnoreReason::FillerOnly),
                "filler={filler:?}"
            );
        }
    }

    #[test]
    fn heuristic_accepts_filler_mixed_with_content() {
        assert_eq!(evaluate_heuristic("um yes please", None), RelevanceVerdict::Accept);
    }

    #[test]
    fn heuristic_rejects_prompt_echo() {
        let prompt = "What is your favourite colour?";
        let echo = "what is your favourite colour";
        assert_eq!(
            evaluate_heuristic(echo, Some(prompt)),
            RelevanceVerdict::Reject(IgnoreReason::PromptEcho)
        );
    }

    #[test]
    fn heuristic_accepts_short_answer_to_long_prompt() {
        let prompt = "What is your favourite colour?";
        // Short legitimate answer must not trigger the echo check.
        assert_eq!(evaluate_heuristic("blue", Some(prompt)), RelevanceVerdict::Accept);
    }

    #[test]
    fn jaro_winkler_identity_is_one() {
        assert!((jaro_winkler("hello world", "hello world") - 1.0).abs() < 1e-6);
    }

    #[test]
    fn jaro_winkler_disjoint_is_low() {
        assert!(jaro_winkler("abc", "xyz") < 0.5);
    }

    // ── LLM classifier tests ──────────────────────────────────────────

    use anyhow::Result;
    use async_trait::async_trait;
    use std::time::Duration;

    /// Stub classifier that returns a fixed string after an optional
    /// delay. Used to exercise `evaluate_llm`'s verdict-parsing and
    /// timeout paths without hitting a real polish backend.
    struct StubClassifier {
        reply: String,
        delay: Duration,
    }

    impl StubClassifier {
        fn fast(reply: &str) -> Self {
            Self { reply: reply.to_string(), delay: Duration::from_millis(0) }
        }
        fn slow(reply: &str, delay: Duration) -> Self {
            Self { reply: reply.to_string(), delay }
        }
    }

    #[async_trait]
    impl TextFormatter for StubClassifier {
        async fn format(&self, _raw: &str, _ctx: &FormatContext) -> Result<String> {
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            Ok(self.reply.clone())
        }
        fn name(&self) -> &'static str {
            "stub"
        }
    }

    /// Erroring stub — exercises the `Err` fail-open path.
    struct ErrClassifier;
    #[async_trait]
    impl TextFormatter for ErrClassifier {
        async fn format(&self, _raw: &str, _ctx: &FormatContext) -> Result<String> {
            Err(anyhow::anyhow!("stub failure"))
        }
        fn name(&self) -> &'static str {
            "err"
        }
    }

    #[test]
    fn parse_verdict_accepts_answer_uppercase() {
        assert_eq!(parse_verdict("ANSWER"), RelevanceVerdict::Accept);
    }

    #[test]
    fn parse_verdict_rejects_background_with_punctuation() {
        assert_eq!(
            parse_verdict("BACKGROUND."),
            RelevanceVerdict::Reject(IgnoreReason::Background)
        );
    }

    #[test]
    fn parse_verdict_unsure_fails_open() {
        assert_eq!(parse_verdict("UNSURE"), RelevanceVerdict::Accept);
    }

    #[test]
    fn parse_verdict_case_insensitive_and_takes_first_token() {
        assert_eq!(
            parse_verdict("  background — radio in kitchen"),
            RelevanceVerdict::Reject(IgnoreReason::Background)
        );
        assert_eq!(parse_verdict("answer to your question"), RelevanceVerdict::Accept);
    }

    #[test]
    fn parse_verdict_gibberish_fails_open() {
        assert_eq!(parse_verdict(""), RelevanceVerdict::Accept);
        assert_eq!(parse_verdict("\"\""), RelevanceVerdict::Accept);
        assert_eq!(parse_verdict("???"), RelevanceVerdict::Accept);
        assert_eq!(parse_verdict("maybe"), RelevanceVerdict::Accept);
    }

    #[tokio::test]
    async fn evaluate_llm_accept_on_answer() {
        let stub = StubClassifier::fast("ANSWER");
        assert_eq!(
            evaluate_llm(&stub, "the sky is blue", "asking about colour").await,
            RelevanceVerdict::Accept
        );
    }

    #[tokio::test]
    async fn evaluate_llm_reject_on_background() {
        let stub = StubClassifier::fast("BACKGROUND");
        assert_eq!(
            evaluate_llm(&stub, "and now the weather", "asking about colour").await,
            RelevanceVerdict::Reject(IgnoreReason::Background)
        );
    }

    #[tokio::test]
    async fn evaluate_llm_unsure_fails_open() {
        let stub = StubClassifier::fast("UNSURE");
        assert_eq!(
            evaluate_llm(&stub, "hmm well maybe", "asking about colour").await,
            RelevanceVerdict::Accept
        );
    }

    #[tokio::test]
    async fn evaluate_llm_err_fails_open() {
        assert_eq!(
            evaluate_llm(&ErrClassifier, "yes", "asking about colour").await,
            RelevanceVerdict::Accept
        );
    }

    #[tokio::test(start_paused = true)]
    async fn evaluate_llm_timeout_fails_open() {
        // Slow stub: 5 s reply. Timeout is 1.5 s; tokio's paused
        // clock auto-advances when every task is timer-blocked, so
        // the test resolves in zero real wall-clock time without
        // needing to manually pump the clock.
        let stub = StubClassifier::slow("BACKGROUND", Duration::from_millis(5_000));
        let verdict = evaluate_llm(&stub, "anything", "context").await;
        assert_eq!(verdict, RelevanceVerdict::Accept);
    }

    #[test]
    fn classifier_prompt_includes_context_when_present() {
        let p = build_classifier_prompt("favourite colour");
        assert!(p.contains("favourite colour"));
        assert!(p.contains("ANSWER"));
        assert!(p.contains("BACKGROUND"));
    }

    #[test]
    fn classifier_prompt_handles_empty_context() {
        let p = build_classifier_prompt("");
        assert!(p.contains("ANSWER"));
        assert!(!p.contains("Agent context"));
    }
}
