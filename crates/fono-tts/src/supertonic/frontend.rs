// SPDX-License-Identifier: GPL-3.0-only
//! Supertonic text frontend: text → token ids (Slice 2, Task 2.3).
//!
//! Ported verbatim from the sherpa reference
//! `offline-tts-supertonic-unicode-processor.cc`. Three stages:
//!
//! 1. [`preprocess_text`] — the upstream `PreprocessText`: a fixed table of
//!    character substitutions, emoji stripping, punctuation/space tidying, a
//!    trailing-period rule, and finally wrapping the text in `<lang>…</lang>`.
//! 2. [`text_to_unicode_values`] — UTF-8 decode + NFKD decomposition
//!    ([`decompose_codepoint`], using [`nfkd_table`] for the BMP and the
//!    algorithmic Hangul rule). NFKD is essential: precomposed diacritics
//!    (Romanian ă/ș/ț, Czech č, Polish ą, …) would otherwise miss the indexer
//!    and be dropped.
//! 3. indexer lookup — the flat `int32[65536]` BMP table from
//!    `unicode_indexer.bin` maps each decomposed codepoint to a token id.
//!
//! Expressive tags the model understands ([`EXPRESSIVE_TAGS`]: `<laugh>`,
//! `<breath>`, `<sigh>`) survive preprocessing untouched, so tagged text
//! reaches the model as-is. Everything else that looks like `<…>` markup is
//! stripped defensively ([`strip_unknown_tags`], Slice 4 Task 4.4) so stray
//! HTML/XML never leaks into the audio as spelled-out characters. The
//! assistant-only *emission* policy (only the assistant path is told it may
//! use these tags) is enforced by the caller, not here.

use anyhow::{bail, Context, Result};

use super::nfkd_table::{NFKD_CODEPOINTS, NFKD_OFFSETS, NFKD_POOL};

/// The 31 languages Supertonic 3 accepts (`kSupertonicAvailableLangs`).
pub const AVAILABLE_LANGS: [&str; 31] = [
    "en", "ko", "ja", "ar", "bg", "cs", "da", "de", "el", "es", "et", "fi", "fr", "hi", "hr", "hu",
    "id", "it", "lt", "lv", "nl", "pl", "pt", "ro", "ru", "sk", "sl", "sv", "tr", "uk", "vi",
];

/// `true` if `lang` is one of the 31 supported codes.
#[must_use]
pub fn is_supported_lang(lang: &str) -> bool {
    AVAILABLE_LANGS.contains(&lang)
}

// Hangul syllable decomposition constants (Unicode Annex #15).
const HANGUL_SBASE: u32 = 0xAC00;
const HANGUL_LBASE: u32 = 0x1100;
const HANGUL_VBASE: u32 = 0x1161;
const HANGUL_TBASE: u32 = 0x11A7;
const HANGUL_VCOUNT: u32 = 21;
const HANGUL_TCOUNT: u32 = 28;
const HANGUL_NCOUNT: u32 = HANGUL_VCOUNT * HANGUL_TCOUNT; // 588
const HANGUL_SCOUNT: u32 = 19 * HANGUL_NCOUNT; // 11172

/// The fixed character-substitution table applied first, in order
/// (upstream `PreprocessText`'s `replacements` array).
const REPLACEMENTS: &[(&str, &str)] = &[
    ("\u{2013}", "-"), // – en dash
    ("\u{2011}", "-"), // ‑ non-breaking hyphen
    ("\u{2014}", "-"), // — em dash
    ("_", " "),
    ("\u{201C}", "\""), // “
    ("\u{201D}", "\""), // ”
    ("\u{2018}", "'"),  // ‘
    ("\u{2019}", "'"),  // ’
    ("\u{00B4}", "'"),  // ´
    ("`", "'"),
    ("[", " "),
    ("]", " "),
    ("|", " "),
    ("/", " "),
    ("#", " "),
    ("\u{2192}", " "), // →
    ("\u{2190}", " "), // ←
    ("\u{2665}", ""),  // ♥
    ("\u{2606}", ""),  // ☆
    ("\u{2661}", ""),  // ♡
    ("\u{00A9}", ""),  // ©
    ("\\", ""),
    ("@", " at "),
    ("e.g.,", "for example, "),
    ("i.e.,", "that is, "),
];

/// Decompose one codepoint into BMP `u16` values, matching the reference
/// `DecomposeCharacter`: Hangul syllables algorithmically, everything else via
/// the NFKD table, with a non-decomposable BMP codepoint passed through
/// unchanged (codepoints above the BMP are dropped).
pub fn decompose_codepoint(cp: u32, out: &mut Vec<u16>) {
    if (HANGUL_SBASE..HANGUL_SBASE + HANGUL_SCOUNT).contains(&cp) {
        let s_index = cp - HANGUL_SBASE;
        let l_index = s_index / HANGUL_NCOUNT;
        let v_index = (s_index % HANGUL_NCOUNT) / HANGUL_TCOUNT;
        let t_index = s_index % HANGUL_TCOUNT;
        out.push((HANGUL_LBASE + l_index) as u16);
        out.push((HANGUL_VBASE + v_index) as u16);
        if t_index > 0 {
            out.push((HANGUL_TBASE + t_index) as u16);
        }
        return;
    }
    if cp <= 0xFFFF {
        if let Ok(i) = NFKD_CODEPOINTS.binary_search(&(cp as u16)) {
            let (lo, hi) = (NFKD_OFFSETS[i] as usize, NFKD_OFFSETS[i + 1] as usize);
            out.extend_from_slice(&NFKD_POOL[lo..hi]);
            return;
        }
        out.push(cp as u16);
    }
}

/// Expressive tags the Supertonic model is trained to render as non-speech
/// events (per the model card). Any other `<…>` markup is stripped by
/// [`strip_unknown_tags`]; add entries here to allow more tags through.
pub const EXPRESSIVE_TAGS: &[&str] = &["<laugh>", "<breath>", "<sigh>"];

/// Drop `<…>`-style markup that is not a known [`EXPRESSIVE_TAGS`] entry, so
/// stray tags (`<div>`, `</p>`, `<script>`, …) never reach the model as literal
/// characters. A `<` that does not open a tag-shaped token (e.g. `a < b`) is
/// left untouched.
#[must_use]
pub fn strip_unknown_tags(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            if let Some(close) = tag_close(&chars[i..]) {
                let tag: String = chars[i..=i + close].iter().collect();
                if EXPRESSIVE_TAGS.contains(&tag.as_str()) {
                    out.push_str(&tag);
                }
                i += close + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Given a slice starting at `<`, return the index of the matching `>` if the
/// enclosed text is a tag-shaped token (non-empty, only ASCII letters/digits
/// and `/`, `-`, `_`). Returns `None` for a bare `<` (space or other content).
fn tag_close(chars: &[char]) -> Option<usize> {
    let mut j = 1;
    while j < chars.len() {
        match chars[j] {
            '>' => return (j > 1).then_some(j),
            c if c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_') => j += 1,
            _ => return None,
        }
    }
    None
}

/// The upstream `PreprocessText`: normalize `text` and wrap it in language
/// tags. Deterministic and independent of the indexer. Fono adds one step the
/// reference lacks: [`strip_unknown_tags`] runs first so stray markup is gone
/// before substitution.
#[must_use]
pub fn preprocess_text(text: &str, lang: &str) -> String {
    // 0. Defensively drop unknown `<…>` markup (Fono addition).
    let text = strip_unknown_tags(text);
    // 1. Fixed substitutions, applied in order.
    let mut result = text;
    for (from, to) in REPLACEMENTS {
        if result.contains(from) {
            result = result.replace(from, to);
        }
    }

    // 2. Strip the U+1F000..U+1FFFF emoji/symbol block.
    result.retain(|c| !(0x1F000..=0x1FFFF).contains(&(c as u32)));

    // 3. Drop a space that immediately precedes closing punctuation.
    let punct_fixed = drop_space_before_punct(&result);

    // 4. Discard backticks; collapse doubled quotes ("" → ", '' → ').
    let quotes_fixed = collapse_quotes(&punct_fixed);

    // 5. Collapse whitespace runs to a single space, then trim.
    result = collapse_spaces(&quotes_fixed);
    let mut result = result.trim().to_string();

    // 6. Ensure the utterance ends in punctuation (append "." otherwise).
    if !result.is_empty() && !ends_with_punctuation(&result) {
        result.push('.');
    }

    // 7. Wrap in language tags.
    format!("<{lang}>{result}</{lang}>")
}

fn drop_space_before_punct(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ' ' && i + 1 < chars.len() {
            let next = chars[i + 1];
            if matches!(next, ',' | '.' | '!' | '?' | ';' | ':' | '\'') {
                out.push(next);
                i += 2;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn collapse_quotes(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            i += 1;
            continue;
        }
        if (c == '"' || c == '\'') && i + 1 < chars.len() && chars[i + 1] == c {
            out.push(c);
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(c);
            last_was_space = false;
        }
    }
    out
}

fn ends_with_punctuation(s: &str) -> bool {
    let Some(last) = s.chars().next_back() else {
        return false;
    };
    if matches!(last, '.' | '!' | '?' | ';' | ':' | ',' | '\'' | '"' | ')' | ']' | '}' | '>') {
        return true;
    }
    // Non-ASCII closing punctuation (upstream `IsEndingPunctuationCodepoint`).
    matches!(
        last as u32,
        0x2026
            | 0x3002
            | 0x300D
            | 0x300F
            | 0x3011
            | 0x3009
            | 0x300B
            | 0x203A
            | 0x00BB
            | 0x201C
            | 0x201D
            | 0x2018
            | 0x2019
    )
}

/// UTF-8 decode `text` and NFKD-decompose each codepoint into BMP `u16`s.
#[must_use]
pub fn text_to_unicode_values(text: &str) -> Vec<u16> {
    let mut out = Vec::new();
    for c in text.chars() {
        decompose_codepoint(c as u32, &mut out);
    }
    out
}

/// The text frontend: owns the `unicode_indexer.bin` lookup table.
#[derive(Debug, Clone)]
pub struct Frontend {
    /// Flat BMP → token-id table; `indexer[u]` for a `u16` codepoint `u`.
    indexer: Vec<i32>,
}

impl Frontend {
    /// Load the indexer from `unicode_indexer.bin` (a flat little-endian
    /// `int32` array).
    pub fn load(indexer_path: &std::path::Path) -> Result<Self> {
        let bytes = std::fs::read(indexer_path)
            .with_context(|| format!("read unicode indexer {}", indexer_path.display()))?;
        Self::from_bytes(&bytes)
    }

    /// Build the frontend from raw `unicode_indexer.bin` bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() % 4 != 0 {
            bail!(
                "invalid unicode_indexer.bin size {} (must be a nonzero multiple of 4)",
                bytes.len()
            );
        }
        let indexer =
            bytes.chunks_exact(4).map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        Ok(Self { indexer })
    }

    /// Full frontend: `text` + `lang` → token ids. An out-of-range codepoint
    /// (beyond the indexer) maps to the unknown id 0, matching the reference.
    #[must_use]
    pub fn process(&self, text: &str, lang: &str) -> Vec<i64> {
        let processed = preprocess_text(text, lang);
        let unicode_vals = text_to_unicode_values(&processed);
        unicode_vals
            .iter()
            .map(|&u| self.indexer.get(u as usize).copied().unwrap_or(0) as i64)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decomp(cp: u32) -> Vec<u16> {
        let mut v = Vec::new();
        decompose_codepoint(cp, &mut v);
        v
    }

    #[test]
    fn romanian_diacritics_decompose_to_base_plus_combining() {
        // ă U+0103 → a + combining breve U+0306
        assert_eq!(decomp(0x0103), vec![0x0061, 0x0306]);
        // ș U+0219 → s + combining comma below U+0326
        assert_eq!(decomp(0x0219), vec![0x0073, 0x0326]);
        // ț U+021B → t + combining comma below U+0326
        assert_eq!(decomp(0x021B), vec![0x0074, 0x0326]);
        // â U+00E2 → a + combining circumflex U+0302
        assert_eq!(decomp(0x00E2), vec![0x0061, 0x0302]);
    }

    #[test]
    fn other_language_diacritics_decompose() {
        // Czech č U+010D → c + caron U+030C
        assert_eq!(decomp(0x010D), vec![0x0063, 0x030C]);
        // Polish ą U+0105 → a + ogonek U+0328
        assert_eq!(decomp(0x0105), vec![0x0061, 0x0328]);
    }

    #[test]
    fn plain_ascii_passes_through_unchanged() {
        assert_eq!(decomp(0x0061), vec![0x0061]); // 'a'
        assert_eq!(decomp(0x0020), vec![0x0020]); // space
    }

    #[test]
    fn hangul_syllable_decomposes_algorithmically() {
        // 한 U+D55C → ᄒ(U+1112) ᅡ(U+1161) ᆫ(U+11AB)
        assert_eq!(decomp(0xD55C), vec![0x1112, 0x1161, 0x11AB]);
        // 가 U+AC00 → ᄀ(U+1100) ᅡ(U+1161), no trailing jamo
        assert_eq!(decomp(0xAC00), vec![0x1100, 0x1161]);
    }

    #[test]
    fn preprocess_wraps_and_terminates() {
        assert_eq!(preprocess_text("hello", "en"), "<en>hello.</en>");
        // Already-terminated text keeps its punctuation.
        assert_eq!(preprocess_text("Salut!", "ro"), "<ro>Salut!</ro>");
    }

    #[test]
    fn preprocess_applies_substitutions_and_spacing() {
        // em dash → hyphen; doubled quotes collapse; space-before-comma dropped.
        assert_eq!(preprocess_text("a \u{2014} b , c", "en"), "<en>a - b, c.</en>");
        // A trailing double-quote already counts as ending punctuation
        // (upstream `ends_with_punct`), so no period is appended.
        assert_eq!(preprocess_text("say \"\"hi\"\"", "en"), "<en>say \"hi\"</en>");
    }

    #[test]
    fn preprocess_strips_emoji_and_collapses_spaces() {
        assert_eq!(preprocess_text("hi \u{1F600}  there", "en"), "<en>hi there.</en>");
    }

    #[test]
    fn text_to_unicode_values_decomposes_romanian_word() {
        // "ăț" → a, breve, t, comma-below
        assert_eq!(
            text_to_unicode_values("\u{0103}\u{021B}"),
            vec![0x0061, 0x0306, 0x0074, 0x0326]
        );
    }

    #[test]
    fn known_expressive_tags_survive_stripping() {
        assert_eq!(strip_unknown_tags("hi <laugh> there"), "hi <laugh> there");
        assert_eq!(strip_unknown_tags("<breath>ok<sigh>"), "<breath>ok<sigh>");
    }

    #[test]
    fn unknown_tags_are_stripped() {
        assert_eq!(strip_unknown_tags("a<div>b</div>c"), "abc");
        assert_eq!(strip_unknown_tags("x<script>y"), "xy");
        // Closing form of a known tag is not itself known -> stripped.
        assert_eq!(strip_unknown_tags("<laugh></laugh>"), "<laugh>");
    }

    #[test]
    fn bare_angle_bracket_is_left_alone() {
        // Not tag-shaped (space after '<'): preserved as literal maths.
        assert_eq!(strip_unknown_tags("a < b > c"), "a < b > c");
        // Empty <> is not a tag.
        assert_eq!(strip_unknown_tags("a<>b"), "a<>b");
    }

    #[test]
    fn preprocess_strips_unknown_tags_but_keeps_expressive() {
        assert_eq!(preprocess_text("hello <b>world</b>", "en"), "<en>hello world.</en>");
        assert_eq!(preprocess_text("hi <laugh> ok", "ro"), "<ro>hi <laugh> ok.</ro>");
    }

    #[test]
    fn language_allowlist_has_31_entries_including_ro() {
        assert_eq!(AVAILABLE_LANGS.len(), 31);
        assert!(is_supported_lang("ro"));
        assert!(is_supported_lang("en"));
        assert!(!is_supported_lang("zz"));
    }

    #[test]
    fn frontend_rejects_malformed_indexer() {
        assert!(Frontend::from_bytes(&[]).is_err());
        assert!(Frontend::from_bytes(&[1, 2, 3]).is_err()); // not a multiple of 4
    }

    #[test]
    fn frontend_maps_codepoints_through_the_indexer() {
        // Synthetic identity-ish indexer of 256 entries: id = codepoint + 1.
        let mut bytes = Vec::new();
        for i in 0..256i32 {
            bytes.extend_from_slice(&(i + 1).to_le_bytes());
        }
        let fe = Frontend::from_bytes(&bytes).unwrap();
        // "a." wrapped as <en>a.</en>; every char is ASCII < 256, so present.
        let ids = fe.process("a", "en");
        assert!(!ids.is_empty());
        // 'a' (0x61) → indexer[0x61] = 0x62.
        assert!(ids.contains(&0x62));
    }
}
