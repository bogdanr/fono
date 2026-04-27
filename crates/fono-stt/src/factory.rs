// SPDX-License-Identifier: GPL-3.0-only
//! Build a concrete [`SpeechToText`] from `Config` + `Secrets`.
//!
//! Cloud branches are gated by feature flags; missing features produce a
//! clear error so the daemon can stay running in a "degraded" state and
//! the user can fix the config without recompiling the world.

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use fono_core::config::{General, Stt, SttBackend, SttCloud};
use fono_core::providers::stt_key_env;
use fono_core::Secrets;

use crate::traits::SpeechToText;

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
    let cloud_force_primary = general.cloud_force_primary_language;
    let cloud_rerun = general.cloud_rerun_on_language_mismatch;
    match &cfg.backend {
        SttBackend::Local => build_local(cfg, whisper_models_dir, languages),
        SttBackend::Groq => build_groq(cfg, secrets, languages, cloud_force_primary, cloud_rerun),
        SttBackend::OpenAI => {
            build_openai(cfg, secrets, languages, cloud_force_primary, cloud_rerun)
        }
        other => Err(anyhow!(
            "STT backend {other:?} is not yet implemented in this build; \
             pick `groq`, `openai`, or `local` (rebuild with `--features whisper-local` \
             for `local`)"
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
fn build_local(cfg: &Stt, dir: &Path, languages: Vec<String>) -> Result<Arc<dyn SpeechToText>> {
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
    Ok(Arc::new(
        crate::whisper_local::WhisperLocal::with_threads(path, threads).with_languages(languages),
    ))
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
fn build_local(_cfg: &Stt, _dir: &Path, _languages: Vec<String>) -> Result<Arc<dyn SpeechToText>> {
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
    cloud_force_primary: bool,
    cloud_rerun: bool,
) -> Result<Arc<dyn SpeechToText>> {
    let (key, model) = resolve_cloud(cfg, secrets, &SttBackend::Groq, "groq")?;
    Ok(Arc::new(
        crate::groq::GroqStt::with_model(key, model)
            .with_languages(languages)
            .with_cloud_force_primary(cloud_force_primary)
            .with_cloud_rerun_on_mismatch(cloud_rerun),
    ))
}

#[cfg(not(feature = "groq"))]
fn build_groq(
    _: &Stt,
    _: &Secrets,
    _: Vec<String>,
    _: bool,
    _: bool,
) -> Result<Arc<dyn SpeechToText>> {
    Err(anyhow!(
        "Groq STT not compiled in (enable the `groq` feature on `fono-stt`)"
    ))
}

#[cfg(feature = "openai")]
fn build_openai(
    cfg: &Stt,
    secrets: &Secrets,
    languages: Vec<String>,
    cloud_force_primary: bool,
    cloud_rerun: bool,
) -> Result<Arc<dyn SpeechToText>> {
    let (key, model) = resolve_cloud(cfg, secrets, &SttBackend::OpenAI, "openai")?;
    Ok(Arc::new(
        crate::openai::OpenAiStt::with_model(key, model)
            .with_languages(languages)
            .with_cloud_force_primary(cloud_force_primary)
            .with_cloud_rerun_on_mismatch(cloud_rerun),
    ))
}

#[cfg(not(feature = "openai"))]
fn build_openai(
    _: &Stt,
    _: &Secrets,
    _: Vec<String>,
    _: bool,
    _: bool,
) -> Result<Arc<dyn SpeechToText>> {
    Err(anyhow!(
        "OpenAI STT not compiled in (enable the `openai` feature on `fono-stt`)"
    ))
}

/// Streaming-STT factory. Mirrors [`build_stt`] but returns
/// `Arc<dyn StreamingStt>`. Slice A only implements the local
/// (`whisper-rs`) streaming path; cloud backends return `Ok(None)`
/// with a `warn!` so the caller (the daemon) can fall back to the
/// batch path gracefully. Slice B will fill in cloud streaming.
#[cfg(feature = "streaming")]
pub fn build_streaming_stt(
    cfg: &Stt,
    general: &General,
    _secrets: &Secrets,
    whisper_models_dir: &Path,
) -> Result<Option<Arc<dyn crate::streaming::StreamingStt>>> {
    match &cfg.backend {
        SttBackend::Local => {
            let languages = effective_languages(cfg, general);
            build_local_streaming(cfg, whisper_models_dir, languages).map(Some)
        }
        other => {
            let label = fono_core::providers::stt_backend_str(other);
            tracing::warn!(
                "streaming STT not yet supported for backend {label}; \
                 live dictation will fall back to batch"
            );
            Ok(None)
        }
    }
}

#[cfg(all(feature = "streaming", feature = "whisper-local"))]
fn build_local_streaming(
    cfg: &Stt,
    dir: &Path,
    languages: Vec<String>,
) -> Result<Arc<dyn crate::streaming::StreamingStt>> {
    let model = &cfg.local.model;
    let path = dir.join(format!("ggml-{model}.bin"));
    if !path.exists() {
        return Err(anyhow!(
            "local whisper model {model:?} not found at {} — \
             run `fono models install {model}`",
            path.display()
        ));
    }
    let threads: i32 = match cfg.local.threads {
        0 => i32::try_from(detect_physical_cores()).unwrap_or(4),
        n => i32::try_from(n).unwrap_or(i32::MAX),
    };
    Ok(Arc::new(
        crate::whisper_local::WhisperLocal::with_threads(path, threads).with_languages(languages),
    ))
}

#[cfg(all(feature = "streaming", not(feature = "whisper-local")))]
fn build_local_streaming(
    _cfg: &Stt,
    _dir: &Path,
    _languages: Vec<String>,
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
        let cfg = SttCfg {
            backend: SttBackend::Groq,
            cloud: None,
            ..SttCfg::default()
        };
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
        let cfg = SttCfg {
            backend: SttBackend::Groq,
            cloud: None,
            ..SttCfg::default()
        };
        let general = fono_core::config::General::default();
        let secrets = Secrets::default();
        let dir = std::path::PathBuf::from("/tmp");
        let err = build_stt(&cfg, &general, &secrets, &dir)
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.contains("GROQ_API_KEY") && err.contains("fono keys add"),
            "error message should mention env var and remediation: {err}"
        );
    }

    #[cfg(all(feature = "streaming", feature = "groq"))]
    #[test]
    fn build_streaming_stt_returns_none_for_cloud_backend() {
        // Groq is configured (key present) but is not yet a streaming
        // backend in Slice A; the factory should report `None` and let
        // the caller fall back to the batch path.
        let cfg = SttCfg {
            backend: SttBackend::Groq,
            cloud: None,
            ..SttCfg::default()
        };
        let general = fono_core::config::General::default();
        let mut secrets = Secrets::default();
        secrets.insert("GROQ_API_KEY", "gsk-test");
        let dir = std::path::PathBuf::from("/tmp");
        let got = build_streaming_stt(&cfg, &general, &secrets, &dir).expect("ok");
        assert!(got.is_none(), "cloud backend should yield None in Slice A");
    }

    #[cfg(all(feature = "streaming", feature = "whisper-local"))]
    #[test]
    fn build_streaming_stt_local_missing_model_errors_clearly() {
        // Local *does* support streaming, but the model file is absent
        // — the factory should surface the same explicit error
        // `build_stt` uses so the daemon can warn the user rather than
        // silently falling back.
        let cfg = SttCfg {
            backend: SttBackend::Local,
            ..SttCfg::default()
        };
        let general = fono_core::config::General::default();
        let secrets = Secrets::default();
        let dir = std::env::temp_dir().join("fono-streaming-stt-test-empty");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let err = build_streaming_stt(&cfg, &general, &secrets, &dir)
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
