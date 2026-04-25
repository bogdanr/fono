// SPDX-License-Identifier: GPL-3.0-only
//! Build a concrete [`SpeechToText`] from `Config` + `Secrets`.
//!
//! Cloud branches are gated by feature flags; missing features produce a
//! clear error so the daemon can stay running in a "degraded" state and
//! the user can fix the config without recompiling the world.

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use fono_core::config::{Stt, SttBackend};
use fono_core::Secrets;

use crate::traits::SpeechToText;

/// Construct an STT backend matching `cfg`. The returned [`Arc`] is
/// `Send + Sync` so it can be shared across the orchestrator's tokio tasks.
///
/// `whisper_models_dir` is consulted only for [`SttBackend::Local`].
pub fn build_stt(
    cfg: &Stt,
    secrets: &Secrets,
    whisper_models_dir: &Path,
) -> Result<Arc<dyn SpeechToText>> {
    match &cfg.backend {
        SttBackend::Local => build_local(cfg, whisper_models_dir),
        SttBackend::Groq => build_groq(cfg, secrets),
        SttBackend::OpenAI => build_openai(cfg, secrets),
        other => Err(anyhow!(
            "STT backend {other:?} is not yet implemented in this build; \
             pick `groq`, `openai`, or `local` (rebuild with `--features whisper-local` \
             for `local`)"
        )),
    }
}

#[cfg(feature = "whisper-local")]
fn build_local(cfg: &Stt, dir: &Path) -> Result<Arc<dyn SpeechToText>> {
    let model = &cfg.local.model;
    let path = dir.join(format!("ggml-{model}.bin"));
    if !path.exists() {
        return Err(anyhow!(
            "local whisper model {model:?} not found at {} — \
             run `fono models install {model}`",
            path.display()
        ));
    }
    // Latency plan L18 — `0` means auto-detect physical cores so we
    // don't oversubscribe SMT siblings. `with_threads` clamps the
    // value into the i32 range whisper-rs expects.
    let threads: i32 = match cfg.local.threads {
        0 => i32::try_from(detect_physical_cores()).unwrap_or(4),
        n => i32::try_from(n).unwrap_or(i32::MAX),
    };
    Ok(Arc::new(crate::whisper_local::WhisperLocal::with_threads(
        path, threads,
    )))
}

/// Best-effort physical-core count. Falls back to `available_parallelism`
/// (which usually reports logical cores) and then to 4 if even that
/// fails. Whisper.cpp scales sub-linearly past the physical-core count
/// because it's MAC-bound, so over-counting hurts more than it helps.
#[cfg(feature = "whisper-local")]
fn detect_physical_cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| {
            // Heuristic: assume SMT siblings, halve, but never go
            // below 1. Users who care can override `stt.local.threads`.
            (n.get() / 2).max(1)
        })
        .unwrap_or(4)
}

#[cfg(not(feature = "whisper-local"))]
fn build_local(_cfg: &Stt, _dir: &Path) -> Result<Arc<dyn SpeechToText>> {
    Err(anyhow!(
        "local STT requested but this binary was built without the \
         `whisper-local` feature; rebuild with `cargo build --features whisper-local` \
         or pick a cloud STT backend in `fono setup`"
    ))
}

#[cfg(feature = "groq")]
fn build_groq(cfg: &Stt, secrets: &Secrets) -> Result<Arc<dyn SpeechToText>> {
    let cloud = cfg.cloud.as_ref().ok_or_else(|| {
        anyhow!("stt.cloud not configured for Groq backend; run `fono setup` again")
    })?;
    let key = secrets.resolve(&cloud.api_key_ref).ok_or_else(|| {
        anyhow!(
            "Groq STT API key {:?} not found in secrets.toml or environment",
            cloud.api_key_ref
        )
    })?;
    let model = if cloud.model.is_empty() {
        crate::defaults::default_cloud_model("groq").to_string()
    } else {
        cloud.model.clone()
    };
    Ok(Arc::new(crate::groq::GroqStt::with_model(key, model)))
}

#[cfg(not(feature = "groq"))]
fn build_groq(_: &Stt, _: &Secrets) -> Result<Arc<dyn SpeechToText>> {
    Err(anyhow!(
        "Groq STT not compiled in (enable the `groq` feature on `fono-stt`)"
    ))
}

#[cfg(feature = "openai")]
fn build_openai(cfg: &Stt, secrets: &Secrets) -> Result<Arc<dyn SpeechToText>> {
    let cloud = cfg
        .cloud
        .as_ref()
        .ok_or_else(|| anyhow!("stt.cloud not configured for OpenAI backend"))?;
    let key = secrets.resolve(&cloud.api_key_ref).ok_or_else(|| {
        anyhow!(
            "OpenAI STT API key {:?} not found in secrets.toml or environment",
            cloud.api_key_ref
        )
    })?;
    let model = if cloud.model.is_empty() {
        crate::defaults::default_cloud_model("openai").to_string()
    } else {
        cloud.model.clone()
    };
    Ok(Arc::new(crate::openai::OpenAiStt::with_model(key, model)))
}

#[cfg(not(feature = "openai"))]
fn build_openai(_: &Stt, _: &Secrets) -> Result<Arc<dyn SpeechToText>> {
    Err(anyhow!(
        "OpenAI STT not compiled in (enable the `openai` feature on `fono-stt`)"
    ))
}
