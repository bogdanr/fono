// SPDX-License-Identifier: GPL-3.0-only
//! Build a concrete [`Assistant`] from `Config` + `Secrets`.
//!
//! Returns `Ok(None)` when the assistant is disabled in config, so
//! callers can treat "no assistant" without matching on the enum.
//! Otherwise mirrors [`fono_llm::factory::build_llm`] with chat
//! invariants (different default models, different prompt usage).

#[allow(unused_imports)]
use std::sync::Arc;

use anyhow::{anyhow, Result};
use fono_core::config::{Assistant as AssistantCfg, AssistantBackend};
#[cfg(any(feature = "openai-compat", feature = "anthropic"))]
use fono_core::providers::assistant_key_env;
#[allow(unused_imports)]
use fono_core::Secrets;

use crate::traits::Assistant;

/// Resolve `(api_key, model)` for a cloud assistant backend, falling
/// through to the canonical env var and the [`default_cloud_model`]
/// when the relevant fields in `[assistant.cloud]` are blank.
#[cfg(any(feature = "openai-compat", feature = "anthropic"))]
fn resolve_cloud(
    cfg: &AssistantCfg,
    secrets: &Secrets,
    backend: &AssistantBackend,
    provider_name: &str,
) -> Result<(String, String)> {
    let canonical = assistant_key_env(backend);
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
            "{provider_name} assistant API key {key_ref:?} not found in secrets.toml or environment; \
             run `fono keys add {key_ref}` to add it"
        )
    })?;
    let model = model_override.unwrap_or_else(|| default_cloud_model(provider_name).to_string());
    Ok((key, model))
}

/// Default chat model per provider. Tuned for the assistant's "1-3
/// sentences, fast" contract — mid-tier where possible.
#[cfg(any(feature = "openai-compat", feature = "anthropic"))]
fn default_cloud_model(provider: &str) -> &'static str {
    match provider {
        "anthropic" => "claude-haiku-4-5-20251001",
        // OpenAI: assistant uses the larger sibling of the cleanup
        // model so it can sustain a multi-turn conversation.
        "openai" => "gpt-5.4-mini",
        // Cerebras: qwen-3-235b-a22b-instruct-2507 is the default
        // because it's available on every Cerebras tier (the
        // alternative `gpt-oss-120b` is gated to billable / preview
        // accounts and 404s on free-tier keys). Groq exposes OpenAI's
        // open-weight gpt-oss-120b under an `openai/` namespace
        // prefix and serves it without per-account gating, so it
        // stays the Groq default.
        "cerebras" => "qwen-3-235b-a22b-instruct-2507",
        "groq" => "openai/gpt-oss-120b",
        "openrouter" => "anthropic/claude-haiku-4.5",
        // Ollama: user must configure their own — "default" model
        // names depend on what they've pulled.
        "ollama" => "llama3.2",
        _ => "",
    }
}

/// Construct an assistant backend from `cfg`. Returns `Ok(None)` for
/// `enabled = false` or `backend = none`. Errors include missing API
/// keys, missing feature flags, or unimplemented backends.
pub fn build_assistant(
    cfg: &AssistantCfg,
    secrets: &Secrets,
) -> Result<Option<Arc<dyn Assistant>>> {
    if !cfg.enabled || matches!(cfg.backend, AssistantBackend::None) {
        return Ok(None);
    }
    match &cfg.backend {
        AssistantBackend::None => Ok(None),
        AssistantBackend::Cerebras => build_cerebras(cfg, secrets).map(Some),
        AssistantBackend::Groq => build_groq(cfg, secrets).map(Some),
        AssistantBackend::OpenAI => build_openai(cfg, secrets).map(Some),
        AssistantBackend::OpenRouter => build_openrouter(cfg, secrets).map(Some),
        AssistantBackend::Ollama => build_ollama(cfg).map(Some),
        AssistantBackend::Anthropic => build_anthropic(cfg, secrets).map(Some),
        AssistantBackend::Local => build_local(cfg).map(Some),
        AssistantBackend::Gemini => Err(anyhow!(
            "Gemini assistant backend is not implemented yet — pick anthropic, \
             cerebras, openai, groq, openrouter, ollama, or local"
        )),
    }
}

#[cfg(feature = "openai-compat")]
fn build_cerebras(cfg: &AssistantCfg, secrets: &Secrets) -> Result<Arc<dyn Assistant>> {
    let (k, m) = resolve_cloud(cfg, secrets, &AssistantBackend::Cerebras, "cerebras")?;
    Ok(Arc::new(
        crate::openai_compat_chat::OpenAiCompatChat::cerebras(k, m),
    ))
}

#[cfg(feature = "openai-compat")]
fn build_groq(cfg: &AssistantCfg, secrets: &Secrets) -> Result<Arc<dyn Assistant>> {
    let (k, m) = resolve_cloud(cfg, secrets, &AssistantBackend::Groq, "groq")?;
    Ok(Arc::new(crate::openai_compat_chat::OpenAiCompatChat::groq(
        k, m,
    )))
}

#[cfg(feature = "openai-compat")]
fn build_openai(cfg: &AssistantCfg, secrets: &Secrets) -> Result<Arc<dyn Assistant>> {
    let (k, m) = resolve_cloud(cfg, secrets, &AssistantBackend::OpenAI, "openai")?;
    Ok(Arc::new(
        crate::openai_compat_chat::OpenAiCompatChat::openai(k, m),
    ))
}

#[cfg(feature = "openai-compat")]
fn build_openrouter(cfg: &AssistantCfg, secrets: &Secrets) -> Result<Arc<dyn Assistant>> {
    let (k, m) = resolve_cloud(cfg, secrets, &AssistantBackend::OpenRouter, "openrouter")?;
    Ok(Arc::new(
        crate::openai_compat_chat::OpenAiCompatChat::openrouter(k, m),
    ))
}

// Returns Result for symmetry with the other build_* functions, even
// though Ollama construction can't currently fail (no key resolution).
#[cfg(feature = "openai-compat")]
#[allow(clippy::unnecessary_wraps)]
fn build_ollama(cfg: &AssistantCfg) -> Result<Arc<dyn Assistant>> {
    // Ollama needs an explicit endpoint; we lift it from cfg.cloud.api_key_ref
    // when provided (interpreted as a URL placeholder), else default to
    // localhost. Same convention as fono-llm.
    let endpoint = cfg
        .cloud
        .as_ref()
        .and_then(|c| {
            let ref_str = &c.api_key_ref;
            if ref_str.starts_with("http://") || ref_str.starts_with("https://") {
                Some(ref_str.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "http://localhost:11434/v1/chat/completions".to_string());
    let model = cfg
        .cloud
        .as_ref()
        .map(|c| c.model.clone())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| default_cloud_model("ollama").to_string());
    Ok(Arc::new(
        crate::openai_compat_chat::OpenAiCompatChat::ollama(endpoint, model),
    ))
}

#[cfg(not(feature = "openai-compat"))]
fn build_cerebras(_cfg: &AssistantCfg, _secrets: &Secrets) -> Result<Arc<dyn Assistant>> {
    Err(anyhow!(
        "OpenAI-compatible assistant backends not compiled in (enable the `openai-compat` feature on `fono-assistant`)"
    ))
}
#[cfg(not(feature = "openai-compat"))]
fn build_groq(_cfg: &AssistantCfg, _secrets: &Secrets) -> Result<Arc<dyn Assistant>> {
    Err(anyhow!(
        "OpenAI-compatible assistant backends not compiled in (enable the `openai-compat` feature on `fono-assistant`)"
    ))
}
#[cfg(not(feature = "openai-compat"))]
fn build_openai(_cfg: &AssistantCfg, _secrets: &Secrets) -> Result<Arc<dyn Assistant>> {
    Err(anyhow!(
        "OpenAI-compatible assistant backends not compiled in (enable the `openai-compat` feature on `fono-assistant`)"
    ))
}
#[cfg(not(feature = "openai-compat"))]
fn build_openrouter(_cfg: &AssistantCfg, _secrets: &Secrets) -> Result<Arc<dyn Assistant>> {
    Err(anyhow!(
        "OpenAI-compatible assistant backends not compiled in (enable the `openai-compat` feature on `fono-assistant`)"
    ))
}
#[cfg(not(feature = "openai-compat"))]
fn build_ollama(_cfg: &AssistantCfg) -> Result<Arc<dyn Assistant>> {
    Err(anyhow!(
        "OpenAI-compatible assistant backends not compiled in (enable the `openai-compat` feature on `fono-assistant`)"
    ))
}

#[cfg(feature = "anthropic")]
fn build_anthropic(cfg: &AssistantCfg, secrets: &Secrets) -> Result<Arc<dyn Assistant>> {
    let (k, m) = resolve_cloud(cfg, secrets, &AssistantBackend::Anthropic, "anthropic")?;
    Ok(Arc::new(crate::anthropic_chat::AnthropicChat::new(k, m)))
}

#[cfg(not(feature = "anthropic"))]
fn build_anthropic(_cfg: &AssistantCfg, _secrets: &Secrets) -> Result<Arc<dyn Assistant>> {
    Err(anyhow!(
        "Anthropic assistant not compiled in (enable the `anthropic` feature on `fono-assistant`)"
    ))
}

fn build_local(_cfg: &AssistantCfg) -> Result<Arc<dyn Assistant>> {
    // Stub. Streaming local llama.cpp wires through the same mutex-
    // guarded inference context as the cleanup path; threading
    // streaming through it is a follow-up. For now point users at a
    // cloud backend.
    Err(anyhow!(
        "local llama.cpp assistant streaming is not yet implemented in this build. Pick a \
         cloud backend (anthropic, cerebras, openai, groq, openrouter) or run an Ollama \
         server locally and select `ollama` instead."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fono_core::config::{Assistant as AssistantCfg, AssistantBackend, AssistantCloud};

    #[test]
    fn disabled_returns_none() {
        let cfg = AssistantCfg {
            enabled: false,
            backend: AssistantBackend::Anthropic,
            ..AssistantCfg::default()
        };
        let secrets = Secrets::default();
        assert!(build_assistant(&cfg, &secrets).unwrap().is_none());
    }

    #[test]
    fn none_backend_returns_none_even_when_enabled() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::None,
            ..AssistantCfg::default()
        };
        assert!(build_assistant(&cfg, &Secrets::default())
            .unwrap()
            .is_none());
    }

    #[cfg(feature = "anthropic")]
    #[test]
    fn anthropic_missing_key_errors_clearly() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Anthropic,
            cloud: Some(AssistantCloud::default()),
            ..AssistantCfg::default()
        };
        let err = build_assistant(&cfg, &Secrets::default())
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.contains("ANTHROPIC_API_KEY") && err.contains("fono keys add"),
            "{err}"
        );
    }

    #[cfg(feature = "anthropic")]
    #[test]
    fn anthropic_with_env_key_succeeds() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Anthropic,
            cloud: None,
            ..AssistantCfg::default()
        };
        let mut secrets = Secrets::default();
        secrets.insert("ANTHROPIC_API_KEY", "sk-ant-test");
        assert!(build_assistant(&cfg, &secrets).unwrap().is_some());
    }

    #[cfg(feature = "openai-compat")]
    #[test]
    fn openai_with_env_key_succeeds() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::OpenAI,
            cloud: None,
            ..AssistantCfg::default()
        };
        let mut secrets = Secrets::default();
        secrets.insert("OPENAI_API_KEY", "sk-test");
        assert!(build_assistant(&cfg, &secrets).unwrap().is_some());
    }

    #[test]
    fn local_backend_errors_with_helpful_pointer() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Local,
            ..AssistantCfg::default()
        };
        let err = build_assistant(&cfg, &Secrets::default())
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("ollama") || err.contains("cloud"), "{err}");
    }

    #[test]
    fn gemini_errors_for_unimplemented() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Gemini,
            ..AssistantCfg::default()
        };
        let err = build_assistant(&cfg, &Secrets::default())
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.contains("Gemini") || err.contains("not implemented"),
            "{err}"
        );
    }
}
