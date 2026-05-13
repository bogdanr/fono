// SPDX-License-Identifier: GPL-3.0-only
//! Build a concrete [`TextToSpeech`] from `Config` + `Secrets`.
//!
//! Mirrors the shape of `fono_stt::factory`: feature-gated, env-var
//! fallback, and a clear error message when a backend isn't compiled
//! in so the daemon can keep running while the user fixes the config.

#[allow(unused_imports)]
use std::sync::Arc;

use anyhow::{anyhow, Result};
use fono_core::config::{Tts, TtsBackend};
#[cfg(any(
    feature = "openai",
    feature = "groq",
    feature = "openrouter",
    feature = "cartesia",
    feature = "deepgram"
))]
use fono_core::providers::tts_key_env;
#[allow(unused_imports)]
use fono_core::Secrets;

use crate::traits::TextToSpeech;

/// Construct a TTS backend from `cfg`. Returns `Ok(None)` for
/// `TtsBackend::None` so callers can treat "TTS disabled" without
/// matching on the enum themselves.
#[cfg_attr(
    not(any(
        feature = "wyoming",
        feature = "openai",
        feature = "groq",
        feature = "openrouter",
        feature = "cartesia",
        feature = "deepgram"
    )),
    allow(unused_variables)
)]
pub fn build_tts(cfg: &Tts, secrets: &Secrets) -> Result<Option<Arc<dyn TextToSpeech>>> {
    match cfg.backend {
        TtsBackend::None => Ok(None),
        TtsBackend::Wyoming => build_wyoming(cfg).map(Some),
        TtsBackend::Piper => build_piper(cfg).map(Some),
        TtsBackend::OpenAI => build_openai(cfg, secrets).map(Some),
        TtsBackend::Groq => build_groq(cfg, secrets).map(Some),
        TtsBackend::OpenRouter => build_openrouter(cfg, secrets).map(Some),
        TtsBackend::Cartesia => build_cartesia(cfg, secrets).map(Some),
        TtsBackend::Deepgram => build_deepgram(cfg, secrets).map(Some),
    }
}

#[cfg(feature = "wyoming")]
fn build_wyoming(cfg: &Tts) -> Result<Arc<dyn TextToSpeech>> {
    let wy = cfg.wyoming.as_ref().ok_or_else(|| {
        anyhow!(
            "wyoming TTS selected but `[tts.wyoming]` is missing — set \
             `[tts.wyoming].uri = \"tcp://localhost:10200\"` (the wyoming-piper default)"
        )
    })?;
    if wy.uri.trim().is_empty() {
        return Err(anyhow!(
            "wyoming TTS selected but `[tts.wyoming].uri` is empty — set it to a \
             URL like `tcp://piper.local:10200`"
        ));
    }
    let backend = crate::wyoming::WyomingTts::from_uri(&wy.uri)?;
    // auth_token reserved for the future fono.auth extension event;
    // not threaded through yet (Wyoming v1 has no in-band auth).
    Ok(Arc::new(backend))
}

#[cfg(not(feature = "wyoming"))]
fn build_wyoming(_cfg: &Tts) -> Result<Arc<dyn TextToSpeech>> {
    Err(anyhow!(
        "Wyoming TTS not compiled in (enable the `wyoming` feature on `fono-tts`)"
    ))
}

#[cfg(feature = "piper-local")]
fn build_piper(_cfg: &Tts) -> Result<Arc<dyn TextToSpeech>> {
    // Stub. See `crates/fono-tts/src/piper_local.rs` for the rationale —
    // onnxruntime conflicts with the static-musl ship build.
    Err(anyhow!(
        "in-process Piper is not yet supported in this build. Run wyoming-piper \
         instead (`docker run --rm -p 10200:10200 rhasspy/wyoming-piper`) and \
         set `tts.backend = \"wyoming\"` with `[tts.wyoming].uri = \
         \"tcp://localhost:10200\"`."
    ))
}

#[cfg(not(feature = "piper-local"))]
fn build_piper(_cfg: &Tts) -> Result<Arc<dyn TextToSpeech>> {
    Err(anyhow!(
        "in-process Piper not compiled in. Use the `wyoming` backend pointing \
         at wyoming-piper instead."
    ))
}

/// Resolve `(api_key_ref, model_override, voice_override)` from the
/// optional `[tts.cloud]` block, falling back to the canonical env-var
/// name for `backend` when the block is absent or fields are empty.
#[cfg(any(
    feature = "openai",
    feature = "groq",
    feature = "openrouter",
    feature = "cartesia",
    feature = "deepgram"
))]
fn resolve_cloud(
    cfg: &Tts,
    backend: &TtsBackend,
) -> (String, Option<String>, Option<String>) {
    let canonical = tts_key_env(backend);
    cfg.cloud.as_ref().map_or_else(
        || (canonical.to_string(), None, None),
        |c| {
            let k = if c.api_key_ref.is_empty() {
                canonical.to_string()
            } else {
                c.api_key_ref.clone()
            };
            let m = if c.model.is_empty() {
                None
            } else {
                Some(c.model.clone())
            };
            (k, m, None)
        },
    )
}

#[cfg(any(
    feature = "openai",
    feature = "groq",
    feature = "openrouter",
    feature = "cartesia",
    feature = "deepgram"
))]
fn resolve_voice(cfg: &Tts, voice_override: Option<String>) -> Option<String> {
    if cfg.voice.is_empty() {
        voice_override
    } else {
        Some(cfg.voice.clone())
    }
}

#[cfg(any(
    feature = "openai",
    feature = "groq",
    feature = "openrouter",
    feature = "cartesia",
    feature = "deepgram"
))]
fn resolve_key(key_ref: &str, backend: &TtsBackend, secrets: &Secrets) -> Result<String> {
    secrets.resolve(key_ref).ok_or_else(|| {
        let display = match backend {
            TtsBackend::OpenAI => "OpenAI",
            TtsBackend::Groq => "Groq",
            TtsBackend::OpenRouter => "OpenRouter",
            TtsBackend::Cartesia => "Cartesia",
            TtsBackend::Deepgram => "Deepgram",
            _ => "TTS",
        };
        anyhow!(
            "{display} TTS API key {key_ref:?} not found in secrets.toml or environment; \
             run `fono keys add {key_ref}` to add it"
        )
    })
}

#[cfg(feature = "openai")]
fn build_openai(cfg: &Tts, secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    let (key_ref, model_override, voice_override) = resolve_cloud(cfg, &TtsBackend::OpenAI);
    let key = resolve_key(&key_ref, &TtsBackend::OpenAI, secrets)?;
    let voice = resolve_voice(cfg, voice_override);
    Ok(Arc::new(crate::openai_compat::openai_client(
        key,
        model_override,
        voice,
    )))
}

#[cfg(not(feature = "openai"))]
fn build_openai(_cfg: &Tts, _secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    Err(anyhow!(
        "OpenAI TTS not compiled in (enable the `openai` feature on `fono-tts`)"
    ))
}

#[cfg(feature = "groq")]
fn build_groq(cfg: &Tts, secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    let (key_ref, model_override, voice_override) = resolve_cloud(cfg, &TtsBackend::Groq);
    let key = resolve_key(&key_ref, &TtsBackend::Groq, secrets)?;
    let voice = resolve_voice(cfg, voice_override);
    Ok(Arc::new(crate::openai_compat::groq_client(
        key,
        model_override,
        voice,
    )))
}

#[cfg(not(feature = "groq"))]
fn build_groq(_cfg: &Tts, _secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    Err(anyhow!(
        "Groq TTS not compiled in (enable the `groq` feature on `fono-tts`)"
    ))
}

#[cfg(feature = "openrouter")]
fn build_openrouter(cfg: &Tts, secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    let (key_ref, model_override, voice_override) =
        resolve_cloud(cfg, &TtsBackend::OpenRouter);
    let key = resolve_key(&key_ref, &TtsBackend::OpenRouter, secrets)?;
    let voice = resolve_voice(cfg, voice_override);
    Ok(Arc::new(crate::openai_compat::openrouter_client(
        key,
        model_override,
        voice,
    )))
}

#[cfg(not(feature = "openrouter"))]
fn build_openrouter(_cfg: &Tts, _secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    Err(anyhow!(
        "OpenRouter TTS not compiled in (enable the `openrouter` feature on `fono-tts`)"
    ))
}

#[cfg(feature = "cartesia")]
fn build_cartesia(cfg: &Tts, secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    let (key_ref, model_override, voice_override) =
        resolve_cloud(cfg, &TtsBackend::Cartesia);
    let key = resolve_key(&key_ref, &TtsBackend::Cartesia, secrets)?;
    let voice = resolve_voice(cfg, voice_override);
    Ok(Arc::new(crate::cartesia::CartesiaTts::new(
        key,
        model_override,
        voice,
    )))
}

#[cfg(not(feature = "cartesia"))]
fn build_cartesia(_cfg: &Tts, _secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    Err(anyhow!(
        "Cartesia TTS not compiled in (enable the `cartesia` feature on `fono-tts`)"
    ))
}

#[cfg(feature = "deepgram")]
fn build_deepgram(cfg: &Tts, secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    let (key_ref, model_override, _voice_override) =
        resolve_cloud(cfg, &TtsBackend::Deepgram);
    let key = resolve_key(&key_ref, &TtsBackend::Deepgram, secrets)?;
    Ok(Arc::new(crate::deepgram::DeepgramTts::new(
        key,
        model_override,
    )))
}

#[cfg(not(feature = "deepgram"))]
fn build_deepgram(_cfg: &Tts, _secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    Err(anyhow!(
        "Deepgram TTS not compiled in (enable the `deepgram` feature on `fono-tts`)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fono_core::config::{Tts as TtsCfg, TtsBackend, TtsCloud, TtsWyoming};

    #[test]
    fn none_backend_returns_none() {
        let cfg = TtsCfg {
            backend: TtsBackend::None,
            ..TtsCfg::default()
        };
        let secrets = Secrets::default();
        assert!(build_tts(&cfg, &secrets).unwrap().is_none());
    }

    #[cfg(feature = "wyoming")]
    #[test]
    fn wyoming_missing_block_errors_clearly() {
        let cfg = TtsCfg {
            backend: TtsBackend::Wyoming,
            wyoming: None,
            ..TtsCfg::default()
        };
        let err = build_tts(&cfg, &Secrets::default())
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("[tts.wyoming]"), "{err}");
    }

    #[cfg(feature = "wyoming")]
    #[test]
    fn wyoming_empty_uri_errors_clearly() {
        let cfg = TtsCfg {
            backend: TtsBackend::Wyoming,
            wyoming: Some(TtsWyoming::default()),
            ..TtsCfg::default()
        };
        let err = build_tts(&cfg, &Secrets::default())
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("uri"), "{err}");
    }

    #[cfg(feature = "wyoming")]
    #[test]
    fn wyoming_with_uri_succeeds() {
        let cfg = TtsCfg {
            backend: TtsBackend::Wyoming,
            wyoming: Some(TtsWyoming {
                uri: "tcp://localhost:10200".into(),
                ..TtsWyoming::default()
            }),
            ..TtsCfg::default()
        };
        let got = build_tts(&cfg, &Secrets::default()).unwrap();
        assert!(got.is_some());
    }

    #[cfg(feature = "openai")]
    #[test]
    fn openai_missing_key_errors_clearly() {
        let cfg = TtsCfg {
            backend: TtsBackend::OpenAI,
            cloud: Some(TtsCloud {
                provider: "openai".into(),
                ..TtsCloud::default()
            }),
            ..TtsCfg::default()
        };
        let err = build_tts(&cfg, &Secrets::default())
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.contains("OPENAI_API_KEY") && err.contains("fono keys add"),
            "{err}"
        );
    }

    #[cfg(feature = "openai")]
    #[test]
    fn openai_with_env_key_succeeds() {
        let cfg = TtsCfg {
            backend: TtsBackend::OpenAI,
            cloud: None,
            ..TtsCfg::default()
        };
        let mut secrets = Secrets::default();
        secrets.insert("OPENAI_API_KEY", "sk-test");
        assert!(build_tts(&cfg, &secrets).unwrap().is_some());
    }

    /// Phase F: every new cloud backend errors clearly when the key
    /// isn't configured, and builds successfully when it is.
    #[cfg(feature = "groq")]
    #[test]
    fn groq_with_key_succeeds() {
        let cfg = TtsCfg {
            backend: TtsBackend::Groq,
            ..TtsCfg::default()
        };
        let mut secrets = Secrets::default();
        secrets.insert("GROQ_API_KEY", "gsk-test");
        assert!(build_tts(&cfg, &secrets).unwrap().is_some());
    }

    #[cfg(feature = "openrouter")]
    #[test]
    fn openrouter_with_key_succeeds() {
        let cfg = TtsCfg {
            backend: TtsBackend::OpenRouter,
            ..TtsCfg::default()
        };
        let mut secrets = Secrets::default();
        secrets.insert("OPENROUTER_API_KEY", "sk-or-test");
        assert!(build_tts(&cfg, &secrets).unwrap().is_some());
    }

    #[cfg(feature = "cartesia")]
    #[test]
    fn cartesia_with_key_succeeds() {
        let cfg = TtsCfg {
            backend: TtsBackend::Cartesia,
            ..TtsCfg::default()
        };
        let mut secrets = Secrets::default();
        secrets.insert("CARTESIA_API_KEY", "ck-test");
        assert!(build_tts(&cfg, &secrets).unwrap().is_some());
    }

    #[cfg(feature = "deepgram")]
    #[test]
    fn deepgram_with_key_succeeds() {
        let cfg = TtsCfg {
            backend: TtsBackend::Deepgram,
            ..TtsCfg::default()
        };
        let mut secrets = Secrets::default();
        secrets.insert("DEEPGRAM_API_KEY", "dg-test");
        assert!(build_tts(&cfg, &secrets).unwrap().is_some());
    }

    #[cfg(feature = "groq")]
    #[test]
    fn groq_missing_key_errors_clearly() {
        let cfg = TtsCfg {
            backend: TtsBackend::Groq,
            ..TtsCfg::default()
        };
        let err = build_tts(&cfg, &Secrets::default())
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("GROQ_API_KEY"), "{err}");
    }
}
