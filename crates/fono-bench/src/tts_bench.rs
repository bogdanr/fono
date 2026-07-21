// SPDX-License-Identifier: GPL-3.0-only
//! TTS backend benchmark (`fono-bench tts`) — feature `tts-local`.
//!
//! Compares Fono's three local ONNX voice backends — **Piper**, **Kokoro**,
//! and **Supertonic** — across English and Romanian, using deliberately
//! difficult sentences and several voices per language, so the maintainer can
//! decide whether to change the default local TTS engine/voice.
//!
//! Each backend is constructed **directly** (Piper/Kokoro via
//! [`fono_tts::local_router::load_engine`], Supertonic via
//! [`fono_tts::supertonic::engine::SupertonicLocal::load`]), bypassing
//! `LocalRouter` so a measurement is always the intended backend rather than
//! whatever the language auto-router would have picked (it routes English to
//! Kokoro). Assets are ensured/verified against the voice catalog before
//! measuring.
//!
//! Metrics per (backend, voice, sentence): cold-start model-load time, warm
//! synthesis p50/p95, real-time factor (RTF = synth ÷ audio duration), output
//! sample rate + audio duration + speech rate, and an optional STT round-trip
//! WER/CER anchor. Per backend: on-disk footprint and peak process RSS (via
//! `/proc/self/status` `VmHWM`; run one `--backends` value per process for
//! clean attribution). Every utterance is written to a **raw, un-normalized**
//! WAV (native volume preserved on purpose) plus a Markdown listening index.
//!
//! **Supertonic note:** the public `synthesize` path re-seeds its Gaussian
//! latent from the wall clock on every call, so its output is *not* byte-stable
//! across calls — the report records this rather than asserting determinism.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use fono_stt::traits::SpeechToText;
use fono_tts::supertonic::engine::SupertonicLocal;
use fono_tts::{TextToSpeech, TtsAudio};

use crate::wer::{char_error_rate, word_error_rate};

/// Sample rate every STT backend expects (matches the rest of the bench crate).
const STT_SAMPLE_RATE: u32 = 16_000;

// ───────────────────────────── Fixtures ─────────────────────────────

/// One difficult sentence to synthesize.
#[derive(Debug, Clone, Deserialize)]
pub struct Sentence {
    /// Stable id, unique across the file; used in the WAV filename.
    pub id: String,
    /// Base language code, `"en"` or `"ro"`.
    pub language: String,
    /// Grouping label for aggregated reporting (numbers/acronyms/…).
    pub category: String,
    /// The text to synthesize, verbatim (diacritics preserved).
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SentenceFile {
    #[serde(default)]
    suite_version: String,
    #[serde(default, rename = "sentence")]
    sentences: Vec<Sentence>,
}

/// Load and validate the TTS sentence fixtures from a TOML file.
pub fn load_sentences(path: &Path) -> Result<(String, Vec<Sentence>)> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read tts fixtures {}", path.display()))?;
    let parsed: SentenceFile =
        toml::from_str(&text).with_context(|| format!("parse tts fixtures {}", path.display()))?;
    if parsed.sentences.is_empty() {
        return Err(anyhow!("tts fixtures {} contain no [[sentence]] entries", path.display()));
    }
    let mut seen = BTreeSet::new();
    for s in &parsed.sentences {
        if !seen.insert(s.id.clone()) {
            return Err(anyhow!("duplicate sentence id {:?} in {}", s.id, path.display()));
        }
    }
    Ok((parsed.suite_version, parsed.sentences))
}

// ───────────────────────────── Config ─────────────────────────────

/// Everything the harness needs for one run.
pub struct TtsBenchConfig {
    /// Base language codes to run (`["en", "ro"]`). Empty ⇒ every language
    /// present in the fixtures.
    pub languages: Vec<String>,
    /// Backends to test (`piper`, `kokoro`, `supertonic`).
    pub backends: Vec<String>,
    /// Per-backend voice/speaker overrides (backend → list). Empty ⇒ defaults.
    /// Piper/Kokoro entries are catalog voice names; Supertonic entries are
    /// integer speaker ids.
    pub voice_overrides: BTreeMap<String, Vec<String>>,
    /// Voices cache directory (where the daemon downloads assets).
    pub voices_dir: PathBuf,
    /// Output directory for WAVs + the listening index.
    pub wav_dir: PathBuf,
    /// Timed synthesis repetitions per sentence (for p50/p95).
    pub iterations: usize,
    /// Discarded warm-up synths before timing.
    pub warmup: usize,
    /// Requested ORT intra-op thread count (informational; recorded in metadata).
    pub threads: usize,
    /// Seed recorded in metadata (Supertonic re-seeds per call regardless).
    pub seed: u32,
    /// Human-readable machine label for the report.
    pub machine_label: Option<String>,
    /// When true, missing assets are downloaded/verified via the catalog ensure
    /// path; when false, a missing asset is a hard error pointing at the daemon.
    pub download: bool,
    /// Supertonic flow-matching step override. `None` ⇒ the engine default (5).
    /// Ignored by Piper/Kokoro.
    pub num_steps: Option<i32>,
}

// ───────────────────────────── Report ─────────────────────────────

#[derive(Debug, Serialize)]
pub struct TtsBenchReport {
    pub schema_version: &'static str,
    pub suite_version: String,
    pub machine_label: Option<String>,
    pub build: BuildMeta,
    pub seed: u32,
    pub requested_threads: usize,
    pub warmup: usize,
    pub iterations: usize,
    pub languages: Vec<String>,
    /// Supertonic flow-matching steps used for this run (engine default when the
    /// run did not override it). Recorded so a num_steps sweep is self-describing.
    pub supertonic_num_steps: i32,
    /// Peak process RSS in KiB (`VmHWM`) at the end of the whole run. Clean
    /// per-backend attribution requires one `--backends` value per process.
    pub rss_peak_kib: Option<u64>,
    pub rss_baseline_kib: Option<u64>,
    pub notes: Vec<String>,
    pub backends: Vec<BackendResult>,
    pub wav_dir: String,
    pub listening_index: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BuildMeta {
    pub os: &'static str,
    pub arch: &'static str,
    pub detected_parallelism: usize,
}

#[derive(Debug, Serialize)]
pub struct BackendResult {
    pub backend: String,
    pub language: String,
    /// False when the backend cannot speak this language (Kokoro + Romanian).
    pub supported: bool,
    pub note: Option<String>,
    /// Sum of the distinct on-disk asset file sizes this backend uses, in bytes.
    pub disk_footprint_bytes: u64,
    /// Peak process RSS (`VmHWM`, KiB) sampled right after this backend
    /// finished. Only attributable to this backend alone when the process ran a
    /// single backend.
    pub rss_peak_kib: Option<u64>,
    pub voices: Vec<VoiceResult>,
}

#[derive(Debug, Serialize)]
pub struct VoiceResult {
    /// Catalog voice name, or `spk<N>` for a Supertonic speaker.
    pub voice: String,
    pub cold_start_ms: u64,
    pub sample_rate: u32,
    /// Median of per-utterance RTF (synth ÷ audio duration).
    pub rtf_median: f64,
    /// Median of per-utterance p50 synth times, in ms.
    pub synth_ms_median: u64,
    pub utterances: Vec<UtteranceResult>,
}

#[derive(Debug, Serialize)]
pub struct UtteranceResult {
    pub id: String,
    pub category: String,
    pub text_chars: usize,
    pub synth_ms_p50: u64,
    pub synth_ms_p95: u64,
    pub samples: usize,
    pub audio_secs: f64,
    pub rtf: f64,
    /// Characters of input text per second of audio (speech rate proxy).
    pub speech_rate_cps: f64,
    pub wav: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wer: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cer: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ───────────────────────────── Runner ─────────────────────────────

/// Run the whole benchmark and return the structured report.
pub async fn run_tts_bench(
    cfg: &TtsBenchConfig,
    stt: Option<Arc<dyn SpeechToText>>,
) -> Result<TtsBenchReport> {
    let (suite_version, sentences) = load_sentences(&sentences_path(cfg))?;
    let languages = resolve_languages(&cfg.languages, &sentences);
    std::fs::create_dir_all(&cfg.wav_dir)
        .with_context(|| format!("create wav dir {}", cfg.wav_dir.display()))?;

    let rss_baseline_kib = read_vmrss_kib();
    let mut notes = Vec::new();
    if cfg.backends.iter().any(|b| b == "supertonic") {
        notes.push(
            "Supertonic re-seeds its latent noise per call, so its PCM is not byte-stable across \
             runs; treat its timings as representative rather than reproducible."
                .to_string(),
        );
    }
    if cfg.backends.len() > 1 {
        notes.push(
            "Peak RSS is process-wide; for clean per-backend memory attribution run one \
             --backends value per invocation."
                .to_string(),
        );
    }

    let mut backends = Vec::new();
    for lang in &languages {
        for backend in &cfg.backends {
            let result = run_backend_language(cfg, backend, lang, &sentences, stt.as_deref()).await;
            match result {
                Ok(r) => backends.push(r),
                Err(e) => {
                    warn!("backend {backend} / {lang} failed: {e:#}");
                    backends.push(BackendResult {
                        backend: backend.clone(),
                        language: lang.clone(),
                        supported: true,
                        note: Some(format!("setup failed: {e:#}")),
                        disk_footprint_bytes: 0,
                        rss_peak_kib: read_vmhwm_kib(),
                        voices: Vec::new(),
                    });
                }
            }
        }
    }

    let listening_index = match write_listening_index(cfg, &sentences, &backends) {
        Ok(p) => Some(p),
        Err(e) => {
            warn!("could not write listening index: {e:#}");
            None
        }
    };

    Ok(TtsBenchReport {
        schema_version: "tts-bench-report-v1",
        suite_version,
        machine_label: cfg.machine_label.clone(),
        build: BuildMeta {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            detected_parallelism: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
        },
        seed: cfg.seed,
        requested_threads: cfg.threads,
        warmup: cfg.warmup,
        iterations: cfg.iterations.max(1),
        languages,
        supertonic_num_steps: cfg
            .num_steps
            .unwrap_or(fono_tts::supertonic::engine::DEFAULT_NUM_STEPS),
        rss_peak_kib: read_vmhwm_kib(),
        rss_baseline_kib,
        notes,
        backends,
        wav_dir: cfg.wav_dir.display().to_string(),
        listening_index,
    })
}

/// Benchmark one backend in one language across every matching sentence.
async fn run_backend_language(
    cfg: &TtsBenchConfig,
    backend: &str,
    lang: &str,
    sentences: &[Sentence],
    stt: Option<&dyn SpeechToText>,
) -> Result<BackendResult> {
    // Kokoro is English-only (ADR 0033); surface Romanian as informational.
    if backend == "kokoro" && lang != "en" {
        return Ok(BackendResult {
            backend: backend.to_string(),
            language: lang.to_string(),
            supported: false,
            note: Some(
                "Kokoro has no Romanian voice; compare Romanian on Piper vs Supertonic."
                    .to_string(),
            ),
            disk_footprint_bytes: 0,
            rss_peak_kib: read_vmhwm_kib(),
            voices: Vec::new(),
        });
    }

    let picks = resolve_voices(cfg, backend, lang)?;
    if picks.is_empty() {
        return Ok(BackendResult {
            backend: backend.to_string(),
            language: lang.to_string(),
            supported: true,
            note: Some(format!("no {backend} voice resolved for {lang}")),
            disk_footprint_bytes: 0,
            rss_peak_kib: read_vmhwm_kib(),
            voices: Vec::new(),
        });
    }

    // Ensure assets (download-on-miss when requested) and record the footprint.
    let footprint = ensure_backend_assets(cfg, backend, &picks).await?;

    let lang_sentences: Vec<&Sentence> = sentences.iter().filter(|s| s.language == lang).collect();

    let mut voices = Vec::new();
    for pick in &picks {
        match run_voice(cfg, backend, lang, pick, &lang_sentences, stt).await {
            Ok(v) => voices.push(v),
            Err(e) => warn!("{backend} voice {} / {lang} failed: {e:#}", pick.label),
        }
    }

    Ok(BackendResult {
        backend: backend.to_string(),
        language: lang.to_string(),
        supported: true,
        note: None,
        disk_footprint_bytes: footprint,
        rss_peak_kib: read_vmhwm_kib(),
        voices,
    })
}

/// A resolved voice to benchmark: its display label plus how to construct it.
struct VoicePick {
    /// Filename/report label (`en_US-amy-medium`, `spk0`, …).
    label: String,
    kind: VoiceKind,
}

enum VoiceKind {
    /// A catalog voice (Piper or Kokoro) — built via `load_engine`.
    Catalog(Box<fono_tts::voices::Voice>),
    /// A Supertonic speaker id.
    Supertonic(i64),
}

/// Benchmark a single voice across all sentences for the language.
async fn run_voice(
    cfg: &TtsBenchConfig,
    backend: &str,
    lang: &str,
    pick: &VoicePick,
    sentences: &[&Sentence],
    stt: Option<&dyn SpeechToText>,
) -> Result<VoiceResult> {
    // Cold-start: time the first engine construction.
    let cold_t = Instant::now();
    let engine = build_engine(&cfg.voices_dir, pick, cfg.num_steps)?;
    let cold_start_ms = cold_t.elapsed().as_millis() as u64;
    let sample_rate = engine.native_sample_rate();

    let mut utterances = Vec::new();
    let mut rtfs = Vec::new();
    let mut p50s = Vec::new();
    for s in sentences {
        let u = synth_one(cfg, backend, lang, pick, &engine, s, stt).await;
        if u.error.is_none() {
            rtfs.push(u.rtf);
            p50s.push(u.synth_ms_p50);
        }
        utterances.push(u);
    }

    Ok(VoiceResult {
        voice: pick.label.clone(),
        cold_start_ms,
        sample_rate,
        rtf_median: median_f64(&mut rtfs),
        synth_ms_median: median_u64(&mut p50s),
        utterances,
    })
}

/// Synthesize one sentence: warm up, time `iterations` synths, write the WAV,
/// and optionally transcribe it back for a WER/CER anchor.
async fn synth_one(
    cfg: &TtsBenchConfig,
    backend: &str,
    lang: &str,
    pick: &VoicePick,
    engine: &Arc<dyn TextToSpeech>,
    sentence: &Sentence,
    stt: Option<&dyn SpeechToText>,
) -> UtteranceResult {
    let mut base = UtteranceResult {
        id: sentence.id.clone(),
        category: sentence.category.clone(),
        text_chars: sentence.text.chars().count(),
        synth_ms_p50: 0,
        synth_ms_p95: 0,
        samples: 0,
        audio_secs: 0.0,
        rtf: 0.0,
        speech_rate_cps: 0.0,
        wav: None,
        transcript: None,
        wer: None,
        cer: None,
        error: None,
    };

    // Warm-up synths (discarded).
    for _ in 0..cfg.warmup {
        if let Err(e) = engine.synthesize(&sentence.text, None, Some(lang)).await {
            base.error = Some(format!("warmup synth failed: {e:#}"));
            return base;
        }
    }

    let mut times = Vec::new();
    let mut last_audio: Option<TtsAudio> = None;
    for _ in 0..cfg.iterations.max(1) {
        let t = Instant::now();
        match engine.synthesize(&sentence.text, None, Some(lang)).await {
            Ok(audio) => {
                times.push(t.elapsed().as_millis() as u64);
                last_audio = Some(audio);
            }
            Err(e) => {
                base.error = Some(format!("synth failed: {e:#}"));
                return base;
            }
        }
    }

    let Some(audio) = last_audio else {
        base.error = Some("no audio produced".to_string());
        return base;
    };
    if audio.pcm.is_empty() {
        base.error = Some("empty PCM (backend dropped the text)".to_string());
        return base;
    }

    times.sort_unstable();
    let p50 = percentile(&times, 50.0);
    let p95 = percentile(&times, 95.0);
    let audio_secs = audio.pcm.len() as f64 / f64::from(audio.sample_rate);
    let rtf = if audio_secs > 0.0 { (p50 as f64 / 1000.0) / audio_secs } else { 0.0 };
    let speech_rate = if audio_secs > 0.0 { base.text_chars as f64 / audio_secs } else { 0.0 };

    // Write the raw WAV (no normalization — native volume is a signal).
    let filename = format!("{lang}__{backend}__{}__{}.wav", pick.label, sentence.id);
    let wav_path = cfg.wav_dir.join(&filename);
    if let Err(e) = crate::wav::write(&wav_path, &audio.pcm, audio.sample_rate) {
        base.error = Some(format!("wav write failed: {e:#}"));
    } else {
        base.wav = Some(filename);
    }

    // Optional STT round-trip anchor.
    if let Some(stt) = stt {
        match transcribe_roundtrip(stt, &audio, lang).await {
            Ok(hyp) => {
                base.wer = Some(word_error_rate(&sentence.text, &hyp));
                base.cer = Some(char_error_rate(&sentence.text, &hyp));
                base.transcript = Some(hyp);
            }
            Err(e) => warn!("STT round-trip failed for {}: {e:#}", sentence.id),
        }
    }

    base.synth_ms_p50 = p50;
    base.synth_ms_p95 = p95;
    base.samples = audio.pcm.len();
    base.audio_secs = audio_secs;
    base.rtf = rtf;
    base.speech_rate_cps = speech_rate;
    info!(
        "{backend}/{lang}/{} [{}] p50={p50}ms rtf={rtf:.2} {:.2}s",
        pick.label, sentence.id, audio_secs
    );
    base
}

/// Construct the engine for a voice pick, bypassing `LocalRouter`.
fn build_engine(
    voices_dir: &Path,
    pick: &VoicePick,
    num_steps: Option<i32>,
) -> Result<Arc<dyn TextToSpeech>> {
    match &pick.kind {
        VoiceKind::Catalog(voice) => fono_tts::local_router::load_engine(voices_dir, voice),
        VoiceKind::Supertonic(sid) => {
            let pack_dir = fono_tts::supertonic::supertonic_dir(voices_dir);
            let mut engine = SupertonicLocal::load(&pack_dir, *sid).with_context(|| {
                format!(
                    "load Supertonic pack from {} (run the daemon once to download it)",
                    pack_dir.display()
                )
            })?;
            if let Some(steps) = num_steps {
                engine = engine.with_num_steps(steps);
            }
            Ok(Arc::new(engine))
        }
    }
}

/// Resample TTS PCM to 16 kHz and transcribe it through the STT backend.
async fn transcribe_roundtrip(
    stt: &dyn SpeechToText,
    audio: &TtsAudio,
    lang: &str,
) -> Result<String> {
    let samples = resample_linear(&audio.pcm, audio.sample_rate, STT_SAMPLE_RATE);
    let trans = stt.transcribe(&samples, STT_SAMPLE_RATE, Some(lang)).await?;
    Ok(trans.text)
}

// ─────────────────────── Voice resolution + assets ───────────────────────

/// Resolve which voices to benchmark for `(backend, lang)`, honoring overrides.
fn resolve_voices(cfg: &TtsBenchConfig, backend: &str, lang: &str) -> Result<Vec<VoicePick>> {
    let overrides = cfg.voice_overrides.get(backend);
    match backend {
        "supertonic" => {
            let sids: Vec<i64> = match overrides {
                Some(list) => list
                    .iter()
                    .map(|s| {
                        s.trim().parse::<i64>().map_err(|_| {
                            anyhow!("invalid Supertonic speaker id {s:?} (expected an integer)")
                        })
                    })
                    .collect::<Result<_>>()?,
                None => default_supertonic_speakers(lang),
            };
            Ok(sids
                .into_iter()
                .map(|sid| VoicePick {
                    label: format!("spk{sid}"),
                    kind: VoiceKind::Supertonic(sid),
                })
                .collect())
        }
        "piper" | "kokoro" => {
            let names: Vec<String> = match overrides {
                Some(list) => list.clone(),
                None => default_catalog_voices(backend, lang),
            };
            let mut picks = Vec::new();
            for name in names {
                let voice = fono_tts::voices::by_name(&name)?
                    .ok_or_else(|| anyhow!("voice {name:?} not in the catalog"))?;
                if voice.engine != backend {
                    warn!("voice {name:?} is a {} voice, not {backend}; skipping", voice.engine);
                    continue;
                }
                if voice.language != lang {
                    warn!("voice {name:?} speaks {}, not {lang}; skipping", voice.language);
                    continue;
                }
                picks.push(VoicePick { label: name, kind: VoiceKind::Catalog(Box::new(voice)) });
            }
            Ok(picks)
        }
        other => Err(anyhow!("unknown backend {other:?} (expected piper, kokoro, or supertonic)")),
    }
}

fn default_supertonic_speakers(lang: &str) -> Vec<i64> {
    // English has Piper + Kokoro alternatives, so two Supertonic speakers is
    // enough; Romanian leans on Supertonic for variety, so offer three.
    if lang == "en" {
        vec![0, 1]
    } else {
        vec![0, 1, 2]
    }
}

fn default_catalog_voices(backend: &str, lang: &str) -> Vec<String> {
    match (backend, lang) {
        ("piper", "en") => vec!["en_US-amy-medium".to_string()],
        ("piper", "ro") => vec!["ro_RO-mihai-medium".to_string()],
        ("kokoro", "en") => vec!["af_heart".to_string(), "am_michael".to_string()],
        _ => Vec::new(),
    }
}

/// Ensure each pick's assets are present and return the backend's distinct
/// on-disk footprint in bytes.
async fn ensure_backend_assets(
    cfg: &TtsBenchConfig,
    backend: &str,
    picks: &[VoicePick],
) -> Result<u64> {
    if backend == "supertonic" {
        if cfg.download {
            fono_tts::supertonic::ensure_pack(&cfg.voices_dir, None)
                .await
                .context("ensure Supertonic pack")?;
        }
        let dir = fono_tts::supertonic::supertonic_dir(&cfg.voices_dir);
        let mut files: BTreeSet<PathBuf> = BTreeSet::new();
        for a in fono_tts::supertonic::assets() {
            files.insert(dir.join(a.file));
        }
        return Ok(sum_file_sizes(&files, &cfg.voices_dir));
    }

    let mut files: BTreeSet<PathBuf> = BTreeSet::new();
    for pick in picks {
        if let VoiceKind::Catalog(voice) = &pick.kind {
            if cfg.download {
                fono_tts::voices::ensure_voice(voice, &cfg.voices_dir, None)
                    .await
                    .with_context(|| format!("ensure voice {}", voice.name))?;
            }
            files.insert(cfg.voices_dir.join(&voice.model.file));
            if let Some(c) = &voice.config {
                files.insert(cfg.voices_dir.join(&c.file));
            }
            if let Some(st) = &voice.style {
                files.insert(cfg.voices_dir.join(&st.file));
            }
        }
    }
    Ok(sum_file_sizes(&files, &cfg.voices_dir))
}

/// Sum sizes of the given files, erroring only softly (missing files count 0)
/// but warning so a not-downloaded asset is visible.
fn sum_file_sizes(files: &BTreeSet<PathBuf>, voices_dir: &Path) -> u64 {
    let mut total = 0;
    for f in files {
        match std::fs::metadata(f) {
            Ok(m) => total += m.len(),
            Err(_) => warn!(
                "asset {} not present under {}; run the daemon once (or pass --download) to fetch it",
                f.display(),
                voices_dir.display()
            ),
        }
    }
    total
}

// ───────────────────────────── Listening index ─────────────────────────────

/// Write a Markdown listening index grouping clips by sentence across backends.
fn write_listening_index(
    cfg: &TtsBenchConfig,
    sentences: &[Sentence],
    backends: &[BackendResult],
) -> Result<String> {
    let mut md = String::from("# TTS listening index\n\n");
    md.push_str("Play the clips for each sentence side by side to compare backends/voices. ");
    md.push_str("WAVs are raw (un-normalized): volume differences are intentional.\n\n");

    for s in sentences {
        // Skip sentences with no clips written for the run's languages.
        let rows: Vec<(&str, &str, &str)> = backends
            .iter()
            .filter(|b| b.language == s.language)
            .flat_map(|b| {
                b.voices.iter().flat_map(move |v| {
                    v.utterances
                        .iter()
                        .filter(|u| u.id == s.id)
                        .filter_map(|u| u.wav.as_deref())
                        .map(move |w| (b.backend.as_str(), v.voice.as_str(), w))
                })
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        md.push_str(&format!("## {} [{} / {}]\n\n", s.id, s.language, s.category));
        md.push_str(&format!("> {}\n\n", s.text));
        for (backend, voice, wav) in rows {
            md.push_str(&format!("- **{backend}** / {voice}: [{wav}](./{wav})\n"));
        }
        md.push('\n');
    }

    let path = cfg.wav_dir.join("index.md");
    std::fs::write(&path, md).with_context(|| format!("write {}", path.display()))?;
    Ok(path.display().to_string())
}

// ───────────────────────────── Helpers ─────────────────────────────

fn sentences_path(cfg: &TtsBenchConfig) -> PathBuf {
    let _ = cfg;
    default_tts_fixtures()
}

/// Default fixture path, resolved relative to the workspace root.
#[must_use]
pub fn default_tts_fixtures() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join("tests").join("fixtures").join("tts").join("sentences.toml"))
        .unwrap_or_else(|| PathBuf::from("tests/fixtures/tts/sentences.toml"))
}

fn resolve_languages(requested: &[String], sentences: &[Sentence]) -> Vec<String> {
    if !requested.is_empty() {
        return requested.iter().map(|s| s.to_ascii_lowercase()).collect();
    }
    let mut langs: Vec<String> = Vec::new();
    for s in sentences {
        if !langs.contains(&s.language) {
            langs.push(s.language.clone());
        }
    }
    langs
}

/// Linear-interpolation resampler. Adequate for a WER/CER anchor (not hi-fi).
fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = f64::from(to) / f64::from(from);
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    let last = input.len() - 1;
    for i in 0..out_len {
        let src = i as f64 / ratio;
        let idx = src.floor() as usize;
        let frac = (src - idx as f64) as f32;
        let a = input[idx.min(last)];
        let b = input[(idx + 1).min(last)];
        out.push(a + (b - a) * frac);
    }
    out
}

// ───────────────────── Transcribe-existing scoring ─────────────────────

/// One transcription of one saved WAV by one STT engine.
#[derive(Debug, Clone, Serialize)]
pub struct ScoreEntry {
    pub wav: String,
    pub language: String,
    pub backend: String,
    pub voice: String,
    pub sentence_id: String,
    pub category: String,
    pub reference: String,
    pub engine: String,
    pub transcript: String,
    pub wer: f32,
    pub cer: f32,
}

/// Mean WER/CER for one (backend, voice, engine) cell.
#[derive(Debug, Clone, Serialize)]
pub struct ScoreSummary {
    pub backend: String,
    pub language: String,
    pub voice: String,
    pub engine: String,
    pub n: usize,
    pub mean_wer: f32,
    pub mean_cer: f32,
}

/// Full report for a transcribe-existing scoring run.
#[derive(Debug, Clone, Serialize)]
pub struct ScoreReport {
    pub wav_dir: String,
    pub suite_version: String,
    pub engines: Vec<String>,
    pub summary: Vec<ScoreSummary>,
    pub entries: Vec<ScoreEntry>,
}

/// Parse `<lang>__<backend>__<voice>__<sentence-id>` from a WAV file stem.
fn parse_wav_stem(stem: &str) -> Option<(String, String, String, String)> {
    let parts: Vec<&str> = stem.splitn(4, "__").collect();
    if parts.len() != 4 {
        return None;
    }
    Some((parts[0].to_string(), parts[1].to_string(), parts[2].to_string(), parts[3].to_string()))
}

/// Transcribe every WAV already sitting in `wav_dir` with each supplied STT
/// engine and score it (WER + CER) against the fixture reference recovered from
/// the filename's sentence id. This is the faithful quality anchor: it scores
/// the *exact* clips a human listens to — no re-synthesis, so Supertonic's
/// per-call noise is not re-rolled — and hits every engine in a single pass.
///
/// `backends_filter`, when non-empty, restricts scoring to those backends.
pub async fn score_existing_wavs(
    wav_dir: &Path,
    engines: &[(String, Arc<dyn SpeechToText>)],
    backends_filter: &[String],
) -> Result<ScoreReport> {
    let (suite_version, sentences) = load_sentences(&default_tts_fixtures())?;
    let by_id: BTreeMap<String, Sentence> =
        sentences.into_iter().map(|s| (s.id.clone(), s)).collect();

    let mut wavs: Vec<PathBuf> = std::fs::read_dir(wav_dir)
        .with_context(|| format!("read wav dir {}", wav_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("wav"))
        .collect();
    wavs.sort();

    let mut entries: Vec<ScoreEntry> = Vec::new();
    for path in &wavs {
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        let Some((language, backend, voice, sentence_id)) = parse_wav_stem(stem) else {
            warn!("skip unparseable wav name {}", path.display());
            continue;
        };
        if !backends_filter.is_empty() && !backends_filter.contains(&backend) {
            continue;
        }
        let Some(sentence) = by_id.get(&sentence_id) else {
            warn!("no fixture for sentence id {sentence_id:?} ({})", path.display());
            continue;
        };
        let wav = crate::wav::read(path)?;
        let samples = resample_linear(&wav.samples, wav.sample_rate, STT_SAMPLE_RATE);
        for (engine_name, stt) in engines {
            let transcript = match stt.transcribe(&samples, STT_SAMPLE_RATE, Some(&language)).await
            {
                Ok(t) => t.text,
                Err(e) => {
                    warn!("{engine_name} failed on {}: {e:#}", path.display());
                    continue;
                }
            };
            let wer = word_error_rate(&sentence.text, &transcript);
            let cer = char_error_rate(&sentence.text, &transcript);
            info!(
                "{engine_name} {backend}/{language}/{voice} [{sentence_id}] wer={wer:.2} cer={cer:.2}"
            );
            entries.push(ScoreEntry {
                wav: path.file_name().and_then(|s| s.to_str()).unwrap_or_default().to_string(),
                language: language.clone(),
                backend: backend.clone(),
                voice: voice.clone(),
                sentence_id: sentence_id.clone(),
                category: sentence.category.clone(),
                reference: sentence.text.clone(),
                engine: engine_name.clone(),
                transcript,
                wer,
                cer,
            });
        }
    }

    // Aggregate mean WER/CER per (backend, voice, engine).
    let mut groups: BTreeMap<(String, String, String, String), Vec<&ScoreEntry>> = BTreeMap::new();
    for e in &entries {
        groups
            .entry((e.backend.clone(), e.language.clone(), e.voice.clone(), e.engine.clone()))
            .or_default()
            .push(e);
    }
    let summary = groups
        .into_iter()
        .map(|((backend, language, voice, engine), es)| {
            let n = es.len();
            let mean_wer = es.iter().map(|e| e.wer).sum::<f32>() / n as f32;
            let mean_cer = es.iter().map(|e| e.cer).sum::<f32>() / n as f32;
            ScoreSummary { backend, language, voice, engine, n, mean_wer, mean_cer }
        })
        .collect();

    Ok(ScoreReport {
        wav_dir: wav_dir.display().to_string(),
        suite_version,
        engines: engines.iter().map(|(n, _)| n.clone()).collect(),
        summary,
        entries,
    })
}

/// Percentile of an already-sorted slice (nearest-rank).
fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn median_u64(values: &mut [u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    values[values.len() / 2]
}

fn median_f64(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    values[values.len() / 2]
}

/// Read a `/proc/self/status` field ending in " kB" as KiB (Linux only).
fn read_proc_status_kib(field: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix(field) {
            return rest.trim().trim_end_matches("kB").trim().parse::<u64>().ok();
        }
    }
    None
}

fn read_vmhwm_kib() -> Option<u64> {
    read_proc_status_kib("VmHWM:")
}

fn read_vmrss_kib() -> Option<u64> {
    read_proc_status_kib("VmRSS:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_parse_and_ids_unique() {
        let (version, sentences) = load_sentences(&default_tts_fixtures()).expect("fixtures parse");
        assert!(version.starts_with("tts-sentences"));
        assert!(sentences.len() >= 10, "expected a decent difficult-sentence set");
        assert!(sentences.iter().any(|s| s.language == "en"));
        assert!(sentences.iter().any(|s| s.language == "ro"));
        // Mixed-language + diacritics categories must exist for the RO matrix.
        assert!(sentences.iter().any(|s| s.category == "mixed"));
        assert!(sentences.iter().any(|s| s.category == "diacritics"));
    }

    #[test]
    fn percentile_nearest_rank() {
        let v = vec![10, 20, 30, 40, 50];
        assert_eq!(percentile(&v, 50.0), 30);
        assert_eq!(percentile(&v, 0.0), 10);
        assert_eq!(percentile(&v, 100.0), 50);
        assert_eq!(percentile(&[], 50.0), 0);
    }

    #[test]
    fn resample_identity_and_downsample_length() {
        let sig = vec![0.0, 1.0, 0.0, -1.0, 0.0, 1.0];
        assert_eq!(resample_linear(&sig, 24_000, 24_000), sig);
        let down = resample_linear(&sig, 48_000, 16_000);
        assert_eq!(down.len(), 2, "half-third of 6 samples rounds to 2");
        assert!(resample_linear(&[], 24_000, 16_000).is_empty());
    }

    #[test]
    fn default_speakers_differ_by_language() {
        assert_eq!(default_supertonic_speakers("en"), vec![0, 1]);
        assert_eq!(default_supertonic_speakers("ro"), vec![0, 1, 2]);
    }

    #[test]
    fn default_catalog_voices_match_engine_policy() {
        assert_eq!(default_catalog_voices("piper", "ro"), vec!["ro_RO-mihai-medium"]);
        assert!(default_catalog_voices("kokoro", "en").contains(&"af_heart".to_string()));
        assert!(default_catalog_voices("kokoro", "ro").is_empty());
    }
}
