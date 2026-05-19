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
    Ok(())
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
