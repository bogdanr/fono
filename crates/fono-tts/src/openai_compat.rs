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

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use fono_core::provider_catalog::{self, TtsEndpoint};
use fono_http::{
    emit_http_debug, provider_request_id, read_body_with_watchdog, BodyError, Outcome,
    RequestTimings,
};
use serde::Serialize;

use crate::traits::{TextToSpeech, TtsAudio};

/// OpenAI's `pcm` response format is documented as 24 kHz mono int16 LE.
/// Groq and OpenRouter both inherit that contract.
const NATIVE_RATE: u32 = 24_000;

/// Per-stage chunk watchdog for TTS bodies. Empirically OpenRouter's
/// `/audio/speech` proxy delivers a small preamble (~9.6 KB across
/// ~8 chunks) and then pauses for several seconds before resuming the
/// audio stream proper. 5 s was too tight and produced false-stall
/// failures on otherwise-healthy synthesis; 20 s keeps headroom for
/// that pause while still catching genuinely wedged connections far
/// faster than the overall 30 s request timeout.
const TTS_CHUNK_TIMEOUT: Duration = Duration::from_secs(20);

/// One-shot diagnostic: on the first TTS stall observed in a given
/// process lifetime, dump a hex preview of the partial body bytes at
/// `warn!` level. The 9600-byte ~8-chunk preamble pattern we keep
/// hitting on OpenRouter could be SSE framing (`data: { ... }`),
/// JSON metadata, or genuine PCM — the hex tells us which without
/// further speculation.
static STALL_DUMP_FIRED: AtomicBool = AtomicBool::new(false);
/// Overall request cap, reduced from the legacy 60 s backstop because
/// the per-chunk watchdog now catches stalls 4× faster and a 60 s wait
/// for a voice-assistant turn is unusable UX. 30 s leaves 5× headroom
/// over the documented p99 for OpenAI Mini TTS.
const TTS_OVERALL_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// Wire value for the request's `stream_format` field. `Some("audio")`
    /// asks the upstream to stream raw audio bytes as they are
    /// generated; `None` omits the field entirely so providers that
    /// reject unknown fields (e.g. Groq's Orpheus) keep their
    /// known-good wire shape.
    stream_format: Option<&'static str>,
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
        Self::with_response_format(name, base_url, default_model, default_voice, "pcm", None, auth)
    }

    /// Like [`Self::new`] but lets the caller pin the wire-level
    /// `response_format` and optional `stream_format`. Use this for
    /// providers that reject `pcm` (e.g. Groq's Orpheus deployment,
    /// which only accepts `wav`) or that benefit from explicit audio
    /// streaming (e.g. OpenRouter's `gpt-4o-mini-tts` route, which
    /// otherwise buffers the entire synthesis before opening the body).
    #[must_use]
    pub fn with_response_format(
        name: &'static str,
        base_url: impl Into<String>,
        default_model: impl Into<String>,
        default_voice: impl Into<String>,
        response_format: &'static str,
        stream_format: Option<&'static str>,
        auth: AuthHeader,
    ) -> Self {
        Self {
            name,
            base_url: base_url.into(),
            default_model: default_model.into(),
            default_voice: default_voice.into(),
            response_format,
            stream_format,
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

    /// Configured wire-level `stream_format`. Exposed for tests.
    #[must_use]
    pub fn stream_format(&self) -> Option<&'static str> {
        self.stream_format
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut req = match &self.auth {
            AuthHeader::Bearer(token) => req.bearer_auth(token),
            AuthHeader::XApiKey(token) => req.header("X-Api-Key", token),
        };
        // Attribution headers are gated on the configured base URL so
        // the shared OpenAI-compat TTS client doesn't leak Fono's
        // OpenRouter identity into requests aimed at OpenAI or Groq.
        if fono_core::openrouter_attribution::is_openrouter_url(&self.base_url) {
            for (name, value) in fono_core::openrouter_attribution::headers() {
                req = req.header(name, value);
            }
        }
        req
    }
}

/// Warm reqwest client tuned for short, latency-sensitive POSTs.
///
/// Deliberately **disables connection pool reuse** for TTS
/// (`pool_max_idle_per_host(0)`) and forces HTTP/1.1
/// (`http1_only()`). OpenRouter's `/audio/speech` proxy was observed
/// hanging mid-stream on second and subsequent requests when the
/// HTTP/2 connection from the first request was multiplexed: the
/// proxy delivered a single ~9.6 KB chunk and then went silent. A
/// fresh TCP+TLS handshake per TTS request (~200-400 ms) is
/// negligible against multi-second LLM-based synthesis and
/// definitively rules connection-state reuse out as a cause of the
/// observed second-sentence stalls.
pub fn warm_client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .http1_only()
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .timeout(TTS_OVERALL_TIMEOUT)
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
    /// Optional `stream_format` wire field. Skipped entirely when
    /// `None` so providers that reject unknown fields keep the
    /// historical byte-identical request body.
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_format: Option<&'static str>,
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
            return Ok(TtsAudio { pcm: Vec::new(), sample_rate: NATIVE_RATE });
        }
        let v =
            voice.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(self.default_voice.as_str());
        let body = SpeechReq {
            model: &self.default_model,
            voice: v,
            input: text,
            response_format: self.response_format,
            stream_format: self.stream_format,
        };

        // One retry on stall / transport mid-stream errors. Never
        // retried on http_error or connect_error — see the BodyError
        // docs for the rationale. Cap at 2 attempts.
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 1u8..=2 {
            match self.synthesize_once(text, &body, attempt).await {
                Ok(audio) => return Ok(audio),
                Err(SynthAttemptError::Retryable(e)) if attempt < 2 => {
                    tracing::warn!(
                        target: "fono.http",
                        provider = self.name,
                        stage = "tts",
                        attempt,
                        error = %e,
                        "TTS body stalled; retrying once"
                    );
                    last_err = Some(e);
                }
                Err(SynthAttemptError::Retryable(e) | SynthAttemptError::Fatal(e)) => {
                    return Err(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("TTS retry loop exhausted without error")))
    }

    async fn prewarm(&self) -> Result<()> {
        // Cheap GET to `/models` pays the TLS handshake before the
        // user's first F7/F8 press. Auth header is required; we don't
        // care about the response.
        let _ = self
            .apply_auth(self.client.get(self.models_url()))
            .send()
            .await
            .with_context(|| format!("{} TTS prewarm GET /models", self.name))?;
        Ok(())
    }
}

/// Internal error type distinguishing retryable failures (stall, mid-
/// stream transport drop) from fatal ones (HTTP error, decode failure,
/// connect-stage failure). Only retryable errors trigger the in-loop
/// retry in `synthesize`.
enum SynthAttemptError {
    Retryable(anyhow::Error),
    Fatal(anyhow::Error),
}

impl OpenAiCompatTtsClient {
    #[allow(clippy::too_many_lines)]
    async fn synthesize_once(
        &self,
        text: &str,
        body: &SpeechReq<'_>,
        attempt: u8,
    ) -> Result<TtsAudio, SynthAttemptError> {
        let mut timings = RequestTimings::start();
        let send_res = self.apply_auth(self.client.post(self.speech_url())).json(body).send().await;
        let resp = match send_res {
            Ok(r) => {
                timings.mark_headers();
                r
            }
            Err(e) => {
                emit_http_debug(
                    "tts",
                    self.name,
                    "audio/speech",
                    0,
                    &timings,
                    0,
                    None,
                    0,
                    "<none>",
                    attempt,
                    Outcome::ConnectError,
                );
                return Err(SynthAttemptError::Fatal(
                    anyhow::Error::new(e)
                        .context(format!("posting to {} /audio/speech", self.name)),
                ));
            }
        };
        let status = resp.status();
        let request_id = provider_request_id(resp.headers())
            .map(str::to_owned)
            .unwrap_or_else(|| "<none>".to_string());
        let content_length = resp.content_length();
        if !status.is_success() {
            emit_http_debug(
                "tts",
                self.name,
                "audio/speech",
                status.as_u16(),
                &timings,
                0,
                content_length,
                0,
                &request_id,
                attempt,
                Outcome::HttpError,
            );
            let body_text = resp.text().await.unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(SynthAttemptError::Fatal(anyhow!(
                "{} TTS returned {status} (request_id={request_id}, text_len={}): {}",
                self.name,
                text.len(),
                truncate(&body_text, 400),
            )));
        }
        let (bytes, stats) =
            match read_body_with_watchdog(resp, TTS_CHUNK_TIMEOUT, &mut timings).await {
                Ok(b) => b,
                Err(e) => {
                    let retryable = e.is_retryable();
                    let partial = e.partial_bytes();
                    let err_chunks = e.chunks();
                    let outcome = match &e {
                        BodyError::Stalled { .. } => Outcome::Stalled,
                        BodyError::Transport { .. } => Outcome::TransportError,
                    };
                    if matches!(outcome, Outcome::Stalled)
                        && !STALL_DUMP_FIRED.swap(true, Ordering::Relaxed)
                    {
                        let bytes = e.partial();
                        let preview_len = bytes.len().min(256);
                        let head = &bytes[..preview_len];
                        let hex: String =
                            head.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ");
                        let ascii: String = head
                            .iter()
                            .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
                            .collect();
                        tracing::warn!(
                            target: "fono.http",
                            provider = self.name,
                            stage = "tts",
                            partial_bytes = partial,
                            chunks = err_chunks,
                            hex = %hex,
                            ascii = %ascii,
                            "first TTS stall this session — dumping partial body preview (one-shot)"
                        );
                    }
                    emit_http_debug(
                        "tts",
                        self.name,
                        "audio/speech",
                        status.as_u16(),
                        &timings,
                        partial,
                        content_length,
                        err_chunks,
                        &request_id,
                        attempt,
                        outcome,
                    );
                    let ctx = format!(
                        "{} TTS body read failed (request_id={request_id}, attempt={attempt})",
                        self.name
                    );
                    let wrapped = anyhow::Error::new(e).context(ctx);
                    return Err(if retryable {
                        SynthAttemptError::Retryable(wrapped)
                    } else {
                        SynthAttemptError::Fatal(wrapped)
                    });
                }
            };
        let pcm_bytes: &[u8] = if self.response_format == "wav" {
            match strip_wav_header(&bytes) {
                Ok(b) => b,
                Err(e) => {
                    emit_http_debug(
                        "tts",
                        self.name,
                        "audio/speech",
                        status.as_u16(),
                        &timings,
                        stats.bytes,
                        content_length,
                        stats.chunks,
                        &request_id,
                        attempt,
                        Outcome::DecodeError,
                    );
                    return Err(SynthAttemptError::Fatal(
                        e.context(format!("parsing {} TTS WAV response body", self.name)),
                    ));
                }
            }
        } else {
            &bytes
        };
        let pcm = pcm_i16_le_to_f32(pcm_bytes);
        timings.mark_decode_done();
        emit_http_debug(
            "tts",
            self.name,
            "audio/speech",
            status.as_u16(),
            &timings,
            stats.bytes,
            content_length,
            stats.chunks,
            &request_id,
            attempt,
            Outcome::Ok,
        );
        Ok(TtsAudio { pcm, sample_rate: NATIVE_RATE })
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
    let TtsEndpoint::OpenAiCompat { base_url, response_format, stream_format } = tts.endpoint
    else {
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
        stream_format,
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

/// Thin constructor for the OpenRouter provider (OpenAI Mini TTS by
/// default; Kokoro is tracked as future local+cloud-symmetric work).
#[must_use]
pub fn openrouter_client(
    api_key: impl Into<String>,
    model_override: Option<String>,
    voice_override: Option<String>,
) -> OpenAiCompatTtsClient {
    from_catalog("openrouter", "openrouter", api_key, model_override, voice_override)
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
        let bytes: Vec<u8> = [0_i16, 32767, -32767].iter().flat_map(|s| s.to_le_bytes()).collect();
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
        assert_eq!(c.stream_format(), Some("audio"));
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
        assert_eq!(c.speech_url(), "https://api.groq.com/openai/v1/audio/speech");
        assert_eq!(c.default_model(), "canopylabs/orpheus-v1-english");
        assert_eq!(c.default_voice(), "hannah");
        // Groq's Orpheus deployment only accepts `wav`; the client must
        // pick that up from the catalogue.
        assert_eq!(c.response_format(), "wav");
        // Groq's Orpheus proxy is gated against unknown request fields,
        // so the catalogue intentionally omits `stream_format`.
        assert_eq!(c.stream_format(), None);
    }

    #[test]
    fn openrouter_client_uses_catalogue_defaults() {
        let c = openrouter_client("sk-or-x", None, None);
        assert_eq!(c.base_url(), "https://openrouter.ai/api/v1");
        assert_eq!(c.speech_url(), "https://openrouter.ai/api/v1/audio/speech");
        assert_eq!(c.default_model(), "x-ai/grok-voice-tts-1.0");
        assert_eq!(c.default_voice(), "ara");
        // OpenRouter's `/audio/speech` proxy is conservative about
        // unknown request fields for non-OpenAI models; the catalogue
        // intentionally omits `stream_format`.
        assert_eq!(c.stream_format(), None);
    }

    /// Overrides win over the catalogue.
    #[test]
    fn overrides_take_precedence() {
        let c = openai_client("sk-x", Some("tts-1-hd".to_string()), Some("nova".to_string()));
        assert_eq!(c.default_model(), "tts-1-hd");
        assert_eq!(c.default_voice(), "nova");
    }
}
