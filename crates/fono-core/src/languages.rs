// SPDX-License-Identifier: GPL-3.0-only
//! Curated language shortlist used by the wizard, the tray, and any
//! future settings UI. One canonical list keeps the BCP-47 codes,
//! display names, and ordering consistent across surfaces — picking
//! "English" in the tray and "English" in the wizard write the same
//! `general.languages = ["en"]` to disk.
//!
//! Languages are picked for **dictation accuracy** — Whisper's WER
//! is reasonable on every entry below across both `tiny`/`base` and
//! larger models. Adding a language here is a UX decision: any code
//! not on the list still works via `general.languages` in the TOML
//! config, but won't appear in the quick-pick menus.

/// Canonical shortlist for the wizard's language picker and the
/// tray's `Preferences ▸ Language` submenu. `(BCP-47 alpha-2,
/// display name)`. Order is presentation-only — Fono treats every
/// entry as an equal peer.
pub const CURATED_LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("ro", "Romanian"),
    ("pl", "Polish"),
    ("ru", "Russian"),
    ("uk", "Ukrainian"),
    ("tr", "Turkish"),
    ("zh", "Chinese"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("hi", "Hindi"),
    ("ar", "Arabic"),
];

/// Look up the display name for a BCP-47 code in the curated list,
/// returning the code unchanged when not found (so a user-edited
/// `general.languages = ["sv"]` still renders as `"sv"` rather than
/// disappearing).
#[must_use]
pub fn display_name(code: &str) -> &str {
    CURATED_LANGUAGES
        .iter()
        .find(|(c, _)| *c == code)
        .map_or(code, |(_, name)| *name)
}
