// SPDX-License-Identifier: GPL-3.0-only
//! Build a concrete [`TextFormatter`] (or `None` when LLM cleanup is
//! disabled) from `Config` + `Secrets`.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use fono_core::config::{Llm, LlmBackend};
use fono_core::providers::llm_key_env;
use fono_core::Secrets;

use crate::traits::TextFormatter;

/// Resolve `(key, model)` for a cloud LLM backend, falling through to
/// the canonical env var when `cfg.cloud` is missing or fields blank.
fn resolve_cloud(
    cfg: &Llm,
    secrets: &Secrets,
    backend: &LlmBackend,
    provider_name: &str,
) -> Result<(String, String)> {
    let canonical = llm_key_env(backend);
    let (key_ref, model_override) = cfg.cloud.as_ref().map_or_else(
        || (canonical.to_string(), None),
        |c| {
            let key_ref = if c.api_key_ref.is_empty() {
                canonical.to_string()
            } else {
                c.api_key_ref.clone()
            };
            let model_override = if c.model.is_empty() {
                None
            } else {
                Some(c.model.clone())
            };
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
/// construction failed (missing API key, missing feature, etc.).
pub fn build_llm(cfg: &Llm, secrets: &Secrets) -> Result<Option<Arc<dyn TextFormatter>>> {
    if !cfg.enabled || matches!(cfg.backend, LlmBackend::None) {
        return Ok(None);
    }

    match &cfg.backend {
        LlmBackend::Cerebras => {
            let (k, m) = resolve_cloud(cfg, secrets, &LlmBackend::Cerebras, "cerebras")?;
            build_cerebras(k, m)
        }
        LlmBackend::Groq => {
            let (k, m) = resolve_cloud(cfg, secrets, &LlmBackend::Groq, "groq")?;
            build_oa_groq(k, m)
        }
        LlmBackend::OpenAI => {
            let (k, m) = resolve_cloud(cfg, secrets, &LlmBackend::OpenAI, "openai")?;
            build_oa_openai(k, m)
        }
        LlmBackend::OpenRouter => {
            let (k, m) = resolve_cloud(cfg, secrets, &LlmBackend::OpenRouter, "openrouter")?;
            build_oa_openrouter(k, m)
        }
        LlmBackend::Ollama => {
            let model = cfg
                .cloud
                .as_ref()
                .map(|c| c.model.clone())
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| crate::defaults::default_cloud_model("ollama").to_string());
            build_oa_ollama(cfg, model)
        }
        LlmBackend::Anthropic => {
            let (k, m) = resolve_cloud(cfg, secrets, &LlmBackend::Anthropic, "anthropic")?;
            build_anthropic(k, m)
        }
        LlmBackend::Local => build_local(cfg),
        LlmBackend::Gemini => Err(anyhow!(
            "Gemini LLM backend not yet implemented; pick cerebras/openai/anthropic"
        )),
        LlmBackend::None => unreachable!(),
    }
    .map(Some)
}
#[cfg(feature = "cerebras")]
#[allow(clippy::unnecessary_wraps)]
fn build_cerebras(key: String, model: String) -> Result<Arc<dyn TextFormatter>> {
    Ok(Arc::new(crate::openai_compat::OpenAiCompat::cerebras(
        key, model,
    )))
}

#[cfg(not(feature = "cerebras"))]
fn build_cerebras(_: String, _: String) -> Result<Arc<dyn TextFormatter>> {
    Err(anyhow!(
        "Cerebras LLM not compiled in (enable the `cerebras` feature on `fono-llm`)"
    ))
}

#[cfg(feature = "openai-compat")]
#[allow(clippy::unnecessary_wraps)]
fn build_oa_groq(key: String, model: String) -> Result<Arc<dyn TextFormatter>> {
    Ok(Arc::new(crate::openai_compat::OpenAiCompat::groq(
        key, model,
    )))
}

#[cfg(feature = "openai-compat")]
#[allow(clippy::unnecessary_wraps)]
fn build_oa_openai(key: String, model: String) -> Result<Arc<dyn TextFormatter>> {
    Ok(Arc::new(crate::openai_compat::OpenAiCompat::openai(
        key, model,
    )))
}

#[cfg(feature = "openai-compat")]
#[allow(clippy::unnecessary_wraps)]
fn build_oa_openrouter(key: String, model: String) -> Result<Arc<dyn TextFormatter>> {
    Ok(Arc::new(crate::openai_compat::OpenAiCompat::openrouter(
        key, model,
    )))
}

#[cfg(feature = "openai-compat")]
#[allow(clippy::unnecessary_wraps)]
fn build_oa_ollama(cfg: &Llm, model: String) -> Result<Arc<dyn TextFormatter>> {
    // Ollama doesn't need an API key; the endpoint is the local URL stored
    // in the cloud.api_key_ref slot when configured. Fall back to the
    // upstream default.
    let endpoint = cfg
        .cloud
        .as_ref()
        .map(|c| c.api_key_ref.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "http://localhost:11434/v1/chat/completions".to_string());
    Ok(Arc::new(crate::openai_compat::OpenAiCompat::ollama(
        endpoint, model,
    )))
}

#[cfg(not(feature = "openai-compat"))]
fn build_oa_groq(_: String, _: String) -> Result<Arc<dyn TextFormatter>> {
    Err(anyhow!(
        "Groq LLM not compiled in (enable the `openai-compat` feature on `fono-llm`)"
    ))
}

#[cfg(not(feature = "openai-compat"))]
fn build_oa_openai(_: String, _: String) -> Result<Arc<dyn TextFormatter>> {
    Err(anyhow!(
        "OpenAI LLM not compiled in (enable the `openai-compat` feature on `fono-llm`)"
    ))
}

#[cfg(not(feature = "openai-compat"))]
fn build_oa_openrouter(_: String, _: String) -> Result<Arc<dyn TextFormatter>> {
    Err(anyhow!(
        "OpenRouter LLM not compiled in (enable the `openai-compat` feature on `fono-llm`)"
    ))
}

#[cfg(not(feature = "openai-compat"))]
fn build_oa_ollama(_: &Llm, _: String) -> Result<Arc<dyn TextFormatter>> {
    Err(anyhow!(
        "Ollama LLM not compiled in (enable the `openai-compat` feature on `fono-llm`)"
    ))
}

#[cfg(feature = "anthropic")]
fn build_anthropic(key: String, model: String) -> Result<Arc<dyn TextFormatter>> {
    Ok(Arc::new(crate::anthropic::AnthropicLlm::new(key, model)))
}

#[cfg(not(feature = "anthropic"))]
fn build_anthropic(_: String, _: String) -> Result<Arc<dyn TextFormatter>> {
    Err(anyhow!(
        "Anthropic LLM not compiled in (enable the `anthropic` feature on `fono-llm`)"
    ))
}

#[cfg(feature = "llama-local")]
fn build_local(cfg: &Llm) -> Result<Arc<dyn TextFormatter>> {
    Ok(Arc::new(crate::llama_local::LlamaLocal::new(
        // Caller is expected to resolve the path; for now we use the
        // model name as a placeholder so the construction succeeds and
        // the scaffold's clear runtime error surfaces on first use.
        std::path::PathBuf::from(&cfg.local.model),
        cfg.local.context,
    )))
}

#[cfg(not(feature = "llama-local"))]
fn build_local(_: &Llm) -> Result<Arc<dyn TextFormatter>> {
    Err(anyhow!(
        "local LLM requested but this binary was built without the \
         `llama-local` feature; rebuild with `cargo build --features llama-local` \
         or pick a cloud LLM backend in `fono setup`"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fono_core::config::Llm as LlmCfg;

    #[test]
    fn disabled_returns_none() {
        let mut cfg = LlmCfg::default();
        cfg.enabled = false;
        let s = Secrets::default();
        assert!(build_llm(&cfg, &s).unwrap().is_none());
    }

    #[test]
    fn backend_none_returns_none() {
        let mut cfg = LlmCfg::default();
        cfg.backend = LlmBackend::None;
        cfg.enabled = true;
        let s = Secrets::default();
        assert!(build_llm(&cfg, &s).unwrap().is_none());
    }
}
