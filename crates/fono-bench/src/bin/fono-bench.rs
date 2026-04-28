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
use indicatif::{ProgressBar, ProgressStyle};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use fono_bench::capabilities::ModelCapabilities;
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

    /// Suppress the legend and color key below the results table.
    /// Useful when running multiple benchmarks back-to-back so the
    /// legend only appears once (printed by the caller script).
    #[arg(long, default_value_t = false)]
    no_legend: bool,

    /// When set together with `--output`, write the **deterministic
    /// subset** of the report (no absolute timings: `elapsed_ms`,
    /// `ttff_ms`, `duration_s` are stripped). Used to commit a
    /// per-PR comparison anchor at
    /// `docs/bench/baseline-comfortable-tiny-en.json`. CI compares
    /// fresh runs against the committed baseline by per-fixture
    /// verdict; the timings would flap on shared runners and aren't
    /// part of the contract. See `docs/bench/README.md`.
    #[arg(long, default_value_t = false)]
    baseline: bool,

    /// Sleep this many milliseconds between fixtures. Useful with
    /// `--stt groq` to stay comfortably under the free-tier
    /// rate-limit (30 req/min). 0 = no sleep. Default 250 ms when
    /// `--stt groq`, 0 otherwise.
    #[arg(long)]
    rate_limit_ms: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("FONO_BENCH_LOG").unwrap_or_else(|_e| {
                // Default: info for fono crates, warn for whisper.cpp/GGML
                // (which is extremely chatty at info level). Override with
                // FONO_BENCH_LOG=info to see everything.
                EnvFilter::new("info,whisper_rs=warn")
            }),
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
    let caps: ModelCapabilities;
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
                caps = ModelCapabilities::for_local_whisper(&args.model);
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
            // The fake STT is multilingual by construction (it returns a
            // canned string regardless of input language).
            caps = ModelCapabilities {
                english_only: false,
                model_label: "fake".to_string(),
            };
            Arc::new(FakeStt::new("the quick brown fox jumps over the lazy dog"))
        }
        "groq" => {
            #[cfg(feature = "groq")]
            {
                let key = match std::env::var("GROQ_API_KEY") {
                    Ok(k) if !k.is_empty() => k,
                    _ => {
                        eprintln!(
                            "fono-bench equivalence --stt groq: GROQ_API_KEY not set.\n\
                             Set it in your shell (`export GROQ_API_KEY=gsk_...`) or in CI \
                             (repo Settings → Secrets and variables → Actions)."
                        );
                        std::process::exit(2);
                    }
                };
                // Default model = whisper-large-v3-turbo (Groq's lowest-latency
                // multilingual Whisper). `--model` can override.
                let model = if args.model.is_empty() || args.model == "tiny.en" {
                    "whisper-large-v3-turbo".to_string()
                } else {
                    args.model.clone()
                };
                stt_backend_name = format!("groq:{model}");
                // Groq's Whisper is multilingual.
                caps = ModelCapabilities {
                    english_only: false,
                    model_label: model.clone(),
                };
                Arc::new(fono_stt::groq::GroqStt::with_model(key, model))
            }
            #[cfg(not(feature = "groq"))]
            {
                eprintln!(
                    "fono-bench equivalence: built without --features groq.\n\
                     Rebuild with `cargo build -p fono-bench --features equivalence,groq`."
                );
                std::process::exit(2);
            }
        }
        other => {
            return Err(anyhow!(
                "unsupported --stt {other:?}; supported: `local` (whisper.cpp), \
                 `groq` (cloud Whisper-large-v3-turbo via GROQ_API_KEY), \
                 `fake` (harness shape testing)."
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
        model_capabilities: Some(caps.clone()),
    };

    let quick = if args.quick { Some(5.0_f32) } else { None };

    // Inter-fixture pacing — defaults to 250 ms for cloud Groq, 0 for
    // anything else. Burns less than 3 s of wall time over our current
    // 10-fixture set and keeps us comfortably under Groq's 30 req/min.
    let default_rate = if args.stt == "groq" { 250 } else { 0 };
    let rate_limit_ms = args.rate_limit_ms.unwrap_or(default_rate);

    // Progress bar: 2 steps per fixture (batch pass + streaming pass).
    let total_steps = manifest.fixtures.len() as u64 * 2;
    let pb = ProgressBar::new(total_steps);
    pb.set_style(
        ProgressStyle::with_template(
            "  {spinner:.green} [{elapsed_precise}] {wide_bar:.cyan/blue} {pos}/{len} {msg}",
        )
        .expect("valid progress template")
        .progress_chars("█▓░"),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(200));

    for (i, fx) in manifest.fixtures.iter().enumerate() {
        let fixture_num = i + 1;
        let total = manifest.fixtures.len();

        if i > 0 && rate_limit_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(rate_limit_ms)).await;
        }

        pb.set_message(format!("{}/{} {} — batch", fixture_num, total, fx.name));
        pb.tick();

        match run_fixture(
            fx,
            &fixtures_dir,
            Arc::clone(&stt),
            streaming.clone(),
            &caps,
            quick,
        )
        .await
        {
            Ok(r) => {
                // run_fixture does batch + streaming internally; advance
                // by 2 unless the fixture was skipped (no streaming pass).
                let steps = if r.verdict == fono_bench::Verdict::Skipped {
                    1
                } else {
                    2
                };
                pb.inc(steps);
                report.results.push(r);
            }
            Err(e) => {
                pb.inc(2);
                let msg = format!("{e:#}");
                // Hard-fail on rate-limit: explanatory exit so a release
                // bumping into the cap gets a clear signal instead of
                // looping. Recovery is "merge later"; not the harness's
                // job to retry.
                if msg.contains("429") || msg.to_lowercase().contains("rate limit") {
                    eprintln!(
                        "fono-bench equivalence: hit Groq rate limit on fixture {}.\n\
                         Wait an hour and re-run, or push the tag with the \
                         `-no-cloud-gate` suffix to bypass.\n\
                         Underlying error: {msg}",
                        fx.name
                    );
                    std::process::exit(3);
                }
                warn!("fixture {} failed: {msg}", fx.name);
                report.results.push(fono_bench::EquivalenceResult {
                    fixture: fx.name.clone(),
                    language: fx.language.clone(),
                    synthetic_placeholder: fx.synthetic_placeholder,
                    duration_s: 0.0,
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
                        stt_accuracy_levenshtein: None,
                        ttff_ratio: None,
                        ttc_ratio: None,
                    },
                    verdict: fono_bench::Verdict::Fail,
                    skip_reason: None,
                    note: format!("error: {e:#}"),
                });
            }
        }
    }

    pb.finish_and_clear();

    print_table(&report, !args.no_legend);

    if let Some(out) = args.output.as_ref() {
        // Wave 2 Thread C: `--baseline` strips absolute timings so the
        // committed `docs/bench/baseline-comfortable-tiny-en.json`
        // anchor doesn't churn on every CI run. The verdict per
        // fixture and the structural metadata (model_capabilities,
        // pinned_params, skip_reason) stay.
        let payload = if args.baseline {
            serde_json::to_string_pretty(&fono_bench::baseline_subset(&report))?
        } else {
            serde_json::to_string_pretty(&report)?
        };
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

use std::io::IsTerminal;

fn print_table(report: &EquivalenceReport, legend: bool) {
    let color = std::io::stdout().is_terminal();
    let s = Style::new(color);

    println!();
    println!(
        "fono-bench equivalence ({}, threshold ≤ {:.3})",
        report.tier, report.threshold_levenshtein
    );
    println!();

    // Column widths must match the data formatting below exactly.
    // Each data field is pre-padded to its header column width so that
    // ANSI escape codes (invisible to alignment) don't shift columns.
    let hdr = format!(
        " {:<22} {:>6} {:>6} {:>8} {:>8} {:>8} {:>8} {:>10} {:>10} {:>7} {:<5} {}",
        "fixture",
        "lev",
        "acc",
        "audio_s",
        "batch_s",
        "stream_s",
        "ttff_s",
        "ttff_r",
        "ttc_r",
        "result",
        "lang",
        "note"
    );
    println!("{hdr}");
    println!("{}", "─".repeat(hdr.len()));

    for r in &report.results {
        let audio_s = if r.duration_s > 0.0 {
            format!("{:>8.1}", r.duration_s)
        } else {
            format!("{:>8}", "-")
        };
        let batch_s = format!("{:>8.1}", r.modes.batch.elapsed_ms as f64 / 1000.0);
        let stream_s = r
            .modes
            .streaming
            .as_ref()
            .map(|m| format!("{:>8.1}", m.elapsed_ms as f64 / 1000.0))
            .unwrap_or_else(|| format!("{:>8}", "-"));
        let ttff_s = r
            .modes
            .streaming
            .as_ref()
            .map(|m| format!("{:>8.1}", m.ttff_ms as f64 / 1000.0))
            .unwrap_or_else(|| format!("{:>8}", "-"));

        // Ratios with color: green = good, yellow = marginal, red = bad.
        // Visible width must be 10 to match the header column.
        let ttff_r = fmt_ratio(r.metrics.ttff_ratio, &s);
        let ttc_r = fmt_ratio(r.metrics.ttc_ratio, &s);

        // Color the lev value: green at 0, yellow approaching threshold, red at/above.
        // Visible width must be 6 to match the header column.
        let lev_str = fmt_lev(
            r.metrics.stt_levenshtein_norm,
            report.threshold_levenshtein,
            &s,
        );
        let acc_str = match r.metrics.stt_accuracy_levenshtein {
            Some(a) => fmt_lev(a, report.threshold_levenshtein, &s),
            None => format!("{:>6}", "-"),
        };

        // Color the verdict. Visible width must be 7 to match the header column.
        let verdict_str = fmt_verdict(r.verdict, &s);

        println!(
            " {:<22} {} {} {} {} {} {} {} {} {} {:<5} {}",
            r.fixture,
            lev_str,
            acc_str,
            audio_s,
            batch_s,
            stream_s,
            ttff_s,
            ttff_r,
            ttc_r,
            verdict_str,
            r.language,
            r.note
        );
    }

    println!("{}", "─".repeat(hdr.len()));
    println!(" overall: {}", fmt_verdict(report.overall_verdict(), &s));
    println!();
    if legend {
        println!("Legend:");
        println!("  audio_s    Duration of the audio clip (seconds)");
        println!("  batch_s    Batch transcription total time (seconds)");
        println!("  stream_s   Streaming transcription total time (seconds)");
        println!("  ttff_s     Time to first feedback from streaming (seconds)");
        println!(
            "  ttff_r     Streaming TTFF / batch TTC  (< 1.0 = streaming shows first word sooner)"
        );
        println!(
            "  ttc_r      Streaming TTC / batch TTC   (< 1.0 = streaming completes faster overall)"
        );
        println!("  lev        Stream↔batch Levenshtein (0.0 = streaming and batch agree)");
        println!(
            "  acc        Batch↔reference Levenshtein (0.0 = batch matches the canonical text)"
        );
        println!();
        println!("Color key:");
        println!(
            "  {} = good        {} = marginal / caution        {} = bad / over threshold",
            s.green("green"),
            s.yellow("yellow"),
            s.red("red"),
        );
    }
}

/// Format a ratio with color coding.
/// < 1.0 = green (streaming advantage), 1.0–2.0 = yellow, > 2.0 = red.
/// Visible width = 10 (matches header column).
fn fmt_ratio(ratio: Option<f32>, s: &Style) -> String {
    match ratio {
        None => format!("{:>10}", "-"),
        Some(v) => {
            let txt = format!("{v:>10.2}");
            if v <= 1.0 {
                s.green(&txt)
            } else if v <= 2.0 {
                s.yellow(&txt)
            } else {
                s.red(&txt)
            }
        }
    }
}

/// Format the Levenshtein value with color: 0.0 = green, approaching
/// threshold = yellow, at/above = red.
/// Visible width = 6 (matches header column).
fn fmt_lev(lev: f32, threshold: f32, s: &Style) -> String {
    let txt = format!("{lev:>6.4}");
    if lev <= 0.0 {
        s.green(&txt)
    } else if lev < threshold * 0.5 {
        s.yellow(&txt)
    } else {
        s.red(&txt)
    }
}

/// Format the verdict with color.
/// Visible width = 7 (matches header column).
fn fmt_verdict(v: fono_bench::Verdict, s: &Style) -> String {
    match v {
        fono_bench::Verdict::Pass => s.green("   PASS"),
        fono_bench::Verdict::Fail => s.red("   FAIL"),
        fono_bench::Verdict::Skipped => s.yellow("   SKIP"),
    }
}

// ---------------------------------------------------------------------
// Minimal ANSI styling helper. No external dependency; no-ops when
// colors are disabled (piped output, CI, etc.).
// ---------------------------------------------------------------------

struct Style {
    enabled: bool,
}

impl Style {
    fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    fn green(&self, text: &str) -> String {
        if self.enabled {
            format!("\x1b[32m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    fn yellow(&self, text: &str) -> String {
        if self.enabled {
            format!("\x1b[33m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    fn red(&self, text: &str) -> String {
        if self.enabled {
            format!("\x1b[31m{text}\x1b[0m")
        } else {
            text.to_string()
        }
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
