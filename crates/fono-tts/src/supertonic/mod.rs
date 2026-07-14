// SPDX-License-Identifier: GPL-3.0-only
//! Supertonic 3 local TTS engine (feature `tts-local`).
//!
//! Slice 1 of `plans/2026-07-12-supertonic3-local-tts-engine-v1.md`: model
//! distribution. This module owns the pack descriptor + on-demand
//! fetch/verify/cache; the engine core (config/style/frontend/chunker/engine)
//! lands in later slices under this same directory.
//!
//! Supertonic is **one shared pack** — four graphs plus three data files —
//! that serves all 31 languages and 10 speakers from a single download
//! (unlike Piper's per-language voices). Distribution mirrors the rest of the
//! voice stack exactly (ADR 0033, `crate::voices`; `fono_audio::wake_registry`):
//! every asset is a SHA-256-pinned file hosted on the **`fono-voice`** release
//! mirror under an ONNX-Runtime-ABI tag (`ort-<version>`), fetched + verified
//! through [`fono_download::download`], reusing a cached copy whose hash
//! already matches.
//!
//! **Why `.ort` (not the upstream `.onnx`):** the shipped binary links the
//! *minimal* onnxruntime (ADR 0032/0033), which loads **only** `.ort`
//! flatbuffer models, never plain `.onnx`. The upstream Supertonic pack ships
//! four `*.int8.onnx` graphs; Fono converts them to `.ort` via
//! `scripts/gen-ort-models.sh` (the same pipeline as Piper/Kokoro/wake) and
//! hosts the conversions on its own mirror. That conversion step also emits
//! the operator/type union feeding the minimal-runtime rebuild (Slice 3), so
//! the four graph pins below stay [`UNPINNED`] until that conversion is run
//! and the `.ort` files are uploaded — the same "hosted-later" convention the
//! wake `hey_fono` classifier uses. The three non-graph files (`tts.json`,
//! `voice.bin`, `unicode_indexer.bin`) are byte-identical before and after
//! conversion, so they are pinned now.
//!
//! **License (ADR 0004, amended 2026-07-12):** Supertonic's code is MIT and
//! its weights are **OpenRAIL-M** — a RAIL-class behavioral-restriction
//! license, which the amended policy makes *default-eligible* provided the
//! restrictions are behavioral-only and the download names + links the
//! license. [`license_notice`] is that notice; the weights are never bundled
//! in the GPL binary, only downloaded as runtime data.
//!
//! **Coexistence / eviction:** the pack lives in its own [`supertonic_dir`]
//! subdirectory of the voices cache and is independent of the per-voice Piper
//! `.ort` files and the shared Kokoro model — enabling Supertonic does not
//! evict them, and they remain the fallback for the languages Supertonic
//! lacks (`crate::local_router`). The pack is a single ~140 MiB download
//! shared across every language/speaker; removing it is a matter of deleting
//! [`supertonic_dir`], and nothing else in the cache depends on it.

pub mod chunker;
pub mod config;
pub mod engine;
pub mod frontend;
mod nfkd_table;
pub mod style;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// SHA-256 sentinel for assets without a pinned digest yet — same convention
/// and value as `fono_audio::wake_registry::UNPINNED` and the downloader's
/// all-zeros "unpinned" check. The downloader logs the computed hash and
/// accepts the file; tighten to a real pin once the `.ort` conversion is run
/// and the artifact is hosted.
pub const UNPINNED: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// ONNX Runtime version the Supertonic `.ort` graphs are converted for. Must
/// match the linked runtime ABI (ADR 0032) — the same value the voice stack,
/// wake stack, and `scripts/fetch-onnxruntime.sh` pin.
pub const ORT_VERSION: &str = "1.24.2";

/// Release tag the Supertonic assets live under on the mirror, named for the
/// ONNX Runtime ABI (ADR 0033), e.g. `ort-1.24.2`. The `.ort` flatbuffer
/// schema is ABI-coupled, so the pack shares the voice stack's tag.
pub const RELEASE_TAG: &str = "ort-1.24.2";

/// Default mirror base URL: the `fono-voice` repo's release-download root —
/// the **same** mirror the local voices and wake models use (ADR 0033). The
/// [`RELEASE_TAG`] and per-asset `file` are appended to form the full URL
/// (see [`asset_url`]). Override via the `base_url` argument to
/// [`ensure_pack`] for forks, self-hosting, or a CDN.
pub const DEFAULT_BASE_URL: &str = "https://github.com/bogdanr/fono-voice/releases/download";

/// Upstream weights license SPDX-ish id, surfaced by [`license_notice`] and
/// `fono doctor`. Supertonic code is MIT; the *weights* are OpenRAIL-M.
pub const WEIGHTS_LICENSE: &str = "OpenRAIL-M";

/// Canonical link to the weights license text, named in [`license_notice`].
pub const WEIGHTS_LICENSE_URL: &str =
    "https://huggingface.co/spaces/CompVis/stable-diffusion-license/resolve/main/license.txt";

/// One downloadable file in the Supertonic pack: its on-disk basename (also
/// the mirror asset name) and its pinned lowercase-hex SHA-256 (or
/// [`UNPINNED`]). The release tag is shared ([`RELEASE_TAG`]); the full URL is
/// built at fetch time via [`asset_url`], so forks/self-hosting only swap the
/// base. Mirrors `crate::voices::Asset` / `wake_registry::WakeAsset`.
#[derive(Debug, Clone, Copy)]
pub struct SupertonicAsset {
    /// Cache basename == mirror asset name, e.g. `text_encoder.ort`.
    pub file: &'static str,
    /// Lowercase-hex SHA-256, or [`UNPINNED`] for a not-yet-hosted artifact.
    pub sha256: &'static str,
}

/// The four ONNX graphs, converted to `.ort` for the minimal runtime. Pins
/// stay [`UNPINNED`] until `scripts/gen-ort-models.sh` converts the upstream
/// `*.int8.onnx` graphs and the `.ort` files are uploaded to the mirror.
pub const GRAPHS: &[SupertonicAsset] = &[
    SupertonicAsset { file: "text_encoder.ort", sha256: UNPINNED },
    SupertonicAsset { file: "vector_estimator.ort", sha256: UNPINNED },
    SupertonicAsset { file: "vocoder.ort", sha256: UNPINNED },
    SupertonicAsset { file: "duration_predictor.ort", sha256: UNPINNED },
];

/// The Supertonic runtime config (`ae`/`ttl`/`dp` shapes). Byte-identical
/// before/after `.ort` conversion, so pinned now from the upstream int8 pack
/// `sherpa-onnx-supertonic-3-tts-int8-2026-05-11`.
pub const CONFIG: SupertonicAsset = SupertonicAsset {
    file: "tts.json",
    sha256: "42078d3aef1cd43ab43021f3c54f47d2d75ceb4e75f627f118890128b06a0d09",
};

/// The per-speaker style vectors (`voice.bin`): a 6×i64 header + two f32
/// payloads. Byte-identical before/after conversion — pinned now.
pub const VOICE: SupertonicAsset = SupertonicAsset {
    file: "voice.bin",
    sha256: "67d5209b0ee8ce6c74105ffbe12fe6a7628aea3b4ba2fcb308a4a67938a93ce8",
};

/// The flat `int32[65536]` BMP → token-id lookup table (`unicode_indexer.bin`).
/// Byte-identical before/after conversion — pinned now.
pub const INDEXER: SupertonicAsset = SupertonicAsset {
    file: "unicode_indexer.bin",
    sha256: "8402ca48e5189a8950138580b0fff64db6f072f24ac07cd54ba8b2fbb9883b30",
};

/// Every asset in the pack, graphs first, in fetch order.
#[must_use]
pub fn assets() -> Vec<SupertonicAsset> {
    let mut all = GRAPHS.to_vec();
    all.push(CONFIG);
    all.push(VOICE);
    all.push(INDEXER);
    all
}

/// `true` once every asset in the pack carries a real (non-[`UNPINNED`]) pin —
/// i.e. the `.ort` conversion has been run and the graphs uploaded. Used by
/// `fono doctor` / the wizard to gate offering the engine.
#[must_use]
pub fn is_hosted() -> bool {
    assets().iter().all(|a| !a.sha256.chars().all(|c| c == '0'))
}

/// The OpenRAIL-M notice shown before the pack is downloaded and recorded in
/// the model metadata (ADR 0004 default-eligibility requirement). Informs; it
/// does not gate — the notice pattern from the wake community models
/// (`wake_registry::license_notice`).
#[must_use]
pub fn license_notice() -> String {
    format!(
        "The Supertonic 3 voice pack has MIT-licensed code and weights licensed \
         under {WEIGHTS_LICENSE} (an OpenRAIL-M behavioral-use license: free \
         use, modification, redistribution, and commercial use, with \
         restrictions only on harmful/illegal use). The weights are NOT part \
         of Fono and are NOT bundled in the binary — they are downloaded as \
         runtime data. Full license text: {WEIGHTS_LICENSE_URL}"
    )
}

/// Build the full download URL for an asset: `{base}/{RELEASE_TAG}/{file}`.
/// Identical join to `crate::voices::asset_url`.
#[must_use]
pub fn asset_url(base_url: &str, file: &str) -> String {
    format!("{}/{}/{}", base_url.trim_end_matches('/'), RELEASE_TAG, file)
}

/// The cache subdirectory the pack's files live in: a `supertonic/` folder
/// under the shared voices cache. Keeps the ~140 MiB shared pack tidily apart
/// from the flat per-voice Piper/Kokoro files (see module docs on eviction).
#[must_use]
pub fn supertonic_dir(voices_dir: &Path) -> PathBuf {
    voices_dir.join("supertonic")
}

/// Resolved on-disk paths for the fetched pack. Returned by [`ensure_pack`]
/// and computable without touching the network via [`resolved_paths`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupertonicPaths {
    pub text_encoder: PathBuf,
    pub vector_estimator: PathBuf,
    pub vocoder: PathBuf,
    pub duration_predictor: PathBuf,
    pub config: PathBuf,
    pub voice: PathBuf,
    pub indexer: PathBuf,
}

/// Compute where the pack's files would live on disk, without downloading.
#[must_use]
pub fn resolved_paths(voices_dir: &Path) -> SupertonicPaths {
    let dir = supertonic_dir(voices_dir);
    SupertonicPaths {
        text_encoder: dir.join(GRAPHS[0].file),
        vector_estimator: dir.join(GRAPHS[1].file),
        vocoder: dir.join(GRAPHS[2].file),
        duration_predictor: dir.join(GRAPHS[3].file),
        config: dir.join(CONFIG.file),
        voice: dir.join(VOICE.file),
        indexer: dir.join(INDEXER.file),
    }
}

/// Ensure the whole Supertonic pack is present and verified under
/// `voices_dir/supertonic/`, downloading from the mirror if needed.
///
/// Logs [`license_notice`] once before fetching (so the daemon log records the
/// OpenRAIL-M terms of the downloaded weights) and then fetches every asset,
/// reusing cached files whose hash already matches. `base_url` overrides
/// [`DEFAULT_BASE_URL`] (forks / self-hosting / CDN); pass `None` for the
/// default mirror.
pub async fn ensure_pack(voices_dir: &Path, base_url: Option<&str>) -> Result<SupertonicPaths> {
    tracing::info!("{}", license_notice());
    let base = base_url.unwrap_or(DEFAULT_BASE_URL);
    let dir = supertonic_dir(voices_dir);
    // Box each fetch future so this frame holds pointers rather than several
    // inlined download state machines (avoids `clippy::large_stack_frames`),
    // mirroring `crate::voices::ensure_voice`.
    let text_encoder = Box::pin(fetch_asset(&GRAPHS[0], base, &dir)).await?;
    let vector_estimator = Box::pin(fetch_asset(&GRAPHS[1], base, &dir)).await?;
    let vocoder = Box::pin(fetch_asset(&GRAPHS[2], base, &dir)).await?;
    let duration_predictor = Box::pin(fetch_asset(&GRAPHS[3], base, &dir)).await?;
    let config = Box::pin(fetch_asset(&CONFIG, base, &dir)).await?;
    let voice = Box::pin(fetch_asset(&VOICE, base, &dir)).await?;
    let indexer = Box::pin(fetch_asset(&INDEXER, base, &dir)).await?;
    Ok(SupertonicPaths {
        text_encoder,
        vector_estimator,
        vocoder,
        duration_predictor,
        config,
        voice,
        indexer,
    })
}

/// Fetch one asset into `dir`, reusing a cached copy whose SHA-256 already
/// matches. Returns the absolute path to the verified file. Mirrors
/// `crate::voices::fetch_asset` / `wake_registry::fetch_asset`.
async fn fetch_asset(asset: &SupertonicAsset, base_url: &str, dir: &Path) -> Result<PathBuf> {
    let dest = dir.join(asset.file);
    let pinned = !asset.sha256.chars().all(|c| c == '0');
    if dest.is_file() && pinned {
        let actual = fono_download::sha256_file(&dest)
            .await
            .with_context(|| format!("hash cached {}", dest.display()))?;
        if actual.eq_ignore_ascii_case(asset.sha256) {
            tracing::debug!("supertonic asset {} present and verified (cache hit)", asset.file);
            return Ok(dest);
        }
        tracing::warn!("cached {} failed checksum; re-downloading", dest.display());
    }
    let url = asset_url(base_url, asset.file);
    fono_download::download(&url, &dest, asset.sha256)
        .await
        .with_context(|| format!("download supertonic asset {url}"))?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extension-equality helper that avoids clippy's case-sensitive
    /// `ends_with(".ext")` lint in the assertions below.
    fn ext_is(file: &str, ext: &str) -> bool {
        std::path::Path::new(file).extension().is_some_and(|e| e == ext)
    }

    #[test]
    fn pack_has_seven_assets_graphs_first() {
        let all = assets();
        assert_eq!(all.len(), 7, "four graphs + tts.json + voice.bin + unicode_indexer.bin");
        assert_eq!(all[0].file, "text_encoder.ort");
        assert!(all[..4].iter().all(|a| ext_is(a.file, "ort")), "first four are graphs");
    }

    #[test]
    fn asset_filenames_are_unique() {
        let files: std::collections::BTreeSet<&str> = assets().iter().map(|a| a.file).collect();
        assert_eq!(files.len(), 7, "asset basenames must be unique");
    }

    #[test]
    fn graphs_are_ort_not_onnx() {
        // The minimal runtime loads only `.ort`; a stray `.onnx` here would be
        // unloadable at runtime (ADR 0033).
        for g in GRAPHS {
            assert!(ext_is(g.file, "ort"), "graph {} must be .ort", g.file);
            assert!(!ext_is(g.file, "onnx"), "graph {} must not be raw .onnx", g.file);
        }
    }

    #[test]
    fn format_stable_files_are_pinned() {
        // These three do not change across `.ort` conversion, so they carry
        // real pins from the upstream int8 pack.
        for a in [CONFIG, VOICE, INDEXER] {
            assert_eq!(a.sha256.len(), 64, "{} sha must be 64 hex", a.file);
            assert!(a.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(!a.sha256.chars().all(|c| c == '0'), "{} must be pinned", a.file);
        }
    }

    #[test]
    fn graphs_are_unpinned_until_converted_and_hosted() {
        for g in GRAPHS {
            assert_eq!(g.sha256, UNPINNED, "graph {} pinned before conversion?", g.file);
        }
        assert!(!is_hosted(), "pack must report not-hosted while graphs are unpinned");
    }

    #[test]
    fn license_notice_names_openrail_and_links_it() {
        let n = license_notice();
        assert!(n.contains("OpenRAIL-M"), "notice must name the license");
        assert!(n.contains(WEIGHTS_LICENSE_URL), "notice must link the license text");
        assert!(n.contains("NOT bundled"), "notice must state weights are not bundled");
    }

    #[test]
    fn asset_url_joins_without_double_slashes() {
        assert_eq!(
            asset_url("https://example.test/dl/", "text_encoder.ort"),
            "https://example.test/dl/ort-1.24.2/text_encoder.ort"
        );
        assert_eq!(
            asset_url("https://example.test/dl", "voice.bin"),
            "https://example.test/dl/ort-1.24.2/voice.bin"
        );
    }

    #[test]
    fn resolved_paths_live_in_a_supertonic_subdir() {
        let dir = Path::new("/cache/models/voices");
        let p = resolved_paths(dir);
        assert_eq!(p.text_encoder, dir.join("supertonic").join("text_encoder.ort"));
        assert_eq!(p.config, dir.join("supertonic").join("tts.json"));
        assert_eq!(p.indexer, dir.join("supertonic").join("unicode_indexer.bin"));
    }

    /// A cached file whose hash matches skips the network entirely (the URL
    /// points at an unroutable host, so a download attempt would fail).
    #[tokio::test]
    async fn cached_asset_with_matching_hash_skips_download() {
        let dir = tempfile::tempdir().unwrap();
        let body = b"supertonic probe";
        let sha = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(body);
            hex::encode(h.finalize())
        };
        let leaked: &'static str = Box::leak(sha.into_boxed_str());
        let asset = SupertonicAsset { file: "probe.bin", sha256: leaked };
        std::fs::write(dir.path().join("probe.bin"), body).unwrap();

        let got = fetch_asset(&asset, "http://127.0.0.1:1/never", dir.path())
            .await
            .expect("cache hit must succeed without network");
        assert_eq!(got, dir.path().join("probe.bin"));
    }
}
