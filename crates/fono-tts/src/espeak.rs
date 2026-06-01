// SPDX-License-Identifier: GPL-3.0-only
//! Embedded espeak-ng grapheme-to-phoneme (G2P) core for the local voice
//! stack (feature `tts-local`).
//!
//! Piper turns text into audio in two stages: text → IPA phonemes (espeak-ng),
//! then phonemes → PCM (the neural VITS model). espeak-ng's phonemizer reads
//! its data from a directory. The full upstream payload is ~2.3 MiB — but the
//! text→IPA path touches only four small files, and never the 554 KB
//! `phondata` waveform body (that drives espeak's *own* synthesizer, which
//! Fono never runs). So Fono vendors just the G2P essentials and embeds them
//! in the binary (≈102 KiB, first `include_bytes!` in the tree — ADR 0033):
//!
//! | File | Role |
//! |------|------|
//! | `phontab` | phoneme name/attribute table (language-independent) |
//! | `phonindex` | phoneme bytecode index, incl. the IPA renderer's table |
//! | `intonations` | intonation contour data |
//! | `phondata` | **8-byte header only** — version magic + sample rate |
//!
//! Per-language `<lang>_dict` files are *not* embedded: they download on
//! demand from the voice mirror (see [`crate::voices`]) into the same data
//! directory, keeping the binary independent of language count.
//!
//! Regenerate the vendored bytes with `scripts/gen-espeak-core.sh` when the
//! pinned `espeak-ng` data version changes.

use std::path::Path;

use anyhow::{Context, Result};

/// The four G2P core files, embedded from `assets/espeak-core/` at build time.
const CORE_FILES: [(&str, &[u8]); 4] = [
    ("phontab", include_bytes!("../assets/espeak-core/phontab")),
    ("phonindex", include_bytes!("../assets/espeak-core/phonindex")),
    ("intonations", include_bytes!("../assets/espeak-core/intonations")),
    ("phondata", include_bytes!("../assets/espeak-core/phondata")),
];

/// Write the embedded espeak-ng G2P core into `data_dir`, creating it if
/// needed. Idempotent: existing core files are overwritten (cheap, ~102 KiB)
/// so an upgraded binary refreshes stale bytes; any `<lang>_dict` already
/// present (e.g. a downloaded dictionary) is left untouched.
///
/// After this, point `espeak_ng::Translator::new(lang, Some(data_dir))` at the
/// same directory — provided the matching `<lang>_dict` has been placed there.
pub fn install_core(data_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("create espeak data dir {}", data_dir.display()))?;
    for (name, bytes) in CORE_FILES {
        let dest = data_dir.join(name);
        std::fs::write(&dest, bytes)
            .with_context(|| format!("write espeak core file {}", dest.display()))?;
    }
    Ok(())
}

/// Map a Piper voice's `espeak.voice` code to the canonical espeak-ng base
/// language used for both phoneme-table lookup and dictionary naming.
///
/// Most codes pass through unchanged. A few Piper voices declare a code that
/// espeak-ng only resolves via its `lang/` voice-definition files — which Fono
/// strips from the embedded core (they aren't needed for G2P). Those are folded
/// onto the base language whose phoneme table *is* present, so phonemization
/// works against the ~102 KiB core alone:
///
/// - `nb` → `no` and `zh` → `cmn`: espeak language aliases (the voice-alias
///   names live only in the stripped `lang/` dir; the real tables are `no`/`cmn`).
/// - `en-gb-x-rp` → `en` and `es-419` → `es`: regional/extended variants with no
///   standalone phoneme table; the base table is the correct fallback.
///
/// The matching `<canonical>_dict` is what the catalog hosts and what
/// [`crate::voices::ensure_voice`] downloads, so this is also the dictionary
/// stem. Verified to phonemize cleanly for every catalog voice.
#[must_use]
pub fn canonical_lang(code: &str) -> &str {
    match code {
        "nb" => "no",
        "zh" => "cmn",
        "en-gb-x-rp" => "en",
        "es-419" => "es",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_lang_folds_known_variants_and_passes_through_the_rest() {
        assert_eq!(canonical_lang("nb"), "no");
        assert_eq!(canonical_lang("zh"), "cmn");
        assert_eq!(canonical_lang("en-gb-x-rp"), "en");
        assert_eq!(canonical_lang("es-419"), "es");
        // Unmapped codes (incl. variants espeak resolves itself) pass through.
        assert_eq!(canonical_lang("ro"), "ro");
        assert_eq!(canonical_lang("en-us"), "en-us");
        assert_eq!(canonical_lang("cmn"), "cmn");
    }

    /// `VERSION_PHDATA` the embedded `phondata` stub must carry in its first
    /// four little-endian bytes; mirrors the constant in `espeak-ng`. A
    /// mismatch means the vendored core drifted from the linked crate.
    const VERSION_PHDATA: u32 = 0x0001_4801;

    #[test]
    fn phondata_stub_is_an_eight_byte_header_with_expected_version() {
        let phondata = CORE_FILES.iter().find(|(n, _)| *n == "phondata").unwrap().1;
        assert_eq!(phondata.len(), 8, "phondata stub must be exactly the 8-byte header");
        let version = u32::from_le_bytes(phondata[0..4].try_into().unwrap());
        assert_eq!(
            version, VERSION_PHDATA,
            "vendored phondata stub version drifted from espeak-ng; \
             re-run scripts/gen-espeak-core.sh"
        );
        // Bytes 4-7 are the sample rate; every espeak voice is 22.05 kHz.
        let rate = u32::from_le_bytes(phondata[4..8].try_into().unwrap());
        assert_eq!(rate, 22050, "phondata stub sample rate must be 22050");
    }

    #[test]
    fn core_tables_are_nonempty() {
        for (name, bytes) in CORE_FILES {
            if name == "phondata" {
                continue;
            }
            assert!(!bytes.is_empty(), "embedded {name} is empty");
        }
    }

    #[test]
    fn install_core_writes_all_four_files() {
        let dir = tempfile::tempdir().unwrap();
        install_core(dir.path()).expect("install core");
        for name in ["phontab", "phonindex", "intonations", "phondata"] {
            let p = dir.path().join(name);
            assert!(p.is_file(), "{name} not written");
        }
        // A pre-existing dictionary must survive a re-install.
        let dict = dir.path().join("ro_dict");
        std::fs::write(&dict, b"sentinel").unwrap();
        install_core(dir.path()).expect("re-install core");
        assert_eq!(std::fs::read(&dict).unwrap(), b"sentinel", "dict clobbered by install_core");
    }
}
