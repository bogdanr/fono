// SPDX-License-Identifier: GPL-3.0-only
//! Default per-provider model / voice / endpoint values.

/// Default model name when `[tts.cloud].model` is empty.
#[must_use]
pub fn default_cloud_model(provider: &str) -> &'static str {
    match provider {
        // OpenAI's tts-1 is the lower-latency variant; tts-1-hd is
        // higher-quality but slower. Latency wins for a voice assistant.
        "openai" => "tts-1",
        // OpenRouter proxies OpenAI-compatible `POST /v1/audio/speech`
        // to upstream providers. We default to OpenAI's classical
        // `tts-1` (~0.5-2 s synthesis, single-pass decoder) because
        // OpenRouter's proxy was empirically unable to forward the
        // streaming output of `openai/gpt-4o-mini-tts-…` — it
        // flushed an ~9.6 KB preamble and then buffered the rest of
        // the body until upstream finished, exceeding every
        // reasonable client timeout. Users who want the LLM-based
        // voice can pin it in `config.toml`. Mirrors the catalogue
        // entry in `fono_core::provider_catalog::CLOUD_PROVIDERS`.
        "openrouter" => "openai/tts-1",
        _ => "",
    }
}

/// Default voice when `[tts].voice` is empty. Backend-specific.
#[must_use]
pub fn default_voice(provider: &str) -> &'static str {
    match provider {
        "openai" => "alloy",
        // Wyoming-piper picks its server-side default if we send no
        // voice; let it.
        _ => "",
    }
}

/// Default Wyoming TTS server URI. Distinct port from STT (10300) by
/// convention — wyoming-piper listens on 10200 out of the box.
pub const DEFAULT_WYOMING_URI: &str = "tcp://localhost:10200";
