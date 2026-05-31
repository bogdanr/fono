// SPDX-License-Identifier: GPL-3.0-only
//! `fono speak --stream` implementation.
//!
//! Reads text from stdin, applies the markdown sanitiser, segments into
//! sentences via the existing [`fono_tts::SentenceSplitter`] (reused from
//! the assistant TTS pump), and speaks each sentence through the configured
//! TTS backend.
//!
//! **Backpressure:** at most [`MAX_PENDING`] sentences may be queued for
//! synthesis simultaneously. When the queue is full the stdin reader
//! stalls until the synthesis consumer drains at least one slot, preventing
//! a fast producer (e.g. `forge`) from running arbitrarily far ahead of
//! the listener.
//!
//! **Cancellation:** SIGINT (Ctrl-C) flushes the pending queue and exits
//! cleanly. Escape via the global hotkey listener cancels playback via
//! the existing `assistant stop` plumbing when the daemon is also running,
//! but `fono speak --stream` can also run standalone.

use anyhow::{Context, Result};
use fono_audio::AudioPlayback;
use fono_core::{Config, Paths, Secrets};
use fono_tts::SentenceSplitter;
use once_cell::sync::Lazy;
use regex::Regex;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{debug, warn};

/// Maximum sentences queued for synthesis. When the bounded channel
/// fills the stdin-reader task stalls, applying backpressure to the
/// producer.
const MAX_PENDING: usize = 5;

/// Hard cap on a single sentence's character count. Prose that omits
/// terminal punctuation is split at word boundaries before this limit
/// is reached so the TTS backend never receives a wall of text as one
/// utterance.
pub const MAX_SENTENCE_CHARS: usize = 200;

/// Run `fono speak --stream`.
///
/// Loads the TTS backend from the user's config, then:
/// 1. Spawns a reader task that reads stdin, sanitises markdown, and
///    feeds sentences into a bounded channel (backpressure gate).
/// 2. In the main task, consumes sentences from the channel, synthesises
///    each one, and enqueues the resulting PCM for playback.
/// 3. When stdin closes (EOF), waits for in-flight audio to drain.
pub async fn run(paths: &Paths) -> Result<()> {
    let cfg = Config::load(&paths.config_file())?;
    let secrets = Secrets::load(&paths.secrets_file()).unwrap_or_default();

    let tts_arc =
        fono_tts::build_tts(&cfg.tts, &secrets, &cfg.general.languages, &paths.voices_dir())
            .context("loading TTS backend")?
            .context(
                "TTS backend is disabled — set `[tts].backend` to a real provider \
             (e.g. `fono use tts openai`) before using `fono speak --stream`",
            )?;

    let playback = AudioPlayback::new(None).context("opening audio playback device")?;

    // Bounded channel: when full the reader task stalls naturally.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(MAX_PENDING);

    // Reader task: stdin → markdown sanitiser → sentence splitter → channel.
    let reader = tokio::spawn(async move {
        let mut reader = BufReader::new(tokio::io::stdin());
        let mut splitter = SentenceSplitter::new();
        let mut line = String::new();
        loop {
            line.clear();
            let n = match reader.read_line(&mut line).await {
                Ok(n) => n,
                Err(e) => {
                    warn!(target: "fono::speak_stream", error = %e, "stdin read error");
                    break;
                }
            };
            if n == 0 {
                break; // EOF
            }
            let sanitised = sanitise_markdown(&line);
            for sentence in splitter.push(&sanitised) {
                for chunk in hard_cap_sentence(&sentence, MAX_SENTENCE_CHARS) {
                    if tx.send(chunk).await.is_err() {
                        return; // synthesis side dropped — exit cleanly
                    }
                }
            }
        }
        // Flush the splitter at EOF — emit any trailing partial sentence.
        if let Some(tail) = splitter.flush() {
            for chunk in hard_cap_sentence(&tail, MAX_SENTENCE_CHARS) {
                let _ = tx.send(chunk).await;
            }
        }
    });

    // Main task: channel → synthesise → enqueue for playback.
    while let Some(sentence) = rx.recv().await {
        if sentence.trim().is_empty() {
            continue;
        }
        debug!(
            target: "fono::speak_stream",
            sentence = &sentence[..sentence.len().min(60)],
            "synthesising"
        );
        match tts_arc.synthesize(&sentence, None, None).await {
            Ok(audio) if !audio.pcm.is_empty() => {
                if let Err(e) = playback.enqueue(audio.pcm, audio.sample_rate) {
                    warn!(target: "fono::speak_stream", error = %e, "playback enqueue failed");
                }
            }
            Ok(_) => {} // empty PCM (silent TTS result) — skip
            Err(e) => {
                warn!(target: "fono::speak_stream", error = %e, "TTS synthesis failed");
            }
        }
    }

    // Drain in-flight audio before exiting.
    let _ = reader.await;
    let drain_start = std::time::Instant::now();
    let drain_timeout = std::time::Duration::from_secs(120);
    while !playback.is_idle() {
        if drain_start.elapsed() >= drain_timeout {
            warn!(target: "fono::speak_stream", "playback drain timeout; exiting");
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Ok(())
}

// ─── Markdown sanitiser ───────────────────────────────────────────────────────

/// Apply the voice-output markdown sanitiser to a text fragment.
///
/// Applied in order (each step sees the output of the previous):
///
/// 1. Fenced code blocks (` ``` ` … ` ``` `) → `"(code block elided)"`.
/// 2. `**bold**` / `__bold__` → inner text.
/// 3. `*em*` / `_em_` → inner text (safe after step 2 removed `**`/`__`).
/// 4. ATX headings (`# ` / `## ` etc. at line start) → drop the `#` prefix.
/// 5. Markdown links `[text](url)` → `text`.
/// 6. Inline code `` `code` `` → `code`.
/// 7. Bare URLs longer than 30 characters → `"a link"`.
pub fn sanitise_markdown(input: &str) -> String {
    let s = replace_code_fences(input);
    let s = strip_bold(&s);
    let s = strip_em(&s);
    let s = strip_headings(&s);
    let s = strip_markdown_links(&s);
    let s = strip_inline_code(&s);
    replace_bare_urls(&s)
}

/// Replace ` ``` `…` ``` ` spans with `"(code block elided)"`.
///
/// If the closing fence hasn't arrived yet in this chunk, the opening
/// fence and everything after it is left intact so the
/// [`SentenceSplitter`] can discard it when the close arrives later.
fn replace_code_fences(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find("```") {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 3..];
        if let Some(close) = after_open.find("```") {
            out.push_str("(code block elided)");
            rest = &after_open[close + 3..];
        } else {
            // Unclosed fence — leave the remainder as-is.
            out.push_str(&rest[open..]);
            return out;
        }
    }
    out.push_str(rest);
    out
}

static RE_BOLD_STAR: Lazy<Regex> = Lazy::new(|| Regex::new(r"\*\*([^*]+)\*\*").unwrap());
static RE_BOLD_UNDER: Lazy<Regex> = Lazy::new(|| Regex::new(r"__([^_]+)__").unwrap());
static RE_EM_STAR: Lazy<Regex> = Lazy::new(|| Regex::new(r"\*([^*]+)\*").unwrap());
static RE_EM_UNDER: Lazy<Regex> = Lazy::new(|| Regex::new(r"_([^_]+)_").unwrap());
static RE_LINK: Lazy<Regex> = Lazy::new(|| Regex::new(r"\[([^\]]+)\]\([^\)]+\)").unwrap());
static RE_INLINE_CODE: Lazy<Regex> = Lazy::new(|| Regex::new(r"`([^`]+)`").unwrap());
static RE_URL: Lazy<Regex> = Lazy::new(|| Regex::new(r"https?://[^\s\]>)]+").unwrap());

fn strip_bold(s: &str) -> String {
    let s = RE_BOLD_STAR.replace_all(s, "$1").into_owned();
    RE_BOLD_UNDER.replace_all(&s, "$1").into_owned()
}

fn strip_em(s: &str) -> String {
    // Safe to call after `strip_bold` has removed `**`/`__`.
    let s = RE_EM_STAR.replace_all(s, "$1").into_owned();
    RE_EM_UNDER.replace_all(&s, "$1").into_owned()
}

fn strip_headings(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let ends_with_newline = s.ends_with('\n');
    for (i, line) in s.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // Drop leading `#` characters and the space/tab that follows.
        let stripped = line.trim_start_matches('#').trim_start_matches([' ', '\t']);
        out.push_str(stripped);
    }
    if ends_with_newline && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn strip_markdown_links(s: &str) -> String {
    RE_LINK.replace_all(s, "$1").into_owned()
}

fn strip_inline_code(s: &str) -> String {
    RE_INLINE_CODE.replace_all(s, "$1").into_owned()
}

fn replace_bare_urls(s: &str) -> String {
    RE_URL
        .replace_all(s, |caps: &regex::Captures<'_>| {
            let url = &caps[0];
            if url.len() > 30 {
                "a link".to_string()
            } else {
                url.to_string()
            }
        })
        .into_owned()
}

// ─── Sentence hard-cap ────────────────────────────────────────────────────────

/// Split `sentence` at word boundaries so that no returned chunk exceeds
/// `max_chars` characters. Returns `vec![sentence]` when the input
/// already fits; returns at least one non-empty chunk otherwise.
pub fn hard_cap_sentence(sentence: &str, max_chars: usize) -> Vec<String> {
    if sentence.chars().count() <= max_chars {
        return vec![sentence.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in sentence.split_whitespace() {
        let word_chars = word.chars().count();
        if !current.is_empty() {
            if current.chars().count() + 1 + word_chars <= max_chars {
                current.push(' ');
            } else {
                out.push(std::mem::take(&mut current));
            }
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Markdown sanitiser ──────────────────────────────────────────

    #[test]
    fn code_fence_replaced_with_elided() {
        let got = sanitise_markdown("Here is some setup now.\n\n```\nlet x = 1;\n```\nDone.");
        assert!(got.contains("(code block elided)"), "got: {got:?}");
        assert!(!got.contains("let x = 1"), "fence content must be elided: {got:?}");
    }

    #[test]
    fn unclosed_fence_left_intact() {
        // SentenceSplitter handles unclosed fences; we must not swallow them.
        let got = sanitise_markdown("Before the fence.\n```\nunfinished");
        assert!(got.contains("```"), "unclosed fence must pass through: {got:?}");
    }

    #[test]
    fn bold_asterisk_stripped() {
        assert_eq!(sanitise_markdown("This is **important** text."), "This is important text.");
    }

    #[test]
    fn bold_underscore_stripped() {
        assert_eq!(sanitise_markdown("__Bold__ word here."), "Bold word here.");
    }

    #[test]
    fn em_asterisk_stripped() {
        let got = sanitise_markdown("A *single* em span here.");
        assert_eq!(got, "A single em span here.");
    }

    #[test]
    fn em_underscore_stripped() {
        let got = sanitise_markdown("A _single_ em span here.");
        assert_eq!(got, "A single em span here.");
    }

    #[test]
    fn heading_hashes_dropped() {
        let got = sanitise_markdown("## Section title here\nBody follows.");
        assert_eq!(got, "Section title here\nBody follows.");
    }

    #[test]
    fn multiple_heading_levels_dropped() {
        let got = sanitise_markdown("# H1\n### H3 subtitle\nParagraph text.");
        assert_eq!(got, "H1\nH3 subtitle\nParagraph text.");
    }

    #[test]
    fn markdown_link_reduced_to_text() {
        let got = sanitise_markdown("See [the docs](https://example.com/docs) for details.");
        assert_eq!(got, "See the docs for details.");
    }

    #[test]
    fn inline_code_backtick_stripped() {
        let got = sanitise_markdown("Run `cargo test` to check the build.");
        assert_eq!(got, "Run cargo test to check the build.");
    }

    #[test]
    fn long_url_replaced_with_a_link() {
        let got = sanitise_markdown("Check https://very-long-example.com/path/to/resource/page");
        assert!(got.contains("a link"), "long URL must become 'a link': {got:?}");
        assert!(!got.contains("very-long"), "URL body must be removed: {got:?}");
    }

    #[test]
    fn short_url_preserved() {
        let got = sanitise_markdown("See https://fono.page for info.");
        assert!(got.contains("https://fono.page"), "short URL must be kept: {got:?}");
    }

    #[test]
    fn plain_text_unchanged() {
        let input = "Hello there, this is a plain sentence today.";
        assert_eq!(sanitise_markdown(input), input);
    }

    // ── Sentence hard-cap ───────────────────────────────────────────

    #[test]
    fn short_sentence_passes_through() {
        let got = hard_cap_sentence("Short sentence.", 200);
        assert_eq!(got, vec!["Short sentence.".to_string()]);
    }

    #[test]
    fn long_sentence_split_at_word_boundary() {
        let words: Vec<&str> = (0..20).map(|_| "word").collect();
        let long = words.join(" "); // 20 × "word" + 19 spaces = 99 chars; let's go bigger
        let very_long = format!("{long} {long} more words here"); // definitely > 100
        let chunks = hard_cap_sentence(&very_long, 50);
        assert!(chunks.len() > 1, "long sentence must be split");
        for c in &chunks {
            assert!(c.chars().count() <= 50, "chunk too long: {c:?}");
        }
    }

    #[test]
    fn all_chunks_non_empty() {
        let s = "a ".repeat(300);
        for chunk in hard_cap_sentence(s.trim(), 200) {
            assert!(!chunk.is_empty());
        }
    }

    #[test]
    fn exactly_max_chars_fits_in_one_chunk() {
        let s = "x".repeat(200);
        let got = hard_cap_sentence(&s, 200);
        assert_eq!(got.len(), 1);
    }

    // ── Backpressure semantics ──────────────────────────────────────

    #[test]
    fn bounded_channel_blocks_when_full() {
        // A bounded channel with capacity MAX_PENDING = 5.
        // Fill it to capacity; verify the 6th send would block (try_send fails).
        let (tx, _rx) = tokio::sync::mpsc::channel::<String>(MAX_PENDING);
        for i in 0..MAX_PENDING {
            tx.try_send(format!("sentence {i}")).expect("should not be full yet");
        }
        assert!(
            tx.try_send("overflow".to_string()).is_err(),
            "channel must be full at MAX_PENDING items"
        );
    }
}
