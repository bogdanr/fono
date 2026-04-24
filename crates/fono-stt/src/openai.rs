// SPDX-License-Identifier: GPL-3.0-only
//! OpenAI STT backend (whisper-1 / gpt-4o-transcribe). Compatible JSON shape
//! with Groq for the text field.

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::multipart;
use serde::Deserialize;

use crate::traits::{SpeechToText, Transcription};

const OPENAI_ENDPOINT: &str = "https://api.openai.com/v1/audio/transcriptions";
const DEFAULT_MODEL: &str = "whisper-1";

pub struct OpenAiStt {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl OpenAiStt {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_model(api_key, DEFAULT_MODEL)
    }
    pub fn with_model(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            client: reqwest::Client::new(),
        }
    }
}

#[derive(Deserialize)]
struct Resp {
    text: String,
}

#[async_trait]
impl SpeechToText for OpenAiStt {
    async fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
        lang: Option<&str>,
    ) -> Result<Transcription> {
        let wav = crate::groq::encode_wav(pcm, sample_rate);
        let part = multipart::Part::bytes(wav)
            .file_name("audio.wav")
            .mime_str("audio/wav")?;
        let mut form = multipart::Form::new()
            .text("model", self.model.clone())
            .part("file", part);
        if let Some(l) = lang {
            form = form.text("language", l.to_string());
        }
        let res = self
            .client
            .post(OPENAI_ENDPOINT)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .context("openai POST failed")?;
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("openai STT {status}: {body}");
        }
        let parsed: Resp = serde_json::from_str(&body)
            .with_context(|| format!("parse openai response: {body}"))?;
        Ok(Transcription {
            text: parsed.text,
            language: lang.map(str::to_string),
            duration_ms: None,
        })
    }

    fn name(&self) -> &'static str {
        "openai"
    }
}
