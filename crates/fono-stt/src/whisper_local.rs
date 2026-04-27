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

// ---------------------------------------------------------------------
// Streaming impl. Plan R3.
// ---------------------------------------------------------------------

#[cfg(feature = "streaming")]
mod streaming_impl {
    #[allow(clippy::wildcard_imports)]
    use super::*;
    use std::time::{Duration, Instant};

    use async_trait::async_trait;
    use futures::stream::{BoxStream, StreamExt};
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::UnboundedReceiverStream;

    use crate::streaming::{
        LocalAgreement, StreamFrame, StreamingStt, TranscriptUpdate,
    };

    /// Minimum buffered audio (in samples at 16 kHz) before we run the
    /// preview lane. 0.8 s — small enough to hit a sub-400 ms TTFF after
    /// VAD onset, large enough that whisper does not hallucinate. Plan
    /// R3 / "adaptive chunking math".
    const PREVIEW_MIN_SAMPLES: usize = 16_000 * 8 / 10;
    /// Run the preview lane at most every `PREVIEW_MIN_INTERVAL` of
    /// wall-clock time so we don't peg the CPU on a long monologue.
    const PREVIEW_MIN_INTERVAL: Duration = Duration::from_millis(700);

    #[async_trait]
    impl StreamingStt for WhisperLocal {
        async fn stream_transcribe(
            &self,
            mut frames: BoxStream<'static, StreamFrame>,
            sample_rate: u32,
            lang: Option<String>,
        ) -> anyhow::Result<BoxStream<'static, TranscriptUpdate>> {
            self.ensure_ctx()?;

            let (tx, rx) = mpsc::unbounded_channel::<TranscriptUpdate>();
            let lang_owned = lang;
            let started = Instant::now();
            let stt = self.clone_arc();

            tokio::spawn(async move {
                let mut segment_index: u32 = 0;
                let mut segment_pcm: Vec<f32> = Vec::with_capacity(16_000 * 30);
                let mut last_preview_at: Option<Instant> = None;
                let mut agreement = LocalAgreement::new();

                while let Some(frame) = frames.next().await {
                    match frame {
                        StreamFrame::Pcm(chunk) => {
                            segment_pcm.extend_from_slice(&chunk);
                            // Decide whether to fire a preview pass.
                            let big_enough = segment_pcm.len() >= PREVIEW_MIN_SAMPLES;
                            let cooled =
                                last_preview_at.is_none_or(|t| t.elapsed() >= PREVIEW_MIN_INTERVAL);
                            if big_enough && cooled {
                                let preview_pcm = segment_pcm.clone();
                                let lang = lang_owned.clone();
                                let stt2 = Arc::clone(&stt);
                                let res = tokio::task::spawn_blocking(move || {
                                    decode_blocking(&stt2, &preview_pcm, sample_rate, lang.as_deref())
                                })
                                .await;
                                if let Ok(Ok(text)) = res {
                                    let tokens = whitespace_tokens(&text);
                                    agreement.observe(tokens.iter().cloned());
                                    let stable = agreement.stable().join(" ");
                                    let upd = TranscriptUpdate::preview(
                                        segment_index,
                                        if stable.is_empty() { text } else { stable },
                                        started.elapsed(),
                                    )
                                    .with_language(lang_owned.clone());
                                    if tx.send(upd).is_err() {
                                        return;
                                    }
                                }
                                last_preview_at = Some(Instant::now());
                            }
                        }
                        StreamFrame::SegmentBoundary | StreamFrame::Eof => {
                            // Dual-pass finalize: run twice on the
                            // accumulated segment audio and use
                            // LocalAgreement to keep only the prefix
                            // both passes agreed on. Plan R3.
                            if !segment_pcm.is_empty() {
                                let mut la_final = LocalAgreement::new();
                                let mut last_text = String::new();
                                for _ in 0..2 {
                                    let pcm = segment_pcm.clone();
                                    let lang = lang_owned.clone();
                                    let stt2 = Arc::clone(&stt);
                                    let res = tokio::task::spawn_blocking(move || {
                                        decode_blocking(&stt2, &pcm, sample_rate, lang.as_deref())
                                    })
                                    .await;
                                    if let Ok(Ok(text)) = res {
                                        let toks = whitespace_tokens(&text);
                                        la_final.observe(toks.iter().cloned());
                                        last_text = text;
                                    }
                                }
                                let stable = la_final.stable().join(" ");
                                let final_text = if stable.is_empty() {
                                    last_text
                                } else {
                                    stable
                                };
                                let upd = TranscriptUpdate::finalize(
                                    segment_index,
                                    final_text,
                                    started.elapsed(),
                                )
                                .with_language(lang_owned.clone());
                                let _ = tx.send(upd);
                            }
                            segment_pcm.clear();
                            agreement.reset();
                            last_preview_at = None;
                            segment_index += 1;
                            if matches!(frame, StreamFrame::Eof) {
                                break;
                            }
                        }
                    }
                }
            });

            Ok(UnboundedReceiverStream::new(rx).boxed())
        }

        fn name(&self) -> &'static str {
            <Self as crate::SpeechToText>::name(self)
        }
    }

    /// Helper used by the streaming task to invoke whisper from a
    /// blocking thread. Returns plain `String` so the caller can decide
    /// the lane / segment-index wrapping.
    fn decode_blocking(
        stt: &Arc<WhisperLocal>,
        pcm: &[f32],
        _sample_rate: u32,
        lang: Option<&str>,
    ) -> anyhow::Result<String> {
        let guard = stt
            .ctx
            .lock()
            .map_err(|_| anyhow!("whisper mutex poisoned"))?;
        let ctx = guard.as_ref().expect("ensure_ctx already called");
        let mut state = ctx.create_state().context("create whisper state")?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(stt.threads);
        params.set_translate(false);
        if let Some(l) = lang {
            if l != "auto" {
                params.set_language(Some(l));
            }
        }
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        state.full(params, pcm).context("whisper full()")?;
        let segments = state.full_n_segments();
        let mut text = String::new();
        for i in 0..segments {
            if let Some(seg) = state.get_segment(i) {
                if let Ok(s) = seg.to_str_lossy() {
                    text.push_str(&s);
                }
            }
        }
        Ok(strip_whisper_artifacts(text.trim()))
    }

    fn whitespace_tokens(s: &str) -> Vec<String> {
        s.split_whitespace().map(ToString::to_string).collect()
    }

    /// Filter out whisper meta-tokens emitted on silence / non-speech
    /// audio. Whisper-large/medium/small were trained on transcripts
    /// that bracket non-verbal segments with all-caps tags like
    /// `[BLANK_AUDIO]`, `[MUSIC PLAYING]`, `[SILENCE]`, `(applause)`,
    /// or `*coughing*`. When the user pauses or there's ambient noise,
    /// these leak into the transcript and are visually noisy in the
    /// overlay (and worse — get injected as text into the focused app).
    ///
    /// Strategy: drop any all-uppercase or "(verb)" parenthetical /
    /// bracketed run that consists only of letters, spaces, and
    /// underscores. Real bracketed content the user dictated (e.g.
    /// "see RFC 1234" or "[TODO] write tests") survives because it's
    /// not all-uppercase pseudo-tokens.
    fn strip_whisper_artifacts(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i] as char;
            if c == '[' || c == '(' || c == '*' {
                let close = match c {
                    '[' => ']',
                    '(' => ')',
                    _ => '*',
                };
                if let Some(end) = s[i + 1..].find(close) {
                    let inner = &s[i + 1..i + 1 + end];
                    if is_meta_token(inner) {
                        i += 1 + end + 1;
                        // Eat one trailing space so we don't leave
                        // a double-space hole.
                        while i < bytes.len() && bytes[i] == b' ' {
                            i += 1;
                        }
                        continue;
                    }
                }
            }
            out.push(c);
            i += 1;
        }
        out.trim().to_string()
    }

    fn is_meta_token(inner: &str) -> bool {
        if inner.is_empty() {
            return false;
        }
        // Letters, spaces, and underscores only — and at least one
        // alphabetic letter. Whisper meta-tokens never contain
        // digits or sentence punctuation.
        let valid = inner
            .chars()
            .all(|ch| ch.is_ascii_alphabetic() || ch == ' ' || ch == '_');
        if !valid {
            return false;
        }
        let alpha = inner.chars().filter(char::is_ascii_alphabetic);
        let upper_only = alpha.clone().all(|c| c.is_ascii_uppercase());
        let lower_only = alpha.clone().all(|c| c.is_ascii_lowercase());
        // Treat as meta if it's all-uppercase ([BLANK_AUDIO],
        // [MUSIC PLAYING]) OR all-lowercase short verb-like
        // ((applause), (coughing), (laughs)). Mixed case is treated
        // as user content and preserved.
        upper_only || (lower_only && inner.len() <= 24)
    }

    impl WhisperLocal {
        fn clone_arc(&self) -> Arc<Self> {
            Arc::new(Self {
                model_path: self.model_path.clone(),
                ctx: Arc::clone(&self.ctx),
                threads: self.threads,
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::strip_whisper_artifacts;

        #[test]
        fn strips_uppercase_bracketed_meta() {
            assert_eq!(strip_whisper_artifacts("[BLANK_AUDIO]"), "");
            assert_eq!(strip_whisper_artifacts("[MUSIC PLAYING]"), "");
            assert_eq!(strip_whisper_artifacts("[SILENCE]"), "");
            assert_eq!(
                strip_whisper_artifacts("hello [BLANK_AUDIO] world"),
                "hello world"
            );
        }

        #[test]
        fn strips_lowercase_parenthetical_verbs() {
            assert_eq!(strip_whisper_artifacts("(applause)"), "");
            assert_eq!(strip_whisper_artifacts("(coughing)"), "");
            assert_eq!(strip_whisper_artifacts("hello (laughs) world"), "hello world");
        }

        #[test]
        fn preserves_mixed_case_user_content() {
            // Mixed case → user dictated "see Fig 3", whisper rendered it.
            assert_eq!(strip_whisper_artifacts("see (Fig 3)"), "see (Fig 3)");
            assert_eq!(strip_whisper_artifacts("note [version 2]"), "note [version 2]");
        }

        #[test]
        fn strips_all_caps_bracketed_even_if_user_might_have_meant_them() {
            // Trade-off: a user dictating "open bracket TODO close
            // bracket" almost certainly won't get whisper to emit
            // "[TODO]"; whisper meta-tokens are the dominant cause of
            // all-caps bracketed runs in transcripts. Strip them.
            assert_eq!(strip_whisper_artifacts("[TODO]"), "");
        }

        #[test]
        fn collapses_double_spaces_after_strip() {
            assert_eq!(
                strip_whisper_artifacts("hello [BLANK_AUDIO]  world"),
                "hello world"
            );
        }

        #[test]
        fn empty_brackets_are_left_alone() {
            assert_eq!(strip_whisper_artifacts("[]"), "[]");
        }

        #[test]
        fn unmatched_bracket_is_preserved() {
            assert_eq!(strip_whisper_artifacts("hello ["), "hello [");
        }
    }
}
