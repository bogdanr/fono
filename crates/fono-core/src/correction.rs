// SPDX-License-Identifier: GPL-3.0-only
//! Personal vocabulary: deterministic transcript correction (ADR 0037).
//!
//! A user-authored `vocabulary.toml` maps STT mishearings to canonical
//! spellings (`phono → Fono`). [`VocabularyTable::apply`] is a pure,
//! single-pass, idempotent substitution run on the raw transcript
//! immediately after STT — upstream of polish, injection, and history —
//! so every downstream consumer sees corrected text.
//!
//! Matching semantics (locked by the ADR):
//! - whole-word / whole-phrase only ("phonograph" is never touched);
//! - case-insensitive via plain Unicode case-folding, canonical-casing
//!   output;
//! - multi-word phrases match across whitespace or hyphens, never across
//!   other punctuation;
//! - longest match first;
//! - single pass; idempotency is guaranteed by load-time validation.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// One vocabulary rule: any of the `from` terms rewrites to `to`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VocabularyEntry {
    /// Mishearings (case-insensitive; may be multi-word phrases).
    pub from: Vec<String>,
    /// Canonical spelling, emitted verbatim.
    pub to: String,
}

/// On-disk shape of `vocabulary.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VocabularyFile {
    #[serde(default)]
    pub vocabulary: Vec<VocabularyEntry>,
}

impl VocabularyFile {
    /// Load from disk. A missing file is an empty vocabulary, not an error.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(raw) => toml::from_str(&raw)
                .map_err(|source| Error::TomlParse { path: path.to_path_buf(), source }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(Error::Io { path: path.to_path_buf(), source }),
        }
    }

    /// Atomic write (tempfile + rename), mode 0644. Never destroys the
    /// previous file on a crash mid-write.
    pub fn save(&self, path: &Path) -> Result<()> {
        let toml_str = toml::to_string_pretty(self)?;
        crate::config::atomic_write(path, toml_str.as_bytes(), 0o644)
    }

    /// Validate and compile into a matching table. Errors are
    /// human-readable and name the offending entry.
    pub fn to_table(&self) -> std::result::Result<VocabularyTable, String> {
        let mut rules: Vec<Rule> = Vec::new();
        for (i, entry) in self.vocabulary.iter().enumerate() {
            let n = i + 1;
            if entry.to.trim().is_empty() {
                return Err(format!("entry {n}: `to` is empty"));
            }
            if entry.from.is_empty() {
                return Err(format!("entry {n} (to = {:?}): `from` list is empty", entry.to));
            }
            for term in &entry.from {
                let words = fold_words(term);
                if words.is_empty() {
                    return Err(format!(
                        "entry {n} (to = {:?}): `from` term {term:?} contains no words",
                        entry.to
                    ));
                }
                let chars: usize = words.iter().map(|w| w.chars().count()).sum();
                rules.push(Rule { words, to: entry.to.clone(), chars });
            }
        }
        // Duplicate-`from` rejection (case-folded, whitespace-normalized).
        for (a, rule) in rules.iter().enumerate() {
            for other in rules.iter().skip(a + 1) {
                if rule.words == other.words {
                    return Err(format!(
                        "duplicate `from` term {:?} (maps to both {:?} and {:?})",
                        rule.words.join(" "),
                        rule.to,
                        other.to
                    ));
                }
            }
        }
        // Idempotency: a rule's output may not itself be another rule's
        // input unless it rewrites to the same output (this permits
        // case-normalization entries like `fono → Fono`).
        for rule in &rules {
            let to_words = fold_words(&rule.to);
            for other in &rules {
                if to_words == other.words && other.to != rule.to {
                    return Err(format!(
                        "`to` value {:?} is also a `from` term of a rule mapping to {:?}; \
                         applying twice would change the text again",
                        rule.to, other.to
                    ));
                }
            }
        }
        // Longest match first: more words, then more characters.
        rules.sort_by(|a, b| b.words.len().cmp(&a.words.len()).then(b.chars.cmp(&a.chars)));
        Ok(VocabularyTable { rules })
    }
}

/// A compiled rule: folded word sequence → canonical output.
#[derive(Debug, Clone)]
struct Rule {
    words: Vec<String>,
    to: String,
    chars: usize,
}

/// Compiled, validated vocabulary. Cheap to build; built per dictation.
#[derive(Debug, Clone, Default)]
pub struct VocabularyTable {
    rules: Vec<Rule>,
}

impl VocabularyTable {
    /// Load + compile, falling back to an empty (no-op) table with a
    /// logged warning when the file is malformed. The daemon never
    /// crashes on user data.
    #[must_use]
    pub fn load_or_empty(path: &Path) -> Self {
        let file = match VocabularyFile::load(path) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(
                    "vocabulary: cannot read {}: {e} — corrections disabled until fixed",
                    path.display()
                );
                return Self::default();
            }
        };
        match file.to_table() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    "vocabulary: {} is invalid: {e} — corrections disabled until fixed",
                    path.display()
                );
                Self::default()
            }
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Apply the vocabulary to `text`. Pure, single-pass, idempotent.
    #[must_use]
    pub fn apply(&self, text: &str) -> String {
        if self.rules.is_empty() {
            return text.to_string();
        }
        let toks = tokenize(text);
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        while i < toks.len() {
            match toks[i] {
                Tok::Sep(s) => {
                    out.push_str(s);
                    i += 1;
                }
                Tok::Word(w) => {
                    if let Some((rule, consumed)) = self.match_at(&toks, i) {
                        out.push_str(&rule.to);
                        i += consumed;
                    } else {
                        out.push_str(w);
                        i += 1;
                    }
                }
            }
        }
        out
    }

    /// Try every rule (longest first) at word-token position `i`.
    /// Returns the winning rule and how many tokens it consumed.
    fn match_at(&self, toks: &[Tok<'_>], i: usize) -> Option<(&Rule, usize)> {
        'rule: for rule in &self.rules {
            let mut j = i;
            for (wi, rw) in rule.words.iter().enumerate() {
                if wi > 0 {
                    // Between phrase words: exactly one separator token,
                    // whitespace or hyphens only — never across other
                    // punctuation (sentence boundaries stay boundaries).
                    match toks.get(j) {
                        Some(Tok::Sep(s)) if s.chars().all(|c| c.is_whitespace() || c == '-') => {
                            j += 1;
                        }
                        _ => continue 'rule,
                    }
                }
                match toks.get(j) {
                    Some(Tok::Word(w)) if w.to_lowercase() == *rw => j += 1,
                    _ => continue 'rule,
                }
            }
            return Some((rule, j - i));
        }
        None
    }
}

/// Word / separator tokens. Words are maximal runs of Unicode
/// alphanumerics; everything else (spaces, punctuation) is a separator.
#[derive(Debug, Clone, Copy)]
enum Tok<'a> {
    Word(&'a str),
    Sep(&'a str),
}

fn tokenize(text: &str) -> Vec<Tok<'_>> {
    let mut toks = Vec::new();
    let mut start = 0;
    let mut cur_is_word: Option<bool> = None;
    for (idx, c) in text.char_indices() {
        let is_word = c.is_alphanumeric();
        match cur_is_word {
            Some(w) if w == is_word => {}
            Some(w) => {
                toks.push(if w {
                    Tok::Word(&text[start..idx])
                } else {
                    Tok::Sep(&text[start..idx])
                });
                start = idx;
                cur_is_word = Some(is_word);
            }
            None => cur_is_word = Some(is_word),
        }
    }
    if let Some(w) = cur_is_word {
        toks.push(if w { Tok::Word(&text[start..]) } else { Tok::Sep(&text[start..]) });
    }
    toks
}

/// Case-folded word sequence of a term: split on non-alphanumerics,
/// lowercase each word. `"Phone   oh"` → `["phone", "oh"]`.
fn fold_words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(entries: &[(&[&str], &str)]) -> VocabularyTable {
        let file = VocabularyFile {
            vocabulary: entries
                .iter()
                .map(|(from, to)| VocabularyEntry {
                    from: from.iter().map(|s| (*s).to_string()).collect(),
                    to: (*to).to_string(),
                })
                .collect(),
        };
        file.to_table().expect("valid test table")
    }

    fn phono() -> VocabularyTable {
        table(&[(&["phono", "phone oh"], "Fono")])
    }

    #[test]
    fn basic_replacement() {
        assert_eq!(phono().apply("I use phono every day"), "I use Fono every day");
    }

    #[test]
    fn substring_safety() {
        let t = phono();
        for s in ["phonograph", "phonetic", "telephone", "the phonograph rocks"] {
            assert_eq!(t.apply(s), s, "substring must never be corrected: {s}");
        }
    }

    #[test]
    fn case_variants_emit_canonical_casing() {
        let t = phono();
        assert_eq!(t.apply("Phono"), "Fono");
        assert_eq!(t.apply("phono"), "Fono");
        assert_eq!(t.apply("PHONO"), "Fono");
        assert_eq!(t.apply("Phono rocks. phono, PHONO!"), "Fono rocks. Fono, Fono!");
    }

    #[test]
    fn multi_word_phrase() {
        let t = phono();
        assert_eq!(t.apply("i love phone oh so much"), "i love Fono so much");
        assert_eq!(t.apply("Phone   Oh works"), "Fono works");
        // Hyphen-joined phrase words match too.
        assert_eq!(t.apply("phone-oh works"), "Fono works");
    }

    #[test]
    fn phrase_never_crosses_sentence_punctuation() {
        let t = phono();
        assert_eq!(t.apply("on the phone. Oh, I see"), "on the phone. Oh, I see");
        assert_eq!(t.apply("the phone, oh well"), "the phone, oh well");
    }

    #[test]
    fn longest_match_first() {
        let t = table(&[(&["phone"], "Fon"), (&["phone oh"], "Fono")]);
        assert_eq!(t.apply("phone oh yes"), "Fono yes");
        assert_eq!(t.apply("phone yes"), "Fon yes");
    }

    #[test]
    fn idempotent_double_apply() {
        let t = table(&[
            (&["phono", "phone oh"], "Fono"),
            (&["fono"], "Fono"), // case-normalization entry: allowed
            (&["cube ernetes"], "Kubernetes"),
        ]);
        let input = "phono and phone oh and fono run cube ernetes daily";
        let once = t.apply(input);
        assert_eq!(once, "Fono and Fono and Fono run Kubernetes daily");
        assert_eq!(t.apply(&once), once, "double apply must be a no-op");
    }

    #[test]
    fn unicode_diacritics_in_source_and_target() {
        let t = table(&[(&["bogdan"], "Bogdăn"), (&["stiinta"], "știința")]);
        assert_eq!(t.apply("bogdan studies stiinta"), "Bogdăn studies știința");
        // Folded match on a diacritic source word.
        let t2 = table(&[(&["știința"], "Science")]);
        assert_eq!(t2.apply("Știința wins"), "Science wins");
    }

    #[test]
    fn empty_table_is_noop() {
        let t = VocabularyTable::default();
        assert!(t.is_empty());
        assert_eq!(t.apply("phono stays"), "phono stays");
    }

    #[test]
    fn validation_rejects_empty_to() {
        let f = VocabularyFile {
            vocabulary: vec![VocabularyEntry { from: vec!["x".into()], to: "  ".into() }],
        };
        assert!(f.to_table().unwrap_err().contains("`to` is empty"));
    }

    #[test]
    fn validation_rejects_empty_from() {
        let f = VocabularyFile {
            vocabulary: vec![VocabularyEntry { from: vec![], to: "Fono".into() }],
        };
        assert!(f.to_table().unwrap_err().contains("`from` list is empty"));
        let f2 = VocabularyFile {
            vocabulary: vec![VocabularyEntry { from: vec!["  !!".into()], to: "Fono".into() }],
        };
        assert!(f2.to_table().unwrap_err().contains("no words"));
    }

    #[test]
    fn validation_rejects_duplicate_from() {
        let f = VocabularyFile {
            vocabulary: vec![
                VocabularyEntry { from: vec!["phono".into()], to: "Fono".into() },
                VocabularyEntry { from: vec!["Phono".into()], to: "Phone".into() },
            ],
        };
        assert!(f.to_table().unwrap_err().contains("duplicate `from`"));
    }

    #[test]
    fn validation_rejects_to_from_overlap() {
        // a → b while b → c: applying twice would turn a into c.
        let f = VocabularyFile {
            vocabulary: vec![
                VocabularyEntry { from: vec!["alpha".into()], to: "beta".into() },
                VocabularyEntry { from: vec!["beta".into()], to: "gamma".into() },
            ],
        };
        assert!(f.to_table().unwrap_err().contains("applying twice"));
    }

    #[test]
    fn case_normalization_entry_is_allowed() {
        // fono → Fono: `to` folds to its own `from`, same output → fine.
        let t = table(&[(&["fono"], "Fono")]);
        assert_eq!(t.apply("fono"), "Fono");
        assert_eq!(t.apply("Fono"), "Fono");
    }

    #[test]
    fn file_roundtrip_and_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("vocabulary.toml");
        // Missing file → empty vocabulary, no error.
        assert!(VocabularyFile::load(&path).unwrap().vocabulary.is_empty());
        assert!(VocabularyTable::load_or_empty(&path).is_empty());

        let f = VocabularyFile {
            vocabulary: vec![VocabularyEntry {
                from: vec!["phono".into(), "phone oh".into()],
                to: "Fono".into(),
            }],
        };
        f.save(&path).unwrap();
        let loaded = VocabularyFile::load(&path).unwrap();
        assert_eq!(loaded.vocabulary, f.vocabulary);
        let t = VocabularyTable::load_or_empty(&path);
        assert_eq!(t.len(), 2); // two `from` terms compile to two rules
        assert_eq!(t.apply("phono"), "Fono");
    }

    #[test]
    fn malformed_file_falls_back_to_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("vocabulary.toml");
        std::fs::write(&path, "this is [ not toml").unwrap();
        assert!(VocabularyTable::load_or_empty(&path).is_empty());
        // Valid TOML, invalid semantics (empty to) → also empty table.
        std::fs::write(&path, "[[vocabulary]]\nfrom = [\"x\"]\nto = \"\"\n").unwrap();
        assert!(VocabularyTable::load_or_empty(&path).is_empty());
    }
}
