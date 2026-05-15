// SPDX-License-Identifier: GPL-3.0-only
//! Text-to-speech trait + Wyoming, OpenAI, and other cloud
//! backends. Mirrors the shape of `fono-stt`: one trait, a factory
//! gated by feature flags, and per-backend modules that talk the
//! provider's wire format and return mono `f32` PCM at the backend's
//! native sample rate.
//!
//! Designed for the **voice assistant** path: callers stream
//! sentence-sized chunks of LLM output through `synthesize()` and feed
//! the resulting `TtsAudio` into `fono-audio::playback` for output.
//! Time-to-first-audio is bounded by the first sentence's synth
//! latency rather than the full LLM completion.

pub mod defaults;
pub mod factory;
pub mod sentence_split;
pub mod traits;

#[cfg(feature = "cartesia")]
pub mod cartesia;
#[cfg(feature = "deepgram")]
pub mod deepgram;
#[cfg(any(
    feature = "openai",
    feature = "groq",
    feature = "openrouter",
    feature = "cartesia",
    feature = "deepgram"
))]
pub mod openai_compat;
#[cfg(feature = "wyoming")]
pub mod wyoming;

pub use factory::build_tts;
pub use sentence_split::SentenceSplitter;
pub use traits::{TextToSpeech, TtsAudio};
