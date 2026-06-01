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
    /// The `.ort` model asset.
    pub model: Asset,
    /// The `.onnx.json` Piper config sidecar asset.
    pub config: Asset,
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
    /// Absolute path to the cached `.onnx.json` config sidecar.
    pub config: PathBuf,
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
    // than three inlined download state machines (otherwise their combined size
    // trips `clippy::large_stack_frames`).
    let model = Box::pin(fetch_asset(&voice.model, &voice.release_tag, base, voices_dir)).await?;
    let config = Box::pin(fetch_asset(&voice.config, &voice.release_tag, base, voices_dir)).await?;
    // Piper voices phonemize with espeak-ng, which needs the matching
    // `<lang>_dict` beside the embedded G2P core. Fetch it on first use.
    Box::pin(ensure_voice_dict(&config, voices_dir, base)).await?;
    Ok(VoicePaths { model, config })
}

/// Ensure the espeak-ng dictionary for a voice (read from its downloaded
/// `.onnx.json`) is present under `voices_dir/espeak/`. A voice whose config
/// declares no espeak voice (e.g. a future non-espeak engine) is a no-op; a
/// language absent from the catalog logs an actionable warning rather than
/// failing the voice, so the model still loads and the gap is visible.
async fn ensure_voice_dict(config_path: &Path, voices_dir: &Path, base_url: &str) -> Result<()> {
    let bytes = std::fs::read(config_path)
        .with_context(|| format!("read voice config {}", config_path.display()))?;
    let Some(lang) = read_espeak_voice(&bytes) else {
        return Ok(());
    };
    // Fold variant/alias codes onto the canonical base the catalog hosts a dict
    // for (e.g. nb→no, en-gb-x-rp→en); keeps one dict per base language.
    let lang = crate::espeak::canonical_lang(&lang);
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
        assert_eq!(v.config.file, "ro_RO-mihai-medium.onnx.json");
        // Catalog SHA-256s must be canonical 64-char lowercase hex.
        for sha in [&v.model.sha256, &v.config.sha256] {
            assert_eq!(sha.len(), 64);
            assert!(sha.bytes().all(|b| b.is_ascii_hexdigit()));
        }
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
        for code in ["nb", "zh", "en-gb-x-rp", "es-419", "ro", "de", "fr"] {
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
