// SPDX-License-Identifier: GPL-3.0-only
//! Pure voice resolution shared by every TTS call site (the four MCP
//! tools and the `fono summarize` CLI).
//!
//! Given the active backend's [`Palette`], the calling program's
//! identity, an optional per-call voice override, and the relevant
//! `[mcp]` config, [`resolve_voice`] yields the concrete backend voice
//! id to synthesise with — or `None`, meaning "fall back to the
//! backend default".
//!
//! Precedence (highest first):
//! 1. **Explicit per-call `voice`** — a positional label ("female 1")
//!    resolved against the palette, or a raw backend id passed through
//!    verbatim. The literal `"auto"` forces automatic assignment.
//! 2. **Manual pin** — `[mcp.voices]` entry for the program: a label,
//!    `"auto"`, or a raw id. A stale label (slot absent on the active
//!    backend) degrades to automatic assignment rather than erroring.
//! 3. **Automatic assignment** — when `auto_assign_voices` is on and a
//!    program key is known, a deterministic hash of the (normalised)
//!    program name selects a voice from the gender-filtered palette,
//!    stable across restarts.
//! 4. **Default** — `None`; the caller uses the backend default voice.
//!
//! A gender preference (`[mcp].voice_gender`) filters the palette
//! before *automatic* assignment only; explicit overrides and manual
//! pins are specific and bypass the filter.

use std::collections::BTreeMap;

use crate::voice_palette::{parse_label, Gender, Palette};

/// Inputs to [`resolve_voice`]. Borrowed so the resolver allocates
/// nothing but the returned id.
#[derive(Debug, Clone, Copy)]
pub struct VoiceQuery<'a> {
    /// The active backend's palette (curated, gender-tagged voices).
    pub palette: &'a Palette,
    /// Normalised-or-raw program identity (MCP `clientInfo.name`, or
    /// `source_app` for `fono.summarize`). `None` when unknown (e.g. a
    /// CLI call with no source), which disables pins + auto-assignment.
    pub program: Option<&'a str>,
    /// The per-call `voice` argument, if any.
    pub explicit: Option<&'a str>,
    /// `[mcp].voices` — program → label/`"auto"`/raw-id pins.
    pub pins: &'a BTreeMap<String, String>,
    /// `[mcp].voice_gender` — `"male"`/`"female"`/`"any"`/empty.
    pub voice_gender: &'a str,
    /// `[mcp].auto_assign_voices`.
    pub auto_assign: bool,
}

/// Resolve the concrete backend voice id for a TTS call, per the
/// precedence documented on the module. `None` ⇒ use the backend
/// default voice.
#[must_use]
pub fn resolve_voice(q: &VoiceQuery) -> Option<String> {
    // 1. Explicit per-call override wins. A concrete voice (label or
    //    raw id) returns immediately; "auto" / a stale label skips the
    //    pin layer and forces automatic assignment.
    let mut force_auto = false;
    if let Some(explicit) = q.explicit.map(str::trim).filter(|s| !s.is_empty()) {
        match resolve_token(q.palette, explicit) {
            TokenOutcome::Voice(id) => return Some(id),
            TokenOutcome::Auto | TokenOutcome::Stale => force_auto = true,
        }
    }

    let program = q.program.map(str::trim).filter(|s| !s.is_empty());

    // 2. Manual pin (skipped when an explicit override forced auto).
    if !force_auto {
        if let Some(program) = program {
            if let Some(value) = lookup_pin(q.pins, program) {
                match resolve_token(q.palette, value) {
                    TokenOutcome::Voice(id) => return Some(id),
                    // "auto" or a stale label → fall through to auto.
                    TokenOutcome::Auto | TokenOutcome::Stale => {}
                }
            }
        }
    }

    // 3. Automatic, deterministic per-program assignment.
    if q.auto_assign || force_auto {
        if let Some(program) = program {
            return auto_assign(q.palette, program, parse_gender_pref(q.voice_gender));
        }
    }

    // 4. Backend default.
    None
}

/// Outcome of interpreting a single voice token (explicit arg or pin
/// value) against a palette.
enum TokenOutcome {
    /// A concrete backend voice id to use.
    Voice(String),
    /// The literal `"auto"` — defer to automatic assignment.
    Auto,
    /// A positional label that does not resolve on this palette — also
    /// defers to automatic assignment (graceful degradation).
    Stale,
}

/// Interpret a voice token: the literal `auto`, a positional label
/// ("female 2"), or a raw backend id passed through verbatim.
fn resolve_token(palette: &Palette, token: &str) -> TokenOutcome {
    let t = token.trim();
    if t.eq_ignore_ascii_case("auto") {
        return TokenOutcome::Auto;
    }
    // A positional-looking label is resolved against the palette; if the
    // slot is absent it is stale, not a raw id.
    if parse_label(t).is_some() {
        return palette
            .by_label(t)
            .map_or(TokenOutcome::Stale, |v| TokenOutcome::Voice(v.backend_id.clone()));
    }
    // Otherwise treat it as a raw backend id and pass it through.
    TokenOutcome::Voice(t.to_string())
}

/// Deterministically assign a palette voice to `program`, optionally
/// filtered to a gender. Returns `None` only when the palette is empty.
fn auto_assign(palette: &Palette, program: &str, gender: Option<Gender>) -> Option<String> {
    // Build the candidate pool: the gender-filtered slice if a
    // preference is set and yields at least one voice, else the whole
    // palette (so e.g. a "male" preference on an all-female backend
    // degrades gracefully rather than erroring).
    let all = || palette.voices().iter().map(|v| v.backend_id.as_str()).collect::<Vec<_>>();
    let pool: Vec<&str> = gender.map_or_else(all, |g| {
        let filtered: Vec<&str> =
            palette.by_gender(g).iter().map(|v| v.backend_id.as_str()).collect();
        if filtered.is_empty() {
            all()
        } else {
            filtered
        }
    });
    if pool.is_empty() {
        return None;
    }
    let idx = (fnv1a_64(&normalize_key(program)) % pool.len() as u64) as usize;
    Some(pool[idx].to_string())
}

/// Look up a pin for `program`, matching on the normalised key so that
/// case / surrounding whitespace differences don't defeat the lookup.
fn lookup_pin<'a>(pins: &'a BTreeMap<String, String>, program: &str) -> Option<&'a str> {
    let want = normalize_key(program);
    // Fast path: exact key hit.
    if let Some(v) = pins.get(program) {
        return Some(v.as_str());
    }
    pins.iter().find(|(k, _)| normalize_key(k) == want).map(|(_, v)| v.as_str())
}

/// Map `[mcp].voice_gender` to a filter: `Some` for a concrete gender,
/// `None` for "any"/empty/unknown (no filtering).
#[must_use]
pub fn parse_gender_pref(s: &str) -> Option<Gender> {
    match Gender::parse(s) {
        Some(Gender::Female) => Some(Gender::Female),
        Some(Gender::Male) => Some(Gender::Male),
        _ => None,
    }
}

/// Normalise a program key for stable hashing + matching: trim and
/// lowercase.
fn normalize_key(s: &str) -> String {
    s.trim().to_lowercase()
}

/// FNV-1a 64-bit hash. Chosen over `DefaultHasher` for a guarantee of
/// stability across Rust versions, so an automatic voice assignment
/// never shifts under a program after a toolchain bump.
fn fnv1a_64(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice_palette::PaletteVoice;

    fn palette() -> Palette {
        Palette::new(vec![
            PaletteVoice::new("af_heart", Gender::Female),
            PaletteVoice::new("am_michael", Gender::Male),
            PaletteVoice::new("af_bella", Gender::Female),
            PaletteVoice::new("bm_lewis", Gender::Male),
        ])
    }

    fn query<'a>(
        p: &'a Palette,
        program: Option<&'a str>,
        explicit: Option<&'a str>,
        pins: &'a BTreeMap<String, String>,
        gender: &'a str,
        auto: bool,
    ) -> VoiceQuery<'a> {
        VoiceQuery { palette: p, program, explicit, pins, voice_gender: gender, auto_assign: auto }
    }

    #[test]
    fn explicit_label_beats_everything() {
        let p = palette();
        let mut pins = BTreeMap::new();
        pins.insert("coach".into(), "female 1".into());
        let q = query(&p, Some("coach"), Some("male 2"), &pins, "female", true);
        assert_eq!(resolve_voice(&q).as_deref(), Some("bm_lewis"));
    }

    #[test]
    fn explicit_raw_id_passes_through_verbatim() {
        let p = palette();
        let pins = BTreeMap::new();
        // A raw id not in the palette is still honoured on the explicit path.
        let q = query(&p, Some("coach"), Some("nova"), &pins, "", true);
        assert_eq!(resolve_voice(&q).as_deref(), Some("nova"));
    }

    #[test]
    fn manual_pin_label_resolves_to_backend_id() {
        let p = palette();
        let mut pins = BTreeMap::new();
        pins.insert("coach".into(), "male 1".into());
        let q = query(&p, Some("coach"), None, &pins, "", true);
        assert_eq!(resolve_voice(&q).as_deref(), Some("am_michael"));
    }

    #[test]
    fn pin_lookup_is_case_insensitive() {
        let p = palette();
        let mut pins = BTreeMap::new();
        pins.insert("Coach".into(), "female 2".into());
        let q = query(&p, Some("  coach "), None, &pins, "", true);
        assert_eq!(resolve_voice(&q).as_deref(), Some("af_bella"));
    }

    #[test]
    fn stale_pin_label_degrades_to_auto_not_error() {
        let p = palette();
        let mut pins = BTreeMap::new();
        pins.insert("coach".into(), "male 9".into()); // no such slot
        let q = query(&p, Some("coach"), None, &pins, "", true);
        // Falls through to deterministic auto-assignment (Some, not the label).
        let got = resolve_voice(&q).expect("auto-assign should kick in");
        assert!(p.by_backend_id(&got).is_some(), "auto picks a real palette voice: {got}");
    }

    #[test]
    fn auto_assignment_is_deterministic_across_calls() {
        let p = palette();
        let pins = BTreeMap::new();
        let q = query(&p, Some("chat-cli"), None, &pins, "", true);
        let a = resolve_voice(&q);
        let b = resolve_voice(&q);
        assert_eq!(a, b, "same program ⇒ same voice every time");
        assert!(a.is_some());
    }

    #[test]
    fn auto_assignment_distinguishes_distinct_programs() {
        let p = palette();
        let pins = BTreeMap::new();
        // With four voices, three common names should not all collide.
        let names = ["coach", "chat", "coder"];
        let voices: std::collections::BTreeSet<_> = names
            .iter()
            .map(|n| {
                let q = query(&p, Some(n), None, &pins, "", true);
                resolve_voice(&q).unwrap()
            })
            .collect();
        assert!(voices.len() >= 2, "distinct programs should mostly differ: {voices:?}");
    }

    #[test]
    fn gender_preference_restricts_auto_pool() {
        let p = palette();
        let pins = BTreeMap::new();
        for name in ["coach", "chat", "coder", "agent", "bot"] {
            let q = query(&p, Some(name), None, &pins, "male", true);
            let got = resolve_voice(&q).unwrap();
            assert!(
                got == "am_michael" || got == "bm_lewis",
                "male preference must pick a male voice, got {got}"
            );
        }
    }

    #[test]
    fn gender_preference_with_no_matching_voice_falls_back_to_full_palette() {
        // All-female palette, male preference: degrade rather than error.
        let p = Palette::new(vec![
            PaletteVoice::new("af_heart", Gender::Female),
            PaletteVoice::new("af_bella", Gender::Female),
        ]);
        let pins = BTreeMap::new();
        let q = query(&p, Some("coach"), None, &pins, "male", true);
        let got = resolve_voice(&q).expect("should still resolve");
        assert!(p.by_backend_id(&got).is_some());
    }

    #[test]
    fn auto_disabled_unpinned_program_uses_default() {
        let p = palette();
        let pins = BTreeMap::new();
        let q = query(&p, Some("coach"), None, &pins, "", false);
        assert_eq!(resolve_voice(&q), None, "auto off + no pin ⇒ backend default");
    }

    #[test]
    fn explicit_auto_forces_assignment_even_when_pin_present() {
        let p = palette();
        let mut pins = BTreeMap::new();
        pins.insert("coach".into(), "female 1".into());
        let q = query(&p, Some("coach"), Some("auto"), &pins, "", false);
        // Explicit "auto" overrides the pin and forces assignment even
        // though auto_assign is globally off.
        let got = resolve_voice(&q).expect("explicit auto forces assignment");
        assert!(p.by_backend_id(&got).is_some());
    }

    #[test]
    fn no_program_no_explicit_yields_default() {
        let p = palette();
        let pins = BTreeMap::new();
        let q = query(&p, None, None, &pins, "", true);
        assert_eq!(resolve_voice(&q), None);
    }

    #[test]
    fn empty_palette_auto_yields_default() {
        let p = Palette::default();
        let pins = BTreeMap::new();
        let q = query(&p, Some("coach"), None, &pins, "", true);
        assert_eq!(resolve_voice(&q), None);
    }

    #[test]
    fn parse_gender_pref_maps_any_to_no_filter() {
        assert_eq!(parse_gender_pref("male"), Some(Gender::Male));
        assert_eq!(parse_gender_pref("female"), Some(Gender::Female));
        assert_eq!(parse_gender_pref("any"), None);
        assert_eq!(parse_gender_pref(""), None);
        assert_eq!(parse_gender_pref("garbage"), None);
    }
}
