// SPDX-License-Identifier: GPL-3.0-only
//! Local `llama-cpp-2` backend — opt-in via the `llama-local` feature because
//! it vendors llama.cpp (C++ build).

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;

use crate::traits::{FormatContext, TextFormatter};

pub struct LlamaLocal {
    model_path: PathBuf,
    context_size: u32,
}

impl LlamaLocal {
    pub fn new(model_path: impl Into<PathBuf>, context_size: u32) -> Self {
        Self {
            model_path: model_path.into(),
            context_size,
        }
    }
}

#[async_trait]
impl TextFormatter for LlamaLocal {
    async fn format(&self, _raw: &str, _ctx: &FormatContext) -> Result<String> {
        // NOTE: `llama-cpp-2`'s API surface differs by minor version; this
        // scaffold keeps the binary compilable when `llama-local` is enabled
        // and defers wiring to the follow-up ADR/phase that pins a working
        // revision of the crate. Calling this backend today returns an error
        // instead of silently returning empty text.
        let _ = &self.model_path;
        let _ = self.context_size;
        Err(anyhow!(
            "LlamaLocal is not yet wired; pin a working `llama-cpp-2` revision \
             before enabling the `llama-local` feature"
        ))
        .context("llama-local scaffold")
    }

    fn name(&self) -> &'static str {
        "llama-local"
    }
}
