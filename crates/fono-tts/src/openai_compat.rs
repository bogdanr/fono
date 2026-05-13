// SPDX-License-Identifier: GPL-3.0-only
//! Shared OpenAI-compatible `/audio/speech` client.
//!
//! Parameterised on base URL, default model, default voice, and auth
//! header style so OpenAI, Groq (`https://api.groq.com/openai/v1`),
//! OpenRouter (`https://openrouter.ai/api/v1`), and any future
//! OpenAI-compatible host can share one implementation.
//!
//! Wire shape:
//!   POST `{base_url}/audio/speech`
//!   body: `{ "model": ..., "voice": ..., "input": ..., "response_format": "pcm" }`
//!   response: raw int16 LE mono PCM at 24 kHz (decoded to f32 mono).

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use fono_core::provider_catalog::{self, TtsEndpoint};
use serde::Serialize;

use crate::traits::{TextToSpeech, TtsAudio};

/// OpenAI's `pcm` response format is documented as 24 kHz mono int16 LE.
/// Groq and OpenRouter both inherit that contract.
const NATIVE_RATE: u32 = 24_000;

/// Auth-header style for an OpenAI-compatible provider.
#[derive(Debug, Clone)]
pub enum AuthHeader {
    /// `Authorization: Bearer <token>` — OpenAI, Groq, OpenRouter.
    Bearer(String),
    /// `X-Api-Key: <token>` — reserved for hypothetical future
    /// providers; not used by any catalogue entry today.
    XApiKey(String),
}

/// OpenAI-compatible TTS client.
pub struct OpenAiCompatTtsClient {
    /// Backend identifier reported by [`TextToSpeech::name`] for
    /// history / logging. Distinct from the OpenAI client so the
    /// daemon's history rows reflect which provider actually served
    /// the synthesis.
    name: &'static str,
    /// Base URL up to and including `/v1`. The client appends
    /// `/audio/speech` and `/models` itself.
    base_url: String,
    default_model: String,
    default_voice: String,
    /// Wire value for the request's `response_format` field. Either
    /// `"pcm"` (raw int16 LE, fastest) or `"wav"` (RIFF-wrapped — the
    /// only format Groq's Orpheus accepts). The client strips the WAV
    /// header transparently when this is `"wav"`.
    response_format: &'static str,
    auth: AuthHeader,
    client: reqwest::Client,
}

impl OpenAiCompatTtsClient {
    /// Build a new client. The caller is expected to source `base_url`,
    /// `default_model`, and `default_voice` from the
    /// [`provider_catalog`] rather than hard-coding strings.
    #[must_use]
    pub fn new(
        name: &'static str,
        base_url: impl Into<String>,
        default_model: impl Into<String>,
        default_voice: impl Into<String>,
        auth: AuthHeader,
    ) -> Self {
        Self::with_response_format(name, base_url, default_model, default_voice, "pcm", auth)
    }

    /// Like [`Self::new`] but lets the caller pin the wire-level
    /// `response_format`. Use this for providers that reject `pcm`
    /// (e.g. Groq's Orpheus deployment, which only accepts `wav`).
    #[must_use]
    pub fn with_response_format(
        name: &'static str,
        base_url: impl Into<String>,
        default_model: impl Into<String>,
        default_voice: impl Into<String>,
        response_format: &'static str,
        auth: AuthHeader,
    ) -> Self {
        Self {
            name,
            base_url: base_url.into(),
            default_model: default_model.into(),
            default_voice: default_voice.into(),
            response_format,
            auth,
            client: warm_client(),
        }
    }

    /// Resolved `POST /audio/speech` URL.
    #[must_use]
    pub fn speech_url(&self) -> String {
        format!("{}/audio/speech", self.base_url.trim_end_matches('/'))
    }

    /// Cheap `GET /models` URL used for prewarm.
    #[must_use]
    pub fn models_url(&self) -> String {
        format!("{}/models", self.base_url.trim_end_matches('/'))
    }

    /// Configured base URL (catalogue value). Exposed for tests.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Configured default model. Exposed for tests.
    #[must_use]
    pub fn default_model(&self) -> &str {
        &self.default_model
    }

    /// Configured default voice. Exposed for tests.
    #[must_use]
    pub fn default_voice(&self) -> &str {
        &self.default_voice
    }

    /// Configured wire-level `response_format`. Exposed for tests.
    #[must_use]
    pub fn response_format(&self) -> &'static str {
        self.response_format
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            AuthHeader::Bearer(token) => req.bearer_auth(token),
            AuthHeader::XApiKey(token) => req.header("X-Api-Key", token),
        }
    }
}

/// Warm reqwest client tuned for short, latency-sensitive POSTs. Same
/// shape as `fono_llm::openai_compat::warm_client()`; kept local to
/// avoid pulling fono-llm into fono-tts.
pub fn warm_client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(4)
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .http2_keep_alive_interval(Some(Duration::from_secs(20)))
        .http2_keep_alive_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default()
}

#[derive(Serialize)]
struct SpeechReq<'a> {
    model: &'a str,
    voice: &'a str,
    input: &'a str,
    response_format: &'static str,
}

#[async_trait]
impl TextToSpeech for OpenAiCompatTtsClient {
    fn name(&self) -> &'static str {
        self.name
    }

    fn native_sample_rate(&self) -> u32 {
        NATIVE_RATE
    }

    async fn synthesize(
        &self,
        text: &str,
        voice: Option<&str>,
        _lang: Option<&str>,
    ) -> Result<TtsAudio> {
        if text.is_empty() {
            return Ok(TtsAudio {
                pcm: Vec::new(),
                sample_rate: NATIVE_RATE,
            });
        }
        let v = voice
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(self.default_voice.as_str());
        let req = SpeechReq {
            model: &self.default_model,
            voice: v,
            input: text,
            response_format: self.response_format,
        };
        let resp = self
            .apply_auth(self.client.post(self.speech_url()))
            .json(&req)
            .send()
            .await
            .with_context(|| format!("posting to {} /audio/speech", self.name))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(anyhow!(
                "{} TTS returned {status}: {}",
                self.name,
                truncate(&body, 400)
            ));
        }
        let bytes = resp
            .bytes()
            .await
            .with_context(|| format!("reading {} TTS response body", self.name))?;
        let pcm_bytes: &[u8] = if self.response_format == "wav" {
            strip_wav_header(&bytes).with_context(|| {
                format!("parsing {} TTS WAV response body", self.name)
            })?
        } else {
            &bytes
        };
        let pcm = pcm_i16_le_to_f32(pcm_bytes);
        Ok(TtsAudio {
            pcm,
            sample_rate: NATIVE_RATE,
        })
    }

    async fn prewarm(&self) -> Result<()> {
        // Cheap GET to `/models` pays the TLS handshake before the
        // user's first F8/F10 press. Auth header is required; we don't
        // care about the response.
        let _ = self
            .apply_auth(self.client.get(self.models_url()))
            .send()
            .await
            .with_context(|| format!("{} TTS prewarm GET /models", self.name))?;
        Ok(())
    }
}

/// Pull the OpenAI-compat base URL out of the catalogue entry for
/// `id`. Returns `None` for catalogue entries that lack a TTS
/// capability or use a non-OpenAI-compat endpoint shape.
#[must_use]
pub fn catalog_base_url(id: &str) -> Option<&'static str> {
    let entry = provider_catalog::find(id)?;
    let tts = entry.tts.as_ref()?;
    match tts.endpoint {
        TtsEndpoint::OpenAiCompat { base_url, .. } => Some(base_url),
        _ => None,
    }
}

/// Build an OpenAI-compatible client for the given catalogue id, using
/// the catalogue's model + default voice + base URL. Returns `None`
/// if the catalogue entry doesn't exist or isn't OpenAI-compat.
#[must_use]
pub fn from_catalog(
    id: &'static str,
    name: &'static str,
    api_key: impl Into<String>,
    model_override: Option<String>,
    voice_override: Option<String>,
) -> Option<OpenAiCompatTtsClient> {
    let entry = provider_catalog::find(id)?;
    let tts = entry.tts.as_ref()?;
    let TtsEndpoint::OpenAiCompat { base_url, response_format } = tts.endpoint else {
        return None;
    };
    let model = model_override.unwrap_or_else(|| tts.model.to_string());
    let voice = voice_override.unwrap_or_else(|| tts.default_voice.to_string());
    Some(OpenAiCompatTtsClient::with_response_format(
        name,
        base_url,
        model,
        voice,
        response_format,
        AuthHeader::Bearer(api_key.into()),
    ))
}

/// Thin constructor for the OpenAI provider. Pulls every default out
/// of the catalogue so changes there flow through automatically.
#[must_use]
pub fn openai_client(
    api_key: impl Into<String>,
    model_override: Option<String>,
    voice_override: Option<String>,
) -> OpenAiCompatTtsClient {
    from_catalog("openai", "openai", api_key, model_override, voice_override)
        .expect("openai catalogue entry must exist with an OpenAI-compat TTS endpoint")
}

/// Thin constructor for the Groq provider (Canopy Labs Orpheus on
/// Groq's OpenAI-compat endpoint). Replaces the decommissioned
/// PlayAI family Groq retired in 2026.
#[must_use]
pub fn groq_client(
    api_key: impl Into<String>,
    model_override: Option<String>,
    voice_override: Option<String>,
) -> OpenAiCompatTtsClient {
    from_catalog("groq", "groq", api_key, model_override, voice_override)
        .expect("groq catalogue entry must exist with an OpenAI-compat TTS endpoint")
}

/// Thin constructor for the OpenRouter provider (Kokoro by default).
#[must_use]
pub fn openrouter_client(
    api_key: impl Into<String>,
    model_override: Option<String>,
    voice_override: Option<String>,
) -> OpenAiCompatTtsClient {
    from_catalog(
        "openrouter",
        "openrouter",
        api_key,
        model_override,
        voice_override,
    )
    .expect("openrouter catalogue entry must exist with an OpenAI-compat TTS endpoint")
}

/// Locate the `data` chunk in a RIFF/WAVE byte stream and return the
/// PCM payload slice. Tolerates additional chunks (`LIST`, `bext`,
/// etc.) appearing between the `fmt ` and `data` chunks, which some
/// providers emit. Returns an error if the buffer isn't a well-formed
/// WAVE with at least one `data` chunk.
fn strip_wav_header(bytes: &[u8]) -> anyhow::Result<&[u8]> {
    if bytes.len() < 20 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(anyhow!(
            "expected RIFF/WAVE container, got {} bytes (first 4: {:?})",
            bytes.len(),
            bytes.get(..4).unwrap_or(&[]),
        ));
    }
    let mut cursor = 12_usize;
    while cursor + 8 <= bytes.len() {
        let id = &bytes[cursor..cursor + 4];
        let size = u32::from_le_bytes([
            bytes[cursor + 4],
            bytes[cursor + 5],
            bytes[cursor + 6],
            bytes[cursor + 7],
        ]) as usize;
        let payload_start = cursor + 8;
        if id == b"data" {
            let end = payload_start.saturating_add(size).min(bytes.len());
            return Ok(&bytes[payload_start..end]);
        }
        // Chunks are word-aligned: round size up to even.
        cursor = payload_start.saturating_add(size + (size & 1));
    }
    Err(anyhow!("RIFF/WAVE container had no `data` chunk"))
}

fn pcm_i16_le_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32767.0)
        .collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out = s[..max].to_string();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_text_returns_empty_audio_without_request() {
        let tts = openai_client("sk-test", None, None);
        let audio = tts.synthesize("", None, None).await.unwrap();
        assert!(audio.pcm.is_empty());
        assert_eq!(audio.sample_rate, NATIVE_RATE);
    }

    #[test]
    fn pcm_decode_round_trip() {
        let bytes: Vec<u8> = [0_i16, 32767, -32767]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let f = pcm_i16_le_to_f32(&bytes);
        assert_eq!(f.len(), 3);
        assert!((f[1] - 1.0).abs() < 1e-3);
        assert!((f[2] - -1.0).abs() < 1e-3);
    }

    #[test]
    fn truncate_handles_short_strings() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    /// F1 acceptance: every OpenAI-compat constructor picks up the
    /// catalogue's base URL, model and voice without runtime HTTP.
    #[test]
    fn openai_client_uses_catalogue_defaults() {
        let c = openai_client("sk-x", None, None);
        assert_eq!(c.base_url(), "https://api.openai.com/v1");
        assert_eq!(c.speech_url(), "https://api.openai.com/v1/audio/speech");
        assert_eq!(c.models_url(), "https://api.openai.com/v1/models");
        assert_eq!(c.default_model(), "tts-1");
        assert_eq!(c.default_voice(), "alloy");
        assert_eq!(c.native_sample_rate(), NATIVE_RATE);
        assert_eq!(c.response_format(), "pcm");
    }

    /// A minimal RIFF/WAVE byte stream carrying three int16 samples,
    /// shaped like the body Groq's Orpheus deployment returns.
    fn make_wav(samples: &[i16]) -> Vec<u8> {
        let data_bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let data_len = u32::try_from(data_bytes.len()).unwrap();
        let mut buf = Vec::with_capacity(44 + data_bytes.len());
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&(36u32 + data_len).to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
        buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
        buf.extend_from_slice(&1u16.to_le_bytes()); // mono
        buf.extend_from_slice(&NATIVE_RATE.to_le_bytes());
        buf.extend_from_slice(&(NATIVE_RATE * 2).to_le_bytes()); // byte rate
        buf.extend_from_slice(&2u16.to_le_bytes()); // block align
        buf.extend_from_slice(&16u16.to_le_bytes()); // bits/sample
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&data_len.to_le_bytes());
        buf.extend_from_slice(&data_bytes);
        buf
    }

    #[test]
    fn strip_wav_header_extracts_pcm_payload() {
        let wav = make_wav(&[0, 32_767, -32_767]);
        let pcm = strip_wav_header(&wav).expect("valid WAV");
        assert_eq!(pcm.len(), 6, "three int16 samples = 6 bytes");
        let decoded = pcm_i16_le_to_f32(pcm);
        assert_eq!(decoded.len(), 3);
        assert!((decoded[1] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn strip_wav_header_tolerates_unknown_chunks() {
        // Insert a `LIST` chunk between `fmt ` and `data` to mimic
        // providers that emit metadata chunks.
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&100u32.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&[0u8; 16]);
        wav.extend_from_slice(b"LIST");
        wav.extend_from_slice(&4u32.to_le_bytes());
        wav.extend_from_slice(&[1, 2, 3, 4]);
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&4u32.to_le_bytes());
        wav.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        let pcm = strip_wav_header(&wav).expect("valid WAV with LIST chunk");
        assert_eq!(pcm, &[0xAA, 0xBB, 0xCC, 0xDD]);
    }

    #[test]
    fn strip_wav_header_rejects_non_riff() {
        let err = strip_wav_header(b"not a wav at all").unwrap_err();
        assert!(err.to_string().contains("RIFF"));
    }

    #[test]
    fn groq_client_uses_catalogue_defaults() {
        let c = groq_client("gsk-x", None, None);
        assert_eq!(c.base_url(), "https://api.groq.com/openai/v1");
        assert_eq!(
            c.speech_url(),
            "https://api.groq.com/openai/v1/audio/speech"
        );
        assert_eq!(c.default_model(), "canopylabs/orpheus-v1-english");
        assert_eq!(c.default_voice(), "hannah");
        // Groq's Orpheus deployment only accepts `wav`; the client must
        // pick that up from the catalogue.
        assert_eq!(c.response_format(), "wav");
    }

    #[test]
    fn openrouter_client_uses_catalogue_defaults() {
        let c = openrouter_client("sk-or-x", None, None);
        assert_eq!(c.base_url(), "https://openrouter.ai/api/v1");
        assert_eq!(
            c.speech_url(),
            "https://openrouter.ai/api/v1/audio/speech"
        );
        assert_eq!(c.default_model(), "hexgrad/kokoro-82m");
        assert_eq!(c.default_voice(), "af_heart");
    }

    /// Overrides win over the catalogue.
    #[test]
    fn overrides_take_precedence() {
        let c = openai_client(
            "sk-x",
            Some("tts-1-hd".to_string()),
            Some("nova".to_string()),
        );
        assert_eq!(c.default_model(), "tts-1-hd");
        assert_eq!(c.default_voice(), "nova");
    }
}
