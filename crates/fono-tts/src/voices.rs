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
    let model = fetch_asset(&voice.model, &voice.release_tag, base, voices_dir).await?;
    let config = fetch_asset(&voice.config, &voice.release_tag, base, voices_dir).await?;
    Ok(VoicePaths { model, config })
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
