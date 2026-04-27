// SPDX-License-Identifier: GPL-3.0-only
//! Language selection plumbing for STT backends.
//!
//! A user can specify a list of BCP-47 language codes that fono should
//! restrict speech-to-text to. The list semantics are:
//!
//! * empty → unconstrained Whisper auto-detect (historical `language =
//!   "auto"` behaviour);
//! * one entry → that language is **forced** as if the user had typed
//!   `language = "en"` in v0.1 configs;
//! * two or more entries → **constrained auto-detect**: Whisper still
//!   picks, but only from the supplied set. Local Whisper enforces this
//!   by running `lang_detect` on the audio prefix and argmaxing over
//!   the masked subset; cloud STT degrades gracefully via
//!   post-validation (see `cloud_force_primary_language` and
//!   `cloud_rerun_on_language_mismatch` in `[general]`).
//!
//! Codes are normalised on entry (trimmed, lowercased, the alias
//! `"auto"` is dropped). Duplicates are collapsed.

use std::fmt;

/// Effective language selection for a single STT call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LanguageSelection {
    /// No constraint — let the backend auto-detect across every
    /// language it knows.
    Auto,
    /// Force a single language. Backends MUST pass this code through
    /// to the underlying engine (Whisper `set_language`, cloud
    /// `language=` form field, …).
    Forced(String),
    /// Allow-list of two or more languages. Backends MUST refuse to
    /// emit a transcription tagged with any code outside this set;
    /// see the module docstring for the per-backend mechanism.
    AllowList(Vec<String>),
}

impl LanguageSelection {
    /// Build a selection from a config-style list, applying
    /// normalisation rules. Returns [`Self::Auto`] when the list is
    /// empty or every entry is the alias `"auto"`.
    #[must_use]
    pub fn from_config(codes: &[String]) -> Self {
        let normalised = normalise_codes(codes);
        match normalised.len() {
            0 => Self::Auto,
            1 => Self::Forced(normalised.into_iter().next().expect("len==1")),
            _ => Self::AllowList(normalised),
        }
    }

    /// Build a selection from a comma-separated string (CLI / wizard
    /// flow). Whitespace and case are normalised; the alias `"auto"`
    /// collapses to [`Self::Auto`].
    #[must_use]
    pub fn parse_csv(s: &str) -> Self {
        let codes: Vec<String> = s
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        Self::from_config(&codes)
    }

    /// `true` when no constraint is in effect.
    #[must_use]
    pub fn is_auto(&self) -> bool {
        matches!(self, Self::Auto)
    }

    /// Primary BCP-47 code: the forced code, or the first entry of
    /// the allow-list. `None` for [`Self::Auto`]. Useful as a "best
    /// single guess" to send to a cloud provider that only accepts a
    /// single `language` field.
    #[must_use]
    pub fn primary(&self) -> Option<&str> {
        match self {
            Self::Auto => None,
            Self::Forced(c) => Some(c.as_str()),
            Self::AllowList(v) => v.first().map(String::as_str),
        }
    }

    /// All codes in the selection (single-element slice for forced;
    /// empty for auto). Iteration order is the user's configured
    /// order, used by `WhisperLocal::lang_detect` masking and by the
    /// cloud post-validation path.
    #[must_use]
    pub fn codes(&self) -> &[String] {
        match self {
            Self::Auto => &[],
            Self::Forced(_) => std::slice::from_ref(self.forced_owned().expect("Forced")),
            Self::AllowList(v) => v.as_slice(),
        }
    }

    fn forced_owned(&self) -> Option<&String> {
        if let Self::Forced(c) = self {
            Some(c)
        } else {
            None
        }
    }

    /// `true` when `code` is allowed under this selection. Auto
    /// allows everything. Comparison is case-insensitive.
    #[must_use]
    pub fn contains(&self, code: &str) -> bool {
        let lc = code.trim().to_ascii_lowercase();
        match self {
            Self::Auto => true,
            Self::Forced(c) => c.eq_ignore_ascii_case(&lc),
            Self::AllowList(v) => v.iter().any(|c| c.eq_ignore_ascii_case(&lc)),
        }
    }

    /// Apply a per-call override. Used by the bench harness and
    /// `fono record --language XX` to force a single code regardless
    /// of the configured allow-list.
    #[must_use]
    pub fn with_override(self, override_code: Option<&str>) -> Self {
        match override_code.map(str::trim) {
            None | Some("") => self,
            Some("auto") => Self::Auto,
            Some(code) => Self::Forced(code.to_ascii_lowercase()),
        }
    }
}

impl Default for LanguageSelection {
    fn default() -> Self {
        Self::Auto
    }
}

impl fmt::Display for LanguageSelection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Forced(c) => write!(f, "forced({c})"),
            Self::AllowList(v) => write!(f, "allow-list({})", v.join(",")),
        }
    }
}

/// Trim, lowercase, drop empties + the `"auto"` alias, deduplicate
/// while preserving first-seen order.
fn normalise_codes(codes: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(codes.len());
    for raw in codes {
        let lc = raw.trim().to_ascii_lowercase();
        if lc.is_empty() || lc == "auto" {
            continue;
        }
        if !out.contains(&lc) {
            out.push(lc);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_list_is_auto() {
        let s = LanguageSelection::from_config(&[]);
        assert!(s.is_auto());
        assert!(matches!(s, LanguageSelection::Auto));
    }

    #[test]
    fn auto_alias_collapses_to_auto() {
        let codes = vec!["auto".to_string()];
        assert!(matches!(
            LanguageSelection::from_config(&codes),
            LanguageSelection::Auto
        ));
    }

    #[test]
    fn single_entry_becomes_forced_lowercased() {
        let codes = vec!["EN".to_string()];
        let s = LanguageSelection::from_config(&codes);
        match s {
            LanguageSelection::Forced(c) => assert_eq!(c, "en"),
            other => panic!("expected Forced, got {other:?}"),
        }
    }

    #[test]
    fn multi_entry_becomes_allow_list_and_dedupes() {
        let codes = vec![
            "en".to_string(),
            " RO ".to_string(),
            "en".to_string(),
            "fr".to_string(),
        ];
        let s = LanguageSelection::from_config(&codes);
        match s {
            LanguageSelection::AllowList(v) => assert_eq!(v, vec!["en", "ro", "fr"]),
            other => panic!("expected AllowList, got {other:?}"),
        }
    }

    #[test]
    fn parse_csv_handles_spaces_and_auto() {
        assert!(LanguageSelection::parse_csv("auto").is_auto());
        assert!(LanguageSelection::parse_csv("   ").is_auto());
        let s = LanguageSelection::parse_csv("en, ro , fr");
        assert!(matches!(s, LanguageSelection::AllowList(_)));
        assert_eq!(
            s.codes(),
            &["en".to_string(), "ro".to_string(), "fr".to_string()]
        );
    }

    #[test]
    fn primary_picks_first() {
        assert_eq!(LanguageSelection::Auto.primary(), None);
        assert_eq!(
            LanguageSelection::Forced("en".into()).primary(),
            Some("en")
        );
        assert_eq!(
            LanguageSelection::AllowList(vec!["en".into(), "ro".into()]).primary(),
            Some("en")
        );
    }

    #[test]
    fn contains_is_case_insensitive() {
        let s = LanguageSelection::AllowList(vec!["en".into(), "ro".into()]);
        assert!(s.contains("EN"));
        assert!(s.contains("ro"));
        assert!(!s.contains("fr"));
        assert!(LanguageSelection::Auto.contains("anything"));
    }

    #[test]
    fn override_replaces_or_clears() {
        let base = LanguageSelection::AllowList(vec!["en".into(), "ro".into()]);
        let f = base.clone().with_override(Some("FR"));
        assert!(matches!(f, LanguageSelection::Forced(ref c) if c == "fr"));
        let a = base.clone().with_override(Some("auto"));
        assert!(a.is_auto());
        let same = base.clone().with_override(None);
        assert_eq!(same, base);
    }
}
