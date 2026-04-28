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
use std::collections::HashMap;
use std::sync::Once;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::lang::LanguageSelection;
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
    /// Configured allow-list. Empty = unconstrained auto-detect.
    /// One = forced. Two or more = constrained auto-detect via
    /// `state.lang_detect` masked to this set.
    languages: Vec<String>,
    /// Optional per-language initial prompts. Keys are BCP-47 alpha-2
    /// codes (e.g. `"en"`, `"ro"`). Selected at call time based on
    /// the resolved language.
    prompts: HashMap<String, String>,
}

/// Built-in default prompt used when the model is English-only and
/// the user has not configured a custom `[stt.prompts].en`. Biases
/// Whisper away from training-set closers ("Thank you for watching")
/// without affecting accent / vocabulary.
const DEFAULT_EN_PROMPT: &str =
    "Professional dictation. Output exactly what the speaker says with proper \
     punctuation and capitalization.";

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
            languages: Vec::new(),
            prompts: HashMap::new(),
        }
    }

    /// Builder: set the language allow-list. Codes are normalised
    /// (trimmed, lowercased, `"auto"` collapsed). See
    /// [`crate::lang::LanguageSelection`] for semantics.
    #[must_use]
    pub fn with_languages(mut self, codes: Vec<String>) -> Self {
        self.languages = codes;
        self
    }

    /// Builder: set the per-language initial-prompt map.
    #[must_use]
    pub fn with_prompts(mut self, prompts: HashMap<String, String>) -> Self {
        self.prompts = prompts;
        self
    }

    /// Returns true when the model file name carries Whisper's `.en`
    /// suffix (e.g. `ggml-small.en.bin`), indicating an English-only
    /// model.
    fn model_is_english_only(&self) -> bool {
        self.model_path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains(".en"))
    }

    /// Resolve the initial prompt to send for a given language.
    /// Returns `None` when the language is unknown (cold-start
    /// auto-detect) so we don't bias the language classifier.
    fn resolve_prompt(&self, lang: Option<&str>) -> Option<String> {
        let code = lang?;
        if let Some(p) = self.prompts.get(code) {
            return Some(p.clone());
        }
        // English-only model + English audio + no custom prompt:
        // ship the built-in default to suppress the YouTube-flavoured
        // "Thank you" hallucination at minimal risk.
        if code == "en" && self.model_is_english_only() {
            return Some(DEFAULT_EN_PROMPT.to_string());
        }
        None
    }

    /// Resolve the effective selection for a single call: a per-call
    /// `lang` override beats the configured allow-list, and the alias
    /// `"auto"` clears any constraint.
    fn effective_selection(&self, lang_override: Option<&str>) -> LanguageSelection {
        LanguageSelection::from_config(&self.languages).with_override(lang_override)
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
        let selection = self.effective_selection(lang);
        let threads = self.threads;
        let guard = self
            .ctx
            .lock()
            .map_err(|_| anyhow!("whisper mutex poisoned"))?;
        let ctx = guard.as_ref().expect("ensure_ctx succeeded");

        // Resolve the single language code we'll lock the decoder to.
        // For `Auto` this stays `None` (full whisper auto-detect).
        // For `Forced` we use the code directly. For `AllowList` we
        // run an encoder-only `lang_detect` pass over the audio prefix
        // and argmax over the masked subset — that's the "banning"
        // mechanism that gives users multi-language dictation without
        // letting Whisper drift into unrelated languages.
        let resolved = resolve_language(ctx, &selection, pcm, threads)?;

        let mut state = ctx.create_state().context("create whisper state")?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(threads);
        params.set_translate(false);
        if let Some(code) = resolved.as_deref() {
            params.set_language(Some(code));
        }
        // Hallucination guards. `whisper-rs::FullParams::new()` leaves
        // these disabled even though canonical whisper.cpp enables them
        // by default. Without these, Whisper-large/turbo readily
        // hallucinate "Thank you" / "Bye" / "you you you" on silent or
        // low-volume tails. Values match whisper.cpp defaults.
        params.set_no_speech_thold(0.6);
        params.set_logprob_thold(-1.0);
        params.set_temperature_inc(0.2);
        // Resolve initial prompt by language for the active call.
        let prompt = self.resolve_prompt(resolved.as_deref());
        if let Some(p) = prompt.as_deref() {
            params.set_initial_prompt(p);
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
        // Surface the language whisper actually decoded against —
        // either our resolved pick or, for unconstrained auto, the
        // post-hoc lang id from the state.
        let detected = resolved.or_else(|| post_hoc_lang(&state));
        Ok(Transcription {
            text: text.trim().to_string(),
            language: detected,
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
// Constrained language detection. The "ban languages outside the
// allow-list" mechanism: run an encoder-only `lang_detect` pass over
// the audio prefix, mask the resulting probability vector to the
// allow-list, argmax -- then pin the decoder to that single code via
// `params.set_language`. Whisper itself has no native multi-language
// constraint; this is the supported wrapper-layer enforcement.
// ---------------------------------------------------------------------

/// At most this many seconds of leading audio are fed to the
/// `lang_detect` encoder pass. Whisper internally only looks at the
/// first 30 s anyway; capping in the wrapper keeps very long buffers
/// from re-running the encoder over audio the detector won't use.
const LANG_DETECT_PREFIX_SAMPLES: usize = 16_000 * 30;

/// Resolve the single language code we'll lock the decoder to for one
/// pipeline call. Returns `Ok(None)` for [`LanguageSelection::Auto`],
/// which keeps today's "let whisper auto-detect freely" behaviour.
fn resolve_language(
    ctx: &WhisperContext,
    selection: &LanguageSelection,
    pcm: &[f32],
    threads: i32,
) -> Result<Option<String>> {
    match selection {
        LanguageSelection::Auto => Ok(None),
        LanguageSelection::Forced(c) => Ok(Some(c.clone())),
        LanguageSelection::AllowList(codes) => {
            let pick = pick_from_allow_list(ctx, codes, pcm, threads)?;
            Ok(Some(pick))
        }
    }
}

/// Run `pcm_to_mel` + `lang_detect` on the first ~30 s of audio and
/// argmax over the allow-list. Falls back to the first allow-list
/// entry if the detector returns nonsense (negative probs, all-zero
/// row, unknown codes), so the worst-case "ban" still produces a
/// transcript in a known-allowed language.
fn pick_from_allow_list(
    ctx: &WhisperContext,
    codes: &[String],
    pcm: &[f32],
    threads: i32,
) -> Result<String> {
    let prefix_len = pcm.len().min(LANG_DETECT_PREFIX_SAMPLES);
    let prefix = &pcm[..prefix_len];

    let mut state = ctx
        .create_state()
        .context("create whisper state (lang_detect)")?;
    state
        .pcm_to_mel(prefix, threads as usize)
        .context("whisper pcm_to_mel for lang_detect")?;
    let (_top_id, probs) = state
        .lang_detect(0, threads.max(1) as usize)
        .context("whisper lang_detect")?;

    // Translate each user-supplied BCP-47 code to whisper's internal
    // language id, then argmax. Unknown codes are dropped with a warn.
    let mut best: Option<(f32, &str)> = None;
    let mut considered = 0usize;
    for code in codes {
        let Some(id) = whisper_rs::get_lang_id(code) else {
            tracing::warn!("language allow-list contains unknown BCP-47 code {code:?}; skipping");
            continue;
        };
        let idx = id as usize;
        let prob = probs.get(idx).copied().unwrap_or(0.0);
        considered += 1;
        if best.is_none_or(|(p, _)| prob > p) {
            best = Some((prob, code.as_str()));
        }
    }

    if considered == 0 {
        // Every supplied code was unknown to whisper. Fall through to
        // the first entry as a deterministic last resort.
        let fallback = codes.first().cloned().unwrap_or_default();
        tracing::warn!(
            "no language in allow-list {codes:?} is known to whisper; \
             falling back to {fallback:?} (transcript may be garbled)"
        );
        return Ok(fallback);
    }

    let (best_prob, best_code) = best.expect("considered > 0 implies Some(best)");
    tracing::debug!(
        target: "fono_stt::lang",
        "lang_detect picked {best_code} (p={best_prob:.3}) from allow-list of {} codes",
        codes.len()
    );
    Ok(best_code.to_string())
}

/// Best-effort post-decode language id read-back. Used for the `Auto`
/// path where we want history rows to record the language whisper
/// actually decoded against, not just `None`.
fn post_hoc_lang(state: &whisper_rs::WhisperState) -> Option<String> {
    let id = state.full_lang_id_from_state();
    if id < 0 {
        return None;
    }
    whisper_rs::get_lang_str(id).map(str::to_string)
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

    use crate::streaming::{LocalAgreement, StreamFrame, StreamingStt, TranscriptUpdate};

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
            // Selection seed for this entire stream. Constraint
            // resolution (allow-list -> single code) happens per
            // segment in `resolve_segment_lang` below so each segment
            // re-reads the prefix language; users routinely switch
            // languages mid-session and a stream-wide cache would lock
            // them into the first guess.
            let selection_seed = self.effective_selection(lang.as_deref());
            let started = Instant::now();
            let stt = self.clone_arc();

            tokio::spawn(async move {
                let mut segment_index: u32 = 0;
                let mut segment_pcm: Vec<f32> = Vec::with_capacity(16_000 * 30);
                let mut last_preview_at: Option<Instant> = None;
                let mut agreement = LocalAgreement::new();
                // Per-segment cached pick from `lang_detect`. We
                // detect on the first qualifying preview pass and
                // reuse the picked code for every subsequent decode
                // of this segment, resetting on SegmentBoundary/Eof.
                let mut segment_lang: Option<String> = None;

                while let Some(frame) = frames.next().await {
                    match frame {
                        StreamFrame::Pcm(chunk) => {
                            segment_pcm.extend_from_slice(&chunk);
                            // Decide whether to fire a preview pass.
                            let big_enough = segment_pcm.len() >= PREVIEW_MIN_SAMPLES;
                            let cooled =
                                last_preview_at.is_none_or(|t| t.elapsed() >= PREVIEW_MIN_INTERVAL);
                            if big_enough && cooled {
                                if segment_lang.is_none() {
                                    segment_lang =
                                        resolve_segment_lang(&stt, &selection_seed, &segment_pcm);
                                }
                                let preview_pcm = segment_pcm.clone();
                                let lang = segment_lang.clone();
                                let stt2 = Arc::clone(&stt);
                                let res = tokio::task::spawn_blocking(move || {
                                    decode_blocking(
                                        &stt2,
                                        &preview_pcm,
                                        sample_rate,
                                        lang.as_deref(),
                                    )
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
                                    .with_language(segment_lang.clone());
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
                                if segment_lang.is_none() {
                                    segment_lang =
                                        resolve_segment_lang(&stt, &selection_seed, &segment_pcm);
                                }
                                let mut la_final = LocalAgreement::new();
                                let mut last_text = String::new();
                                for _ in 0..2 {
                                    let pcm = segment_pcm.clone();
                                    let lang = segment_lang.clone();
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
                                let final_text = if stable.is_empty() { last_text } else { stable };
                                let upd = TranscriptUpdate::finalize(
                                    segment_index,
                                    final_text,
                                    started.elapsed(),
                                )
                                .with_language(segment_lang.clone());
                                let _ = tx.send(upd);
                            }
                            segment_pcm.clear();
                            agreement.reset();
                            last_preview_at = None;
                            segment_lang = None;
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

    /// Resolve the allow-list to a single locked code for the current
    /// segment, calling `lang_detect` once. Returns the forced code
    /// directly, `None` for `Auto`, and the detector's pick for
    /// `AllowList`. Errors degrade to the allow-list's first entry so
    /// streaming never aborts mid-session over a transient detect
    /// failure.
    fn resolve_segment_lang(
        stt: &Arc<WhisperLocal>,
        selection: &LanguageSelection,
        pcm: &[f32],
    ) -> Option<String> {
        if matches!(selection, LanguageSelection::Auto) {
            return None;
        }
        let Ok(guard) = stt.ctx.lock() else {
            return selection.fallback_hint().map(str::to_string);
        };
        let Some(ctx) = guard.as_ref() else {
            return selection.fallback_hint().map(str::to_string);
        };
        match resolve_language(ctx, selection, pcm, stt.threads) {
            Ok(opt) => opt,
            Err(e) => {
                tracing::warn!(
                    "lang_detect failed mid-stream ({e}); falling back to {:?}",
                    selection.fallback_hint()
                );
                selection.fallback_hint().map(str::to_string)
            }
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
        // Same hallucination guards as the batch path.
        params.set_no_speech_thold(0.6);
        params.set_logprob_thold(-1.0);
        params.set_temperature_inc(0.2);
        let lang_code = lang.filter(|l| *l != "auto");
        let prompt = stt.resolve_prompt(lang_code);
        if let Some(p) = prompt.as_deref() {
            params.set_initial_prompt(p);
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
                languages: self.languages.clone(),
                prompts: self.prompts.clone(),
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
            assert_eq!(
                strip_whisper_artifacts("hello (laughs) world"),
                "hello world"
            );
        }

        #[test]
        fn preserves_mixed_case_user_content() {
            // Mixed case → user dictated "see Fig 3", whisper rendered it.
            assert_eq!(strip_whisper_artifacts("see (Fig 3)"), "see (Fig 3)");
            assert_eq!(
                strip_whisper_artifacts("note [version 2]"),
                "note [version 2]"
            );
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
