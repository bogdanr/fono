// SPDX-License-Identifier: GPL-3.0-only
//! Bench runner — drives a `SpeechToText` (and optional `TextFormatter`)
//! through a set of fixtures, records timings + WER, and aggregates a
//! `Report`. Separated from the binary entry point so unit tests and the
//! integration test can exercise it without spawning a process.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use tracing::{info, warn};

use fono_llm::traits::{FormatContext, TextFormatter};
use fono_stt::traits::SpeechToText;

use crate::fixtures::{Fixture, UNPINNED};
use crate::report::{ClipReport, Report};
use crate::wav;
use crate::wer::word_error_rate;

#[derive(Debug, Clone)]
pub struct BenchOutcome {
    pub clip: ClipReport,
}

pub struct BenchRunner {
    pub stt: Arc<dyn SpeechToText>,
    pub llm: Option<Arc<dyn TextFormatter>>,
    pub bench_root: PathBuf,
    /// When `true`, abort on a fixture whose on-disk SHA-256 doesn't
    /// match the pin. CI sets this to `true`; the binary keeps it
    /// `false` so first-run benchmarking on a fresh checkout still
    /// produces numbers (with a logged warning) before pins are filled.
    pub strict_pin: bool,
}

impl BenchRunner {
    pub fn new(stt: Arc<dyn SpeechToText>, bench_root: impl Into<PathBuf>) -> Self {
        Self {
            stt,
            llm: None,
            bench_root: bench_root.into(),
            strict_pin: false,
        }
    }

    #[must_use]
    pub fn with_llm(mut self, llm: Arc<dyn TextFormatter>) -> Self {
        self.llm = Some(llm);
        self
    }

    #[must_use]
    pub fn strict(mut self) -> Self {
        self.strict_pin = true;
        self
    }

    /// Ensure the fixture WAV is on disk; download via `fono_download`
    /// if missing. Verify SHA-256 if pinned.
    pub async fn ensure_fixture(&self, fx: &Fixture) -> Result<PathBuf> {
        let path = fx.cache_path(&self.bench_root);
        if !path.exists() {
            tokio::fs::create_dir_all(&self.bench_root).await.ok();
            // The URL in the registry is the *source* (LibriVox MP3 etc.);
            // production fixtures should have already been transcoded to
            // 16 kHz mono 16-bit PCM WAV by `scripts/fetch-fixtures.sh`.
            // We refuse to download MP3/OGG directly — that would silently
            // bypass the transcode step and produce numbers nobody can
            // reproduce.
            return Err(anyhow!(
                "fixture {} not found at {} — run \
                 `crates/fono-bench/scripts/fetch-fixtures.sh` first",
                fx.id,
                path.display()
            ));
        }

        if fx.sha256 == UNPINNED {
            warn!(
                "fixture {} has UNPINNED sha256; results are reproducible only against \
                 the local copy at {}",
                fx.id,
                path.display()
            );
            if self.strict_pin {
                return Err(anyhow!(
                    "strict mode: fixture {} has unpinned sha256",
                    fx.id
                ));
            }
        } else {
            let actual = sha256_of(&path).await?;
            if !actual.eq_ignore_ascii_case(fx.sha256) {
                return Err(anyhow!(
                    "fixture {} sha256 mismatch: pinned {}, on-disk {}",
                    fx.id,
                    fx.sha256,
                    actual
                ));
            }
        }
        Ok(path)
    }

    /// Run a single fixture end-to-end, returning a `ClipReport`.
    pub async fn run_one(&self, fx: &Fixture) -> Result<BenchOutcome> {
        let path = self.ensure_fixture(fx).await?;
        let wav = wav::read(&path).with_context(|| format!("decode {}", path.display()))?;
        if wav.sample_rate != 16_000 {
            return Err(anyhow!(
                "fixture {} is {} Hz; expected 16000 Hz (re-run fetch-fixtures.sh)",
                fx.id,
                wav.sample_rate
            ));
        }

        let total_t = Instant::now();
        let stt_t = Instant::now();
        let trans = self
            .stt
            .transcribe(&wav.samples, wav.sample_rate, Some(fx.language))
            .await
            .with_context(|| format!("STT failed on {}", fx.id))?;
        let stt_ms = stt_t.elapsed().as_millis() as u64;

        let (final_text, llm_ms) = if let Some(llm) = &self.llm {
            let ctx = FormatContext {
                language: Some(fx.language.to_string()),
                ..FormatContext::default()
            };
            let llm_t = Instant::now();
            let cleaned = llm
                .format(&trans.text, &ctx)
                .await
                .with_context(|| format!("LLM failed on {}", fx.id))?;
            let llm_ms = llm_t.elapsed().as_millis() as u64;
            (cleaned, Some(llm_ms))
        } else {
            (trans.text.clone(), None)
        };
        let total_ms = total_t.elapsed().as_millis() as u64;

        let wer = word_error_rate(fx.transcript, &final_text);
        info!(
            "fixture {} [{}]: stt={stt_ms}ms llm={:?}ms total={total_ms}ms wer={:.3}",
            fx.id, fx.language, llm_ms, wer
        );

        let clip = ClipReport {
            id: fx.id.to_string(),
            language: fx.language.to_string(),
            reference: fx.transcript.to_string(),
            hypothesis: final_text,
            wer,
            stt_ms,
            llm_ms,
            total_ms,
            samples: wav.samples.len(),
            sample_rate: wav.sample_rate,
        };
        Ok(BenchOutcome { clip })
    }

    /// Run every fixture matching `langs` (or all when empty), aggregate
    /// into a `Report`. Continues past per-clip errors and surfaces them
    /// in the returned vector — partial reports are more useful than no
    /// report when a single fixture is broken.
    pub async fn run_all(
        &self,
        langs: &[String],
        iterations: usize,
    ) -> Result<(Report, Vec<String>)> {
        let mut clips = Vec::new();
        let mut errors = Vec::new();

        let mut filtered: Vec<&Fixture> = crate::fixtures::FIXTURES.iter().collect();
        if !langs.is_empty() {
            let wanted: Vec<String> = langs.iter().map(|s| s.to_ascii_lowercase()).collect();
            filtered.retain(|f| wanted.iter().any(|w| w == f.language));
        }
        if filtered.is_empty() {
            return Err(anyhow!("no fixtures matched languages {langs:?}"));
        }

        for fx in filtered {
            for _ in 0..iterations.max(1) {
                match self.run_one(fx).await {
                    Ok(o) => clips.push(o.clip),
                    Err(e) => errors.push(format!("{}: {e:#}", fx.id)),
                }
            }
        }

        let report = Report::build(
            self.stt.name(),
            self.llm.as_ref().map(|l| l.name().to_string()),
            clips,
        );
        Ok((report, errors))
    }
}

async fn sha256_of(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;
    let mut f = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}
