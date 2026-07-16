// SPDX-License-Identifier: GPL-3.0-only
//! Build a concrete [`TextToSpeech`] from `Config` + `Secrets`.
//!
//! Mirrors the shape of `fono_stt::factory`: feature-gated, env-var
//! fallback, and a clear error message when a backend isn't compiled
//! in so the daemon can keep running while the user fixes the config.

#[allow(unused_imports)]
use std::sync::Arc;

use std::path::Path;

use anyhow::{anyhow, Result};
use fono_core::config::{Tts, TtsBackend};
#[cfg(any(
    feature = "openai",
    feature = "groq",
    feature = "openrouter",
    feature = "cartesia",
    feature = "deepgram",
    feature = "elevenlabs",
    feature = "speechmatics",
    feature = "gemini"
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
        feature = "deepgram",
        feature = "elevenlabs",
        feature = "speechmatics",
        feature = "gemini"
    )),
    allow(unused_variables)
)]
pub fn build_tts(
    cfg: &Tts,
    secrets: &Secrets,
    languages: &[String],
    voices_dir: &Path,
) -> Result<Option<Arc<dyn TextToSpeech>>> {
    let primary: Arc<dyn TextToSpeech> = match cfg.backend {
        TtsBackend::None => return Ok(None),
        TtsBackend::Wyoming => build_wyoming(cfg)?,
        TtsBackend::OpenAI => build_openai(cfg, secrets)?,
        TtsBackend::Groq => build_groq(cfg, secrets)?,
        TtsBackend::OpenRouter => build_openrouter(cfg, secrets)?,
        TtsBackend::Cartesia => build_cartesia(cfg, secrets, languages)?,
        TtsBackend::Deepgram => build_deepgram(cfg, secrets)?,
        TtsBackend::ElevenLabs => build_elevenlabs(cfg, secrets)?,
        TtsBackend::Speechmatics => build_speechmatics(cfg, secrets)?,
        TtsBackend::Gemini => build_gemini(cfg, secrets)?,
        TtsBackend::Local => build_local(cfg, languages, voices_dir)?,
    };
    Ok(Some(maybe_wrap_english_only(primary, cfg, languages, voices_dir)))
}

/// Wrap an English-only cloud backend so non-English utterances are routed to
/// the local multilingual voice instead of being phonemized as gibberish
/// (see [`crate::english_only_fallback`]). Only the `tts-local` build has a
/// local engine to fall back to; without it the backend is returned as-is.
#[cfg(feature = "tts-local")]
fn maybe_wrap_english_only(
    primary: Arc<dyn TextToSpeech>,
    cfg: &Tts,
    languages: &[String],
    voices_dir: &Path,
) -> Arc<dyn TextToSpeech> {
    if fono_core::provider_catalog::tts_backend_english_only(&cfg.backend) {
        let base_url = (!cfg.local.base_url.is_empty()).then(|| cfg.local.base_url.clone());
        Arc::new(crate::english_only_fallback::EnglishOnlyFallback::new(
            primary, voices_dir, base_url, languages,
        ))
    } else {
        primary
    }
}

#[cfg(not(feature = "tts-local"))]
fn maybe_wrap_english_only(
    primary: Arc<dyn TextToSpeech>,
    _cfg: &Tts,
    _languages: &[String],
    _voices_dir: &Path,
) -> Arc<dyn TextToSpeech> {
    primary
}

/// Build the on-device Piper engine(s) from cached voices. The `.ort`
/// model + `.onnx.json` config of the primary voice are expected to
/// already be present under `voices_dir` (the daemon downloads them at
/// startup via the voice catalog, mirroring the STT model-ensure flow);
/// a missing voice yields an actionable error rather than a silent
/// failure. The returned [`crate::local_router::LocalRouter`] routes
/// each utterance to the voice matching its language and lazily loads
/// the other configured languages on first use.
#[cfg(feature = "tts-local")]
fn build_local(
    cfg: &Tts,
    languages: &[String],
    voices_dir: &Path,
) -> Result<Arc<dyn TextToSpeech>> {
    use fono_core::config::TtsLocalEngine;
    // Supertonic is a single shared pack outside the catalog router, so it is
    // built directly from the cached pack dir. The daemon ensures the pack at
    // startup when the engine is pinned to Supertonic (mirroring the voice
    // ensure flow); a missing pack yields an actionable error here.
    if cfg.local.engine == TtsLocalEngine::Supertonic {
        return build_supertonic(cfg, voices_dir);
    }
    let engine_filter = cfg.local.engine.catalog_filter().map(str::to_string);
    let (default_voice, pinned) = resolve_local_voice(cfg, languages, engine_filter.as_deref())?;
    let router = crate::local_router::LocalRouter::new(
        voices_dir,
        default_voice,
        pinned,
        languages,
        engine_filter,
    )?;
    Ok(Arc::new(router))
}

/// Build the Supertonic engine from the cached pack under
/// `voices_dir/supertonic/`. The speaker id comes from `[tts.local].voice`
/// (a Supertonic speaker name or 0-based index); an empty/unknown value uses
/// speaker 0.
#[cfg(feature = "tts-local")]
fn build_supertonic(cfg: &Tts, voices_dir: &Path) -> Result<Arc<dyn TextToSpeech>> {
    let pack_dir = crate::supertonic::supertonic_dir(voices_dir);
    if !pack_dir.join(crate::supertonic::CONFIG.file).is_file() {
        return Err(anyhow!(
            "Supertonic engine selected but its voice pack is not present under {}; \
             the daemon downloads it on startup — check the logs for a download error",
            pack_dir.display()
        ));
    }
    let sid = crate::supertonic::style::speaker_id(&cfg.local.voice).unwrap_or(0);
    let engine = crate::supertonic::engine::SupertonicLocal::load(&pack_dir, sid)?;
    Ok(Arc::new(engine))
}

/// Resolve which catalog voice the local backend should load, plus whether
/// it is a hard pin (disabling per-language routing). Rules:
///
/// * An explicit `[tts.local].voice` is honoured **only when it is consistent
///   with a pinned `[tts.local].engine`** — a Kokoro voice while the engine
///   is pinned to Piper is ignored so the *engine* selection wins (this is
///   what makes "test Piper" actually play Piper even when the configured
///   voice is a Kokoro one). A consistent pin returns `(voice, true)`.
/// * Otherwise the first catalog voice matching the primary language is
///   chosen, constrained to the pinned engine when one is set, falling back
///   to that engine's first catalog voice. Returned as `(voice, false)` so
///   the router still does per-language routing within the engine.
#[cfg(feature = "tts-local")]
fn resolve_local_voice(
    cfg: &Tts,
    languages: &[String],
    engine_filter: Option<&str>,
) -> Result<(crate::voices::Voice, bool)> {
    if !cfg.local.voice.is_empty() {
        match crate::voices::by_name(&cfg.local.voice)? {
            Some(v) if engine_filter.is_none_or(|e| v.engine == e) => return Ok((v, true)),
            Some(_) => {} // engine pin wins over a cross-engine voice → fall through
            None if engine_filter.is_none() => {
                return Err(anyhow!(
                    "[tts.local].voice = {:?} is not in the voice catalog",
                    cfg.local.voice
                ));
            }
            None => {} // unknown voice but an engine is pinned → fall through
        }
    }
    let lang = languages.first().map_or("en", String::as_str);
    let voice = match engine_filter {
        Some(e) => match crate::voices::for_language_engine(lang, e)? {
            Some(v) => Some(v),
            None => crate::voices::for_engine(e)?.into_iter().next(),
        },
        None => crate::voices::for_language(lang)?,
    };
    let voice = voice.ok_or_else(|| {
        engine_filter.map_or_else(
            || {
                anyhow!(
                    "no local voice in the catalog for language {lang:?}; \
                     set [tts.local].voice to a catalog voice id"
                )
            },
            |e| anyhow!("no local {e} voice in the catalog (language {lang:?})"),
        )
    })?;
    Ok((voice, false))
}

#[cfg(not(feature = "tts-local"))]
fn build_local(
    _cfg: &Tts,
    _languages: &[String],
    _voices_dir: &Path,
) -> Result<Arc<dyn TextToSpeech>> {
    Err(anyhow!("local ONNX TTS not compiled in (build `fono` with the `tts-local` feature)"))
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
    Err(anyhow!("Wyoming TTS not compiled in (enable the `wyoming` feature on `fono-tts`)"))
}

/// Resolve `(api_key_ref, model_override, voice_override)` from the
/// optional `[tts.cloud]` block, falling back to the canonical env-var
/// name for `backend` when the block is absent or fields are empty.
#[cfg(any(
    feature = "openai",
    feature = "groq",
    feature = "openrouter",
    feature = "cartesia",
    feature = "deepgram",
    feature = "elevenlabs",
    feature = "speechmatics",
    feature = "gemini"
))]
fn resolve_cloud(cfg: &Tts, backend: &TtsBackend) -> (String, Option<String>, Option<String>) {
    let canonical = tts_key_env(backend);
    cfg.cloud.as_ref().map_or_else(
        || (canonical.to_string(), None, None),
        |c| {
            let k = if c.api_key_ref.is_empty() {
                canonical.to_string()
            } else {
                c.api_key_ref.clone()
            };
            let m = if c.model.is_empty() { None } else { Some(c.model.clone()) };
            (k, m, None)
        },
    )
}

#[cfg(any(
    feature = "openai",
    feature = "groq",
    feature = "openrouter",
    feature = "cartesia",
    feature = "deepgram",
    feature = "elevenlabs",
    feature = "speechmatics",
    feature = "gemini"
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
    feature = "deepgram",
    feature = "elevenlabs",
    feature = "speechmatics",
    feature = "gemini"
))]
fn resolve_key(key_ref: &str, backend: &TtsBackend, secrets: &Secrets) -> Result<String> {
    secrets.resolve(key_ref).ok_or_else(|| {
        let display = match backend {
            TtsBackend::OpenAI => "OpenAI",
            TtsBackend::Groq => "Groq",
            TtsBackend::OpenRouter => "OpenRouter",
            TtsBackend::Cartesia => "Cartesia",
            TtsBackend::Deepgram => "Deepgram",
            TtsBackend::ElevenLabs => "ElevenLabs",
            TtsBackend::Speechmatics => "Speechmatics",
            TtsBackend::Gemini => "Gemini",
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
    Ok(Arc::new(crate::openai_compat::openai_client(key, model_override, voice)))
}

#[cfg(not(feature = "openai"))]
fn build_openai(_cfg: &Tts, _secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    Err(anyhow!("OpenAI TTS not compiled in (enable the `openai` feature on `fono-tts`)"))
}

#[cfg(feature = "groq")]
fn build_groq(cfg: &Tts, secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    let (key_ref, model_override, voice_override) = resolve_cloud(cfg, &TtsBackend::Groq);
    let key = resolve_key(&key_ref, &TtsBackend::Groq, secrets)?;
    let voice = resolve_voice(cfg, voice_override);
    Ok(Arc::new(crate::openai_compat::groq_client(key, model_override, voice)))
}

#[cfg(not(feature = "groq"))]
fn build_groq(_cfg: &Tts, _secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    Err(anyhow!("Groq TTS not compiled in (enable the `groq` feature on `fono-tts`)"))
}

#[cfg(feature = "openrouter")]
fn build_openrouter(cfg: &Tts, secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    let (key_ref, model_override, voice_override) = resolve_cloud(cfg, &TtsBackend::OpenRouter);
    let key = resolve_key(&key_ref, &TtsBackend::OpenRouter, secrets)?;
    let voice = resolve_voice(cfg, voice_override);
    Ok(Arc::new(crate::openai_compat::openrouter_client(key, model_override, voice)))
}

#[cfg(not(feature = "openrouter"))]
fn build_openrouter(_cfg: &Tts, _secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    Err(anyhow!("OpenRouter TTS not compiled in (enable the `openrouter` feature on `fono-tts`)"))
}

#[cfg(feature = "cartesia")]
fn build_cartesia(
    cfg: &Tts,
    secrets: &Secrets,
    languages: &[String],
) -> Result<Arc<dyn TextToSpeech>> {
    let (key_ref, model_override, voice_override) = resolve_cloud(cfg, &TtsBackend::Cartesia);
    let key = resolve_key(&key_ref, &TtsBackend::Cartesia, secrets)?;
    let voice = resolve_voice(cfg, voice_override);
    // The wizard writes the catalogue's `default_voice` UUID into
    // `cfg.tts.voice` every time the user picks Cartesia, which would
    // otherwise look like a hard voice pin to the Cartesia client and
    // disable per-language voice routing. Strip the override when it
    // matches the catalogue default so only a *genuinely customised*
    // voice (a different UUID the user typed in by hand) disables the
    // per-language cache.
    let voice = strip_cartesia_default_voice(voice);
    // The Cartesia client maintains a per-language voice cache and
    // looks up a native voice the first time we need to synthesise
    // in each language. Pass the raw `general.languages` slice
    // straight through; the client normalises it internally.
    Ok(Arc::new(crate::cartesia::CartesiaTts::new(key, model_override, voice, languages)))
}

/// Treat a `cfg.tts.voice` value that matches the Cartesia catalogue
/// default as "no override". The wizard persists that UUID on every
/// pick of the Cartesia backend, so without this filter the client
/// would think the user pinned a voice and disable per-language
/// routing. Exposed for tests.
#[cfg(feature = "cartesia")]
fn strip_cartesia_default_voice(voice: Option<String>) -> Option<String> {
    let default = fono_core::provider_catalog::find("cartesia")
        .and_then(|p| p.tts.as_ref())
        .map(|t| t.default_voice);
    match (voice.as_deref(), default) {
        (Some(v), Some(d)) if v == d => None,
        _ => voice,
    }
}

#[cfg(not(feature = "cartesia"))]
fn build_cartesia(
    _cfg: &Tts,
    _secrets: &Secrets,
    _languages: &[String],
) -> Result<Arc<dyn TextToSpeech>> {
    Err(anyhow!("Cartesia TTS not compiled in (enable the `cartesia` feature on `fono-tts`)"))
}

#[cfg(feature = "deepgram")]
fn build_deepgram(cfg: &Tts, secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    let (key_ref, model_override, _voice_override) = resolve_cloud(cfg, &TtsBackend::Deepgram);
    let key = resolve_key(&key_ref, &TtsBackend::Deepgram, secrets)?;
    Ok(Arc::new(crate::deepgram::DeepgramTts::new(key, model_override)))
}

#[cfg(not(feature = "deepgram"))]
fn build_deepgram(_cfg: &Tts, _secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    Err(anyhow!("Deepgram TTS not compiled in (enable the `deepgram` feature on `fono-tts`)"))
}

#[cfg(feature = "elevenlabs")]
fn build_elevenlabs(cfg: &Tts, secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    let (key_ref, model_override, voice_override) = resolve_cloud(cfg, &TtsBackend::ElevenLabs);
    let key = resolve_key(&key_ref, &TtsBackend::ElevenLabs, secrets)?;
    let voice = resolve_voice(cfg, voice_override);
    Ok(Arc::new(crate::elevenlabs::ElevenLabsTts::new(key, model_override, voice)))
}

#[cfg(not(feature = "elevenlabs"))]
fn build_elevenlabs(_cfg: &Tts, _secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    Err(anyhow!("ElevenLabs TTS not compiled in (enable the `elevenlabs` feature on `fono-tts`)"))
}

#[cfg(feature = "speechmatics")]
fn build_speechmatics(cfg: &Tts, secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    let (key_ref, _model_override, voice_override) = resolve_cloud(cfg, &TtsBackend::Speechmatics);
    let key = resolve_key(&key_ref, &TtsBackend::Speechmatics, secrets)?;
    let voice = resolve_voice(cfg, voice_override);
    Ok(Arc::new(crate::speechmatics::SpeechmaticsTts::new(key, voice)))
}

#[cfg(not(feature = "speechmatics"))]
fn build_speechmatics(_cfg: &Tts, _secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    Err(anyhow!(
        "Speechmatics TTS not compiled in (enable the `speechmatics` feature on `fono-tts`)"
    ))
}

#[cfg(feature = "gemini")]
fn build_gemini(cfg: &Tts, secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    let (key_ref, model_override, voice_override) = resolve_cloud(cfg, &TtsBackend::Gemini);
    let key = resolve_key(&key_ref, &TtsBackend::Gemini, secrets)?;
    let voice = resolve_voice(cfg, voice_override);
    Ok(Arc::new(crate::gemini::GeminiTts::new(key, model_override, voice)))
}

#[cfg(not(feature = "gemini"))]
fn build_gemini(_cfg: &Tts, _secrets: &Secrets) -> Result<Arc<dyn TextToSpeech>> {
    Err(anyhow!("Gemini TTS not compiled in (enable the `gemini` feature on `fono-tts`)"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "openai")]
    use fono_core::config::TtsCloud;
    #[cfg(feature = "wyoming")]
    use fono_core::config::TtsWyoming;
    use fono_core::config::{Tts as TtsCfg, TtsBackend};

    #[test]
    fn none_backend_returns_none() {
        let cfg = TtsCfg { backend: TtsBackend::None, ..TtsCfg::default() };
        let secrets = Secrets::default();
        assert!(build_tts(&cfg, &secrets, &[], std::path::Path::new("")).unwrap().is_none());
    }

    #[cfg(feature = "wyoming")]
    #[test]
    fn wyoming_missing_block_errors_clearly() {
        let cfg = TtsCfg { backend: TtsBackend::Wyoming, wyoming: None, ..TtsCfg::default() };
        let err = build_tts(&cfg, &Secrets::default(), &[], std::path::Path::new(""))
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
        let err = build_tts(&cfg, &Secrets::default(), &[], std::path::Path::new(""))
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
        let got = build_tts(&cfg, &Secrets::default(), &[], std::path::Path::new("")).unwrap();
        assert!(got.is_some());
    }

    #[cfg(feature = "openai")]
    #[test]
    fn openai_missing_key_errors_clearly() {
        let cfg = TtsCfg {
            backend: TtsBackend::OpenAI,
            cloud: Some(TtsCloud { provider: "openai".into(), ..TtsCloud::default() }),
            ..TtsCfg::default()
        };
        let err = build_tts(&cfg, &Secrets::default(), &[], std::path::Path::new(""))
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("OPENAI_API_KEY") && err.contains("fono keys add"), "{err}");
    }

    #[cfg(feature = "openai")]
    #[test]
    fn openai_with_env_key_succeeds() {
        let cfg = TtsCfg { backend: TtsBackend::OpenAI, cloud: None, ..TtsCfg::default() };
        let mut secrets = Secrets::default();
        secrets.insert("OPENAI_API_KEY", "sk-test");
        assert!(build_tts(&cfg, &secrets, &[], std::path::Path::new("")).unwrap().is_some());
    }

    /// Phase F: every new cloud backend errors clearly when the key
    /// isn't configured, and builds successfully when it is.
    #[cfg(feature = "groq")]
    #[test]
    fn groq_with_key_succeeds() {
        let cfg = TtsCfg { backend: TtsBackend::Groq, ..TtsCfg::default() };
        let mut secrets = Secrets::default();
        secrets.insert("GROQ_API_KEY", "gsk-test");
        assert!(build_tts(&cfg, &secrets, &[], std::path::Path::new("")).unwrap().is_some());
    }

    #[cfg(feature = "openrouter")]
    #[test]
    fn openrouter_with_key_succeeds() {
        let cfg = TtsCfg { backend: TtsBackend::OpenRouter, ..TtsCfg::default() };
        let mut secrets = Secrets::default();
        secrets.insert("OPENROUTER_API_KEY", "sk-or-test");
        assert!(build_tts(&cfg, &secrets, &[], std::path::Path::new("")).unwrap().is_some());
    }

    #[cfg(feature = "cartesia")]
    #[test]
    fn cartesia_with_key_succeeds() {
        let cfg = TtsCfg { backend: TtsBackend::Cartesia, ..TtsCfg::default() };
        let mut secrets = Secrets::default();
        secrets.insert("CARTESIA_API_KEY", "ck-test");
        assert!(build_tts(&cfg, &secrets, &[], std::path::Path::new("")).unwrap().is_some());
    }

    #[cfg(feature = "deepgram")]
    #[test]
    fn deepgram_with_key_succeeds() {
        let cfg = TtsCfg { backend: TtsBackend::Deepgram, ..TtsCfg::default() };
        let mut secrets = Secrets::default();
        secrets.insert("DEEPGRAM_API_KEY", "dg-test");
        assert!(build_tts(&cfg, &secrets, &[], std::path::Path::new("")).unwrap().is_some());
    }

    #[cfg(feature = "elevenlabs")]
    #[test]
    fn elevenlabs_with_key_succeeds() {
        let cfg = TtsCfg { backend: TtsBackend::ElevenLabs, ..TtsCfg::default() };
        let mut secrets = Secrets::default();
        secrets.insert("ELEVENLABS_API_KEY", "sk-test");
        let tts = build_tts(&cfg, &secrets, &[], std::path::Path::new("")).unwrap().unwrap();
        assert_eq!(tts.name(), "elevenlabs");
    }

    #[cfg(feature = "elevenlabs")]
    #[test]
    fn elevenlabs_missing_key_errors_clearly() {
        let cfg = TtsCfg { backend: TtsBackend::ElevenLabs, ..TtsCfg::default() };
        let err = build_tts(&cfg, &Secrets::default(), &[], std::path::Path::new(""))
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("ELEVENLABS_API_KEY") && err.contains("fono keys add"), "{err}");
    }

    #[cfg(feature = "speechmatics")]
    #[test]
    fn speechmatics_with_key_succeeds() {
        let cfg = TtsCfg { backend: TtsBackend::Speechmatics, ..TtsCfg::default() };
        let mut secrets = Secrets::default();
        secrets.insert("SPEECHMATICS_API_KEY", "sm-test");
        let tts = build_tts(&cfg, &secrets, &[], std::path::Path::new("")).unwrap().unwrap();
        assert_eq!(tts.name(), "speechmatics");
    }

    #[cfg(feature = "speechmatics")]
    #[test]
    fn speechmatics_missing_key_errors_clearly() {
        let cfg = TtsCfg { backend: TtsBackend::Speechmatics, ..TtsCfg::default() };
        let err = build_tts(&cfg, &Secrets::default(), &[], std::path::Path::new(""))
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("SPEECHMATICS_API_KEY") && err.contains("fono keys add"), "{err}");
    }

    #[cfg(feature = "groq")]
    #[test]
    fn groq_missing_key_errors_clearly() {
        let cfg = TtsCfg { backend: TtsBackend::Groq, ..TtsCfg::default() };
        let err = build_tts(&cfg, &Secrets::default(), &[], std::path::Path::new(""))
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("GROQ_API_KEY"), "{err}");
    }

    #[cfg(feature = "cartesia")]
    #[test]
    fn strip_cartesia_default_voice_returns_none_for_catalogue_uuid() {
        let default = fono_core::provider_catalog::find("cartesia")
            .and_then(|p| p.tts.as_ref())
            .map(|t| t.default_voice.to_string())
            .expect("cartesia catalogue must define a default voice");
        assert!(strip_cartesia_default_voice(Some(default)).is_none());
    }

    #[cfg(feature = "cartesia")]
    #[test]
    fn strip_cartesia_default_voice_preserves_custom_voice() {
        let custom = "deadbeef-dead-beef-dead-beefdeadbeef".to_string();
        assert_eq!(strip_cartesia_default_voice(Some(custom.clone())), Some(custom));
    }

    #[cfg(feature = "cartesia")]
    #[test]
    fn strip_cartesia_default_voice_preserves_none() {
        assert!(strip_cartesia_default_voice(None).is_none());
    }

    /// Wizard round-trip regression: a config that contains the
    /// catalogue's default voice (the wizard's behaviour every time
    /// the user picks Cartesia) must NOT disable per-language voice
    /// routing. We verify the helper instead of the constructed
    /// trait-object so the test stays cheap; the constructor wiring
    /// is covered by the `voice_pinned` cartesia.rs unit test.
    #[cfg(feature = "cartesia")]
    #[test]
    fn cartesia_wizard_default_voice_does_not_pin() {
        let default = fono_core::provider_catalog::find("cartesia")
            .and_then(|p| p.tts.as_ref())
            .map(|t| t.default_voice.to_string())
            .unwrap();
        let cfg =
            TtsCfg { backend: TtsBackend::Cartesia, voice: default.clone(), ..TtsCfg::default() };
        let (_key, _model, override_voice) = resolve_cloud(&cfg, &TtsBackend::Cartesia);
        let voice = resolve_voice(&cfg, override_voice);
        assert_eq!(
            voice,
            Some(default),
            "factory's resolve_voice must surface the configured UUID"
        );
        // The crucial bit: `build_cartesia` then strips the catalogue
        // default so the Cartesia client doesn't see a pin.
        assert!(
            strip_cartesia_default_voice(voice).is_none(),
            "catalogue default UUID must NOT pin the voice"
        );
    }

    #[cfg(feature = "tts-local")]
    #[test]
    fn engine_pin_overrides_cross_engine_voice() {
        use fono_core::config::{TtsLocal, TtsLocalEngine};
        // A configured Kokoro voice while the engine is pinned to Piper: the
        // engine must win, so the resolved voice is a Piper one and NOT a hard
        // pin (routing within the engine stays enabled). Regression for
        // "selected Piper but it played Kokoro".
        let kokoro = crate::voices::for_engine("kokoro").unwrap().into_iter().next().unwrap();
        let cfg = TtsCfg {
            backend: TtsBackend::Local,
            local: TtsLocal {
                voice: kokoro.name,
                engine: TtsLocalEngine::Piper,
                ..TtsLocal::default()
            },
            ..TtsCfg::default()
        };
        let (voice, pinned) =
            resolve_local_voice(&cfg, &["en".to_string()], Some("piper")).unwrap();
        assert_eq!(voice.engine, "piper", "engine pin must override a cross-engine voice");
        assert!(!pinned, "a dropped cross-engine voice must not pin the router");
    }

    #[cfg(feature = "tts-local")]
    #[test]
    fn consistent_voice_pin_is_honoured() {
        use fono_core::config::{TtsLocal, TtsLocalEngine};
        let kokoro = crate::voices::for_engine("kokoro").unwrap().into_iter().next().unwrap();
        let expected = kokoro.name.clone();
        let cfg = TtsCfg {
            backend: TtsBackend::Local,
            local: TtsLocal {
                voice: kokoro.name,
                engine: TtsLocalEngine::Kokoro,
                ..TtsLocal::default()
            },
            ..TtsCfg::default()
        };
        let (voice, pinned) =
            resolve_local_voice(&cfg, &["en".to_string()], Some("kokoro")).unwrap();
        assert_eq!(voice.name, expected, "an engine-consistent voice must be honoured");
        assert!(pinned, "an explicit consistent voice pins the router");
    }
}
