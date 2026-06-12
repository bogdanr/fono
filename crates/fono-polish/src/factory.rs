// SPDX-License-Identifier: GPL-3.0-only
//! Build a concrete [`TextFormatter`] (or `None` when polish is
//! disabled) from `Config` + `Secrets`.

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};
#[cfg(test)]
use fono_core::config::DEFAULT_POLISH_LOCAL_MODEL;
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

#[cfg(feature = "openai-compat")]
fn local_openai_endpoint(cfg: &Polish) -> String {
    cfg.cloud
        .as_ref()
        .map(|c| c.api_key_ref.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "http://localhost:11434/v1/chat/completions".to_string())
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
    // Ollama / llama.cpp-server don't need an API key; the endpoint is the local URL stored
    // in the cloud.api_key_ref slot when configured. Fall back to the
    // upstream default.
    Ok(Arc::new(crate::openai_compat::OpenAiCompat::ollama(local_openai_endpoint(cfg), model)))
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

// `PolishBackend::Local` always means the embedded `llama-cpp-2` engine
// running a local GGUF — never an Ollama / OpenAI-compatible *server*.
// The server path is reached only via the explicit `PolishBackend::Ollama`
// backend (see `build_oa_ollama`). This mirrors the assistant factory
// (`fono-assistant/src/factory.rs` `build_embedded_local`); a missing
// model file fails loudly with `fono models install` guidance rather than
// silently degrading to no cleanup.
#[cfg(feature = "llama-local")]
fn build_local(cfg: &Polish, polish_models_dir: &Path) -> Result<Arc<dyn TextFormatter>> {
    let model_path = resolve_local_model_path(cfg, polish_models_dir);
    if !model_path.exists() {
        return Err(anyhow!(
            "local polish model not found at {model_path:?}; run `fono models install {}` \
             or pick a cloud/Ollama polish backend in `fono setup`",
            cfg.local.model
        ));
    }
    // Streaming injection runs a consumer task concurrently with the
    // barrier-synchronized llama decode, so reserve one core for it (see
    // `streaming_decode_threads`); the one-shot path has no concurrent consumer
    // and keeps every core via `LlamaLocal::new`.
    let backend = if cfg.stream_injection {
        crate::llama_local::LlamaLocal::with_threads(
            model_path,
            cfg.local.context,
            fono_core::llama_backend::streaming_decode_threads(),
        )
    } else {
        crate::llama_local::LlamaLocal::new(model_path, cfg.local.context)
    };
    Ok(Arc::new(backend))
}

#[cfg(not(feature = "llama-local"))]
fn build_local(_: &Polish, _: &Path) -> Result<Arc<dyn TextFormatter>> {
    Err(anyhow!(
        "local polish requested but this binary was built without the \
         `llama-local` feature; rebuild with `cargo build --features llama-local` \
         or pick a cloud/Ollama polish backend in `fono setup`"
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

    // `backend = local` with the default (Gemma) model must resolve to
    // the embedded engine, NOT an Ollama HTTP client. With a nonexistent
    // models dir (and regardless of the `llama-local` feature) it must
    // fail loudly rather than silently producing a server-backed
    // formatter. Regression guard for the "local cleanup silently POSTs
    // to localhost:11434" bug.
    #[test]
    fn local_polish_uses_embedded_model_by_default() {
        let cfg = LlmCfg {
            enabled: true,
            backend: PolishBackend::Local,
            local: fono_core::config::PolishLocal {
                model: DEFAULT_POLISH_LOCAL_MODEL.into(),
                ..fono_core::config::PolishLocal::default()
            },
            // A stale Ollama cloud block must NOT activate a server when
            // the backend is `local`.
            cloud: Some(fono_core::config::PolishCloud {
                provider: "ollama".into(),
                api_key_ref: "http://localhost:11434/v1/chat/completions".into(),
                model: DEFAULT_POLISH_LOCAL_MODEL.into(),
            }),
            ..LlmCfg::default()
        };
        let s = Secrets::default();
        assert!(build_polish(&cfg, &s, Path::new("/this/path/does/not/exist")).is_err());
    }

    // The Ollama / OpenAI-compatible server path is reached only via the
    // explicit `PolishBackend::Ollama` backend, and builds without any
    // local model file on disk.
    #[cfg(feature = "openai-compat")]
    #[test]
    fn explicit_ollama_server_still_builds() {
        let cfg = LlmCfg {
            enabled: true,
            backend: PolishBackend::Ollama,
            cloud: Some(fono_core::config::PolishCloud {
                provider: "ollama".into(),
                api_key_ref: "http://localhost:11434/v1/chat/completions".into(),
                model: "gemma3:1b".into(),
            }),
            ..LlmCfg::default()
        };
        let s = Secrets::default();
        assert!(build_polish(&cfg, &s, Path::new("/this/path/does/not/exist")).unwrap().is_some());
    }
}
