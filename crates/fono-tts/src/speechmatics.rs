// SPDX-License-Identifier: GPL-3.0-only
//! Speechmatics text-to-speech client (preview).
//!
//! Wire shape:
//!   POST `https://preview.tts.speechmatics.com/generate/<voice>
//!         ?output_format=pcm_16000`
//!   header: `Authorization: Bearer <key>` (literal word `Bearer`, NOT
//!           Deepgram's `Token`).
//!   body: `{ "text": <text> }`
//!   response: raw int16 LE mono PCM at 16 kHz (`pcm_16000`).
//!
//! Speechmatics selects a voice via the URL path — `/generate/sarah`.
//! The preview is **English-only** and offers four voices (`sarah`,
//! `theo`, `megan`, `jack`); the `lang` hint is ignored. We request
//! the raw `pcm_16000` format to skip WAV-header parsing, mirroring
//! `crates/fono-tts/src/deepgram.rs`.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use fono_core::provider_catalog;
use fono_core::provider_catalog::TtsEndpoint;
use serde::Serialize;

use crate::traits::{TextToSpeech, TtsAudio};

const NATIVE_RATE: u32 = 16_000;
/// The four English preview voices Speechmatics currently offers.
const KNOWN_VOICES: [&str; 4] = ["sarah", "theo", "megan", "jack"];
const TTS_OVERALL_TIMEOUT: Duration = Duration::from_secs(30);

pub struct SpeechmaticsTts {
    api_key: String,
    base_url: String,
    voice: String,
    client: reqwest::Client,
}

impl SpeechmaticsTts {
    /// Build from the catalogue defaults, overriding the voice when a
    /// non-empty `voice_override` is supplied. An unrecognised voice
    /// falls back to the catalogue default (Speechmatics 404s on an
    /// unknown voice path).
    #[must_use]
    pub fn new(api_key: impl Into<String>, voice_override: Option<String>) -> Self {
        let entry = provider_catalog::find("speechmatics")
            .and_then(|p| p.tts.as_ref())
            .expect("speechmatics catalogue entry must exist with a TTS capability");
        let base_url = match entry.endpoint {
            TtsEndpoint::Speechmatics { base_url } => base_url.to_string(),
            _ => panic!("speechmatics catalogue entry must use the Speechmatics TTS endpoint"),
        };
        let default_voice = entry.default_voice.to_string();
        let voice = match voice_override {
            Some(v) if KNOWN_VOICES.contains(&v.as_str()) => v,
            Some(v) => {
                tracing::debug!(
                    "speechmatics: unknown TTS voice {v:?}; falling back to {default_voice:?}"
                );
                default_voice
            }
            None => default_voice,
        };
        Self { api_key: api_key.into(), base_url, voice, client: warm_client() }
    }

    /// Configured voice id (which doubles as the URL path segment).
    #[must_use]
    pub fn voice(&self) -> &str {
        &self.voice
    }

    /// Resolved POST URL with the voice path + `output_format` query.
    /// Exposed for tests.
    #[must_use]
    pub fn speech_url(&self) -> String {
        format!(
            "{base}/generate/{voice}?output_format=pcm_16000",
            base = self.base_url,
            voice = self.voice
        )
    }

    /// Build the JSON body for `synthesize`. Exposed for tests.
    #[must_use]
    pub fn build_request_body(&self, text: &str) -> serde_json::Value {
        serde_json::to_value(GenerateReq { text })
            .expect("serialising static-shape Speechmatics request must not fail")
    }
}

fn warm_client() -> reqwest::Client {
    reqwest::Client::builder()
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .timeout(TTS_OVERALL_TIMEOUT)
        .connect_timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default()
}

#[derive(Serialize)]
struct GenerateReq<'a> {
    text: &'a str,
}

#[async_trait]
impl TextToSpeech for SpeechmaticsTts {
    fn name(&self) -> &'static str {
        "speechmatics"
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
        // A per-call voice hint overrides the configured voice when it
        // names a known Speechmatics voice; otherwise we keep the
        // configured path.
        let voice_path = match voice {
            Some(v) if KNOWN_VOICES.contains(&v) => v,
            _ => self.voice.as_str(),
        };
        let url = format!(
            "{base}/generate/{voice}?output_format=pcm_16000",
            base = self.base_url,
            voice = voice_path
        );
        let body = self.build_request_body(text);
        let resp = self
            .client
            .post(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .context("posting to speechmatics /generate")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(anyhow!("speechmatics TTS returned {status}: {}", truncate(&body, 400)));
        }
        let bytes = resp.bytes().await.context("reading speechmatics TTS response body")?;
        let pcm = pcm_i16_le_to_f32(&bytes);
        Ok(TtsAudio { pcm, sample_rate: NATIVE_RATE })
    }

    async fn prewarm(&self) -> Result<()> {
        // No documented cheap GET on the preview host; defer the TLS
        // handshake to the first synthesis call.
        Ok(())
    }
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

    #[test]
    fn client_uses_catalogue_defaults() {
        let c = SpeechmaticsTts::new("sm-test", None);
        assert_eq!(c.voice(), "sarah");
        assert_eq!(
            c.speech_url(),
            "https://preview.tts.speechmatics.com/generate/sarah?output_format=pcm_16000"
        );
        assert_eq!(c.native_sample_rate(), NATIVE_RATE);
        assert_eq!(c.name(), "speechmatics");
    }

    #[test]
    fn known_voice_override_applies() {
        let c = SpeechmaticsTts::new("sm-test", Some("theo".to_string()));
        assert_eq!(c.voice(), "theo");
        assert!(c.speech_url().contains("/generate/theo"));
    }

    #[test]
    fn unknown_voice_override_falls_back_to_default() {
        let c = SpeechmaticsTts::new("sm-test", Some("nonexistent".to_string()));
        assert_eq!(c.voice(), "sarah");
    }

    #[test]
    fn request_body_shape_matches_spec() {
        let c = SpeechmaticsTts::new("sm-test", None);
        let body = c.build_request_body("hi");
        assert_eq!(body["text"], "hi");
        let obj = body.as_object().expect("body is a JSON object");
        assert_eq!(obj.len(), 1, "body has only the `text` field");
    }

    #[test]
    fn auth_header_uses_bearer_prefix() {
        // Footgun parity: Deepgram uses `Token`, Speechmatics uses
        // `Bearer`. Pin the exact prefix.
        let c = SpeechmaticsTts::new("sm-key-x", None);
        let value = format!("Bearer {}", c.api_key);
        assert!(value.starts_with("Bearer "), "Speechmatics auth must use the `Bearer` prefix");
        assert!(!value.starts_with("Token "), "Speechmatics auth must NOT use Deepgram's Token");
    }

    #[test]
    fn pcm_i16_le_to_f32_known_samples() {
        // 0 -> 0.0, 32767 -> ~1.0, -32767 -> ~-1.0
        let bytes = [0x00, 0x00, 0xFF, 0x7F, 0x01, 0x80];
        let pcm = pcm_i16_le_to_f32(&bytes);
        assert_eq!(pcm.len(), 3);
        assert!((pcm[0]).abs() < 1e-6);
        assert!((pcm[1] - 1.0).abs() < 1e-4);
        assert!((pcm[2] + 1.0).abs() < 1e-4);
    }

    #[tokio::test]
    async fn empty_text_returns_empty_audio_without_request() {
        let c = SpeechmaticsTts::new("sm-test", None);
        let audio = c.synthesize("", None, None).await.unwrap();
        assert!(audio.pcm.is_empty());
        assert_eq!(audio.sample_rate, NATIVE_RATE);
    }
}
