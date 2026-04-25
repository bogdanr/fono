// SPDX-License-Identifier: GPL-3.0-only
//! Build a concrete [`TextFormatter`] (or `None` when LLM cleanup is
//! disabled) from `Config` + `Secrets`.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use fono_core::config::{Llm, LlmBackend};
use fono_core::Secrets;

use crate::traits::TextFormatter;

/// Returns `Ok(None)` when `cfg.enabled == false` or `cfg.backend == None`.
/// Otherwise returns the constructed backend or an error explaining why
/// construction failed (missing API key, missing feature, etc.).
pub fn build_llm(cfg: &Llm, secrets: &Secrets) -> Result<Option<Arc<dyn TextFormatter>>> {
    if !cfg.enabled || matches!(cfg.backend, LlmBackend::None) {
        return Ok(None);
    }

    let provider_name = match &cfg.backend {
        LlmBackend::Cerebras => "cerebras",
        LlmBackend::Groq => "groq",
        LlmBackend::OpenAI => "openai",
        LlmBackend::Anthropic => "anthropic",
        LlmBackend::OpenRouter => "openrouter",
        LlmBackend::Ollama => "ollama",
        LlmBackend::Gemini => "gemini",
        LlmBackend::Local => "local",
        LlmBackend::None => unreachable!(),
    };

    let model = cfg
        .cloud
        .as_ref()
        .map(|c| c.model.clone())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| crate::defaults::default_cloud_model(provider_name).to_string());

    let resolve_key = || -> Result<String> {
        let cloud = cfg.cloud.as_ref().ok_or_else(|| {
            anyhow!("llm.cloud not configured for {provider_name} backend; run `fono setup` again")
        })?;
        secrets.resolve(&cloud.api_key_ref).ok_or_else(|| {
            anyhow!(
                "LLM API key {:?} not found in secrets.toml or environment",
                cloud.api_key_ref
            )
        })
    };

    match &cfg.backend {
        LlmBackend::Cerebras => build_cerebras(resolve_key()?, model),
        LlmBackend::Groq => build_oa_groq(resolve_key()?, model),
        LlmBackend::OpenAI => build_oa_openai(resolve_key()?, model),
        LlmBackend::OpenRouter => build_oa_openrouter(resolve_key()?, model),
        LlmBackend::Ollama => build_oa_ollama(cfg, model),
        LlmBackend::Anthropic => build_anthropic(resolve_key()?, model),
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
