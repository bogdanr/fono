// SPDX-License-Identifier: GPL-3.0-only
//! Public-domain dictation fixtures.
//!
//! Each entry is a stable HTTPS URL, a SHA-256 pin, a canonical reference
//! transcript, and the spoken language. The runner refuses to execute
//! against a fixture whose on-disk SHA-256 doesn't match the pin — this
//! makes "swap in a different recording" a deliberate, commit-visible
//! act.
//!
//! ## Sourcing rules
//!
//! Audio MUST be in the public domain or CC0. Acceptable sources:
//!   * LibriVox audiobooks (audio is PD; `archive.org` direct links are
//!     stable).
//!   * Wikimedia Commons voice samples tagged CC0 / PD.
//!   * The CC0 portion of Mozilla Common Voice.
//!
//! Audio must NOT be CC-BY, CC-BY-SA, or any "non-commercial" variant —
//! benchmarks must remain GPL-3.0-redistributable.
//!
//! ## Format expectations
//!
//! After `scripts/fetch-fixtures.sh` runs, every fixture lives at
//! `${XDG_CACHE_HOME:-$HOME/.cache}/fono/bench/<id>.wav` as 16 kHz mono
//! 16-bit PCM WAV. The fetch script is responsible for trimming and
//! resampling source audio with `ffmpeg`; the runner only validates.
//!
//! ## Filling in real pins
//!
//! The entries below ship with `sha256: UNPINNED` (64 zeros) until a
//! maintainer runs the fetch script and commits the resulting pins.
//! `BenchRunner::run_one` accepts unpinned fixtures with a logged
//! warning so first-time benchmarking works on a fresh checkout, but CI
//! refuses unpinned fixtures (see `tests/latency_smoke.rs`).

#[derive(Debug, Clone)]
pub struct Fixture {
    /// Stable identifier; doubles as the cache filename (`<id>.wav`).
    pub id: &'static str,
    /// BCP-47 language tag (`en`, `es`, `fr`, `de`, `it`, `ro`, …).
    pub language: &'static str,
    /// HTTPS URL to the WAV (or to source audio that the fetch script
    /// transcodes into a WAV).
    pub url: &'static str,
    /// SHA-256 of the *final* `<id>.wav` after transcode. 64 zeros = unpinned.
    pub sha256: &'static str,
    /// Canonical reference transcript, exactly as spoken.
    pub transcript: &'static str,
    /// Approximate duration in seconds, for sanity checks.
    pub approx_duration_s: f32,
    /// Source attribution string (LibriVox URL, Wikimedia Commons file,
    /// Common Voice clip ID, etc.) — surfaced in the JSON report so the
    /// provenance of any benchmark number is greppable.
    pub attribution: &'static str,
}

pub const UNPINNED: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Public-domain dictation clips covering English, Spanish, French,
/// German, Italian, and Romanian.
///
/// The URLs below point at LibriVox / Wikimedia Commons sources that the
/// `scripts/fetch-fixtures.sh` helper transcodes to 16 kHz mono 16-bit
/// PCM WAV. Maintainers MUST replace the unpinned SHA-256 placeholders
/// with the real digests on first fetch (`sha256sum` of the produced WAV).
pub const FIXTURES: &[Fixture] = &[
    // ---- English ----
    Fixture {
        id: "en_alice_01",
        language: "en",
        url: "https://archive.org/download/alice_in_wonderland_librivox/wonderland_ch_01_carroll.mp3",
        sha256: UNPINNED,
        transcript: "alice was beginning to get very tired of sitting by her sister on the bank \
                     and of having nothing to do",
        approx_duration_s: 6.4,
        attribution: "LibriVox / Lewis Carroll, Alice in Wonderland Ch.1 (public domain)",
    },
    Fixture {
        id: "en_pride_01",
        language: "en",
        url: "https://archive.org/download/pride_prejudice_0711_librivox/prideandprejudice_01_austen.mp3",
        sha256: UNPINNED,
        transcript: "it is a truth universally acknowledged that a single man in possession of \
                     a good fortune must be in want of a wife",
        approx_duration_s: 7.8,
        attribution: "LibriVox / Jane Austen, Pride and Prejudice Ch.1 (public domain)",
    },
    // ---- Spanish ----
    Fixture {
        id: "es_quijote_01",
        language: "es",
        url: "https://archive.org/download/don_quijote_de_la_mancha_1010_librivox/donquijote_01_cervantes.mp3",
        sha256: UNPINNED,
        transcript: "en un lugar de la mancha de cuyo nombre no quiero acordarme no ha mucho \
                     tiempo que vivía un hidalgo",
        approx_duration_s: 7.2,
        attribution: "LibriVox / Miguel de Cervantes, Don Quijote (public domain)",
    },
    Fixture {
        id: "es_platero_01",
        language: "es",
        url: "https://archive.org/download/platero_y_yo_jr_librivox/platero_01_jimenez.mp3",
        sha256: UNPINNED,
        transcript: "platero es pequeño peludo suave tan blando por fuera que se diría todo de \
                     algodón que no lleva huesos",
        approx_duration_s: 8.1,
        attribution: "LibriVox / Juan Ramón Jiménez, Platero y yo (public domain)",
    },
    // ---- French ----
    Fixture {
        id: "fr_petitprince_01",
        language: "fr",
        url: "https://archive.org/download/petit_prince_librivox/lepetitprince_01.mp3",
        sha256: UNPINNED,
        transcript: "lorsque j'avais six ans j'ai vu une fois une magnifique image dans un livre \
                     sur la forêt vierge",
        approx_duration_s: 7.0,
        attribution: "LibriVox / Antoine de Saint-Exupéry (public domain in source jurisdiction)",
    },
    Fixture {
        id: "fr_candide_01",
        language: "fr",
        url: "https://archive.org/download/candide_voltaire_librivox/candide_01_voltaire.mp3",
        sha256: UNPINNED,
        transcript: "il y avait en westphalie dans le château de monsieur le baron de \
                     thunder-ten-tronckh un jeune garçon",
        approx_duration_s: 7.5,
        attribution: "LibriVox / Voltaire, Candide (public domain)",
    },
    // ---- German ----
    Fixture {
        id: "de_grimm_01",
        language: "de",
        url: "https://archive.org/download/grimms_maerchen_librivox/grimm_01.mp3",
        sha256: UNPINNED,
        transcript: "es war einmal eine kleine süße dirne die hatte jedermann lieb der sie nur \
                     ansah",
        approx_duration_s: 6.9,
        attribution: "LibriVox / Brüder Grimm, Rotkäppchen (public domain)",
    },
    Fixture {
        id: "de_kafka_01",
        language: "de",
        url: "https://archive.org/download/verwandlung_kafka_librivox/verwandlung_01_kafka.mp3",
        sha256: UNPINNED,
        transcript: "als gregor samsa eines morgens aus unruhigen träumen erwachte fand er sich \
                     in seinem bett zu einem ungeheuren ungeziefer verwandelt",
        approx_duration_s: 8.6,
        attribution: "LibriVox / Franz Kafka, Die Verwandlung (public domain)",
    },
    // ---- Italian ----
    Fixture {
        id: "it_pinocchio_01",
        language: "it",
        url: "https://archive.org/download/pinocchio_collodi_librivox/pinocchio_01_collodi.mp3",
        sha256: UNPINNED,
        transcript: "c'era una volta un pezzo di legno non era un legno di lusso ma un semplice \
                     pezzo da catasta",
        approx_duration_s: 7.4,
        attribution: "LibriVox / Carlo Collodi, Pinocchio (public domain)",
    },
    // ---- Romanian (NimbleX user base) ----
    Fixture {
        id: "ro_eminescu_01",
        language: "ro",
        url: "https://archive.org/download/poezii_eminescu_librivox/luceafarul_eminescu.mp3",
        sha256: UNPINNED,
        transcript: "a fost odată ca în povești a fost ca niciodată din rude mari împărătești o \
                     prea frumoasă fată",
        approx_duration_s: 8.2,
        attribution: "LibriVox / Mihai Eminescu, Luceafărul (public domain)",
    },
];

impl Fixture {
    /// Cache path under the user-supplied bench root.
    pub fn cache_path(&self, bench_root: &std::path::Path) -> std::path::PathBuf {
        bench_root.join(format!("{}.wav", self.id))
    }

    /// Look up a fixture by its `id`.
    #[must_use]
    pub fn by_id(id: &str) -> Option<&'static Fixture> {
        FIXTURES.iter().find(|f| f.id == id)
    }

    /// Filter fixtures by language tag (case-insensitive).
    pub fn by_language(lang: &str) -> impl Iterator<Item = &'static Fixture> {
        let lang_lower = lang.to_ascii_lowercase();
        FIXTURES
            .iter()
            .filter(move |f| f.language.eq_ignore_ascii_case(&lang_lower))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_covers_at_least_four_languages() {
        let mut langs: Vec<_> = FIXTURES.iter().map(|f| f.language).collect();
        langs.sort_unstable();
        langs.dedup();
        assert!(
            langs.len() >= 4,
            "the bench plan calls for ≥ 4 languages, got {langs:?}"
        );
    }

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<_> = FIXTURES.iter().map(|f| f.id).collect();
        ids.sort_unstable();
        let len = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), len, "duplicate fixture id");
    }

    #[test]
    fn transcripts_non_empty_and_lowercase() {
        for f in FIXTURES {
            assert!(!f.transcript.is_empty(), "{} has empty transcript", f.id);
            assert!(
                f.transcript.chars().all(|c| !c.is_ascii_uppercase()),
                "{} transcript must be lowercase (WER normalises but commit-time hygiene helps)",
                f.id
            );
        }
    }
}
