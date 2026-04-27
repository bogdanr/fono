// SPDX-License-Identifier: GPL-3.0-only
//! Ensure the local-model files referenced by `config.toml` are present
//! on disk, downloading them on demand.
//!
//! Called from:
//! * the daemon startup path (before the IPC loop begins),
//! * the wizard after a fresh `Setup`,
//! * the tray's STT/LLM switcher (when the user picks `Local` and the
//!   weights are missing — see [`ensure_local_stt`] / [`ensure_local_llm`]).
//!
//! Both whisper STT and llama-cpp LLM are covered; the LLM auto-download
//! resolves the model name in `config.llm.local.model` against the
//! `fono-llm` registry and writes to `<llm_models_dir>/<name>.gguf`,
//! mirroring the path resolver in `fono-llm::factory`.

use anyhow::{Context, Result};
use fono_core::config::{Config, LlmBackend, SttBackend};
use fono_core::Paths;
use fono_stt::ModelRegistry;
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

/// Check every model the current config references and download any that
/// are missing. Individual failures log a warning but do not abort the
/// daemon; this is invoked unconditionally from startup and we never
/// want a transient HTTP failure to keep the daemon from coming up.
pub async fn ensure_models(paths: &Paths, config: &Config) -> Result<()> {
    if config.stt.backend == SttBackend::Local {
        if let Err(e) = ensure_local_stt(paths, &config.stt.local.model).await {
            warn!("auto-download of whisper model failed: {e:#}");
        }
    }
    if config.llm.backend == LlmBackend::Local {
        if let Err(e) = ensure_local_llm(paths, &config.llm.local.model).await {
            warn!("auto-download of LLM model failed: {e:#}");
        }
    }
    Ok(())
}

/// Ensure the named whisper model is on disk. Returns the outcome so
/// the tray switcher can show "downloading…" / "ready" notifications
/// only when work was actually done.
pub async fn ensure_local_stt(paths: &Paths, model_name: &str) -> Result<EnsureOutcome> {
    let Some(info) = ModelRegistry::get(model_name) else {
        warn!(
            "config references unknown whisper model {model_name:?} — run \
             `fono models list` to see available names"
        );
        return Ok(EnsureOutcome::Unknown);
    };
    let dest = paths
        .whisper_models_dir()
        .join(format!("ggml-{}.bin", info.name));
    if dest.exists() {
        debug!("whisper model ready: {}", dest.display());
        return Ok(EnsureOutcome::AlreadyPresent);
    }
    let url = ModelRegistry::url_for(info);
    info!(
        "whisper model {model_name:?} missing — downloading {} MB from {url}",
        info.approx_mb
    );
    fono_download::download(&url, &dest, info.sha256)
        .await
        .with_context(|| format!("downloading whisper model {model_name:?}"))?;
    info!("whisper model installed: {}", dest.display());
    Ok(EnsureOutcome::Downloaded)
}

/// Ensure the named local LLM (`.gguf`) is on disk. Path resolution
/// matches `fono-llm::factory::resolve_local_model_path`:
/// `<llm_models_dir>/<name>.gguf`.
pub async fn ensure_local_llm(paths: &Paths, model_name: &str) -> Result<EnsureOutcome> {
    let Some(info) = fono_llm::LlmRegistry::get(model_name) else {
        warn!(
            "config references unknown LLM model {model_name:?} — run \
             `fono models list` to see available names"
        );
        return Ok(EnsureOutcome::Unknown);
    };
    let dest = paths.llm_models_dir().join(format!("{}.gguf", info.name));
    if dest.exists() {
        debug!("LLM model ready: {}", dest.display());
        return Ok(EnsureOutcome::AlreadyPresent);
    }
    let url = fono_llm::LlmRegistry::url_for(info);
    info!(
        "LLM model {model_name:?} missing — downloading {} MB from {url}",
        info.approx_mb
    );
    fono_download::download(&url, &dest, info.sha256)
        .await
        .with_context(|| format!("downloading LLM model {model_name:?}"))?;
    info!("LLM model installed: {}", dest.display());
    Ok(EnsureOutcome::Downloaded)
}

/// Approximate download size (MB) for the given local STT model name,
/// or `None` when the registry doesn't know about it. Used by the tray
/// switcher to put a useful number in the "downloading…" notification.
#[must_use]
pub fn local_stt_size_mb(model_name: &str) -> Option<u32> {
    ModelRegistry::get(model_name).map(|m| m.approx_mb)
}

/// Approximate download size (MB) for the given local LLM model name,
/// or `None` when the registry doesn't know about it.
#[must_use]
pub fn local_llm_size_mb(model_name: &str) -> Option<u32> {
    fono_llm::LlmRegistry::get(model_name).map(|m| m.approx_mb)
}
