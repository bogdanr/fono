// SPDX-License-Identifier: GPL-3.0-only
//! Friendly, gender-aware voice palette shared by every TTS backend.
//!
//! Provider voice ids are cryptic and backend-specific (`alloy`,
//! `EXAVITQu4vr4xnSDxMaL`, a Cartesia UUID, `af_heart`). The palette is the
//! abstraction that hides them: each backend exposes a short curated list of
//! [`PaletteVoice`]s, and the user only ever addresses a voice by a
//! **positional, gendered label** — "Female 1", "Male 2" — never the raw id.
//!
//! The label is purely positional *within a gender*, assigned in palette
//! order, so we never rename a voice that already has an intrinsic identity
//! (e.g. `af_heart` stays `af_heart`; it is merely *addressed* as "Female 1").
//! The intrinsic [`backend_id`](PaletteVoice::backend_id) is shown beside the
//! label in `fono voices list` for context.

use std::fmt;

/// Perceived voice gender. Tags every palette entry and is also the axis a
/// user gender preference filters on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gender {
    /// A female-presenting voice.
    Female,
    /// A male-presenting voice.
    Male,
    /// A neutral / unspecified voice (used when a backend does not label
    /// gender, or for androgynous presets).
    Neutral,
}

impl Gender {
    /// Lowercase canonical token: `"female"`, `"male"`, `"neutral"`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Female => "female",
            Self::Male => "male",
            Self::Neutral => "neutral",
        }
    }

    /// Capitalised label stem used in positional labels: `"Female"`,
    /// `"Male"`, `"Neutral"`.
    #[must_use]
    pub fn label_stem(self) -> &'static str {
        match self {
            Self::Female => "Female",
            Self::Male => "Male",
            Self::Neutral => "Neutral",
        }
    }

    /// Parse a gender token case-insensitively. Accepts the full word
    /// (`female`/`male`/`neutral`) and the `f`/`m`/`n` shorthands. Returns
    /// `None` for anything else (callers treat that as "no preference").
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "female" | "f" | "woman" | "w" | "feminine" => Some(Self::Female),
            "male" | "m" | "man" | "masculine" => Some(Self::Male),
            "neutral" | "n" | "any" | "neuter" | "gender_neutral" | "genderless" => {
                Some(Self::Neutral)
            }
            _ => None,
        }
    }
}

impl fmt::Display for Gender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One voice in a backend's palette: its intrinsic backend id plus the
/// gender used for labelling and filtering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteVoice {
    /// The cryptic, backend-specific wire value (`alloy`, a UUID,
    /// `af_heart`, …). This is what the TTS client actually sends.
    pub backend_id: String,
    /// Perceived gender, for positional labelling and gender filtering.
    pub gender: Gender,
}

impl PaletteVoice {
    /// Construct from any string-like id.
    pub fn new(backend_id: impl Into<String>, gender: Gender) -> Self {
        Self { backend_id: backend_id.into(), gender }
    }
}

/// A `const`-constructible palette entry, for baking a provider's curated
/// voice list into a static catalogue (see `provider_catalog`). Convert to
/// the owned [`PaletteVoice`] at runtime via [`PaletteEntry::to_voice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteEntry {
    /// The cryptic, backend-specific wire value.
    pub backend_id: &'static str,
    /// Perceived gender.
    pub gender: Gender,
}

impl PaletteEntry {
    /// Owned-string view used by the runtime palette.
    #[must_use]
    pub fn to_voice(self) -> PaletteVoice {
        PaletteVoice { backend_id: self.backend_id.to_string(), gender: self.gender }
    }
}

/// An ordered list of a backend's palette voices. Positional labels are
/// derived from this order: the first `Female` entry is "Female 1", the
/// second "Female 2", and so on, independently per gender.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Palette {
    voices: Vec<PaletteVoice>,
}

impl Palette {
    /// Build a palette from an ordered list of voices.
    #[must_use]
    pub fn new(voices: Vec<PaletteVoice>) -> Self {
        Self { voices }
    }

    /// Build a palette from a static slice of [`PaletteEntry`]s (a cloud
    /// provider's curated list).
    #[must_use]
    pub fn from_entries(entries: &[PaletteEntry]) -> Self {
        Self { voices: entries.iter().map(|e| e.to_voice()).collect() }
    }

    /// All voices in palette order.
    #[must_use]
    pub fn voices(&self) -> &[PaletteVoice] {
        &self.voices
    }

    /// True when the palette has no voices.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.voices.is_empty()
    }

    /// The positional label for the voice at `index` ("Female 1", "Male 2").
    /// Returns `None` if `index` is out of range.
    #[must_use]
    pub fn label_at(&self, index: usize) -> Option<String> {
        let voice = self.voices.get(index)?;
        // Count how many same-gender voices precede this one (1-based).
        let position = self.voices[..index].iter().filter(|v| v.gender == voice.gender).count() + 1;
        Some(positional_label(voice.gender, position))
    }

    /// Every `(label, voice)` pair in palette order — what `fono voices
    /// list` renders.
    #[must_use]
    pub fn labelled(&self) -> Vec<(String, &PaletteVoice)> {
        self.voices
            .iter()
            .enumerate()
            .map(|(i, v)| (self.label_at(i).unwrap_or_default(), v))
            .collect()
    }

    /// Resolve a positional label ("female 2", case-insensitive) to its
    /// voice. Returns `None` if the label does not parse or no such slot
    /// exists in this palette.
    #[must_use]
    pub fn by_label(&self, label: &str) -> Option<&PaletteVoice> {
        let (gender, position) = parse_label(label)?;
        if position == 0 {
            return None;
        }
        self.voices.iter().filter(|v| v.gender == gender).nth(position - 1)
    }

    /// Voices matching `gender`, in palette order. With `Gender::Neutral`
    /// callers usually mean "no preference" — that is the resolver's job;
    /// here we filter strictly on the tag.
    #[must_use]
    pub fn by_gender(&self, gender: Gender) -> Vec<&PaletteVoice> {
        self.voices.iter().filter(|v| v.gender == gender).collect()
    }

    /// Find a voice by its raw backend id (exact match).
    #[must_use]
    pub fn by_backend_id(&self, backend_id: &str) -> Option<&PaletteVoice> {
        self.voices.iter().find(|v| v.backend_id == backend_id)
    }
}

/// Render a positional gendered label, e.g. `(Gender::Female, 1)` →
/// `"Female 1"`. Position is 1-based.
#[must_use]
pub fn positional_label(gender: Gender, position: usize) -> String {
    format!("{} {}", gender.label_stem(), position)
}

/// Parse a positional gendered label back into `(gender, position)`.
///
/// Accepts case-insensitive forms with or without a space:
/// `"Female 1"`, `"female 1"`, `"FEMALE1"`, `"f 2"`, `"m3"`. Returns `None`
/// if it does not look like a positional label.
#[must_use]
pub fn parse_label(label: &str) -> Option<(Gender, usize)> {
    let s = label.trim();
    if s.is_empty() {
        return None;
    }
    // Split into a leading non-digit gender token and a trailing number.
    let digit_start = s.find(|c: char| c.is_ascii_digit())?;
    let (gender_part, num_part) = s.split_at(digit_start);
    let gender = Gender::parse(gender_part.trim())?;
    let position: usize = num_part.trim().parse().ok()?;
    if position == 0 {
        return None;
    }
    Some((gender, position))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Palette {
        Palette::new(vec![
            PaletteVoice::new("af_heart", Gender::Female),
            PaletteVoice::new("am_michael", Gender::Male),
            PaletteVoice::new("af_bella", Gender::Female),
            PaletteVoice::new("bm_lewis", Gender::Male),
        ])
    }

    #[test]
    fn gender_parse_is_case_insensitive_and_accepts_shorthands() {
        assert_eq!(Gender::parse("Female"), Some(Gender::Female));
        assert_eq!(Gender::parse("  MALE "), Some(Gender::Male));
        assert_eq!(Gender::parse("f"), Some(Gender::Female));
        assert_eq!(Gender::parse("m"), Some(Gender::Male));
        assert_eq!(Gender::parse("any"), Some(Gender::Neutral));
        // Provider-specific synonyms (e.g. Cartesia uses these).
        assert_eq!(Gender::parse("feminine"), Some(Gender::Female));
        assert_eq!(Gender::parse("Masculine"), Some(Gender::Male));
        assert_eq!(Gender::parse("gender_neutral"), Some(Gender::Neutral));
        assert_eq!(Gender::parse("zzz"), None);
    }

    #[test]
    fn labels_are_positional_per_gender_in_palette_order() {
        let p = sample();
        assert_eq!(p.label_at(0).as_deref(), Some("Female 1"));
        assert_eq!(p.label_at(1).as_deref(), Some("Male 1"));
        assert_eq!(p.label_at(2).as_deref(), Some("Female 2"));
        assert_eq!(p.label_at(3).as_deref(), Some("Male 2"));
        assert_eq!(p.label_at(4), None);
    }

    #[test]
    fn by_label_resolves_to_intrinsic_id() {
        let p = sample();
        assert_eq!(p.by_label("Female 1").unwrap().backend_id, "af_heart");
        assert_eq!(p.by_label("female 2").unwrap().backend_id, "af_bella");
        assert_eq!(p.by_label("MALE 2").unwrap().backend_id, "bm_lewis");
        assert_eq!(p.by_label("m1").unwrap().backend_id, "am_michael");
        assert!(p.by_label("Female 3").is_none(), "no third female slot");
        assert!(p.by_label("nonsense").is_none());
        assert!(p.by_label("Female 0").is_none(), "positions are 1-based");
    }

    #[test]
    fn label_round_trips_through_parse() {
        for (g, n) in [(Gender::Female, 1), (Gender::Male, 7), (Gender::Neutral, 3)] {
            let rendered = positional_label(g, n);
            assert_eq!(parse_label(&rendered), Some((g, n)));
        }
    }

    #[test]
    fn parse_label_tolerates_spacing_and_case() {
        assert_eq!(parse_label("Female 1"), Some((Gender::Female, 1)));
        assert_eq!(parse_label("female1"), Some((Gender::Female, 1)));
        assert_eq!(parse_label("  MALE  12 "), Some((Gender::Male, 12)));
        assert_eq!(parse_label("f2"), Some((Gender::Female, 2)));
        assert_eq!(parse_label(""), None);
        assert_eq!(parse_label("female"), None, "missing position");
        assert_eq!(parse_label("3"), None, "missing gender");
    }

    #[test]
    fn by_gender_filters_in_order() {
        let p = sample();
        let males: Vec<_> =
            p.by_gender(Gender::Male).iter().map(|v| v.backend_id.clone()).collect();
        assert_eq!(males, vec!["am_michael", "bm_lewis"]);
    }
}
