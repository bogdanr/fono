// SPDX-License-Identifier: GPL-3.0-only
//! Capability-skip integration test (Wave 2 Thread A, Task A13).
//!
//! Builds a `PanicStt` whose `transcribe` panics — the assertion is
//! that `run_fixture` short-circuits via the typed capability gate
//! before ever touching the STT, when an English-only model is paired
//! with a non-English fixture.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use fono_bench::capabilities::ModelCapabilities;
use fono_bench::equivalence::{run_fixture, ManifestFixture};
use fono_bench::{SkipReason, Verdict};
use fono_stt::{SpeechToText, Transcription};

struct PanicStt;

#[async_trait]
impl SpeechToText for PanicStt {
    async fn transcribe(
        &self,
        _pcm: &[f32],
        _sample_rate: u32,
        _lang: Option<&str>,
    ) -> Result<Transcription> {
        panic!("PanicStt::transcribe must not be invoked on capability-skipped fixtures")
    }

    fn name(&self) -> &'static str {
        "panic-stt"
    }
}

fn make_fixture(language: &str) -> ManifestFixture {
    ManifestFixture {
        name: "synthetic-skip".into(),
        // Path intentionally points at a file that does not exist on
        // disk — `run_fixture` must short-circuit before opening it.
        path: "does-not-exist.wav".into(),
        sha256: String::new(),
        source_url: String::new(),
        license: String::new(),
        reference: String::new(),
        language: language.into(),
        synthetic_placeholder: false,
        duration_estimate_s: 0.0,
        equivalence_threshold: None,
        accuracy_threshold: None,
        requires_multilingual: None,
    }
}

#[tokio::test]
async fn english_only_model_skips_non_english_fixture_without_invoking_stt() {
    let fixture = make_fixture("ro");
    let caps = ModelCapabilities::for_local_whisper("tiny.en");
    assert!(caps.english_only, "tiny.en must classify as english_only");

    let stt: Arc<dyn SpeechToText> = Arc::new(PanicStt);
    let result = run_fixture(
        &fixture,
        &PathBuf::from("/nonexistent"),
        stt,
        None,
        &caps,
        None,
    )
    .await
    .expect("run_fixture must succeed via capability skip");

    assert_eq!(result.verdict, Verdict::Skipped);
    assert_eq!(result.skip_reason, Some(SkipReason::Capability));
    assert!(
        result.note.contains("English-only"),
        "note must explain the capability skip; got {:?}",
        result.note
    );
    assert!(
        result.metrics.stt_accuracy_levenshtein.is_none(),
        "no accuracy measurement when transcribe is never invoked"
    );
}

#[tokio::test]
async fn english_only_model_runs_english_fixture() {
    // Sanity check: with an English fixture the skip path must not
    // fire — the test would then panic via PanicStt because we never
    // hand it real audio. Instead we override `requires_multilingual`
    // to false on a non-English fixture to prove the override path.
    let mut fixture = make_fixture("ro");
    fixture.requires_multilingual = Some(false);
    let caps = ModelCapabilities::for_local_whisper("tiny.en");

    // We supply PanicStt; the assertion is that `run_fixture` would
    // proceed to invoke STT (and therefore panic). We catch the panic
    // through an Err return from a spawn.
    let stt: Arc<dyn SpeechToText> = Arc::new(PanicStt);
    let handle = tokio::spawn(async move {
        run_fixture(
            &fixture,
            &PathBuf::from("/nonexistent"),
            stt,
            None,
            &caps,
            None,
        )
        .await
    });
    // The task either panics (via PanicStt) or fails reading the WAV
    // (also acceptable — proves the skip path was bypassed).
    let outcome = handle.await;
    let proceeded = match outcome {
        Err(join_err) => join_err.is_panic(),
        Ok(Ok(r)) => r.verdict != Verdict::Skipped,
        Ok(Err(_)) => true,
    };
    assert!(
        proceeded,
        "with requires_multilingual=false the capability gate must not fire"
    );
}
