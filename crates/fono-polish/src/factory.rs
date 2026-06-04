// SPDX-License-Identifier: GPL-3.0-only
//! Build a concrete [`TextFormatter`] (or `None` when polish is
//! disabled) from `Config` + `Secrets`.

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use fono_core::config::{Polish, PolishBackend};
use fono_core::providers::polish_key_env;
use fono_core::Secrets;

use crate::traits::TextFormatter;

/// Resolve `(key, model)` for a cloud polish backend, falling through to
/// the canonical env var when `cfg.cloud` is missing or fields blank.
fn resolve_cloud(
    cfg: &Polish,
    secrets: &Secrets,
    backend: &PolishBackend,
    provider_name: &str,
) -> Result<(String, String)> {
    let canonical = polish_key_env(backend);
    let (key_ref, model_override) = cfg.cloud.as_ref().map_or_else(
        || (canonical.to_string(), None),
        |c| {
            let key_ref = if c.api_key_ref.is_empty() {
                canonical.to_string()
            } else {
                c.api_key_ref.clone()
            };
            let model_override = if c.model.is_empty() { None } else { Some(c.model.clone()) };
            (key_ref, model_override)
        },
    );
    let key = secrets.resolve(&key_ref).ok_or_else(|| {
        anyhow!(
            "{provider_name} LLM API key {key_ref:?} not found in secrets.toml or environment; \
             run `fono keys add {key_ref}` to add it"
        )
    })?;
    let model = model_override
        .unwrap_or_else(|| crate::defaults::default_cloud_model(provider_name).to_string());
    Ok((key, model))
}

/// Returns `Ok(None)` when `cfg.enabled == false` or `cfg.backend == None`.
/// Otherwise returns the constructed backend or an error explaining why
/// construction failed (missing API key, missing model file, missing feature
/// flag, etc.).
///
/// `polish_models_dir` is the on-disk directory where local LLM GGUF weights
/// live (typically `~/.local/share/fono/models/polish/`). It is only consulted
/// when `cfg.backend == PolishBackend::Local`.
pub fn build_polish(
    cfg: &Polish,
    secrets: &Secrets,
    polish_models_dir: &Path,
) -> Result<Option<Arc<dyn TextFormatter>>> {
    if !cfg.enabled || matches!(cfg.backend, PolishBackend::None) {
        return Ok(None);
    }

    match &cfg.backend {
        PolishBackend::Cerebras => {
            let (k, m) = resolve_cloud(cfg, secrets, &PolishBackend::Cerebras, "cerebras")?;
            build_cerebras(k, m)
        }
        PolishBackend::Groq => {
            let (k, m) = resolve_cloud(cfg, secrets, &PolishBackend::Groq, "groq")?;
            build_oa_groq(k, m)
        }
        PolishBackend::OpenAI => {
            let (k, m) = resolve_cloud(cfg, secrets, &PolishBackend::OpenAI, "openai")?;
            build_oa_openai(k, m)
        }
        PolishBackend::OpenRouter => {
            let (k, m) = resolve_cloud(cfg, secrets, &PolishBackend::OpenRouter, "openrouter")?;
            build_oa_openrouter(k, m)
        }
        PolishBackend::Ollama => {
            let model = cfg
                .cloud
                .as_ref()
                .map(|c| c.model.clone())
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| crate::defaults::default_cloud_model("ollama").to_string());
            build_oa_ollama(cfg, model)
        }
        PolishBackend::Anthropic => {
            let (k, m) = resolve_cloud(cfg, secrets, &PolishBackend::Anthropic, "anthropic")?;
            build_anthropic(k, m)
        }
        PolishBackend::Local => build_local(cfg, polish_models_dir),
        PolishBackend::Gemini => Err(anyhow!(
            "Gemini polish backend not yet implemented; pick cerebras/openai/anthropic"
        )),
        PolishBackend::None => unreachable!(),
    }
    .map(Some)
}

/// Resolve the model name in `cfg.local.model` to a `<polish_models_dir>/<name>.gguf`
/// path. Mirrors the whisper resolver in `fono::models::ensure_whisper`.
#[cfg(any(feature = "llama-local", test))]
fn resolve_local_model_path(cfg: &Polish, polish_models_dir: &Path) -> std::path::PathBuf {
    polish_models_dir.join(format!("{}.gguf", cfg.local.model))
}
#[cfg(feature = "cerebras")]
#[allow(clippy::unnecessary_wraps)]
fn build_cerebras(key: String, model: String) -> Result<Arc<dyn TextFormatter>> {
    Ok(Arc::new(crate::openai_compat::OpenAiCompat::cerebras(key, model)))
}

#[cfg(not(feature = "cerebras"))]
fn build_cerebras(_: String, _: String) -> Result<Arc<dyn TextFormatter>> {
    Err(anyhow!("Cerebras LLM not compiled in (enable the `cerebras` feature on `fono-polish`)"))
}

#[cfg(feature = "openai-compat")]
#[allow(clippy::unnecessary_wraps)]
fn build_oa_groq(key: String, model: String) -> Result<Arc<dyn TextFormatter>> {
    Ok(Arc::new(crate::openai_compat::OpenAiCompat::groq(key, model)))
}

#[cfg(feature = "openai-compat")]
#[allow(clippy::unnecessary_wraps)]
fn build_oa_openai(key: String, model: String) -> Result<Arc<dyn TextFormatter>> {
    Ok(Arc::new(crate::openai_compat::OpenAiCompat::openai(key, model)))
}

#[cfg(feature = "openai-compat")]
#[allow(clippy::unnecessary_wraps)]
fn build_oa_openrouter(key: String, model: String) -> Result<Arc<dyn TextFormatter>> {
    Ok(Arc::new(crate::openai_compat::OpenAiCompat::openrouter(key, model)))
}

#[cfg(feature = "openai-compat")]
#[allow(clippy::unnecessary_wraps)]
fn build_oa_ollama(cfg: &Polish, model: String) -> Result<Arc<dyn TextFormatter>> {
    // Ollama doesn't need an API key; the endpoint is the local URL stored
    // in the cloud.api_key_ref slot when configured. Fall back to the
    // upstream default.
    let endpoint = cfg
        .cloud
        .as_ref()
        .map(|c| c.api_key_ref.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "http://localhost:11434/v1/chat/completions".to_string());
    Ok(Arc::new(crate::openai_compat::OpenAiCompat::ollama(endpoint, model)))
}

#[cfg(not(feature = "openai-compat"))]
fn build_oa_groq(_: String, _: String) -> Result<Arc<dyn TextFormatter>> {
    Err(anyhow!("Groq LLM not compiled in (enable the `openai-compat` feature on `fono-polish`)"))
}

#[cfg(not(feature = "openai-compat"))]
fn build_oa_openai(_: String, _: String) -> Result<Arc<dyn TextFormatter>> {
    Err(anyhow!("OpenAI LLM not compiled in (enable the `openai-compat` feature on `fono-polish`)"))
}

#[cfg(not(feature = "openai-compat"))]
fn build_oa_openrouter(_: String, _: String) -> Result<Arc<dyn TextFormatter>> {
    Err(anyhow!(
        "OpenRouter LLM not compiled in (enable the `openai-compat` feature on `fono-polish`)"
    ))
}

#[cfg(not(feature = "openai-compat"))]
fn build_oa_ollama(_: &Polish, _: String) -> Result<Arc<dyn TextFormatter>> {
    Err(anyhow!("Ollama LLM not compiled in (enable the `openai-compat` feature on `fono-polish`)"))
}

#[cfg(feature = "anthropic")]
#[allow(clippy::unnecessary_wraps)]
fn build_anthropic(key: String, model: String) -> Result<Arc<dyn TextFormatter>> {
    Ok(Arc::new(crate::anthropic::AnthropicLlm::new(key, model)))
}

#[cfg(not(feature = "anthropic"))]
fn build_anthropic(_: String, _: String) -> Result<Arc<dyn TextFormatter>> {
    Err(anyhow!("Anthropic LLM not compiled in (enable the `anthropic` feature on `fono-polish`)"))
}

#[cfg(feature = "llama-local")]
#[allow(clippy::unnecessary_wraps)]
fn build_local(cfg: &Polish, polish_models_dir: &Path) -> Result<Arc<dyn TextFormatter>> {
    Ok(Arc::new(crate::llama_local::LlamaLocal::new(
        resolve_local_model_path(cfg, polish_models_dir),
        cfg.local.context,
    )))
}

#[cfg(not(feature = "llama-local"))]
fn build_local(_: &Polish, _: &Path) -> Result<Arc<dyn TextFormatter>> {
    Err(anyhow!(
        "local LLM requested but this binary was built without the \
         `llama-local` feature; rebuild with `cargo build --features llama-local` \
         or pick a cloud polish backend in `fono setup`"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fono_core::config::Polish as LlmCfg;

    #[test]
    fn disabled_returns_none() {
        let cfg = LlmCfg { enabled: false, ..LlmCfg::default() };
        let s = Secrets::default();
        assert!(build_polish(&cfg, &s, Path::new("/nonexistent")).unwrap().is_none());
    }

    #[test]
    fn backend_none_returns_none() {
        let cfg = LlmCfg { backend: PolishBackend::None, enabled: true, ..LlmCfg::default() };
        let s = Secrets::default();
        assert!(build_polish(&cfg, &s, Path::new("/nonexistent")).unwrap().is_none());
    }

    #[test]
    fn local_path_resolution_uses_models_dir() {
        let cfg = LlmCfg {
            local: fono_core::config::PolishLocal {
                model: "qwen3.5-2b".into(),
                ..fono_core::config::PolishLocal::default()
            },
            ..LlmCfg::default()
        };
        let dir = Path::new("/var/lib/fono/polish");
        let p = resolve_local_model_path(&cfg, dir);
        assert_eq!(p, std::path::PathBuf::from("/var/lib/fono/polish/qwen3.5-2b.gguf"));
    }
}
