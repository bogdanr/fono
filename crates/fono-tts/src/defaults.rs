// SPDX-License-Identifier: GPL-3.0-only
//! TTS endpoint constants.
//!
//! Default cloud TTS models and voices live in the catalogue
//! ([`fono_core::provider_catalog::CLOUD_PROVIDERS`]) — to change them,
//! edit the `TtsDefaults` field of the relevant `CloudProvider` entry.
//! The OpenAI-compat client constructors in
//! [`crate::openai_compat`] (`openai_client`, `groq_client`,
//! `openrouter_client`) read the model + voice + base URL straight
//! from the catalogue, so a single edit there flows through every
//! consumer.

/// Default Wyoming TTS server URI. Distinct port from STT (10300) by
/// convention — wyoming-piper listens on 10200 out of the box.
pub const DEFAULT_WYOMING_URI: &str = "tcp://localhost:10200";
