// SPDX-License-Identifier: GPL-3.0-only
//! Ensure the models referenced by `config.toml` are present on disk,
//! downloading them on demand.
//!
//! Called from the daemon startup path (before the IPC loop begins) and
//! exposed as a helper that the wizard can call after a fresh `Setup`.
//!
//! Only the whisper STT model is covered today; LLM auto-download will
//! follow once the GGUF registry lands.

use anyhow::{Context, Result};
use fono_core::config::{Config, SttBackend};
use fono_core::Paths;
use fono_stt::ModelRegistry;
use tracing::{info, warn};

/// Check every model the current config references and download any that
/// are missing. Returns Ok(()) on success; individual failures log a
/// warning but do not abort the daemon.
pub async fn ensure_models(paths: &Paths, config: &Config) -> Result<()> {
    // -----------------------------------------------------------------
    // Whisper (local STT)
    // -----------------------------------------------------------------
    if config.stt.backend == SttBackend::Local {
        if let Err(e) = ensure_whisper(paths, &config.stt.local.model).await {
            warn!("auto-download of whisper model failed: {e:#}");
        }
    }
    // TODO: LLM GGUF auto-download once the llm registry lands.
    Ok(())
}

async fn ensure_whisper(paths: &Paths, model_name: &str) -> Result<()> {
    let Some(info) = ModelRegistry::get(model_name) else {
        warn!(
            "config references unknown whisper model {model_name:?} — run \
             `fono models list` to see available names"
        );
        return Ok(());
    };
    let dest = paths
        .whisper_models_dir()
        .join(format!("ggml-{}.bin", info.name));
    if dest.exists() {
        info!("whisper model ready: {}", dest.display());
        return Ok(());
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
    Ok(())
}
