// SPDX-License-Identifier: GPL-3.0-only
//! Speech-to-text trait + local (`whisper-rs`, opt-in) and cloud
//! (Groq by default; Deepgram, OpenAI behind feature flags) backends.
//! Phase 4 of `docs/plans/2026-04-24-fono-design-v1.md`.

pub mod defaults;
pub mod factory;
pub mod lang;
pub mod lang_cache;
pub mod registry;
pub mod traits;

#[cfg(feature = "streaming")]
pub mod streaming;

#[cfg(feature = "groq")]
pub mod groq;
#[cfg(all(feature = "groq", feature = "streaming"))]
pub mod groq_streaming;
#[cfg(feature = "openai")]
pub mod openai;
#[cfg(feature = "whisper-local")]
pub mod whisper_local;

#[cfg(feature = "streaming")]
pub use factory::build_streaming_stt;
pub use factory::build_stt;
pub use lang::LanguageSelection;
pub use lang_cache::LanguageCache;
pub use registry::{ModelInfo, ModelRegistry};
pub use traits::{SpeechToText, Transcription};

#[cfg(feature = "streaming")]
pub use streaming::{LocalAgreement, StreamFrame, StreamingStt, TranscriptUpdate, UpdateLane};

#[cfg(all(feature = "groq", feature = "streaming"))]
pub use groq_streaming::{GroqRequestFn, GroqRequestFuture, GroqStreaming};

#[cfg(feature = "groq")]
pub use groq::GroqResponse;
