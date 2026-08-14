// SPDX-License-Identifier: GPL-3.0-only
//! Build a concrete [`TextFormatter`] (or `None` when polish is
//! disabled) from `Config` + `Secrets`.

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};
#[cfg(test)]
use fono_core::config::DEFAULT_POLISH_LOCAL_MODEL;
use fono_core::config::{LlmBackend, Polish};
use fono_core::providers::{llm_backend_str, llm_key_env};
use fono_core::Secrets;

use crate::traits::TextFormatter;

/// Resolve `(key, model)` for a cloud polish backend, falling through to
/// the canonical env var when `[polish.cloud]` fields are blank.
fn resolve_cloud(cfg: &Polish, secrets: &Secrets, backend: LlmBackend) -> Result<(String, String)> {
    let provider_name = llm_backend_str(&backend);
    let key_ref = if cfg.cloud.api_key_ref.is_empty() {
        llm_key_env(&backend).to_string()
    } else {
        cfg.cloud.api_key_ref.clone()
    };
    let key = secrets.resolve(&key_ref).ok_or_else(|| {
        anyhow!(
            "{provider_name} LLM API key {key_ref:?} not found in secrets.toml or environment; \
             run `fono keys add {key_ref}` to add it"
        )
    })?;
    let model = if cfg.cloud.model.is_empty() {
        crate::defaults::default_cloud_model(provider_name).to_string()
    } else {
        cfg.cloud.model.clone()
    };
    Ok((key, model))
}

/// Returns `Ok(None)` when `cfg.enabled == false` or `cfg.backend == None`.
/// Otherwise returns the constructed backend or an error explaining why
/// construction failed (missing API key, missing model file, missing feature
/// flag, etc.).
///
/// `polish_models_dir` is the on-disk directory where local LLM GGUF weights
/// live (typically `~/.local/share/fono/models/polish/`). It is only consulted
/// when `cfg.backend == LlmBackend::Local`.
pub fn build_polish(
    cfg: &Polish,
    secrets: &Secrets,
    polish_models_dir: &Path,
) -> Result<Option<Arc<dyn TextFormatter>>> {
    if !cfg.enabled || matches!(cfg.backend, LlmBackend::None) {
        return Ok(None);
    }

    match cfg.backend {
        // Off is handled by the early return above; keeping the arm
        // exhaustive (rather than `unreachable!()`) means a future caller
        // that skips the guard degrades to "no cleanup" instead of panicking
        // mid-dictation.
        LlmBackend::None => return Ok(None),
        LlmBackend::Local => build_local(cfg, polish_models_dir),
        LlmBackend::Network => build_network(cfg, secrets),
        LlmBackend::Cerebras => {
            let (k, m) = resolve_cloud(cfg, secrets, LlmBackend::Cerebras)?;
            build_cerebras(k, m)
        }
        LlmBackend::Groq => {
            let (k, m) = resolve_cloud(cfg, secrets, LlmBackend::Groq)?;
            build_oa_groq(k, m)
        }
        LlmBackend::OpenAI => {
            let (k, m) = resolve_cloud(cfg, secrets, LlmBackend::OpenAI)?;
            build_oa_openai(k, m)
        }
        LlmBackend::OpenRouter => {
            let (k, m) = resolve_cloud(cfg, secrets, LlmBackend::OpenRouter)?;
            build_oa_openrouter(k, m)
        }
        LlmBackend::Anthropic => {
            let (k, m) = resolve_cloud(cfg, secrets, LlmBackend::Anthropic)?;
            build_anthropic(k, m)
        }
        LlmBackend::Gemini => {
            let (k, m) = resolve_cloud(cfg, secrets, LlmBackend::Gemini)?;
            build_oa_gemini(k, m)
        }
    }
    .map(Some)
}

/// Resolve the model name in `cfg.local.model` to a `<polish_models_dir>/<name>.gguf`
/// path. Mirrors the whisper resolver in `fono::models::ensure_whisper`.
#[cfg(any(feature = "llama-local", test))]
fn resolve_local_model_path(cfg: &Polish, polish_models_dir: &Path) -> std::path::PathBuf {
    // Resolve through the registry so a config value copied from the
    // HuggingFace repo name (mixed case, trailing `-GGUF`) maps to the same
    // on-disk stem the downloader writes; unknown names pass through verbatim
    // for manually-placed GGUFs. See `LocalLlmRegistry::resolve_filename_stem`.
    let stem = crate::registry::LocalLlmRegistry::resolve_filename_stem(&cfg.local.model);
    polish_models_dir.join(format!("{stem}.gguf"))
}
/// Where the configured local cleanup model would run if it were loaded right
/// now, as one line naming the device and the numbers behind the answer.
///
/// `None` when cleanup does not load a local model at all, or when the model
/// file is missing — there is nothing to size in either case.
///
/// This is a fresh decision rather than a record of one: the answer depends on
/// how much of the machine is free, so a diagnostic can only say what would
/// happen now. It agrees with a running daemon only to the extent that the
/// machine has not changed since that daemon loaded.
#[cfg(feature = "llama-local")]
#[must_use]
pub fn offload_plan(cfg: &Polish, polish_models_dir: &Path) -> Option<String> {
    use llama_cpp_2::context::params::KvCacheType;
    if !cfg.enabled || !matches!(cfg.backend, LlmBackend::Local) {
        return None;
    }
    let path = resolve_local_model_path(cfg, polish_models_dir);
    if !path.exists() {
        return None;
    }
    // The cache types must match what `LlamaLocal::ensure_loaded` asks for, or
    // the size reported here is not the size that will be allocated.
    Some(
        fono_core::gpu_offload::decide(
            &path,
            cfg.local.context.max(crate::llama_local::MIN_CTX),
            KvCacheType::F16,
            KvCacheType::F16,
        )
        .explanation,
    )
}

#[cfg(not(feature = "llama-local"))]
#[must_use]
pub fn offload_plan(_: &Polish, _: &Path) -> Option<String> {
    None
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
fn build_oa_gemini(key: String, model: String) -> Result<Arc<dyn TextFormatter>> {
    Ok(Arc::new(crate::openai_compat::OpenAiCompat::gemini(key, model)))
}

/// Any OpenAI-compatible chat-completions server, named by URL rather
/// than by engine. Most local servers need no credentials; a bearer
/// token is sent only when `[polish.network].api_key_ref` names a stored
/// secret, which covers authenticated gateways like LiteLLM.
#[cfg(feature = "openai-compat")]
fn build_network(cfg: &Polish, secrets: &Secrets) -> Result<Arc<dyn TextFormatter>> {
    let url = cfg.network.chat_url();
    if url.is_empty() {
        return Err(anyhow!(
            "cleanup is set to `network` but no server URL is configured; set it in \
             `fono settings` or run `fono use llm network --url <URL>`"
        ));
    }
    if cfg.network.model.trim().is_empty() {
        return Err(anyhow!(
            "cleanup is set to `network` but no model id is configured; set it in \
             `fono settings` or run `fono use llm network --url <URL> --model <ID>`"
        ));
    }
    let token = if cfg.network.api_key_ref.is_empty() {
        None
    } else {
        Some(secrets.resolve(&cfg.network.api_key_ref).ok_or_else(|| {
            anyhow!(
                "cleanup server token {:?} not found in secrets.toml or environment; \
                 run `fono keys add {}` to add it",
                cfg.network.api_key_ref,
                cfg.network.api_key_ref
            )
        })?)
    };
    Ok(Arc::new(crate::openai_compat::OpenAiCompat::network(url, cfg.network.model.clone(), token)))
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
fn build_oa_gemini(_: String, _: String) -> Result<Arc<dyn TextFormatter>> {
    Err(anyhow!("Gemini LLM not compiled in (enable the `openai-compat` feature on `fono-polish`)"))
}

#[cfg(not(feature = "openai-compat"))]
fn build_network(_: &Polish, _: &Secrets) -> Result<Arc<dyn TextFormatter>> {
    Err(anyhow!(
        "a self-hosted cleanup server was requested but this binary was built without the \
         `openai-compat` feature on `fono-polish`"
    ))
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

// `LlmBackend::Local` always means the embedded `llama-cpp-2` engine
// running a local GGUF: it never opens a socket. A self-hosted server is
// the separate `LlmBackend::Network` backend (see `build_network`). This
// mirrors the assistant factory (`fono-assistant/src/factory.rs`
// `build_embedded_local`); a missing model file fails loudly with
// `fono models install` guidance rather than silently degrading to no
// cleanup.
#[cfg(feature = "llama-local")]
fn build_local(cfg: &Polish, polish_models_dir: &Path) -> Result<Arc<dyn TextFormatter>> {
    let model_path = resolve_local_model_path(cfg, polish_models_dir);
    if !model_path.exists() {
        return Err(anyhow!(
            "local polish model not found at {model_path:?}; run `fono models install {}` \
             or pick a cloud or network cleanup backend in `fono setup`",
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
    // Glass Cortex keyframe capture — off unless the daemon armed the
    // process-wide latch from `[overlay].brain_capture`.
    let backend = backend.with_brain_tap(fono_core::brain_tap::capture_enabled());
    Ok(Arc::new(backend))
}

#[cfg(not(feature = "llama-local"))]
fn build_local(_: &Polish, _: &Path) -> Result<Arc<dyn TextFormatter>> {
    Err(anyhow!(
        "local polish requested but this binary was built without the \
         `llama-local` feature; rebuild with `cargo build --features llama-local` \
         or pick a cloud or network cleanup backend in `fono setup`"
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
        let cfg = LlmCfg { backend: LlmBackend::None, enabled: true, ..LlmCfg::default() };
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
    // the embedded engine, NOT an HTTP client. With a nonexistent models
    // dir (and regardless of the `llama-local` feature) it must fail
    // loudly rather than silently producing a server-backed formatter.
    // Regression guard for the "local cleanup silently POSTs to
    // localhost:11434" bug.
    #[test]
    fn local_polish_uses_embedded_model_by_default() {
        let cfg = LlmCfg {
            enabled: true,
            backend: LlmBackend::Local,
            local: fono_core::config::PolishLocal {
                model: DEFAULT_POLISH_LOCAL_MODEL.into(),
                ..fono_core::config::PolishLocal::default()
            },
            // A leftover network block must NOT activate a server when
            // the backend is `local`.
            network: fono_core::config::LlmNetwork {
                url: "http://localhost:11434/v1/chat/completions".into(),
                model: DEFAULT_POLISH_LOCAL_MODEL.into(),
                api_key_ref: String::new(),
            },
            ..LlmCfg::default()
        };
        let s = Secrets::default();
        assert!(build_polish(&cfg, &s, Path::new("/this/path/does/not/exist")).is_err());
    }

    // The self-hosted server path is reached only via the explicit
    // `network` backend, and builds without any local model file on disk.
    #[cfg(feature = "openai-compat")]
    #[test]
    fn explicit_network_server_still_builds() {
        let cfg = LlmCfg {
            enabled: true,
            backend: LlmBackend::Network,
            network: fono_core::config::LlmNetwork {
                url: "http://localhost:11434".into(),
                model: "gemma4:12b".into(),
                api_key_ref: String::new(),
            },
            ..LlmCfg::default()
        };
        let s = Secrets::default();
        assert!(build_polish(&cfg, &s, Path::new("/this/path/does/not/exist")).unwrap().is_some());
    }

    /// `build_polish` returns a trait object that is not `Debug`, so
    /// `unwrap_err()` is unavailable; pull the message out by hand.
    fn err_of(cfg: &LlmCfg) -> String {
        match build_polish(cfg, &Secrets::default(), Path::new("/nonexistent")) {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.to_string(),
        }
    }

    // A `network` backend with no URL must say so, rather than POSTing to
    // a hardcoded localhost guess the user never configured.
    #[test]
    fn network_without_url_errors() {
        let cfg = LlmCfg { enabled: true, backend: LlmBackend::Network, ..LlmCfg::default() };
        let err = err_of(&cfg);
        assert!(err.contains("no server URL"), "unexpected error: {err}");
    }

    // Same for a URL with no model id: the server cannot pick one for us.
    #[test]
    fn network_without_model_errors() {
        let cfg = LlmCfg {
            enabled: true,
            backend: LlmBackend::Network,
            network: fono_core::config::LlmNetwork {
                url: "http://localhost:11434".into(),
                ..fono_core::config::LlmNetwork::default()
            },
            ..LlmCfg::default()
        };
        let err = err_of(&cfg);
        assert!(err.contains("no model id"), "unexpected error: {err}");
    }
}
