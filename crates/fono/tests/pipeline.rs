// SPDX-License-Identifier: GPL-3.0-only
//! End-to-end orchestrator test: synthetic PCM → fake STT → fake LLM
//! → fake injector → history row. No microphone, no network.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use fono_core::config::{Config, PolishBackend};
use fono_core::history::HistoryDb;
use fono_core::paths::Paths;
use fono_polish::{FormatContext, TextFormatter};
use fono_stt::{SpeechToText, Transcription};

use fono::session::{orchestrator_for_test, FocusProbe, Injector, PipelineOutcome};
use fono_inject::FocusInfo;

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

struct FakePolish;

#[async_trait]
impl TextFormatter for FakePolish {
    async fn format(&self, raw: &str, _ctx: &FormatContext) -> Result<String> {
        Ok(format!("CLEANED: {}", raw.trim()))
    }
    fn name(&self) -> &'static str {
        "fake-polish"
    }
}

struct CapturingInjector(Arc<Mutex<Vec<String>>>);

impl Injector for CapturingInjector {
    fn inject(&self, text: &str) -> Result<(bool, String)> {
        self.0.lock().unwrap().push(text.to_string());
        Ok((true, "test".to_string()))
    }
}

struct StubFocus;

impl FocusProbe for StubFocus {
    fn probe(&self) -> FocusInfo {
        FocusInfo {
            window_class: Some("Slack".into()),
            window_title: Some("general".into()),
            window_pid: None,
        }
    }
}

#[tokio::test]
async fn pipeline_produces_history_row_and_injects_cleaned_text() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("history.sqlite");

    let stt: Arc<dyn SpeechToText> =
        Arc::new(FakeStt { text: "hello world".into(), lang: Some("en".into()) });
    let polish: Option<Arc<dyn TextFormatter>> = Some(Arc::new(FakePolish));
    let injected = Arc::new(Mutex::new(Vec::<String>::new()));
    let injector = Arc::new(CapturingInjector(Arc::clone(&injected)));

    let mut cfg = Config::default();
    cfg.polish.enabled = true;
    cfg.polish.backend = PolishBackend::OpenAI; // anything non-None
                                                // Force the LLM to run regardless of the default short-utterance
                                                // skip threshold; this test covers the cleaned-output path.
    cfg.polish.skip_if_words_lt = 0;
    let cfg = Arc::new(cfg);

    let (orch, _action_rx) = orchestrator_for_test(
        stt,
        polish,
        &db_path,
        Arc::clone(&cfg),
        injector,
        Arc::new(StubFocus),
    );

    // 1 second of silence at 16 kHz to drive the pipeline.
    let pcm = vec![0.0_f32; 16_000];
    let outcome = orch.run_oneshot(pcm, 1000).await;

    let metrics = match outcome {
        PipelineOutcome::Completed { raw, cleaned, metrics } => {
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
    assert_eq!(row.polish_backend.as_deref(), Some("fake-polish"));
    assert_eq!(row.app_class.as_deref(), Some("Slack"));
    assert_eq!(row.language.as_deref(), Some("en"));
}

struct ClarifyingPolish;

#[async_trait]
impl TextFormatter for ClarifyingPolish {
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
        "fake-clarifying-polish"
    }
}

/// Plan task 6 — when the polish backend rejects a clarification reply
/// (the F8 push-to-talk failure mode), the pipeline must inject the
/// raw STT text rather than the meta-question.
#[tokio::test]
async fn pipeline_falls_back_to_raw_when_llm_rejects_clarification() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("history.sqlite");

    // Four words so it's above the new `skip_if_words_lt = 3` default
    // and the LLM is actually invoked.
    let stt: Arc<dyn SpeechToText> =
        Arc::new(FakeStt { text: "the response is this".into(), lang: Some("en".into()) });
    let polish: Option<Arc<dyn TextFormatter>> = Some(Arc::new(ClarifyingPolish));
    let injected = Arc::new(Mutex::new(Vec::<String>::new()));
    let injector = Arc::new(CapturingInjector(Arc::clone(&injected)));

    let mut cfg = Config::default();
    cfg.polish.enabled = true;
    cfg.polish.backend = PolishBackend::OpenAI;
    let cfg = Arc::new(cfg);

    let (orch, _rx) = orchestrator_for_test(
        stt,
        polish,
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

    let stt: Arc<dyn SpeechToText> =
        Arc::new(FakeStt { text: "okay".into(), lang: Some("en".into()) });
    // If the LLM is invoked, the test fails: ClarifyingPolish bails, but
    // we'd still see `llm_skipped_short = false` in metrics.
    let polish: Option<Arc<dyn TextFormatter>> = Some(Arc::new(ClarifyingPolish));
    let injected = Arc::new(Mutex::new(Vec::<String>::new()));
    let injector = Arc::new(CapturingInjector(Arc::clone(&injected)));

    let mut cfg = Config::default();
    cfg.polish.enabled = true;
    cfg.polish.backend = PolishBackend::OpenAI;
    // Sanity-check the new default rather than hard-coding it.
    assert!(cfg.polish.skip_if_words_lt >= 3);
    let cfg = Arc::new(cfg);

    let (orch, _rx) = orchestrator_for_test(
        stt,
        polish,
        &db_path,
        Arc::clone(&cfg),
        injector,
        Arc::new(StubFocus),
    );

    let outcome = orch.run_oneshot(vec![0.0_f32; 16_000], 1000).await;
    match outcome {
        PipelineOutcome::Completed { raw, cleaned, metrics } => {
            assert_eq!(raw, "okay");
            assert!(cleaned.is_none(), "skip path must not produce cleaned text");
            assert!(metrics.llm_skipped_short, "metrics must record the short-utterance skip");
        }
        other => panic!("expected Completed, got {other:?}"),
    }
    assert_eq!(injected.lock().unwrap().clone(), vec!["okay".to_string()]);
}

#[tokio::test]
async fn pipeline_skips_history_when_stt_returns_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("history.sqlite");

    let stt: Arc<dyn SpeechToText> = Arc::new(FakeStt { text: "   ".into(), lang: None });
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

    let stt: Arc<dyn SpeechToText> = Arc::new(FakeStt { text: "uppercase me".into(), lang: None });
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
    assert!(rows[0].polish_backend.is_none());
}

/// A `TextFormatter` that records the [`FormatContext`] it was handed
/// (the candidate set and the rendered `system_prompt()`) so the test
/// can assert the cleanup directive reached the LLM. Echoes the raw
/// text back as the cleaned output.
struct AssertingPolish {
    candidates: Arc<Mutex<Vec<String>>>,
    system_prompt: Arc<Mutex<String>>,
}

#[async_trait]
impl TextFormatter for AssertingPolish {
    async fn format(&self, raw: &str, ctx: &FormatContext) -> Result<String> {
        self.candidates.lock().unwrap().clone_from(&ctx.candidate_languages);
        *self.system_prompt.lock().unwrap() = ctx.system_prompt();
        Ok(raw.trim().to_string())
    }
    fn name(&self) -> &'static str {
        "asserting-polish"
    }
}

/// Plan task E1 — a garbled, diacritic-stripped Romanian capture with
/// `language: None` (STT engine reports no language) but
/// `general.languages = ["ro", "en"]` must still reach the cleanup LLM
/// with the candidate set and the Romanian diacritic directive. Proves
/// engine-independence: correctness does not depend on
/// `Transcription.language`.
#[tokio::test]
async fn pipeline_feeds_candidate_set_and_directive_when_stt_reports_no_language() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("history.sqlite");

    // Five words, diacritics stripped, language unreported by the engine.
    let stt: Arc<dyn SpeechToText> =
        Arc::new(FakeStt { text: "as vrea sa merg acasa".into(), lang: None });
    let candidates = Arc::new(Mutex::new(Vec::<String>::new()));
    let system_prompt = Arc::new(Mutex::new(String::new()));
    let polish: Option<Arc<dyn TextFormatter>> = Some(Arc::new(AssertingPolish {
        candidates: Arc::clone(&candidates),
        system_prompt: Arc::clone(&system_prompt),
    }));
    let injected = Arc::new(Mutex::new(Vec::<String>::new()));
    let injector = Arc::new(CapturingInjector(Arc::clone(&injected)));

    let mut cfg = Config::default();
    cfg.polish.enabled = true;
    cfg.polish.backend = PolishBackend::OpenAI;
    cfg.general.languages = vec!["ro".into(), "en".into()];
    let cfg = Arc::new(cfg);

    let (orch, _rx) = orchestrator_for_test(
        stt,
        polish,
        &db_path,
        Arc::clone(&cfg),
        injector,
        Arc::new(StubFocus),
    );

    let outcome = orch.run_oneshot(vec![0.0_f32; 16_000], 1000).await;
    assert!(matches!(outcome, PipelineOutcome::Completed { .. }));

    // The candidate set reached the formatter intact.
    assert_eq!(candidates.lock().unwrap().clone(), vec!["ro".to_string(), "en".to_string()]);

    // The rendered system prompt carries the Romanian diacritic directive.
    let sp = system_prompt.lock().unwrap().clone();
    assert!(sp.contains("Romanian"), "directive must name Romanian: {sp}");
    assert!(sp.contains("English"), "directive must name English: {sp}");
    assert!(sp.contains("ă, â, î, ș, ț"), "directive must list Romanian diacritics: {sp}");
    // No engine language ⇒ no soft-hint sentence.
    assert!(!sp.contains("It is most likely"), "no soft hint when STT reports no language: {sp}");
}

/// Plan task E2 — when the STT engine *does* report a language, cleanup
/// treats it as the source-language anchor. The prompt must frame the
/// job as same-language editing, not candidate detection or translation.
#[tokio::test]
async fn pipeline_adds_source_language_contract_when_stt_reports_language() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("history.sqlite");
    let stt: Arc<dyn SpeechToText> =
        Arc::new(FakeStt { text: "as vrea sa merg acasa".into(), lang: Some("ro".into()) });
    let candidates = Arc::new(Mutex::new(Vec::<String>::new()));
    let system_prompt = Arc::new(Mutex::new(String::new()));
    let polish: Option<Arc<dyn TextFormatter>> = Some(Arc::new(AssertingPolish {
        candidates: Arc::clone(&candidates),
        system_prompt: Arc::clone(&system_prompt),
    }));
    let injected = Arc::new(Mutex::new(Vec::<String>::new()));
    let injector = Arc::new(CapturingInjector(Arc::clone(&injected)));

    let mut cfg = Config::default();
    cfg.polish.enabled = true;
    cfg.polish.backend = PolishBackend::OpenAI;
    cfg.general.languages = vec!["ro".into(), "en".into()];
    let cfg = Arc::new(cfg);

    let (orch, _rx) = orchestrator_for_test(
        stt,
        polish,
        &db_path,
        Arc::clone(&cfg),
        injector,
        Arc::new(StubFocus),
    );

    let outcome = orch.run_oneshot(vec![0.0_f32; 16_000], 1000).await;
    assert!(matches!(outcome, PipelineOutcome::Completed { .. }));

    let sp = system_prompt.lock().unwrap().clone();
    assert!(sp.contains("SOURCE_LANGUAGE: Romanian (ro)."), "source contract present: {sp}");
    assert!(sp.contains("same-language transcription cleanup task"), "same-language task: {sp}");
    assert!(sp.contains("not a translation task"), "translation must be forbidden: {sp}");
    assert!(
        !sp.contains("Detect which one"),
        "known source language must not ask for detection: {sp}"
    );
    assert!(
        !sp.contains("It is most likely"),
        "known source language must not be a soft hint: {sp}"
    );
}

// ---- Personal vocabulary (ADR 0037, plan v3 Task 2.2) ----------------
//
// The four-way matrix from the plan is {batch, live} × {polish on, off}.
// The batch half plus the v0.10 word-by-word streaming-cleanup path are
// exercised end-to-end here through `run_oneshot`. The live half shares
// the identical one-line hook (`load_vocabulary()` + `apply`) in
// `on_stop_live_dictation`, which cannot be driven without a live
// microphone session; its correctness is covered by the engine unit
// tests in `fono-core::correction` plus compile-time signature parity.

/// Root a `Paths` under `tmp` and write a `phono → Fono` vocabulary.
fn seed_vocabulary(tmp: &std::path::Path) -> Arc<Paths> {
    let paths = Paths::rooted_at(tmp);
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(
        paths.vocabulary_file(),
        "[[vocabulary]]\nfrom = [\"phono\", \"phone oh\"]\nto = \"Fono\"\n",
    )
    .unwrap();
    Arc::new(paths)
}

/// polish OFF — the pure deterministic guarantee: corrected raw is what
/// reaches the injector and the history row; substrings stay untouched.
#[tokio::test]
async fn pipeline_vocabulary_corrects_raw_without_polish() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("history.sqlite");

    let stt: Arc<dyn SpeechToText> = Arc::new(FakeStt {
        text: "i showed phono and the phonograph to everyone".into(),
        lang: Some("en".into()),
    });
    let injected = Arc::new(Mutex::new(Vec::<String>::new()));
    let injector = Arc::new(CapturingInjector(Arc::clone(&injected)));

    let cfg = Arc::new(Config::default());
    let (mut orch, _rx) =
        orchestrator_for_test(stt, None, &db_path, cfg, injector, Arc::new(StubFocus));
    orch.set_paths(seed_vocabulary(tmp.path()));

    let outcome = orch.run_oneshot(vec![0.0_f32; 16_000], 1000).await;
    match outcome {
        PipelineOutcome::Completed { raw, .. } => {
            assert_eq!(raw, "i showed Fono and the phonograph to everyone");
        }
        other => panic!("expected Completed, got {other:?}"),
    }
    assert_eq!(
        injected.lock().unwrap().clone(),
        vec!["i showed Fono and the phonograph to everyone".to_string()]
    );
    // History stores the corrected transcript (intended: it is what the
    // user meant — noted in the ADR).
    let db = HistoryDb::open(&db_path).unwrap();
    let rows = db.recent(1).unwrap();
    assert_eq!(rows[0].raw, "i showed Fono and the phonograph to everyone");
}

/// polish ON (one-shot) — the formatter must *receive* corrected text,
/// and a formatter that re-introduces the mishearing is fixed again by
/// the belt-and-suspenders pass on `final_text`.
struct ReintroducingPolish;

#[async_trait]
impl TextFormatter for ReintroducingPolish {
    async fn format(&self, raw: &str, _ctx: &FormatContext) -> Result<String> {
        // Prove the corrected raw reached the LLM, then regress it —
        // the pipeline's second pass must repair the output.
        assert!(raw.contains("Fono"), "polish must receive corrected raw, got: {raw}");
        Ok(raw.replace("Fono", "phono"))
    }
    fn name(&self) -> &'static str {
        "reintroducing-polish"
    }
}

#[tokio::test]
async fn pipeline_vocabulary_survives_polish_reintroducing_mishearing() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("history.sqlite");

    let stt: Arc<dyn SpeechToText> = Arc::new(FakeStt {
        text: "we ship phono to every desktop".into(),
        lang: Some("en".into()),
    });
    let polish: Option<Arc<dyn TextFormatter>> = Some(Arc::new(ReintroducingPolish));
    let injected = Arc::new(Mutex::new(Vec::<String>::new()));
    let injector = Arc::new(CapturingInjector(Arc::clone(&injected)));

    let mut cfg = Config::default();
    cfg.polish.enabled = true;
    cfg.polish.backend = PolishBackend::OpenAI;
    cfg.polish.skip_if_words_lt = 0;
    let cfg = Arc::new(cfg);

    let (mut orch, _rx) = orchestrator_for_test(
        stt,
        polish,
        &db_path,
        Arc::clone(&cfg),
        injector,
        Arc::new(StubFocus),
    );
    orch.set_paths(seed_vocabulary(tmp.path()));

    let outcome = orch.run_oneshot(vec![0.0_f32; 16_000], 1000).await;
    match outcome {
        PipelineOutcome::Completed { raw, cleaned, .. } => {
            assert_eq!(raw, "we ship Fono to every desktop");
            assert_eq!(cleaned.as_deref(), Some("we ship Fono to every desktop"));
        }
        other => panic!("expected Completed, got {other:?}"),
    }
    assert_eq!(injected.lock().unwrap().clone(), vec!["we ship Fono to every desktop".to_string()]);
}

/// polish ON (v0.10 word-by-word streaming inject) — the case v2 would
/// have shipped broken: text is typed at the cursor before any final
/// pass could run, so the upstream correction is the only chance.
struct EchoStreamPolish;

#[async_trait]
impl TextFormatter for EchoStreamPolish {
    async fn format(&self, raw: &str, _ctx: &FormatContext) -> Result<String> {
        Ok(raw.trim().to_string())
    }
    fn name(&self) -> &'static str {
        "echo-stream-polish"
    }
    async fn format_stream(
        &self,
        raw: &str,
        _ctx: &FormatContext,
    ) -> Result<futures::stream::BoxStream<'static, Result<String>>> {
        use futures::StreamExt as _;
        // Echo the received raw word-by-word, mimicking a local model
        // that preserves the proper nouns it is given.
        let chunks: Vec<Result<String>> =
            raw.split_inclusive(' ').map(|c| Ok(c.to_string())).collect();
        Ok(futures::stream::iter(chunks).boxed())
    }
    fn is_local(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn pipeline_vocabulary_reaches_streaming_cleanup_inject_path() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("history.sqlite");

    let stt: Arc<dyn SpeechToText> = Arc::new(FakeStt {
        text: "this is phono typing words at the cursor right now".into(),
        lang: Some("en".into()),
    });
    let polish: Option<Arc<dyn TextFormatter>> = Some(Arc::new(EchoStreamPolish));
    let injected = Arc::new(Mutex::new(Vec::<String>::new()));
    let injector = Arc::new(CapturingInjector(Arc::clone(&injected)));

    let mut cfg = Config::default();
    cfg.polish.enabled = true;
    cfg.polish.backend = PolishBackend::OpenAI;
    cfg.polish.skip_if_words_lt = 0;
    cfg.polish.stream_injection = true;
    let cfg = Arc::new(cfg);

    let (mut orch, _rx) = orchestrator_for_test(
        stt,
        polish,
        &db_path,
        Arc::clone(&cfg),
        injector,
        Arc::new(StubFocus),
    );
    orch.set_paths(seed_vocabulary(tmp.path()));

    let outcome = orch.run_oneshot(vec![0.0_f32; 16_000], 1000).await;
    let expected = "this is Fono typing words at the cursor right now";
    match outcome {
        PipelineOutcome::Completed { raw, cleaned, .. } => {
            assert_eq!(raw, expected);
            assert_eq!(cleaned.as_deref(), Some(expected));
        }
        other => panic!("expected Completed, got {other:?}"),
    }
    // The streamed deltas were typed at the cursor incrementally and
    // must concatenate to the corrected text — no "phono" ever typed.
    let captured = injected.lock().unwrap().clone();
    let typed: String = captured.concat();
    assert_eq!(typed, expected);
    assert!(captured.len() > 1, "streaming must inject more than one delta: {captured:?}");
    for delta in &captured {
        assert!(!delta.to_lowercase().contains("phono"), "mishearing leaked: {delta}");
    }
}
