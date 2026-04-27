// SPDX-License-Identifier: GPL-3.0-only
//! `fono-bench` — CLI driver. Two subcommands:
//!
//! * `bench` — legacy latency + WER benchmark over the multilingual
//!   fixture set; emits a JSON `Report`.
//! * `equivalence` — streaming↔batch equivalence harness (plan v6 R18
//!   Slice A foundation).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use fono_bench::equivalence::{
    run_fixture, EquivalenceReport, Manifest, TIER1_LEVENSHTEIN_THRESHOLD,
};
use fono_bench::fakes::{FakeLlm, FakeStt};
use fono_bench::fixtures::Fixture;
use fono_bench::report::Report;
use fono_bench::runner::BenchRunner;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Provider {
    /// Fake STT that returns each fixture's reference transcript verbatim.
    /// Always available; latency reports orchestrator overhead, WER == 0.
    Fake,
    /// Groq-hosted whisper-large-v3-turbo. Requires `GROQ_API_KEY` and the
    /// `groq` feature.
    Groq,
    /// OpenAI whisper-1. Requires `OPENAI_API_KEY` and the `openai` feature.
    Openai,
    /// Local whisper.cpp via `whisper-rs`. Requires the `whisper-local`
    /// feature and a model in `~/.cache/fono/models/whisper/`.
    Local,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum LlmProvider {
    None,
    Fake,
    Cerebras,
    Groq,
    Openai,
}

#[derive(Debug, Parser)]
#[command(
    name = "fono-bench",
    version,
    about = "Latency + WER benchmark and streaming↔batch equivalence harness for Fono."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Latency + WER benchmark over the multilingual fixture set.
    Bench(BenchArgs),
    /// Streaming↔batch equivalence harness (plan v6 R18).
    Equivalence(EquivalenceArgs),
}

#[derive(Debug, Parser)]
struct BenchArgs {
    /// STT backend to benchmark.
    #[arg(long, value_enum, default_value_t = Provider::Fake)]
    provider: Provider,

    /// Optional LLM cleanup stage. `none` runs STT-only.
    #[arg(long, value_enum, default_value_t = LlmProvider::None)]
    llm: LlmProvider,

    /// STT model name (provider-specific). Default = provider's recommended.
    #[arg(long)]
    model: Option<String>,

    /// LLM model name (provider-specific).
    #[arg(long)]
    llm_model: Option<String>,

    /// Comma-separated list of language tags to run (`en,es,fr,de`).
    /// Empty = all fixtures.
    #[arg(long, default_value = "")]
    languages: String,

    /// Run each fixture this many times to stabilise p50/p95.
    #[arg(long, default_value_t = 1)]
    iterations: usize,

    /// Where to read/write the cached fixtures. Defaults to
    /// `${XDG_CACHE_HOME:-$HOME/.cache}/fono/bench/`.
    #[arg(long)]
    bench_dir: Option<PathBuf>,

    /// Optional baseline JSON to compare against; non-zero exit on
    /// regression beyond the configured thresholds.
    #[arg(long)]
    baseline: Option<PathBuf>,

    /// Max acceptable WER regression in percentage points (default 5pp).
    #[arg(long, default_value_t = 5.0)]
    wer_pp_max: f32,

    /// Max acceptable p95 latency regression in percent (default 15%).
    #[arg(long, default_value_t = 15.0)]
    latency_pct_max: f32,

    /// Refuse to run any fixture whose SHA-256 is unpinned (CI mode).
    #[arg(long)]
    strict: bool,

    /// Pretty-print the JSON report to stdout (default: yes).
    #[arg(long, default_value_t = true)]
    pretty: bool,

    /// Optional path to write the report. `--baseline` and `--out` are
    /// independent: `--out` writes the *current* run, `--baseline` reads
    /// a previous run for comparison.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Debug, Parser)]
struct EquivalenceArgs {
    /// Fixtures directory containing `manifest.toml` + the WAV files
    /// it references. Defaults to `tests/fixtures/equivalence/` under
    /// the workspace root.
    #[arg(long)]
    fixtures: Option<PathBuf>,

    /// STT backend. Slice A only supports `local` (whisper.cpp). Cloud
    /// streaming rows of R18 land in Slice B.
    #[arg(long, default_value = "local")]
    stt: String,

    /// Whisper model name when `--stt local`. Resolves to
    /// `<models-dir>/ggml-<name>.bin`.
    #[arg(long, default_value = "tiny.en")]
    model: String,

    /// Where to write the JSON report. When unset, the harness prints
    /// the human-readable table only.
    #[arg(long)]
    output: Option<PathBuf>,

    /// Skip fixtures whose `duration_estimate_s` exceeds 5.0 — fast
    /// smoke pass for local development.
    #[arg(long, default_value_t = false)]
    quick: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("FONO_BENCH_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    match cli.cmd {
        Cmd::Bench(a) => run_bench(a).await,
        Cmd::Equivalence(a) => run_equivalence(a).await,
    }
}

async fn run_bench(args: BenchArgs) -> Result<()> {
    let bench_root = args.bench_dir.clone().unwrap_or_else(default_bench_dir);
    info!("bench root: {}", bench_root.display());

    let stt = build_stt(args.provider, args.model.as_deref())?;
    let llm = build_llm(args.llm, args.llm_model.as_deref())?;

    let mut runner = BenchRunner::new(stt, &bench_root);
    if let Some(l) = llm {
        runner = runner.with_llm(l);
    }
    if args.strict {
        runner = runner.strict();
    }

    let langs: Vec<String> = if args.languages.is_empty() {
        Vec::new()
    } else {
        args.languages
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    info!(
        "running {} fixtures × {} iterations",
        if langs.is_empty() {
            fono_bench::fixtures::FIXTURES.len()
        } else {
            langs.iter().flat_map(|l| Fixture::by_language(l)).count()
        },
        args.iterations
    );

    let (report, errors) = runner.run_all(&langs, args.iterations).await?;
    for e in &errors {
        warn!("clip failed: {e}");
    }

    let payload = if args.pretty {
        serde_json::to_string_pretty(&report)?
    } else {
        serde_json::to_string(&report)?
    };
    if let Some(p) = &args.out {
        std::fs::write(p, &payload)?;
        info!("wrote report to {}", p.display());
    }
    println!("{payload}");

    if let Some(p) = args.baseline {
        let baseline_text = std::fs::read_to_string(&p)
            .with_context(|| format!("read baseline {}", p.display()))?;
        let baseline: Report = serde_json::from_str(&baseline_text)?;
        match report.check_regression(&baseline, args.wer_pp_max, args.latency_pct_max) {
            Ok(()) => info!("regression check: clean"),
            Err(issues) => {
                for i in &issues {
                    eprintln!("REGRESSION: {i}");
                }
                std::process::exit(2);
            }
        }
    }

    if !errors.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

async fn run_equivalence(args: EquivalenceArgs) -> Result<()> {
    let fixtures_dir = args
        .fixtures
        .clone()
        .unwrap_or_else(default_equivalence_dir);
    let manifest_path = fixtures_dir.join("manifest.toml");
    let manifest = Manifest::load(&manifest_path).with_context(|| {
        format!(
            "load equivalence manifest {} \
             (run from the workspace root or pass --fixtures)",
            manifest_path.display()
        )
    })?;
    info!(
        "equivalence: {} fixtures from {}",
        manifest.fixtures.len(),
        fixtures_dir.display()
    );

    let stt_backend_name;
    let stt: Arc<dyn fono_stt::SpeechToText> = match args.stt.as_str() {
        "local" => {
            #[cfg(feature = "whisper-local")]
            {
                let cache = default_models_dir();
                let path = cache.join(format!("ggml-{}.bin", args.model));
                if !path.exists() {
                    eprintln!(
                        "fono-bench equivalence: whisper model {} missing.\n\
                         Hint: run `fono models install {}` to fetch it.",
                        path.display(),
                        args.model
                    );
                    std::process::exit(2);
                }
                stt_backend_name = format!("local:{}", args.model);
                Arc::new(fono_stt::whisper_local::WhisperLocal::new(path))
            }
            #[cfg(not(feature = "whisper-local"))]
            {
                eprintln!(
                    "fono-bench equivalence: built without --features whisper-local.\n\
                     Rebuild with `cargo build -p fono-bench \
                     --features equivalence,whisper-local` and re-run."
                );
                std::process::exit(2);
            }
        }
        "fake" => {
            stt_backend_name = "fake".to_string();
            Arc::new(FakeStt::new("the quick brown fox jumps over the lazy dog"))
        }
        other => {
            return Err(anyhow!(
                "unsupported --stt {other:?}; Slice A supports `local` (or `fake` for harness shape testing). \
                 Cloud streaming rows of R18 land in Slice B."
            ));
        }
    };

    let streaming = build_streaming(&args.stt, &args.model)?;

    let mut report = EquivalenceReport {
        fono_version: env!("CARGO_PKG_VERSION").to_string(),
        stt_backend: stt_backend_name,
        tier: "tier1".to_string(),
        threshold_levenshtein: TIER1_LEVENSHTEIN_THRESHOLD,
        results: Vec::with_capacity(manifest.fixtures.len()),
        // R18.23: pin the v7 default boundary knobs into the report so
        // streaming runs are fully reproducible. The CLI does not yet
        // dispatch the four A2-* rows separately (deferred to Slice
        // B's Tier-2 wiring); this pin records the knob set used by
        // the gating row.
        pinned_params: Some(fono_bench::equivalence::BoundaryKnobs::defaults()),
    };

    let quick = if args.quick { Some(5.0_f32) } else { None };

    for fx in &manifest.fixtures {
        match run_fixture(fx, &fixtures_dir, Arc::clone(&stt), streaming.clone(), quick).await {
            Ok(r) => report.results.push(r),
            Err(e) => {
                warn!("fixture {} failed: {e:#}", fx.name);
                report.results.push(fono_bench::EquivalenceResult {
                    fixture: fx.name.clone(),
                    language: fx.language.clone(),
                    synthetic_placeholder: fx.synthetic_placeholder,
                    modes: fono_bench::Modes {
                        batch: fono_bench::ModeResult {
                            text: String::new(),
                            elapsed_ms: 0,
                            ttff_ms: 0,
                        },
                        streaming: None,
                    },
                    metrics: fono_bench::Metrics {
                        stt_levenshtein_norm: 0.0,
                        ttff_ratio: None,
                        ttc_ratio: None,
                    },
                    verdict: fono_bench::Verdict::Fail,
                    note: format!("error: {e:#}"),
                });
            }
        }
    }

    print_table(&report);

    if let Some(out) = args.output.as_ref() {
        let payload = serde_json::to_string_pretty(&report)?;
        std::fs::write(out, payload).with_context(|| format!("write {}", out.display()))?;
        info!("wrote report to {}", out.display());
    }

    match report.overall_verdict() {
        fono_bench::Verdict::Pass => Ok(()),
        fono_bench::Verdict::Skipped => {
            warn!("all fixtures skipped (no streaming pass available?)");
            Ok(())
        }
        fono_bench::Verdict::Fail => {
            std::process::exit(1);
        }
    }
}

fn print_table(report: &EquivalenceReport) {
    println!();
    println!(
        "fono-bench equivalence ({}, threshold ≤ {:.3})",
        report.tier, report.threshold_levenshtein
    );
    println!(
        "{:<24} {:>8} {:>10} {:>10} {:>8} {:<10} note",
        "fixture", "lev", "ttff_ms", "ttc_ms", "verdict", "lang"
    );
    for r in &report.results {
        let stream_ttff = r
            .modes
            .streaming
            .as_ref()
            .map(|m| m.ttff_ms.to_string())
            .unwrap_or_else(|| "-".to_string());
        let stream_ttc = r
            .modes
            .streaming
            .as_ref()
            .map(|m| m.elapsed_ms.to_string())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:<24} {:>8.4} {:>10} {:>10} {:>8} {:<10} {}",
            r.fixture,
            r.metrics.stt_levenshtein_norm,
            stream_ttff,
            stream_ttc,
            verdict_label(r.verdict),
            r.language,
            r.note
        );
    }
    println!(
        "overall: {}",
        verdict_label(report.overall_verdict())
    );
}

fn verdict_label(v: fono_bench::Verdict) -> &'static str {
    match v {
        fono_bench::Verdict::Pass => "PASS",
        fono_bench::Verdict::Fail => "FAIL",
        fono_bench::Verdict::Skipped => "SKIP",
    }
}

#[cfg(feature = "equivalence")]
fn build_streaming(
    stt: &str,
    model: &str,
) -> Result<Option<Arc<dyn fono_bench::equivalence::StreamingSttHandle>>> {
    if stt != "local" {
        return Ok(None);
    }
    #[cfg(feature = "whisper-local")]
    {
        let cache = default_models_dir();
        let path = cache.join(format!("ggml-{model}.bin"));
        if !path.exists() {
            return Ok(None);
        }
        let s: Arc<dyn fono_stt::StreamingStt> =
            Arc::new(fono_stt::whisper_local::WhisperLocal::new(path));
        Ok(Some(Arc::new(
            fono_bench::equivalence::WhisperStreamingHandle::new(s),
        )))
    }
    #[cfg(not(feature = "whisper-local"))]
    {
        let _ = model;
        Ok(None)
    }
}

#[cfg(not(feature = "equivalence"))]
fn build_streaming(
    _stt: &str,
    _model: &str,
) -> Result<Option<Arc<dyn fono_bench::equivalence::StreamingSttHandle>>> {
    eprintln!(
        "fono-bench equivalence: built without --features equivalence; \
         streaming pass will be skipped. Rebuild with \
         `cargo build -p fono-bench --features equivalence,whisper-local` for the full gate."
    );
    Ok(None)
}

fn build_stt(p: Provider, model: Option<&str>) -> Result<Arc<dyn fono_stt::SpeechToText>> {
    Ok(match p {
        Provider::Fake => {
            // Fake returns each fixture's reference verbatim; the runner
            // needs per-fixture canned text, so we build a router that
            // reads the reference based on the language tag passed
            // through `transcribe(_, _, lang)`. Simpler approach: bench
            // with a single fixed sentence — for the orchestrator-overhead
            // smoke test we don't care about WER. Real-fixture
            // `--provider fake` simply demonstrates the runner pipeline
            // works; it'll show WER ≈ 1.0, expected.
            Arc::new(FakeStt::new("fake transcription"))
        }
        Provider::Groq => {
            #[cfg(feature = "groq")]
            {
                let key = std::env::var("GROQ_API_KEY").context("GROQ_API_KEY not set")?;
                let model = model.unwrap_or("whisper-large-v3").to_string();
                Arc::new(fono_stt::groq::GroqStt::with_model(key, model))
            }
            #[cfg(not(feature = "groq"))]
            {
                let _ = model;
                return Err(anyhow!("compiled without --features groq"));
            }
        }
        Provider::Openai => {
            #[cfg(feature = "openai")]
            {
                let key = std::env::var("OPENAI_API_KEY").context("OPENAI_API_KEY not set")?;
                let _ = model;
                Arc::new(fono_stt::openai::OpenAiStt::new(key))
            }
            #[cfg(not(feature = "openai"))]
            {
                let _ = model;
                return Err(anyhow!("compiled without --features openai"));
            }
        }
        Provider::Local => {
            #[cfg(feature = "whisper-local")]
            {
                let cache = default_models_dir();
                let model_name = model.unwrap_or("small");
                let path = cache.join(format!("ggml-{model_name}.bin"));
                if !path.exists() {
                    return Err(anyhow!(
                        "whisper model {} missing — run `fono models install {model_name}`",
                        path.display()
                    ));
                }
                Arc::new(fono_stt::whisper_local::WhisperLocal::new(path))
            }
            #[cfg(not(feature = "whisper-local"))]
            {
                let _ = model;
                return Err(anyhow!("compiled without --features whisper-local"));
            }
        }
    })
}

fn build_llm(
    p: LlmProvider,
    model: Option<&str>,
) -> Result<Option<Arc<dyn fono_llm::traits::TextFormatter>>> {
    use fono_llm::openai_compat::OpenAiCompat;
    Ok(match p {
        LlmProvider::None => None,
        LlmProvider::Fake => Some(Arc::new(FakeLlm::new())),
        LlmProvider::Cerebras => {
            let key = std::env::var("CEREBRAS_API_KEY").context("CEREBRAS_API_KEY not set")?;
            let m = model.unwrap_or("llama3.1-70b").to_string();
            Some(Arc::new(OpenAiCompat::cerebras(key, m)))
        }
        LlmProvider::Groq => {
            let key = std::env::var("GROQ_API_KEY").context("GROQ_API_KEY not set")?;
            let m = model.unwrap_or("llama-3.3-70b-versatile").to_string();
            Some(Arc::new(OpenAiCompat::groq(key, m)))
        }
        LlmProvider::Openai => {
            let key = std::env::var("OPENAI_API_KEY").context("OPENAI_API_KEY not set")?;
            let m = model.unwrap_or("gpt-4o-mini").to_string();
            Some(Arc::new(OpenAiCompat::openai(key, m)))
        }
    })
}

fn default_bench_dir() -> PathBuf {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".cache"))
                .unwrap_or_else(|| PathBuf::from("/tmp"))
        });
    cache.join("fono").join("bench")
}

fn default_equivalence_dir() -> PathBuf {
    // Walk up from CARGO_MANIFEST_DIR (the fono-bench crate) to the
    // workspace root. CARGO_MANIFEST_DIR is set at build time by cargo.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join("tests").join("fixtures").join("equivalence"))
        .unwrap_or_else(|| PathBuf::from("tests/fixtures/equivalence"))
}

#[cfg(feature = "whisper-local")]
fn default_models_dir() -> PathBuf {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".cache"))
                .unwrap_or_else(|| PathBuf::from("/tmp"))
        });
    cache.join("fono").join("models").join("whisper")
}
