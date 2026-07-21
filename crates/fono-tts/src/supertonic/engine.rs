// SPDX-License-Identifier: GPL-3.0-only
//! Supertonic flow-matching inference engine (Slice 2, Tasks 2.5 & 2.6).
//!
//! The back half of the Supertonic voice: four ONNX graphs run in sequence on
//! the shared `ort` runtime (ADR 0032), ported from the sherpa reference
//! `OfflineTtsSupertonicImpl::Process` / `ProcessChunksAndConcatenate`:
//!
//! 1. **duration predictor** (`text_ids`, `style_dp`, `text_mask`) → one
//!    duration in seconds for the utterance.
//! 2. **text encoder** (`text_ids`, `style_ttl`, `text_mask`) → a text
//!    embedding.
//! 3. **vector estimator** — the flow-matching step, run `num_steps` times,
//!    each time denoising the latent `xt` a little further toward speech.
//! 4. **vocoder** (`latent`) → the final `f32` waveform.
//!
//! The initial latent is Gaussian noise from a seeded generator ([`GaussianRng`])
//! so a fixed seed gives a reproducible waveform. [`SupertonicLocal`] chunks
//! long input (see [`super::chunker`]), synthesises each chunk with one shared
//! RNG, and concatenates the pieces with a short silence between them — exactly
//! as the reference does.
//!
//! Because `batch size == 1` throughout, the latent mask is all-ones, so the
//! reference's per-batch masking is a no-op and is omitted.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use fono_core::turn_trace::current_span;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;
use serde_json::json;

use super::chunker::{chunk_text, default_max_len};
use super::config::SupertonicConfig;
use super::frontend::{is_supported_lang, Frontend};
use super::style::SupertonicStyle;
use crate::traits::{TextToSpeech, TtsAudio};

/// Default speech-speed factor (reference `Generate` default).
const DEFAULT_SPEED: f32 = 1.05;
/// Default number of flow-matching denoising steps (reference default).
pub const DEFAULT_NUM_STEPS: i32 = 5;
/// Lowest speed factor worth allowing (below this speech is unnaturally slow).
pub const MIN_SPEED: f32 = 0.5;
/// Highest speed factor worth allowing (above this speech is unintelligible).
pub const MAX_SPEED: f32 = 2.0;
/// Lowest step count worth allowing: below this the flow-matching latent is too
/// under-denoised to produce usable speech.
pub const MIN_NUM_STEPS: i32 = 1;
/// Highest step count worth allowing: beyond this quality has saturated and the
/// (linear) time cost is wasted.
pub const MAX_NUM_STEPS: i32 = 32;
/// Default silence between chunks, in seconds (reference default).
const DEFAULT_SILENCE: f32 = 0.3;
/// Minimum utterance duration in seconds, preventing zero-length audio.
const MIN_DURATION: f32 = 0.1;
/// Cap on latent length, guarding against pathological OOM (reference value).
const MAX_LATENT_LEN: i32 = 10_000;

/// A minimal, self-contained Mersenne Twister (MT19937) matching the C++
/// `std::mt19937`, so a given seed reproduces the reference generator's raw
/// 32-bit stream exactly. Only what [`GaussianRng`] needs is implemented.
#[derive(Debug, Clone)]
struct Mt19937 {
    state: [u32; 624],
    index: usize,
}

impl Mt19937 {
    fn new(seed: u32) -> Self {
        let mut state = [0u32; 624];
        state[0] = seed;
        for i in 1..624 {
            state[i] = 1_812_433_253u32
                .wrapping_mul(state[i - 1] ^ (state[i - 1] >> 30))
                .wrapping_add(i as u32);
        }
        Self { state, index: 624 }
    }

    fn generate(&mut self) {
        const LOWER: u32 = 0x7fff_ffff;
        const UPPER: u32 = 0x8000_0000;
        for i in 0..624 {
            let y = (self.state[i] & UPPER) | (self.state[(i + 1) % 624] & LOWER);
            let mut next = self.state[(i + 397) % 624] ^ (y >> 1);
            if y & 1 != 0 {
                next ^= 0x9908_b0df;
            }
            self.state[i] = next;
        }
        self.index = 0;
    }

    fn next_u32(&mut self) -> u32 {
        if self.index >= 624 {
            self.generate();
        }
        let mut y = self.state[self.index];
        self.index += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^= y >> 18;
        y
    }
}

/// A seeded Gaussian (normal) generator matching the C++ reference's
/// deterministic mode: `std::mt19937` driving the Marsaglia polar method, so a
/// fixed seed yields a reproducible noise stream. Values are standard normal
/// (mean 0, stddev 1).
#[derive(Debug, Clone)]
pub struct GaussianRng {
    mt: Mt19937,
    saved: Option<f32>,
}

impl GaussianRng {
    /// Create a generator seeded with `seed` (matching `NormalDataGenerator`'s
    /// deterministic branch, which seeds `mt19937` with the raw value).
    #[must_use]
    pub fn new(seed: u32) -> Self {
        Self { mt: Mt19937::new(seed), saved: None }
    }

    /// A float in `[0, 1)`, matching libstdc++'s `generate_canonical<float>`
    /// for a 32-bit engine (one draw, divided by 2^32).
    fn canonical(&mut self) -> f32 {
        let r = f64::from(self.mt.next_u32()) / 4_294_967_296.0;
        // Guard against rounding up to exactly 1.0 when narrowing to f32.
        let v = r as f32;
        if v >= 1.0 {
            f32::from_bits(0x3f7f_ffff) // largest f32 < 1.0
        } else {
            v
        }
    }

    /// The next standard-normal sample (Marsaglia polar; caches its pair).
    // `mul_add` is intentionally avoided: matching libstdc++'s plain
    // `2.0 * x - 1.0` rounding keeps this stream bit-identical to the C++
    // reference generator.
    #[allow(clippy::suboptimal_flops)]
    fn next(&mut self) -> f32 {
        if let Some(s) = self.saved.take() {
            return s;
        }
        loop {
            let x = 2.0 * self.canonical() - 1.0;
            let y = 2.0 * self.canonical() - 1.0;
            let r2 = x * x + y * y;
            if r2 <= 1.0 && r2 != 0.0 {
                let mult = (-2.0 * r2.ln() / r2).sqrt();
                self.saved = Some(x * mult);
                return y * mult;
            }
        }
    }

    /// Fill `out` with standard-normal samples.
    pub fn fill(&mut self, out: &mut [f32]) {
        for slot in out.iter_mut() {
            *slot = self.next();
        }
    }
}

/// Compute the latent length (number of latent frames) for a duration, matching
/// the reference: `ceil(duration * sample_rate / chunk_size)`, capped at
/// [`MAX_LATENT_LEN`]. `chunk_size = base_chunk_size * chunk_compress_factor`.
#[must_use]
fn latent_len(duration_secs: f32, cfg: &SupertonicConfig) -> i32 {
    let wav_len_max = duration_secs * cfg.sample_rate as f32;
    let chunk_size = cfg.wav_chunk_size();
    let len = ((wav_len_max + chunk_size as f32 - 1.0) / chunk_size as f32) as i32;
    len.clamp(1, MAX_LATENT_LEN)
}

/// Number of output samples for a duration: `max(1, duration * sample_rate)`.
#[must_use]
fn wav_len(duration_secs: f32, sample_rate: i32) -> i64 {
    let n = (duration_secs * sample_rate as f32) as i64;
    n.max(1)
}

/// A loaded Supertonic voice engine: the four ONNX sessions, the parsed config,
/// the full multi-speaker style pack, and the text frontend. One instance
/// serves every language and speaker in the pack.
pub struct SupertonicLocal {
    /// `duration_predictor`, `text_encoder`, `vector_estimator`, `vocoder`.
    /// Each guarded by a mutex (ort `run` needs `&mut Session`); synthesis is
    /// serialised per engine, callers parallelise across sentences.
    duration_predictor: Arc<Mutex<Session>>,
    text_encoder: Arc<Mutex<Session>>,
    vector_estimator: Arc<Mutex<Session>>,
    vocoder: Arc<Mutex<Session>>,
    cfg: SupertonicConfig,
    style: Arc<SupertonicStyle>,
    frontend: Arc<Frontend>,
    /// Speaker id selected for this engine (0-based; clamped at use).
    sid: i64,
    /// Flow-matching denoising steps per chunk (defaults to
    /// [`DEFAULT_NUM_STEPS`]; the quality/latency knob).
    num_steps: i32,
    /// Speech-speed factor (defaults to [`DEFAULT_SPEED`]; the tempo knob).
    speed: f32,
}

/// Open one `.ort` model as a session on the shared minimal runtime, using the
/// same pre-optimised-model idiom as [`crate::kokoro`].
fn open_session(path: &Path, what: &str) -> Result<Session> {
    Session::builder()
        .map_err(|e| anyhow::anyhow!("create ort session builder: {e}"))?
        .with_optimization_level(GraphOptimizationLevel::Disable)
        .unwrap_or_else(ort::Error::recover)
        .with_intra_threads(1)
        .map_err(|e| anyhow::anyhow!("set intra-op threads: {e}"))?
        .commit_from_file(path)
        .map_err(|e| anyhow::anyhow!("load Supertonic {what} {}: {e}", path.display()))
}

impl SupertonicLocal {
    /// Load an engine from a resolved pack directory containing the four `.ort`
    /// graphs, `tts.json`, `voice.bin`, and `unicode_indexer.bin`. `sid` selects
    /// the speaker (clamped into range at synthesis time).
    pub fn load(pack_dir: impl AsRef<Path>, sid: i64) -> Result<Self> {
        crate::local::ensure_runtime();
        let dir: PathBuf = pack_dir.as_ref().to_path_buf();

        let cfg = SupertonicConfig::parse(
            &std::fs::read(dir.join("tts.json")).context("read Supertonic tts.json")?,
        )?;
        let style = SupertonicStyle::parse(
            &std::fs::read(dir.join("voice.bin")).context("read Supertonic voice.bin")?,
        )?;
        let frontend = Frontend::load(&dir.join("unicode_indexer.bin"))?;

        let duration_predictor =
            open_session(&dir.join("duration_predictor.ort"), "duration predictor")?;
        let text_encoder = open_session(&dir.join("text_encoder.ort"), "text encoder")?;
        let vector_estimator = open_session(&dir.join("vector_estimator.ort"), "vector estimator")?;
        let vocoder = open_session(&dir.join("vocoder.ort"), "vocoder")?;

        Ok(Self {
            duration_predictor: Arc::new(Mutex::new(duration_predictor)),
            text_encoder: Arc::new(Mutex::new(text_encoder)),
            vector_estimator: Arc::new(Mutex::new(vector_estimator)),
            vocoder: Arc::new(Mutex::new(vocoder)),
            cfg,
            style: Arc::new(style),
            frontend: Arc::new(frontend),
            sid,
            num_steps: DEFAULT_NUM_STEPS,
            speed: DEFAULT_SPEED,
        })
    }

    /// Override the flow-matching step count (clamped to
    /// `[MIN_NUM_STEPS, MAX_NUM_STEPS]`). Fewer steps are faster but lower
    /// quality; more steps cost linearly more time for diminishing gains.
    #[must_use]
    pub fn with_num_steps(mut self, steps: i32) -> Self {
        self.num_steps = steps.clamp(MIN_NUM_STEPS, MAX_NUM_STEPS);
        self
    }

    /// Override the speech-speed factor (clamped to `[MIN_SPEED, MAX_SPEED]`).
    /// Values below 1.0 slow speech down; above 1.0 speed it up.
    #[must_use]
    pub fn with_speed(mut self, speed: f32) -> Self {
        self.speed = speed.clamp(MIN_SPEED, MAX_SPEED);
        self
    }

    /// Synthesise one already-chunked text into mono `f32` PCM, advancing `rng`.
    /// Ported from the reference `Process`. Returns an empty vector when the
    /// text produces no tokens (the caller skips it).
    #[allow(clippy::too_many_lines, clippy::significant_drop_tightening)]
    fn run_chunk(
        &self,
        text: &str,
        lang: &str,
        num_steps: i32,
        speed: f32,
        rng: &mut GaussianRng,
    ) -> Result<Vec<f32>> {
        let slice = self.style.slice_for_sid(self.sid);

        // Frontend: text → token ids; mask is all-ones of shape [1,1,len].
        let text_ids = self.frontend.process(text, lang);
        if text_ids.is_empty() {
            return Ok(Vec::new());
        }
        let seq_len = text_ids.len() as i64;
        let text_mask: Vec<f32> = vec![1.0; text_ids.len()];
        let text_mask_shape = vec![1_i64, 1, seq_len];
        let text_ids_shape = vec![1_i64, seq_len];

        // 1. Duration predictor → one duration, adjusted for speed.
        let dp_out = {
            let text_ids_t = Tensor::from_array((text_ids_shape.clone(), text_ids.clone()))
                .map_err(|e| anyhow::anyhow!("build text_ids tensor: {e}"))?;
            let style_dp_t = Tensor::from_array((slice.dp_shape.to_vec(), slice.dp_data.to_vec()))
                .map_err(|e| anyhow::anyhow!("build style_dp tensor: {e}"))?;
            let text_mask_t = Tensor::from_array((text_mask_shape.clone(), text_mask.clone()))
                .map_err(|e| anyhow::anyhow!("build text_mask tensor: {e}"))?;
            let mut sess = self.duration_predictor.lock().expect("dp mutex poisoned");
            let outputs = sess
                .run(ort::inputs![text_ids_t, style_dp_t, text_mask_t])
                .map_err(|e| anyhow::anyhow!("run duration predictor: {e}"))?;
            let (_shape, data) = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow::anyhow!("extract duration: {e}"))?;
            data.to_vec()
        };
        if dp_out.len() != 1 {
            bail!("duration predictor returned {} values, expected 1", dp_out.len());
        }
        let mut duration = dp_out[0];
        if (speed - 1.0).abs() > f32::EPSILON {
            duration = (duration / speed).max(MIN_DURATION);
        }

        // 2. Text encoder → text embedding (kept for the estimator loop).
        let (text_emb, text_emb_shape) = {
            let text_ids_t = Tensor::from_array((text_ids_shape, text_ids))
                .map_err(|e| anyhow::anyhow!("build text_ids tensor: {e}"))?;
            let style_ttl_t =
                Tensor::from_array((slice.ttl_shape.to_vec(), slice.ttl_data.to_vec()))
                    .map_err(|e| anyhow::anyhow!("build style_ttl tensor: {e}"))?;
            let text_mask_t = Tensor::from_array((text_mask_shape.clone(), text_mask.clone()))
                .map_err(|e| anyhow::anyhow!("build text_mask tensor: {e}"))?;
            let mut sess = self.text_encoder.lock().expect("text encoder mutex poisoned");
            let outputs = sess
                .run(ort::inputs![text_ids_t, style_ttl_t, text_mask_t])
                .map_err(|e| anyhow::anyhow!("run text encoder: {e}"))?;
            let (shape, data) = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow::anyhow!("extract text embedding: {e}"))?;
            (data.to_vec(), shape.iter().copied().collect::<Vec<i64>>())
        };
        if text_emb.is_empty() {
            bail!("text encoder produced an empty embedding");
        }

        // Latent geometry.
        let latent_len = latent_len(duration, &self.cfg);
        let latent_dim = self.cfg.latent_dim_full();
        let latent_total = latent_dim as usize * latent_len as usize;
        let latent_shape = vec![1_i64, i64::from(latent_dim), i64::from(latent_len)];
        let latent_mask: Vec<f32> = vec![1.0; latent_len as usize];
        let latent_mask_shape = vec![1_i64, 1, i64::from(latent_len)];
        let total_step = vec![num_steps as f32];
        let step_shape = vec![1_i64];

        // Initial latent: seeded Gaussian noise. (bsz==1 ⇒ latent mask is
        // all-ones, so the reference's masking multiply is a no-op.)
        let mut xt = vec![0.0f32; latent_total];
        rng.fill(&mut xt);

        // 3. Vector estimator: iteratively denoise xt over num_steps.
        for step in 0..num_steps {
            let current_step = vec![step as f32];
            let noisy = Tensor::from_array((latent_shape.clone(), xt.clone()))
                .map_err(|e| anyhow::anyhow!("build noisy latent tensor: {e}"))?;
            let text_emb_t = Tensor::from_array((text_emb_shape.clone(), text_emb.clone()))
                .map_err(|e| anyhow::anyhow!("build text_emb tensor: {e}"))?;
            let style_ttl_t =
                Tensor::from_array((slice.ttl_shape.to_vec(), slice.ttl_data.to_vec()))
                    .map_err(|e| anyhow::anyhow!("build style_ttl tensor: {e}"))?;
            let latent_mask_t =
                Tensor::from_array((latent_mask_shape.clone(), latent_mask.clone()))
                    .map_err(|e| anyhow::anyhow!("build latent_mask tensor: {e}"))?;
            let text_mask_t = Tensor::from_array((text_mask_shape.clone(), text_mask.clone()))
                .map_err(|e| anyhow::anyhow!("build text_mask tensor: {e}"))?;
            let current_step_t = Tensor::from_array((step_shape.clone(), current_step))
                .map_err(|e| anyhow::anyhow!("build current_step tensor: {e}"))?;
            let total_step_t = Tensor::from_array((step_shape.clone(), total_step.clone()))
                .map_err(|e| anyhow::anyhow!("build total_step tensor: {e}"))?;

            let mut sess = self.vector_estimator.lock().expect("vector estimator mutex poisoned");
            let outputs = sess
                .run(ort::inputs![
                    noisy,
                    text_emb_t,
                    style_ttl_t,
                    latent_mask_t,
                    text_mask_t,
                    current_step_t,
                    total_step_t
                ])
                .map_err(|e| anyhow::anyhow!("run vector estimator: {e}"))?;
            let (_shape, denoised) = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow::anyhow!("extract denoised latent: {e}"))?;
            if denoised.len() != latent_total {
                bail!(
                    "vector estimator step {step}: got {} latents, expected {latent_total}",
                    denoised.len()
                );
            }
            xt.copy_from_slice(denoised);
        }

        // 4. Vocoder: latent → waveform, trimmed to the predicted sample count.
        let wav = {
            let latent_t = Tensor::from_array((latent_shape, xt))
                .map_err(|e| anyhow::anyhow!("build latent tensor: {e}"))?;
            let mut sess = self.vocoder.lock().expect("vocoder mutex poisoned");
            let outputs = sess
                .run(ort::inputs![latent_t])
                .map_err(|e| anyhow::anyhow!("run vocoder: {e}"))?;
            let (_shape, data) = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow::anyhow!("extract waveform: {e}"))?;
            data.to_vec()
        };
        if wav.is_empty() {
            bail!("vocoder produced an empty waveform");
        }
        let want = wav_len(duration, self.cfg.sample_rate) as usize;
        let take = want.min(wav.len());
        Ok(wav[..take].to_vec())
    }

    /// Resolve the language: default `en`, error on an unsupported code.
    fn resolve_lang(lang: Option<&str>) -> Result<String> {
        let lang = lang.unwrap_or("en");
        if !is_supported_lang(lang) {
            bail!("unsupported Supertonic language '{lang}' (see the 31-language allowlist)");
        }
        Ok(lang.to_string())
    }
}

#[async_trait]
impl TextToSpeech for SupertonicLocal {
    async fn synthesize(
        &self,
        text: &str,
        _voice: Option<&str>,
        lang: Option<&str>,
    ) -> Result<TtsAudio> {
        let sample_rate = self.cfg.sample_rate as u32;
        if text.trim().is_empty() {
            return Ok(TtsAudio { pcm: Vec::new(), sample_rate });
        }
        let lang = Self::resolve_lang(lang)?;
        let chunks = chunk_text(text, default_max_len(&lang));
        if chunks.is_empty() {
            return Ok(TtsAudio { pcm: Vec::new(), sample_rate });
        }

        // One engine handle + RNG moved into the blocking task; inference is
        // CPU-bound and blocking, so keep it off the async runtime.
        let engine = self.clone_handle();
        let span = current_span("tts.supertonic_synthesize", "assistant.tts", "tts");
        let n_chunks = chunks.len();
        let pcm = tokio::task::spawn_blocking(move || engine.synthesize_blocking(&chunks, &lang))
            .await
            .context("supertonic inference task")??;
        span.finish(
            json!({ "chunks": n_chunks, "samples": pcm.len(), "sample_rate": sample_rate }),
        );
        Ok(TtsAudio { pcm, sample_rate })
    }

    fn name(&self) -> &'static str {
        "supertonic-local"
    }

    fn native_sample_rate(&self) -> u32 {
        self.cfg.sample_rate as u32
    }
}

impl SupertonicLocal {
    /// Cheap clone of the shared engine state for a blocking task.
    fn clone_handle(&self) -> Self {
        Self {
            duration_predictor: Arc::clone(&self.duration_predictor),
            text_encoder: Arc::clone(&self.text_encoder),
            vector_estimator: Arc::clone(&self.vector_estimator),
            vocoder: Arc::clone(&self.vocoder),
            cfg: self.cfg,
            style: Arc::clone(&self.style),
            frontend: Arc::clone(&self.frontend),
            sid: self.sid,
            num_steps: self.num_steps,
            speed: self.speed,
        }
    }

    /// Synthesise all chunks with one shared RNG and concatenate them with a
    /// short silence between (reference `ProcessChunksAndConcatenate`). A random
    /// seed is drawn per call so successive utterances differ naturally.
    fn synthesize_blocking(&self, chunks: &[String], lang: &str) -> Result<Vec<f32>> {
        let seed = rng_seed_from_time();
        let mut rng = GaussianRng::new(seed);
        let silence_len = (DEFAULT_SILENCE * self.cfg.sample_rate as f32) as usize;
        let mut out: Vec<f32> = Vec::new();
        for chunk in chunks {
            let samples = self.run_chunk(chunk, lang, self.num_steps, self.speed, &mut rng)?;
            if samples.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.extend(std::iter::repeat_n(0.0, silence_len));
            }
            out.extend_from_slice(&samples);
        }
        Ok(out)
    }
}

/// A non-cryptographic seed from the wall clock, for per-utterance noise.
fn rng_seed_from_time() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0x1234_5678)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mt19937_matches_the_reference_stream() {
        // The C++ standard fixes mt19937(5489)'s 10000th output at 4123659995.
        let mut mt = Mt19937::new(5489);
        let mut last = 0;
        for _ in 0..10_000 {
            last = mt.next_u32();
        }
        assert_eq!(last, 4_123_659_995);
    }

    #[test]
    fn gaussian_is_deterministic_for_a_seed() {
        let bits = |xs: &[f32; 8]| xs.iter().map(|f| f.to_bits()).collect::<Vec<u32>>();
        let mut a = GaussianRng::new(42);
        let mut b = GaussianRng::new(42);
        let (mut xa, mut xb) = ([0.0f32; 8], [0.0f32; 8]);
        a.fill(&mut xa);
        b.fill(&mut xb);
        assert_eq!(bits(&xa), bits(&xb), "same seed must reproduce the same noise");

        let mut c = GaussianRng::new(43);
        let mut xc = [0.0f32; 8];
        c.fill(&mut xc);
        assert_ne!(bits(&xa), bits(&xc), "different seeds should differ");
    }

    #[test]
    fn gaussian_is_roughly_standard_normal() {
        let mut rng = GaussianRng::new(7);
        let mut xs = vec![0.0f32; 20_000];
        rng.fill(&mut xs);
        let n = xs.len() as f32;
        let mean = xs.iter().sum::<f32>() / n;
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / n;
        assert!(mean.abs() < 0.05, "mean {mean} not ~0");
        assert!((var - 1.0).abs() < 0.1, "variance {var} not ~1");
        assert!(xs.iter().all(|x| x.is_finite()), "all samples finite");
    }

    #[test]
    fn latent_len_rounds_up_and_caps() {
        let cfg = SupertonicConfig {
            sample_rate: 44_100,
            base_chunk_size: 512,
            latent_dim: 24,
            chunk_compress_factor: 6,
        };
        // chunk_size = 512*6 = 3072. 1s → 44100 samples → ceil(44100/3072) = 15.
        assert_eq!(latent_len(1.0, &cfg), 15);
        // Never below 1.
        assert_eq!(latent_len(0.0, &cfg), 1);
        // Capped.
        assert_eq!(latent_len(1_000_000.0, &cfg), MAX_LATENT_LEN);
    }

    #[test]
    fn wav_len_is_at_least_one() {
        assert_eq!(wav_len(1.0, 44_100), 44_100);
        assert_eq!(wav_len(0.0, 44_100), 1);
    }

    #[test]
    fn resolve_lang_defaults_and_validates() {
        assert_eq!(SupertonicLocal::resolve_lang(None).unwrap(), "en");
        assert_eq!(SupertonicLocal::resolve_lang(Some("ro")).unwrap(), "ro");
        assert!(SupertonicLocal::resolve_lang(Some("zz")).is_err());
    }

    /// Full pipeline against a real converted Supertonic pack, including `ort`
    /// inference. Ignored by default: it needs the four graphs converted to
    /// `.ort`, the pack's data files, and a linked runtime, none present in a
    /// plain `cargo test` (Slice 3 lands the conversion + runtime rebuild). Run
    /// with:
    ///
    /// ```text
    /// ORT_LIB_LOCATION=tmp/supertonic-build \
    /// FONO_TEST_SUPERTONIC_DIR=tmp/supertonic/publish \
    ///   cargo test -p fono-tts --features tts-local -- --ignored supertonic_local
    /// ```
    #[test]
    #[ignore = "needs a converted .ort pack + linked runtime (see doc comment)"]
    fn supertonic_local_synthesizes_real_audio() {
        let dir = std::env::var("FONO_TEST_SUPERTONIC_DIR").expect("FONO_TEST_SUPERTONIC_DIR");
        let engine = SupertonicLocal::load(&dir, 0).expect("load SupertonicLocal");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let audio = rt
            .block_on(engine.synthesize(
                "The quick brown fox jumps over the lazy dog.",
                None,
                Some("en"),
            ))
            .expect("synthesize");

        assert_eq!(audio.sample_rate, engine.native_sample_rate());
        assert!(
            audio.pcm.len() > audio.sample_rate as usize / 2,
            "expected >0.5s, got {}",
            audio.pcm.len()
        );
        let peak = audio.pcm.iter().fold(0.0_f32, |m, &s| m.max(s.abs()));
        assert!(peak > 0.01, "output PCM is near-silent (peak {peak})");
        assert!(peak <= 1.5, "output PCM wildly out of range (peak {peak})");

        let empty = rt.block_on(engine.synthesize("   ", None, Some("en"))).unwrap();
        assert!(empty.pcm.is_empty());
    }
}
