// SPDX-License-Identifier: GPL-3.0-only
//! End-to-end orchestrator test: synthetic PCM → fake STT → fake LLM
//! → fake injector → history row. No microphone, no network.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use fono_core::config::{Config, LlmBackend};
use fono_core::history::HistoryDb;
use fono_llm::{FormatContext, TextFormatter};
use fono_stt::{SpeechToText, Transcription};

use fono::session::{orchestrator_for_test, FocusProbe, Injector, PipelineOutcome};

struct FakeStt {
    text: String,
    lang: Option<String>,
}

#[async_trait]
impl SpeechToText for FakeStt {
    async fn transcribe(
        &self,
        _pcm: &[f32],
        _sr: u32,
        _lang: Option<&str>,
    ) -> Result<Transcription> {
        Ok(Transcription {
            text: self.text.clone(),
            language: self.lang.clone(),
            duration_ms: None,
        })
    }
    fn name(&self) -> &'static str {
        "fake-stt"
    }
}

struct FakeLlm;

#[async_trait]
impl TextFormatter for FakeLlm {
    async fn format(&self, raw: &str, _ctx: &FormatContext) -> Result<String> {
        Ok(format!("CLEANED: {}", raw.trim()))
    }
    fn name(&self) -> &'static str {
        "fake-llm"
    }
}

struct CapturingInjector(Arc<Mutex<Vec<String>>>);

impl Injector for CapturingInjector {
    fn inject(&self, text: &str) -> Result<bool> {
        self.0.lock().unwrap().push(text.to_string());
        Ok(true)
    }
}

struct StubFocus;

impl FocusProbe for StubFocus {
    fn probe(&self) -> (Option<String>, Option<String>) {
        (Some("Slack".into()), Some("general".into()))
    }
}

#[tokio::test]
async fn pipeline_produces_history_row_and_injects_cleaned_text() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("history.sqlite");

    let stt: Arc<dyn SpeechToText> = Arc::new(FakeStt {
        text: "hello world".into(),
        lang: Some("en".into()),
    });
    let llm: Option<Arc<dyn TextFormatter>> = Some(Arc::new(FakeLlm));
    let injected = Arc::new(Mutex::new(Vec::<String>::new()));
    let injector = Arc::new(CapturingInjector(Arc::clone(&injected)));

    let mut cfg = Config::default();
    cfg.llm.enabled = true;
    cfg.llm.backend = LlmBackend::OpenAI; // anything non-None
    // Force the LLM to run regardless of the default short-utterance
    // skip threshold; this test covers the cleaned-output path.
    cfg.llm.skip_if_words_lt = 0;
    let cfg = Arc::new(cfg);

    let (orch, _action_rx) = orchestrator_for_test(
        stt,
        llm,
        &db_path,
        Arc::clone(&cfg),
        injector,
        Arc::new(StubFocus),
    );

    // 1 second of silence at 16 kHz to drive the pipeline.
    let pcm = vec![0.0_f32; 16_000];
    let outcome = orch.run_oneshot(pcm, 1000).await;

    let metrics = match outcome {
        PipelineOutcome::Completed {
            raw,
            cleaned,
            metrics,
        } => {
            assert_eq!(raw, "hello world");
            assert_eq!(cleaned.as_deref(), Some("CLEANED: hello world"));
            metrics
        }
        other => panic!("expected Completed, got {other:?}"),
    };
    assert_eq!(metrics.samples, 16_000);
    assert!(metrics.capture_ms > 0);

    // Injection received cleaned text.
    let captured = injected.lock().unwrap().clone();
    assert_eq!(captured, vec!["CLEANED: hello world".to_string()]);

    // History row landed with the right backend names.
    let db = HistoryDb::open(&db_path).unwrap();
    let rows = db.recent(10).unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.raw, "hello world");
    assert_eq!(row.cleaned.as_deref(), Some("CLEANED: hello world"));
    assert_eq!(row.stt_backend.as_deref(), Some("fake-stt"));
    assert_eq!(row.llm_backend.as_deref(), Some("fake-llm"));
    assert_eq!(row.app_class.as_deref(), Some("Slack"));
    assert_eq!(row.language.as_deref(), Some("en"));
}

struct ClarifyingLlm;

#[async_trait]
impl TextFormatter for ClarifyingLlm {
    async fn format(&self, _raw: &str, _ctx: &FormatContext) -> Result<String> {
        // Mirror the exact failure mode the bug report captured: a
        // chat-tuned model responding with a clarification question
        // instead of a cleaned transcript. The real backends (`OpenAiCompat`,
        // `AnthropicLlm`, `LlamaLocal`) detect this via
        // `looks_like_clarification` and bail with `Err`; reproduce that
        // here so the pipeline-level fallback is exercised end-to-end.
        anyhow::bail!(
            "openai-compat LLM returned a clarification reply instead of a cleaned transcript; \
             falling back to raw text. response: \"It seems like you're describing a situation, \
             but the details are incomplete. Could you provide the full text you're referring to, \
             so I can better understand and assist you?\""
        )
    }
    fn name(&self) -> &'static str {
        "fake-clarifying-llm"
    }
}

/// Plan task 6 — when the LLM backend rejects a clarification reply
/// (the F8 push-to-talk failure mode), the pipeline must inject the
/// raw STT text rather than the meta-question.
#[tokio::test]
async fn pipeline_falls_back_to_raw_when_llm_rejects_clarification() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("history.sqlite");

    // Four words so it's above the new `skip_if_words_lt = 3` default
    // and the LLM is actually invoked.
    let stt: Arc<dyn SpeechToText> = Arc::new(FakeStt {
        text: "the response is this".into(),
        lang: Some("en".into()),
    });
    let llm: Option<Arc<dyn TextFormatter>> = Some(Arc::new(ClarifyingLlm));
    let injected = Arc::new(Mutex::new(Vec::<String>::new()));
    let injector = Arc::new(CapturingInjector(Arc::clone(&injected)));

    let mut cfg = Config::default();
    cfg.llm.enabled = true;
    cfg.llm.backend = LlmBackend::OpenAI;
    let cfg = Arc::new(cfg);

    let (orch, _rx) = orchestrator_for_test(
        stt,
        llm,
        &db_path,
        Arc::clone(&cfg),
        injector,
        Arc::new(StubFocus),
    );

    let outcome = orch.run_oneshot(vec![0.0_f32; 16_000], 1000).await;
    match outcome {
        PipelineOutcome::Completed { raw, cleaned, .. } => {
            assert_eq!(raw, "the response is this");
            assert!(
                cleaned.is_none(),
                "clarification refusal must not become the cleaned text, got: {cleaned:?}"
            );
        }
        other => panic!("expected Completed, got {other:?}"),
    }

    // Most important assertion: the meta-question never reaches the
    // injector — the user sees their raw transcript instead.
    let captured = injected.lock().unwrap().clone();
    assert_eq!(captured, vec!["the response is this".to_string()]);
    for line in &captured {
        assert!(
            !line.contains("Could you provide"),
            "clarification text leaked into injection: {line}"
        );
    }
}

/// Plan task 4 — a sub-`skip_if_words_lt` capture must skip the LLM
/// entirely so the F8 push-to-talk "okay" / "yes" case never reaches
/// a chat-tuned model in the first place.
#[tokio::test]
async fn pipeline_skips_llm_for_short_capture_under_default_threshold() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("history.sqlite");

    let stt: Arc<dyn SpeechToText> = Arc::new(FakeStt {
        text: "okay".into(),
        lang: Some("en".into()),
    });
    // If the LLM is invoked, the test fails: ClarifyingLlm bails, but
    // we'd still see `llm_skipped_short = false` in metrics.
    let llm: Option<Arc<dyn TextFormatter>> = Some(Arc::new(ClarifyingLlm));
    let injected = Arc::new(Mutex::new(Vec::<String>::new()));
    let injector = Arc::new(CapturingInjector(Arc::clone(&injected)));

    let mut cfg = Config::default();
    cfg.llm.enabled = true;
    cfg.llm.backend = LlmBackend::OpenAI;
    // Sanity-check the new default rather than hard-coding it.
    assert!(cfg.llm.skip_if_words_lt >= 3);
    let cfg = Arc::new(cfg);

    let (orch, _rx) = orchestrator_for_test(
        stt,
        llm,
        &db_path,
        Arc::clone(&cfg),
        injector,
        Arc::new(StubFocus),
    );

    let outcome = orch.run_oneshot(vec![0.0_f32; 16_000], 1000).await;
    match outcome {
        PipelineOutcome::Completed {
            raw,
            cleaned,
            metrics,
        } => {
            assert_eq!(raw, "okay");
            assert!(cleaned.is_none(), "skip path must not produce cleaned text");
            assert!(
                metrics.llm_skipped_short,
                "metrics must record the short-utterance skip"
            );
        }
        other => panic!("expected Completed, got {other:?}"),
    }
    assert_eq!(injected.lock().unwrap().clone(), vec!["okay".to_string()]);
}

#[tokio::test]
async fn pipeline_skips_history_when_stt_returns_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("history.sqlite");

    let stt: Arc<dyn SpeechToText> = Arc::new(FakeStt {
        text: "   ".into(),
        lang: None,
    });
    let injected = Arc::new(Mutex::new(Vec::<String>::new()));
    let injector = Arc::new(CapturingInjector(Arc::clone(&injected)));

    let cfg = Arc::new(Config::default());
    let (orch, _rx) =
        orchestrator_for_test(stt, None, &db_path, cfg, injector, Arc::new(StubFocus));
    let pcm = vec![0.0_f32; 16_000];
    let outcome = orch.run_oneshot(pcm, 1000).await;
    assert!(matches!(outcome, PipelineOutcome::EmptyOrTooShort { .. }));
    assert!(injected.lock().unwrap().is_empty());
    let db = HistoryDb::open(&db_path).unwrap();
    assert_eq!(db.count().unwrap(), 0);
}

#[tokio::test]
async fn pipeline_passes_raw_through_when_no_llm() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("history.sqlite");

    let stt: Arc<dyn SpeechToText> = Arc::new(FakeStt {
        text: "uppercase me".into(),
        lang: None,
    });
    let injected = Arc::new(Mutex::new(Vec::<String>::new()));
    let injector = Arc::new(CapturingInjector(Arc::clone(&injected)));

    let cfg = Arc::new(Config::default());
    let (orch, _rx) =
        orchestrator_for_test(stt, None, &db_path, cfg, injector, Arc::new(StubFocus));
    let outcome = orch.run_oneshot(vec![0.0_f32; 16_000], 1000).await;
    assert!(matches!(outcome, PipelineOutcome::Completed { .. }));
    let captured = injected.lock().unwrap().clone();
    assert_eq!(captured, vec!["uppercase me".to_string()]);
    let db = HistoryDb::open(&db_path).unwrap();
    let rows = db.recent(1).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].llm_backend.is_none());
}
