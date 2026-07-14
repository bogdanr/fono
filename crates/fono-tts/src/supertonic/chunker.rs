// SPDX-License-Identifier: GPL-3.0-only
//! Supertonic text chunker: long text → synthesis-sized chunks (Slice 2,
//! Task 2.4).
//!
//! Ported from the sherpa reference `ChunkText` (in `text-utils.cc`) plus the
//! per-language `max_len` selection in `offline-tts-supertonic-impl.cc`. The
//! model synthesises one chunk at a time; long input is split on paragraph,
//! sentence, and finally hard-length boundaries so each chunk stays within the
//! model's comfortable window (300 codepoints, or 120 for Korean/Japanese).
//!
//! The pipeline, per the reference:
//!
//! 1. [`split_by_blank_lines`] — join wrapped lines into paragraphs, split on
//!    blank lines.
//! 2. [`split_by_punctuation`] — split each paragraph into sentences at
//!    `. ! ? 。 ！ ？`.
//! 3. [`split_long_sentence`] — hard-split any sentence longer than `max_len`,
//!    preferring a space or clause boundary near the cut.
//! 4. greedily pack pieces back together up to `max_len`, inserting a space
//!    only where [`need_space_between`] says one is wanted (never inside CJK or
//!    next to punctuation).

/// Default chunk length in codepoints (`max_len`): 120 for Korean/Japanese,
/// 300 otherwise, matching the reference default.
#[must_use]
pub fn default_max_len(lang: &str) -> usize {
    if lang == "ko" || lang == "ja" {
        120
    } else {
        300
    }
}

fn is_sentence_boundary(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '\u{3002}' | '\u{FF01}' | '\u{FF1F}')
}

fn is_chunk_boundary(c: char) -> bool {
    is_sentence_boundary(c) || matches!(c, ',' | ';' | ':' | '\u{FF0C}' | '\u{FF1B}' | '\u{FF1A}')
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{000C}' | '\u{000B}')
}

fn is_cjk(cp: u32) -> bool {
    (0x1100..=0x11FF).contains(&cp)
        || (0x2E80..=0xA4CF).contains(&cp)
        || (0xA840..=0xD7AF).contains(&cp)
        || (0xF900..=0xFAFF).contains(&cp)
        || (0xFE30..=0xFE4F).contains(&cp)
        || (0xFF65..=0xFFDC).contains(&cp)
        || (0x20000..=0x2FFFF).contains(&cp)
}

/// ASCII-only trim, matching the reference `Trim` (which uses `std::isspace`
/// on individual bytes, so it strips only ASCII whitespace).
fn trim_ascii(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_ascii_whitespace())
}

fn count_codepoints(s: &str) -> usize {
    s.chars().count()
}

/// `true` if a single space should join `left` and `right` when concatenating.
/// No space next to existing whitespace, CJK, or clause punctuation.
#[must_use]
pub fn need_space_between(left: &str, right: &str) -> bool {
    let (Some(last), Some(first)) = (left.chars().next_back(), right.chars().next()) else {
        return false;
    };
    if is_space(last) || is_space(first) {
        return false;
    }
    if is_cjk(last as u32)
        || is_cjk(first as u32)
        || is_chunk_boundary(last)
        || is_chunk_boundary(first)
    {
        return false;
    }
    true
}

/// Join wrapped lines into paragraphs; split on blank lines.
#[must_use]
pub fn split_by_blank_lines(text: &str) -> Vec<String> {
    let mut paragraphs = Vec::new();
    let mut cur = String::new();
    for line in text.split('\n') {
        let line = trim_ascii(line);
        if line.is_empty() {
            let s = trim_ascii(&cur);
            if !s.is_empty() {
                paragraphs.push(s.to_string());
            }
            cur.clear();
        } else {
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(line);
        }
    }
    let s = trim_ascii(&cur);
    if !s.is_empty() {
        paragraphs.push(s.to_string());
    }
    if paragraphs.is_empty() {
        let s = trim_ascii(text);
        if !s.is_empty() {
            paragraphs.push(s.to_string());
        }
    }
    paragraphs
}

/// Split `text` into sentences at sentence-ending punctuation.
#[must_use]
pub fn split_by_punctuation(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        cur.push(c);
        if is_sentence_boundary(c) {
            let s = trim_ascii(&cur);
            if !s.is_empty() {
                sentences.push(s.to_string());
            }
            cur.clear();
        }
    }
    let s = trim_ascii(&cur);
    if !s.is_empty() {
        sentences.push(s.to_string());
    }
    sentences
}

/// Hard-split `sentence` into pieces no longer than `max_chars` codepoints,
/// preferring to cut at a space or clause boundary near the limit.
#[must_use]
pub fn split_long_sentence(sentence: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    if max_chars == 0 {
        return chunks;
    }
    let s = trim_ascii(sentence);
    if s.is_empty() {
        return chunks;
    }
    let u32: Vec<char> = s.chars().collect();
    let len = u32.len();
    let mut start = 0;
    while start < len {
        let end = (start + max_chars).min(len);
        if end >= len {
            let piece: String = u32[start..].iter().collect();
            let piece = trim_ascii(&piece);
            if !piece.is_empty() {
                chunks.push(piece.to_string());
            }
            break;
        }

        let mut split_pos = end;
        let mut found = false;
        let mut i = end;
        while i > start {
            let c = u32[i - 1];
            if is_space(c) {
                split_pos = i - 1;
                found = true;
                break;
            }
            if is_chunk_boundary(c) {
                split_pos = i;
                found = true;
                break;
            }
            i -= 1;
        }

        if !found || split_pos <= start {
            split_pos = end;
        }

        let piece: String = u32[start..split_pos].iter().collect();
        let piece = trim_ascii(&piece);
        if !piece.is_empty() {
            chunks.push(piece.to_string());
        }

        start = split_pos;
        while start < len && is_space(u32[start]) {
            start += 1;
        }
    }
    chunks
}

/// Split `text` into synthesis chunks of at most `max_len` codepoints.
#[must_use]
pub fn chunk_text(text: &str, max_len: usize) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    if max_len == 0 {
        return chunks;
    }
    let text_single = trim_ascii(text);
    if text_single.is_empty() {
        return chunks;
    }

    let mut cur = String::new();
    let flush = |cur: &mut String, chunks: &mut Vec<String>| {
        let s = trim_ascii(cur);
        if !s.is_empty() {
            chunks.push(s.to_string());
        }
        cur.clear();
    };

    for para in split_by_blank_lines(text_single) {
        for sent in split_by_punctuation(&para) {
            for p in split_long_sentence(&sent, max_len) {
                if p.is_empty() {
                    continue;
                }
                if cur.is_empty() {
                    cur = p;
                    continue;
                }
                let need_space = need_space_between(&cur, &p);
                let projected =
                    count_codepoints(&cur) + count_codepoints(&p) + usize::from(need_space);
                if projected <= max_len {
                    if need_space {
                        cur.push(' ');
                    }
                    cur.push_str(&p);
                } else {
                    flush(&mut cur, &mut chunks);
                    cur = p;
                }
            }
        }
    }

    flush(&mut cur, &mut chunks);
    if chunks.is_empty() {
        chunks.push(text_single.to_string());
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_max_len_is_shorter_for_cjk() {
        assert_eq!(default_max_len("en"), 300);
        assert_eq!(default_max_len("ro"), 300);
        assert_eq!(default_max_len("ko"), 120);
        assert_eq!(default_max_len("ja"), 120);
    }

    #[test]
    fn short_text_is_a_single_chunk() {
        assert_eq!(chunk_text("Hello there.", 300), vec!["Hello there."]);
    }

    #[test]
    fn empty_or_whitespace_yields_no_chunks() {
        assert!(chunk_text("", 300).is_empty());
        assert!(chunk_text("   \n  \t ", 300).is_empty());
        assert!(chunk_text("anything", 0).is_empty());
    }

    #[test]
    fn sentences_pack_greedily_up_to_max_len() {
        // Two short sentences fit together under a generous limit. The joining
        // space is suppressed because the left piece ends in a clause boundary
        // ('.') — faithful to the upstream `need_space_between`.
        let chunks = chunk_text("One. Two.", 300);
        assert_eq!(chunks, vec!["One.Two."]);
        // A tiny limit forces them apart.
        let chunks = chunk_text("One. Two.", 4);
        assert_eq!(chunks, vec!["One.", "Two."]);
    }

    #[test]
    fn blank_lines_separate_paragraphs() {
        let paras = split_by_blank_lines("line one\nstill one\n\nsecond para");
        assert_eq!(paras, vec!["line one still one", "second para"]);
    }

    #[test]
    fn punctuation_splits_sentences() {
        let s = split_by_punctuation("Hi there! How are you? Fine.");
        assert_eq!(s, vec!["Hi there!", "How are you?", "Fine."]);
    }

    #[test]
    fn long_sentence_splits_on_space_near_limit() {
        // No sentence punctuation, so it must hard-split; prefers a space.
        let pieces = split_long_sentence("aaaa bbbb cccc dddd", 10);
        for p in &pieces {
            assert!(p.chars().count() <= 10, "piece too long: {p:?}");
        }
        // Rejoining (with single spaces) recovers the words in order.
        assert_eq!(pieces.join(" "), "aaaa bbbb cccc dddd");
    }

    #[test]
    fn every_chunk_stays_within_max_len() {
        let text = "The quick brown fox jumps over the lazy dog. \
                    Pack my box with five dozen liquor jugs. \
                    How vexingly quick daft zebras jump!";
        let max = 30;
        let chunks = chunk_text(text, max);
        assert!(chunks.len() > 1);
        for c in &chunks {
            assert!(c.chars().count() <= max, "chunk over budget: {c:?}");
        }
    }

    #[test]
    fn no_space_inserted_next_to_cjk_or_punctuation() {
        // CJK on either side suppresses the joining space.
        assert!(!need_space_between("\u{4E2D}", "\u{6587}"));
        // Clause punctuation suppresses it too.
        assert!(!need_space_between("hi,", "there"));
        // Plain Latin words want a space.
        assert!(need_space_between("hi", "there"));
    }
}
