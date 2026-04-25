// SPDX-License-Identifier: GPL-3.0-only
//! Word Error Rate (WER) — the standard accuracy metric for STT.
//!
//! WER = (S + D + I) / N
//!
//! where S = substitutions, D = deletions, I = insertions in the
//! Levenshtein alignment of the hypothesis to the reference, and N is
//! the number of reference tokens.

/// Tokenise: lowercase, drop punctuation, split on whitespace, drop
/// empties. Numbers stay as-is (whisper transcribes "5" or "five"
/// inconsistently across providers; bench transcripts SHOULD spell out
/// numbers to keep WER comparable).
fn tokenise(s: &str) -> Vec<String> {
    s.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Compute the Levenshtein distance between two token vectors.
///
/// Uses two rolling rows for O(min(n,m)) memory.
fn levenshtein(a: &[String], b: &[String]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];
    for (i, ai) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, bj) in b.iter().enumerate() {
            let cost = usize::from(ai != bj);
            curr[j + 1] = (prev[j + 1] + 1) // deletion
                .min(curr[j] + 1) // insertion
                .min(prev[j] + cost); // sub / match
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Word Error Rate of `hypothesis` against `reference`.
///
/// Returns `0.0` when the reference is empty (avoids division by zero).
/// Values can exceed `1.0` when the hypothesis is much longer than the
/// reference (insertions dominate).
#[must_use]
pub fn word_error_rate(reference: &str, hypothesis: &str) -> f32 {
    let r = tokenise(reference);
    let h = tokenise(hypothesis);
    if r.is_empty() {
        return 0.0;
    }
    let dist = levenshtein(&r, &h);
    dist as f32 / r.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perfect_match_is_zero() {
        let r = "hello world this is a test";
        let h = "hello world this is a test";
        assert_eq!(word_error_rate(r, h), 0.0);
    }

    #[test]
    fn punctuation_and_case_normalised() {
        let r = "Hello, world!";
        let h = "hello world";
        assert_eq!(word_error_rate(r, h), 0.0);
    }

    #[test]
    fn single_substitution() {
        let r = "the quick brown fox";
        let h = "the quick brown dog";
        // 1 sub / 4 ref tokens = 0.25
        assert!((word_error_rate(r, h) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn deletion_and_insertion() {
        let r = "alpha beta gamma";
        let h = "alpha gamma delta epsilon";
        // alignment: alpha = alpha, [del beta], gamma = gamma, [ins delta], [ins epsilon]
        // 1 del + 2 ins = 3 edits / 3 ref = 1.0
        assert!((word_error_rate(r, h) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn empty_reference_returns_zero() {
        assert_eq!(word_error_rate("", "anything goes"), 0.0);
    }

    #[test]
    fn empty_hypothesis_is_full_deletion() {
        // 4 deletions / 4 ref = 1.0
        assert!((word_error_rate("the quick brown fox", "") - 1.0).abs() < 1e-6);
    }

    #[test]
    fn apostrophes_preserved() {
        // "don't" vs "do not" → 1 sub + 1 ins = 2 edits / 1 ref token = 2.0
        let r = "don't";
        let h = "do not";
        assert!(word_error_rate(r, h) >= 1.0);
    }
}
