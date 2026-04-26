// SPDX-License-Identifier: GPL-3.0-only
//! Local `whisper-rs` backend. Compiled only with the `whisper-local` feature
//! since it vendors whisper.cpp (C++ build) and materially increases build
//! time. See Phase 4 Task 4.2.
//
// We hold the context mutex for the whole `transcribe` call (and
// likewise inside `prewarm`) by design: whisper.cpp inference borrows
// from the loaded `WhisperContext`, and serialising calls is the
// simplest way to avoid concurrent state misuse. Silence clippy.
#![allow(clippy::significant_drop_tightening)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use std::sync::Once;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::traits::{SpeechToText, Transcription};

/// Install whisper-rs's tracing bridge once per process so whisper.cpp + GGML
/// logs flow through `tracing` (where they are filtered by the daemon's normal
/// log-level config) instead of being printed straight to stderr at every
/// transcription. The default CLI filter keeps whisper.cpp/GGML `info` chatter
/// hidden; users can re-enable it with an explicit `FONO_LOG` module filter
/// when debugging.
static WHISPER_LOG_INIT: Once = Once::new();

fn init_whisper_logging() {
    WHISPER_LOG_INIT.call_once(|| {
        whisper_rs::install_logging_hooks();
    });
}

pub struct WhisperLocal {
    model_path: PathBuf,
    ctx: Arc<Mutex<Option<WhisperContext>>>,
    threads: i32,
}

impl WhisperLocal {
    pub fn new(model_path: impl Into<PathBuf>) -> Self {
        Self::with_threads(model_path, num_cpus())
    }

    pub fn with_threads(model_path: impl Into<PathBuf>, threads: i32) -> Self {
        init_whisper_logging();
        Self {
            model_path: model_path.into(),
            ctx: Arc::new(Mutex::new(None)),
            threads,
        }
    }

    fn ensure_ctx(&self) -> Result<()> {
        let mut guard = self
            .ctx
            .lock()
            .map_err(|_| anyhow!("whisper mutex poisoned"))?;
        if guard.is_none() {
            let path = self
                .model_path
                .to_str()
                .ok_or_else(|| anyhow!("non-UTF-8 model path"))?;
            let ctx = WhisperContext::new_with_params(path, WhisperContextParameters::default())
                .context("load whisper model")?;
            *guard = Some(ctx);
        }
        Ok(())
    }
}

#[async_trait]
impl SpeechToText for WhisperLocal {
    async fn transcribe(
        &self,
        pcm: &[f32],
        _sample_rate: u32,
        lang: Option<&str>,
    ) -> Result<Transcription> {
        self.ensure_ctx()?;
        let pcm = pcm.to_vec();
        let lang = lang.map(str::to_string);
        let threads = self.threads;
        let guard = self
            .ctx
            .lock()
            .map_err(|_| anyhow!("whisper mutex poisoned"))?;
        let ctx = guard.as_ref().expect("ensure_ctx succeeded");
        let mut state = ctx.create_state().context("create whisper state")?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(threads);
        params.set_translate(false);
        if let Some(l) = lang.as_deref() {
            if l != "auto" {
                params.set_language(Some(l));
            }
        }
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state.full(params, &pcm).context("whisper full()")?;
        let segments = state.full_n_segments();
        let mut text = String::new();
        for i in 0..segments {
            if let Some(seg) = state.get_segment(i) {
                if let Ok(s) = seg.to_str_lossy() {
                    text.push_str(&s);
                }
            }
        }
        Ok(Transcription {
            text: text.trim().to_string(),
            language: lang,
            duration_ms: None,
        })
    }

    fn name(&self) -> &'static str {
        "whisper-local"
    }

    async fn prewarm(&self) -> Result<()> {
        // mmap the model on a blocking thread so we don't park an
        // async executor for 200–600 ms (latency plan L2).
        let path = self.model_path.clone();
        let ctx = Arc::clone(&self.ctx);
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut guard = ctx.lock().map_err(|_| anyhow!("whisper mutex poisoned"))?;
            if guard.is_none() {
                let p = path
                    .to_str()
                    .ok_or_else(|| anyhow!("non-UTF-8 model path"))?;
                let c = WhisperContext::new_with_params(p, WhisperContextParameters::default())
                    .context("load whisper model")?;
                *guard = Some(c);
            }
            Ok(())
        })
        .await
        .context("whisper prewarm join")?
    }
}

fn num_cpus() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4)
}
