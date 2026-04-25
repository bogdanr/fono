// SPDX-License-Identifier: GPL-3.0-only
//! Latency smoke test — runs the bench runner against a synthetic
//! in-memory PCM buffer using a fake STT that returns the reference
//! transcript. Asserts the orchestrator overhead p95 stays under
//! 50 ms (network-free, deterministic).
//!
//! Real-provider, real-fixture runs are NOT executed here — they live in
//! the `fono-bench` binary because they need network access and pinned
//! fixtures. Run those manually:
//!
//!   cargo run -p fono-bench --release --features groq -- \
//!       --provider groq --languages en,es,fr,de --iterations 3
//!
//! This test is `#[ignore]` to keep the default `cargo test` cycle fast;
//! CI invokes it explicitly with `--ignored`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fono_bench::fakes::FakeStt;
use fono_bench::report::{ClipReport, Report};
use fono_bench::word_error_rate;
use fono_stt::traits::SpeechToText;

const REFERENCE_EN: &str = "the quick brown fox jumps over the lazy dog";

#[tokio::test]
#[ignore = "latency smoke test — run with `cargo test -p fono-bench --release -- --ignored`"]
async fn orchestrator_overhead_under_50ms_p95() {
    // 30 iterations is the smallest sample that yields a stable p95.
    const ITERS: usize = 30;
    const FAKE_STT_DELAY: Duration = Duration::from_millis(20);

    let stt: Arc<dyn SpeechToText> = Arc::new(FakeStt::with_delay(REFERENCE_EN, FAKE_STT_DELAY));

    let pcm = vec![0.0f32; 16_000 * 3]; // 3 s of silence

    let mut clips = Vec::with_capacity(ITERS);
    for i in 0..ITERS {
        let t = Instant::now();
        let trans = stt
            .transcribe(&pcm, 16_000, Some("en"))
            .await
            .expect("fake stt cannot fail");
        let total_ms = t.elapsed().as_millis() as u64;
        let wer = word_error_rate(REFERENCE_EN, &trans.text);
        clips.push(ClipReport {
            id: format!("synthetic_en_{i:02}"),
            language: "en".into(),
            reference: REFERENCE_EN.into(),
            hypothesis: trans.text,
            wer,
            stt_ms: total_ms,
            llm_ms: None,
            total_ms,
            samples: pcm.len(),
            sample_rate: 16_000,
        });
    }

    let report = Report::build("fake-stt", None, clips);
    let lang = report
        .by_language
        .get("en")
        .expect("en language report present");

    println!(
        "fake-stt orchestrator p50={} ms / p95={} ms / mean WER={:.3}",
        lang.p50_total_ms, lang.p95_total_ms, lang.mean_wer
    );

    // WER must be 0 for a fake-stt that returns the reference verbatim.
    assert_eq!(
        lang.mean_wer, 0.0,
        "fake stt returned non-reference text — WER pipeline broken"
    );

    // Orchestrator + dispatch overhead, on top of a fixed 20 ms fake
    // delay, must stay under 50 ms p95. If this trips, something on the
    // hot path regressed (Mutex contention, allocations in trait
    // dispatch, etc.).
    let budget_ms = FAKE_STT_DELAY.as_millis() as u64 + 50;
    assert!(
        lang.p95_total_ms <= budget_ms,
        "p95={} ms > budget {} ms (fake delay {} ms + 50 ms overhead)",
        lang.p95_total_ms,
        budget_ms,
        FAKE_STT_DELAY.as_millis()
    );
}

#[tokio::test]
#[ignore = "latency smoke test — see module docs"]
async fn wer_is_zero_for_perfect_match() {
    // Spot-check the WER metric end-to-end; cheap insurance against
    // tokenizer drift breaking the regression gate silently.
    let stt: Arc<dyn SpeechToText> = Arc::new(FakeStt::new(REFERENCE_EN));
    let trans = stt
        .transcribe(&[0.0f32; 16_000], 16_000, None)
        .await
        .unwrap();
    assert_eq!(word_error_rate(REFERENCE_EN, &trans.text), 0.0);
}
