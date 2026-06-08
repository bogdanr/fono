// SPDX-License-Identifier: GPL-3.0-only
//! Kokoro English TTS support for the local ONNX voice engine (feature
//! `tts-local`).
//!
//! Kokoro is the English engine in Fono's split-engine policy (ADR 0033):
//! Kokoro handles English, Piper handles every other language. It runs on the
//! same shared `ort` runtime as Piper (ADR 0032), but the model contract is
//! different, so it gets its own engine rather than reusing [`crate::piper`]:
//!
//! 1. **text → IPA** via the embedded pure-Rust `espeak_ng` G2P core
//!    (shared with Piper — see [`crate::espeak`]), using the voice's accent
//!    (`en-us` for `af_*`, `en-gb` for `bf_*`).
//! 2. **IPA → token ids** via a single *fixed* vocabulary baked into the
//!    binary ([`VOCAB`], from `hexgrad/Kokoro-82M`), not a per-voice map like
//!    Piper. The sequence is bracketed by a boundary token (id 0); unmapped
//!    IPA codepoints are skipped.
//! 3. **inference** through the shared Kokoro `.ort` model
//!    (`input_ids`/`style`/`speed` → waveform) at 24 kHz mono. The per-voice
//!    "style" is a `[510, 256]` `f32` pack; the row indexed by the token count
//!    selects the 256-d style vector for this utterance.
//!
//! Every Kokoro voice shares one model file; the voice identity is the small
//! style pack ([`KokoroVoice`]). [`KokoroLocal`] is the [`TextToSpeech`] the
//! router dispatches to for English.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use fono_core::turn_trace::current_span;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;
use serde_json::json;

use crate::traits::{TextToSpeech, TtsAudio};

/// Kokoro emits 24 kHz mono PCM.
const SAMPLE_RATE: u32 = 24_000;
/// Width of a Kokoro style vector.
const STYLE_DIM: usize = 256;
/// Number of style rows in a voice pack: one per output token-count bucket.
const STYLE_ROWS: usize = 510;
/// Boundary token bracketing a Kokoro token sequence (the pad/`$` id).
const BOUNDARY: i64 = 0;

/// Kokoro's fixed phoneme → token-id vocabulary, transcribed verbatim from
/// `hexgrad/Kokoro-82M`'s `config.json`. Unlike Piper (a per-voice map in the
/// `.onnx.json`), Kokoro uses this one table for every voice. IPA codepoints
/// absent from it are dropped, mirroring the upstream tokenizer.
#[rustfmt::skip]
const VOCAB: &[(char, i64)] = &[
    (';', 1), (':', 2), (',', 3), ('.', 4), ('!', 5), ('?', 6),
    ('\u{2014}', 9), ('\u{2026}', 10), ('"', 11), ('(', 12), (')', 13),
    ('\u{201c}', 14), ('\u{201d}', 15), (' ', 16), ('\u{303}', 17),
    ('\u{2a3}', 18), ('\u{2a5}', 19), ('\u{2a6}', 20), ('\u{2a8}', 21),
    ('\u{1d5d}', 22), ('\u{ab67}', 23), ('A', 24), ('I', 25), ('O', 31),
    ('Q', 33), ('S', 35), ('T', 36), ('W', 39), ('Y', 41), ('\u{1d4a}', 42),
    ('a', 43), ('b', 44), ('c', 45), ('d', 46), ('e', 47), ('f', 48),
    ('h', 50), ('i', 51), ('j', 52), ('k', 53), ('l', 54), ('m', 55),
    ('n', 56), ('o', 57), ('p', 58), ('q', 59), ('r', 60), ('s', 61),
    ('t', 62), ('u', 63), ('v', 64), ('w', 65), ('x', 66), ('y', 67),
    ('z', 68), ('\u{251}', 69), ('\u{250}', 70), ('\u{252}', 71),
    ('\u{e6}', 72), ('\u{3b2}', 75), ('\u{254}', 76), ('\u{255}', 77),
    ('\u{e7}', 78), ('\u{256}', 80), ('\u{f0}', 81), ('\u{2a4}', 82),
    ('\u{259}', 83), ('\u{25a}', 85), ('\u{25b}', 86), ('\u{25c}', 87),
    ('\u{25f}', 90), ('\u{261}', 92), ('\u{265}', 99), ('\u{268}', 101),
    ('\u{26a}', 102), ('\u{29d}', 103), ('\u{26f}', 110), ('\u{270}', 111),
    ('\u{14b}', 112), ('\u{273}', 113), ('\u{272}', 114), ('\u{274}', 115),
    ('\u{f8}', 116), ('\u{278}', 118), ('\u{3b8}', 119), ('\u{153}', 120),
    ('\u{279}', 123), ('\u{27e}', 125), ('\u{27b}', 126), ('\u{281}', 128),
    ('\u{27d}', 129), ('\u{282}', 130), ('\u{283}', 131), ('\u{288}', 132),
    ('\u{2a7}', 133), ('\u{28a}', 135), ('\u{28b}', 136), ('\u{28c}', 138),
    ('\u{263}', 139), ('\u{264}', 140), ('\u{3c7}', 142), ('\u{28e}', 143),
    ('\u{292}', 147), ('\u{294}', 148), ('\u{2c8}', 156), ('\u{2cc}', 157),
    ('\u{2d0}', 158), ('\u{2b0}', 162), ('\u{2b2}', 164), ('\u{2193}', 169),
    ('\u{2192}', 171), ('\u{2197}', 172), ('\u{2198}', 173), ('\u{1d7b}', 177),
];

/// A loaded Kokoro voice: the fixed vocab, this voice's style pack, and the
/// espeak-ng data directory for phonemization. Construct once per voice and
/// reuse; [`Self::phonemize`] and [`Self::text_to_tokens`] are cheap.
#[derive(Debug, Clone)]
pub struct KokoroVoice {
    vocab: HashMap<char, i64>,
    /// Flattened `[STYLE_ROWS, STYLE_DIM]` style pack in row-major order.
    style: Vec<f32>,
    /// espeak-ng accent code, e.g. `"en-us"` / `"en-gb"`.
    espeak_voice: String,
    data_dir: PathBuf,
}

impl KokoroVoice {
    /// Build a voice from its style pack and accent, materialising the embedded
    /// espeak-ng G2P core under `data_dir` (the matching `<lang>_dict` is
    /// downloaded separately; see [`crate::voices`]).
    pub fn new(
        style: Vec<f32>,
        espeak_voice: impl Into<String>,
        data_dir: impl Into<PathBuf>,
    ) -> Result<Self> {
        if style.len() != STYLE_ROWS * STYLE_DIM {
            bail!(
                "kokoro style pack has {} floats, expected {} ({STYLE_ROWS}x{STYLE_DIM})",
                style.len(),
                STYLE_ROWS * STYLE_DIM
            );
        }
        let data_dir = data_dir.into();
        crate::espeak::install_core(&data_dir)?;
        Ok(Self {
            vocab: VOCAB.iter().copied().collect(),
            style,
            espeak_voice: espeak_voice.into(),
            data_dir,
        })
    }

    /// Phonemize `text` to an IPA string using this voice's espeak-ng data.
    pub fn phonemize(&self, text: &str) -> Result<String> {
        // Fold the accent onto the canonical base language whose phoneme table
        // ships in the embedded core (en-us/en-gb → en); this also matches the
        // downloaded `<canonical>_dict` filename.
        let voice = crate::espeak::canonical_lang(&self.espeak_voice);
        let translator = espeak_ng::Translator::new(voice, Some(self.data_dir.as_path()))
            .map_err(|e| anyhow::anyhow!("espeak-ng translator init for '{voice}': {e}"))?;
        let text = crate::espeak::normalize_diacritics(text);
        translator
            .text_to_ipa(&text)
            .map_err(|e| anyhow::anyhow!("espeak-ng phonemize '{voice}': {e}"))
    }

    /// Encode an IPA string into Kokoro token ids: a leading boundary token,
    /// each mapped phoneme (unmapped codepoints skipped), then a trailing
    /// boundary token. The phoneme count is capped so the full sequence fits
    /// the style table. Returns an empty vector when nothing maps (treated by
    /// the caller as "nothing to synthesize").
    #[must_use]
    pub fn ipa_to_tokens(&self, ipa: &str) -> Vec<i64> {
        let mut ids: Vec<i64> = Vec::with_capacity(ipa.chars().count() + 2);
        ids.push(BOUNDARY);
        for ch in ipa.chars() {
            // Leave room for the trailing boundary token; the style row is
            // indexed by total token count, which the pack bounds at STYLE_ROWS.
            if ids.len() >= STYLE_ROWS - 1 {
                break;
            }
            if let Some(&id) = self.vocab.get(&ch) {
                ids.push(id);
            }
        }
        if ids.len() == 1 {
            return Vec::new();
        }
        ids.push(BOUNDARY);
        ids
    }

    /// Full front half: `text` → IPA → Kokoro token ids.
    pub fn text_to_tokens(&self, text: &str) -> Result<Vec<i64>> {
        Ok(self.ipa_to_tokens(&self.phonemize(text)?))
    }

    /// The 256-d style vector for a sequence of `n_tokens` tokens: row
    /// `clamp(n_tokens, 0, STYLE_ROWS - 1)` of the pack, matching upstream
    /// Kokoro's `voice_pack[len(tokens)]` indexing.
    #[must_use]
    fn style_row(&self, n_tokens: usize) -> &[f32] {
        let row = n_tokens.min(STYLE_ROWS - 1);
        let start = row * STYLE_DIM;
        &self.style[start..start + STYLE_DIM]
    }
}

/// Parse a Kokoro style pack file: a raw little-endian `f32` `[510, 256]`
/// tensor in row-major order (the format published to the voice mirror).
pub fn read_style(path: &Path) -> Result<Vec<f32>> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("read kokoro style pack {}", path.display()))?;
    let expected = STYLE_ROWS * STYLE_DIM * 4;
    if bytes.len() != expected {
        bail!(
            "kokoro style pack {} is {} bytes, expected {expected} ({STYLE_ROWS}x{STYLE_DIM} f32)",
            path.display(),
            bytes.len()
        );
    }
    Ok(bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

/// A local Kokoro TTS engine: the [`TextToSpeech`] back half.
///
/// Loads the shared Kokoro `.ort` model into an `ort` [`Session`] on the
/// statically-linked ONNX Runtime (ADR 0032) and pairs it with a
/// [`KokoroVoice`] front half. `synthesize` runs the Kokoro graph
/// (`input_ids` + `style` + `speed` → waveform) and returns mono `f32` PCM at
/// [`SAMPLE_RATE`].
pub struct KokoroLocal {
    voice: KokoroVoice,
    /// `run` needs `&mut Session`; the `Mutex` gives interior mutability and
    /// the `Arc` lets a blocking inference task own a handle. Synthesis is
    /// serialised per engine — callers parallelise across sentences.
    session: Arc<Mutex<Session>>,
    /// The model's first input name. Upstream variants differ (`input_ids` on
    /// the onnx-community export, `tokens` on kokoro-onnx), so read it from the
    /// session rather than hard-coding.
    input_name: String,
    /// The model's first output (waveform) name, read from the session.
    output_name: String,
}

impl KokoroLocal {
    /// Load a Kokoro voice: read its style pack, materialise the embedded
    /// espeak-ng data under `espeak_data_dir`, and open the shared `.ort`
    /// model at `model_path`.
    pub fn load(
        model_path: impl AsRef<Path>,
        style_path: impl AsRef<Path>,
        espeak_voice: impl Into<String>,
        espeak_data_dir: impl Into<PathBuf>,
    ) -> Result<Self> {
        // Ensure the process-wide ONNX Runtime environment exists before the
        // first session is built (idempotent; see `local::ensure_runtime`).
        crate::local::ensure_runtime();
        let style = read_style(style_path.as_ref())?;
        let voice = KokoroVoice::new(style, espeak_voice, espeak_data_dir)?;
        let model_path = model_path.as_ref();
        let session = Session::builder()
            .map_err(|e| anyhow::anyhow!("create ort session builder: {e}"))?
            // `.ort` models are pre-optimised and a minimal-build runtime has
            // the optimiser compiled out, so setting a level errors — recover
            // the builder and carry on (the documented `ort` minimal idiom).
            .with_optimization_level(GraphOptimizationLevel::Disable)
            .unwrap_or_else(ort::Error::recover)
            .with_intra_threads(1)
            .map_err(|e| anyhow::anyhow!("set intra-op threads: {e}"))?
            .commit_from_file(model_path)
            .map_err(|e| anyhow::anyhow!("load Kokoro model {}: {e}", model_path.display()))?;
        let input_name = session
            .inputs()
            .first()
            .map_or_else(|| "input_ids".to_string(), |o| o.name().to_string());
        let output_name = session
            .outputs()
            .first()
            .map_or_else(|| "waveform".to_string(), |o| o.name().to_string());
        Ok(Self { voice, session: Arc::new(Mutex::new(session)), input_name, output_name })
    }

    /// Run the Kokoro graph for a prepared token sequence. Blocking; call via
    /// `spawn_blocking`. Returns flattened mono `f32` PCM.
    // The session guard must span both `run` and the output extract (the PCM
    // slice borrows the run outputs), so it cannot be tightened further.
    #[allow(clippy::significant_drop_tightening)]
    fn run_inference(
        session: &Mutex<Session>,
        input_name: &str,
        output_name: &str,
        tokens: Vec<i64>,
        style: Vec<f32>,
    ) -> Result<Vec<f32>> {
        let n = tokens.len() as i64;
        let input_ids = Tensor::from_array((vec![1_i64, n], tokens))
            .map_err(|e| anyhow::anyhow!("build input_ids tensor: {e}"))?;
        let style = Tensor::from_array((vec![1_i64, STYLE_DIM as i64], style))
            .map_err(|e| anyhow::anyhow!("build style tensor: {e}"))?;
        let speed = Tensor::from_array((vec![1_i64], vec![1.0_f32]))
            .map_err(|e| anyhow::anyhow!("build speed tensor: {e}"))?;

        let pcm: Vec<f32> = {
            let mut session = session.lock().expect("kokoro session mutex poisoned");
            let outputs = session
                .run(ort::inputs![
                    input_name => input_ids,
                    "style" => style,
                    "speed" => speed,
                ])
                .map_err(|e| anyhow::anyhow!("run Kokoro inference: {e}"))?;
            let (_shape, pcm) = outputs[output_name]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow::anyhow!("extract Kokoro output PCM: {e}"))?;
            pcm.to_vec()
        };
        Ok(pcm)
    }
}

#[async_trait]
impl TextToSpeech for KokoroLocal {
    async fn synthesize(
        &self,
        text: &str,
        _voice: Option<&str>,
        _lang: Option<&str>,
    ) -> Result<TtsAudio> {
        if text.trim().is_empty() {
            return Ok(TtsAudio { pcm: Vec::new(), sample_rate: SAMPLE_RATE });
        }
        let tokenize_span = current_span("tts.kokoro_tokenize", "assistant.tts", "tts");
        let tokens = self.voice.text_to_tokens(text)?;
        tokenize_span.finish(json!({ "chars": text.chars().count(), "tokens": tokens.len() }));
        if tokens.is_empty() {
            return Ok(TtsAudio { pcm: Vec::new(), sample_rate: SAMPLE_RATE });
        }
        let style = self.voice.style_row(tokens.len()).to_vec();
        let session = Arc::clone(&self.session);
        let input_name = self.input_name.clone();
        let output_name = self.output_name.clone();
        // ONNX inference is CPU-bound and blocking; keep it off the async runtime.
        let inference_span = current_span("tts.kokoro_onnx_run", "assistant.tts", "tts");
        let pcm = tokio::task::spawn_blocking(move || {
            Self::run_inference(&session, &input_name, &output_name, tokens, style)
        })
        .await
        .context("kokoro inference task")??;
        inference_span.finish(json!({ "samples": pcm.len(), "sample_rate": SAMPLE_RATE }));
        Ok(TtsAudio { pcm, sample_rate: SAMPLE_RATE })
    }

    fn name(&self) -> &'static str {
        "kokoro-local"
    }

    fn native_sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_voice() -> KokoroVoice {
        let dir = std::env::temp_dir().join("fono-kokoro-unit");
        std::fs::create_dir_all(&dir).unwrap();
        KokoroVoice::new(vec![0.0; STYLE_ROWS * STYLE_DIM], "en-us", &dir).expect("build voice")
    }

    #[test]
    fn vocab_has_no_duplicate_chars_or_ids() {
        let mut chars = std::collections::HashSet::new();
        let mut ids = std::collections::HashSet::new();
        for &(c, i) in VOCAB {
            assert!(chars.insert(c), "duplicate char {c:?} in VOCAB");
            assert!(ids.insert(i), "duplicate id {i} in VOCAB");
        }
        assert_eq!(VOCAB.len(), 114, "Kokoro vocab is 114 entries (hexgrad/Kokoro-82M)");
    }

    #[test]
    fn vocab_covers_core_english_ipa() {
        let v = test_voice();
        // Stress marks, schwa, affricate, rhotic, diphthong building blocks.
        for ch in ['\u{2c8}', '\u{259}', '\u{2a4}', '\u{279}', '\u{251}', 'i', 't'] {
            assert!(v.vocab.contains_key(&ch), "vocab missing {ch:?}");
        }
    }

    #[test]
    fn ipa_to_tokens_brackets_with_boundary_and_skips_unmapped() {
        let v = test_voice();
        // 'a','b' map (43,44); '§' is unmapped and dropped.
        assert_eq!(v.ipa_to_tokens("a§b"), vec![0, 43, 44, 0]);
    }

    #[test]
    fn ipa_to_tokens_empty_when_nothing_maps() {
        let v = test_voice();
        assert!(v.ipa_to_tokens("").is_empty());
        assert!(
            v.ipa_to_tokens("§§§").is_empty(),
            "all-unmapped yields empty (not a bare bracket)"
        );
    }

    #[test]
    fn ipa_to_tokens_caps_at_style_table() {
        let v = test_voice();
        // A very long mapped string must still produce a sequence that indexes
        // a valid style row (<= STYLE_ROWS) and stays bracketed.
        let long = "a".repeat(STYLE_ROWS * 2);
        let toks = v.ipa_to_tokens(&long);
        assert!(toks.len() <= STYLE_ROWS, "token count {} exceeds style rows", toks.len());
        assert_eq!(toks.first(), Some(&0));
        assert_eq!(toks.last(), Some(&0));
    }

    #[test]
    fn style_row_clamps_to_last_row() {
        let v = test_voice();
        // Out-of-range token counts must clamp, never panic.
        assert_eq!(v.style_row(0).len(), STYLE_DIM);
        assert_eq!(v.style_row(STYLE_ROWS - 1).len(), STYLE_DIM);
        assert_eq!(v.style_row(STYLE_ROWS).len(), STYLE_DIM);
        assert_eq!(v.style_row(usize::MAX).len(), STYLE_DIM);
    }

    #[test]
    fn new_rejects_wrong_size_style_pack() {
        let dir = std::env::temp_dir().join("fono-kokoro-bad-style");
        std::fs::create_dir_all(&dir).unwrap();
        let err = KokoroVoice::new(vec![0.0; 10], "en-us", &dir).expect_err("must reject");
        assert!(err.to_string().contains("style pack"), "{err}");
    }

    #[test]
    fn read_style_validates_length() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.style.bin");
        std::fs::write(&good, vec![0u8; STYLE_ROWS * STYLE_DIM * 4]).unwrap();
        assert_eq!(read_style(&good).unwrap().len(), STYLE_ROWS * STYLE_DIM);

        let bad = dir.path().join("bad.style.bin");
        std::fs::write(&bad, vec![0u8; 16]).unwrap();
        assert!(read_style(&bad).is_err(), "short file must error");
    }

    /// Full pipeline against the real Kokoro `.ort` model + a voice style pack,
    /// including `ort` inference. Ignored by default: needs the converted model
    /// artefact, a style pack, an `en_dict`, and a linked runtime, none present
    /// in a plain `cargo test`. Run with:
    ///
    /// ```text
    /// ORT_LIB_LOCATION=tmp/kokoro-qunion-build \
    /// FONO_TEST_KOKORO_ORT=tmp/kokoro/publish/kokoro-v1.0-q8f16.ort \
    /// FONO_TEST_KOKORO_STYLE=tmp/kokoro/publish/af_heart.style.bin \
    /// FONO_TEST_ESPEAK_DICT=/path/to/en_dict \
    ///   cargo test -p fono-tts --features tts-local -- --ignored kokoro_local
    /// ```
    #[test]
    #[ignore = "needs a converted .ort model + style pack + linked runtime (see doc comment)"]
    fn kokoro_local_synthesizes_real_audio() {
        let model = std::env::var("FONO_TEST_KOKORO_ORT").expect("FONO_TEST_KOKORO_ORT");
        let style = std::env::var("FONO_TEST_KOKORO_STYLE").expect("FONO_TEST_KOKORO_STYLE");
        let dict = std::env::var("FONO_TEST_ESPEAK_DICT").expect("FONO_TEST_ESPEAK_DICT");
        let dir = std::env::temp_dir().join("fono-kokoro-engine-en");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(dir.join("espeak")).unwrap();
        std::fs::copy(&dict, dir.join("espeak").join("en_dict")).expect("stage en_dict");
        let engine =
            KokoroLocal::load(model, style, "en-us", dir.join("espeak")).expect("load KokoroLocal");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let audio = rt
            .block_on(engine.synthesize("The quick brown fox jumps over the lazy dog.", None, None))
            .expect("synthesize");

        assert_eq!(audio.sample_rate, SAMPLE_RATE);
        assert!(
            audio.pcm.len() > SAMPLE_RATE as usize / 2,
            "expected >0.5s, got {}",
            audio.pcm.len()
        );
        let peak = audio.pcm.iter().fold(0.0_f32, |m, &s| m.max(s.abs()));
        assert!(peak > 0.01, "output PCM is near-silent (peak {peak})");
        assert!(peak <= 1.5, "output PCM wildly out of range (peak {peak})");

        let empty = rt.block_on(engine.synthesize("   ", None, None)).unwrap();
        assert!(empty.pcm.is_empty());
    }
}
