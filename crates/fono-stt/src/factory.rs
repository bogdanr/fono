// SPDX-License-Identifier: GPL-3.0-only
//! Build a concrete [`SpeechToText`] from `Config` + `Secrets`.
//!
//! Cloud branches are gated by feature flags; missing features produce a
//! clear error so the daemon can stay running in a "degraded" state and
//! the user can fix the config without recompiling the world.

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};
#[cfg(feature = "wyoming")]
use fono_core::config::SttWyoming;
use fono_core::config::{General, Stt, SttBackend, SttCloud};
use fono_core::locale::detect_os_languages;
use fono_core::providers::stt_key_env;
use fono_core::Secrets;

use crate::lang_cache::LanguageCache;
use crate::traits::SpeechToText;

/// Best-effort: seed the global language cache for `backend_key`
/// from the OS locale, but only if the detected code is in
/// `allow_list`. No-ops once the cache is populated. Plan v3 task 3.
fn bootstrap_language_cache(allow_list: &[String], backend_key: &'static str) {
    if allow_list.len() < 2 {
        return; // single-language / auto: no peer set to disambiguate
    }
    let cache = LanguageCache::global();
    let allow_lc: Vec<String> = allow_list.iter().map(|c| c.trim().to_ascii_lowercase()).collect();
    for code in detect_os_languages() {
        if allow_lc.iter().any(|c| c == &code) {
            cache.seed_if_empty(backend_key, code);
            return;
        }
    }
}

/// Resolve the effective `(api_key_ref, model)` pair for a cloud STT
/// backend. When `cfg.cloud` is missing, fall through to the canonical
/// env-var name from [`fono_core::providers`] and the default model
/// from [`crate::defaults`]. Returns the (key, model) tuple ready for
/// the backend constructor.
fn resolve_cloud(
    cfg: &Stt,
    secrets: &Secrets,
    backend: &SttBackend,
    provider_name: &str,
) -> Result<(String, String)> {
    let canonical = stt_key_env(backend);
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
            "{provider_name} STT API key {key_ref:?} not found in secrets.toml or environment; \
             run `fono keys add {key_ref}` to add it"
        )
    })?;
    let model = model_override
        .unwrap_or_else(|| crate::defaults::default_cloud_model(provider_name).to_string());
    Ok((key, model))
}

/// Helper used by integration tests / docs to surface the SttCloud
/// equivalent of the resolution above without performing it.
#[must_use]
pub fn synthetic_cloud(backend: &SttBackend, provider_name: &str) -> SttCloud {
    SttCloud {
        provider: provider_name.to_string(),
        api_key_ref: stt_key_env(backend).to_string(),
        model: crate::defaults::default_cloud_model(provider_name).to_string(),
    }
}

/// Construct an STT backend matching `cfg`. The returned [`Arc`] is
/// `Send + Sync` so it can be shared across the orchestrator's tokio tasks.
///
/// `general` carries the language allow-list (`general.languages`) and
/// the cloud-mismatch knobs that every STT backend honours uniformly.
/// `whisper_models_dir` is consulted only for [`SttBackend::Local`].
pub fn build_stt(
    cfg: &Stt,
    general: &General,
    secrets: &Secrets,
    whisper_models_dir: &Path,
) -> Result<Arc<dyn SpeechToText>> {
    let languages = effective_languages(cfg, general);
    let prompts = cfg.prompts.clone();
    let cloud_rerun = general.cloud_rerun_on_language_mismatch;
    match &cfg.backend {
        SttBackend::Local => build_local(cfg, whisper_models_dir, languages, prompts),
        SttBackend::Groq => build_groq(cfg, secrets, languages, prompts, cloud_rerun),
        SttBackend::OpenAI => build_openai(cfg, secrets, languages, prompts, cloud_rerun),
        SttBackend::OpenRouter => build_openrouter(cfg, secrets, languages, prompts, cloud_rerun),
        SttBackend::Cartesia => build_cartesia(cfg, secrets, languages, prompts, cloud_rerun),
        SttBackend::Deepgram => build_deepgram(cfg, secrets, languages, prompts, cloud_rerun),
        SttBackend::Wyoming => build_wyoming(cfg, secrets, languages),
        other => Err(anyhow!(
            "STT backend {other:?} is not yet implemented in this build; \
             pick `groq`, `openai`, `openrouter`, `cartesia`, `deepgram`, `wyoming`, or `local` \
             (rebuild with `--features whisper-local` for `local`)"
        )),
    }
}

/// `[stt.local].languages` overrides `[general].languages` when set.
fn effective_languages(cfg: &Stt, general: &General) -> Vec<String> {
    if cfg.local.languages.is_empty() {
        general.languages.clone()
    } else {
        cfg.local.languages.clone()
    }
}

#[cfg(feature = "whisper-local")]
fn build_local(
    cfg: &Stt,
    dir: &Path,
    languages: Vec<String>,
    mut prompts: std::collections::HashMap<String, String>,
) -> Result<Arc<dyn SpeechToText>> {
    let model = &cfg.local.model;
    let path = resolve_local_model_path(dir, model, &cfg.local.quantization)?;
    // Latency plan L18 — `0` means auto-detect physical cores so we
    // don't oversubscribe SMT siblings. `with_threads` clamps the
    // value into the i32 range whisper-rs expects.
    let threads: i32 = match cfg.local.threads {
        0 => i32::try_from(detect_physical_cores()).unwrap_or(4),
        n => i32::try_from(n).unwrap_or(i32::MAX),
    };
    // English-only model (`*.en` suffix) defaults to a built-in
    // English prompt that biases Whisper away from training-corpus
    // closers ("Thank you for watching") without affecting accent
    // or vocabulary. Multilingual models stay unprompted unless the
    // user configured `[stt.prompts]` explicitly, since a wrong-
    // language prompt can mislead the language classifier.
    if is_english_only_model(model) {
        prompts.entry("en".to_string()).or_insert_with(|| {
            "Professional dictation. Output exactly what the speaker says with proper \
                 punctuation and capitalization."
                .to_string()
        });
    }
    Ok(Arc::new(
        crate::whisper_local::WhisperLocal::with_threads(path, threads)
            .with_languages(languages)
            .with_prompts(prompts),
    ))
}

/// Resolve `<dir>/<ggml-name-quant.bin>` for the given user-facing
/// model name and `[stt.local].quantization` preference. Returns a
/// clear `models install` hint when the file is missing.
#[cfg(feature = "whisper-local")]
fn resolve_local_model_path(
    dir: &Path,
    model: &str,
    quantization: &str,
) -> Result<std::path::PathBuf> {
    use crate::registry::{ModelRegistry, QuantizationPref};
    let info = ModelRegistry::get(model).ok_or_else(|| {
        anyhow!(
            "local whisper model {model:?} is not in the registry — \
             run `fono models list` to see available names"
        )
    })?;
    let pref = QuantizationPref::parse(quantization).ok_or_else(|| {
        anyhow!(
            "invalid `[stt.local].quantization = {quantization:?}` — \
             expected `auto`, `fp16`, `q5_1`, or `q8_0`"
        )
    })?;
    let quant = ModelRegistry::resolve_quantization(info, pref).map_err(anyhow::Error::msg)?;
    let path = dir.join(ModelRegistry::filename(info.name, quant));
    if !path.exists() {
        return Err(anyhow!(
            "local whisper model {model:?} ({quant}) not found at {} — \
             run `fono models install {model}`",
            path.display()
        ));
    }
    Ok(path)
}

/// English-only Whisper variant detection (e.g. `tiny.en`, `small.en-q5_1`).
#[cfg(feature = "whisper-local")]
fn is_english_only_model(model: &str) -> bool {
    model.split(['-', '.']).any(|part| part.eq_ignore_ascii_case("en"))
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
fn build_local(
    _cfg: &Stt,
    _dir: &Path,
    _languages: Vec<String>,
    _prompts: std::collections::HashMap<String, String>,
) -> Result<Arc<dyn SpeechToText>> {
    Err(anyhow!(
        "local STT requested but this binary was built without the \
         `whisper-local` feature; rebuild with `cargo build --features whisper-local` \
         or pick a cloud STT backend in `fono setup`"
    ))
}

#[cfg(feature = "groq")]
fn build_groq(
    cfg: &Stt,
    secrets: &Secrets,
    languages: Vec<String>,
    prompts: std::collections::HashMap<String, String>,
    cloud_rerun: bool,
) -> Result<Arc<dyn SpeechToText>> {
    let (key, model) = resolve_cloud(cfg, secrets, &SttBackend::Groq, "groq")?;
    bootstrap_language_cache(&languages, crate::groq::BACKEND_KEY);
    Ok(Arc::new(
        crate::groq::GroqStt::with_model(key, model)
            .with_languages(languages)
            .with_prompts(prompts)
            .with_cloud_rerun_on_mismatch(cloud_rerun),
    ))
}

#[cfg(not(feature = "groq"))]
fn build_groq(
    _: &Stt,
    _: &Secrets,
    _: Vec<String>,
    _: std::collections::HashMap<String, String>,
    _: bool,
) -> Result<Arc<dyn SpeechToText>> {
    Err(anyhow!("Groq STT not compiled in (enable the `groq` feature on `fono-stt`)"))
}

#[cfg(feature = "openai")]
fn build_openai(
    cfg: &Stt,
    secrets: &Secrets,
    languages: Vec<String>,
    prompts: std::collections::HashMap<String, String>,
    cloud_rerun: bool,
) -> Result<Arc<dyn SpeechToText>> {
    let (key, model) = resolve_cloud(cfg, secrets, &SttBackend::OpenAI, "openai")?;
    bootstrap_language_cache(&languages, crate::openai::BACKEND_KEY);
    Ok(Arc::new(
        crate::openai::OpenAiStt::with_model(key, model)
            .with_languages(languages)
            .with_prompts(prompts)
            .with_cloud_rerun_on_mismatch(cloud_rerun),
    ))
}

#[cfg(not(feature = "openai"))]
fn build_openai(
    _: &Stt,
    _: &Secrets,
    _: Vec<String>,
    _: std::collections::HashMap<String, String>,
    _: bool,
) -> Result<Arc<dyn SpeechToText>> {
    Err(anyhow!("OpenAI STT not compiled in (enable the `openai` feature on `fono-stt`)"))
}

#[cfg(feature = "openrouter")]
fn build_openrouter(
    cfg: &Stt,
    secrets: &Secrets,
    languages: Vec<String>,
    prompts: std::collections::HashMap<String, String>,
    cloud_rerun: bool,
) -> Result<Arc<dyn SpeechToText>> {
    let (key, model) = resolve_cloud(cfg, secrets, &SttBackend::OpenRouter, "openrouter")?;
    bootstrap_language_cache(&languages, crate::openrouter::BACKEND_KEY);
    Ok(Arc::new(
        crate::openrouter::OpenRouterStt::with_model(key, model)
            .with_languages(languages)
            .with_prompts(prompts)
            .with_cloud_rerun_on_mismatch(cloud_rerun),
    ))
}

#[cfg(not(feature = "openrouter"))]
fn build_openrouter(
    _: &Stt,
    _: &Secrets,
    _: Vec<String>,
    _: std::collections::HashMap<String, String>,
    _: bool,
) -> Result<Arc<dyn SpeechToText>> {
    Err(anyhow!("OpenRouter STT not compiled in (enable the `openrouter` feature on `fono-stt`)"))
}

#[cfg(feature = "cartesia")]
fn build_cartesia(
    cfg: &Stt,
    secrets: &Secrets,
    languages: Vec<String>,
    prompts: std::collections::HashMap<String, String>,
    cloud_rerun: bool,
) -> Result<Arc<dyn SpeechToText>> {
    let (key, model) = resolve_cloud(cfg, secrets, &SttBackend::Cartesia, "cartesia")?;
    bootstrap_language_cache(&languages, crate::cartesia::BACKEND_KEY);
    Ok(Arc::new(
        crate::cartesia::CartesiaStt::with_model(key, model)
            .with_languages(languages)
            .with_prompts(prompts)
            .with_cloud_rerun_on_mismatch(cloud_rerun),
    ))
}

#[cfg(not(feature = "cartesia"))]
fn build_cartesia(
    _: &Stt,
    _: &Secrets,
    _: Vec<String>,
    _: std::collections::HashMap<String, String>,
    _: bool,
) -> Result<Arc<dyn SpeechToText>> {
    Err(anyhow!("Cartesia STT not compiled in (enable the `cartesia` feature on `fono-stt`)"))
}

#[cfg(feature = "deepgram")]
fn build_deepgram(
    cfg: &Stt,
    secrets: &Secrets,
    languages: Vec<String>,
    prompts: std::collections::HashMap<String, String>,
    cloud_rerun: bool,
) -> Result<Arc<dyn SpeechToText>> {
    let (key, model) = resolve_cloud(cfg, secrets, &SttBackend::Deepgram, "deepgram")?;
    bootstrap_language_cache(&languages, crate::deepgram::BACKEND_KEY);
    Ok(Arc::new(
        crate::deepgram::DeepgramStt::with_model(key, model)
            .with_languages(languages)
            .with_prompts(prompts)
            .with_cloud_rerun_on_mismatch(cloud_rerun),
    ))
}

#[cfg(not(feature = "deepgram"))]
fn build_deepgram(
    _: &Stt,
    _: &Secrets,
    _: Vec<String>,
    _: std::collections::HashMap<String, String>,
    _: bool,
) -> Result<Arc<dyn SpeechToText>> {
    Err(anyhow!("Deepgram STT not compiled in (enable the `deepgram` feature on `fono-stt`)"))
}

#[cfg(feature = "wyoming")]
fn build_wyoming(
    cfg: &Stt,
    secrets: &Secrets,
    languages: Vec<String>,
) -> Result<Arc<dyn SpeechToText>> {
    let wy: &SttWyoming = cfg.wyoming.as_ref().ok_or_else(|| {
        anyhow!(
            "wyoming STT selected but `[stt.wyoming]` is missing — run \
             `fono use stt wyoming --uri tcp://host:10300` or pick a \
             discovered peer from the tray menu"
        )
    })?;
    if wy.uri.trim().is_empty() {
        return Err(anyhow!(
            "wyoming STT selected but `[stt.wyoming].uri` is empty — set it to a \
             URL like `tcp://kitchen-pc.local:10300`"
        ));
    }
    let token = if wy.auth_token_ref.trim().is_empty() {
        None
    } else {
        secrets.resolve(&wy.auth_token_ref)
    };
    let mut backend = crate::wyoming::WyomingStt::from_uri(&wy.uri)?
        .with_languages(languages)
        .with_auth_token(token);
    if !wy.model.trim().is_empty() {
        backend = backend.with_model(wy.model.clone());
    }
    Ok(Arc::new(backend))
}

#[cfg(not(feature = "wyoming"))]
fn build_wyoming(_: &Stt, _: &Secrets, _: Vec<String>) -> Result<Arc<dyn SpeechToText>> {
    Err(anyhow!("Wyoming STT not compiled in (enable the `wyoming` feature on `fono-stt`)"))
}

/// Streaming-STT factory. Mirrors [`build_stt`] but returns
/// `Arc<dyn StreamingStt>`. Slice A only implements the local
/// (`whisper-rs`) streaming path; cloud backends return `Ok(None)`
/// with a `warn!` so the caller (the daemon) can fall back to the
/// batch path gracefully. Slice B1 / Thread B adds Groq via a
/// pseudo-stream (re-POST trailing N seconds every 700 ms).
///
/// Whether streaming runs at all is gated on `live_preview` — the
/// resolved boolean from `Config::live_preview()` (currently:
/// `[overlay].style == "transcript"`). The four passive
/// visualisation styles keep the daemon on the batch path.
#[cfg(feature = "streaming")]
pub fn build_streaming_stt(
    cfg: &Stt,
    general: &General,
    live_preview: bool,
    interactive: &fono_core::config::Interactive,
    secrets: &Secrets,
    whisper_models_dir: &Path,
) -> Result<Option<Arc<dyn crate::streaming::StreamingStt>>> {
    let _ = secrets;
    let cloud_rerun = general.cloud_rerun_on_language_mismatch;
    let cloud_streaming = live_preview;
    let cadence = interactive.preview_cadence();
    let prompts = cfg.prompts.clone();
    match &cfg.backend {
        SttBackend::Local => {
            let languages = effective_languages(cfg, general);
            build_local_streaming(cfg, whisper_models_dir, languages, prompts).map(Some)
        }
        SttBackend::Groq if cloud_streaming => {
            let languages = effective_languages(cfg, general);
            build_groq_streaming(cfg, secrets, languages, prompts, cloud_rerun, cadence).map(Some)
        }
        SttBackend::Deepgram if cloud_streaming => {
            let languages = effective_languages(cfg, general);
            build_deepgram_streaming(cfg, secrets, languages, cloud_rerun, cadence).map(Some)
        }
        other => {
            // When live preview is off the user explicitly asked for
            // batch mode; calling that "a fallback" is misleading.
            // Only warn when streaming was requested and the backend
            // cannot deliver.
            if live_preview {
                let label = fono_core::providers::stt_backend_str(other);
                tracing::warn!(
                    "streaming STT not yet supported for backend {label}; \
                     live dictation will fall back to batch"
                );
            }
            Ok(None)
        }
    }
}

#[cfg(all(feature = "streaming", feature = "groq"))]
fn build_groq_streaming(
    cfg: &Stt,
    secrets: &Secrets,
    languages: Vec<String>,
    prompts: std::collections::HashMap<String, String>,
    cloud_rerun: bool,
    cadence: fono_core::config::PreviewCadence,
) -> Result<Arc<dyn crate::streaming::StreamingStt>> {
    let (key, model) = resolve_cloud(cfg, secrets, &SttBackend::Groq, "groq")?;
    bootstrap_language_cache(&languages, crate::groq::BACKEND_KEY);
    let cadence_opt = match cadence {
        fono_core::config::PreviewCadence::Interval(ms) => {
            Some(std::time::Duration::from_millis(u64::from(ms)))
        }
        fono_core::config::PreviewCadence::DisabledFinalizeOnly => None,
    };
    Ok(Arc::new(
        crate::groq_streaming::GroqStreaming::new(key, model)
            .with_languages(languages)
            .with_prompts(prompts)
            .with_cloud_rerun_on_mismatch(cloud_rerun)
            .with_preview_cadence(cadence_opt),
    ))
}

#[cfg(all(feature = "streaming", not(feature = "groq")))]
fn build_groq_streaming(
    _cfg: &Stt,
    _secrets: &Secrets,
    _languages: Vec<String>,
    _prompts: std::collections::HashMap<String, String>,
    _cloud_rerun: bool,
    _cadence: fono_core::config::PreviewCadence,
) -> Result<Arc<dyn crate::streaming::StreamingStt>> {
    Err(anyhow!(
        "Groq streaming STT requested but this binary was built without \
         the `groq` feature on `fono-stt`"
    ))
}

#[cfg(all(feature = "streaming", feature = "deepgram"))]
fn build_deepgram_streaming(
    cfg: &Stt,
    secrets: &Secrets,
    languages: Vec<String>,
    cloud_rerun: bool,
    cadence: fono_core::config::PreviewCadence,
) -> Result<Arc<dyn crate::streaming::StreamingStt>> {
    let (key, model) = resolve_cloud(cfg, secrets, &SttBackend::Deepgram, "deepgram")?;
    bootstrap_language_cache(&languages, crate::deepgram::BACKEND_KEY);
    let cadence_opt = match cadence {
        fono_core::config::PreviewCadence::Interval(ms) => {
            Some(std::time::Duration::from_millis(u64::from(ms)))
        }
        fono_core::config::PreviewCadence::DisabledFinalizeOnly => None,
    };
    Ok(Arc::new(
        crate::deepgram_streaming::DeepgramStreaming::new(key, model)
            .with_languages(languages)
            .with_cloud_rerun_on_mismatch(cloud_rerun)
            .with_preview_cadence(cadence_opt),
    ))
}

#[cfg(all(feature = "streaming", not(feature = "deepgram")))]
fn build_deepgram_streaming(
    _cfg: &Stt,
    _secrets: &Secrets,
    _languages: Vec<String>,
    _cloud_rerun: bool,
    _cadence: fono_core::config::PreviewCadence,
) -> Result<Arc<dyn crate::streaming::StreamingStt>> {
    Err(anyhow!(
        "Deepgram streaming STT requested but this binary was built without \
         the `deepgram` feature on `fono-stt`"
    ))
}

#[cfg(all(feature = "streaming", feature = "whisper-local"))]
fn build_local_streaming(
    cfg: &Stt,
    dir: &Path,
    languages: Vec<String>,
    mut prompts: std::collections::HashMap<String, String>,
) -> Result<Arc<dyn crate::streaming::StreamingStt>> {
    let model = &cfg.local.model;
    let path = resolve_local_model_path(dir, model, &cfg.local.quantization)?;
    let threads: i32 = match cfg.local.threads {
        0 => i32::try_from(detect_physical_cores()).unwrap_or(4),
        n => i32::try_from(n).unwrap_or(i32::MAX),
    };
    if is_english_only_model(model) {
        prompts.entry("en".to_string()).or_insert_with(|| {
            "Professional dictation. Output exactly what the speaker says with proper \
                 punctuation and capitalization."
                .to_string()
        });
    }
    Ok(Arc::new(
        crate::whisper_local::WhisperLocal::with_threads(path, threads)
            .with_languages(languages)
            .with_prompts(prompts),
    ))
}

#[cfg(all(feature = "streaming", not(feature = "whisper-local")))]
fn build_local_streaming(
    _cfg: &Stt,
    _dir: &Path,
    _languages: Vec<String>,
    _prompts: std::collections::HashMap<String, String>,
) -> Result<Arc<dyn crate::streaming::StreamingStt>> {
    Err(anyhow!(
        "local streaming STT requested but this binary was built without \
         the `whisper-local` feature"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fono_core::config::{Stt as SttCfg, SttBackend};

    #[cfg(feature = "groq")]
    #[test]
    fn cloud_optional_with_env_key() {
        let cfg = SttCfg { backend: SttBackend::Groq, cloud: None, ..SttCfg::default() };
        let general = fono_core::config::General::default();
        let mut secrets = Secrets::default();
        secrets.insert("GROQ_API_KEY", "gsk-test");
        let dir = std::path::PathBuf::from("/tmp");
        // Should succeed: factory falls through to GROQ_API_KEY.
        assert!(build_stt(&cfg, &general, &secrets, &dir).is_ok());
    }

    #[cfg(feature = "groq")]
    #[test]
    fn missing_key_yields_clear_error() {
        let cfg = SttCfg { backend: SttBackend::Groq, cloud: None, ..SttCfg::default() };
        let general = fono_core::config::General::default();
        let secrets = Secrets::default();
        let dir = std::path::PathBuf::from("/tmp");
        let err = build_stt(&cfg, &general, &secrets, &dir).err().unwrap().to_string();
        assert!(
            err.contains("GROQ_API_KEY") && err.contains("fono keys add"),
            "error message should mention env var and remediation: {err}"
        );
    }

    #[cfg(feature = "cartesia")]
    #[test]
    fn cartesia_cloud_optional_with_env_key() {
        // Same shape as the Groq test: omitting `[stt.cloud]` must
        // fall through to `CARTESIA_API_KEY` in secrets without
        // surfacing a "not yet implemented" error (the bug that
        // shipped before this slice landed).
        let cfg = SttCfg { backend: SttBackend::Cartesia, cloud: None, ..SttCfg::default() };
        let general = fono_core::config::General::default();
        let mut secrets = Secrets::default();
        secrets.insert("CARTESIA_API_KEY", "cart-test");
        let dir = std::path::PathBuf::from("/tmp");
        let stt = build_stt(&cfg, &general, &secrets, &dir).expect("cartesia factory ok");
        assert_eq!(stt.name(), "cartesia");
    }

    #[cfg(feature = "cartesia")]
    #[test]
    fn cartesia_missing_key_yields_clear_error() {
        let cfg = SttCfg { backend: SttBackend::Cartesia, cloud: None, ..SttCfg::default() };
        let general = fono_core::config::General::default();
        let secrets = Secrets::default();
        let dir = std::path::PathBuf::from("/tmp");
        let err = build_stt(&cfg, &general, &secrets, &dir).err().unwrap().to_string();
        assert!(
            err.contains("CARTESIA_API_KEY") && err.contains("fono keys add"),
            "error should mention env var and remediation: {err}"
        );
    }

    #[cfg(feature = "deepgram")]
    #[test]
    fn deepgram_cloud_optional_with_env_key() {
        let cfg = SttCfg { backend: SttBackend::Deepgram, cloud: None, ..SttCfg::default() };
        let general = fono_core::config::General::default();
        let mut secrets = Secrets::default();
        secrets.insert("DEEPGRAM_API_KEY", "dg-test");
        let dir = std::path::PathBuf::from("/tmp");
        let stt = build_stt(&cfg, &general, &secrets, &dir).expect("deepgram factory ok");
        assert_eq!(stt.name(), "deepgram");
    }

    #[cfg(feature = "deepgram")]
    #[test]
    fn deepgram_missing_key_yields_clear_error() {
        let cfg = SttCfg { backend: SttBackend::Deepgram, cloud: None, ..SttCfg::default() };
        let general = fono_core::config::General::default();
        let secrets = Secrets::default();
        let dir = std::path::PathBuf::from("/tmp");
        let err = build_stt(&cfg, &general, &secrets, &dir).err().unwrap().to_string();
        assert!(
            err.contains("DEEPGRAM_API_KEY") && err.contains("fono keys add"),
            "error should mention env var and remediation: {err}"
        );
    }

    #[cfg(all(feature = "streaming", feature = "groq"))]
    #[test]
    fn build_streaming_stt_returns_none_when_live_preview_off() {
        // Groq IS a streaming backend (Slice B1 Thread B), but the
        // factory only constructs a streaming client when
        // `live_preview` is on. With it off the daemon must fall
        // back to the batch path.
        let cfg = SttCfg { backend: SttBackend::Groq, cloud: None, ..SttCfg::default() };
        let general = fono_core::config::General::default();
        let mut secrets = Secrets::default();
        secrets.insert("GROQ_API_KEY", "gsk-test");
        let dir = std::path::PathBuf::from("/tmp");
        let interactive = fono_core::config::Interactive::default();
        let got =
            build_streaming_stt(&cfg, &general, false, &interactive, &secrets, &dir).expect("ok");
        assert!(got.is_none(), "live_preview=false should yield None for Groq");
    }

    #[cfg(all(feature = "streaming", feature = "groq"))]
    #[test]
    fn build_streaming_stt_returns_groq_when_live_preview_on() {
        let cfg = SttCfg { backend: SttBackend::Groq, cloud: None, ..SttCfg::default() };
        let general = fono_core::config::General::default();
        let mut secrets = Secrets::default();
        secrets.insert("GROQ_API_KEY", "gsk-test");
        let dir = std::path::PathBuf::from("/tmp");
        let interactive = fono_core::config::Interactive::default();
        let got =
            build_streaming_stt(&cfg, &general, true, &interactive, &secrets, &dir).expect("ok");
        assert!(got.is_some(), "live_preview=true should yield Some(GroqStreaming) for Groq");
    }

    #[cfg(all(feature = "streaming", feature = "deepgram"))]
    #[test]
    fn build_streaming_stt_returns_deepgram_when_live_preview_on() {
        let cfg = SttCfg { backend: SttBackend::Deepgram, cloud: None, ..SttCfg::default() };
        let general = fono_core::config::General::default();
        let mut secrets = Secrets::default();
        secrets.insert("DEEPGRAM_API_KEY", "dg-test");
        let dir = std::path::PathBuf::from("/tmp");
        let interactive = fono_core::config::Interactive::default();
        let got =
            build_streaming_stt(&cfg, &general, true, &interactive, &secrets, &dir).expect("ok");
        assert!(got.is_some(), "live_preview=true should yield Some(DeepgramStreaming)");
        assert_eq!(got.unwrap().name(), "deepgram");
    }

    #[cfg(all(feature = "streaming", feature = "whisper-local"))]
    #[test]
    fn build_streaming_stt_local_missing_model_errors_clearly() {
        // Local *does* support streaming, but the model file is absent
        // — the factory should surface the same explicit error
        // `build_stt` uses so the daemon can warn the user rather than
        // silently falling back.
        let cfg = SttCfg { backend: SttBackend::Local, ..SttCfg::default() };
        let general = fono_core::config::General::default();
        let secrets = Secrets::default();
        let dir = std::env::temp_dir().join("fono-streaming-stt-test-empty");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let interactive = fono_core::config::Interactive::default();
        let err = build_streaming_stt(&cfg, &general, true, &interactive, &secrets, &dir)
            .err()
            .expect("missing model should error");
        let msg = err.to_string();
        assert!(
            msg.contains("not found") && msg.contains("models install"),
            "error should mention remediation: {msg}"
        );
    }

    #[test]
    fn local_languages_override_general_languages() {
        let mut cfg = SttCfg::default();
        cfg.local.languages = vec!["fr".into()];
        let general = fono_core::config::General {
            languages: vec!["en".into(), "ro".into()],
            ..fono_core::config::General::default()
        };
        assert_eq!(effective_languages(&cfg, &general), vec!["fr"]);
    }

    #[test]
    fn empty_local_languages_falls_through_to_general() {
        let cfg = SttCfg::default();
        let general = fono_core::config::General {
            languages: vec!["en".into(), "ro".into()],
            ..fono_core::config::General::default()
        };
        assert_eq!(effective_languages(&cfg, &general), vec!["en", "ro"]);
    }
}
