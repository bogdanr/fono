// SPDX-License-Identifier: GPL-3.0-only
//! Streaming sentence splitter for the assistant's LLM-to-TTS pump.
//!
//! Receives token deltas from a streaming LLM, emits whole sentences
//! ready for [`crate::TextToSpeech::synthesize`]. Time-to-first-audio
//! is bounded by the first sentence's length, so the pump can begin
//! playback well before the model has finished generating.
//!
//! Splitting rules (intentionally conservative for spoken output):
//!
//! * **Boundary** = `.`, `!`, or `?` (runs collapse) followed by
//!   whitespace, end-of-line, or end-of-buffer.
//! * **Min-emit threshold** = `MIN_EMIT_CHARS` non-whitespace chars
//!   *outside code blocks*. Below the threshold a candidate boundary
//!   is treated as an abbreviation / number / bullet (`Mr.`, `e.g.`,
//!   `3.14`, `1.`) and the buffer keeps growing.
//! * **Inline `` ` ` ``-quoted code** suppresses boundary detection
//!   inside the quote, but the surrounding sentence still includes
//!   the inline span — typical for snippets like `` `cargo test` ``
//!   in prose.
//! * **Triple-backtick code fences** are treated as opaque opaque-
//!   to-the-listener regions: any prose before the fence is force-
//!   emitted as a sentence, the fenced content is discarded, and
//!   prose after the fence resumes normal sentence detection.
//! * **Paragraph break** (`\n\n`) outside any code region forces a
//!   flush even without terminal punctuation, so headings / list
//!   items still get spoken.
//! * Sentences are emitted with surrounding whitespace trimmed.

/// Below this many non-whitespace characters of *prose* (excluding
/// code spans), a candidate boundary is treated as an abbreviation /
/// number / bullet and the splitter keeps buffering.
///
/// 24 is tuned to absorb `e.g.` / `i.e.` / `Mr.` followed by their
/// short companion word and still emit on the *real* sentence-final
/// period a few words later. Genuinely short sentences ("Hi.",
/// "Done.") are held until [`SentenceSplitter::flush`] runs at the
/// LLM stream's end — acceptable because they only ever occur as the
/// last sentence in practice.
const MIN_EMIT_CHARS: usize = 24;

/// State for the streaming sentence splitter.
///
/// Pushes are append-only into an internal buffer; emissions consume
/// the prefix up to and including the chosen boundary. Memory is
/// bounded by the longest unsplit sentence + any pending code-fence
/// content.
#[derive(Debug, Default, Clone)]
pub struct SentenceSplitter {
    buf: String,
    /// True while inside a `` ``` ``-fenced code block. Content is
    /// discarded; we never emit fenced text as a "sentence".
    in_code_fence: bool,
}

/// Outcome of one scan over the buffer in non-fence mode.
enum Event {
    /// A complete sentence ends at byte offset `end` (exclusive).
    /// Buffer up to `end` should be drained and trimmed.
    Boundary { end: usize },
    /// A triple-backtick code fence opens at byte offset `at`; its
    /// closing run starts somewhere later (or hasn't arrived yet).
    /// Caller force-emits prose before `at` and switches to fence
    /// mode after dropping the opening run.
    OpenFence { prose_end: usize, drop_to: usize },
    /// Buffer ends in mid-stream content; need more input.
    Pending,
}

impl SentenceSplitter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `delta` to the internal buffer and return zero or more
    /// complete sentences ready for TTS. Trailing partials remain
    /// buffered until a later call (or [`Self::flush`]) consumes them.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        self.buf.push_str(delta);
        self.drain()
    }

    /// Drain the trailing partial when the LLM stream ends. Returns
    /// `None` when nothing meaningful is left (whitespace only,
    /// unclosed code fence, or empty buffer).
    pub fn flush(&mut self) -> Option<String> {
        let tail = std::mem::take(&mut self.buf);
        // An unclosed code fence at flush time means the model ended
        // its output mid-fence — we drop it (the orchestrator
        // clipboard-copies the full text as a fallback).
        if self.in_code_fence {
            self.in_code_fence = false;
            return None;
        }
        let trimmed = tail.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn drain(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        loop {
            if self.in_code_fence {
                if let Some(after) = self.find_fence_close() {
                    self.buf.drain(..after);
                    self.in_code_fence = false;
                    continue;
                }
                // Fence still open — discard what we've buffered for
                // it so far and wait for more input. (A future close
                // will arrive on a later push; until then we don't
                // hold onto the body.)
                self.buf.clear();
                return out;
            }

            match self.next_event() {
                Event::Boundary { end } => {
                    let head = self.buf[..end].trim().to_string();
                    self.buf.drain(..end);
                    if !head.is_empty() {
                        out.push(head);
                    }
                }
                Event::OpenFence { prose_end, drop_to } => {
                    let head = self.buf[..prose_end].trim().to_string();
                    self.buf.drain(..drop_to);
                    self.in_code_fence = true;
                    if !head.is_empty() {
                        out.push(head);
                    }
                }
                Event::Pending => return out,
            }
        }
    }

    /// Walk the buffer outside any fence, looking for the first event
    /// (boundary, fence open, or end-of-buffer).
    fn next_event(&self) -> Event {
        let bytes = self.buf.as_bytes();
        let mut i = 0;
        let mut emit_chars = 0_usize;
        let mut in_inline_code = false;

        while i < bytes.len() {
            let c = bytes[i];

            if c == b'`' {
                // Count consecutive backticks.
                let mut run = 1;
                while i + run < bytes.len() && bytes[i + run] == b'`' {
                    run += 1;
                }
                if run >= 3 {
                    return Event::OpenFence {
                        prose_end: i,
                        drop_to: i + run,
                    };
                }
                if run == 1 {
                    in_inline_code = !in_inline_code;
                }
                // Inline-code chars don't count toward emit_chars
                // (they're typically identifiers; counting them would
                // glue short prose to the next sentence).
                i += run;
                continue;
            }

            // Paragraph-break flush — outside fences only (we already
            // bailed out of fence mode above).
            if c == b'\n'
                && i + 1 < bytes.len()
                && bytes[i + 1] == b'\n'
                && !in_inline_code
            {
                if emit_chars >= MIN_EMIT_CHARS {
                    return Event::Boundary { end: i + 2 };
                }
                i += 2;
                continue;
            }

            if !in_inline_code && matches!(c, b'.' | b'!' | b'?') {
                // Collapse runs of `?!`/`...`.
                let mut j = i + 1;
                while j < bytes.len() && matches!(bytes[j], b'.' | b'!' | b'?') {
                    j += 1;
                }
                // Tolerate one closing quote / paren after the run.
                if j < bytes.len() && matches!(bytes[j], b'"' | b'\'' | b')' | b']' | b'}') {
                    j += 1;
                }
                let after = bytes.get(j).copied();
                let is_boundary = matches!(after, Some(b' ' | b'\t' | b'\n' | b'\r'));
                if is_boundary && emit_chars >= MIN_EMIT_CHARS {
                    let mut end = j;
                    while end < bytes.len()
                        && matches!(bytes[end], b' ' | b'\t' | b'\n' | b'\r')
                    {
                        end += 1;
                    }
                    return Event::Boundary { end };
                }
                i = j;
                continue;
            }

            // Count visible characters towards the threshold. Skip
            // ASCII whitespace; for multi-byte UTF-8 count once per
            // code point (continuation bytes start with 0b10xxxxxx).
            if !in_inline_code
                && !c.is_ascii_whitespace()
                && (c & 0xC0) != 0x80
            {
                emit_chars += 1;
            }
            i += 1;
        }

        Event::Pending
    }

    /// Find the byte offset just past a closing `` ``` `` run inside
    /// the current buffer (when [`Self::in_code_fence`] is true).
    fn find_fence_close(&self) -> Option<usize> {
        let bytes = self.buf.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'`' {
                let mut run = 1;
                while i + run < bytes.len() && bytes[i + run] == b'`' {
                    run += 1;
                }
                if run >= 3 {
                    return Some(i + run);
                }
                i += run;
            } else {
                i += 1;
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split_all(input: &str) -> Vec<String> {
        let mut s = SentenceSplitter::new();
        let mut out = s.push(input);
        if let Some(tail) = s.flush() {
            out.push(tail);
        }
        out
    }

    #[test]
    fn splits_basic_two_sentences() {
        let got = split_all("Hello there friend, how are you today? I am doing fine here.");
        assert_eq!(
            got,
            vec![
                "Hello there friend, how are you today?".to_string(),
                "I am doing fine here.".to_string()
            ]
        );
    }

    #[test]
    fn absorbs_abbreviation_mr() {
        let got = split_all("Mr. Smith called yesterday afternoon about the meeting.");
        assert_eq!(
            got,
            vec!["Mr. Smith called yesterday afternoon about the meeting.".to_string()]
        );
    }

    #[test]
    fn absorbs_abbreviation_eg() {
        let got = split_all("Use a fast model, e.g. haiku, for cleanup tasks.");
        assert_eq!(
            got,
            vec!["Use a fast model, e.g. haiku, for cleanup tasks.".to_string()]
        );
    }

    #[test]
    fn absorbs_decimal_number() {
        let got = split_all("The value is 3.14 today rounded to two digits.");
        assert_eq!(
            got,
            vec!["The value is 3.14 today rounded to two digits.".to_string()]
        );
    }

    #[test]
    fn absorbs_list_bullet() {
        let got = split_all("1. first item that is reasonably long enough to emit.");
        assert_eq!(
            got,
            vec!["1. first item that is reasonably long enough to emit.".to_string()]
        );
    }

    #[test]
    fn skips_triple_backtick_fence() {
        let got = split_all(
            "Here is some code now to look at carefully.\n\n\
             ```\nlet x = 1.0;\nfn foo() {}\n```\n\
             Done reading the snippet so we move on now.",
        );
        assert!(
            got.iter().any(|x| x.contains("Here is some code now")),
            "got: {got:?}"
        );
        assert!(
            got.iter().any(|x| x.contains("Done reading the snippet")),
            "got: {got:?}"
        );
        assert!(
            !got.iter().any(|x| x.contains("let x = 1.0")),
            "code-fence content must not be emitted: {got:?}"
        );
    }

    #[test]
    fn keeps_inline_backtick_code_in_sentence() {
        let got = split_all("Run `cargo test` to verify the changes work correctly.");
        assert_eq!(
            got,
            vec!["Run `cargo test` to verify the changes work correctly.".to_string()]
        );
    }

    #[test]
    fn handles_mid_token_deltas() {
        let mut s = SentenceSplitter::new();
        let a = s.push("Hel");
        let b = s.push("lo there friend, how are");
        let c = s.push(" you doing today my dear?");
        let d = s.push(" I hope you are well at this hour.");
        let mut got: Vec<String> = Vec::new();
        got.extend(a);
        got.extend(b);
        got.extend(c);
        got.extend(d);
        if let Some(tail) = s.flush() {
            got.push(tail);
        }
        assert_eq!(
            got,
            vec![
                "Hello there friend, how are you doing today my dear?".to_string(),
                "I hope you are well at this hour.".to_string()
            ]
        );
    }

    #[test]
    fn flush_returns_none_when_buffer_drained_at_boundary() {
        // Trailing space lets the boundary fire on push; the buffer
        // is empty afterwards so flush() yields None.
        let mut s = SentenceSplitter::new();
        let pushed = s.push("Hello world this is a fine afternoon. ");
        assert_eq!(
            pushed,
            vec!["Hello world this is a fine afternoon.".to_string()]
        );
        assert!(s.flush().is_none());
    }

    #[test]
    fn flush_returns_trailing_partial() {
        let mut s = SentenceSplitter::new();
        let pushed = s.push(
            "First sentence is right here in front of us. And then a partial without ending",
        );
        assert_eq!(
            pushed,
            vec!["First sentence is right here in front of us.".to_string()]
        );
        let tail = s.flush();
        assert_eq!(
            tail,
            Some("And then a partial without ending".to_string())
        );
    }

    #[test]
    fn forced_flush_on_paragraph_break() {
        let got = split_all("# Some Heading That Is Reasonably Long\n\nNext paragraph here, also of decent length.");
        assert_eq!(
            got,
            vec![
                "# Some Heading That Is Reasonably Long".to_string(),
                "Next paragraph here, also of decent length.".to_string()
            ]
        );
    }

    #[test]
    fn collapses_punctuation_runs_into_single_sentence() {
        // "Wait, what?!" alone is below the min-emit threshold so it
        // glues onto the next sentence. Acceptable for spoken output:
        // listener hears one slightly-longer utterance instead of a
        // staccato "Wait, what?!" then "That can't be right."
        let got = split_all("Wait, what?! That can't be right at all.");
        assert_eq!(
            got,
            vec!["Wait, what?! That can't be right at all.".to_string()]
        );
    }

    #[test]
    fn handles_closing_quote_after_period() {
        let got = split_all(
            "She said \"hello there world today.\" Then she walked out without speaking again.",
        );
        assert_eq!(
            got,
            vec![
                "She said \"hello there world today.\"".to_string(),
                "Then she walked out without speaking again.".to_string()
            ]
        );
    }

    #[test]
    fn empty_input_yields_nothing() {
        let mut s = SentenceSplitter::new();
        assert!(s.push("").is_empty());
        assert!(s.flush().is_none());
    }

    #[test]
    fn unterminated_short_input_held_until_flush() {
        let mut s = SentenceSplitter::new();
        let pushed = s.push("Hi there.");
        assert!(pushed.is_empty(), "too short to emit mid-stream: {pushed:?}");
        assert_eq!(s.flush(), Some("Hi there.".to_string()));
    }

    #[test]
    fn unclosed_code_fence_at_flush_drops_silently() {
        // Model ends mid-fence — we drop the partial fence content
        // rather than emit it as a sentence.
        let mut s = SentenceSplitter::new();
        let pushed = s.push("Here is a long enough preamble before the fence.\n\n```\nlet x = 1;");
        assert!(pushed.iter().any(|x| x.contains("preamble")));
        assert!(s.flush().is_none());
    }

    #[test]
    fn code_fence_split_across_pushes() {
        // Triple-backtick spans two pushes — must still drop fence
        // content correctly.
        let mut s = SentenceSplitter::new();
        let mut got = s.push(
            "Here is a long enough preamble before the fence opens.\n\n```\nlet x = 1.0;",
        );
        got.extend(s.push("\nfn foo() {}\n```\nNow some prose after the fence is over."));
        if let Some(tail) = s.flush() {
            got.push(tail);
        }
        assert!(got.iter().any(|x| x.contains("preamble")));
        assert!(got.iter().any(|x| x.contains("after the fence")));
        assert!(!got.iter().any(|x| x.contains("let x = 1.0")));
    }
}
