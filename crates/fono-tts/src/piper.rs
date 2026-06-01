// SPDX-License-Identifier: GPL-3.0-only
//! Piper voice support for the local ONNX voice engine (feature `tts-local`).
//!
//! Front half of the Piper pipeline, on the shared `ort` runtime (ADR 0032):
//!
//! 1. **text → IPA** via the pure-Rust [`espeak_ng`] phonemizer (no system
//!    `libespeak-ng`; the shared G2P core is embedded and per-language dicts
//!    download on demand — see [`crate::espeak`] and [`PiperVoice::phonemize`]).
//! 2. **IPA → phoneme ids** via the voice's `.onnx.json` map, using the
//!    canonical piper-phonemize layout (BOS, interspersed PAD, EOS — see
//!    [`PiperConfig::phoneme_ids`]).
//!
//! The ids then feed the Piper VITS `.ort` model through `ort` to produce
//! mono `f32` PCM at [`AudioConfig::sample_rate`] — that back half is
//! [`PiperLocal`], the [`TextToSpeech`] implementation. [`PiperConfig`] and
//! [`PiperVoice`] are the deterministic, unit-testable front half it builds on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use async_trait::async_trait;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;
use serde::Deserialize;

use crate::traits::{TextToSpeech, TtsAudio};

/// Piper's special phoneme tokens (see piper-phonemize). `_` pads between
/// every emitted phoneme, `^` marks begin-of-sentence, `$` end-of-sentence.
const PAD: &str = "_";
const BOS: &str = "^";
const EOS: &str = "$";

/// The subset of a Piper voice's companion `<voice>.onnx.json` we consume.
///
/// Piper ships each voice as an ONNX model plus this JSON sidecar; only the
/// fields below drive synthesis. Unknown fields are ignored so future Piper
/// config additions do not break parsing.
#[derive(Debug, Clone, Deserialize)]
pub struct PiperConfig {
    pub audio: AudioConfig,
    pub espeak: EspeakConfig,
    #[serde(default)]
    pub inference: InferenceConfig,
    /// Maps each phoneme (an IPA grapheme, or a special token) to its model
    /// input id(s). Values are sequences because a handful of phonemes map
    /// to more than one id.
    pub phoneme_id_map: HashMap<String, Vec<i64>>,
}

/// Output audio format declared by the voice.
#[derive(Debug, Clone, Deserialize)]
pub struct AudioConfig {
    /// Native PCM sample rate, e.g. 22050 Hz for `*-medium` voices.
    pub sample_rate: u32,
}

/// Phonemizer settings: which espeak-ng voice generates the IPA.
#[derive(Debug, Clone, Deserialize)]
pub struct EspeakConfig {
    /// espeak-ng language/voice code, e.g. `"ro"`.
    pub voice: String,
}

/// VITS inference knobs. Defaults match Piper's documented values, used when
/// the sidecar omits the `inference` block.
#[derive(Debug, Clone, Deserialize)]
pub struct InferenceConfig {
    pub noise_scale: f32,
    pub length_scale: f32,
    pub noise_w: f32,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self { noise_scale: 0.667, length_scale: 1.0, noise_w: 0.8 }
    }
}

impl PiperConfig {
    /// Parse a voice's `.onnx.json` sidecar.
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).context("parse Piper .onnx.json sidecar")
    }

    /// Encode an IPA phoneme string into Piper model input ids.
    ///
    /// Follows piper-phonemize's default `interspersePad` layout: begin with
    /// BOS then a PAD, emit each known phoneme followed by a PAD, and finish
    /// with EOS. IPA codepoints absent from the map are skipped — matching
    /// Piper's own handling of phonemes a voice does not model.
    ///
    /// Returns an empty vector if the map lacks the required special tokens
    /// (a malformed sidecar); callers treat an empty id list as "nothing to
    /// synthesize".
    pub fn phoneme_ids(&self, ipa: &str) -> Vec<i64> {
        let mut ids = Vec::with_capacity(ipa.chars().count() * 2 + 3);
        match self.phoneme_id_map.get(BOS) {
            Some(bos) => ids.extend_from_slice(bos),
            None => return Vec::new(),
        }
        if let Some(pad) = self.phoneme_id_map.get(PAD) {
            ids.extend_from_slice(pad);
        }
        let mut buf = [0u8; 4];
        for ch in ipa.chars() {
            let key: &str = ch.encode_utf8(&mut buf);
            if let Some(mapped) = self.phoneme_id_map.get(key) {
                ids.extend_from_slice(mapped);
                if let Some(pad) = self.phoneme_id_map.get(PAD) {
                    ids.extend_from_slice(pad);
                }
            }
        }
        if let Some(eos) = self.phoneme_id_map.get(EOS) {
            ids.extend_from_slice(eos);
        }
        ids
    }
}

/// A loaded Piper voice ready to turn text into model input ids.
///
/// Owns the parsed [`PiperConfig`] and the on-disk directory holding the
/// espeak-ng language data extracted for this voice. Construct once per voice
/// and reuse; [`Self::phonemize`] and [`Self::text_to_ids`] are cheap.
#[derive(Debug, Clone)]
pub struct PiperVoice {
    config: PiperConfig,
    data_dir: PathBuf,
}

impl PiperVoice {
    /// Build a voice from its parsed config, materialising the embedded
    /// espeak-ng G2P core under `data_dir`.
    ///
    /// `data_dir` is a persistent cache location (e.g. under the voice cache);
    /// the embedded core (~102 KiB, [`crate::espeak::install_core`]) is written
    /// there. The matching per-language `<lang>_dict` is **not** written here —
    /// it downloads on demand (see [`crate::voices`]) into the same directory
    /// and must be present before [`Self::phonemize`] runs.
    pub fn new(config: PiperConfig, data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        crate::espeak::install_core(&data_dir)?;
        Ok(Self { config, data_dir })
    }

    /// Native output sample rate of this voice, in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.config.audio.sample_rate
    }

    /// VITS inference parameters for this voice.
    pub fn inference(&self) -> &InferenceConfig {
        &self.config.inference
    }

    /// Phonemize `text` to an IPA string using this voice's espeak-ng data.
    pub fn phonemize(&self, text: &str) -> Result<String> {
        // Fold the voice's espeak code onto the canonical base language whose
        // phoneme table ships in the embedded core (e.g. nb→no, en-gb-x-rp→en);
        // this also matches the downloaded `<canonical>_dict` filename.
        let voice = crate::espeak::canonical_lang(&self.config.espeak.voice);
        let translator = espeak_ng::Translator::new(voice, Some(self.data_dir.as_path()))
            .map_err(|e| anyhow::anyhow!("espeak-ng translator init for '{voice}': {e}"))?;
        translator
            .text_to_ipa(text)
            .map_err(|e| anyhow::anyhow!("espeak-ng phonemize '{voice}': {e}"))
    }

    /// Full front half: `text` → IPA → Piper model input ids.
    pub fn text_to_ids(&self, text: &str) -> Result<Vec<i64>> {
        Ok(self.config.phoneme_ids(&self.phonemize(text)?))
    }
}

/// A local Piper TTS engine: the [`TextToSpeech`] back half.
///
/// Loads a Piper VITS `.ort` model into an `ort` [`Session`] on the shared,
/// statically-linked ONNX Runtime (ADR 0032) and pairs it with a
/// [`PiperVoice`] front half. `synthesize` runs the standard Piper graph
/// (`input` ids + `input_lengths` + `scales` → `output` PCM) and returns mono
/// `f32` PCM at the voice's native sample rate.
///
/// The model must be the `.ort` flatbuffer produced by
/// `scripts/gen-ort-models.sh`; a `--minimal_build` runtime cannot load plain
/// `.onnx`. One engine wraps one voice; the language router (plan task 2.4)
/// owns a map of these.
pub struct PiperLocal {
    voice: PiperVoice,
    /// `run` needs `&mut Session`; the `Mutex` gives interior mutability and
    /// the `Arc` lets a blocking inference task own a handle. Synthesis is
    /// serialised per engine, which is fine — callers parallelise across
    /// sentences, not within one.
    session: Arc<Mutex<Session>>,
    sample_rate: u32,
}

impl PiperLocal {
    /// Load a Piper voice: parse its `.onnx.json` sidecar, materialise the
    /// embedded espeak-ng data under `espeak_data_dir`, and open the `.ort`
    /// model at `model_path`.
    pub fn load(
        model_path: impl AsRef<Path>,
        config: PiperConfig,
        espeak_data_dir: impl Into<PathBuf>,
    ) -> Result<Self> {
        // Ensure the process-wide ONNX Runtime environment exists before the
        // first session is built (idempotent; see `local::ensure_runtime`).
        crate::local::ensure_runtime();
        let sample_rate = config.audio.sample_rate;
        let voice = PiperVoice::new(config, espeak_data_dir)?;
        let model_path = model_path.as_ref();
        // `ort`'s builder methods return a generic `Error<SessionBuilder>` that
        // is not `StdError + Send + Sync`, so map via `Display` rather than `?`.
        let session = Session::builder()
            .map_err(|e| anyhow::anyhow!("create ort session builder: {e}"))?
            // `.ort` models are already optimised at conversion time, and a
            // minimal-build runtime has the optimiser compiled out entirely —
            // setting a level *errors*, so recover the builder and carry on
            // (the documented `ort` minimal-build idiom).
            .with_optimization_level(GraphOptimizationLevel::Disable)
            .unwrap_or_else(ort::Error::recover)
            .with_intra_threads(1)
            .map_err(|e| anyhow::anyhow!("set intra-op threads: {e}"))?
            .commit_from_file(model_path)
            .map_err(|e| anyhow::anyhow!("load Piper model {}: {e}", model_path.display()))?;
        Ok(Self { voice, session: Arc::new(Mutex::new(session)), sample_rate })
    }

    /// Run the Piper VITS graph for a prepared id sequence. Blocking; call via
    /// `spawn_blocking`. Returns flattened mono `f32` PCM.
    // The session guard must span both `run` and the output extract (the PCM
    // slice borrows the run outputs), so it cannot be tightened further.
    #[allow(clippy::significant_drop_tightening)]
    fn run_inference(
        session: &Mutex<Session>,
        ids: Vec<i64>,
        scales: [f32; 3],
    ) -> Result<Vec<f32>> {
        let n = ids.len() as i64;
        let input = Tensor::from_array((vec![1_i64, n], ids))
            .map_err(|e| anyhow::anyhow!("build input tensor: {e}"))?;
        let input_lengths = Tensor::from_array((vec![1_i64], vec![n]))
            .map_err(|e| anyhow::anyhow!("build input_lengths tensor: {e}"))?;
        let scales = Tensor::from_array((vec![3_i64], scales.to_vec()))
            .map_err(|e| anyhow::anyhow!("build scales tensor: {e}"))?;

        // Hold the session lock only for the inference + extract, then drop it
        // before returning (keeps the guard's scope tight).
        let pcm: Vec<f32> = {
            let mut session = session.lock().expect("piper session mutex poisoned");
            let outputs = session
                .run(ort::inputs![
                    "input" => input,
                    "input_lengths" => input_lengths,
                    "scales" => scales,
                ])
                .map_err(|e| anyhow::anyhow!("run Piper inference: {e}"))?;
            let (_shape, pcm) = outputs["output"]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow::anyhow!("extract Piper output PCM: {e}"))?;
            pcm.to_vec()
        };
        Ok(pcm)
    }
}

#[async_trait]
impl TextToSpeech for PiperLocal {
    async fn synthesize(
        &self,
        text: &str,
        _voice: Option<&str>,
        _lang: Option<&str>,
    ) -> Result<TtsAudio> {
        // Empty input → empty PCM (trait contract for the stream-end case).
        if text.trim().is_empty() {
            return Ok(TtsAudio { pcm: Vec::new(), sample_rate: self.sample_rate });
        }
        let ids = self.voice.text_to_ids(text)?;
        if ids.is_empty() {
            return Ok(TtsAudio { pcm: Vec::new(), sample_rate: self.sample_rate });
        }
        let inf = self.voice.inference();
        let scales = [inf.noise_scale, inf.length_scale, inf.noise_w];
        let session = Arc::clone(&self.session);
        // ONNX inference is CPU-bound and blocking; keep it off the async runtime.
        let pcm = tokio::task::spawn_blocking(move || Self::run_inference(&session, ids, scales))
            .await
            .context("piper inference task")??;
        Ok(TtsAudio { pcm, sample_rate: self.sample_rate })
    }

    fn name(&self) -> &'static str {
        "piper-local"
    }

    fn native_sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal sidecar mirroring the real `ro_RO-mihai-medium.onnx.json`
    /// shape (verified against the published voice): 22.05 kHz, espeak voice
    /// "ro", documented inference defaults, and the canonical special-token
    /// ids `_`=0, `^`=1, `$`=2.
    const SAMPLE_JSON: &str = r#"{
        "audio": { "sample_rate": 22050 },
        "espeak": { "voice": "ro" },
        "inference": { "noise_scale": 0.667, "length_scale": 1.0, "noise_w": 0.8 },
        "phoneme_id_map": {
            "_": [0], "^": [1], "$": [2], " ": [3], "a": [5], "b": [6]
        }
    }"#;

    fn sample() -> PiperConfig {
        PiperConfig::from_json(SAMPLE_JSON.as_bytes()).unwrap()
    }

    #[test]
    fn parses_real_sidecar_shape() {
        let c = sample();
        assert_eq!(c.audio.sample_rate, 22050);
        assert_eq!(c.espeak.voice, "ro");
        assert!((c.inference.noise_scale - 0.667).abs() < 1e-6);
        assert!((c.inference.length_scale - 1.0).abs() < 1e-6);
    }

    #[test]
    fn inference_defaults_when_omitted() {
        let json = r#"{ "audio": {"sample_rate": 16000}, "espeak": {"voice":"en"},
            "phoneme_id_map": {"_":[0],"^":[1],"$":[2]} }"#;
        let c = PiperConfig::from_json(json.as_bytes()).unwrap();
        let d = InferenceConfig::default();
        assert!((c.inference.noise_w - d.noise_w).abs() < 1e-6);
    }

    #[test]
    fn phoneme_ids_follow_interspersed_pad_layout() {
        // "ab" → BOS, PAD, a, PAD, b, PAD, EOS = 1,0,5,0,6,0,2
        assert_eq!(sample().phoneme_ids("ab"), vec![1, 0, 5, 0, 6, 0, 2]);
    }

    #[test]
    fn phoneme_ids_skip_unmapped_codepoints() {
        // 'x' and 'ə' are not in the map; they are dropped, not errored.
        assert_eq!(sample().phoneme_ids("axb"), vec![1, 0, 5, 0, 6, 0, 2]);
        assert_eq!(sample().phoneme_ids("aəb"), vec![1, 0, 5, 0, 6, 0, 2]);
    }

    #[test]
    fn phoneme_ids_empty_without_bos() {
        let json = r#"{ "audio":{"sample_rate":22050}, "espeak":{"voice":"ro"},
            "phoneme_id_map": {"a":[5]} }"#;
        let c = PiperConfig::from_json(json.as_bytes()).unwrap();
        assert!(c.phoneme_ids("a").is_empty());
    }

    /// End-to-end front half against the *embedded* G2P core plus a real
    /// `<lang>_dict`. Builds a voice in a temp dir, installs the embedded core,
    /// drops in the dictionary, phonemizes Romanian, and checks the id stream
    /// is well-formed (BOS-prefixed, EOS-suffixed, non-trivial).
    ///
    /// Ignored by default: `tts-local` links `ort` (needs the static runtime),
    /// and the test needs a `ro_dict` it can't bundle. Provide one via
    /// `FONO_TEST_ESPEAK_DICT` (path to a `ro_dict`) and run with:
    ///
    /// ```text
    /// ORT_LIB_LOCATION=tmp/onnxruntime-minimal \
    /// FONO_TEST_ESPEAK_DICT=/path/to/ro_dict \
    ///   cargo test -p fono-tts --features tts-local -- --ignored text_to_ids
    /// ```
    #[test]
    #[ignore = "needs a linked runtime + a ro_dict via FONO_TEST_ESPEAK_DICT"]
    fn romanian_text_to_ids_end_to_end() {
        let dict = std::env::var("FONO_TEST_ESPEAK_DICT").expect("FONO_TEST_ESPEAK_DICT");
        let dir = std::env::temp_dir().join("fono-piper-test-ro");
        std::fs::create_dir_all(&dir).unwrap();
        // Stage the dictionary the way the voice mirror would, then build the
        // voice (which writes the embedded core alongside it).
        std::fs::copy(&dict, dir.join("ro_dict")).expect("stage ro_dict");
        let voice = PiperVoice::new(sample(), &dir).expect("build ro voice");

        let ipa = voice.phonemize("Bună ziua").expect("phonemize");
        assert!(!ipa.is_empty(), "espeak produced empty IPA");

        let ids = voice.text_to_ids("Bună ziua").expect("text_to_ids");
        assert_eq!(ids.first(), Some(&1), "must start with BOS id");
        assert_eq!(ids.last(), Some(&2), "must end with EOS id");
        assert!(ids.len() > 3, "expected real phoneme ids between BOS/EOS, got {ids:?}");
    }

    /// Full pipeline against a real Piper `.ort` model + sidecar, including
    /// `ort` inference. Ignored by default: needs the converted model artefact
    /// and a linked runtime, neither present in a plain `cargo test`. Run with:
    ///
    /// ```text
    /// ORT_LIB_LOCATION=tmp/onnxruntime-minimal \
    /// FONO_TEST_PIPER_ORT=tmp/voice-models/ort/ro_RO-mihai-medium.ort \
    /// FONO_TEST_PIPER_JSON=tmp/voice-models/ro_RO-mihai-medium.onnx.json \
    /// FONO_TEST_ESPEAK_DICT=/path/to/ro_dict \
    ///   cargo test -p fono-tts --features tts-local -- --ignored piper_local
    /// ```
    #[test]
    #[ignore = "needs a converted .ort model + linked runtime (see doc comment)"]
    fn piper_local_synthesizes_real_audio() {
        let model = std::env::var("FONO_TEST_PIPER_ORT").expect("FONO_TEST_PIPER_ORT");
        let json = std::env::var("FONO_TEST_PIPER_JSON").expect("FONO_TEST_PIPER_JSON");
        let dict = std::env::var("FONO_TEST_ESPEAK_DICT").expect("FONO_TEST_ESPEAK_DICT");
        let config = PiperConfig::from_json(&std::fs::read(json).unwrap()).unwrap();
        let dir = std::env::temp_dir().join("fono-piper-engine-ro");
        std::fs::create_dir_all(&dir).unwrap();
        // Stage the dictionary the way the voice mirror would; `PiperLocal::load`
        // writes the embedded core alongside it.
        std::fs::copy(&dict, dir.join("ro_dict")).expect("stage ro_dict");
        let engine = PiperLocal::load(model, config, &dir).expect("load PiperLocal");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let audio = rt
            .block_on(engine.synthesize("Bună ziua, acesta este un test.", None, None))
            .expect("synthesize");

        assert_eq!(audio.sample_rate, 22050);
        assert!(
            audio.pcm.len() > 22050 / 2,
            "expected >0.5s of audio, got {} samples",
            audio.pcm.len()
        );
        let peak = audio.pcm.iter().fold(0.0_f32, |m, &s| m.max(s.abs()));
        assert!(peak > 0.01, "output PCM is near-silent (peak {peak})");
        assert!(peak <= 1.5, "output PCM wildly out of range (peak {peak})");

        // Empty text → empty PCM (trait contract).
        let empty = rt.block_on(engine.synthesize("   ", None, None)).unwrap();
        assert!(empty.pcm.is_empty());
    }
}
