// SPDX-License-Identifier: GPL-3.0-only
//! OpenRouter STT backend — `POST /api/v1/audio/transcriptions` with a
//! **JSON body** containing base64-encoded audio. Unlike OpenAI's
//! transcriptions endpoint, OpenRouter does NOT accept `multipart/form-data`
//! here; the wire shape is documented at
//! <https://openrouter.ai/docs/guides/overview/multimodal/stt>:
//!
//! ```json
//! POST https://openrouter.ai/api/v1/audio/transcriptions
//! Authorization: Bearer <key>
//! Content-Type: application/json
//!
//! {
//!   "model": "openai/whisper-large-v3-turbo",
//!   "input_audio": { "data": "<base64>", "format": "wav" },
//!   "language": "en"              // optional ISO-639-1
//! }
//! ```
//!
//! Response shape:
//! ```json
//! { "text": "...", "usage": { "seconds": 9.2, "cost": 0.0005, ... } }
//! ```
//!
//! The response intentionally omits a `language` field — OpenRouter
//! does not expose per-utterance detection or per-segment
//! `avg_logprob` scores. As a result this client cannot run the
//! per-peer rerun lane that the Groq / OpenAI backends use to
//! recover when Whisper picks the wrong language inside the
//! allow-list. We document that trade-off in the wizard's
//! provider-picker tagline and fall back to the simple "send the
//! cached language, otherwise auto-detect" policy.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use fono_http::{
    emit_http_debug, provider_request_id, read_body_with_watchdog, BodyError, Outcome,
    RequestTimings,
};
use serde::{Deserialize, Serialize};

use crate::lang::LanguageSelection;
use crate::lang_cache::LanguageCache;
use crate::traits::{SpeechToText, Transcription};

/// Inter-chunk watchdog for STT transcription bodies. STT responses
/// are small JSON payloads (≤ a few KB); 30 s of inter-chunk silence
/// is well past any normal upstream latency.
const STT_CHUNK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1/audio/transcriptions";
/// OpenRouter's authed key-info endpoint — same one the wizard's
/// `validate_cloud_key` uses. Cheap, returns 200 with credit/tier
/// metadata for valid keys, perfect for prewarm.
const OPENROUTER_AUTH_ENDPOINT: &str = "https://openrouter.ai/api/v1/auth/key";
/// Default routes to Groq's distilled Whisper Turbo via OpenRouter.
const DEFAULT_MODEL: &str = "openai/whisper-large-v3-turbo";
pub(crate) const BACKEND_KEY: &str = "openrouter";
pub struct OpenRouterStt {
    api_key: String,
    model: String,
    client: reqwest::Client,
    languages: Vec<String>,
    lang_cache: Arc<LanguageCache>,
    /// Kept for trait parity; OpenRouter STT does not surface a
    /// `language` field on the response so the rerun lane cannot
    /// fire. Setter retained so the factory call site looks the same
    /// as the other backends.
    _prompts: HashMap<String, String>,
}

impl OpenRouterStt {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_model(api_key, DEFAULT_MODEL)
    }

    pub fn with_model(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            client: crate::groq::warm_client(),
            languages: Vec::new(),
            lang_cache: LanguageCache::global(),
            _prompts: HashMap::new(),
        }
    }

    #[must_use]
    pub fn with_languages(mut self, codes: Vec<String>) -> Self {
        self.languages = codes;
        self
    }

    /// No-op for OpenRouter (no `language` field on the response means
    /// we have nothing to validate against the allow-list). Kept so
    /// the factory builder pattern matches the other backends.
    #[must_use]
    pub const fn with_cloud_rerun_on_mismatch(self, _on: bool) -> Self {
        self
    }

    #[must_use]
    pub fn with_lang_cache(mut self, cache: Arc<LanguageCache>) -> Self {
        self.lang_cache = cache;
        self
    }

    #[must_use]
    pub fn with_prompts(mut self, prompts: HashMap<String, String>) -> Self {
        self._prompts = prompts;
        self
    }

    fn effective_selection(&self, lang_override: Option<&str>) -> LanguageSelection {
        LanguageSelection::from_config(&self.languages).with_override(lang_override)
    }

    /// Resolve which `language` value (if any) to send on the
    /// request. `Forced` always wins; `AllowList` prefers the cached
    /// last-seen language for stability across utterances; `Auto`
    /// omits the field so the upstream model auto-detects.
    fn pick_language(&self, selection: &LanguageSelection) -> Option<String> {
        match selection {
            LanguageSelection::Auto => None,
            LanguageSelection::Forced(c) => Some(c.clone()),
            LanguageSelection::AllowList(peers) => {
                self.lang_cache.get(BACKEND_KEY).filter(|cached| peers.iter().any(|p| p == cached))
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn do_request(&self, wav: &[u8], lang: Option<&str>) -> Result<Resp> {
        let body = Body {
            model: &self.model,
            input_audio: InputAudio { data: BASE64_STANDARD.encode(wav), format: "wav" },
            language: lang,
        };
        let mut req = self.client.post(OPENROUTER_ENDPOINT).bearer_auth(&self.api_key);
        for (name, value) in fono_core::openrouter_attribution::headers() {
            req = req.header(name, value);
        }
        let mut timings = RequestTimings::start();
        let res = match req.json(&body).send().await {
            Ok(r) => {
                timings.mark_headers();
                r
            }
            Err(e) => {
                emit_http_debug(
                    "stt",
                    "openrouter",
                    "audio/transcriptions",
                    0,
                    &timings,
                    0,
                    None,
                    0,
                    "<none>",
                    1,
                    Outcome::ConnectError,
                );
                return Err(anyhow::Error::new(e).context("openrouter POST failed"));
            }
        };
        let status = res.status();
        let request_id = provider_request_id(res.headers())
            .map(str::to_owned)
            .unwrap_or_else(|| "<none>".to_string());
        let content_length = res.content_length();
        let (bytes, stats) =
            match read_body_with_watchdog(res, STT_CHUNK_TIMEOUT, &mut timings).await {
                Ok(b) => b,
                Err(e) => {
                    let outcome = match &e {
                        BodyError::Stalled { .. } => Outcome::Stalled,
                        BodyError::Transport { .. } => Outcome::TransportError,
                    };
                    emit_http_debug(
                        "stt",
                        "openrouter",
                        "audio/transcriptions",
                        status.as_u16(),
                        &timings,
                        e.partial_bytes(),
                        content_length,
                        e.chunks(),
                        &request_id,
                        1,
                        outcome,
                    );
                    return Err(anyhow::Error::new(e).context(format!(
                        "openrouter STT body read failed (request_id={request_id})"
                    )));
                }
            };
        let body = String::from_utf8_lossy(&bytes).to_string();
        if !status.is_success() {
            emit_http_debug(
                "stt",
                "openrouter",
                "audio/transcriptions",
                status.as_u16(),
                &timings,
                stats.bytes,
                content_length,
                stats.chunks,
                &request_id,
                1,
                Outcome::HttpError,
            );
            anyhow::bail!("openrouter STT {status} (request_id={request_id}): {body}");
        }
        let parsed = match serde_json::from_str::<Resp>(&body) {
            Ok(p) => p,
            Err(e) => {
                emit_http_debug(
                    "stt",
                    "openrouter",
                    "audio/transcriptions",
                    status.as_u16(),
                    &timings,
                    stats.bytes,
                    content_length,
                    stats.chunks,
                    &request_id,
                    1,
                    Outcome::DecodeError,
                );
                return Err(
                    anyhow::Error::new(e).context(format!("parse openrouter response: {body}"))
                );
            }
        };
        timings.mark_decode_done();
        emit_http_debug(
            "stt",
            "openrouter",
            "audio/transcriptions",
            status.as_u16(),
            &timings,
            stats.bytes,
            content_length,
            stats.chunks,
            &request_id,
            1,
            Outcome::Ok,
        );
        Ok(parsed)
    }
}

#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    input_audio: InputAudio,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<&'a str>,
}

#[derive(Serialize)]
struct InputAudio {
    /// Base64-encoded raw audio bytes (NOT a `data:` URI).
    data: String,
    /// `wav`, `mp3`, `flac`, `m4a`, `ogg`, `webm`, `aac`. We always
    /// send WAV because Fono's recorder produces float32 PCM that we
    /// can RIFF-wrap cheaply via `crate::groq::encode_wav`.
    format: &'static str,
}

#[derive(Deserialize)]
struct Resp {
    text: String,
    /// OpenRouter forwards `usage.seconds` when the upstream provider
    /// reports it. Captured so we can populate `Transcription::duration_ms`.
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    seconds: Option<f32>,
}

#[async_trait]
impl SpeechToText for OpenRouterStt {
    async fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
        lang: Option<&str>,
    ) -> Result<Transcription> {
        let wav = crate::groq::encode_wav(pcm, sample_rate);
        let selection = self.effective_selection(lang);
        let pick = self.pick_language(&selection);

        let parsed = self.do_request(&wav, pick.as_deref()).await?;

        // Without a server-echoed language we record the *requested*
        // code (when forced or cached) so future requests stay
        // consistent. This is a best-effort fallback; if the upstream
        // model actually transcribed a different language we have no
        // way of knowing.
        if let Some(ref code) = pick {
            self.lang_cache.record(BACKEND_KEY, code);
        }

        let language = pick.or_else(|| selection.fallback_hint().map(str::to_string));
        let duration_ms = parsed.usage.and_then(|u| u.seconds).map(|s| (s * 1000.0) as u64);

        Ok(Transcription { text: parsed.text, language, duration_ms })
    }

    fn name(&self) -> &'static str {
        "openrouter"
    }

    async fn prewarm(&self) -> Result<()> {
        // /auth/key is the cheapest authed endpoint OpenRouter exposes
        // (the wizard uses it for `validate_cloud_key`). Cheaper than
        // POSTing a silence buffer to /audio/transcriptions, and warms
        // the same TLS connection the real request will reuse.
        let mut req = self.client.get(OPENROUTER_AUTH_ENDPOINT).bearer_auth(&self.api_key);
        for (name, value) in fono_core::openrouter_attribution::headers() {
            req = req.header(name, value);
        }
        let res = req.send().await.context("openrouter prewarm")?;
        let _ = res.bytes().await;
        Ok(())
    }
}
