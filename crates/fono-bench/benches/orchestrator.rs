// SPDX-License-Identifier: GPL-3.0-only
//! Criterion bench — measures the overhead of the bench runner itself
//! (and, by proxy once the orchestrator from
//! `docs/plans/2026-04-25-fono-pipeline-wiring-v1.md` lands, the
//! production pipeline overhead) against a fake STT/LLM pair.
//!
//! This is the network-free, deterministic CI gate referenced as Task
//! L27 in `docs/plans/2026-04-25-fono-latency-v1.md`. Budget:
//! orchestrator overhead + injection + history write < 50 ms p95 over
//! the sum of fake STT/LLM latencies.

use std::sync::Arc;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};
use tokio::runtime::Runtime;

use fono_bench::fakes::{FakeLlm, FakeStt};
use fono_bench::runner::BenchRunner;

fn bench_runner_no_audio_io(c: &mut Criterion) {
    // We synthesise a 3-second 16 kHz mono buffer in memory and feed it
    // straight into a runner that bypasses fixture I/O. We do this by
    // calling the STT trait directly — the bench thus measures the
    // tokio-task hop + trait dispatch + WER computation, which is the
    // smallest harness we can build to gate orchestrator regressions
    // before the real orchestrator exists.
    let rt = Runtime::new().unwrap();

    let stt = Arc::new(FakeStt::with_delay(
        "the quick brown fox jumps over the lazy dog",
        Duration::from_millis(100),
    ));
    let llm = Arc::new(FakeLlm::with_delay(Duration::from_millis(50)));

    c.bench_function("fake_pipeline_3s_clip", |b| {
        b.to_async(&rt).iter(|| {
            let stt = Arc::clone(&stt);
            let llm = Arc::clone(&llm);
            async move {
                use fono_llm::traits::{FormatContext, TextFormatter};
                use fono_stt::traits::SpeechToText;
                let pcm = vec![0.0f32; 16_000 * 3];
                let trans = stt.transcribe(&pcm, 16_000, Some("en")).await.unwrap();
                let cleaned = llm
                    .format(&trans.text, &FormatContext::default())
                    .await
                    .unwrap();
                let _wer = fono_bench::word_error_rate(
                    "the quick brown fox jumps over the lazy dog",
                    &cleaned,
                );
            }
        });
    });

    // Tighter loop without the LLM stage — STT-only path.
    c.bench_function("fake_stt_only_3s_clip", |b| {
        let stt2 = Arc::new(FakeStt::with_delay(
            "the quick brown fox",
            Duration::from_millis(50),
        ));
        b.to_async(&rt).iter(|| {
            let stt = Arc::clone(&stt2);
            async move {
                use fono_stt::traits::SpeechToText;
                let pcm = vec![0.0f32; 16_000 * 3];
                let _ = stt.transcribe(&pcm, 16_000, Some("en")).await.unwrap();
            }
        });
    });

    // Sanity: BenchRunner construction is allocation-light.
    c.bench_function("bench_runner_construct", |b| {
        b.iter(|| {
            let stt: Arc<dyn fono_stt::SpeechToText> = Arc::new(FakeStt::new("x"));
            let _r = BenchRunner::new(stt, "/tmp/fono-bench-criterion-noop");
        });
    });
}

criterion_group!(benches, bench_runner_no_audio_io);
criterion_main!(benches);
