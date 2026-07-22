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
#[cfg(any(feature = "openai-compat", feature = "anthropic", feature = "realtime"))]
use fono_core::provider_catalog;
#[cfg(any(feature = "openai-compat", feature = "anthropic"))]
use fono_core::provider_catalog::WebSearchSupport;
#[cfg(any(feature = "openai-compat", feature = "anthropic"))]
use fono_core::providers::assistant_key_env;
#[allow(unused_imports)]
use fono_core::Secrets;

use crate::traits::Assistant;
#[cfg(feature = "realtime")]
use crate::traits::RealtimeAssistant;

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

/// A built assistant, in one of two execution shapes:
///
/// * [`AssistantHandle::Staged`] — the classic STT → LLM → TTS pipeline
///   (every backend, and the default for Gemini).
/// * [`AssistantHandle::Realtime`] — a single bidirectional
///   speech-to-speech WebSocket session (selected only when the
///   configured Gemini model equals the catalogue's
///   [`RealtimeProfile`](fono_core::provider_catalog::RealtimeProfile)
///   model). Fixes the staged path's per-sentence voice drift and
///   ~6 s/sentence batch-TTS latency.
pub enum AssistantHandle {
    /// Staged STT → LLM → TTS assistant.
    Staged(Arc<dyn Assistant>),
    /// Realtime speech-to-speech assistant (Gemini Live).
    #[cfg(feature = "realtime")]
    Realtime(Arc<dyn RealtimeAssistant>),
}

/// Resolve the assistant to either a staged or a realtime handle.
///
/// Returns `Ok(None)` when the assistant is disabled or the backend is
/// `none`. The realtime path is chosen only when the backend is Gemini
/// **and** the configured `[assistant.cloud].model` equals the
/// catalogue's realtime profile model; otherwise this delegates to
/// [`build_assistant`] and wraps the result in
/// [`AssistantHandle::Staged`].
pub fn build_assistant_handle(
    cfg: &AssistantCfg,
    secrets: &Secrets,
    assistant_models_dir: &Path,
) -> Result<Option<AssistantHandle>> {
    if !cfg.enabled || matches!(cfg.backend, AssistantBackend::None) {
        return Ok(None);
    }
    #[cfg(feature = "realtime")]
    {
        if let Some(profile) = realtime_selection(cfg) {
            return build_gemini_realtime(cfg, secrets, profile)
                .map(|a| Some(AssistantHandle::Realtime(a)));
        }
    }
    build_assistant(cfg, secrets, assistant_models_dir).map(|opt| opt.map(AssistantHandle::Staged))
}

/// Return the Gemini realtime profile when the configured assistant
/// selects it: backend is Gemini and `[assistant.cloud].model` matches
/// the catalogue's `RealtimeProfile::model`. A blank/default model (the
/// staged text model) yields `None`.
#[cfg(feature = "realtime")]
fn realtime_selection(cfg: &AssistantCfg) -> Option<provider_catalog::RealtimeProfile> {
    if !matches!(cfg.backend, AssistantBackend::Gemini) {
        return None;
    }
    let model = cfg.cloud.as_ref().map(|c| c.model.trim()).filter(|m| !m.is_empty())?;
    let profile = provider_catalog::find("gemini")?.assistant.as_ref()?.realtime?;
    (profile.model == model).then_some(profile)
}

/// Resolve the Gemini API key for the realtime client, honouring an
/// explicit `[assistant.cloud].api_key_ref` and falling back to the
/// canonical `GEMINI_API_KEY`.
#[cfg(feature = "realtime")]
fn resolve_gemini_key(cfg: &AssistantCfg, secrets: &Secrets) -> Result<String> {
    let key_ref = cfg
        .cloud
        .as_ref()
        .map(|c| c.api_key_ref.trim())
        .filter(|r| !r.is_empty())
        .map_or_else(|| "GEMINI_API_KEY".to_string(), ToString::to_string);
    secrets.resolve(&key_ref).ok_or_else(|| {
        anyhow!(
            "gemini realtime assistant API key {key_ref:?} not found in secrets.toml or \
             environment; run `fono keys add {key_ref}` to add it"
        )
    })
}

/// Build the Gemini Live realtime client from the catalogue profile.
/// The reply voice is the catalogue's Gemini TTS `default_voice`
/// (falling back to `Kore`).
#[cfg(feature = "realtime")]
fn build_gemini_realtime(
    cfg: &AssistantCfg,
    secrets: &Secrets,
    profile: provider_catalog::RealtimeProfile,
) -> Result<Arc<dyn RealtimeAssistant>> {
    let key = resolve_gemini_key(cfg, secrets)?;
    let voice = provider_catalog::find("gemini")
        .and_then(|e| e.tts.as_ref())
        .map(|t| t.default_voice)
        .filter(|v| !v.is_empty())
        .unwrap_or("Kore");
    Ok(Arc::new(crate::gemini_live::GeminiLive::new(
        key,
        profile.model,
        profile.ws_url,
        voice,
        profile.input_sample_rate,
        profile.output_sample_rate,
    )))
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
        AssistantBackend::Gemini => build_gemini(cfg, secrets).map(Some),
        AssistantBackend::Ollama => build_ollama(cfg, assistant_models_dir).map(Some),
        AssistantBackend::Anthropic => build_anthropic(cfg, secrets).map(Some),
    }
}

/// Build the *extra* assistant the local LLM server needs when it
/// cannot simply reuse the primary staged assistant.
///
/// Returns `Ok(Some(_))` in two cases:
/// * an explicit `[server.llm].model` override is set — build a staged
///   assistant on the configured backend using that model id; or
/// * the primary `[assistant]` is a *realtime* (speech-to-speech)
///   backend (Gemini Live) that the text chat API cannot expose — build
///   the same provider's default staged **text** model (Gemini Live →
///   `gemini-flash-lite-latest`), reusing the same API key, by cloning
///   `[assistant]` with the cloud model field cleared so
///   [`resolve_cloud`] falls through to the catalogue `text_model`.
///
/// Returns `Ok(None)` when the server should just reuse the primary
/// staged assistant (the common case), or when the assistant is
/// disabled / `none`. Errors propagate the underlying build failure
/// (e.g. a missing API key) so the caller can surface it. See ADR 0036.
pub fn build_server_assistant_override(
    assistant_cfg: &AssistantCfg,
    server_model_override: Option<&str>,
    secrets: &Secrets,
    assistant_models_dir: &Path,
) -> Result<Option<Arc<dyn Assistant>>> {
    if !assistant_cfg.enabled || matches!(assistant_cfg.backend, AssistantBackend::None) {
        return Ok(None);
    }
    // Case 1 — explicit override pins a staged text model regardless of
    // the primary assistant.
    if let Some(model) = server_model_override.map(str::trim).filter(|m| !m.is_empty()) {
        let mut cfg = assistant_cfg.clone();
        if matches!(cfg.backend, AssistantBackend::Ollama) {
            cfg.local.model = model.to_string();
        } else if let Some(c) = cfg.cloud.as_mut() {
            c.model = model.to_string();
        } else {
            cfg.cloud = Some(fono_core::config::AssistantCloud {
                model: model.to_string(),
                ..Default::default()
            });
        }
        cfg.prefer_vision = false;
        return build_assistant(&cfg, secrets, assistant_models_dir);
    }
    // Case 2 — realtime primary → same-provider text sibling. Clearing
    // the cloud model routes the staged builder through
    // `default_cloud_model` (the catalogue `text_model`).
    #[cfg(feature = "realtime")]
    {
        if realtime_selection(assistant_cfg).is_some() {
            let mut cfg = assistant_cfg.clone();
            if let Some(c) = cfg.cloud.as_mut() {
                c.model.clear();
            }
            cfg.prefer_vision = false;
            return build_assistant(&cfg, secrets, assistant_models_dir);
        }
    }
    // Case 3 — a normal staged primary; the server reuses it directly.
    Ok(None)
}

/// The model id the local LLM server should advertise (`/v1/models`,
/// `/api/tags`) for the assistant it serves, accounting for the
/// `[server.llm].model` override and the realtime→text-sibling
/// fallback. Returns an empty string when nothing is served (the caller
/// substitutes a cosmetic default such as `"fono"`). See ADR 0036.
#[must_use]
pub fn server_assistant_model_name(
    assistant_cfg: &AssistantCfg,
    server_model_override: Option<&str>,
) -> String {
    if !assistant_cfg.enabled || matches!(assistant_cfg.backend, AssistantBackend::None) {
        return String::new();
    }
    if let Some(m) = server_model_override.map(str::trim).filter(|m| !m.is_empty()) {
        return m.to_string();
    }
    // Realtime primary → advertise the fallback text sibling's model.
    #[cfg(feature = "realtime")]
    {
        if realtime_selection(assistant_cfg).is_some() {
            let provider = fono_core::providers::assistant_backend_str(&assistant_cfg.backend);
            return provider_catalog::find(provider)
                .and_then(|p| p.assistant.as_ref())
                .map_or_else(String::new, |a| a.text_model.to_string());
        }
    }
    // Staged primary → its own configured model id.
    match assistant_cfg.backend {
        AssistantBackend::Ollama => assistant_cfg.local.model.clone(),
        _ => assistant_cfg.cloud.as_ref().map_or_else(String::new, |c| c.model.clone()),
    }
}

// --- Cloud pass-through proxy (ADR 0036) ---------------------------------

/// Per-provider OpenAI-compatible `/chat/completions` endpoints. Single
/// source of truth shared by the [`crate::openai_compat_chat`] client
/// constructors and the local LLM server's pass-through proxy
/// ([`chat_endpoint`] / [`cloud_chat_upstream`]). Defined here in the
/// always-compiled factory module so the proxy can reference them
/// regardless of the `openai-compat` feature gate.
pub const CEREBRAS_CHAT_ENDPOINT: &str = "https://api.cerebras.ai/v1/chat/completions";
pub const GROQ_CHAT_ENDPOINT: &str = "https://api.groq.com/openai/v1/chat/completions";
pub const OPENAI_CHAT_ENDPOINT: &str = "https://api.openai.com/v1/chat/completions";
pub const OPENROUTER_CHAT_ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
pub const GEMINI_CHAT_ENDPOINT: &str =
    "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions";

/// The OpenAI-compatible `/chat/completions` endpoint for a backend, or
/// `None` for backends that are not OpenAI-shaped: Anthropic (its own
/// Messages API), the local Ollama server (no cloud upstream), and
/// `none`. This is the single decision point for "is this backend
/// proxyable?" used by the local LLM server. See ADR 0036.
#[must_use]
pub fn chat_endpoint(backend: &AssistantBackend) -> Option<&'static str> {
    Some(match backend {
        AssistantBackend::Cerebras => CEREBRAS_CHAT_ENDPOINT,
        AssistantBackend::Groq => GROQ_CHAT_ENDPOINT,
        AssistantBackend::OpenAI => OPENAI_CHAT_ENDPOINT,
        AssistantBackend::OpenRouter => OPENROUTER_CHAT_ENDPOINT,
        AssistantBackend::Gemini => GEMINI_CHAT_ENDPOINT,
        AssistantBackend::Anthropic | AssistantBackend::Ollama | AssistantBackend::None => {
            return None;
        }
    })
}

/// A resolved OpenAI-compatible cloud upstream the local LLM server can
/// forward requests to verbatim — preserving full tool/vision/parameter
/// fidelity that the `Assistant`-trait adapter cannot carry. See
/// [`cloud_chat_upstream`] and ADR 0036.
#[derive(Debug, Clone)]
pub struct CloudUpstream {
    /// Provider `/chat/completions` URL.
    pub chat_url: String,
    /// Provider `/models` URL when derivable (for `/v1/models` passthrough).
    pub models_url: Option<String>,
    /// Resolved API key, injected as `Authorization: Bearer` outbound.
    pub api_key: String,
    /// Default model id, used when the client omits/blanks `model`.
    pub model: String,
}

/// Resolve the OpenAI-compatible cloud upstream the local LLM server
/// should proxy to, or `Ok(None)` when the served backend is **not**
/// proxyable (embedded llama.cpp / Ollama / Anthropic / disabled).
///
/// Reuses the staged clients' key resolution and
/// [`server_assistant_model_name`]'s model selection, so a Gemini Live
/// primary resolves to the `gemini-flash-lite-latest` text sibling on
/// Gemini's OpenAI-compat endpoint (the realtime→text fallback). An
/// explicit `[server.llm].model` override wins. `Err` propagates a
/// missing API key so the daemon can surface it. See ADR 0036.
pub fn cloud_chat_upstream(
    assistant_cfg: &AssistantCfg,
    server_model_override: Option<&str>,
    secrets: &Secrets,
) -> Result<Option<CloudUpstream>> {
    if !assistant_cfg.enabled || matches!(assistant_cfg.backend, AssistantBackend::None) {
        return Ok(None);
    }
    let Some(chat_url) = chat_endpoint(&assistant_cfg.backend) else {
        return Ok(None); // not an OpenAI-compat cloud backend
    };
    #[cfg(feature = "openai-compat")]
    {
        let provider = fono_core::providers::assistant_backend_str(&assistant_cfg.backend);
        // Key resolution mirrors `resolve_cloud`: explicit
        // `[assistant.cloud].api_key_ref`, else the canonical env var.
        let canonical = assistant_key_env(&assistant_cfg.backend);
        let key_ref = assistant_cfg
            .cloud
            .as_ref()
            .map(|c| c.api_key_ref.trim())
            .filter(|r| !r.is_empty())
            .map_or_else(|| canonical.to_string(), ToString::to_string);
        let api_key = secrets.resolve(&key_ref).ok_or_else(|| {
            anyhow!(
                "{provider} assistant API key {key_ref:?} not found in secrets.toml or \
                 environment; run `fono keys add {key_ref}` to add it"
            )
        })?;
        let mut model = server_assistant_model_name(assistant_cfg, server_model_override);
        if model.is_empty() {
            model = default_cloud_model(provider).to_string();
        }
        let models_url =
            chat_url.strip_suffix("/chat/completions").map(|root| format!("{root}/models"));
        Ok(Some(CloudUpstream { chat_url: chat_url.to_string(), models_url, api_key, model }))
    }
    #[cfg(not(feature = "openai-compat"))]
    {
        let _ = (secrets, server_model_override, chat_url);
        Ok(None)
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

#[cfg(feature = "openai-compat")]
fn build_gemini(cfg: &AssistantCfg, secrets: &Secrets) -> Result<Arc<dyn Assistant>> {
    let r = resolve_cloud(cfg, secrets, &AssistantBackend::Gemini, "gemini")?;
    // Gemini's OpenAI-compatible surface does not accept the native
    // `google_search` grounding tool, so we deliberately do not attach
    // `r.web_search_tool` here (ADR 0034); native search is a follow-up
    // on the `generateContent` endpoint.
    Ok(Arc::new(crate::openai_compat_chat::OpenAiCompatChat::gemini(r.key, r.model)))
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

/// True when `[assistant]` resolves to the embedded llama.cpp local model —
/// the `ollama` backend *without* a manual server endpoint. This is exactly
/// the case where [`build_ollama`] loads a local GGUF, so it is the condition
/// under which a caller should ensure the model is downloaded before building
/// the assistant. A manual Ollama/OpenAI-compatible server URL, any cloud
/// backend, or `none` all return `false` (nothing to fetch locally).
#[must_use]
pub fn uses_embedded_local_model(cfg: &AssistantCfg) -> bool {
    matches!(cfg.backend, AssistantBackend::Ollama) && manual_local_server_endpoint(cfg).is_none()
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
    let verbatim = assistant_models_dir.join(format!("{}.gguf", cfg.local.model));
    if verbatim.exists() {
        return verbatim;
    }
    // Fall back to the canonical filename stem the downloader uses for
    // registry models: lowercased with a trailing `-gguf` (the HuggingFace
    // repo suffix) stripped. This lets a config value copied verbatim from the
    // repo name — e.g. `gemma-4-26B-A4B-it-asym-GGUF` — resolve to the file the
    // auto-downloader actually wrote (`gemma-4-26b-a4b-it-asym.gguf`). Kept as a
    // dependency-free string transform so `fono-assistant` need not pull in the
    // `fono-polish` registry crate; the canonical names live in
    // `fono_polish::LocalLlmRegistry`. A manually-placed file matches the
    // verbatim path above, so this only changes behaviour when it is missing.
    let lower = cfg.local.model.to_ascii_lowercase();
    let stem = lower.strip_suffix("-gguf").unwrap_or(&lower);
    assistant_models_dir.join(format!("{stem}.gguf"))
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
    // The assistant streams token deltas concurrently with TTS synthesis (a
    // heavy CPU consumer) on the normal voice-reply path, so reserve one core
    // for that consumer to avoid the per-token barrier stall that throttles a
    // fully-saturated decode (see `streaming_decode_threads`; the same trick
    // recovered F7 dictation from ~13 to ~26 tok/s). Falls back to all cores on
    // ≤2-core hosts.
    Ok(Arc::new(
        crate::llama_local::LlamaLocalAssistant::with_threads(
            model_path,
            cfg.local.context,
            fono_core::llama_backend::streaming_decode_threads(),
        )
        // Glass Cortex keyframe capture — off unless the daemon armed
        // the process-wide latch from `[overlay].brain_capture`.
        .with_brain_tap(fono_core::brain_tap::capture_enabled()),
    ))
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
fn build_gemini(_cfg: &AssistantCfg, _secrets: &Secrets) -> Result<Arc<dyn Assistant>> {
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

    // ── Realtime dispatch (Path B) ────────────────────────────────
    #[cfg(feature = "realtime")]
    fn gemini_realtime_model() -> &'static str {
        provider_catalog::find("gemini")
            .unwrap()
            .assistant
            .as_ref()
            .unwrap()
            .realtime
            .unwrap()
            .model
    }

    #[cfg(feature = "realtime")]
    #[test]
    fn realtime_model_selects_realtime_handle() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Gemini,
            cloud: Some(AssistantCloud {
                provider: "gemini".into(),
                api_key_ref: String::new(),
                model: gemini_realtime_model().into(),
            }),
            ..AssistantCfg::default()
        };
        let mut secrets = Secrets::default();
        secrets.insert("GEMINI_API_KEY", "test-key");
        let handle = build_assistant_handle(&cfg, &secrets, Path::new(".")).unwrap().unwrap();
        assert!(matches!(handle, AssistantHandle::Realtime(_)));
    }

    #[cfg(feature = "realtime")]
    #[test]
    fn realtime_model_without_key_errors_clearly() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Gemini,
            cloud: Some(AssistantCloud {
                provider: "gemini".into(),
                api_key_ref: String::new(),
                model: gemini_realtime_model().into(),
            }),
            ..AssistantCfg::default()
        };
        let err = build_assistant_handle(&cfg, &Secrets::default(), Path::new("."))
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("GEMINI_API_KEY") && err.contains("fono keys add"), "{err}");
    }

    #[cfg(all(feature = "realtime", feature = "openai-compat"))]
    #[test]
    fn default_gemini_model_selects_staged() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Gemini,
            cloud: None,
            ..AssistantCfg::default()
        };
        let mut secrets = Secrets::default();
        secrets.insert("GEMINI_API_KEY", "test-key");
        let handle = build_assistant_handle(&cfg, &secrets, Path::new(".")).unwrap().unwrap();
        assert!(matches!(handle, AssistantHandle::Staged(_)));
    }

    #[cfg(all(feature = "realtime", feature = "openai-compat"))]
    #[test]
    fn non_gemini_backend_ignores_realtime_model() {
        // Even with the realtime model id set, a non-Gemini backend
        // never selects the realtime path.
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::OpenAI,
            cloud: Some(AssistantCloud {
                provider: "openai".into(),
                api_key_ref: String::new(),
                model: gemini_realtime_model().into(),
            }),
            ..AssistantCfg::default()
        };
        let mut secrets = Secrets::default();
        secrets.insert("OPENAI_API_KEY", "sk-test");
        let handle = build_assistant_handle(&cfg, &secrets, Path::new(".")).unwrap().unwrap();
        assert!(matches!(handle, AssistantHandle::Staged(_)));
    }

    // ── ADR 0036: LLM-server assistant resolution ─────────────────

    #[test]
    fn server_model_name_disabled_is_empty() {
        let cfg = AssistantCfg { enabled: false, ..AssistantCfg::default() };
        assert!(server_assistant_model_name(&cfg, None).is_empty());
    }

    #[test]
    fn server_model_name_override_wins() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Gemini,
            cloud: Some(AssistantCloud {
                provider: "gemini".into(),
                api_key_ref: String::new(),
                model: "some-other-model".into(),
            }),
            ..AssistantCfg::default()
        };
        assert_eq!(server_assistant_model_name(&cfg, Some("pinned-model")), "pinned-model");
        // Blank / whitespace override is ignored.
        assert_eq!(server_assistant_model_name(&cfg, Some("   ")), "some-other-model");
    }

    #[cfg(feature = "realtime")]
    #[test]
    fn server_model_name_realtime_falls_back_to_text_sibling() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Gemini,
            cloud: Some(AssistantCloud {
                provider: "gemini".into(),
                api_key_ref: String::new(),
                model: gemini_realtime_model().into(),
            }),
            ..AssistantCfg::default()
        };
        // Realtime primary → advertise the catalogue text model, not the
        // Live model the text API can't serve.
        let served = server_assistant_model_name(&cfg, None);
        assert_ne!(served, gemini_realtime_model());
        assert_eq!(served, "gemini-flash-lite-latest");
    }

    #[cfg(all(feature = "realtime", feature = "openai-compat"))]
    #[test]
    fn build_server_assistant_realtime_builds_text_sibling() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Gemini,
            cloud: Some(AssistantCloud {
                provider: "gemini".into(),
                api_key_ref: String::new(),
                model: gemini_realtime_model().into(),
            }),
            ..AssistantCfg::default()
        };
        let mut secrets = Secrets::default();
        secrets.insert("GEMINI_API_KEY", "test-key");
        // Realtime primary → the server gets a staged text sibling built
        // from the same key.
        let built = build_server_assistant_override(&cfg, None, &secrets, Path::new(".")).unwrap();
        assert!(built.is_some(), "realtime primary should yield a text-sibling fallback");
    }

    #[cfg(feature = "openai-compat")]
    #[test]
    fn build_server_assistant_staged_primary_reuses_it() {
        // A normal staged primary needs no extra assistant — the server
        // reuses the primary directly, so this returns None.
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::OpenAI,
            cloud: None,
            ..AssistantCfg::default()
        };
        let mut secrets = Secrets::default();
        secrets.insert("OPENAI_API_KEY", "sk-test");
        let built = build_server_assistant_override(&cfg, None, &secrets, Path::new(".")).unwrap();
        assert!(built.is_none(), "staged primary should reuse the primary (None)");
    }

    #[cfg(feature = "openai-compat")]
    #[test]
    fn build_server_assistant_override_builds_pinned_model() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::OpenAI,
            cloud: None,
            ..AssistantCfg::default()
        };
        let mut secrets = Secrets::default();
        secrets.insert("OPENAI_API_KEY", "sk-test");
        let built =
            build_server_assistant_override(&cfg, Some("gpt-pinned"), &secrets, Path::new("."))
                .unwrap();
        assert!(built.is_some(), "an explicit override should build a staged assistant");
    }

    #[test]
    fn chat_endpoint_marks_proxyable_backends() {
        // OpenAI-compat cloud backends are proxyable (Some).
        assert!(chat_endpoint(&AssistantBackend::OpenAI).is_some());
        assert!(chat_endpoint(&AssistantBackend::Gemini).is_some());
        assert!(chat_endpoint(&AssistantBackend::Groq).is_some());
        assert!(chat_endpoint(&AssistantBackend::Cerebras).is_some());
        assert!(chat_endpoint(&AssistantBackend::OpenRouter).is_some());
        // Non-OpenAI-shaped / local / disabled backends are not.
        assert!(chat_endpoint(&AssistantBackend::Anthropic).is_none());
        assert!(chat_endpoint(&AssistantBackend::Ollama).is_none());
        assert!(chat_endpoint(&AssistantBackend::None).is_none());
        // Gemini uses its OpenAI-compat layer, not native generateContent.
        assert!(chat_endpoint(&AssistantBackend::Gemini).unwrap().contains("/openai/"));
    }

    #[cfg(feature = "openai-compat")]
    #[test]
    fn cloud_upstream_resolves_for_openai_compat_backend() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::OpenAI,
            cloud: None,
            ..AssistantCfg::default()
        };
        let mut secrets = Secrets::default();
        secrets.insert("OPENAI_API_KEY", "sk-test");
        let up = cloud_chat_upstream(&cfg, None, &secrets).unwrap();
        let up = up.expect("OpenAI backend should be proxyable");
        assert_eq!(up.chat_url, OPENAI_CHAT_ENDPOINT);
        assert_eq!(up.api_key, "sk-test");
        assert_eq!(up.models_url.as_deref(), Some("https://api.openai.com/v1/models"));
        assert!(!up.model.is_empty(), "a default model should be resolved");
    }

    #[cfg(feature = "openai-compat")]
    #[test]
    fn cloud_upstream_honours_explicit_override_model() {
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::OpenAI,
            cloud: None,
            ..AssistantCfg::default()
        };
        let mut secrets = Secrets::default();
        secrets.insert("OPENAI_API_KEY", "sk-test");
        let up = cloud_chat_upstream(&cfg, Some("gpt-pinned"), &secrets).unwrap().unwrap();
        assert_eq!(up.model, "gpt-pinned");
    }

    #[cfg(feature = "openai-compat")]
    #[test]
    fn cloud_upstream_gemini_live_falls_back_to_text_sibling() {
        // A realtime Gemini Live primary is still proxyable — it resolves
        // to the same-provider text sibling on Gemini's compat endpoint.
        let cfg = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Gemini,
            cloud: Some(AssistantCloud {
                provider: "gemini".into(),
                api_key_ref: String::new(),
                model: gemini_realtime_model().into(),
            }),
            ..AssistantCfg::default()
        };
        let mut secrets = Secrets::default();
        secrets.insert("GEMINI_API_KEY", "test-key");
        let up = cloud_chat_upstream(&cfg, None, &secrets).unwrap().unwrap();
        assert_eq!(up.chat_url, GEMINI_CHAT_ENDPOINT);
        assert_ne!(up.model, gemini_realtime_model(), "should not proxy the Live model");
        assert!(!up.model.is_empty());
    }

    #[test]
    fn cloud_upstream_none_for_anthropic_and_disabled() {
        let secrets = Secrets::default();
        // Anthropic is not OpenAI-shaped → not proxyable.
        let anthropic = AssistantCfg {
            enabled: true,
            backend: AssistantBackend::Anthropic,
            ..AssistantCfg::default()
        };
        assert!(cloud_chat_upstream(&anthropic, None, &secrets).unwrap().is_none());
        // Disabled assistant → not proxyable.
        let disabled = AssistantCfg {
            enabled: false,
            backend: AssistantBackend::OpenAI,
            ..AssistantCfg::default()
        };
        assert!(cloud_chat_upstream(&disabled, None, &secrets).unwrap().is_none());
    }
}
