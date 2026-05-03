// SPDX-License-Identifier: GPL-3.0-only
//! In-process Piper backend — **stub in v1**.
//!
//! Piper's reference runtime is `piper-phonemize` + `onnxruntime`.
//! Onnxruntime's static-build story conflicts with fono's
//! "no shared libraries" promise on the static-musl ship build, and
//! the dynamic build pulls a 14 MB+ libonnxruntime.so into `NEEDED`.
//!
//! Until either onnxruntime gains a clean static-musl path, or a
//! pure-Rust ONNX runtime ships with usable performance on Piper
//! voices, this module exists only as a placeholder. Users who want
//! local Piper today should run `wyoming-piper` (a 12 MB Docker image
//! with an OS-package install path) and point fono at it via the
//! `wyoming` backend.
//!
//! The factory in `crate::factory` returns a clear error pointing at
//! the wyoming-piper path when this backend is selected.

#![allow(dead_code)]

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::traits::{TextToSpeech, TtsAudio};

/// Placeholder type. Constructing one always fails — see module docs.
pub struct PiperLocal;

impl PiperLocal {
    pub fn new() -> Result<Self> {
        Err(anyhow!(
            "in-process Piper is not yet supported. Use wyoming-piper instead: \
             `docker run --rm -p 10200:10200 rhasspy/wyoming-piper` and set \
             `tts.backend = \"wyoming\"`."
        ))
    }
}

#[async_trait]
impl TextToSpeech for PiperLocal {
    fn name(&self) -> &'static str {
        "piper"
    }
    fn native_sample_rate(&self) -> u32 {
        22_050
    }
    async fn synthesize(
        &self,
        _text: &str,
        _voice: Option<&str>,
        _lang: Option<&str>,
    ) -> Result<TtsAudio> {
        Err(anyhow!("in-process Piper not implemented"))
    }
}
