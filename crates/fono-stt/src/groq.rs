// SPDX-License-Identifier: GPL-3.0-only
//! Groq STT backend — fastest hosted whisper. HTTPS via reqwest+rustls.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::multipart;
use serde::Deserialize;

use crate::lang::LanguageSelection;
use crate::lang_cache::LanguageCache;
use crate::traits::{SpeechToText, Transcription};

const GROQ_ENDPOINT: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const GROQ_MODELS_ENDPOINT: &str = "https://api.groq.com/openai/v1/models";
/// Default Groq STT model. Latency plan L15 picks `whisper-large-v3-turbo`
/// (≈5× faster than `whisper-1`) — overridable via `stt.cloud.model`.
const DEFAULT_MODEL: &str = "whisper-large-v3-turbo";
/// Cache key shared between `GroqStt` (batch) and `GroqStreaming`
/// (pseudo-stream) so a session that mixes the two converges on a
/// single language memory.
pub(crate) const BACKEND_KEY: &str = "groq";

/// Body text shared by the warn-log line and the desktop notification
/// when Groq returns HTTP 429. Centralised so the two surfaces stay
/// in sync.
const RATE_LIMIT_HINT: &str = "Try increasing `interactive.streaming_interval` to 2.0 or \
                               higher in your config to stay under the per-minute request cap.";

/// Parse Groq's verbose 429 JSON body into a single human-readable
/// line. Falls back to a 120-char excerpt of the raw body when the
/// body isn't the expected `{ "error": { "message": "Rate limit reached
/// for model X in organization Y on requests per minute (RPM): Limit N,
/// Used N, Requested 1. Please try again in Ts." } }` shape.
fn summarise_429(body: &str) -> String {
    #[derive(serde::Deserialize)]
    struct ErrEnvelope {
        error: ErrInner,
    }
    #[derive(serde::Deserialize)]
    struct ErrInner {
        #[serde(default)]
        message: Option<String>,
    }
    let parsed: Option<ErrEnvelope> = serde_json::from_str(body).ok();
    let Some(msg) = parsed.as_ref().and_then(|e| e.error.message.as_deref()) else {
        // Truncate raw body for the fallback so we don't dump a multi-
        // line JSON blob into the log.
        let trimmed: String = body.chars().take(120).collect();
        return if body.len() > 120 {
            format!("{trimmed}…")
        } else {
            trimmed
        };
    };
    // The upstream message is itself dense but readable; trim the
    // upgrade pitch ("Need more tokens? Upgrade to Dev Tier today …")
    // since we already point users at config tuning.
    let cut = msg.find("Need more tokens?").unwrap_or(msg.len());
    msg[..cut].trim().to_string()
}

/// Crate-public wrapper so `groq_streaming.rs` can compact 429 bodies
/// observed via the `with_request_fn` closure (which surfaces them as
/// `anyhow::Error` strings). The body here is usually the full
/// `groq STT 429 …: {json}` error message, not the bare JSON.
pub(crate) fn summarise_429_public(body: &str) -> String {
    // Try to find the JSON envelope inside the wrapped error.
    if let Some(start) = body.find('{') {
        return summarise_429(&body[start..]);
    }
    summarise_429(body)
}

pub struct GroqStt {
    api_key: String,
    model: String,
    client: reqwest::Client,
    /// Configured language allow-list (see `crate::lang`).
    languages: Vec<String>,
    /// When the provider returns a banned language **and** the cache
    /// has a previously-observed peer code for this backend, rerun
    /// once with that code forced. Cold-start (empty cache) skips
    /// the rerun and accepts the unforced response. Default `true`
    /// in v3.
    cloud_rerun_on_mismatch: bool,
    /// Per-backend in-memory language memory. Read on rerun, written
    /// on every in-allow-list detection. See `crate::lang_cache`.
    lang_cache: Arc<LanguageCache>,
    /// Per-language initial-prompt map. See [`crate::groq::GroqStt::resolve_prompt`].
    prompts: std::collections::HashMap<String, String>,
}

impl GroqStt {
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_model(api_key, DEFAULT_MODEL)
    }

    pub fn with_model(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            client: warm_client(),
            languages: Vec::new(),
            cloud_rerun_on_mismatch: false,
            lang_cache: LanguageCache::global(),
            prompts: std::collections::HashMap::new(),
        }
    }

    /// Builder: set the language allow-list. See
    /// [`crate::lang::LanguageSelection`] for semantics.
    #[must_use]
    pub fn with_languages(mut self, codes: Vec<String>) -> Self {
        self.languages = codes;
        self
    }

    /// Builder: re-issue the request with a cached peer code if the
    /// provider returned a banned language. Default `true` in v3.
    #[must_use]
    pub fn with_cloud_rerun_on_mismatch(mut self, on: bool) -> Self {
        self.cloud_rerun_on_mismatch = on;
        self
    }

    /// Builder: inject a specific language cache (tests + bench). The
    /// default constructor uses `LanguageCache::global()`.
    #[must_use]
    pub fn with_lang_cache(mut self, cache: Arc<LanguageCache>) -> Self {
        self.lang_cache = cache;
        self
    }

    /// Builder: per-language initial-prompt map. Sent as the cloud
    /// `prompt` form field when the resolved language has a key. Empty
    /// map = no prompts (default behaviour).
    #[must_use]
    pub fn with_prompts(mut self, prompts: std::collections::HashMap<String, String>) -> Self {
        self.prompts = prompts;
        self
    }

    /// Resolve the prompt for a known language; `None` if unknown
    /// (cold-start auto-detect — sending a prompt then would bias the
    /// language classifier).
    fn resolve_prompt(&self, lang: Option<&str>) -> Option<&str> {
        self.prompts.get(lang?).map(String::as_str)
    }

    fn effective_selection(&self, lang_override: Option<&str>) -> LanguageSelection {
        LanguageSelection::from_config(&self.languages).with_override(lang_override)
    }
}

/// Build a `reqwest::Client` tuned for low-latency reuse across many
/// short requests (latency plan L3): HTTP/2 keep-alive, idle pool kept
/// hot for a minute, generous-but-bounded request timeout.
pub(crate) fn warm_client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_idle_timeout(std::time::Duration::from_secs(60))
        .pool_max_idle_per_host(4)
        .tcp_keepalive(Some(std::time::Duration::from_secs(30)))
        .http2_keep_alive_interval(Some(std::time::Duration::from_secs(20)))
        .http2_keep_alive_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(45))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default()
}

#[derive(Deserialize, Debug, Clone)]
pub struct GroqResponse {
    pub text: String,
    #[serde(default)]
    pub language: Option<String>,
}

/// `response_format=verbose_json` shape — used by the rerun lane to
/// score candidate peers by mean per-segment `avg_logprob`. The outer
/// `language` field is intentionally ignored: when we force `language=`
/// the peer code we sent IS the code we record on a winning rerun, so
/// the verbose echo (which is the full English name like "english",
/// not the alpha-2 code) would only confuse `LanguageSelection::contains`.
#[derive(Deserialize, Debug, Clone)]
pub struct GroqVerboseResponse {
    pub text: String,
    #[serde(default)]
    pub segments: Vec<GroqSegment>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct GroqSegment {
    #[serde(default)]
    pub avg_logprob: Option<f32>,
}

impl GroqVerboseResponse {
    /// Mean per-segment `avg_logprob`. Returns `f32::NEG_INFINITY` when
    /// the response carries no segments (very short clip, parser drift)
    /// so this candidate loses every tiebreak and we fall through to
    /// the next peer. Negative-infinity rather than 0.0 because a
    /// real Whisper score is always ≤ 0; using 0.0 would make a missing
    /// score artificially win.
    pub fn mean_logprob(&self) -> f32 {
        let scored: Vec<f32> = self.segments.iter().filter_map(|s| s.avg_logprob).collect();
        if scored.is_empty() {
            f32::NEG_INFINITY
        } else {
            scored.iter().sum::<f32>() / scored.len() as f32
        }
    }
}

/// Issue a single transcription request to Groq's batch endpoint.
/// Shared between [`GroqStt`] (batch) and the streaming pseudo-stream
/// backend in [`crate::groq_streaming`] (re-POSTs the trailing N
/// seconds every 700 ms). The caller resolves the model + language
/// allow-list semantics; this helper just does the multipart POST and
/// parses the JSON.
pub(crate) async fn groq_post_wav(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    wav: &[u8],
    lang: Option<&str>,
    prompt: Option<&str>,
) -> Result<GroqResponse> {
    let part = multipart::Part::bytes(wav.to_vec())
        .file_name("audio.wav")
        .mime_str("audio/wav")?;
    let mut form = multipart::Form::new()
        .text("model", model.to_string())
        .text("response_format", "verbose_json")
        .part("file", part);
    if let Some(l) = lang {
        form = form.text("language", l.to_string());
    }
    if let Some(p) = prompt {
        form = form.text("prompt", p.to_string());
    }
    let res = client
        .post(GROQ_ENDPOINT)
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .context("groq POST failed")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        if status.as_u16() == 429 {
            let summary = summarise_429(&body);
            tracing::warn!("groq rate-limited (429): {summary}. {RATE_LIMIT_HINT}");
            crate::rate_limit_notify::mark_rate_limited();
            crate::rate_limit_notify::notify_once("groq", RATE_LIMIT_HINT);
        }
        anyhow::bail!("groq STT {status}: {body}");
    }
    serde_json::from_str(&body).with_context(|| format!("parse groq response: {body}"))
}

/// `verbose_json` variant of [`groq_post_wav`]. Used by the rerun lane
/// to obtain per-segment `avg_logprob` scores so we can pick the peer
/// most likely to be the spoken language.
pub async fn groq_post_wav_verbose(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    wav: &[u8],
    lang: Option<&str>,
    prompt: Option<&str>,
) -> Result<GroqVerboseResponse> {
    let part = multipart::Part::bytes(wav.to_vec())
        .file_name("audio.wav")
        .mime_str("audio/wav")?;
    let mut form = multipart::Form::new()
        .text("model", model.to_string())
        .text("response_format", "verbose_json")
        .part("file", part);
    if let Some(l) = lang {
        form = form.text("language", l.to_string());
    }
    if let Some(p) = prompt {
        form = form.text("prompt", p.to_string());
    }
    let res = client
        .post(GROQ_ENDPOINT)
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .context("groq verbose POST failed")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        if status.as_u16() == 429 {
            let summary = summarise_429(&body);
            tracing::warn!("groq verbose rate-limited (429): {summary}. {RATE_LIMIT_HINT}");
            crate::rate_limit_notify::mark_rate_limited();
            crate::rate_limit_notify::notify_once("groq", RATE_LIMIT_HINT);
        }
        anyhow::bail!("groq STT verbose {status}: {body}");
    }
    serde_json::from_str(&body).with_context(|| format!("parse groq verbose response: {body}"))
}

#[async_trait]
impl SpeechToText for GroqStt {
    async fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
        lang: Option<&str>,
    ) -> Result<Transcription> {
        let wav = encode_wav(pcm, sample_rate);
        let selection = self.effective_selection(lang);

        // First-pass language: forced -> the code; auto -> none;
        // allow-list -> none (let cloud auto-detect, then
        // post-validate against the allow-list).
        let first_pass_lang: Option<String> = match &selection {
            LanguageSelection::Auto => None,
            LanguageSelection::Forced(c) => Some(c.clone()),
            LanguageSelection::AllowList(_) => None,
        };

        let parsed = self.do_request(&wav, first_pass_lang.as_deref()).await?;

        // Post-validate against the allow-list. v3.1: confidence-aware
        // rerun. On in-list detection, record. On banned detection
        // **and** rerun enabled, issue one verbose_json request per
        // peer and pick the one with the highest mean per-segment
        // `avg_logprob` (the standard Whisper "this is the language
        // I'm most confident about" signal). This handles cold-start
        // (cache empty) and warm-but-wrong-cache cases uniformly:
        // confidence picks the right peer even when the cache holds
        // the wrong code from a previous topic.
        if let LanguageSelection::AllowList(peers) = &selection {
            if let Some(detected_raw) = parsed.language.as_deref() {
                // verbose_json echoes the full English name ("english",
                // "bulgarian"); the allow-list is alpha-2. Normalise.
                let detected = crate::lang::whisper_lang_to_code(detected_raw);
                if selection.contains(&detected) {
                    self.lang_cache.record(BACKEND_KEY, &detected);
                } else if self.cloud_rerun_on_mismatch {
                    tracing::info!(
                        "groq returned banned language {detected_raw:?} (normalised \
                         {detected:?}, allow-list {:?}); reranking by per-peer avg_logprob",
                        self.languages
                    );
                    if let Some((picked, resp)) = self.pick_best_peer(&wav, peers).await {
                        self.lang_cache.record(BACKEND_KEY, &picked);
                        return Ok(Transcription {
                            text: resp.text,
                            language: Some(picked),
                            duration_ms: None,
                        });
                    }
                    tracing::warn!(
                        "groq rerun: every peer attempt failed; \
                         falling back to unforced response"
                    );
                } else {
                    tracing::info!("groq detected banned language {detected_raw:?} (normalised {detected:?}); rerun disabled");
                }
            }
        }

        Ok(Transcription {
            text: parsed.text,
            language: parsed.language,
            duration_ms: None,
        })
    }

    fn name(&self) -> &'static str {
        "groq"
    }

    async fn prewarm(&self) -> Result<()> {
        // GET /v1/models is a cheap authed call; warms TCP+TLS so the
        // first real request doesn't pay handshake latency.
        let res = self
            .client
            .get(GROQ_MODELS_ENDPOINT)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .context("groq prewarm")?;
        let _ = res.bytes().await; // drain so the connection returns to the pool
        Ok(())
    }
}

impl GroqStt {
    /// Issue a single transcription request to Groq with the given
    /// optional `language` form field. Factored out so the
    /// post-validation rerun path is one extra await away.
    async fn do_request(&self, wav: &[u8], lang: Option<&str>) -> Result<GroqResponse> {
        let prompt = self.resolve_prompt(lang);
        groq_post_wav(&self.client, &self.api_key, &self.model, wav, lang, prompt).await
    }

    /// `verbose_json` variant for the rerun lane.
    async fn do_request_verbose(
        &self,
        wav: &[u8],
        lang: Option<&str>,
    ) -> Result<GroqVerboseResponse> {
        let prompt = self.resolve_prompt(lang);
        groq_post_wav_verbose(&self.client, &self.api_key, &self.model, wav, lang, prompt).await
    }

    /// Run one verbose request per peer, pick the response with the
    /// highest mean per-segment `avg_logprob`. Returns `Some((code,
    /// resp))` on success, `None` only when every peer attempt errors.
    /// On the rare tie or empty-segments edge case, the first peer in
    /// `peers` wins (since `>` not `>=`).
    pub(crate) async fn pick_best_peer(
        &self,
        wav: &[u8],
        peers: &[String],
    ) -> Option<(String, GroqVerboseResponse)> {
        let mut best: Option<(f32, String, GroqVerboseResponse)> = None;
        for peer in peers {
            match self.do_request_verbose(wav, Some(peer)).await {
                Ok(r) => {
                    let score = r.mean_logprob();
                    tracing::info!("groq rerun candidate language={peer}: avg_logprob={score:.3}");
                    if best.as_ref().is_none_or(|(s, _, _)| score > *s) {
                        best = Some((score, peer.clone(), r));
                    }
                }
                Err(e) => {
                    tracing::warn!("groq rerun candidate language={peer} failed: {e:#}");
                }
            }
        }
        best.map(|(_, code, resp)| (code, resp))
    }
}

/// Encode mono f32 samples as a 16-bit PCM WAV blob.
pub fn encode_wav(pcm: &[f32], sample_rate: u32) -> Vec<u8> {
    let num_samples = pcm.len() as u32;
    let byte_rate = sample_rate * 2;
    let data_size = num_samples * 2;
    let mut out = Vec::with_capacity(44 + data_size as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_size).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_size.to_le_bytes());
    for s in pcm {
        let clamped = s.clamp(-1.0, 1.0);
        let i = (clamped * i16::MAX as f32) as i16;
        out.extend_from_slice(&i.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_is_44_bytes() {
        let blob = encode_wav(&[0.0; 16], 16_000);
        assert_eq!(&blob[..4], b"RIFF");
        assert_eq!(&blob[8..12], b"WAVE");
        assert_eq!(blob.len(), 44 + 32);
    }
}
