// SPDX-License-Identifier: GPL-3.0-only
//! Groq streaming STT via "pseudo-stream" — Groq has no native streaming
//! endpoint today, so this backend implements [`StreamingStt`] by
//! re-POSTing the trailing N seconds of buffered audio to the existing
//! batch endpoint every ~700 ms, piping each decode through
//! [`LocalAgreement`] to produce a stable token-prefix preview.
//!
//! Plan: `plans/2026-04-28-wave-3-slice-b1-v1.md` Thread B (R4.2 of
//! `plans/2026-04-27-fono-interactive-v1.md`). ADR
//! `docs/decisions/0020-groq-pseudo-stream.md` captures the
//! design trade-offs (pseudo-stream vs WebSocket, 700 ms cadence,
//! in-flight cap = 1).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use tokio::sync::mpsc;
use tokio::sync::Mutex as AsyncMutex;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::groq::{
    groq_post_wav, groq_post_wav_verbose, warm_client, GroqResponse, GroqVerboseResponse,
    BACKEND_KEY,
};
use crate::lang::LanguageSelection;
use crate::lang_cache::LanguageCache;
use crate::streaming::{LocalAgreement, StreamFrame, StreamingStt, TranscriptUpdate};

/// Trailing audio window (in samples at 16 kHz) we re-POST on each
/// preview cadence tick. Groq's batch endpoint enforces a 30-second
/// per-request cap on cheaper models; we cap a touch under that to
/// leave headroom. Long segments will hit a `SegmentBoundary` from the
/// VAD before this matters in practice.
const TRAILING_WINDOW_SAMPLES: usize = 16_000 * 28;

/// Minimum buffered audio before the first preview decode. Smaller
/// than the local backend's threshold (0.8 s) because Groq's
/// round-trip is ~150 ms — we want to cover the first word as fast as
/// the network allows.
const PREVIEW_MIN_SAMPLES: usize = 16_000 * 6 / 10;

/// Wall-clock cadence between preview re-POSTs. R4.2 picked 700 ms as
/// the sweet spot between latency and API cost (~25% overhead vs the
/// equivalent batch POST). Captured in ADR 0020.
const PSEUDO_STREAM_INTERVAL: Duration = Duration::from_millis(700);

/// Owned-future helper alias for the request closure signature. Each
/// call returns a `Pin<Box<dyn Future + Send>>` so the closure can be
/// stored behind a trait object and the streaming task can `.await`
/// it without lifetime juggling.
pub type GroqRequestFuture = Pin<Box<dyn Future<Output = Result<GroqResponse>> + Send + 'static>>;

/// Closure type used by [`GroqStreaming`] to issue a single
/// transcription request. Production callers use the real Groq HTTPS
/// path via [`GroqStreaming::new`]; tests and the equivalence cloud
/// mock (Slice B1 / Thread C) inject a recorded-HTTP closure via
/// [`GroqStreaming::with_request_fn`].
pub type GroqRequestFn = Arc<dyn Fn(Vec<u8>, Option<String>) -> GroqRequestFuture + Send + Sync>;

/// Owned-future helper alias for the verbose-rerun closure. Returns
/// `GroqVerboseResponse` so the per-peer rerun lane can score
/// candidates by `avg_logprob`.
pub type GroqVerboseFuture =
    Pin<Box<dyn Future<Output = Result<GroqVerboseResponse>> + Send + 'static>>;

/// Verbose-mode counterpart of [`GroqRequestFn`]. Optional; tests that
/// only exercise the streaming pump (not the rerun lane) leave this
/// unset and reruns become no-ops.
pub type GroqVerboseFn = Arc<dyn Fn(Vec<u8>, Option<String>) -> GroqVerboseFuture + Send + Sync>;

/// Streaming wrapper around the Groq batch endpoint. Implements
/// [`StreamingStt`] by re-POSTing the trailing
/// [`TRAILING_WINDOW_SAMPLES`] every [`PSEUDO_STREAM_INTERVAL`] (with
/// an in-flight cap of 1 — overlap drops the would-be preview) and
/// piping results through [`LocalAgreement`].
pub struct GroqStreaming {
    request_fn: GroqRequestFn,
    /// Verbose-mode request closure used by the per-peer rerun lane.
    /// `None` for tests that only need the streaming pump; reruns
    /// become no-ops in that case.
    verbose_fn: Option<GroqVerboseFn>,
    languages: Vec<String>,
    cloud_force_primary: bool,
    cloud_rerun_on_mismatch: bool,
    lang_cache: Arc<LanguageCache>,
    /// Diagnostic counter — incremented every time a 700 ms cadence
    /// tick wanted to fire a preview but found the prior request
    /// still in flight. Surfaced via [`Self::preview_skipped_count`]
    /// so `fono doctor` (or a follow-up commit) can flag chronically
    ///-bursty audio.
    preview_skipped_count: Arc<AtomicU64>,
}

impl GroqStreaming {
    /// Construct a real-API Groq pseudo-stream backend. The closure
    /// captures `api_key` + `model` + a warmed `reqwest::Client`
    /// (HTTP/2 keep-alive via [`crate::groq::warm_client`]).
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        let api_key = api_key.into();
        let model = model.into();
        let client = warm_client();
        let request_fn: GroqRequestFn = {
            let client = client.clone();
            let api_key = api_key.clone();
            let model = model.clone();
            Arc::new(move |wav: Vec<u8>, lang: Option<String>| {
                let client = client.clone();
                let api_key = api_key.clone();
                let model = model.clone();
                Box::pin(async move {
                    groq_post_wav(&client, &api_key, &model, &wav, lang.as_deref()).await
                }) as GroqRequestFuture
            })
        };
        let verbose_fn: GroqVerboseFn = Arc::new(move |wav: Vec<u8>, lang: Option<String>| {
            let client = client.clone();
            let api_key = api_key.clone();
            let model = model.clone();
            Box::pin(async move {
                groq_post_wav_verbose(&client, &api_key, &model, &wav, lang.as_deref()).await
            }) as GroqVerboseFuture
        });
        Self {
            request_fn,
            verbose_fn: Some(verbose_fn),
            languages: Vec::new(),
            cloud_force_primary: false,
            cloud_rerun_on_mismatch: false,
            lang_cache: LanguageCache::global(),
            preview_skipped_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Test / cloud-mock entry point. The supplied closure replaces
    /// the real HTTPS request. Used by the equivalence harness's
    /// `--stt cloud-mock --provider groq` mode (Slice B1 / Thread C)
    /// and by the unit tests in this module.
    #[must_use]
    pub fn with_request_fn(request_fn: GroqRequestFn) -> Self {
        Self {
            request_fn,
            verbose_fn: None,
            languages: Vec::new(),
            cloud_force_primary: false,
            cloud_rerun_on_mismatch: false,
            lang_cache: LanguageCache::global(),
            preview_skipped_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Test entry point pairing a `request_fn` with a verbose-mode
    /// closure for exercising the per-peer rerun lane.
    #[must_use]
    pub fn with_request_and_verbose_fn(
        request_fn: GroqRequestFn,
        verbose_fn: GroqVerboseFn,
    ) -> Self {
        Self {
            request_fn,
            verbose_fn: Some(verbose_fn),
            languages: Vec::new(),
            cloud_force_primary: false,
            cloud_rerun_on_mismatch: false,
            lang_cache: LanguageCache::global(),
            preview_skipped_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Builder: language allow-list. Same semantics as
    /// [`crate::groq::GroqStt::with_languages`].
    #[must_use]
    pub fn with_languages(mut self, codes: Vec<String>) -> Self {
        self.languages = codes;
        self
    }

    /// Builder: force the primary code on the first request when the
    /// allow-list has > 1 entry.
    #[must_use]
    pub fn with_cloud_force_primary(mut self, on: bool) -> Self {
        self.cloud_force_primary = on;
        self
    }

    /// Builder: re-issue the request with a cached peer code if the
    /// provider returned a banned language (finalize lane only —
    /// preview lane skips re-runs to keep cadence tight).
    #[must_use]
    pub fn with_cloud_rerun_on_mismatch(mut self, on: bool) -> Self {
        self.cloud_rerun_on_mismatch = on;
        self
    }

    /// Builder: inject a specific language cache (tests + bench).
    #[must_use]
    pub fn with_lang_cache(mut self, cache: Arc<LanguageCache>) -> Self {
        self.lang_cache = cache;
        self
    }

    /// Snapshot of the diagnostic counter — number of preview cadence
    /// ticks that were dropped because the prior request was still
    /// in flight. Documented in ADR 0020.
    #[must_use]
    pub fn preview_skipped_count(&self) -> u64 {
        self.preview_skipped_count.load(Ordering::Relaxed)
    }

    fn effective_selection(&self, lang_override: Option<&str>) -> LanguageSelection {
        LanguageSelection::from_config(&self.languages).with_override(lang_override)
    }
}

#[async_trait]
impl StreamingStt for GroqStreaming {
    #[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
    async fn stream_transcribe(
        &self,
        mut frames: BoxStream<'static, StreamFrame>,
        sample_rate: u32,
        lang: Option<String>,
    ) -> Result<BoxStream<'static, TranscriptUpdate>> {
        let (tx, rx) = mpsc::unbounded_channel::<TranscriptUpdate>();

        let request_fn = Arc::clone(&self.request_fn);
        let verbose_fn = self.verbose_fn.clone();
        let preview_skipped = Arc::clone(&self.preview_skipped_count);
        let selection = self.effective_selection(lang.as_deref());
        let cloud_force_primary = self.cloud_force_primary;
        let cloud_rerun_on_mismatch = self.cloud_rerun_on_mismatch;
        let lang_cache = Arc::clone(&self.lang_cache);
        let started = Instant::now();
        // In-flight cap = 1 for the *preview* lane: an AtomicBool that
        // the frame loop swap-sets before spawning a preview request,
        // and the preview task clears on completion. A separate
        // AsyncMutex serialises the finalize lane after any pending
        // preview so they don't race the same `tx` ordering.
        let in_flight = Arc::new(AtomicBool::new(false));
        let finalize_gate = Arc::new(AsyncMutex::new(()));

        // First-pass language to send: forced -> the code; auto -> none;
        // allow-list -> primary if cloud_force_primary, else None and
        // we accept the provider's pick. Mirrors the batch path at
        // crates/fono-stt/src/groq.rs:116-126.
        let first_pass_lang: Option<String> = match &selection {
            LanguageSelection::Auto => None,
            LanguageSelection::Forced(c) => Some(c.clone()),
            LanguageSelection::AllowList(_) => {
                if cloud_force_primary {
                    selection.fallback_hint().map(str::to_string)
                } else {
                    None
                }
            }
        };

        tokio::spawn(async move {
            let mut segment_index: u32 = 0;
            let mut segment_pcm: Vec<f32> = Vec::with_capacity(16_000 * 30);
            let mut last_preview_at: Option<Instant> = None;
            let agreement = Arc::new(std::sync::Mutex::new(LocalAgreement::new()));
            let mut last_decoded_len: usize = 0;

            while let Some(frame) = frames.next().await {
                match frame {
                    StreamFrame::Pcm(chunk) => {
                        segment_pcm.extend_from_slice(&chunk);
                        let big_enough = segment_pcm.len() >= PREVIEW_MIN_SAMPLES;
                        let cooled =
                            last_preview_at.is_none_or(|t| t.elapsed() >= PSEUDO_STREAM_INTERVAL);
                        let grew = segment_pcm.len() > last_decoded_len;
                        if !(big_enough && cooled && grew) {
                            continue;
                        }
                        // In-flight cap = 1: an AtomicBool swap-set
                        // here, cleared by the spawned preview task
                        // on completion. Concurrent preview attempts
                        // hit `was_set = true`, increment the
                        // skipped counter, and bail. This is the
                        // documented design — drop-on-overlap is
                        // preferable to queueing because the audio
                        // thread feeds us bursty tail-extensions.
                        let was_in_flight = in_flight.swap(true, Ordering::AcqRel);
                        if was_in_flight {
                            preview_skipped.fetch_add(1, Ordering::Relaxed);
                            last_preview_at = Some(Instant::now());
                            continue;
                        }
                        let trailing = trailing_slice(&segment_pcm).to_vec();
                        let wav = crate::groq::encode_wav(&trailing, sample_rate);
                        last_decoded_len = segment_pcm.len();
                        last_preview_at = Some(Instant::now());
                        let request_fn_p = Arc::clone(&request_fn);
                        let agreement_p = Arc::clone(&agreement);
                        let in_flight_p = Arc::clone(&in_flight);
                        let finalize_gate_p = Arc::clone(&finalize_gate);
                        let tx_p = tx.clone();
                        let lang_p = first_pass_lang.clone();
                        let seg_idx = segment_index;
                        let allow_list_p: Option<Vec<String>> = match &selection {
                            LanguageSelection::AllowList(v) => Some(v.clone()),
                            _ => None,
                        };
                        // Spawn the request as a detached task so the
                        // frame loop keeps consuming PCM while Groq
                        // round-trips. Holds `finalize_gate` for its
                        // lifetime; finalize waits on the same gate.
                        tokio::spawn(async move {
                            let _gate = finalize_gate_p.lock().await;
                            let res = (request_fn_p)(wav, lang_p).await;
                            match res {
                                Ok(resp) => {
                                    // Suppress preview when the
                                    // detected language is outside the
                                    // allow-list. The overlay would
                                    // otherwise flash garbage in the
                                    // wrong language while the user
                                    // is still speaking; finalize will
                                    // run the per-peer rerun and emit
                                    // the corrected text. Only LCP
                                    // bookkeeping is skipped — the
                                    // next preview can still promote
                                    // the right tokens once Groq's
                                    // detection settles.
                                    if let (Some(allow), Some(detected)) =
                                        (allow_list_p.as_ref(), resp.language.as_deref())
                                    {
                                        let detected_lc = detected.trim().to_ascii_lowercase();
                                        let in_list = allow
                                            .iter()
                                            .any(|c| c.eq_ignore_ascii_case(&detected_lc));
                                        if !in_list {
                                            tracing::info!(
                                                "groq preview: detected banned language \
                                                 {detected:?} (allow-list {allow:?}); \
                                                 suppressing overlay update"
                                            );
                                            in_flight_p.store(false, Ordering::Release);
                                            return;
                                        }
                                    }
                                    let tokens = whitespace_tokens(&resp.text);
                                    let stable_text = {
                                        let mut g = agreement_p.lock().unwrap();
                                        g.observe(tokens.iter().cloned());
                                        g.stable().join(" ")
                                    };
                                    let preview_text = if stable_text.is_empty() {
                                        resp.text.clone()
                                    } else {
                                        stable_text
                                    };
                                    let upd = TranscriptUpdate::preview(
                                        seg_idx,
                                        preview_text,
                                        started.elapsed(),
                                    )
                                    .with_language(resp.language);
                                    let _ = tx_p.send(upd);
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "groq pseudo-stream: preview decode failed: {e:#}"
                                    );
                                }
                            }
                            in_flight_p.store(false, Ordering::Release);
                        });
                    }
                    StreamFrame::SegmentBoundary | StreamFrame::Eof => {
                        if !segment_pcm.is_empty() {
                            // Finalize: a single decode of the full
                            // segment audio. Groq's batch endpoint is
                            // deterministic per input, so a "dual
                            // pass" wouldn't reduce noise — one
                            // request is the cost-correct call.
                            let wav = crate::groq::encode_wav(&segment_pcm, sample_rate);
                            // Wait for any in-flight preview to settle
                            // before finalize so the per-segment tx
                            // order is preview…preview…finalize.
                            let _gate = finalize_gate.lock().await;
                            let req = (request_fn)(wav.clone(), first_pass_lang.clone());
                            match req.await {
                                Ok(mut resp) => {
                                    // Allow-list post-validation. v3.1:
                                    // confidence-aware rerun. On
                                    // banned detection, issue one
                                    // verbose request per peer and
                                    // pick the highest avg_logprob.
                                    // No-op when verbose_fn is None
                                    // (test-only `with_request_fn`
                                    // construction).
                                    if let (LanguageSelection::AllowList(peers), Some(detected)) =
                                        (&selection, resp.language.as_deref())
                                    {
                                        if selection.contains(detected) {
                                            lang_cache.record(BACKEND_KEY, detected);
                                        } else if cloud_rerun_on_mismatch {
                                            tracing::info!(
                                                "groq returned banned language {detected:?} \
                                                 on finalize (allow-list {peers:?}); \
                                                 reranking by per-peer avg_logprob"
                                            );
                                            if let Some(vfn) = verbose_fn.as_ref() {
                                                let mut best: Option<(f32, String, String)> = None;
                                                for peer in peers {
                                                    let req2 =
                                                        (vfn)(wav.clone(), Some(peer.clone()));
                                                    match req2.await {
                                                        Ok(verbose) => {
                                                            let score = verbose.mean_logprob();
                                                            tracing::info!(
                                                                "groq finalize rerun candidate \
                                                                 language={peer}: \
                                                                 avg_logprob={score:.3}"
                                                            );
                                                            if best
                                                                .as_ref()
                                                                .is_none_or(|(s, _, _)| score > *s)
                                                            {
                                                                best = Some((
                                                                    score,
                                                                    peer.clone(),
                                                                    verbose.text,
                                                                ));
                                                            }
                                                        }
                                                        Err(e) => {
                                                            tracing::warn!(
                                                                "groq finalize rerun candidate \
                                                                 language={peer} failed: {e:#}"
                                                            );
                                                        }
                                                    }
                                                }
                                                if let Some((_, picked, text)) = best {
                                                    lang_cache.record(BACKEND_KEY, &picked);
                                                    resp = GroqResponse {
                                                        text,
                                                        language: Some(picked),
                                                    };
                                                } else {
                                                    tracing::warn!(
                                                        "groq finalize rerun: every peer \
                                                         attempt failed; falling back to \
                                                         unforced response"
                                                    );
                                                }
                                            } else {
                                                tracing::debug!(
                                                    "groq finalize: verbose_fn unset (test \
                                                     harness); skipping rerun"
                                                );
                                            }
                                        }
                                    }
                                    let upd = TranscriptUpdate::finalize(
                                        segment_index,
                                        resp.text,
                                        started.elapsed(),
                                    )
                                    .with_language(resp.language);
                                    let _ = tx.send(upd);
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "groq pseudo-stream: finalize decode failed: {e:#}"
                                    );
                                }
                            }
                        }
                        segment_pcm.clear();
                        agreement.lock().unwrap().reset();
                        last_preview_at = None;
                        last_decoded_len = 0;
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
        "groq-streaming"
    }
}

fn trailing_slice(buf: &[f32]) -> &[f32] {
    if buf.len() <= TRAILING_WINDOW_SAMPLES {
        buf
    } else {
        &buf[buf.len() - TRAILING_WINDOW_SAMPLES..]
    }
}

fn whitespace_tokens(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use super::*;

    /// Build a `GroqRequestFn` that returns successive responses from
    /// a script and panics if asked for more than `script.len()`
    /// times. Lets each test pin down exactly how many decodes ran.
    fn scripted(script: Vec<&'static str>) -> (GroqRequestFn, Arc<StdMutex<usize>>) {
        let counter = Arc::new(StdMutex::new(0usize));
        let counter_cb = Arc::clone(&counter);
        let script: Arc<Vec<String>> = Arc::new(script.into_iter().map(String::from).collect());
        let f: GroqRequestFn = Arc::new(move |_wav, _lang| {
            let counter = Arc::clone(&counter_cb);
            let script = Arc::clone(&script);
            Box::pin(async move {
                let text = {
                    let mut g = counter.lock().unwrap();
                    let idx = *g;
                    *g += 1;
                    let t = script.get(idx).cloned().unwrap_or_else(|| {
                        panic!(
                            "scripted GroqRequestFn called {} times; script has {} entries",
                            idx + 1,
                            script.len()
                        )
                    });
                    drop(g);
                    t
                };
                Ok(GroqResponse {
                    text,
                    language: Some("en".into()),
                })
            }) as GroqRequestFuture
        });
        (f, counter)
    }

    fn pcm(seconds: f32) -> Vec<f32> {
        vec![0.1_f32; (16_000.0 * seconds) as usize]
    }

    #[tokio::test]
    async fn three_previews_promote_lcp_then_finalize_emits_full_text() {
        let (req, counter) = scripted(vec![
            "the",
            "the quick",
            "the quick brown",
            "the quick brown fox",
        ]);
        let backend = GroqStreaming::with_request_fn(req);

        // Drive the pump synchronously: 3 PCM frames each big enough
        // to trip PREVIEW_MIN_SAMPLES, then SegmentBoundary, Eof.
        // Sleeps between PCM frames satisfy the 700 ms cadence guard.
        let (tx, rx) = mpsc::unbounded_channel::<StreamFrame>();
        let frames: BoxStream<'static, StreamFrame> = UnboundedReceiverStream::new(rx).boxed();
        let stream = backend
            .stream_transcribe(frames, 16_000, None)
            .await
            .unwrap();

        // First chunk (~1 s): triggers preview #1 immediately.
        tx.send(StreamFrame::Pcm(pcm(1.0))).unwrap();
        tokio::time::sleep(Duration::from_millis(800)).await;
        // Second chunk: cadence elapsed, triggers preview #2.
        tx.send(StreamFrame::Pcm(pcm(1.0))).unwrap();
        tokio::time::sleep(Duration::from_millis(800)).await;
        // Third chunk: cadence elapsed, triggers preview #3.
        tx.send(StreamFrame::Pcm(pcm(1.0))).unwrap();
        tokio::time::sleep(Duration::from_millis(800)).await;
        // Boundary fires the finalize decode (#4).
        tx.send(StreamFrame::SegmentBoundary).unwrap();
        tx.send(StreamFrame::Eof).unwrap();
        drop(tx);

        let updates: Vec<TranscriptUpdate> = stream.collect().await;
        assert_eq!(*counter.lock().unwrap(), 4, "expected exactly 4 decodes");

        // Three preview updates + one finalize.
        let previews: Vec<&TranscriptUpdate> = updates
            .iter()
            .filter(|u| u.lane == crate::streaming::UpdateLane::Preview)
            .collect();
        let finalizes: Vec<&TranscriptUpdate> = updates
            .iter()
            .filter(|u| u.lane == crate::streaming::UpdateLane::Finalize)
            .collect();
        assert_eq!(previews.len(), 3, "got previews: {previews:?}");
        assert_eq!(finalizes.len(), 1);
        // After observation #2 ("the" + "the quick"), LocalAgreement
        // promotes "the" to stable; after #3 ("the quick brown"),
        // "the quick" is stable. Preview #1 has no stable prefix yet
        // (need 2 observations) so it falls back to the raw text.
        assert_eq!(previews[0].text, "the");
        assert_eq!(previews[1].text, "the");
        assert_eq!(previews[2].text, "the quick");
        // Finalize is the 4th scripted response, untouched.
        assert_eq!(finalizes[0].text, "the quick brown fox");
    }

    #[tokio::test]
    async fn in_flight_cap_drops_overlap_and_increments_counter() {
        // The mock holds the AsyncMutex for ~500 ms; meanwhile we
        // send 5 PCM chunks back-to-back. Only the first acquires
        // the in_flight guard; the next four bump the
        // preview_skipped counter and exit early.
        let started = Arc::new(AsyncMutex::new(()));
        let started_cb = Arc::clone(&started);
        let f: GroqRequestFn = Arc::new(move |_wav, _lang| {
            let s = Arc::clone(&started_cb);
            Box::pin(async move {
                let _g = s.lock().await;
                tokio::time::sleep(Duration::from_millis(3000)).await;
                Ok(GroqResponse {
                    text: "slow".into(),
                    language: None,
                })
            }) as GroqRequestFuture
        });
        let backend = GroqStreaming::with_request_fn(f);
        let counter = Arc::clone(&backend.preview_skipped_count);

        let (tx, rx) = mpsc::unbounded_channel::<StreamFrame>();
        let frames: BoxStream<'static, StreamFrame> = UnboundedReceiverStream::new(rx).boxed();
        let _stream = backend
            .stream_transcribe(frames, 16_000, None)
            .await
            .unwrap();

        // First chunk acquires the guard and pins the request.
        tx.send(StreamFrame::Pcm(pcm(1.0))).unwrap();
        // Yield so the spawned task picks up the chunk and grabs
        // the in-flight guard before we send overlap chunks.
        tokio::time::sleep(Duration::from_millis(50)).await;
        // Overlap chunks: each must also be cooled (700 ms apart) and
        // must each grow segment_pcm — sleep + send.
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(750)).await;
            tx.send(StreamFrame::Pcm(pcm(1.0))).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        let dropped = counter.load(Ordering::Relaxed);
        assert!(
            dropped >= 1,
            "expected at least one preview drop while request is in flight; got {dropped}",
        );

        drop(tx);
    }
}
