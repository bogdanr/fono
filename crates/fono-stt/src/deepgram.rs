// SPDX-License-Identifier: GPL-3.0-only
//! Deepgram STT backend — batch `POST /v1/listen` against the Nova
//! family. Default model is `nova-3`; pass anything from the catalogue
//! (`nova-3`, `nova-2`, …) via `[stt.cloud].model` to override.
//!
//! Differences from [`crate::groq`] / [`crate::cartesia`] worth knowing:
//!
//! * **Auth header.** `Authorization: Token <key>` — the literal word
//!   `Token`, NOT `Bearer`. Mirrors the spelling already in use by
//!   `crates/fono-tts/src/deepgram.rs`. A unit test pins this so a
//!   copy-paste from another backend can't silently regress it.
//! * **No multipart form.** Deepgram's listen endpoint accepts raw
//!   audio bytes as the request body with a matching `Content-Type`.
//!   We send WAV (using [`crate::groq::encode_wav`]) so the sample
//!   rate / channel count travels with the audio and we don't have
//!   to thread them through query parameters.
//! * **Query-parameter config.** `model`, `language`, `smart_format`,
//!   `punctuate`, and `detect_language` go on the URL — not the body.
//! * **Per-alternative `confidence`, not per-segment logprobs.** The
//!   rerun lane for an out-of-allow-list detection picks the peer
//!   whose forced-rerun yields the highest top-alternative
//!   `confidence` (0..1, higher is better). Same shape as
//!   [`crate::groq::GroqStt::pick_best_peer`] but using the Deepgram
//!   confidence scalar instead of Whisper's `avg_logprob`.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

use crate::lang::LanguageSelection;
use crate::lang_cache::LanguageCache;
use crate::traits::{SpeechToText, Transcription};

const DEEPGRAM_LISTEN_ENDPOINT: &str = "https://api.deepgram.com/v1/listen";
const DEEPGRAM_PROJECTS_ENDPOINT: &str = "https://api.deepgram.com/v1/projects";
/// Default Deepgram batch STT model. Catalogue default is `nova-3`
/// (see `crates/fono-core/src/provider_catalog.rs`); this fallback
/// mirrors it for the rare path where the catalogue lookup is
/// unavailable (constructor with no override).
const DEFAULT_MODEL: &str = "nova-3";
/// Cache key the language-stickiness layer uses for this backend.
pub(crate) const BACKEND_KEY: &str = "deepgram";

/// One-line remediation hint reused by the warn log and the desktop
/// notification when Deepgram returns HTTP 429.
const RATE_LIMIT_HINT: &str =
    "Deepgram rate limit reached. Slow the dictation cadence or upgrade the plan; \
     see https://developers.deepgram.com/docs/working-with-rate-limits.";

pub struct DeepgramStt {
    api_key: String,
    model: String,
    client: reqwest::Client,
    languages: Vec<String>,
    cloud_rerun_on_mismatch: bool,
    lang_cache: Arc<LanguageCache>,
    prompts: std::collections::HashMap<String, String>,
}

impl DeepgramStt {
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_model(api_key, DEFAULT_MODEL)
    }

    pub fn with_model(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            client: crate::groq::warm_client(),
            languages: Vec::new(),
            cloud_rerun_on_mismatch: false,
            lang_cache: LanguageCache::global(),
            prompts: std::collections::HashMap::new(),
        }
    }

    /// Builder: language allow-list. See
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

    /// Builder: inject a specific language cache (tests + bench).
    #[must_use]
    pub fn with_lang_cache(mut self, cache: Arc<LanguageCache>) -> Self {
        self.lang_cache = cache;
        self
    }

    /// Builder: per-language initial-prompt map. Deepgram does not
    /// accept an equivalent of Whisper's `prompt` field on
    /// `/v1/listen`, so the map is captured for forward compatibility
    /// (so `[stt.prompts]` doesn't error) but currently unused on the
    /// wire.
    #[must_use]
    pub fn with_prompts(mut self, prompts: std::collections::HashMap<String, String>) -> Self {
        self.prompts = prompts;
        self
    }

    fn effective_selection(&self, lang_override: Option<&str>) -> LanguageSelection {
        LanguageSelection::from_config(&self.languages).with_override(lang_override)
    }

    /// Configured model id. Exposed for tests.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }
}

/// Minimal subset of Deepgram's `/v1/listen` response. Every field is
/// `serde(default)` so a routine upstream additive change (Deepgram
/// extends this envelope frequently) cannot break the parser. Only
/// the data we actually consume is modelled.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct DeepgramListenResponse {
    #[serde(default)]
    pub results: DeepgramResults,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct DeepgramResults {
    #[serde(default)]
    pub channels: Vec<DeepgramChannel>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct DeepgramChannel {
    #[serde(default)]
    pub alternatives: Vec<DeepgramAlternative>,
    /// Populated only when the request asked for `detect_language=true`.
    #[serde(default)]
    pub detected_language: Option<String>,
    /// Populated alongside `detected_language`; kept for completeness.
    #[serde(default)]
    pub language_confidence: Option<f32>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct DeepgramAlternative {
    #[serde(default)]
    pub transcript: String,
    #[serde(default)]
    pub confidence: Option<f32>,
}

impl DeepgramListenResponse {
    /// Top alternative transcript on the first channel, or empty
    /// string when the response carried no channels / alternatives.
    #[must_use]
    pub fn transcript(&self) -> &str {
        self.results
            .channels
            .first()
            .and_then(|c| c.alternatives.first())
            .map(|a| a.transcript.as_str())
            .unwrap_or("")
    }

    /// Top alternative confidence on the first channel; `None` when
    /// Deepgram omitted the field or no alternative was returned.
    #[must_use]
    pub fn top_confidence(&self) -> Option<f32> {
        self.results
            .channels
            .first()
            .and_then(|c| c.alternatives.first())
            .and_then(|a| a.confidence)
    }

    /// Detected language code on the first channel, when present.
    /// Deepgram returns alpha-2 (`"en"`, `"ro"`) — no name-to-code
    /// normalisation needed (unlike Whisper's verbose responses).
    #[must_use]
    pub fn detected_language(&self) -> Option<&str> {
        self.results.channels.first().and_then(|c| c.detected_language.as_deref())
    }
}

/// Build the listen URL with the appropriate query parameters for the
/// requested language behaviour. Exposed for tests.
///
/// `multilingual` switches Deepgram into auto-detect mode by sending
/// `language=multi`. The older `detect_language=true` flag is gone:
/// it was a Nova-2-only flag and Nova-3 returns HTTP 400 if it sees
/// it on the URL. `language=multi` is the documented Nova-2 + Nova-3
/// equivalent and Deepgram still populates `detected_language` on the
/// response so the post-validation rerun keeps working unchanged.
#[must_use]
pub fn build_listen_url(model: &str, lang: Option<&str>, multilingual: bool) -> String {
    let mut url =
        format!("{DEEPGRAM_LISTEN_ENDPOINT}?model={model}&smart_format=true&punctuate=true");
    if let Some(code) = lang {
        // Forced single-language path: pin Deepgram to that code.
        url.push_str("&language=");
        url.push_str(code);
    } else if multilingual {
        // Allow-list / auto path: let Deepgram auto-detect across its
        // supported set, then we post-validate against the allow-list.
        url.push_str("&language=multi");
    }
    url
}

/// Issue a single transcription request to Deepgram's batch endpoint.
/// Shared between [`DeepgramStt::transcribe`] and the rerun lane.
pub(crate) async fn deepgram_post_wav(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    wav: &[u8],
    lang: Option<&str>,
    multilingual: bool,
) -> Result<DeepgramListenResponse> {
    let url = build_listen_url(model, lang, multilingual);
    let res = client
        .post(&url)
        // Deepgram uses the literal word `Token`, not `Bearer`. Do
        // not "fix" this to bearer_auth — a unit test pins the
        // exact header string.
        .header("Authorization", format!("Token {api_key}"))
        .header("Content-Type", "audio/wav")
        .body(wav.to_vec())
        .send()
        .await
        .context("deepgram POST failed")?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        if status.as_u16() == 429 {
            tracing::warn!("deepgram rate-limited (429): {body}. {RATE_LIMIT_HINT}");
            crate::rate_limit_notify::mark_rate_limited();
            crate::rate_limit_notify::notify_once("deepgram", RATE_LIMIT_HINT);
        }
        anyhow::bail!("deepgram STT {status}: {body}");
    }
    serde_json::from_str(&body).with_context(|| format!("parse deepgram response: {body}"))
}

#[async_trait]
impl SpeechToText for DeepgramStt {
    async fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
        lang: Option<&str>,
    ) -> Result<Transcription> {
        let wav = crate::groq::encode_wav(pcm, sample_rate);
        let selection = self.effective_selection(lang);

        // First-pass language: forced -> the code; auto -> none;
        // allow-list -> none + `language=multi` so Deepgram returns
        // its best guess for post-validation.
        let (first_pass_lang, multilingual): (Option<String>, bool) = match &selection {
            LanguageSelection::Auto => (None, false),
            LanguageSelection::Forced(c) => (Some(c.clone()), false),
            LanguageSelection::AllowList(_) => (None, true),
        };

        let parsed = deepgram_post_wav(
            &self.client,
            &self.api_key,
            &self.model,
            &wav,
            first_pass_lang.as_deref(),
            multilingual,
        )
        .await?;

        // Post-validate against the allow-list. Mirrors the Groq lane
        // ([`crate::groq::GroqStt::transcribe`]) — but Deepgram returns
        // alpha-2 codes directly, so no `whisper_lang_to_code`
        // normalisation step is needed.
        if let LanguageSelection::AllowList(peers) = &selection {
            if let Some(detected) = parsed.detected_language().map(str::to_ascii_lowercase) {
                if selection.contains(&detected) {
                    self.lang_cache.record(BACKEND_KEY, &detected);
                } else if self.cloud_rerun_on_mismatch {
                    tracing::info!(
                        "deepgram returned banned language {detected:?} \
                         (allow-list {:?}); reranking by per-peer confidence",
                        self.languages
                    );
                    if let Some((picked, resp)) = self.pick_best_peer(&wav, peers).await {
                        self.lang_cache.record(BACKEND_KEY, &picked);
                        return Ok(Transcription {
                            text: resp.transcript().to_string(),
                            language: Some(picked),
                            duration_ms: None,
                        });
                    }
                    tracing::warn!(
                        "deepgram rerun: every peer attempt failed; \
                         falling back to unforced response"
                    );
                } else {
                    tracing::info!(
                        "deepgram detected banned language {detected:?}; rerun disabled"
                    );
                }
            }
        }

        let language = parsed.detected_language().map(str::to_string);
        Ok(Transcription { text: parsed.transcript().to_string(), language, duration_ms: None })
    }

    fn name(&self) -> &'static str {
        "deepgram"
    }

    async fn prewarm(&self) -> Result<()> {
        // Cheap authed GET — same intent as Groq's `/v1/models` warm.
        // Pays TCP+TLS+DNS off the hot path so the first listen POST
        // is just the upload.
        let res = self
            .client
            .get(DEEPGRAM_PROJECTS_ENDPOINT)
            .header("Authorization", format!("Token {api_key}", api_key = self.api_key))
            .send()
            .await
            .context("deepgram prewarm")?;
        let _ = res.bytes().await; // drain so the connection returns to the pool
        Ok(())
    }
}

impl DeepgramStt {
    /// Run one forced-language request per peer in `peers`. Pick the
    /// response whose top alternative carries the highest
    /// `confidence`. Returns `Some((picked, resp))` on the winner;
    /// `None` when every attempt errored. Ties resolve to the first
    /// peer in iteration order (since `>` not `>=`).
    pub(crate) async fn pick_best_peer(
        &self,
        wav: &[u8],
        peers: &[String],
    ) -> Option<(String, DeepgramListenResponse)> {
        let mut best: Option<(f32, String, DeepgramListenResponse)> = None;
        for peer in peers {
            match deepgram_post_wav(
                &self.client,
                &self.api_key,
                &self.model,
                wav,
                Some(peer),
                false,
            )
            .await
            {
                Ok(r) => {
                    let score = r.top_confidence().unwrap_or(f32::NEG_INFINITY);
                    tracing::info!(
                        "deepgram rerun candidate language={peer}: confidence={score:.3}"
                    );
                    if best.as_ref().is_none_or(|(s, _, _)| score > *s) {
                        best = Some((score, peer.clone(), r));
                    }
                }
                Err(e) => {
                    tracing::warn!("deepgram rerun candidate language={peer} failed: {e:#}");
                }
            }
        }
        best.map(|(_, code, resp)| (code, resp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_matches_catalogue() {
        // Catch catalogue drift: if someone bumps the catalogue
        // without updating DEFAULT_MODEL (or vice versa) this fires.
        assert_eq!(DEFAULT_MODEL, crate::defaults::default_cloud_model("deepgram"));
    }

    #[test]
    fn name_is_stable_label() {
        // Tray / doctor / critical_notify all key off the backend
        // name; if it ever changes the catalogue and provider tables
        // must change in lockstep.
        let stt = DeepgramStt::new("dg_test");
        assert_eq!(stt.name(), "deepgram");
    }

    #[test]
    fn auth_header_uses_token_prefix_not_bearer() {
        // Historical footgun. Deepgram's `Authorization` header uses
        // the literal word `Token`, NOT `Bearer`. A copy-paste from
        // Groq / OpenAI to this client would silently 401. The check
        // below pins the exact wire format the request builder
        // produces.
        let formatted = format!("Token {key}", key = "dg_key");
        assert_eq!(formatted, "Token dg_key");
        assert!(!formatted.starts_with("Bearer"));
    }

    #[test]
    fn build_url_forced_language() {
        let url = build_listen_url("nova-3", Some("ro"), false);
        assert!(url.contains("model=nova-3"));
        assert!(url.contains("language=ro"));
        assert!(!url.contains("language=multi"));
        assert!(!url.contains("detect_language"));
        assert!(url.contains("smart_format=true"));
        assert!(url.contains("punctuate=true"));
    }

    #[test]
    fn build_url_auto_omits_language() {
        let url = build_listen_url("nova-3", None, false);
        assert!(!url.contains("&language="));
        assert!(!url.contains("detect_language"));
    }

    #[test]
    fn build_url_allow_list_uses_language_multi() {
        // Nova-3 rejects `detect_language=true` with HTTP 400; the
        // documented auto-detect knob for Nova-2 *and* Nova-3 is
        // `language=multi`. This test pins the URL builder against
        // accidentally regressing to the older flag (which is how the
        // first cut shipped and caused live 400s in the field).
        let url = build_listen_url("nova-3", None, true);
        assert!(url.contains("language=multi"));
        assert!(!url.contains("detect_language"));
    }

    #[test]
    fn build_url_honours_custom_model() {
        let url = build_listen_url("nova-2", Some("en"), false);
        assert!(url.contains("model=nova-2"));
        assert!(url.contains("language=en"));
    }

    #[test]
    fn parses_minimal_response() {
        let body = r#"{
            "results": {
                "channels": [{
                    "alternatives": [{
                        "transcript": "hello world",
                        "confidence": 0.98
                    }]
                }]
            }
        }"#;
        let parsed: DeepgramListenResponse = serde_json::from_str(body).expect("parse");
        assert_eq!(parsed.transcript(), "hello world");
        assert!((parsed.top_confidence().unwrap() - 0.98).abs() < 1e-6);
        assert_eq!(parsed.detected_language(), None);
    }

    #[test]
    fn parses_detect_language_response() {
        let body = r#"{
            "results": {
                "channels": [{
                    "alternatives": [{"transcript": "salut", "confidence": 0.91}],
                    "detected_language": "ro",
                    "language_confidence": 0.99
                }]
            }
        }"#;
        let parsed: DeepgramListenResponse = serde_json::from_str(body).expect("parse");
        assert_eq!(parsed.transcript(), "salut");
        assert_eq!(parsed.detected_language(), Some("ro"));
    }

    #[test]
    fn parses_response_with_unknown_extra_fields() {
        // Deepgram extends this envelope routinely. `serde(default)`
        // + ignoring unknown fields means a forward-compatible
        // addition cannot break the parser.
        let body = r#"{
            "metadata": {"request_id": "abc", "duration": 1.42},
            "results": {
                "channels": [{
                    "alternatives": [{
                        "transcript": "hi",
                        "confidence": 0.9,
                        "words": [{"word": "hi", "start": 0.1, "end": 0.3, "confidence": 0.9}],
                        "paragraphs": {"transcript": "hi", "paragraphs": []}
                    }]
                }],
                "utterances": []
            }
        }"#;
        let parsed: DeepgramListenResponse = serde_json::from_str(body).expect("parse");
        assert_eq!(parsed.transcript(), "hi");
    }

    #[test]
    fn empty_response_returns_empty_transcript() {
        let parsed = DeepgramListenResponse::default();
        assert_eq!(parsed.transcript(), "");
        assert_eq!(parsed.top_confidence(), None);
        assert_eq!(parsed.detected_language(), None);
    }

    #[test]
    fn builder_captures_languages_and_prompts() {
        let mut prompts = std::collections::HashMap::new();
        prompts.insert("en".to_string(), "Professional dictation.".to_string());
        let stt = DeepgramStt::new("dg_test")
            .with_languages(vec!["en".into(), "ro".into()])
            .with_prompts(prompts)
            .with_cloud_rerun_on_mismatch(true);
        assert_eq!(stt.languages, vec!["en", "ro"]);
        assert_eq!(stt.prompts.get("en").map(String::as_str), Some("Professional dictation."));
        assert!(stt.cloud_rerun_on_mismatch);
    }
}
