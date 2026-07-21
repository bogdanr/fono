// SPDX-License-Identifier: GPL-3.0-only
//! Voice catalog + cache-aware resolver for the local ONNX voice stack
//! (feature `tts-local`).
//!
//! A small catalog (`voices/catalog.json`, embedded at build time) maps a
//! voice name or language to a pinned `.ort` model + `.onnx.json` config. The
//! bytes themselves live in the `fono-voice` mirror as release assets tagged
//! by ONNX Runtime ABI version (ADR 0033); [`ensure_voice`] downloads them on
//! first use, verifies each against the catalog's SHA-256, and caches them
//! under [`fono_core::paths::Paths::voices_dir`]. Subsequent runs verify the
//! cached file's hash and skip the network entirely.
//!
//! Engine policy (ADR 0033): Kokoro handles English; Piper handles every other
//! language. The router (plan task 2.4) consumes [`for_language`] /
//! [`by_name`]; this module owns only catalog lookup + fetch/verify/cache.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use fono_core::voice_palette::{Gender, Palette, PaletteVoice};
use serde::Deserialize;

/// Default mirror base URL: the `fono-voice` repo's release-download root.
/// The per-voice `release_tag` and asset `file` are appended to form the full
/// asset URL (see [`asset_url`]). Override via the `base_url` argument to
/// [`ensure_voice`] for forks, self-hosting, or a CDN (e.g. Cloudflare R2).
pub const DEFAULT_BASE_URL: &str = "https://github.com/bogdanr/fono-voice/releases/download";

/// The embedded catalog, parsed once on demand via [`catalog`].
const CATALOG_JSON: &str = include_str!("../voices/catalog.json");

#[derive(Debug, Clone, Deserialize)]
struct Catalog {
    voices: Vec<Voice>,
    /// Per-language espeak-ng `<lang>_dict` phonemizer dictionaries. Optional
    /// so older catalogs (and tests) parse without it; an empty list simply
    /// means "no dictionaries to fetch" and the per-voice ensure logs a
    /// warning when a voice's language has none.
    #[serde(default)]
    dicts: Vec<Dict>,
}

/// A single catalog entry: one voice's identity plus its two pinned assets.
#[derive(Debug, Clone, Deserialize)]
pub struct Voice {
    /// Canonical voice id, e.g. `"ro_RO-mihai-medium"`.
    pub name: String,
    /// Synthesis engine, `"piper"` or `"kokoro"`.
    pub engine: String,
    /// BCP-47-ish language code the voice speaks, e.g. `"ro"`, `"en"`.
    pub language: String,
    /// ONNX Runtime version the `.ort` model was converted for; must match
    /// the linked runtime ABI (ADR 0032).
    pub ort_version: String,
    /// Release tag the assets live under in the mirror, e.g. `"ort-1.24.2"`.
    pub release_tag: String,
    /// The `.ort` model asset. For Kokoro voices this is the shared
    /// model file (every Kokoro voice in the catalog points at the same
    /// `.ort`; the per-voice difference is the `style` pack below).
    pub model: Asset,
    /// The `.onnx.json` Piper config sidecar asset. Absent for Kokoro
    /// voices, which use a fixed built-in phoneme vocab and a `style`
    /// pack rather than a per-voice sidecar.
    #[serde(default)]
    pub config: Option<Asset>,
    /// Kokoro per-voice style pack: a raw little-endian `f32` `[510, 256]`
    /// tensor, one 256-d style vector per output token-count bucket.
    /// Absent for Piper voices.
    #[serde(default)]
    pub style: Option<Asset>,
    /// espeak-ng voice code driving the phonemizer accent. Piper reads
    /// this from its `.onnx.json`; Kokoro has no sidecar, so the catalog
    /// declares it here (e.g. `en-us` for `af_*`, `en-gb` for `bf_*`).
    /// Folded onto a canonical base for dict lookup (see
    /// [`crate::espeak::canonical_lang`]).
    #[serde(default)]
    pub espeak_voice: Option<String>,
    /// Optional explicit perceived gender (`"female"`/`"male"`/`"neutral"`).
    /// When unset it is derived for Kokoro voices from the `a?_`/`b?_`
    /// naming convention (see [`Voice::gender`]); Piper voices default to
    /// neutral unless declared.
    #[serde(default)]
    pub gender: Option<String>,
}

impl Voice {
    /// Perceived gender for this voice, for the friendly voice palette.
    ///
    /// Uses the explicit `gender` field when present; otherwise derives it
    /// for Kokoro voices from their naming convention — the second letter
    /// of the name encodes gender (`f` female, `m` male), e.g. `af_heart`,
    /// `am_michael`, `bf_emma`, `bm_lewis`. Anything else is
    /// [`Gender::Neutral`].
    #[must_use]
    pub fn gender(&self) -> Gender {
        if let Some(g) = self.gender.as_deref().and_then(Gender::parse) {
            return g;
        }
        if self.engine == "kokoro" {
            match self.name.as_bytes().get(1) {
                Some(b'f') => return Gender::Female,
                Some(b'm') => return Gender::Male,
                _ => {}
            }
        }
        Gender::Neutral
    }
}

/// A downloadable, SHA-256-pinned asset.
#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    /// Asset filename within the release, also the cached basename.
    pub file: String,
    /// Expected lowercase-hex SHA-256 (64 chars).
    pub sha256: String,
    /// Expected size in bytes (informational; not enforced).
    pub size: u64,
}

/// A per-language espeak-ng phonemizer dictionary, hosted on the mirror and
/// downloaded on demand into `voices_dir/espeak/` next to the embedded G2P
/// core (see [`crate::espeak`]). The shared core is in the binary; only the
/// language-specific `<lang>_dict` travels over the network, keeping the
/// binary independent of how many languages exist.
#[derive(Debug, Clone, Deserialize)]
pub struct Dict {
    /// espeak-ng voice code this dictionary serves, e.g. `"ro"`, `"cmn"`.
    /// Matched against the `espeak.voice` field of a voice's `.onnx.json`,
    /// which is *not* always the BCP-47 language (zh → `cmn`, no → `nb`).
    pub lang: String,
    /// Release tag the dict asset lives under in the mirror, e.g.
    /// `"espeak-ng-1.52"` (espeak data is versioned independently of voices).
    pub release_tag: String,
    /// The downloadable `<lang>_dict` asset (`file` is e.g. `ro_dict`).
    #[serde(flatten)]
    pub asset: Asset,
}

/// Resolved on-disk locations of a voice's assets after [`ensure_voice`].
#[derive(Debug, Clone)]
pub struct VoicePaths {
    /// Absolute path to the cached `.ort` model.
    pub model: PathBuf,
    /// Absolute path to the cached `.onnx.json` config sidecar (Piper
    /// voices only; `None` for Kokoro).
    pub config: Option<PathBuf>,
    /// Absolute path to the cached style pack (Kokoro voices only; `None`
    /// for Piper).
    pub style: Option<PathBuf>,
}

/// The embedded voice catalog. Parsed on each call (cheap; a handful of
/// entries) and validated — a malformed embedded catalog is a build-time bug
/// surfaced as an error here.
pub fn catalog() -> Result<Vec<Voice>> {
    let parsed: Catalog =
        serde_json::from_str(CATALOG_JSON).context("parse embedded voices/catalog.json")?;
    Ok(parsed.voices)
}

/// Look up a voice by its canonical `name`.
pub fn by_name(name: &str) -> Result<Option<Voice>> {
    Ok(catalog()?.into_iter().find(|v| v.name == name))
}

/// First catalog voice that speaks `language` (exact code match). The router
/// applies the Kokoro-for-English / Piper-for-the-rest policy on top; this is
/// the raw lookup.
pub fn for_language(language: &str) -> Result<Option<Voice>> {
    Ok(catalog()?.into_iter().find(|v| v.language == language))
}

/// First catalog voice that speaks `language` **and** uses `engine`
/// (`"piper"` / `"kokoro"`). Used when the user has pinned a single local
/// engine via `[tts.local].engine`, so voice routing stays within that
/// engine's catalog entries.
pub fn for_language_engine(language: &str, engine: &str) -> Result<Option<Voice>> {
    Ok(catalog()?.into_iter().find(|v| v.language == language && v.engine == engine))
}

/// Every catalog voice using `engine` (`"piper"` / `"kokoro"`), in catalog
/// order. Powers the settings UI's per-engine voice dropdowns.
pub fn for_engine(engine: &str) -> Result<Vec<Voice>> {
    Ok(catalog()?.into_iter().filter(|v| v.engine == engine).collect())
}

/// Build the local voice palette for the given active base languages
/// (e.g. `["en", "ro"]`). Mirrors the engine policy — Kokoro voices for
/// English, the Piper voice(s) for every other language — so the palette
/// contains exactly the voices a user could actually be routed to, each
/// tagged with its [`Gender`]. An empty `languages` slice includes every
/// catalog language. Order follows the catalog, which determines the
/// positional labels ("Female 1", "Male 2", …).
pub fn local_palette(languages: &[&str]) -> Result<Palette> {
    let voices = catalog()?;
    let mut out = Vec::new();
    for v in &voices {
        let active = languages.is_empty() || languages.contains(&v.language.as_str());
        if !active {
            continue;
        }
        // Kokoro for English, Piper for the rest (ADR 0033).
        let engine_ok = if v.language == "en" { v.engine == "kokoro" } else { v.engine == "piper" };
        if engine_ok {
            out.push(PaletteVoice::new(v.name.clone(), v.gender()));
        }
    }
    Ok(Palette::new(out))
}

/// All espeak-ng dictionaries declared in the catalog.
pub fn dicts() -> Result<Vec<Dict>> {
    let parsed: Catalog =
        serde_json::from_str(CATALOG_JSON).context("parse embedded voices/catalog.json")?;
    Ok(parsed.dicts)
}

/// Look up the espeak-ng dictionary for an espeak voice code (the
/// `espeak.voice` value from a voice's `.onnx.json`, e.g. `"ro"`).
pub fn dict_for(lang: &str) -> Result<Option<Dict>> {
    Ok(dicts()?.into_iter().find(|d| d.lang == lang))
}

/// Build the full download URL for an asset: `{base}/{release_tag}/{file}`.
#[must_use]
pub fn asset_url(base_url: &str, release_tag: &str, file: &str) -> String {
    format!("{}/{}/{}", base_url.trim_end_matches('/'), release_tag, file)
}

/// Ensure a voice's `.ort` model and `.onnx.json` config are present and
/// verified under `voices_dir`, downloading from the mirror if needed.
///
/// `base_url` overrides [`DEFAULT_BASE_URL`] (forks / self-hosting / CDN);
/// pass `None` for the default. For each asset: if the cached file already
/// matches the catalog SHA-256, the network is skipped; otherwise it is
/// (re)downloaded and verified by [`fono_download::download`].
pub async fn ensure_voice(
    voice: &Voice,
    voices_dir: &Path,
    base_url: Option<&str>,
) -> Result<VoicePaths> {
    let base = base_url.unwrap_or(DEFAULT_BASE_URL);
    // Box each fetch future so this fn's own stack frame holds pointers rather
    // than several inlined download state machines (otherwise their combined
    // size trips `clippy::large_stack_frames`).
    let model = Box::pin(fetch_asset(&voice.model, &voice.release_tag, base, voices_dir)).await?;
    // Piper carries a `.onnx.json` sidecar; Kokoro carries a style pack. Each
    // is optional in the schema, so fetch whichever the catalog declares.
    let config = match &voice.config {
        Some(c) => Some(Box::pin(fetch_asset(c, &voice.release_tag, base, voices_dir)).await?),
        None => None,
    };
    let style = match &voice.style {
        Some(s) => Some(Box::pin(fetch_asset(s, &voice.release_tag, base, voices_dir)).await?),
        None => None,
    };
    // Both engines phonemize with espeak-ng, which needs the matching
    // `<lang>_dict` beside the embedded G2P core. Piper declares its espeak
    // voice in the downloaded sidecar; Kokoro declares it in the catalog.
    if let Some(espeak_voice) = espeak_voice_for(voice, config.as_deref())? {
        Box::pin(ensure_dict(&espeak_voice, voices_dir, base)).await?;
    }
    Ok(VoicePaths { model, config, style })
}

/// Determine the espeak-ng voice code a voice phonemizes with: from the
/// downloaded Piper `.onnx.json` sidecar when present, otherwise from the
/// catalog's `espeak_voice` field (Kokoro). `None` means "no phonemizer
/// dictionary needed" (e.g. a config that declares no espeak voice).
fn espeak_voice_for(voice: &Voice, config_path: Option<&Path>) -> Result<Option<String>> {
    if let Some(path) = config_path {
        let bytes =
            std::fs::read(path).with_context(|| format!("read voice config {}", path.display()))?;
        return Ok(read_espeak_voice(&bytes));
    }
    Ok(voice.espeak_voice.clone().filter(|v| !v.is_empty()))
}

/// Ensure the espeak-ng dictionary for an espeak voice code is present under
/// `voices_dir/espeak/`. A language absent from the catalog logs an actionable
/// warning rather than failing the voice, so the model still loads and the gap
/// is visible.
async fn ensure_dict(espeak_voice: &str, voices_dir: &Path, base_url: &str) -> Result<()> {
    // Fold variant/alias codes onto the canonical base the catalog hosts a dict
    // for (e.g. nb→no, en-gb-x-rp→en, en-us→en); keeps one dict per base language.
    let lang = crate::espeak::canonical_lang(espeak_voice);
    let Some(dict) = dict_for(lang)? else {
        tracing::warn!(
            "no espeak dictionary for language {lang:?} in the catalog; local \
             phonemization for this voice will fail until a {lang:?} dict entry \
             is added to voices/catalog.json and uploaded to the mirror"
        );
        return Ok(());
    };
    let espeak_dir = voices_dir.join("espeak");
    fetch_asset(&dict.asset, &dict.release_tag, base_url, &espeak_dir).await?;
    Ok(())
}

/// Extract the `espeak.voice` code from a Piper `.onnx.json` config, if any.
/// Tolerates unrelated/extra fields and non-espeak configs (returns `None`).
fn read_espeak_voice(config_bytes: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct ConfigEspeak {
        espeak: Option<EspeakField>,
    }
    #[derive(Deserialize)]
    struct EspeakField {
        voice: Option<String>,
    }
    serde_json::from_slice::<ConfigEspeak>(config_bytes)
        .ok()
        .and_then(|c| c.espeak)
        .and_then(|e| e.voice)
        .filter(|v| !v.is_empty())
}

/// Fetch one asset into `voices_dir`, reusing a cached copy whose hash already
/// matches. Returns the absolute path to the verified file.
async fn fetch_asset(
    asset: &Asset,
    release_tag: &str,
    base_url: &str,
    voices_dir: &Path,
) -> Result<PathBuf> {
    if asset.sha256.len() != 64 || !asset.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("catalog asset '{}' has a malformed sha256", asset.file);
    }
    let dest = voices_dir.join(&asset.file);
    if dest.is_file() {
        let actual = fono_download::sha256_file(&dest)
            .await
            .with_context(|| format!("hash cached {}", dest.display()))?;
        if actual.eq_ignore_ascii_case(&asset.sha256) {
            tracing::debug!("voice asset {} present and verified (cache hit)", asset.file);
            return Ok(dest);
        }
        tracing::warn!("cached {} failed checksum; re-downloading", dest.display());
    }
    let url = asset_url(base_url, release_tag, &asset.file);
    fono_download::download(&url, &dest, &asset.sha256)
        .await
        .with_context(|| format!("download voice asset {url}"))?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_parses_and_is_nonempty() {
        let voices = catalog().expect("embedded catalog must parse");
        assert!(!voices.is_empty(), "catalog should ship at least one voice");
    }

    #[test]
    fn seed_romanian_voice_is_present_and_well_formed() {
        let v = by_name("ro_RO-mihai-medium").unwrap().expect("seed voice present");
        assert_eq!(v.engine, "piper");
        assert_eq!(v.language, "ro");
        assert_eq!(v.ort_version, "1.24.2");
        assert_eq!(v.release_tag, "ort-1.24.2");
        assert_eq!(v.model.file, "ro_RO-mihai-medium.ort");
        let config = v.config.as_ref().expect("piper voice must carry a config sidecar");
        assert_eq!(config.file, "ro_RO-mihai-medium.onnx.json");
        assert!(v.style.is_none(), "piper voice must not carry a Kokoro style pack");
        // Catalog SHA-256s must be canonical 64-char lowercase hex.
        for sha in [&v.model.sha256, &config.sha256] {
            assert_eq!(sha.len(), 64);
            assert!(sha.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn kokoro_english_voices_are_present_and_well_formed() {
        // af_heart is the default English voice and must win `for_language("en")`.
        let en = for_language("en").unwrap().expect("an English voice");
        assert_eq!(en.name, "af_heart", "af_heart must be the first English voice");
        assert_eq!(en.engine, "kokoro");

        for (name, accent) in [
            ("af_heart", "en-us"),
            ("af_bella", "en-us"),
            ("af_nicole", "en-us"),
            ("am_michael", "en-us"),
            ("bf_emma", "en-gb"),
            ("bm_lewis", "en-gb"),
        ] {
            let v = by_name(name).unwrap().unwrap_or_else(|| panic!("{name} present"));
            assert_eq!(v.engine, "kokoro");
            assert_eq!(v.language, "en");
            // Kokoro voices share the model and carry a per-voice style pack
            // instead of a `.onnx.json` config sidecar.
            assert!(v.config.is_none(), "{name} must not carry a Piper config");
            let style = v.style.as_ref().unwrap_or_else(|| panic!("{name} has a style pack"));
            assert_eq!(style.file, format!("{name}.style.bin"));
            assert_eq!(style.sha256.len(), 64);
            assert!(style.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            assert_eq!(v.espeak_voice.as_deref(), Some(accent));
        }
    }

    #[test]
    fn all_kokoro_voices_share_one_model() {
        let models: std::collections::BTreeSet<String> = catalog()
            .unwrap()
            .into_iter()
            .filter(|v| v.engine == "kokoro")
            .map(|v| v.model.file)
            .collect();
        assert_eq!(models.len(), 1, "every Kokoro voice must point at the same shared model");
    }

    #[test]
    fn kokoro_gender_is_derived_from_naming_convention() {
        for (name, expected) in [
            ("af_heart", Gender::Female),
            ("af_bella", Gender::Female),
            ("af_nicole", Gender::Female),
            ("bf_emma", Gender::Female),
            ("am_michael", Gender::Male),
            ("bm_lewis", Gender::Male),
        ] {
            let v = by_name(name).unwrap().unwrap_or_else(|| panic!("{name} present"));
            assert_eq!(v.gender(), expected, "derived gender for {name}");
        }
        // Piper voices have no naming convention → neutral.
        let piper = by_name("ro_RO-mihai-medium").unwrap().unwrap();
        assert_eq!(piper.gender(), Gender::Neutral);
    }

    #[test]
    fn local_palette_for_english_is_kokoro_and_gender_labelled() {
        let palette = local_palette(&["en"]).unwrap();
        // Six Kokoro voices, no Piper English voices.
        let ids: Vec<&str> = palette.voices().iter().map(|v| v.backend_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["af_heart", "af_bella", "af_nicole", "bf_emma", "am_michael", "bm_lewis"]
        );
        // Positional gendered labels resolve back to intrinsic ids.
        assert_eq!(palette.by_label("Female 1").unwrap().backend_id, "af_heart");
        assert_eq!(palette.by_label("Male 1").unwrap().backend_id, "am_michael");
        assert_eq!(palette.by_label("Male 2").unwrap().backend_id, "bm_lewis");
        assert_eq!(palette.by_label("Female 4").unwrap().backend_id, "bf_emma");
        // Romanian (Piper) contributes a single neutral voice.
        let ro = local_palette(&["ro"]).unwrap();
        assert_eq!(ro.voices().len(), 1);
        assert_eq!(ro.voices()[0].backend_id, "ro_RO-mihai-medium");
        assert_eq!(ro.voices()[0].gender, Gender::Neutral);
    }

    #[test]
    fn language_lookup_resolves_romanian() {
        let v = for_language("ro").unwrap().expect("ro voice");
        assert_eq!(v.name, "ro_RO-mihai-medium");
        assert!(for_language("xx").unwrap().is_none(), "unknown language yields None");
    }

    #[test]
    fn seed_romanian_dict_is_present_and_well_formed() {
        let d = dict_for("ro").unwrap().expect("ro dict present in catalog");
        assert_eq!(d.asset.file, "ro_dict");
        assert_eq!(d.release_tag, "espeak-ng-1.52");
        // Flattened asset fields must parse from the top-level dict object.
        assert_eq!(d.asset.sha256.len(), 64);
        assert!(d.asset.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(d.asset.size > 0);
        // An espeak voice code with no catalog dict resolves to None, which the
        // ensure path treats as a warning rather than a hard failure.
        assert!(dict_for("xx").unwrap().is_none());
    }

    #[test]
    fn all_catalog_dicts_are_well_formed_and_named_by_their_stem() {
        let dicts = dicts().expect("catalog dicts parse");
        assert!(dicts.len() >= 38, "expected the full per-language dict set");
        for d in &dicts {
            assert_eq!(d.asset.file, format!("{}_dict", d.lang), "dict file must be <lang>_dict");
            assert_eq!(d.release_tag, "espeak-ng-1.52");
            assert_eq!(d.asset.sha256.len(), 64, "{} sha must be 64 hex", d.lang);
            assert!(d.asset.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(d.asset.size > 0, "{} dict size must be > 0", d.lang);
        }
    }

    #[test]
    fn canonical_lang_targets_all_have_a_catalog_dict() {
        // Every base language the canonicalizer folds onto must be hostable.
        for code in ["nb", "zh", "en-us", "en-gb-x-rp", "es-419", "ro", "de", "fr"] {
            let canon = crate::espeak::canonical_lang(code);
            assert!(
                dict_for(canon).unwrap().is_some(),
                "canonical lang {canon:?} (from {code:?}) has no catalog dict"
            );
        }
    }

    #[test]
    fn asset_url_joins_without_double_slashes() {
        assert_eq!(
            asset_url("https://example.test/dl/", "ort-1.24.2", "v.ort"),
            "https://example.test/dl/ort-1.24.2/v.ort"
        );
        assert_eq!(
            asset_url("https://example.test/dl", "ort-1.24.2", "v.ort"),
            "https://example.test/dl/ort-1.24.2/v.ort"
        );
    }

    /// A cached file whose hash matches the catalog is returned without any
    /// network access (the URL points at an unroutable host, so a download
    /// attempt would fail). Proves the cache-hit short-circuit.
    #[tokio::test]
    async fn cached_asset_with_matching_hash_skips_download() {
        let dir = tempfile::tempdir().unwrap();
        let body = b"hello fono voice";
        let sha = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(body);
            hex::encode(h.finalize())
        };
        let asset = Asset { file: "probe.bin".into(), sha256: sha, size: body.len() as u64 };
        std::fs::write(dir.path().join("probe.bin"), body).unwrap();

        let got = fetch_asset(&asset, "ort-1.24.2", "http://127.0.0.1:1/never", dir.path())
            .await
            .expect("cache hit must succeed without network");
        assert_eq!(got, dir.path().join("probe.bin"));
    }

    /// A malformed catalog SHA-256 is rejected before any network attempt.
    #[tokio::test]
    async fn malformed_sha_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let asset = Asset { file: "x.bin".into(), sha256: "nothex".into(), size: 0 };
        let err = fetch_asset(&asset, "ort-1.24.2", DEFAULT_BASE_URL, dir.path())
            .await
            .expect_err("malformed sha must error");
        assert!(err.to_string().contains("malformed sha256"));
    }
}
