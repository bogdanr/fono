// SPDX-License-Identifier: GPL-3.0-only
//! Pure mapping, capping, and on-disk caching for live TTS voice discovery.
//!
//! The networked probe lives in `fono-tts` (it needs `reqwest` and the
//! per-provider feature flags); everything that can be *wrong* — turning a
//! provider's JSON into a [`Palette`], bounding/balancing it, and persisting
//! the result — lives here so it is unit-testable without a network.
//!
//! The contract that keeps autodiscovery from ever breaking normal operation:
//! the read path ([`DiscoveredVoices::load`]) returns `None` on *any* error
//! (missing file, unreadable, malformed JSON), so a caller transparently falls
//! back to the curated catalogue palette. Refresh is the only fallible step,
//! and it only ever *adds* a cache file — it never mutates the running config.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::provider_catalog::{RawDiscoveredVoice, VoiceDiscovery};
use crate::voice_palette::{Gender, Palette, PaletteVoice};

/// Upper bound on a discovered palette. Matches the curated-palette cap so the
/// positional labels (`Female 1` … `Male N`) stay short and memorable.
pub const MAX_DISCOVERED_VOICES: usize = 10;

/// Map a provider's parsed voice-list JSON into raw voices using a
/// [`VoiceDiscovery`] descriptor. Uses the [`custom`](VoiceDiscovery::custom)
/// parser when present, otherwise the declarative field map. Voices with a
/// missing/empty id are skipped; a missing/unparseable gender ⇒
/// [`Gender::Neutral`].
#[must_use]
pub fn map_raw(body: &serde_json::Value, d: &VoiceDiscovery) -> Vec<RawDiscoveredVoice> {
    if let Some(parse) = d.custom {
        return parse(body);
    }
    let array = d.map.array_pointer.map_or(Some(body), |ptr| body.pointer(ptr));
    let Some(items) = array.and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let id = item.get(d.map.id_field)?.as_str()?.trim();
            if id.is_empty() {
                return None;
            }
            let name = d
                .map
                .name_field
                .and_then(|f| item.get(f))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let gender = d
                .map
                .gender_pointer
                .and_then(|p| item.pointer(p))
                .and_then(serde_json::Value::as_str)
                .and_then(Gender::parse)
                .unwrap_or(Gender::Neutral);
            Some(RawDiscoveredVoice { backend_id: id.to_string(), name, gender })
        })
        .collect()
}

/// Map + cap + balance in one step: the full descriptor-driven transform from
/// a response body to a bounded, deterministic [`Palette`].
#[must_use]
pub fn map_discovered(body: &serde_json::Value, d: &VoiceDiscovery, max: usize) -> Palette {
    let raw = map_raw(body, d);
    Palette::new(cap_and_balance(raw, max))
}

/// Bound a raw voice list to `max` entries, balanced across genders and
/// **deterministic** across refreshes.
///
/// Determinism matters: the resolver assigns a program a voice by hashing onto
/// the palette *by position*, so a stable order keeps a program's automatic
/// voice (and the `Female N` labels) stable between refreshes. We achieve it by
/// de-duplicating on `backend_id` and sorting each gender bucket by id, then
/// round-robin across Female → Male → Neutral until `max` is reached.
#[must_use]
pub fn cap_and_balance(raw: Vec<RawDiscoveredVoice>, max: usize) -> Vec<PaletteVoice> {
    if max == 0 {
        return Vec::new();
    }
    let mut female: Vec<PaletteVoice> = Vec::new();
    let mut male: Vec<PaletteVoice> = Vec::new();
    let mut neutral: Vec<PaletteVoice> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for v in raw {
        if seen.iter().any(|s| s == &v.backend_id) {
            continue;
        }
        seen.push(v.backend_id.clone());
        let voice = PaletteVoice::new(v.backend_id, v.gender);
        match voice.gender {
            Gender::Female => female.push(voice),
            Gender::Male => male.push(voice),
            Gender::Neutral => neutral.push(voice),
        }
    }
    for bucket in [&mut female, &mut male, &mut neutral] {
        bucket.sort_by(|a, b| a.backend_id.cmp(&b.backend_id));
    }
    // Round-robin so a capped palette keeps both genders represented rather
    // than filling up with whichever gender the provider listed first.
    let mut out: Vec<PaletteVoice> =
        Vec::with_capacity(max.min(female.len() + male.len() + neutral.len()));
    let mut queues = [female.into_iter(), male.into_iter(), neutral.into_iter()];
    let mut exhausted = [false; 3];
    while out.len() < max && !exhausted.iter().all(|&e| e) {
        for (i, q) in queues.iter_mut().enumerate() {
            if out.len() >= max {
                break;
            }
            match q.next() {
                Some(v) => out.push(v),
                None => exhausted[i] = true,
            }
        }
    }
    out
}

/// A single discovered voice, in a serialisable, backend-agnostic form
/// (`Gender` is stored as its canonical token).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredVoice {
    /// Backend-specific wire id.
    pub backend_id: String,
    /// Canonical gender token (`"female"` / `"male"` / `"neutral"`).
    pub gender: String,
}

/// The cached result of a successful discovery probe for one backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredVoices {
    /// Backend id this cache belongs to (e.g. `"elevenlabs"`).
    pub backend: String,
    /// Unix seconds at which the probe succeeded (for staleness checks).
    pub fetched_at_unix: u64,
    /// The bounded, ordered palette voices.
    pub voices: Vec<DiscoveredVoice>,
}

impl DiscoveredVoices {
    /// Build a cache record from a freshly discovered [`Palette`].
    #[must_use]
    pub fn from_palette(backend: &str, palette: &Palette, fetched_at_unix: u64) -> Self {
        Self {
            backend: backend.to_string(),
            fetched_at_unix,
            voices: palette
                .voices()
                .iter()
                .map(|v| DiscoveredVoice {
                    backend_id: v.backend_id.clone(),
                    gender: v.gender.as_str().to_string(),
                })
                .collect(),
        }
    }

    /// Reconstruct the runtime [`Palette`] (parsing each stored gender token,
    /// defaulting to [`Gender::Neutral`]).
    #[must_use]
    pub fn to_palette(&self) -> Palette {
        Palette::new(
            self.voices
                .iter()
                .map(|v| {
                    let gender = Gender::parse(&v.gender).unwrap_or(Gender::Neutral);
                    PaletteVoice::new(v.backend_id.clone(), gender)
                })
                .collect(),
        )
    }

    /// On-disk path for a backend's discovered-voices cache under
    /// `<cache_dir>/voices/discovered/<backend>.json`.
    #[must_use]
    pub fn cache_path(cache_dir: &Path, backend: &str) -> PathBuf {
        cache_dir.join("voices").join("discovered").join(format!("{backend}.json"))
    }

    /// Load a backend's cached palette. Returns `None` on **any** error
    /// (missing, unreadable, malformed) so callers fall back to the curated
    /// palette — discovery never breaks the read path.
    #[must_use]
    pub fn load(cache_dir: &Path, backend: &str) -> Option<Self> {
        let path = Self::cache_path(cache_dir, backend);
        let bytes = std::fs::read(&path).ok()?;
        let parsed: Self = serde_json::from_slice(&bytes).ok()?;
        // Guard against a mis-keyed file landing under the wrong name.
        if parsed.backend != backend || parsed.voices.is_empty() {
            return None;
        }
        Some(parsed)
    }

    /// Persist this cache record, creating the parent directory tree.
    ///
    /// # Errors
    /// Propagates filesystem / serialization errors so the caller can report
    /// a failed `fono voices discover` without corrupting an existing cache.
    pub fn save(&self, cache_dir: &Path) -> std::io::Result<()> {
        let path = Self::cache_path(cache_dir, &self.backend);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&path, json)
    }

    /// Age of this cache in seconds relative to `now_unix` (saturating, so a
    /// cache with a future timestamp reads as age 0 rather than underflowing).
    #[must_use]
    pub fn age_secs(&self, now_unix: u64) -> u64 {
        now_unix.saturating_sub(self.fetched_at_unix)
    }

    /// Whether this cache is older than `max_age_secs` and should be refreshed.
    #[must_use]
    pub fn is_stale(&self, now_unix: u64, max_age_secs: u64) -> bool {
        self.age_secs(now_unix) >= max_age_secs
    }
}

/// Default staleness threshold for the lazy `fono voices list` refresh: 24h.
pub const DISCOVERY_MAX_AGE_SECS: u64 = 24 * 60 * 60;

/// Current Unix time in seconds (0 if the clock predates the epoch).
#[must_use]
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_catalog::{KeyAuth, VoiceDiscovery, VoiceFieldMap};

    fn elevenlabs_descriptor() -> VoiceDiscovery {
        VoiceDiscovery {
            list_url: "https://example/v1/voices",
            auth: KeyAuth::Header("xi-api-key"),
            extra_headers: &[],
            map: VoiceFieldMap {
                array_pointer: Some("/voices"),
                id_field: "voice_id",
                name_field: Some("name"),
                gender_pointer: Some("/labels/gender"),
            },
            custom: None,
        }
    }

    fn cartesia_descriptor() -> VoiceDiscovery {
        VoiceDiscovery {
            list_url: "https://example/voices",
            auth: KeyAuth::Header("X-Api-Key"),
            extra_headers: &[],
            map: VoiceFieldMap {
                array_pointer: Some("/data"),
                id_field: "id",
                name_field: Some("name"),
                gender_pointer: Some("/gender"),
            },
            custom: None,
        }
    }

    #[test]
    fn maps_elevenlabs_nested_array_and_labels_gender() {
        let body = serde_json::json!({
            "voices": [
                { "voice_id": "aaa", "name": "Sarah", "labels": { "gender": "female" } },
                { "voice_id": "bbb", "name": "George", "labels": { "gender": "male" } },
            ]
        });
        let raw = map_raw(&body, &elevenlabs_descriptor());
        assert_eq!(raw.len(), 2);
        assert_eq!(raw[0].backend_id, "aaa");
        assert_eq!(raw[0].gender, Gender::Female);
        assert_eq!(raw[1].gender, Gender::Male);
        assert_eq!(raw[0].name.as_deref(), Some("Sarah"));
    }

    #[test]
    fn maps_cartesia_paginated_envelope_and_word_gender() {
        let body = serde_json::json!({
            "data": [
                { "id": "u1", "name": "Ona", "gender": "feminine" },
                { "id": "u2", "name": "Max", "gender": "masculine" },
                { "id": "u3", "name": "Sky", "gender": "gender_neutral" },
            ],
            "has_more": true,
            "next_page": "u3",
        });
        let raw = map_raw(&body, &cartesia_descriptor());
        assert_eq!(raw.len(), 3, "voices live under /data, not the root");
        assert_eq!(raw[0].backend_id, "u1");
        // Cartesia's feminine/masculine/gender_neutral now map correctly.
        assert_eq!(raw[0].gender, Gender::Female);
        assert_eq!(raw[1].gender, Gender::Male);
        assert_eq!(raw[2].gender, Gender::Neutral);
        assert_eq!(raw[0].name.as_deref(), Some("Ona"));
    }

    #[test]
    fn missing_gender_defaults_to_neutral_and_empty_ids_skipped() {
        let body = serde_json::json!({
            "voices": [
                { "voice_id": "x", "name": "No Gender" },
                { "voice_id": "", "name": "Empty Id" },
                { "name": "No Id" },
            ]
        });
        let raw = map_raw(&body, &elevenlabs_descriptor());
        assert_eq!(raw.len(), 1, "empty/missing ids are dropped");
        assert_eq!(raw[0].gender, Gender::Neutral);
    }

    #[test]
    fn custom_parser_takes_precedence() {
        fn parse(_body: &serde_json::Value) -> Vec<RawDiscoveredVoice> {
            vec![RawDiscoveredVoice {
                backend_id: "custom".into(),
                name: None,
                gender: Gender::Male,
            }]
        }
        let mut d = elevenlabs_descriptor();
        d.custom = Some(parse);
        let raw = map_raw(&serde_json::json!({}), &d);
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].backend_id, "custom");
    }

    fn raw(id: &str, g: Gender) -> RawDiscoveredVoice {
        RawDiscoveredVoice { backend_id: id.to_string(), name: None, gender: g }
    }

    #[test]
    fn cap_and_balance_is_deterministic_and_gender_balanced() {
        let input = vec![
            raw("f3", Gender::Female),
            raw("f1", Gender::Female),
            raw("f2", Gender::Female),
            raw("m2", Gender::Male),
            raw("m1", Gender::Male),
        ];
        let out = cap_and_balance(input.clone(), 4);
        let ids: Vec<_> = out.iter().map(|v| v.backend_id.clone()).collect();
        // Sorted within gender, then round-robin F,M,F,M.
        assert_eq!(ids, vec!["f1", "m1", "f2", "m2"]);
        // Same input ⇒ same output.
        let again: Vec<_> =
            cap_and_balance(input, 4).iter().map(|v| v.backend_id.clone()).collect();
        assert_eq!(ids, again);
    }

    #[test]
    fn cap_and_balance_dedups_and_respects_cap() {
        let input =
            vec![raw("a", Gender::Female), raw("a", Gender::Female), raw("b", Gender::Female)];
        let out = cap_and_balance(input, 10);
        assert_eq!(out.len(), 2, "duplicate backend_id removed");
        assert_eq!(cap_and_balance(vec![raw("a", Gender::Female)], 0).len(), 0);
    }

    #[test]
    fn cache_round_trips_through_palette() {
        let palette = Palette::new(vec![
            PaletteVoice::new("aaa", Gender::Female),
            PaletteVoice::new("bbb", Gender::Male),
        ]);
        let record = DiscoveredVoices::from_palette("elevenlabs", &palette, 123);
        assert_eq!(record.to_palette(), palette);
    }

    #[test]
    fn cache_save_load_and_corruption_falls_back_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let palette = Palette::new(vec![PaletteVoice::new("aaa", Gender::Female)]);
        let record = DiscoveredVoices::from_palette("cartesia", &palette, now_unix());
        record.save(dir.path()).unwrap();
        let loaded = DiscoveredVoices::load(dir.path(), "cartesia").unwrap();
        assert_eq!(loaded.voices.len(), 1);
        // Wrong backend ⇒ None.
        assert!(DiscoveredVoices::load(dir.path(), "elevenlabs").is_none());
        // Corrupt file ⇒ None (never an error).
        let path = DiscoveredVoices::cache_path(dir.path(), "cartesia");
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(DiscoveredVoices::load(dir.path(), "cartesia").is_none());
    }

    #[test]
    fn staleness_is_relative_and_saturating() {
        let palette = Palette::new(vec![PaletteVoice::new("aaa", Gender::Female)]);
        // fetched at t=1000.
        let record = DiscoveredVoices::from_palette("cartesia", &palette, 1000);
        // Fresh: 12h later, against a 24h window.
        assert!(!record.is_stale(1000 + 12 * 3600, DISCOVERY_MAX_AGE_SECS));
        assert_eq!(record.age_secs(1000 + 12 * 3600), 12 * 3600);
        // Stale: exactly 24h and beyond.
        assert!(record.is_stale(1000 + DISCOVERY_MAX_AGE_SECS, DISCOVERY_MAX_AGE_SECS));
        assert!(record.is_stale(1000 + DISCOVERY_MAX_AGE_SECS + 1, DISCOVERY_MAX_AGE_SECS));
        // A clock that went backwards (now < fetched_at) saturates to age 0,
        // so a cache never reads as stale on a backwards clock jump.
        assert_eq!(record.age_secs(500), 0);
        assert!(!record.is_stale(500, DISCOVERY_MAX_AGE_SECS));
    }
}
