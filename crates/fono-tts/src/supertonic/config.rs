// SPDX-License-Identifier: GPL-3.0-only
//! Supertonic `tts.json` runtime config (Slice 2, Task 2.1).
//!
//! Deserializes only the handful of fields the inference pipeline actually
//! needs (ported from `ParseConfig` in the sherpa reference
//! `offline-tts-supertonic-model.cc`): the audio sample rate and base chunk
//! size, the latent dimension, and the chunk compression factor. It also
//! validates `ttl.text_encoder.n_langs == 0`, the marker that this is the
//! char-level, language-agnostic acoustic model Fono targets (a nonzero value
//! would mean a language-embedding variant with a different input contract).

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// The runtime-relevant subset of `tts.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupertonicConfig {
    /// `ae.sample_rate` — output PCM rate, 44 100 Hz for Supertonic 3.
    pub sample_rate: i32,
    /// `ae.base_chunk_size` — vocoder base hop, 512.
    pub base_chunk_size: i32,
    /// `ttl.latent_dim` — per-frame latent width before compression, 24.
    pub latent_dim: i32,
    /// `ttl.chunk_compress_factor` — latent time-compression factor, 6.
    pub chunk_compress_factor: i32,
}

impl SupertonicConfig {
    /// Parse and validate the config from `tts.json` bytes.
    pub fn parse(json: &[u8]) -> Result<Self> {
        let raw: RawConfig = serde_json::from_slice(json).context("parse Supertonic tts.json")?;

        let cfg = Self {
            sample_rate: raw.ae.sample_rate,
            base_chunk_size: raw.ae.base_chunk_size,
            latent_dim: raw.ttl.latent_dim,
            chunk_compress_factor: raw.ttl.chunk_compress_factor,
        };

        // Positivity checks mirror the reference's ParseConfig guards.
        for (name, v) in [
            ("ae.sample_rate", cfg.sample_rate),
            ("ae.base_chunk_size", cfg.base_chunk_size),
            ("ttl.latent_dim", cfg.latent_dim),
            ("ttl.chunk_compress_factor", cfg.chunk_compress_factor),
        ] {
            if v <= 0 {
                bail!("invalid Supertonic tts.json: {name} = {v} (must be > 0)");
            }
        }

        // Fono only supports the char-level acoustic model (n_langs == 0).
        let n_langs = raw.ttl.text_encoder.n_langs;
        if n_langs != 0 {
            bail!(
                "unsupported Supertonic tts.json: ttl.text_encoder.n_langs = {n_langs} \
                 (expected 0 — Fono supports only the char-level, language-agnostic model)"
            );
        }

        Ok(cfg)
    }

    /// Vocoder chunk size in samples: `base_chunk_size * chunk_compress_factor`.
    #[must_use]
    pub fn wav_chunk_size(&self) -> i32 {
        self.base_chunk_size * self.chunk_compress_factor
    }

    /// Full latent channel dimension fed to the flow-matching graphs:
    /// `latent_dim * chunk_compress_factor`.
    #[must_use]
    pub fn latent_dim_full(&self) -> i32 {
        self.latent_dim * self.chunk_compress_factor
    }
}

#[derive(Deserialize)]
struct RawConfig {
    ae: RawAe,
    ttl: RawTtl,
}

#[derive(Deserialize)]
struct RawAe {
    sample_rate: i32,
    base_chunk_size: i32,
}

#[derive(Deserialize)]
struct RawTtl {
    latent_dim: i32,
    chunk_compress_factor: i32,
    text_encoder: RawTextEncoder,
}

#[derive(Deserialize)]
struct RawTextEncoder {
    n_langs: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact fields (with values) from the shipped
    /// `sherpa-onnx-supertonic-3-tts-int8-2026-05-11/tts.json`.
    const SAMPLE: &str = r#"{
        "ae": { "sample_rate": 44100, "base_chunk_size": 512, "chunk_compress_factor": 1 },
        "ttl": {
            "latent_dim": 24,
            "chunk_compress_factor": 6,
            "text_encoder": { "n_langs": 0, "lang_emb_dim": 0 }
        }
    }"#;

    #[test]
    fn parses_the_shipped_config_values() {
        let cfg = SupertonicConfig::parse(SAMPLE.as_bytes()).expect("valid config");
        assert_eq!(cfg.sample_rate, 44100);
        assert_eq!(cfg.base_chunk_size, 512);
        assert_eq!(cfg.latent_dim, 24);
        assert_eq!(cfg.chunk_compress_factor, 6);
    }

    #[test]
    fn derived_dimensions_match_the_reference_math() {
        let cfg = SupertonicConfig::parse(SAMPLE.as_bytes()).unwrap();
        // chunk_size = base_chunk_size(512) * ttl.chunk_compress_factor(6)
        assert_eq!(cfg.wav_chunk_size(), 512 * 6);
        // latent_dim = ttl.latent_dim(24) * ttl.chunk_compress_factor(6)
        assert_eq!(cfg.latent_dim_full(), 24 * 6);
    }

    #[test]
    fn rejects_nonzero_n_langs() {
        let bad = SAMPLE.replace("\"n_langs\": 0", "\"n_langs\": 31");
        let err = SupertonicConfig::parse(bad.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("n_langs"), "error names the offending field");
    }

    #[test]
    fn rejects_nonpositive_fields() {
        let bad = SAMPLE.replace("44100", "0");
        let err = SupertonicConfig::parse(bad.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("sample_rate"));
    }

    #[test]
    fn rejects_missing_sections() {
        let err = SupertonicConfig::parse(br#"{"ae":{"sample_rate":44100,"base_chunk_size":512}}"#)
            .unwrap_err();
        assert!(err.to_string().contains("tts.json"));
    }
}
