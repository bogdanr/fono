// SPDX-License-Identifier: GPL-3.0-only
//! ElevenLabs `POST /v1/text-to-speech/<voice_id>` client, targeting
//! the expressive **Eleven v3** model.
//!
//! Wire shape:
//!   POST `https://api.elevenlabs.io/v1/text-to-speech/<voice_id>
//!         ?output_format=pcm_24000`
//!   header: `xi-api-key: <key>`
//!   body: `{ "text": <text>, "model_id": "eleven_v3" }`
//!   response: raw int16 LE mono PCM at 24 kHz.
//!
//! ElevenLabs selects the speaker via the **voice id in the path**
//! (not a body / query field). The catalogue stores `default_voice`
//! ("Sarah", `EXAVITQu4vr4xnSDxMaL`) — a current *premade* multilingual
//! voice usable on every plan, free tier included — and the user can
//! pin a different voice id via `[tts].voice`.
//!
//! Eleven v3 supports inline *audio tags* (e.g. `[whispers]`,
//! `[laughs]`) and IPA pronunciation hints for emotional / phonetic
//! control. Fono posts plain dictation text and adds none of these
//! itself; a user driving the assistant path can type tags into the
//! reply and they pass through verbatim. See the v3 best-practices
//! (`docs/providers.md`) for the available tags.
//!
//! Voice availability is plan-gated: *library* voices (anything you
//! add from the shared voice library, plus legacy voices like the old
//! "Rachel") and *professional* voices reject free-tier keys with HTTP
//! 402 `paid_plan_required`. Current *premade* voices (e.g. Sarah)
//! work on every plan. This is documented in `docs/providers.md`.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use fono_core::provider_catalog;
use serde::Serialize;

use crate::traits::{TextToSpeech, TtsAudio};

const NATIVE_RATE: u32 = 24_000;
const BASE_ENDPOINT: &str = "https://api.elevenlabs.io/v1/text-to-speech";

pub struct ElevenLabsTts {
    api_key: String,
    model: String,
    /// Voice id baked into the request path. Catalogue default
    /// ("Rachel") unless the user pinned `[tts].voice`.
    voice_id: String,
    client: reqwest::Client,
}

impl ElevenLabsTts {
    /// Build a client using the catalogue defaults for model / voice,
    /// overridable per field.
    #[must_use]
    pub fn new(
        api_key: impl Into<String>,
        model_override: Option<String>,
        voice_override: Option<String>,
    ) -> Self {
        let entry = provider_catalog::find("elevenlabs")
            .and_then(|p| p.tts.as_ref())
            .expect("elevenlabs catalogue entry must exist with a TTS capability");
        Self {
            api_key: api_key.into(),
            model: model_override.unwrap_or_else(|| entry.model.to_string()),
            voice_id: voice_override.unwrap_or_else(|| entry.default_voice.to_string()),
            client: crate::openai_compat::warm_client(),
        }
    }

    /// Configured model id. Exposed for tests.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Configured (fallback) voice id. Exposed for tests.
    #[must_use]
    pub fn voice_id(&self) -> &str {
        &self.voice_id
    }

    /// Resolved POST URL for `voice_id`, with the raw-PCM output
    /// format query baked in. Exposed for tests.
    #[must_use]
    pub fn speech_url(&self, voice_id: &str) -> String {
        format!("{BASE_ENDPOINT}/{voice_id}?output_format=pcm_{NATIVE_RATE}")
    }

    /// Build the JSON body for `synthesize`. Exposed for tests.
    #[must_use]
    pub fn build_request_body(&self, text: &str) -> serde_json::Value {
        serde_json::to_value(SpeakReq { text, model_id: &self.model })
            .expect("serialising static-shape ElevenLabs request must not fail")
    }
}

#[derive(Serialize)]
struct SpeakReq<'a> {
    text: &'a str,
    model_id: &'a str,
}

#[async_trait]
impl TextToSpeech for ElevenLabsTts {
    fn name(&self) -> &'static str {
        "elevenlabs"
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
        // Per-call voice override wins; otherwise the configured /
        // catalogue voice. The voice id is part of the request path.
        let voice_id = voice.unwrap_or(&self.voice_id);
        let body = self.build_request_body(text);
        let resp = self
            .client
            .post(self.speech_url(voice_id))
            .header("xi-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("posting to elevenlabs /v1/text-to-speech")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(anyhow!("elevenlabs TTS returned {status}: {}", truncate(&body, 400)));
        }
        let bytes = resp.bytes().await.context("reading elevenlabs TTS response body")?;
        let pcm = pcm_i16_le_to_f32(&bytes);
        Ok(TtsAudio { pcm, sample_rate: NATIVE_RATE })
    }

    async fn prewarm(&self) -> Result<()> {
        // No documented cheap GET on the synth path; defer the TLS
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
    fn elevenlabs_client_uses_catalogue_defaults() {
        let c = ElevenLabsTts::new("sk-test", None, None);
        assert_eq!(c.model(), "eleven_v3");
        assert_eq!(c.voice_id(), "EXAVITQu4vr4xnSDxMaL");
        assert_eq!(
            c.speech_url(c.voice_id()),
            "https://api.elevenlabs.io/v1/text-to-speech/EXAVITQu4vr4xnSDxMaL?output_format=pcm_24000"
        );
        assert_eq!(c.native_sample_rate(), NATIVE_RATE);
    }

    #[test]
    fn overrides_take_effect() {
        let c = ElevenLabsTts::new(
            "sk-test",
            Some("eleven_multilingual_v2".to_string()),
            Some("custom-voice".to_string()),
        );
        assert_eq!(c.model(), "eleven_multilingual_v2");
        assert_eq!(c.voice_id(), "custom-voice");
    }

    #[test]
    fn request_body_shape_matches_spec() {
        let c = ElevenLabsTts::new("sk-test", None, None);
        let body = c.build_request_body("hi");
        assert_eq!(body["text"], "hi");
        assert_eq!(body["model_id"], "eleven_v3");
        let obj = body.as_object().expect("body is a JSON object");
        assert_eq!(obj.len(), 2, "body has only `text` and `model_id`");
    }

    #[tokio::test]
    async fn empty_text_returns_empty_audio_without_request() {
        let c = ElevenLabsTts::new("sk-test", None, None);
        let audio = c.synthesize("", None, None).await.unwrap();
        assert!(audio.pcm.is_empty());
        assert_eq!(audio.sample_rate, NATIVE_RATE);
    }
}
