// SPDX-License-Identifier: GPL-3.0-only
//! Ensure the local-model files referenced by `config.toml` are present
//! on disk, downloading them on demand.
//!
//! Called from:
//! * the daemon startup path (before the IPC loop begins),
//! * the wizard after a fresh `Setup`,
//! * the tray's STT/LLM switcher (when the user picks `Local` and the
//!   weights are missing — see [`ensure_local_stt`] / [`ensure_local_polish`]).
//!
//! Both whisper STT and llama-cpp LLM are covered; the LLM auto-download
//! resolves the model name in `config.polish.local.model` against the
//! `fono-polish` registry and writes to `<polish_models_dir>/<name>.gguf`,
//! mirroring the path resolver in `fono-polish::factory`.
//!
//! Whisper STT additionally honours `[stt.local].quantization` — the
//! user-facing model name (e.g. `small`) and the quantization
//! preference (e.g. `auto`, `q8_0`, `fp16`) together resolve to a
//! single GGML file (e.g. `ggml-small-q5_1.bin`) via
//! [`fono_stt::ModelRegistry`].

use std::path::PathBuf;

use anyhow::{Context, Result};
use fono_core::config::{Config, PolishBackend, SttBackend};
use fono_core::Paths;
use fono_stt::{ModelInfo, ModelRegistry, Quantization, QuantizationPref};
use tracing::{debug, info, warn};

/// Outcome of an `ensure_*` call, for callers that want to surface a
/// notification only on the first download (and stay quiet on subsequent
/// daemon starts when the model is already cached).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureOutcome {
    /// The model file was already on disk; nothing was downloaded.
    AlreadyPresent,
    /// The model file was missing and a download succeeded.
    Downloaded,
    /// The configured model name is not in the registry — nothing we
    /// can auto-download. The caller should leave the existing file (if
    /// any) alone.
    Unknown,
}

/// Resolve a `(name, quantization_pref)` pair from config into the
/// registry entry + concrete quantization + on-disk filename. Returns
/// `Ok(None)` when the model name is unknown — callers translate that
/// into a warning and `EnsureOutcome::Unknown`. Returns `Err` when the
/// model exists but the user pinned a quantization the registry does
/// not ship (e.g. `model = "tiny"` with `quantization = "fp16"` — `tiny`
/// ships only `q5_1`).
pub fn resolve_local_stt(
    name: &str,
    quantization: &str,
) -> Result<Option<(&'static ModelInfo, Quantization)>> {
    let Some(info) = ModelRegistry::get(name) else {
        return Ok(None);
    };
    let pref = QuantizationPref::parse(quantization).ok_or_else(|| {
        anyhow::anyhow!(
            "invalid `[stt.local].quantization = {quantization:?}` — \
             expected `auto`, `fp16`, `q5_1`, or `q8_0`"
        )
    })?;
    let q = ModelRegistry::resolve_quantization(info, pref).map_err(anyhow::Error::msg)?;
    Ok(Some((info, q)))
}

/// Path the whisper GGML file should live at, given the resolved
/// `(name, quantization)`. Pure naming function — does not touch disk.
#[must_use]
pub fn whisper_dest(paths: &Paths, name: &str, quant: Quantization) -> PathBuf {
    paths.whisper_models_dir().join(ModelRegistry::filename(name, quant))
}

/// Check every model the current config references and download any that
/// are missing. Individual failures log a warning but do not abort the
/// daemon; this is invoked unconditionally from startup and we never
/// want a transient HTTP failure to keep the daemon from coming up.
pub async fn ensure_models(paths: &Paths, config: &Config) -> Result<()> {
    if config.stt.backend == SttBackend::Local {
        let r =
            ensure_local_stt(paths, &config.stt.local.model, &config.stt.local.quantization).await;
        if let Err(e) = r {
            warn!("auto-download of whisper model failed: {e:#}");
        }
    }
    if config.polish.backend == PolishBackend::Local {
        if let Err(e) = ensure_local_polish(paths, &config.polish.local.model).await {
            warn!("auto-download of LLM model failed: {e:#}");
        }
    }
    #[cfg(feature = "tts-local")]
    if config.tts.backend == fono_core::config::TtsBackend::Local {
        // Boxed: the voice-ensure future (catalog Voice + download buffers)
        // is large enough to trip `clippy::large_futures` if inlined here.
        if let Err(e) = Box::pin(ensure_local_tts(paths, config)).await {
            warn!("auto-download of local TTS voice failed: {e:#}");
        }
    }
    Ok(())
}

/// Ensure the local TTS voices (`.ort` model + `.onnx.json` config) are
/// cached under `voices_dir`, downloading and verifying them from the
/// `fono-voice` mirror when missing. Resolution mirrors
/// `fono_tts::factory`: an explicit `[tts.local].voice` pins a single
/// voice; otherwise one voice per configured language is ensured so the
/// language router can switch voices offline (a Romanian reply gets the
/// Romanian voice, an English reply the English one). Languages without a
/// catalog voice are skipped with a warning rather than failing the lot.
#[cfg(feature = "tts-local")]
pub async fn ensure_local_tts(paths: &Paths, config: &Config) -> Result<EnsureOutcome> {
    let voices = resolve_local_tts_voices(config)?;
    let voices_dir = paths.voices_dir();
    let base_url = &config.tts.local.base_url;
    let base = (!base_url.is_empty()).then_some(base_url.as_str());
    let mut any_downloaded = false;
    for voice in &voices {
        let already = voices_dir.join(&voice.model.file).is_file()
            && voice.config.as_ref().is_none_or(|c| voices_dir.join(&c.file).is_file())
            && voice.style.as_ref().is_none_or(|s| voices_dir.join(&s.file).is_file());
        if !already {
            any_downloaded = true;
            info!("local voice {:?} missing — downloading from the fono-voice mirror", voice.name);
        }
        fono_tts::voices::ensure_voice(voice, &voices_dir, base)
            .await
            .with_context(|| format!("ensuring local voice {:?}", voice.name))?;
        if already {
            debug!("local voice ready: {}", voice.name);
        } else {
            info!("local voice installed: {}", voice.name);
        }
    }
    Ok(if any_downloaded { EnsureOutcome::Downloaded } else { EnsureOutcome::AlreadyPresent })
}

/// Resolve which catalog voices the local backend needs, mirroring
/// `fono_tts::factory`: an explicit `[tts.local].voice` pins one voice,
/// otherwise one voice per configured language (deduped) is chosen.
/// Languages without a catalog voice are skipped with a warning.
#[cfg(feature = "tts-local")]
fn resolve_local_tts_voices(config: &Config) -> Result<Vec<fono_tts::voices::Voice>> {
    let local = &config.tts.local;
    if !local.voice.is_empty() {
        return Ok(vec![fono_tts::voices::by_name(&local.voice)?.ok_or_else(|| {
            anyhow::anyhow!("[tts.local].voice = {:?} is not in the voice catalog", local.voice)
        })?]);
    }
    let mut langs: Vec<&str> = config.general.languages.iter().map(String::as_str).collect();
    if langs.is_empty() {
        langs.push("en");
    }
    let mut chosen: Vec<fono_tts::voices::Voice> = Vec::new();
    for lang in langs {
        match fono_tts::voices::for_language(lang)? {
            Some(v) if !chosen.iter().any(|c| c.name == v.name) => chosen.push(v),
            Some(_) => {} // a different language already mapped to this voice
            None => warn!(
                "no local TTS voice in the catalog for configured language {lang:?}; \
                 it will fall back to the primary voice"
            ),
        }
    }
    if chosen.is_empty() {
        let lang = config.general.languages.first().map_or("en", String::as_str);
        return Err(anyhow::anyhow!(
            "no local voice in the catalog for any configured language (e.g. {lang:?}); \
             set [tts.local].voice to a catalog voice id"
        ));
    }
    Ok(chosen)
}

/// Approximate total download size (MB) for the local TTS voices the
/// current config requires that are **not yet on disk**, or `None` when
/// every required voice is already cached (so callers can skip the
/// "downloading…" notification). Used by the tray switcher.
#[cfg(feature = "tts-local")]
#[must_use]
pub fn local_tts_pending_mb(paths: &Paths, config: &Config) -> Option<u32> {
    let voices = resolve_local_tts_voices(config).ok()?;
    let voices_dir = paths.voices_dir();
    let pending: u64 = voices
        .iter()
        .filter(|v| {
            let present = voices_dir.join(&v.model.file).is_file()
                && v.config.as_ref().is_none_or(|c| voices_dir.join(&c.file).is_file())
                && v.style.as_ref().is_none_or(|s| voices_dir.join(&s.file).is_file());
            !present
        })
        .map(|v| {
            v.model.size
                + v.config.as_ref().map_or(0, |c| c.size)
                + v.style.as_ref().map_or(0, |s| s.size)
        })
        .sum();
    if pending == 0 {
        None
    } else {
        Some((pending / 1_000_000).max(1) as u32)
    }
}

/// Ensure the named whisper model (at the configured quantization) is
/// on disk. Returns the outcome so the tray switcher can show
/// "downloading…" / "ready" notifications only when work was actually
/// done.
pub async fn ensure_local_stt(
    paths: &Paths,
    model_name: &str,
    quantization: &str,
) -> Result<EnsureOutcome> {
    let resolved = resolve_local_stt(model_name, quantization)?;
    let Some((info, quant)) = resolved else {
        warn!(
            "config references unknown whisper model {model_name:?} — run \
             `fono models list` to see available names"
        );
        return Ok(EnsureOutcome::Unknown);
    };
    let variant = ModelRegistry::variant_for(info, quant)
        .expect("resolve_quantization guarantees variant exists");
    let dest = whisper_dest(paths, info.name, quant);
    if dest.exists() {
        debug!("whisper model ready: {}", dest.display());
        return Ok(EnsureOutcome::AlreadyPresent);
    }
    let url =
        ModelRegistry::url_for(info, quant).expect("variant lookup succeeded so URL must resolve");
    info!(
        "whisper model {model_name:?} ({quant}) missing — downloading {} MB from {url}",
        variant.approx_mb
    );
    fono_download::download(&url, &dest, variant.sha256)
        .await
        .with_context(|| format!("downloading whisper model {model_name:?} ({quant})"))?;
    info!("whisper model installed: {}", dest.display());
    Ok(EnsureOutcome::Downloaded)
}

/// Ensure the named local LLM (`.gguf`) is on disk. Path resolution
/// matches `fono-polish::factory::resolve_local_model_path`:
/// `<polish_models_dir>/<name>.gguf`.
pub async fn ensure_local_polish(paths: &Paths, model_name: &str) -> Result<EnsureOutcome> {
    let Some(info) = fono_polish::PolishRegistry::get(model_name) else {
        warn!(
            "config references unknown LLM model {model_name:?} — run \
             `fono models list` to see available names"
        );
        return Ok(EnsureOutcome::Unknown);
    };
    let dest = paths.polish_models_dir().join(format!("{}.gguf", info.name));
    if dest.exists() {
        debug!("LLM model ready: {}", dest.display());
        return Ok(EnsureOutcome::AlreadyPresent);
    }
    let url = fono_polish::PolishRegistry::url_for(info);
    info!("LLM model {model_name:?} missing — downloading {} MB from {url}", info.approx_mb);
    fono_download::download(&url, &dest, info.sha256)
        .await
        .with_context(|| format!("downloading LLM model {model_name:?}"))?;
    info!("LLM model installed: {}", dest.display());
    Ok(EnsureOutcome::Downloaded)
}

/// Approximate download size (MB) for the given local STT model name +
/// quantization preference, or `None` when the registry doesn't know
/// the combination. Used by the tray switcher to put a useful number
/// in the "downloading…" notification.
#[must_use]
pub fn local_stt_size_mb(model_name: &str, quantization: &str) -> Option<u32> {
    let (info, quant) = resolve_local_stt(model_name, quantization).ok().flatten()?;
    ModelRegistry::variant_for(info, quant).map(|v| v.approx_mb)
}

/// Approximate download size (MB) for the given local LLM model name,
/// or `None` when the registry doesn't know about it.
#[must_use]
pub fn local_llm_size_mb(model_name: &str) -> Option<u32> {
    fono_polish::PolishRegistry::get(model_name).map(|m| m.approx_mb)
}
