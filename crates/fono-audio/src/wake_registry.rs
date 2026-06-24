// SPDX-License-Identifier: GPL-3.0-only
//! Wake-word model registry + on-demand fetch (Phase G of
//! `plans/2026-06-23-wake-word-openwakeword-v2.md`).
//!
//! Distribution mirrors the local voice stack exactly (ADR 0033,
//! `crates/fono-tts/src/voices.rs`): every asset is a SHA-256-pinned `.ort`
//! file hosted on the **`fono-voice`** release mirror under a tag named for
//! the ONNX Runtime ABI it was converted for (`ort-<version>`, e.g.
//! `ort-1.24.2`), and fetched + verified through [`fono_download::download`],
//! reusing a cached copy whose hash already matches. A single static table
//! ([`WAKE_MODELS`]) is the source of truth for the picker, the fetcher, and
//! `fono doctor` — the same shape as `fono_stt::registry`.
//!
//! **Why `.ort` (not the upstream `.onnx`):** the shipped binary links the
//! *minimal* onnxruntime (ADR 0032), which loads only `.ort` flatbuffer
//! models. The upstream openWakeWord artifacts are full `.onnx` graphs and
//! cannot load on that runtime; Fono converts them to `.ort` (the same
//! `scripts/gen-ort-models.sh` pipeline as the voices) and hosts the
//! conversions on its own mirror. The on-disk classifier basename is always
//! `<id>.ort`, matching what `fono::wake::try_load_onnx` looks up.
//!
//! Two classes of model live here:
//!
//! - **Default / clean-license** ([`WakeModelClass::Default`]): the
//!   freshly-trained Apache-2.0 "hey fono" classifier plus the shared
//!   Apache melspectrogram graph and Google `speech_embedding` backbone.
//!   These carry **no usage restriction**. The artifacts are produced in
//!   Phase B and uploaded to the mirror; until then their SHA-256 pins are
//!   the [`UNPINNED`] sentinel.
//! - **Community / NonCommercial** ([`WakeModelClass::Community`]): the
//!   upstream openWakeWord phrases ("hey jarvis", "alexa", "hey mycroft")
//!   published under **CC-BY-NC-SA-4.0**. Per ADR 0004 / the AGENTS
//!   model-licensing rule these are **never a default and never bundled in
//!   the release binary**, and at fetch time the user is **notified** that
//!   the model is NonCommercial (see [`license_notice`]); the notice informs,
//!   it does not gate. Note that publishing the `.ort` *conversions* of these
//!   NC models on the Fono mirror is itself a redistribution decision (the
//!   CC-BY-NC-SA terms permit it with attribution + ShareAlike, but it is a
//!   deliberate policy call): until the conversions are uploaded and pinned
//!   their entries stay [`UNPINNED`] and unfetchable, and a user may instead
//!   train an equivalent `.ort` locally via `scripts/train-wakeword-model.sh`.
//!   As of `ort-1.24.2` the three upstream conversions **are** hosted (the
//!   repo owner's NonCommercial-redistribution call, recorded with attribution
//!   in the `fono-voice` README) and pinned below; `hey_fono` remains
//!   [`UNPINNED`] until the Phase B clean artifact is trained and uploaded.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// SHA-256 sentinel for assets without a pinned digest yet (same convention
/// and value as `fono_stt::registry::UNPINNED` and the downloader's
/// all-zeros "unpinned" check). The downloader logs the computed hash and
/// accepts the file; tighten to a real pin once the artifact is hosted.
pub const UNPINNED: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// ONNX Runtime version the wake `.ort` assets are converted for. Must match
/// the linked runtime ABI (ADR 0032) — the same value the voice stack and
/// `scripts/fetch-onnxruntime.sh` pin.
pub const ORT_VERSION: &str = "1.24.2";

/// Release tag the wake assets live under on the mirror, named for the ONNX
/// Runtime ABI (ADR 0033), e.g. `ort-1.24.2`.
pub const RELEASE_TAG: &str = "ort-1.24.2";

/// Default mirror base URL: the `fono-voice` repo's release-download root —
/// the **same** mirror the local voices use (ADR 0033). The per-asset
/// [`RELEASE_TAG`] and `file` are appended to form the full URL (see
/// [`asset_url`]). Override via the `base_url` argument to [`fetch_model`] for
/// forks, self-hosting, or a CDN.
pub const DEFAULT_BASE_URL: &str = "https://github.com/bogdanr/fono-voice/releases/download";

/// License an asset is published under. Only the variants Fono actually
/// distributes pins for are modelled; [`WakeLicense::is_noncommercial`] is
/// the single gate that decides whether the consent flow applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeLicense {
    /// Apache-2.0 — OSI-approved, GPL-3.0-compatible, no use restriction.
    Apache2_0,
    /// CC-BY-NC-SA-4.0 — Attribution + **NonCommercial** + ShareAlike. The
    /// NonCommercial clause is what forces the opt-in notice.
    CcByNcSa4_0,
}

impl WakeLicense {
    /// SPDX-ish identifier for display / `fono doctor` badges.
    #[must_use]
    pub fn spdx(self) -> &'static str {
        match self {
            Self::Apache2_0 => "Apache-2.0",
            Self::CcByNcSa4_0 => "CC-BY-NC-SA-4.0",
        }
    }

    /// `true` for licenses carrying a NonCommercial restriction. These models
    /// can never be a default or be bundled, and surface a notice before
    /// download.
    #[must_use]
    pub fn is_noncommercial(self) -> bool {
        matches!(self, Self::CcByNcSa4_0)
    }
}

/// Distribution class of a registry entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeModelClass {
    /// Clean-license, shippable as a default. Apache-2.0, no notice.
    Default,
    /// Opt-in community model. NonCommercial; notified at download.
    Community,
}

/// One downloadable file: its on-disk basename, the mirror release tag it
/// lives under, and its pinned hash. The full URL is built at fetch time via
/// [`asset_url`] from a base URL + the tag + the file (mirrors
/// `fono_tts::voices::Asset`), so forks/self-hosting only swap the base.
#[derive(Debug, Clone, Copy)]
pub struct WakeAsset {
    /// Local cache basename, also the mirror asset name (e.g.
    /// `melspectrogram.ort`, `hey_jarvis.ort`).
    pub file: &'static str,
    /// Release tag the asset lives under in the mirror (e.g. `ort-1.24.2`).
    pub release_tag: &'static str,
    /// Lowercase-hex SHA-256, or [`UNPINNED`] for not-yet-hosted artifacts.
    pub sha256: &'static str,
}

/// One wake-word phrase model: a per-phrase classifier riding the shared
/// melspectrogram + embedding graphs ([`MELSPEC`] / [`EMBEDDING`]).
#[derive(Debug, Clone, Copy)]
pub struct WakeModelEntry {
    /// Stable id used in `[wakeword].phrases[].model` and as the cache key.
    /// Also the classifier basename stem: the cached file is `<id>.ort`.
    pub id: &'static str,
    /// Human-readable label for pickers / tray copy.
    pub label: &'static str,
    /// Distribution class (default vs community).
    pub class: WakeModelClass,
    /// License the classifier is published under.
    pub license: WakeLicense,
    /// The per-phrase classifier file.
    pub classifier: WakeAsset,
}

impl WakeModelEntry {
    /// `true` if this entry carries a NonCommercial restriction and therefore
    /// surfaces a license notice before it is fetched.
    #[must_use]
    pub fn is_noncommercial(&self) -> bool {
        self.license.is_noncommercial()
    }

    /// `true` for the clean-license default model (no notice).
    #[must_use]
    pub fn is_default(&self) -> bool {
        matches!(self.class, WakeModelClass::Default)
    }
}

/// Shared Apache-2.0 melspectrogram graph (`.ort` conversion of the
/// openWakeWord v0.5.1 `melspectrogram.onnx`). Reused by every classifier, so
/// extra phrases are nearly free. Hosted on the `fono-voice` mirror.
pub const MELSPEC: WakeAsset = WakeAsset {
    file: "melspectrogram.ort",
    release_tag: RELEASE_TAG,
    sha256: "80827b4a8f15c67b89c55114ad674bfcc5ab1e7a843330a2325bac274146e104",
};

/// Shared frozen Google `speech_embedding` backbone (Apache-2.0; `.ort`
/// conversion of the openWakeWord v0.5.1 `embedding_model.onnx`). Hosted on
/// the `fono-voice` mirror.
pub const EMBEDDING: WakeAsset = WakeAsset {
    file: "embedding.ort",
    release_tag: RELEASE_TAG,
    sha256: "4214565a4a21a20ac066cf91e97300fe567009a2ee973c27714b47dea8a95612",
};

/// The wake-model table — single source of truth for the picker, the fetcher,
/// and `fono doctor`. The default clean model comes first.
pub const WAKE_MODELS: &[WakeModelEntry] = &[
    // ── Default clean-license "hey fono" (Apache-2.0) ───────────────────
    // The freshly-trained classifier from Phase B (Apache melspec + Apache
    // Google embedding + clean-licensed positives/negatives). No usage
    // restriction; no notice. Artifact + pin land in Phase B.
    WakeModelEntry {
        id: "hey_fono",
        label: "Hey Fono",
        class: WakeModelClass::Default,
        license: WakeLicense::Apache2_0,
        // TODO(phase B): pin once the trained `hey_fono.ort` is uploaded.
        classifier: WakeAsset { file: "hey_fono.ort", release_tag: RELEASE_TAG, sha256: UNPINNED },
    },
    // ── Opt-in community phrases (CC-BY-NC-SA-4.0, NonCommercial) ────────
    // `.ort` conversions of the upstream openWakeWord v0.5.1 phrases. NEVER a
    // default, NEVER bundled in the binary. Publishing these conversions on
    // the mirror is a redistribution decision (CC-BY-NC-SA permits it with
    // attribution + ShareAlike; recorded in the fono-voice README). Hosted +
    // pinned as of ort-1.24.2.
    WakeModelEntry {
        id: "hey_jarvis",
        label: "Hey Jarvis (community)",
        class: WakeModelClass::Community,
        license: WakeLicense::CcByNcSa4_0,
        classifier: WakeAsset {
            file: "hey_jarvis.ort",
            release_tag: RELEASE_TAG,
            sha256: "2023946e4a31e526974318844f7320798de8a5cb81e2777f6caa62dde25f584e",
        },
    },
    WakeModelEntry {
        id: "alexa",
        label: "Alexa (community)",
        class: WakeModelClass::Community,
        license: WakeLicense::CcByNcSa4_0,
        classifier: WakeAsset {
            file: "alexa.ort",
            release_tag: RELEASE_TAG,
            sha256: "1c15a4076166dbd624425b6e93e03816a95dff5f73fb129b03e4d2b23230d65a",
        },
    },
    WakeModelEntry {
        id: "hey_mycroft",
        label: "Hey Mycroft (community)",
        class: WakeModelClass::Community,
        license: WakeLicense::CcByNcSa4_0,
        classifier: WakeAsset {
            file: "hey_mycroft.ort",
            release_tag: RELEASE_TAG,
            sha256: "d7f71e68387e74ae715cae1c38521fe1838551a16f8028f94a66e18daad2a81f",
        },
    },
];

/// Runtime fallback phrase used when wake detection is needed but
/// `[wakeword].phrases` is empty — notably the auto-served Wyoming wake
/// service (which mirrors how STT/TTS are served whenever the binary can do
/// them, regardless of local use).
///
/// **TEMPORARY policy exception.** The clean-license default is `hey_fono`
/// ([`WakeModelClass::Default`], Apache-2.0), but its `.ort` artifact is not
/// trained/hosted yet ([`UNPINNED`], unfetchable). Until Phase B ships it,
/// the only model that actually loads out of the box is the community
/// `hey_jarvis` conversion, so the runtime fallback points there. Note that
/// `hey_jarvis` is **CC-BY-NC-SA-4.0 (NonCommercial)** — the fetch path
/// surfaces [`license_notice`] when it is downloaded. This deliberately
/// diverges from the "default must be clean-license" rule **only** as a
/// stopgap; flip it back to `"hey_fono"` the moment that artifact is pinned.
pub const DEFAULT_WAKE_MODEL: &str = "hey_jarvis";

/// Look up an entry by id (exact match). Returns `None` for unknown ids;
/// callers should point users at the model list.
#[must_use]
pub fn get(id: &str) -> Option<&'static WakeModelEntry> {
    WAKE_MODELS.iter().find(|m| m.id == id)
}

/// Every registered wake model, default first.
#[must_use]
pub fn all() -> &'static [WakeModelEntry] {
    WAKE_MODELS
}

/// Build the full download URL for an asset: `{base}/{release_tag}/{file}`.
/// Identical join to `fono_tts::voices::asset_url`.
#[must_use]
pub fn asset_url(base_url: &str, release_tag: &str, file: &str) -> String {
    format!("{}/{}/{}", base_url.trim_end_matches('/'), release_tag, file)
}

/// The cache directory wake-word assets live in, mirroring the
/// `models/<kind>` convention used by the STT and voice model dirs
/// (`Paths::whisper_models_dir`, `Paths::voices_dir`).
#[must_use]
pub fn wakeword_dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join("models").join("wakeword")
}

/// Resolved on-disk paths for a fetched wake model: the shared graphs plus
/// the per-phrase classifier. Returned by [`fetch_model`] and computable
/// without touching the network via [`resolved_paths`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWakeModel {
    pub melspec: PathBuf,
    pub embedding: PathBuf,
    pub classifier: PathBuf,
}

/// Compute where a model's files would live on disk, without downloading.
/// Returns `None` for an unknown id. The classifier basename is `<id>.ort`,
/// matching `fono::wake::try_load_onnx`.
#[must_use]
pub fn resolved_paths(id: &str, cache_dir: &Path) -> Option<ResolvedWakeModel> {
    let entry = get(id)?;
    let dir = wakeword_dir(cache_dir);
    Some(ResolvedWakeModel {
        melspec: dir.join(MELSPEC.file),
        embedding: dir.join(EMBEDDING.file),
        classifier: dir.join(entry.classifier.file),
    })
}

/// Human-readable license notice for an entry, suitable for a CLI prompt,
/// the wizard, or tray copy. For NonCommercial entries this is the text the
/// user is shown before download; for clean entries it is a plain
/// informational note. Returns `None` for an unknown id.
#[must_use]
pub fn license_notice(id: &str) -> Option<String> {
    let entry = get(id)?;
    Some(if entry.is_noncommercial() {
        format!(
            "The \"{label}\" wake model is an upstream openWakeWord model \
             licensed under {spdx} (Creative Commons Attribution-NonCommercial-\
             ShareAlike 4.0). It is NOT part of Fono and is NOT bundled. By \
             downloading it you acknowledge that the NonCommercial terms bind \
             your use: you may use it for non-commercial purposes only, must \
             give attribution, and must share any adaptations under the same \
             license. Full text: https://creativecommons.org/licenses/by-nc-sa/4.0/",
            label = entry.label,
            spdx = entry.license.spdx(),
        )
    } else {
        format!(
            "The \"{label}\" wake model is licensed under {spdx}; no usage \
             restriction applies.",
            label = entry.label,
            spdx = entry.license.spdx(),
        )
    })
}

/// Fetch and verify a wake model by id: the shared melspectrogram + embedding
/// graphs plus its per-phrase classifier, downloaded into the wake-word cache
/// dir via [`fono_download::download`] and checked against the pinned
/// SHA-256. A cached file whose hash already matches is reused (no network).
///
/// `base_url` overrides [`DEFAULT_BASE_URL`] (forks / self-hosting / CDN);
/// pass `None` for the default mirror.
///
/// **NonCommercial notice:** for a community model this logs the
/// [`license_notice`] (so the daemon log records that the fetched artifact is
/// NonCommercial) and then proceeds — the notice informs, it does not block,
/// and nothing is persisted. The user-facing surface (CLI / tray / wizard)
/// shows the same notice via [`license_notice`] when the phrase is chosen.
/// The clean/default model fetches silently.
pub async fn fetch_model(
    id: &str,
    cache_dir: &Path,
    base_url: Option<&str>,
) -> Result<ResolvedWakeModel> {
    let entry = get(id).with_context(|| format!("unknown wake model id '{id}'"))?;
    if entry.is_noncommercial() {
        if let Some(notice) = license_notice(id) {
            tracing::warn!(model = id, "{notice}");
        }
    }

    let base = base_url.unwrap_or(DEFAULT_BASE_URL);
    let dir = wakeword_dir(cache_dir);
    let melspec = Box::pin(fetch_asset(&MELSPEC, base, &dir)).await?;
    let embedding = Box::pin(fetch_asset(&EMBEDDING, base, &dir)).await?;
    let classifier = Box::pin(fetch_asset(&entry.classifier, base, &dir)).await?;
    Ok(ResolvedWakeModel { melspec, embedding, classifier })
}

/// Fetch one asset into `dir`, reusing a cached copy whose SHA-256 already
/// matches. Returns the absolute path to the verified file. Mirrors
/// `fono_tts::voices::fetch_asset`.
async fn fetch_asset(asset: &WakeAsset, base_url: &str, dir: &Path) -> Result<PathBuf> {
    let dest = dir.join(asset.file);
    let pinned = !asset.sha256.chars().all(|c| c == '0');
    if dest.is_file() && pinned {
        let actual = fono_download::sha256_file(&dest)
            .await
            .with_context(|| format!("hash cached {}", dest.display()))?;
        if actual.eq_ignore_ascii_case(asset.sha256) {
            tracing::debug!("wake asset {} present and verified (cache hit)", asset.file);
            return Ok(dest);
        }
        tracing::warn!("cached {} failed checksum; re-downloading", dest.display());
    }
    let url = asset_url(base_url, asset.release_tag, asset.file);
    fono_download::download(&url, &dest, asset.sha256)
        .await
        .with_context(|| format!("download wake asset {url}"))?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_enumerates_default_first_and_unique_ids() {
        let all = all();
        assert!(all.len() >= 4, "expect the default + at least three community phrases");
        assert!(all[0].is_default(), "the clean default model must come first");
        assert_eq!(all[0].id, "hey_fono");
        let mut ids: Vec<&str> = all.iter().map(|m| m.id).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "model ids must be unique");
    }

    #[test]
    fn exactly_one_default_and_it_is_clean() {
        let defaults: Vec<_> = all().iter().filter(|m| m.is_default()).collect();
        assert_eq!(defaults.len(), 1, "there must be exactly one default model");
        let d = defaults[0];
        assert!(!d.is_noncommercial(), "the default must be clean-license");
        assert_eq!(d.license, WakeLicense::Apache2_0);
    }

    #[test]
    fn community_entries_are_flagged_noncommercial() {
        for m in all().iter().filter(|m| matches!(m.class, WakeModelClass::Community)) {
            assert!(m.is_noncommercial(), "community model '{}' must be NonCommercial", m.id);
            assert_eq!(m.license, WakeLicense::CcByNcSa4_0);
        }
        // The three documented upstream phrases are present.
        for id in ["hey_jarvis", "alexa", "hey_mycroft"] {
            assert!(get(id).unwrap().is_noncommercial(), "{id} must be NC");
        }
    }

    #[test]
    fn every_asset_is_an_ort_on_the_fono_voice_mirror_with_abi_tag() {
        // Distribution must match the voice stack (ADR 0033): `.ort` files,
        // ABI-named release tag, hosted on fono-voice (or a fork via base_url).
        assert!(DEFAULT_BASE_URL.contains("fono-voice"), "wake assets ride the fono-voice mirror");
        assert_eq!(RELEASE_TAG, format!("ort-{ORT_VERSION}"));
        let check = |a: &WakeAsset| {
            assert!(
                Path::new(a.file).extension().is_some_and(|e| e.eq_ignore_ascii_case("ort")),
                "{} must be a .ort",
                a.file
            );
            assert_eq!(a.release_tag, RELEASE_TAG, "{} must use the ABI tag", a.file);
        };
        check(&MELSPEC);
        check(&EMBEDDING);
        for m in all() {
            check(&m.classifier);
        }
    }

    #[test]
    fn classifier_basename_is_id_dot_ort() {
        // The loader (`fono::wake::try_load_onnx`) looks up `<id>.ort`; the
        // cached classifier basename MUST equal that or detection silently
        // falls back to the stub.
        for m in all() {
            assert_eq!(
                m.classifier.file,
                format!("{}.ort", m.id),
                "classifier basename must be <id>.ort for the loader contract",
            );
        }
    }

    #[test]
    fn shared_graphs_and_community_classifiers_are_pinned_hey_fono_pending() {
        // The shared graphs and the three hosted community conversions carry
        // real SHA-256 pins (ort-1.24.2 release); only the Phase-B clean
        // `hey_fono` artifact remains the all-zeros sentinel. When hey_fono is
        // uploaded and pinned, update this test.
        let is_pinned = |s: &str| s.len() == 64 && !s.chars().all(|c| c == '0');
        assert!(is_pinned(MELSPEC.sha256), "melspectrogram must be pinned");
        assert!(is_pinned(EMBEDDING.sha256), "embedding must be pinned");
        for m in all().iter().filter(|m| matches!(m.class, WakeModelClass::Community)) {
            assert!(is_pinned(m.classifier.sha256), "community '{}' must be pinned", m.id);
        }
        let hey_fono = get("hey_fono").unwrap();
        assert!(
            hey_fono.classifier.sha256.chars().all(|c| c == '0'),
            "hey_fono is expected to stay UNPINNED until its Phase B artifact ships",
        );
    }

    #[test]
    fn license_notice_distinguishes_clean_from_noncommercial() {
        let clean = license_notice("hey_fono").unwrap();
        assert!(clean.contains("Apache-2.0"));
        assert!(clean.contains("no usage restriction"));
        let nc = license_notice("hey_jarvis").unwrap();
        assert!(nc.contains("NonCommercial"));
        assert!(nc.contains("non-commercial purposes only"));
        assert!(license_notice("does_not_exist").is_none());
    }

    #[test]
    fn resolved_paths_land_under_the_wakeword_cache_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let r = resolved_paths("hey_jarvis", tmp.path()).unwrap();
        let dir = wakeword_dir(tmp.path());
        assert_eq!(r.melspec, dir.join("melspectrogram.ort"));
        assert_eq!(r.embedding, dir.join("embedding.ort"));
        assert_eq!(r.classifier, dir.join("hey_jarvis.ort"));
        assert!(resolved_paths("nope", tmp.path()).is_none());
    }

    #[test]
    fn asset_url_joins_without_double_slashes() {
        assert_eq!(
            asset_url("https://example.test/dl/", "ort-1.24.2", "hey_fono.ort"),
            "https://example.test/dl/ort-1.24.2/hey_fono.ort"
        );
        assert_eq!(
            asset_url("https://example.test/dl", "ort-1.24.2", "hey_fono.ort"),
            "https://example.test/dl/ort-1.24.2/hey_fono.ort"
        );
    }

    #[test]
    fn default_wake_model_is_registered_and_pinned() {
        // The runtime fallback MUST resolve to a real, fetchable (pinned)
        // registry entry — otherwise the auto-served Wyoming wake path would
        // advertise a model whose `.ort` can never be obtained, silently
        // falling back to the never-firing stub.
        let entry = get(DEFAULT_WAKE_MODEL).expect("DEFAULT_WAKE_MODEL must be a registered id");
        let pinned = entry.classifier.sha256.len() == 64
            && !entry.classifier.sha256.chars().all(|c| c == '0');
        assert!(pinned, "DEFAULT_WAKE_MODEL '{DEFAULT_WAKE_MODEL}' must be SHA-pinned/fetchable");
    }

    #[tokio::test]
    async fn fetch_unknown_id_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let err = fetch_model("ghost", tmp.path(), None).await.unwrap_err().to_string();
        assert!(err.contains("unknown wake model"), "{err}");
    }
}
