// SPDX-License-Identifier: GPL-3.0-only
//! Two-gate verdict integration test (Wave 2 Thread A, Task A14).
//!
//! Drives `run_fixture` end-to-end with a `FixedStt` that returns a
//! constant transcript that intentionally diverges from the manifest's
//! reference by ~25%, so the accuracy gate fails while no streaming
//! lane is wired up. Asserts `Verdict::Fail` with the `acc` substring
//! in the note.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use fono_bench::capabilities::ModelCapabilities;
use fono_bench::equivalence::{run_fixture, ManifestFixture};
use fono_bench::Verdict;
use fono_stt::{SpeechToText, Transcription};

struct FixedStt {
    text: String,
}

#[async_trait]
impl SpeechToText for FixedStt {
    async fn transcribe(
        &self,
        _pcm: &[f32],
        _sample_rate: u32,
        _lang: Option<&str>,
    ) -> Result<Transcription> {
        Ok(Transcription {
            text: self.text.clone(),
            language: Some("en".into()),
            duration_ms: Some(0),
        })
    }

    fn name(&self) -> &'static str {
        "fixed-stt"
    }
}

#[tokio::test]
async fn accuracy_gate_fails_when_transcript_diverges_from_reference() {
    // Pick the existing 5-second `en-single-sentence.wav` fixture so
    // no new audio needs committing.
    let fixture_root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/equivalence");
    assert!(
        fixture_root.join("en-single-sentence.wav").exists(),
        "expected en-single-sentence.wav at {}",
        fixture_root.display()
    );

    // Reference text from the manifest:
    //   "neglected. The art of war then is governed by five constant factors."
    // We feed back a transcript that diverges by replacing the second
    // half — ~50% character drift — to push accuracy past a tight
    // 0.20 gate but well below the 1.0 informational ceiling.
    let fixture = ManifestFixture {
        name: "two-gate-test".into(),
        path: "en-single-sentence.wav".into(),
        sha256: String::new(),
        source_url: String::new(),
        license: String::new(),
        reference: "neglected. The art of war then is governed by five constant factors.".into(),
        language: "en".into(),
        synthetic_placeholder: false,
        duration_estimate_s: 5.0,
        equivalence_threshold: Some(0.20),
        accuracy_threshold: Some(0.20),
        requires_multilingual: None,
    };

    let stt: Arc<dyn SpeechToText> = Arc::new(FixedStt {
        text: "completely different text that does not match the reference at all".into(),
    });
    let caps = ModelCapabilities::for_local_whisper("tiny.en");

    let result = run_fixture(&fixture, &fixture_root, stt, None, &caps, None)
        .await
        .expect("run_fixture should produce a verdict");

    assert_eq!(result.verdict, Verdict::Fail, "note: {}", result.note);
    assert!(
        result.note.contains("acc"),
        "note must mention the failing accuracy gate; got {:?}",
        result.note
    );
    let acc = result
        .metrics
        .stt_accuracy_levenshtein
        .expect("accuracy must be computed when reference is supplied");
    assert!(acc > 0.20, "accuracy {acc} should exceed the 0.20 gate");
}
