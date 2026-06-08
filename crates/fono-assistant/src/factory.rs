// SPDX-License-Identifier: GPL-3.0-only
//! Build a concrete [`Assistant`] from `Config` + `Secrets`.
//!
//! Returns `Ok(None)` when the assistant is disabled in config, so
//! callers can treat "no assistant" without matching on the enum.
//! Otherwise mirrors [`fono_polish::factory::build_polish`] with chat
//! invariants (different default models, different prompt usage).

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use fono_core::config::{Assistant as AssistantCfg, AssistantBackend};
#[cfg(any(feature = "openai-compat", feature = "anthropic"))]
use fono_core::provider_catalog;
#[cfg(any(feature = "openai-compat", feature = "anthropic"))]
use fono_core::provider_catalog::WebSearchSupport;
#[cfg(any(feature = "openai-compat", feature = "anthropic"))]
use fono_core::providers::assistant_key_env;
#[allow(unused_imports)]
use fono_core::Secrets;

use crate::traits::Assistant;

/// Resolved cloud-assistant parameters: the secret key, the chosen
/// model (text or multimodal depending on `[assistant].prefer_vision`)
/// and the web-search tool id to attach to every request (depending on
/// `[assistant].prefer_web_search` + the catalogue's
/// `WebSearchSupport`).
#[cfg(any(feature = "openai-compat", feature = "anthropic"))]
struct CloudResolution {
    key: String,
    model: String,
    web_search_tool: Option<&'static str>,
}

/// Resolve `(api_key, model, web_search_tool)` for a cloud assistant
/// backend, falling through to the canonical env var and the
/// catalogue's per-provider defaults when the relevant fields in
/// `[assistant.cloud]` are blank. When `cfg.prefer_vision` is set and
/// the catalogue entry exposes `multimodal_model`, that variant is
/// substituted for the text model. When `cfg.prefer_web_search` is
/// set, the catalogue's [`WebSearchSupport::NativeTool`] id is
/// returned so the per-provider chat client can inject it into the
/// request payload.
#[cfg(any(feature = "openai-compat", feature = "anthropic"))]
fn resolve_cloud(
    cfg: &AssistantCfg,
    secrets: &Secrets,
    backend: &AssistantBackend,
    provider_name: &str,
) -> Result<CloudResolution> {
    let canonical = assistant_key_env(backend);
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
            "{provider_name} assistant API key {key_ref:?} not found in secrets.toml or environment; \
             run `fono keys add {key_ref}` to add it"
        )
    })?;
    let entry = provider_catalog::find(provider_name);
    // Phase E4 — swap to the multimodal variant when the user opted
    // in. If they toggled vision on but the catalogue has no
    // multimodal model for this provider (e.g. user flipped primary
    // to Cerebras after toggling vision elsewhere), log a single
    // warning and stay on the text model.
    let base_model =
        model_override.unwrap_or_else(|| default_cloud_model(provider_name).to_string());
    let model = if cfg.prefer_vision {
        let mm = entry.and_then(|e| e.assistant?.multimodal_model);
        mm.map_or_else(
            || {
                tracing::warn!(
                    target: "fono.assistant",
                    "prefer_vision is on but {provider_name} has no multimodal model; \
                     using text model {base_model}"
                );
                base_model.clone()
            },
            ToString::to_string,
        )
    } else {
        base_model
    };

    // Phase E5 — surface the catalogue's web-search tool id when the
    // user opted in. Providers whose catalogue entry says
    // `WebSearchSupport::None` get `None` here regardless of the
    // toggle (so the per-provider chat client never sees a tool id
    // it doesn't know how to inject).
    let web_search_tool = if cfg.prefer_web_search {
        if let Some(WebSearchSupport::NativeTool(id)) =
            entry.and_then(|e| e.assistant.map(|a| a.web_search))
        {
            Some(id)
        } else {
            // `Always` / `None` / missing entry: leave the body
            // untouched. `Always` is reserved for Perplexity-shape
            // providers (no toggle, always grounded) — Phase E ships
            // no such provider.
            None
        }
    } else {
        None
    };

    Ok(CloudResolution { key, model, web_search_tool })
}

/// Default chat model per provider. Reads from the cloud-provider
/// catalogue (`fono_core::provider_catalog`), which is the single
/// source of truth for default model strings. Ollama is special-cased
/// because it has no catalogue entry (it's a self-hosted local
/// server, not a cloud provider). Returns an empty string for
/// unknown ids — the factory surfaces that as a config error.
#[cfg(any(feature = "openai-compat", feature = "anthropic"))]
fn default_cloud_model(provider: &str) -> &'static str {
    if provider == "ollama" {
        return fono_core::config::DEFAULT_POLISH_LOCAL_MODEL;
    }
    provider_catalog::find(provider).and_then(|p| p.assistant.as_ref()).map_or("", |a| a.text_model)
}

/// Construct an assistant backend from `cfg`. Returns `Ok(None)` for
/// `enabled = false` or `backend = none`. Errors include missing API
/// keys, missing feature flags, or unimplemented backends.
pub fn build_assistant(
    cfg: &AssistantCfg,
    secrets: &Secrets,
    assistant_models_dir: &Path,
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
        AssistantBackend::Ollama => build_ollama(cfg, assistant_models_dir).map(Some),
        AssistantBackend::Anthropic => build_anthropic(cfg, secrets).map(Some),
    }
}

#[cfg(feature = "openai-compat")]
fn build_cerebras(cfg: &AssistantCfg, secrets: &Secrets) -> Result<Arc<dyn Assistant>> {
    let r = resolve_cloud(cfg, secrets, &AssistantBackend::Cerebras, "cerebras")?;
    Ok(Arc::new(
        crate::openai_compat_chat::OpenAiCompatChat::cerebras(r.key, r.model)
            .with_web_search(r.web_search_tool),
    ))
}

#[cfg(feature = "openai-compat")]
fn build_groq(cfg: &AssistantCfg, secrets: &Secrets) -> Result<Arc<dyn Assistant>> {
    let r = resolve_cloud(cfg, secrets, &AssistantBackend::Groq, "groq")?;
    Ok(Arc::new(
        crate::openai_compat_chat::OpenAiCompatChat::groq(r.key, r.model)
            .with_web_search(r.web_search_tool),
    ))
}

#[cfg(feature = "openai-compat")]
fn build_openai(cfg: &AssistantCfg, secrets: &Secrets) -> Result<Arc<dyn Assistant>> {
    let r = resolve_cloud(cfg, secrets, &AssistantBackend::OpenAI, "openai")?;
    Ok(Arc::new(
        crate::openai_compat_chat::OpenAiCompatChat::openai(r.key, r.model)
            .with_web_search(r.web_search_tool),
    ))
}

#[cfg(feature = "openai-compat")]
fn build_openrouter(cfg: &AssistantCfg, secrets: &Secrets) -> Result<Arc<dyn Assistant>> {
    let r = resolve_cloud(cfg, secrets, &AssistantBackend::OpenRouter, "openrouter")?;
    Ok(Arc::new(
        crate::openai_compat_chat::OpenAiCompatChat::openrouter(r.key, r.model)
            .with_web_search(r.web_search_tool),
    ))
}

fn manual_local_server_endpoint(cfg: &AssistantCfg) -> Option<String> {
    cfg.cloud.as_ref().and_then(|c| {
        let provider = c.provider.trim();
        let explicitly_manual = matches!(provider, "ollama-server" | "openai-compatible-local");
        let ref_str = &c.api_key_ref;
        if explicitly_manual && (ref_str.starts_with("http://") || ref_str.starts_with("https://"))
        {
            Some(ref_str.clone())
        } else {
            None
        }
    })
}

fn local_model(cfg: &AssistantCfg) -> String {
    cfg.cloud
        .as_ref()
        .map(|c| c.model.clone())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| cfg.local.model.clone())
}

#[cfg(feature = "llama-local")]
fn resolve_local_model_path(cfg: &AssistantCfg, assistant_models_dir: &Path) -> std::path::PathBuf {
    assistant_models_dir.join(format!("{}.gguf", cfg.local.model))
}

#[cfg(feature = "llama-local")]
fn build_embedded_local(
    cfg: &AssistantCfg,
    assistant_models_dir: &Path,
) -> Result<Arc<dyn Assistant>> {
    let model_path = resolve_local_model_path(cfg, assistant_models_dir);
    if !model_path.exists() {
        return Err(anyhow!(
            "local assistant model not found at {:?}; run `fono models install {}` or choose a cloud assistant backend",
            model_path,
            cfg.local.model
        ));
    }
    Ok(Arc::new(crate::llama_local::LlamaLocalAssistant::new(model_path, cfg.local.context)))
}

#[cfg(not(feature = "llama-local"))]
fn build_embedded_local(_: &AssistantCfg, _: &Path) -> Result<Arc<dyn Assistant>> {
    Err(anyhow!(
        "local assistant requested but this binary was built without embedded local assistant support; rebuild with the `llama-local` feature or manually configure an Ollama/OpenAI-compatible local server URL"
    ))
}

// Returns Result for symmetry with the other build_* functions, even
// though local-server construction can't currently fail (no key resolution).
#[cfg(feature = "openai-compat")]
#[allow(clippy::unnecessary_wraps)]
fn build_ollama(cfg: &AssistantCfg, assistant_models_dir: &Path) -> Result<Arc<dyn Assistant>> {
    if let Some(endpoint) = manual_local_server_endpoint(cfg) {
        return Ok(Arc::new(crate::openai_compat_chat::OpenAiCompatChat::ollama(
            endpoint,
            local_model(cfg),
        )));
    }
    build_embedded_local(cfg, assistant_models_dir)
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
fn build_ollama(cfg: &AssistantCfg, assistant_models_dir: &Path) -> Result<Arc<dyn Assistant>> {
    if manual_local_server_endpoint(cfg).is_some() {
        return Err(anyhow!(
            "manual Ollama/OpenAI-compatible assistant server requested but this binary was built without the `openai-compat` feature"
        ));
    }
    build_embedded_local(cfg, assistant_models_dir)
}

#[cfg(feature = "anthropic")]
fn build_anthropic(cfg: &AssistantCfg, secrets: &Secrets) -> Result<Arc<dyn Assistant>> {
    let r = resolve_cloud(cfg, secrets, &AssistantBackend::Anthropic, "anthropic")?;
    Ok(Arc::new(
        crate::anthropic_chat::AnthropicChat::new(r.key, r.model)
            .with_web_search(r.web_search_tool),
    ))
}

#[cfg(not(feature = "anthropic"))]
fn build_anthropic(_cfg: &AssistantCfg, _secrets: &Secrets) -> Result<Arc<dyn Assistant>> {
    Err(anyhow!(
        "Anthropic assistant not compiled in (enable the `anthropic` feature on `fono-assistant`)"
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
        assert!(build_assistant(&cfg, &secrets, Path::new(".")).unwrap().is_none());
    }

    #[test]
    fn none_backend_returns_none_even_when_enabled() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::None,
            ..AssistantCfg::default()
        };
        assert!(build_assistant(&cfg, &Secrets::default(), Path::new(".")).unwrap().is_none());
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
        let err =
            build_assistant(&cfg, &Secrets::default(), Path::new(".")).err().unwrap().to_string();
        assert!(err.contains("ANTHROPIC_API_KEY") && err.contains("fono keys add"), "{err}");
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
        assert!(build_assistant(&cfg, &secrets, Path::new(".")).unwrap().is_some());
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
        assert!(build_assistant(&cfg, &secrets, Path::new(".")).unwrap().is_some());
    }

    #[cfg(feature = "openai-compat")]
    #[test]
    fn local_assistant_uses_embedded_model_by_default() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Ollama,
            ..AssistantCfg::default()
        };
        assert_eq!(cfg.local.model, fono_core::config::DEFAULT_POLISH_LOCAL_MODEL);
        assert!(build_assistant(&cfg, &Secrets::default(), Path::new("/this/path/does/not/exist"))
            .is_err());
    }

    #[cfg(feature = "openai-compat")]
    #[test]
    fn wizard_legacy_ollama_provider_ignores_stale_endpoint() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Ollama,
            cloud: Some(AssistantCloud {
                provider: "ollama".into(),
                api_key_ref: "http://localhost:11434/v1/chat/completions".into(),
                model: fono_core::config::DEFAULT_POLISH_LOCAL_MODEL.into(),
            }),
            ..AssistantCfg::default()
        };
        assert!(build_assistant(&cfg, &Secrets::default(), Path::new("/this/path/does/not/exist"))
            .is_err());
    }

    #[cfg(feature = "openai-compat")]
    #[test]
    fn manual_ollama_endpoint_still_builds_without_model_file() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Ollama,
            cloud: Some(AssistantCloud {
                provider: "ollama-server".into(),
                api_key_ref: "http://localhost:11434/v1/chat/completions".into(),
                model: "gemma3:1b".into(),
            }),
            ..AssistantCfg::default()
        };
        assert!(build_assistant(&cfg, &Secrets::default(), Path::new("/this/path/does/not/exist"))
            .unwrap()
            .is_some());
    }

    // ── Phase E4 + E5: resolve_cloud unit tests ───────────────────
    #[cfg(any(feature = "openai-compat", feature = "anthropic"))]
    fn make_secrets(env: &str) -> Secrets {
        let mut s = Secrets::default();
        s.insert(env, "sk-test");
        s
    }

    #[cfg(feature = "anthropic")]
    #[test]
    fn prefer_vision_swaps_to_multimodal_when_available() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Anthropic,
            prefer_vision: true,
            ..AssistantCfg::default()
        };
        let secrets = make_secrets("ANTHROPIC_API_KEY");
        let r = resolve_cloud(&cfg, &secrets, &AssistantBackend::Anthropic, "anthropic").unwrap();
        // Anthropic catalogue entry's multimodal_model is the same as
        // the text model (Haiku 4.5 is multimodal); the swap is still
        // a no-op-equivalent — but we assert the field source is the
        // catalogue's multimodal_model literal.
        assert_eq!(r.model, "claude-haiku-4-5-20251001");
    }

    #[cfg(feature = "openai-compat")]
    #[test]
    fn prefer_vision_with_no_multimodal_falls_back_to_text_model() {
        // Cerebras catalogue entry: multimodal_model = None.
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Cerebras,
            prefer_vision: true,
            ..AssistantCfg::default()
        };
        let secrets = make_secrets("CEREBRAS_API_KEY");
        let r = resolve_cloud(&cfg, &secrets, &AssistantBackend::Cerebras, "cerebras").unwrap();
        // Cerebras has no multimodal model — must fall back to the
        // text model and emit a warning (warning verified manually;
        // tracing infra differs across test contexts).
        assert_eq!(r.model, default_cloud_model("cerebras"));
        assert!(r.web_search_tool.is_none());
    }

    #[cfg(feature = "anthropic")]
    #[test]
    fn prefer_web_search_surfaces_native_tool_id() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Anthropic,
            prefer_web_search: true,
            ..AssistantCfg::default()
        };
        let secrets = make_secrets("ANTHROPIC_API_KEY");
        let r = resolve_cloud(&cfg, &secrets, &AssistantBackend::Anthropic, "anthropic").unwrap();
        assert_eq!(r.web_search_tool, Some("web_search_20250305"));
    }

    #[cfg(feature = "openai-compat")]
    #[test]
    fn prefer_web_search_is_none_for_groq() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Groq,
            prefer_web_search: true,
            ..AssistantCfg::default()
        };
        let secrets = make_secrets("GROQ_API_KEY");
        let r = resolve_cloud(&cfg, &secrets, &AssistantBackend::Groq, "groq").unwrap();
        // Groq's catalogue entry advertises WebSearchSupport::None —
        // toggle is a no-op there.
        assert!(r.web_search_tool.is_none());
    }
}
