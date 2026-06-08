// SPDX-License-Identifier: GPL-3.0-only
//! `fono-bench` — CLI driver. Two subcommands:
//!
//! * `bench` — legacy latency + WER benchmark over the multilingual
//!   fixture set; emits a JSON `Report`.
//! * `equivalence` — streaming↔batch equivalence harness (plan v6 R18
//!   Slice A foundation).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand, ValueEnum};
use futures::{stream::BoxStream, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use fono_bench::capabilities::ModelCapabilities;
use fono_bench::equivalence::{
    run_fixture, EquivalenceReport, Manifest, TIER1_LEVENSHTEIN_THRESHOLD,
};
use fono_bench::fakes::{FakePolish, FakeStt};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum LlmProvider {
    None,
    Fake,
    Cerebras,
    Groq,
    Openai,
    /// OpenAI-compatible Ollama endpoint (local or LAN-hosted).
    Ollama,
    Openrouter,
    Anthropic,
    /// Fono's embedded llama.cpp backend over a local GGUF model file.
    LlamaCpp,
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
    /// Text-only LLM polish benchmark over editable transcript fixtures.
    PolishText(PolishTextArgs),
    /// Factual-question assistant benchmark over editable text fixtures.
    AssistantFactual(AssistantFactualArgs),
    /// Replay an exact assistant prompt through embedded or server runtimes.
    AssistantReplay(AssistantReplayArgs),
    /// Benchmark an embedded llama.cpp cached prompt prefix with changing suffixes.
    AssistantPrefixCache(AssistantPrefixCacheArgs),
    /// Sweep cached-prefix savings across tool-count or window-context size.
    AssistantCacheScaling(AssistantCacheScalingArgs),
    /// Replay a growing multi-turn conversation to measure cached prefix reuse.
    AssistantConversationCache(AssistantConversationCacheArgs),
    /// Simulated Home Assistant light-control tool-use benchmark.
    AssistantToolUse(AssistantToolUseArgs),
    /// Extract the first captured assistant prompt from a trace JSON.
    ExtractTracePrompt(ExtractTracePromptArgs),
    /// Streaming↔batch equivalence harness (plan v6 R18).
    Equivalence(EquivalenceArgs),
}

#[derive(Debug, Parser)]
struct BenchArgs {
    /// STT backend to benchmark.
    #[arg(long, value_enum, default_value_t = Provider::Fake)]
    provider: Provider,

    /// Optional polish stage. `none` runs STT-only.
    #[arg(long, value_enum, default_value_t = LlmProvider::None)]
    polish: LlmProvider,

    /// STT model name (provider-specific). Default = provider's recommended.
    #[arg(long)]
    model: Option<String>,

    /// LLM model name (provider-specific).
    #[arg(long)]
    polish_model: Option<String>,

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
struct PolishTextArgs {
    /// Optional polish stage to benchmark. `ollama` accepts `--endpoint`; `llama-cpp` needs `--model-path`.
    #[arg(long, value_enum, default_value_t = LlmProvider::Fake)]
    provider: LlmProvider,

    /// LLM model name. For Ollama this is the exact pulled tag.
    #[arg(long)]
    model: Option<String>,

    /// Local GGUF model path when `--provider llama-cpp`.
    #[arg(long)]
    model_path: Option<PathBuf>,

    /// llama.cpp context size when `--provider llama-cpp`.
    #[arg(long, default_value_t = 2048)]
    ctx_size: u32,

    /// llama.cpp worker threads when `--provider llama-cpp`. Defaults to available parallelism.
    #[arg(long)]
    threads: Option<i32>,

    /// OpenAI-compatible chat/completions endpoint, e.g.
    /// `http://192.168.0.112:11434/v1/chat/completions`.
    #[arg(long)]
    endpoint: Option<String>,

    /// Editable fixture file. Defaults to tests/fixtures/polish_text/fixtures.toml.
    #[arg(long)]
    fixtures: Option<PathBuf>,

    /// Comma-separated list of language tags to run. Empty = all fixtures.
    #[arg(long, default_value = "")]
    languages: String,

    /// Run each fixture this many times to stabilize p50/p95.
    #[arg(long, default_value_t = 1)]
    iterations: usize,

    /// Human-readable machine label stored in the report, e.g. old-baseline-192.168.0.112.
    #[arg(long)]
    machine_label: Option<String>,

    /// Optional secrets file for cloud providers. Defaults to tests/secrets.toml if present.
    #[arg(long)]
    secrets: Option<PathBuf>,

    /// Pretty-print the JSON report to stdout.
    #[arg(long, default_value_t = true)]
    pretty: bool,

    /// Optional path to write the report.
    #[arg(long)]
    out: Option<PathBuf>,
}
#[derive(Debug, Parser)]
struct AssistantFactualArgs {
    /// Assistant provider to benchmark. `ollama` accepts `--endpoint`; `llama-cpp` needs `--model-path`.
    #[arg(long, value_enum, default_value_t = LlmProvider::Fake)]
    provider: LlmProvider,

    /// Assistant model name. For Ollama this is the exact pulled tag.
    #[arg(long)]
    model: Option<String>,

    /// Local GGUF model path when `--provider llama-cpp`.
    #[arg(long)]
    model_path: Option<PathBuf>,

    /// llama.cpp context size when `--provider llama-cpp`.
    #[arg(long, default_value_t = 2048)]
    ctx_size: u32,

    /// llama.cpp worker threads when `--provider llama-cpp`. Defaults to available parallelism.
    #[arg(long)]
    threads: Option<i32>,

    /// llama.cpp logical batch size when `--provider llama-cpp`. Defaults to ctx size.
    #[arg(long)]
    batch_size: Option<u32>,

    /// llama.cpp physical micro-batch size when `--provider llama-cpp`. Defaults to llama.cpp's internal default.
    #[arg(long)]
    ubatch_size: Option<u32>,

    /// Override the assistant system prompt. Use `default` for Fono's F8 prompt, `empty` for no prompt, or literal text.
    #[arg(long)]
    system_prompt: Option<String>,

    /// Add this many synthetic prior chat turns to measure history/prefill cost.
    #[arg(long, default_value_t = 0)]
    history_turns: usize,

    /// OpenAI-compatible chat/completions endpoint, e.g.
    /// `http://192.168.0.112:11434/v1/chat/completions`.
    #[arg(long)]
    endpoint: Option<String>,

    /// Editable fixture file. Defaults to tests/fixtures/assistant_factual/fixtures.toml.
    #[arg(long)]
    fixtures: Option<PathBuf>,

    /// Comma-separated list of language tags to run. Empty = all fixtures.
    #[arg(long, default_value = "")]
    languages: String,

    /// Run each fixture this many times to stabilize p50/p95.
    #[arg(long, default_value_t = 1)]
    iterations: usize,

    /// Human-readable machine label stored in the report.
    #[arg(long)]
    machine_label: Option<String>,

    /// Optional secrets file for cloud providers. Defaults to tests/secrets.toml if present.
    #[arg(long)]
    secrets: Option<PathBuf>,

    /// Pretty-print the JSON report to stdout.
    #[arg(long, default_value_t = true)]
    pretty: bool,

    /// Optional path to write the report.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Debug, Parser)]
struct AssistantReplayArgs {
    /// Assistant provider to replay against. `llama-cpp` uses embedded GGUF; `ollama` uses raw OpenAI-compatible completions.
    #[arg(long, value_enum, default_value_t = LlmProvider::LlamaCpp)]
    provider: LlmProvider,

    /// Assistant model name for HTTP/server replay.
    #[arg(long)]
    model: Option<String>,

    /// Local GGUF model path for embedded llama.cpp replay.
    #[arg(long)]
    model_path: Option<PathBuf>,

    /// Raw prompt text file, usually captured from an assistant trace.
    #[arg(long, conflicts_with = "trace_file")]
    prompt_file: Option<PathBuf>,

    /// Assistant trace JSON to extract the first event with an `args.prompt` string from.
    #[arg(long, conflicts_with = "prompt_file")]
    trace_file: Option<PathBuf>,

    /// Optional path to write the extracted/read prompt before replaying it.
    #[arg(long)]
    prompt_out: Option<PathBuf>,

    /// OpenAI-compatible chat/completions endpoint, e.g. `http://127.0.0.1:18131/v1/chat/completions`.
    #[arg(long)]
    endpoint: Option<String>,

    /// llama.cpp context size.
    #[arg(long, default_value_t = 8192)]
    ctx_size: u32,

    /// llama.cpp worker threads. Defaults to available parallelism.
    #[arg(long)]
    threads: Option<i32>,

    /// llama.cpp logical batch size. Defaults to ctx size.
    #[arg(long)]
    batch_size: Option<u32>,

    /// llama.cpp physical micro-batch size. Defaults to llama.cpp's internal default.
    #[arg(long)]
    ubatch_size: Option<u32>,

    /// Run the same prompt this many times.
    #[arg(long, default_value_t = 1)]
    iterations: usize,

    /// For embedded replay, prefill once, snapshot llama.cpp state, then restore it before each generation.
    #[arg(long)]
    state_cache: bool,

    /// Human-readable machine label stored in the report.
    #[arg(long)]
    machine_label: Option<String>,

    /// Optional OpenAI-compatible API key environment variable override for HTTP replay.
    #[arg(long)]
    api_key_env: Option<String>,

    /// Pretty-print the JSON report to stdout.
    #[arg(long, default_value_t = true)]
    pretty: bool,

    /// Optional path to write the report.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Debug, Parser)]
struct AssistantPrefixCacheArgs {
    /// Local GGUF model path for embedded llama.cpp replay.
    #[arg(long)]
    model_path: PathBuf,

    /// Raw stable prompt prefix file.
    #[arg(long)]
    prefix_file: PathBuf,

    /// Variable prompt suffix. Pass multiple times to model changing user requests.
    #[arg(long, required = true)]
    suffix: Vec<String>,

    /// llama.cpp context size.
    #[arg(long, default_value_t = 2048)]
    ctx_size: u32,

    /// llama.cpp worker threads. Defaults to available parallelism.
    #[arg(long)]
    threads: Option<i32>,

    /// llama.cpp logical batch size. Defaults to ctx size.
    #[arg(long)]
    batch_size: Option<u32>,

    /// llama.cpp physical micro-batch size. Defaults to llama.cpp's internal default.
    #[arg(long)]
    ubatch_size: Option<u32>,

    /// Run the suffix set this many times.
    #[arg(long, default_value_t = 1)]
    iterations: usize,

    /// Human-readable machine label stored in the report.
    #[arg(long)]
    machine_label: Option<String>,

    /// Pretty-print the JSON report to stdout.
    #[arg(long, default_value_t = true)]
    pretty: bool,

    /// Optional path to write the report.
    #[arg(long)]
    out: Option<PathBuf>,
}

/// Scaling dimension for the `assistant-cache-scaling` benchmark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CacheScalingDimension {
    /// Vary the number of tool/function descriptors in the cached prefix
    /// (plan task 14).
    Tools,
    /// Vary the size of the active-window context block in the cached prefix
    /// (plan task 15).
    Window,
}

#[cfg(feature = "llama-local")]
impl CacheScalingDimension {
    fn as_str(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::Window => "window",
        }
    }
}

#[derive(Debug, Parser)]
struct AssistantCacheScalingArgs {
    /// Local GGUF model path for embedded llama.cpp replay.
    #[arg(long)]
    model_path: PathBuf,

    /// Which prefix dimension to scale: tool count or window-context size.
    #[arg(long, value_enum)]
    dimension: CacheScalingDimension,

    /// Comma-separated sizes to sweep. For `tools` this is the tool count
    /// (e.g. 0,5,10,20,40); for `window` it is the number of synthetic
    /// window-context lines (e.g. 0,8,32,96).
    #[arg(long, value_delimiter = ',', required = true)]
    sizes: Vec<usize>,

    /// Variable user-request suffix. Pass multiple times to model changing
    /// requests against the same cached prefix.
    #[arg(long, required = true)]
    suffix: Vec<String>,

    /// llama.cpp context size.
    #[arg(long, default_value_t = 4096)]
    ctx_size: u32,

    /// llama.cpp worker threads. Defaults to available parallelism.
    #[arg(long)]
    threads: Option<i32>,

    /// llama.cpp logical batch size. Defaults to ctx size.
    #[arg(long)]
    batch_size: Option<u32>,

    /// llama.cpp physical micro-batch size. Defaults to llama.cpp's internal default.
    #[arg(long)]
    ubatch_size: Option<u32>,

    /// Run the suffix set this many times per size.
    #[arg(long, default_value_t = 1)]
    iterations: usize,

    /// Human-readable machine label stored in the report.
    #[arg(long)]
    machine_label: Option<String>,

    /// Pretty-print the JSON report to stdout.
    #[arg(long, default_value_t = true)]
    pretty: bool,

    /// Optional path to write the report.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Debug, Parser)]
struct AssistantConversationCacheArgs {
    /// Local GGUF model path for embedded llama.cpp replay.
    #[arg(long)]
    model_path: PathBuf,

    /// System prompt placed at the head of the conversation (cached prefix).
    #[arg(
        long,
        default_value = "You are Fono, a local voice assistant. Answer concisely and call a tool only when needed."
    )]
    system_prompt: String,

    /// User turn text. Pass multiple times; the conversation grows by one
    /// exchange per turn (turn N caches system + N-1 prior exchanges).
    #[arg(long = "turn", required = true)]
    turns: Vec<String>,

    /// Canned assistant reply appended to history after each turn so the cached
    /// prefix grows realistically. Length affects per-turn prefix growth.
    #[arg(
        long,
        default_value = "Okay, I've taken care of that for you. Let me know if you need anything else."
    )]
    assistant_reply: String,

    /// llama.cpp context size.
    #[arg(long, default_value_t = 4096)]
    ctx_size: u32,

    /// llama.cpp worker threads. Defaults to available parallelism.
    #[arg(long)]
    threads: Option<i32>,

    /// llama.cpp logical batch size. Defaults to ctx size.
    #[arg(long)]
    batch_size: Option<u32>,

    /// llama.cpp physical micro-batch size. Defaults to llama.cpp's internal default.
    #[arg(long)]
    ubatch_size: Option<u32>,

    /// Run each turn this many times.
    #[arg(long, default_value_t = 1)]
    iterations: usize,

    /// Human-readable machine label stored in the report.
    #[arg(long)]
    machine_label: Option<String>,

    /// Pretty-print the JSON report to stdout.
    #[arg(long, default_value_t = true)]
    pretty: bool,

    /// Optional path to write the report.
    #[arg(long)]
    out: Option<PathBuf>,
}
#[derive(Debug, Parser)]
struct ExtractTracePromptArgs {
    /// Assistant Chrome Trace / Perfetto JSON file.
    #[arg(long)]
    trace: PathBuf,

    /// Optional output path. When omitted, writes the prompt to stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Debug, Parser)]
struct AssistantToolUseArgs {
    /// Assistant provider to benchmark. `ollama` accepts `--endpoint`; `fake` validates harness shape.
    #[arg(long, value_enum, default_value_t = LlmProvider::Fake)]
    provider: LlmProvider,

    /// Assistant model name. For Ollama this is the exact pulled tag.
    #[arg(long)]
    model: Option<String>,

    /// OpenAI-compatible chat/completions endpoint, e.g.
    /// `http://192.168.0.112:11434/v1/chat/completions`.
    #[arg(long)]
    endpoint: Option<String>,

    /// Editable fixture file. Defaults to tests/fixtures/assistant_tool_use/homeassistant_lights.toml.
    #[arg(long)]
    fixtures: Option<PathBuf>,

    /// Comma-separated list of language tags to run. Empty = all fixtures.
    #[arg(long, default_value = "")]
    languages: String,

    /// Run each fixture this many times to stabilize p50/p95.
    #[arg(long, default_value_t = 1)]
    iterations: usize,

    /// Human-readable machine label stored in the report.
    #[arg(long)]
    machine_label: Option<String>,

    /// Optional secrets file for cloud providers. Defaults to tests/secrets.toml if present.
    #[arg(long)]
    secrets: Option<PathBuf>,

    /// Pretty-print the JSON report to stdout.
    #[arg(long, default_value_t = true)]
    pretty: bool,

    /// Optional path to write the report.
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
    // `Targets` directive syntax — same `target=level,default_level`
    // shape as `EnvFilter`'s level-only subset (no spans / regex /
    // field filters). See `crates/fono/src/main.rs::init_tracing` for
    // the rationale behind avoiding `env-filter`'s ~1 MiB regex
    // engine dependency.
    let directives = std::env::var("FONO_BENCH_LOG")
        .ok()
        .filter(|s| !s.trim().is_empty())
        // Default: info for fono crates, warn for whisper.cpp/GGML
        // (which is extremely chatty at info level). Override with
        // FONO_BENCH_LOG=info to see everything.
        .unwrap_or_else(|| "info,whisper_rs=warn".to_string());
    let targets: Targets =
        directives.parse().unwrap_or_else(|_| Targets::new().with_default(LevelFilter::INFO));
    tracing_subscriber::registry().with(tracing_subscriber::fmt::layer()).with(targets).init();
    match cli.cmd {
        Cmd::Bench(a) => run_bench(a).await,
        Cmd::PolishText(a) => run_polish_text_cmd(a).await,
        Cmd::AssistantFactual(a) => run_assistant_factual_cmd(a).await,
        Cmd::AssistantReplay(a) => run_assistant_replay_cmd(a).await,
        Cmd::AssistantPrefixCache(a) => run_assistant_prefix_cache_cmd(a).await,
        Cmd::AssistantCacheScaling(a) => run_assistant_cache_scaling_cmd(a).await,
        Cmd::AssistantConversationCache(a) => run_assistant_conversation_cache_cmd(a).await,
        Cmd::AssistantToolUse(a) => run_assistant_tool_use_cmd(a).await,
        Cmd::ExtractTracePrompt(a) => run_extract_trace_prompt_cmd(a).await,
        Cmd::Equivalence(a) => run_equivalence(a).await,
    }
}

async fn run_bench(args: BenchArgs) -> Result<()> {
    let bench_root = args.bench_dir.clone().unwrap_or_else(default_bench_dir);
    info!("bench root: {}", bench_root.display());

    let stt = build_stt(args.provider, args.model.as_deref())?;
    let polish = build_polish(args.polish, args.polish_model.as_deref())?;

    let mut runner = BenchRunner::new(stt, &bench_root);
    if let Some(l) = polish {
        runner = runner.with_llm(l);
    }
    if args.strict {
        runner = runner.strict();
    }

    let langs: Vec<String> = parse_languages(&args.languages);

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

async fn run_polish_text_cmd(args: PolishTextArgs) -> Result<()> {
    let manifest_path = args.fixtures.clone().unwrap_or_else(default_polish_text_fixtures);
    let manifest = fono_bench::load_polish_text_manifest(&manifest_path)?;
    let model = args
        .model
        .clone()
        .unwrap_or_else(|| default_polish_model(args.provider, args.model_path.as_deref()));
    let endpoint = resolve_polish_endpoint(args.provider, args.endpoint.as_deref());
    let polish = build_polish_with_options(
        args.provider,
        Some(&model),
        endpoint.as_deref(),
        args.secrets.as_deref(),
        args.model_path.as_deref(),
        args.ctx_size,
        args.threads,
    )?
    .ok_or_else(|| anyhow!("polish-text requires a real or fake polish provider, not `none`"))?;
    let languages = parse_languages(&args.languages);
    let cfg = fono_bench::polish_text::PolishTextRunConfig {
        provider: format!("{:?}", args.provider).to_ascii_lowercase(),
        model,
        endpoint: endpoint.clone(),
        runtime: polish_runtime_metadata(
            args.provider,
            args.model_path.as_deref(),
            args.ctx_size,
            args.threads,
        ),
        machine_label: args.machine_label.clone(),
        iterations: args.iterations,
        languages,
    };
    let report =
        fono_bench::polish_text::run_polish_text(&manifest_path, &manifest, polish, cfg).await?;
    let payload = if args.pretty {
        serde_json::to_string_pretty(&report)?
    } else {
        serde_json::to_string(&report)?
    };
    if let Some(p) = &args.out {
        std::fs::write(p, &payload)?;
        info!("wrote polish-text report to {}", p.display());
    }
    println!("{payload}");
    Ok(())
}

struct LlamaRuntimeOptions<'a> {
    model_path: Option<&'a std::path::Path>,
    ctx_size: u32,
    threads: Option<i32>,
    batch_size: Option<u32>,
    ubatch_size: Option<u32>,
}

struct AssistantBuildOptions<'a> {
    model: Option<&'a str>,
    endpoint: Option<&'a str>,
    secrets_path: Option<&'a std::path::Path>,
    llama: LlamaRuntimeOptions<'a>,
}

struct AssistantRuntimeMetadataOptions<'a> {
    provider: LlmProvider,
    llama: LlamaRuntimeOptions<'a>,
    system_prompt: Option<&'a str>,
    history_turns: usize,
}

async fn run_assistant_factual_cmd(args: AssistantFactualArgs) -> Result<()> {
    let manifest_path = args.fixtures.clone().unwrap_or_else(default_assistant_factual_fixtures);
    let manifest = fono_bench::load_assistant_factual_manifest(&manifest_path)?;
    let model = args.model.clone().unwrap_or_else(|| default_assistant_model(args.provider));
    let endpoint = resolve_polish_endpoint(args.provider, args.endpoint.as_deref());
    let assistant = build_assistant_with_options(
        args.provider,
        AssistantBuildOptions {
            model: Some(&model),
            endpoint: endpoint.as_deref(),
            secrets_path: args.secrets.as_deref(),
            llama: LlamaRuntimeOptions {
                model_path: args.model_path.as_deref(),
                ctx_size: args.ctx_size,
                threads: args.threads,
                batch_size: args.batch_size,
                ubatch_size: args.ubatch_size,
            },
        },
    )?;
    let languages = parse_languages(&args.languages);
    let cfg = fono_bench::assistant_factual::AssistantFactualRunConfig {
        provider: format!("{:?}", args.provider).to_ascii_lowercase(),
        model,
        endpoint: endpoint.clone(),
        machine_label: args.machine_label.clone(),
        iterations: args.iterations,
        languages,
        system_prompt_override: args.system_prompt.as_deref().map(resolve_assistant_system_prompt),
        history_turns: args.history_turns,
        runtime: assistant_runtime_metadata(AssistantRuntimeMetadataOptions {
            provider: args.provider,
            llama: LlamaRuntimeOptions {
                model_path: args.model_path.as_deref(),
                ctx_size: args.ctx_size,
                threads: args.threads,
                batch_size: args.batch_size,
                ubatch_size: args.ubatch_size,
            },
            system_prompt: args.system_prompt.as_deref(),
            history_turns: args.history_turns,
        }),
    };
    let report = fono_bench::assistant_factual::run_assistant_factual(
        &manifest_path,
        &manifest,
        assistant,
        cfg,
    )
    .await?;
    let payload = if args.pretty {
        serde_json::to_string_pretty(&report)?
    } else {
        serde_json::to_string(&report)?
    };
    if let Some(p) = &args.out {
        std::fs::write(p, &payload)?;
        info!("wrote assistant-factual report to {}", p.display());
    }
    println!("{payload}");
    Ok(())
}

async fn run_assistant_replay_cmd(args: AssistantReplayArgs) -> Result<()> {
    if args.provider == LlmProvider::None || args.provider == LlmProvider::Fake {
        return Err(anyhow!("assistant-replay supports `llama-cpp` and HTTP chat providers"));
    }

    let (prompt, prompt_source) = load_replay_prompt(
        args.prompt_file.as_deref(),
        args.trace_file.as_deref(),
        args.prompt_out.as_deref(),
    )?;
    let model = replay_model(&args)?;
    let provider = provider_label(args.provider);
    let endpoint = replay_endpoint(args.provider, args.endpoint.as_deref());
    let mut runtime = assistant_runtime_metadata(AssistantRuntimeMetadataOptions {
        provider: args.provider,
        llama: LlamaRuntimeOptions {
            model_path: args.model_path.as_deref(),
            ctx_size: args.ctx_size,
            threads: args.threads,
            batch_size: args.batch_size,
            ubatch_size: args.ubatch_size,
        },
        system_prompt: Some("raw-prompt"),
        history_turns: 0,
    });
    runtime.insert("prompt_replay".to_string(), "raw".to_string());
    if let Some(endpoint) = endpoint.as_deref() {
        runtime.insert("endpoint".to_string(), endpoint.to_string());
    }

    if args.state_cache && args.provider == LlmProvider::LlamaCpp {
        runtime.insert("state_cache".to_string(), "true".to_string());
    }

    let runs = if args.provider == LlmProvider::LlamaCpp {
        run_embedded_replay(&args, &prompt).await?
    } else {
        let endpoint_ref = endpoint
            .as_deref()
            .ok_or_else(|| anyhow!("--provider {provider} requires --endpoint"))?;
        run_http_replay(&args, endpoint_ref, &model, &prompt).await?
    };

    let report = ReplayReport {
        schema_version: "assistant-replay-report-v1",
        provider,
        model,
        endpoint,
        prompt_source,
        prompt_chars: prompt.chars().count(),
        prompt_sha256: sha256_text(&prompt),
        runtime,
        machine_label: args.machine_label.clone(),
        iterations: args.iterations.max(1),
        runs,
    };
    let payload = if args.pretty {
        serde_json::to_string_pretty(&report)?
    } else {
        serde_json::to_string(&report)?
    };
    if let Some(p) = &args.out {
        std::fs::write(p, &payload)?;
        info!("wrote assistant-replay report to {}", p.display());
    }
    println!("{payload}");
    Ok(())
}

#[cfg(feature = "llama-local")]
#[derive(Serialize)]
struct PrefixCacheReport {
    schema_version: &'static str,
    provider: String,
    model: String,
    prefix_source: String,
    prefix_chars: usize,
    prefix_sha256: String,
    suffix_count: usize,
    runtime: BTreeMap<String, String>,
    machine_label: Option<String>,
    iterations: usize,
    cache_key: String,
    prefix_tokens: usize,
    state_bytes: usize,
    setup_prefill_ms: u64,
    runs: Vec<PrefixCacheRun>,
}

#[cfg(feature = "llama-local")]
#[derive(Serialize)]
struct PrefixCacheRun {
    iteration: usize,
    suffix_index: usize,
    suffix_chars: usize,
    suffix_tokens: usize,
    uncached_latency_ms: u64,
    cached_latency_ms: u64,
    cached_time_to_first_token_ms: Option<u64>,
    state_restore_ms: u64,
    suffix_prefill_ms: u64,
    cached_decode_elapsed_ms: u64,
    cached_delta_count: usize,
    uncached_output_chars: usize,
    cached_output_chars: usize,
    outputs_match: bool,
    uncached_output: String,
    cached_output: String,
}

async fn run_assistant_prefix_cache_cmd(args: AssistantPrefixCacheArgs) -> Result<()> {
    #[cfg(feature = "llama-local")]
    {
        use fono_assistant::llama_local::LlamaLocalAssistant;
        use fono_assistant::Assistant;

        if !args.model_path.exists() {
            return Err(anyhow!("llama.cpp GGUF model not found at {}", args.model_path.display()));
        }
        let prefix = std::fs::read_to_string(&args.prefix_file)
            .with_context(|| format!("read prefix file {}", args.prefix_file.display()))?;
        let assistant = match args.threads {
            Some(t) if t > 0 => LlamaLocalAssistant::with_runtime_options(
                &args.model_path,
                args.ctx_size,
                t,
                args.batch_size,
                args.ubatch_size,
            ),
            Some(t) => return Err(anyhow!("--threads must be positive, got {t}")),
            None => LlamaLocalAssistant::with_runtime_options(
                &args.model_path,
                args.ctx_size,
                std::thread::available_parallelism()
                    .map(|n| i32::try_from(n.get()).unwrap_or(4))
                    .unwrap_or(4),
                args.batch_size,
                args.ubatch_size,
            ),
        };
        assistant.prewarm().await?;
        let cache_report = assistant
            .replay_raw_prompt_prefix_cache(
                prefix.clone(),
                args.suffix.clone(),
                args.iterations.max(1),
            )
            .await?;
        let model =
            args.model_path.file_stem().and_then(|s| s.to_str()).unwrap_or("llama-cpp").to_string();
        let runtime = assistant_runtime_metadata(AssistantRuntimeMetadataOptions {
            provider: LlmProvider::LlamaCpp,
            llama: LlamaRuntimeOptions {
                model_path: Some(&args.model_path),
                ctx_size: args.ctx_size,
                threads: args.threads,
                batch_size: args.batch_size,
                ubatch_size: args.ubatch_size,
            },
            system_prompt: Some("prefix-cache"),
            history_turns: 0,
        });
        let report = PrefixCacheReport {
            schema_version: "assistant-prefix-cache-report-v1",
            provider: "llama-cpp".to_string(),
            model,
            prefix_source: args.prefix_file.display().to_string(),
            prefix_chars: prefix.chars().count(),
            prefix_sha256: sha256_text(&prefix),
            suffix_count: args.suffix.len(),
            runtime,
            machine_label: args.machine_label.clone(),
            iterations: args.iterations.max(1),
            cache_key: cache_report.cache_key,
            prefix_tokens: cache_report.prefix_tokens,
            state_bytes: cache_report.state_bytes,
            setup_prefill_ms: cache_report.setup_prefill_ms,
            runs: cache_report
                .runs
                .into_iter()
                .map(|run| PrefixCacheRun {
                    iteration: run.iteration,
                    suffix_index: run.suffix_index,
                    suffix_chars: run.suffix_chars,
                    suffix_tokens: run.suffix_tokens,
                    uncached_latency_ms: run.uncached_latency_ms,
                    cached_latency_ms: run.cached_latency_ms,
                    cached_time_to_first_token_ms: run.cached_time_to_first_token_ms,
                    state_restore_ms: run.state_restore_ms,
                    suffix_prefill_ms: run.suffix_prefill_ms,
                    cached_decode_elapsed_ms: run.cached_decode_elapsed_ms,
                    cached_delta_count: run.cached_delta_count,
                    uncached_output_chars: run.uncached_output_chars,
                    cached_output_chars: run.cached_output_chars,
                    outputs_match: run.outputs_match,
                    uncached_output: run.uncached_output,
                    cached_output: run.cached_output,
                })
                .collect(),
        };
        let payload = if args.pretty {
            serde_json::to_string_pretty(&report)?
        } else {
            serde_json::to_string(&report)?
        };
        if let Some(path) = &args.out {
            std::fs::write(path, &payload)
                .with_context(|| format!("write prefix-cache report {}", path.display()))?;
            info!("wrote assistant-prefix-cache report to {}", path.display());
        }
        println!("{payload}");
        Ok(())
    }
    #[cfg(not(feature = "llama-local"))]
    {
        let _ = args;
        Err(anyhow!(
            "compiled without --features llama-local; rebuild with `cargo run -p fono-bench --features llama-local -- assistant-prefix-cache ...`"
        ))
    }
}

#[cfg(feature = "llama-local")]
#[derive(Serialize)]
struct CacheScalingReport {
    schema_version: &'static str,
    provider: String,
    model: String,
    dimension: String,
    runtime: BTreeMap<String, String>,
    machine_label: Option<String>,
    iterations: usize,
    suffix_count: usize,
    points: Vec<CacheScalingPoint>,
}

#[cfg(feature = "llama-local")]
#[derive(Serialize)]
struct CacheScalingPoint {
    /// Tool count or window-context line count for this point.
    size: usize,
    prefix_chars: usize,
    prefix_tokens: usize,
    state_bytes: usize,
    setup_prefill_ms: u64,
    run_count: usize,
    outputs_match_count: usize,
    median_uncached_latency_ms: u64,
    median_cached_latency_ms: u64,
    median_cached_time_to_first_token_ms: Option<u64>,
    median_state_restore_ms: u64,
    median_suffix_prefill_ms: u64,
    /// `median_uncached / median_cached`, rounded to two decimals.
    cached_speedup_x: f64,
}

#[cfg(feature = "llama-local")]
fn median_u64(mut values: Vec<u64>) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    values[values.len() / 2]
}

/// Build a synthetic stable prefix whose size scales along one dimension. The
/// prefix always ends at `User request:` so the per-turn suffix (` <request>`)
/// begins on a stable token boundary, matching the assistant reply split.
#[cfg(feature = "llama-local")]
fn synthesize_scaling_prefix(dimension: CacheScalingDimension, size: usize) -> String {
    let mut s = String::from(
        "You are Fono, a local voice assistant. Answer concisely and call a tool only when needed.\n",
    );
    match dimension {
        CacheScalingDimension::Tools => {
            if size > 0 {
                s.push_str("\nAvailable tools:\n");
                for i in 1..=size {
                    s.push_str(&format!(
                        "\nTool: tool_{i}\nDescription: Perform operation {i} on a named target with optional parameters.\nParameters:\n  - target (string, required): the entity to act on.\n  - mode (string, optional): one of \"fast\", \"balanced\", \"thorough\".\n  - dry_run (boolean, optional): when true, only simulate the action.\n"
                    ));
                }
            }
        }
        CacheScalingDimension::Window => {
            if size > 0 {
                s.push_str(
                    "\nActive window context:\nWindow title: Project Workspace — Code Editor\n",
                );
                for i in 1..=size {
                    s.push_str(&format!(
                        "Line {i}: fn process_item_{i}(value: i64) -> i64 {{ value.wrapping_mul({i}) }}\n"
                    ));
                }
            }
        }
    }
    s.push_str("\n\nUser request:");
    s
}

#[allow(clippy::too_many_lines)]
async fn run_assistant_cache_scaling_cmd(args: AssistantCacheScalingArgs) -> Result<()> {
    #[cfg(feature = "llama-local")]
    {
        use fono_assistant::llama_local::LlamaLocalAssistant;
        use fono_assistant::Assistant;

        if !args.model_path.exists() {
            return Err(anyhow!("llama.cpp GGUF model not found at {}", args.model_path.display()));
        }
        if args.sizes.is_empty() {
            return Err(anyhow!("--sizes requires at least one value"));
        }
        let threads = match args.threads {
            Some(t) if t > 0 => t,
            Some(t) => return Err(anyhow!("--threads must be positive, got {t}")),
            None => std::thread::available_parallelism()
                .map(|n| i32::try_from(n.get()).unwrap_or(4))
                .unwrap_or(4),
        };
        let assistant = LlamaLocalAssistant::with_runtime_options(
            &args.model_path,
            args.ctx_size,
            threads,
            args.batch_size,
            args.ubatch_size,
        );
        assistant.prewarm().await?;
        // Suffixes must begin on the stable boundary after `User request:`; add
        // the separating space when the caller did not.
        let suffixes: Vec<String> = args
            .suffix
            .iter()
            .map(|s| if s.starts_with(' ') { s.clone() } else { format!(" {s}") })
            .collect();
        let mut points = Vec::with_capacity(args.sizes.len());
        for &size in &args.sizes {
            let prefix = synthesize_scaling_prefix(args.dimension, size);
            let report = assistant
                .replay_raw_prompt_prefix_cache(
                    prefix.clone(),
                    suffixes.clone(),
                    args.iterations.max(1),
                )
                .await
                .with_context(|| {
                    format!(
                        "cache-scaling replay failed for {} size {size}",
                        args.dimension.as_str()
                    )
                })?;
            let median_uncached =
                median_u64(report.runs.iter().map(|r| r.uncached_latency_ms).collect());
            let median_cached =
                median_u64(report.runs.iter().map(|r| r.cached_latency_ms).collect());
            let ttfb: Vec<u64> =
                report.runs.iter().filter_map(|r| r.cached_time_to_first_token_ms).collect();
            let speedup = if median_cached == 0 {
                0.0
            } else {
                (median_uncached as f64 / median_cached as f64 * 100.0).round() / 100.0
            };
            info!(
                "cache-scaling {} size={size}: prefix {} tok, cached {median_cached}ms vs uncached {median_uncached}ms ({speedup}x)",
                args.dimension.as_str(),
                report.prefix_tokens
            );
            points.push(CacheScalingPoint {
                size,
                prefix_chars: prefix.chars().count(),
                prefix_tokens: report.prefix_tokens,
                state_bytes: report.state_bytes,
                setup_prefill_ms: report.setup_prefill_ms,
                run_count: report.runs.len(),
                outputs_match_count: report.runs.iter().filter(|r| r.outputs_match).count(),
                median_uncached_latency_ms: median_uncached,
                median_cached_latency_ms: median_cached,
                median_cached_time_to_first_token_ms: if ttfb.is_empty() {
                    None
                } else {
                    Some(median_u64(ttfb))
                },
                median_state_restore_ms: median_u64(
                    report.runs.iter().map(|r| r.state_restore_ms).collect(),
                ),
                median_suffix_prefill_ms: median_u64(
                    report.runs.iter().map(|r| r.suffix_prefill_ms).collect(),
                ),
                cached_speedup_x: speedup,
            });
        }
        let model =
            args.model_path.file_stem().and_then(|s| s.to_str()).unwrap_or("llama-cpp").to_string();
        let runtime = assistant_runtime_metadata(AssistantRuntimeMetadataOptions {
            provider: LlmProvider::LlamaCpp,
            llama: LlamaRuntimeOptions {
                model_path: Some(&args.model_path),
                ctx_size: args.ctx_size,
                threads: args.threads,
                batch_size: args.batch_size,
                ubatch_size: args.ubatch_size,
            },
            system_prompt: Some("cache-scaling"),
            history_turns: 0,
        });
        let report = CacheScalingReport {
            schema_version: "assistant-cache-scaling-report-v1",
            provider: "llama-cpp".to_string(),
            model,
            dimension: args.dimension.as_str().to_string(),
            runtime,
            machine_label: args.machine_label.clone(),
            iterations: args.iterations.max(1),
            suffix_count: suffixes.len(),
            points,
        };
        let payload = if args.pretty {
            serde_json::to_string_pretty(&report)?
        } else {
            serde_json::to_string(&report)?
        };
        if let Some(path) = &args.out {
            std::fs::write(path, &payload)
                .with_context(|| format!("write cache-scaling report {}", path.display()))?;
            info!("wrote assistant-cache-scaling report to {}", path.display());
        }
        println!("{payload}");
        Ok(())
    }
    #[cfg(not(feature = "llama-local"))]
    {
        let _ = args;
        Err(anyhow!(
            "compiled without --features llama-local; rebuild with `cargo run -p fono-bench --features llama-local -- assistant-cache-scaling ...`"
        ))
    }
}

#[cfg(feature = "llama-local")]
#[derive(Serialize)]
struct ConversationCacheReport {
    schema_version: &'static str,
    provider: String,
    model: String,
    runtime: BTreeMap<String, String>,
    machine_label: Option<String>,
    iterations: usize,
    turn_count: usize,
    turns: Vec<ConversationCachePoint>,
}

#[cfg(feature = "llama-local")]
#[derive(Serialize)]
struct ConversationCachePoint {
    /// 1-based conversation turn index.
    turn_index: usize,
    /// Number of history turns (user + assistant) preceding this turn.
    history_turns: usize,
    /// Tokens in the cached prefix (system + history). Grows every turn.
    prefix_tokens: usize,
    /// Tokens in the per-turn suffix (the current user text + closing template).
    suffix_tokens: usize,
    /// Bytes of KV state copied for this turn's checkpoint.
    state_bytes: usize,
    /// One-time cost to prefill this turn's (growing) prefix from cold.
    setup_prefill_ms: u64,
    run_count: usize,
    outputs_match_count: usize,
    median_uncached_latency_ms: u64,
    median_cached_latency_ms: u64,
    median_cached_time_to_first_token_ms: Option<u64>,
    median_state_restore_ms: u64,
    median_suffix_prefill_ms: u64,
    /// `median_uncached / median_cached`, rounded to two decimals.
    cached_speedup_x: f64,
}

async fn run_assistant_conversation_cache_cmd(args: AssistantConversationCacheArgs) -> Result<()> {
    #[cfg(feature = "llama-local")]
    {
        use fono_assistant::llama_local::LlamaLocalAssistant;
        use fono_assistant::Assistant;

        if !args.model_path.exists() {
            return Err(anyhow!("llama.cpp GGUF model not found at {}", args.model_path.display()));
        }
        if args.turns.is_empty() {
            return Err(anyhow!("--turn requires at least one value"));
        }
        let threads = match args.threads {
            Some(t) if t > 0 => t,
            Some(t) => return Err(anyhow!("--threads must be positive, got {t}")),
            None => std::thread::available_parallelism()
                .map(|n| i32::try_from(n.get()).unwrap_or(4))
                .unwrap_or(4),
        };
        let assistant = LlamaLocalAssistant::with_runtime_options(
            &args.model_path,
            args.ctx_size,
            threads,
            args.batch_size,
            args.ubatch_size,
        );
        assistant.prewarm().await?;
        let report = assistant
            .replay_conversation_prefix_cache(
                args.system_prompt.clone(),
                args.turns.clone(),
                args.assistant_reply.clone(),
                args.iterations.max(1),
            )
            .await
            .context("conversation prefix-cache replay failed")?;

        let mut points = Vec::with_capacity(report.turns.len());
        for turn in &report.turns {
            let median_uncached =
                median_u64(turn.runs.iter().map(|r| r.uncached_latency_ms).collect());
            let median_cached = median_u64(turn.runs.iter().map(|r| r.cached_latency_ms).collect());
            let ttfb: Vec<u64> =
                turn.runs.iter().filter_map(|r| r.cached_time_to_first_token_ms).collect();
            let speedup = if median_cached == 0 {
                0.0
            } else {
                (median_uncached as f64 / median_cached as f64 * 100.0).round() / 100.0
            };
            info!(
                "conversation-cache turn={} history={}: prefix {} tok, cached {median_cached}ms vs uncached {median_uncached}ms ({speedup}x)",
                turn.turn_index, turn.history_turns, turn.prefix_tokens
            );
            points.push(ConversationCachePoint {
                turn_index: turn.turn_index,
                history_turns: turn.history_turns,
                prefix_tokens: turn.prefix_tokens,
                suffix_tokens: turn.suffix_tokens,
                state_bytes: turn.state_bytes,
                setup_prefill_ms: turn.setup_prefill_ms,
                run_count: turn.runs.len(),
                outputs_match_count: turn.runs.iter().filter(|r| r.outputs_match).count(),
                median_uncached_latency_ms: median_uncached,
                median_cached_latency_ms: median_cached,
                median_cached_time_to_first_token_ms: if ttfb.is_empty() {
                    None
                } else {
                    Some(median_u64(ttfb))
                },
                median_state_restore_ms: median_u64(
                    turn.runs.iter().map(|r| r.state_restore_ms).collect(),
                ),
                median_suffix_prefill_ms: median_u64(
                    turn.runs.iter().map(|r| r.suffix_prefill_ms).collect(),
                ),
                cached_speedup_x: speedup,
            });
        }
        let runtime = assistant_runtime_metadata(AssistantRuntimeMetadataOptions {
            provider: LlmProvider::LlamaCpp,
            llama: LlamaRuntimeOptions {
                model_path: Some(&args.model_path),
                ctx_size: args.ctx_size,
                threads: args.threads,
                batch_size: args.batch_size,
                ubatch_size: args.ubatch_size,
            },
            system_prompt: Some(&args.system_prompt),
            history_turns: 0,
        });
        let out = ConversationCacheReport {
            schema_version: "assistant-conversation-cache-report-v1",
            provider: "llama-cpp".to_string(),
            model: report.model_name,
            runtime,
            machine_label: args.machine_label.clone(),
            iterations: args.iterations.max(1),
            turn_count: points.len(),
            turns: points,
        };
        let payload = if args.pretty {
            serde_json::to_string_pretty(&out)?
        } else {
            serde_json::to_string(&out)?
        };
        if let Some(path) = &args.out {
            std::fs::write(path, &payload)
                .with_context(|| format!("write conversation-cache report {}", path.display()))?;
            info!("wrote assistant-conversation-cache report to {}", path.display());
        }
        println!("{payload}");
        Ok(())
    }
    #[cfg(not(feature = "llama-local"))]
    {
        let _ = args;
        Err(anyhow!(
            "compiled without --features llama-local; rebuild with `cargo run -p fono-bench --features llama-local -- assistant-conversation-cache ...`"
        ))
    }
}

#[derive(Serialize)]
struct ReplayRun {
    iteration: usize,
    latency_ms: u64,
    time_to_first_token_ms: Option<u64>,
    delta_count: usize,
    output_chars: usize,
    output: String,
    trace_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state_restore_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decode_elapsed_ms: Option<u64>,
}

#[derive(Serialize)]
struct ReplayReport {
    schema_version: &'static str,
    provider: String,
    model: String,
    endpoint: Option<String>,
    prompt_source: String,
    prompt_chars: usize,
    prompt_sha256: String,
    runtime: BTreeMap<String, String>,
    machine_label: Option<String>,
    iterations: usize,
    runs: Vec<ReplayRun>,
}

async fn run_embedded_replay(args: &AssistantReplayArgs, prompt: &str) -> Result<Vec<ReplayRun>> {
    #[cfg(feature = "llama-local")]
    {
        use std::time::Instant;

        use fono_assistant::llama_local::LlamaLocalAssistant;
        use fono_assistant::Assistant;
        use fono_core::turn_trace::TurnTrace;
        use serde_json::json;

        let model_path = args.model_path.as_ref().ok_or_else(|| {
            anyhow!("--provider llama-cpp requires --model-path /path/to/model.gguf")
        })?;
        if !model_path.exists() {
            return Err(anyhow!("llama.cpp GGUF model not found at {}", model_path.display()));
        }
        let assistant = match args.threads {
            Some(t) if t > 0 => LlamaLocalAssistant::with_runtime_options(
                model_path,
                args.ctx_size,
                t,
                args.batch_size,
                args.ubatch_size,
            ),
            Some(t) => return Err(anyhow!("--threads must be positive, got {t}")),
            None => LlamaLocalAssistant::with_runtime_options(
                model_path,
                args.ctx_size,
                std::thread::available_parallelism()
                    .map(|n| i32::try_from(n.get()).unwrap_or(4))
                    .unwrap_or(4),
                args.batch_size,
                args.ubatch_size,
            ),
        };
        assistant.prewarm().await?;
        if args.state_cache {
            return run_embedded_state_cache_replay(&assistant, args, prompt).await;
        }

        let mut runs = Vec::with_capacity(args.iterations.max(1));
        for iteration in 0..args.iterations.max(1) {
            let trace = TurnTrace::start_from_env();
            let _trace_guard = trace.as_ref().map(TurnTrace::make_current);
            let started = Instant::now();
            let mut first_token_ms = None;
            let mut output = String::new();
            let mut delta_count = 0_usize;
            let mut stream = assistant.reply_raw_prompt_stream(prompt.to_string()).await?;
            while let Some(delta) = stream.next().await {
                let delta = delta?;
                if delta.tool_event.is_some() || delta.text.is_empty() {
                    continue;
                }
                if first_token_ms.is_none() {
                    first_token_ms = Some(started.elapsed().as_millis() as u64);
                }
                delta_count = delta_count.saturating_add(1);
                output.push_str(&delta.text);
            }
            let output = output.trim().to_string();
            let latency_ms = started.elapsed().as_millis() as u64;
            let trace_path = trace.as_ref().map(|t| t.path().display().to_string());
            if let Some(trace) = trace.as_ref() {
                trace.finish(json!({
                    "iteration": iteration + 1,
                    "latency_ms": latency_ms,
                    "time_to_first_token_ms": first_token_ms,
                    "deltas": delta_count,
                    "output_chars": output.chars().count(),
                }));
            }
            runs.push(ReplayRun {
                iteration: iteration + 1,
                latency_ms,
                time_to_first_token_ms: first_token_ms,
                delta_count,
                output_chars: output.chars().count(),
                output,
                trace_path,
                state_restore_ms: None,
                decode_elapsed_ms: None,
            });
        }
        Ok(runs)
    }
    #[cfg(not(feature = "llama-local"))]
    {
        let _ = (args, prompt);
        Err(anyhow!(
            "compiled without --features llama-local; rebuild with \
             `cargo run -p fono-bench --features llama-local -- assistant-replay \
             --provider llama-cpp --model-path /path/to/model.gguf --prompt-file /path/to/prompt.txt`"
        ))
    }
}

#[cfg(feature = "llama-local")]
async fn run_embedded_state_cache_replay(
    assistant: &fono_assistant::llama_local::LlamaLocalAssistant,
    args: &AssistantReplayArgs,
    prompt: &str,
) -> Result<Vec<ReplayRun>> {
    use fono_core::turn_trace::TurnTrace;
    use serde_json::json;

    let trace = TurnTrace::start_from_env();
    let _trace_guard = trace.as_ref().map(TurnTrace::make_current);
    let report = assistant
        .replay_raw_prompt_with_state_cache(prompt.to_string(), args.iterations.max(1))
        .await?;
    if let Some(trace) = trace.as_ref() {
        trace.finish(json!({
            "prompt_tokens": report.prompt_tokens,
            "state_bytes": report.state_bytes,
            "setup_prefill_ms": report.setup_prefill_ms,
            "iterations": report.runs.len(),
        }));
    }
    let trace_path = trace.as_ref().map(|t| t.path().display().to_string());
    Ok(report
        .runs
        .into_iter()
        .map(|run| ReplayRun {
            iteration: run.iteration,
            latency_ms: run.latency_ms,
            time_to_first_token_ms: run.time_to_first_token_ms,
            delta_count: run.delta_count,
            output_chars: run.output_chars,
            output: run.output,
            trace_path: trace_path.clone(),
            state_restore_ms: Some(run.state_restore_ms),
            decode_elapsed_ms: Some(run.decode_elapsed_ms),
        })
        .collect())
}

async fn run_http_replay(
    args: &AssistantReplayArgs,
    endpoint: &str,
    model: &str,
    prompt: &str,
) -> Result<Vec<ReplayRun>> {
    use std::time::Instant;

    let client = reqwest::Client::builder()
        .pool_idle_timeout(std::time::Duration::from_secs(60))
        .pool_max_idle_per_host(4)
        .tcp_keepalive(Some(std::time::Duration::from_secs(30)))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();
    let api_key = replay_api_key(args.provider, args.api_key_env.as_deref())?;
    let mut runs = Vec::with_capacity(args.iterations.max(1));
    for iteration in 0..args.iterations.max(1) {
        let started = Instant::now();
        let (output, first_token_ms, delta_count) = replay_http_once(
            &client,
            endpoint,
            model,
            prompt,
            api_key.as_deref(),
            args.provider == LlmProvider::Openrouter,
            started,
        )
        .await?;
        let latency_ms = started.elapsed().as_millis() as u64;
        runs.push(ReplayRun {
            iteration: iteration + 1,
            latency_ms,
            time_to_first_token_ms: first_token_ms,
            delta_count,
            output_chars: output.chars().count(),
            output,
            trace_path: None,
            state_restore_ms: None,
            decode_elapsed_ms: None,
        });
    }
    Ok(runs)
}

async fn replay_http_once(
    client: &reqwest::Client,
    endpoint: &str,
    model: &str,
    prompt: &str,
    api_key: Option<&str>,
    is_openrouter: bool,
    started: std::time::Instant,
) -> Result<(String, Option<u64>, usize)> {
    let req = serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": prompt }],
        "temperature": 0.0,
        "max_completion_tokens": 384,
        "stream": true,
        "think": false,
        "chat_template_kwargs": { "enable_thinking": false }
    });
    let mut builder = client.post(endpoint).header("accept", "text/event-stream").json(&req);
    if let Some(key) = api_key.filter(|s| !s.is_empty()) {
        builder = builder.bearer_auth(key);
    }
    if is_openrouter {
        for (name, value) in fono_core::openrouter_attribution::headers() {
            builder = builder.header(name, value);
        }
    }
    let response = builder.send().await.context("assistant replay chat POST failed")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("assistant replay chat returned {status}: {}", truncate(&body, 400)));
    }

    let mut bytes_stream = response.bytes_stream();
    let mut parser = BenchSseBuffer::new();
    let mut output = String::new();
    let mut first_token_ms = None;
    let mut delta_count = 0_usize;
    while let Some(chunk) = bytes_stream.next().await {
        let chunk = chunk.context("assistant replay stream chunk")?;
        parser.push(&chunk);
        while let Some(data) = parser.next_data() {
            let data = data.trim();
            if data == "[DONE]" {
                return Ok((output.trim().to_string(), first_token_ms, delta_count));
            }
            if data.is_empty() {
                continue;
            }
            let parsed: ReplayStreamChunk = serde_json::from_str(data)
                .with_context(|| format!("parse assistant replay stream chunk: {data}"))?;
            for choice in parsed.choices {
                if let Some(content) = choice.delta.content.filter(|s| !s.is_empty()) {
                    if first_token_ms.is_none() {
                        first_token_ms = Some(started.elapsed().as_millis() as u64);
                    }
                    delta_count = delta_count.saturating_add(1);
                    output.push_str(&content);
                }
                if choice.finish_reason.is_some() {
                    return Ok((output.trim().to_string(), first_token_ms, delta_count));
                }
            }
        }
    }
    Ok((output.trim().to_string(), first_token_ms, delta_count))
}

#[derive(Deserialize)]
struct ReplayStreamChunk {
    #[serde(default)]
    choices: Vec<ReplayStreamChoice>,
}

#[derive(Deserialize)]
struct ReplayStreamChoice {
    #[serde(default)]
    delta: ReplayChunkDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct ReplayChunkDelta {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Default)]
struct BenchSseBuffer {
    buf: Vec<u8>,
    cur_data: String,
}

impl BenchSseBuffer {
    fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    fn next_data(&mut self) -> Option<String> {
        loop {
            let nl = self.buf.iter().position(|b| *b == b'\n')?;
            let line_bytes = &self.buf[..nl];
            let line_bytes = match line_bytes.last() {
                Some(b'\r') => &line_bytes[..line_bytes.len() - 1],
                _ => line_bytes,
            };
            let line = std::str::from_utf8(line_bytes).ok().map(str::to_string);
            self.buf.drain(..=nl);
            let Some(line) = line else { continue };
            if line.is_empty() {
                if !self.cur_data.is_empty() {
                    return Some(std::mem::take(&mut self.cur_data));
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("data:") {
                if !self.cur_data.is_empty() {
                    self.cur_data.push('\n');
                }
                self.cur_data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            }
        }
    }
}

fn load_replay_prompt(
    prompt_file: Option<&std::path::Path>,
    trace_file: Option<&std::path::Path>,
    prompt_out: Option<&std::path::Path>,
) -> Result<(String, String)> {
    let (prompt, source) = if let Some(path) = prompt_file {
        (
            std::fs::read_to_string(path)
                .with_context(|| format!("read prompt file {}", path.display()))?,
            path.display().to_string(),
        )
    } else if let Some(path) = trace_file {
        let prompt = extract_trace_prompt(path)?;
        (prompt, path.display().to_string())
    } else {
        return Err(anyhow!("assistant-replay requires --prompt-file or --trace-file"));
    };
    if let Some(path) = prompt_out {
        std::fs::write(path, &prompt)
            .with_context(|| format!("write prompt {}", path.display()))?;
        info!("wrote replay prompt to {}", path.display());
    }
    Ok((prompt, source))
}

fn extract_trace_prompt(path: &std::path::Path) -> Result<String> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("read trace {}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).context("parse assistant trace JSON")?;
    let events = value
        .get("traceEvents")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("trace JSON has no traceEvents array"))?;
    events
        .iter()
        .find_map(|event| event.get("args")?.get("prompt")?.as_str().map(str::to_string))
        .ok_or_else(|| anyhow!("trace contains no event with args.prompt string"))
}

async fn run_extract_trace_prompt_cmd(args: ExtractTracePromptArgs) -> Result<()> {
    let prompt = extract_trace_prompt(&args.trace)?;
    if let Some(path) = args.out.as_ref() {
        std::fs::write(path, &prompt)
            .with_context(|| format!("write prompt {}", path.display()))?;
        println!(
            "wrote {} chars from {} to {} (sha256={})",
            prompt.chars().count(),
            args.trace.display(),
            path.display(),
            sha256_text(&prompt)
        );
    } else {
        println!("{prompt}");
        eprintln!(
            "extracted {} chars from {} (sha256={})",
            prompt.chars().count(),
            args.trace.display(),
            sha256_text(&prompt)
        );
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out = s[..max].to_string();
        out.push('…');
        out
    }
}

fn replay_model(args: &AssistantReplayArgs) -> Result<String> {
    if let Some(model) = args.model.clone() {
        return Ok(model);
    }
    if args.provider == LlmProvider::LlamaCpp {
        return Ok(args
            .model_path
            .as_ref()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or("llama-cpp")
            .to_string());
    }
    Ok(default_assistant_model(args.provider))
}

fn replay_endpoint(provider: LlmProvider, endpoint: Option<&str>) -> Option<String> {
    match (provider, endpoint) {
        (LlmProvider::LlamaCpp, _) | (LlmProvider::None, _) | (LlmProvider::Fake, _) => None,
        (LlmProvider::Ollama, Some(e)) => Some(e.to_string()),
        (LlmProvider::Ollama, None) => {
            Some("http://localhost:11434/v1/chat/completions".to_string())
        }
        (_, Some(e)) => Some(e.to_string()),
        _ => None,
    }
}

fn replay_api_key(provider: LlmProvider, override_env: Option<&str>) -> Result<Option<String>> {
    let name = override_env.filter(|s| !s.trim().is_empty()).or(match provider {
        LlmProvider::Cerebras => Some("CEREBRAS_API_KEY"),
        LlmProvider::Groq => Some("GROQ_API_KEY"),
        LlmProvider::Openai => Some("OPENAI_API_KEY"),
        LlmProvider::Openrouter => Some("OPENROUTER_API_KEY"),
        LlmProvider::Anthropic => Some("ANTHROPIC_API_KEY"),
        LlmProvider::Ollama | LlmProvider::LlamaCpp | LlmProvider::None | LlmProvider::Fake => None,
    });
    name.map(|name| std::env::var(name).with_context(|| format!("{name} not set"))).transpose()
}

fn provider_label(provider: LlmProvider) -> String {
    format!("{:?}", provider).to_ascii_lowercase()
}

async fn run_assistant_tool_use_cmd(args: AssistantToolUseArgs) -> Result<()> {
    if args.provider == LlmProvider::None || args.provider == LlmProvider::LlamaCpp {
        return Err(anyhow!(
            "assistant-tool-use supports `fake`, `ollama`, or cloud OpenAI-compatible providers"
        ));
    }
    let manifest_path = args.fixtures.clone().unwrap_or_else(default_assistant_tool_use_fixtures);
    let manifest = fono_bench::load_assistant_tool_use_manifest(&manifest_path)?;
    let model = args.model.clone().unwrap_or_else(|| default_assistant_model(args.provider));
    let endpoint = resolve_polish_endpoint(args.provider, args.endpoint.as_deref())
        .unwrap_or_else(|| "fake://assistant-tool-use".to_string());
    let secrets = load_optional_secrets(args.secrets.as_deref())?;
    let api_key = match args.provider {
        LlmProvider::Cerebras => Some(resolve_key(&secrets, "CEREBRAS_API_KEY")?),
        LlmProvider::Groq => Some(resolve_key(&secrets, "GROQ_API_KEY")?),
        LlmProvider::Openai => Some(resolve_key(&secrets, "OPENAI_API_KEY")?),
        LlmProvider::Openrouter => Some(resolve_key(&secrets, "OPENROUTER_API_KEY")?),
        LlmProvider::Anthropic => Some(resolve_key(&secrets, "ANTHROPIC_API_KEY")?),
        LlmProvider::Fake | LlmProvider::Ollama => None,
        LlmProvider::None | LlmProvider::LlamaCpp => unreachable!(),
    };
    let languages = parse_languages(&args.languages);
    let cfg = fono_bench::assistant_tool_use::AssistantToolUseRunConfig {
        provider: format!("{:?}", args.provider).to_ascii_lowercase(),
        model,
        endpoint,
        api_key,
        machine_label: args.machine_label.clone(),
        iterations: args.iterations,
        languages,
    };
    let report =
        fono_bench::assistant_tool_use::run_assistant_tool_use(&manifest_path, &manifest, cfg)
            .await?;
    let payload = if args.pretty {
        serde_json::to_string_pretty(&report)?
    } else {
        serde_json::to_string(&report)?
    };
    if let Some(p) = &args.out {
        std::fs::write(p, &payload)?;
        info!("wrote assistant-tool-use report to {}", p.display());
    }
    println!("{payload}");
    Ok(())
}

async fn run_equivalence(args: EquivalenceArgs) -> Result<()> {
    let fixtures_dir = args.fixtures.clone().unwrap_or_else(default_equivalence_dir);
    let manifest_path = fixtures_dir.join("manifest.toml");
    let manifest = Manifest::load(&manifest_path).with_context(|| {
        format!(
            "load equivalence manifest {} \
             (run from the workspace root or pass --fixtures)",
            manifest_path.display()
        )
    })?;
    info!("equivalence: {} fixtures from {}", manifest.fixtures.len(), fixtures_dir.display());

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
                // Phase-0 calibration override: `FONO_WHISPER_THREADS`
                // overrides the default `available_parallelism()` thread
                // count so the bench reflects a sensible compute budget
                // on hosts whose logical-core count over-subscribes
                // whisper.cpp's matmul kernels (e.g. 32-thread Proxmox
                // LXCs where 32 threads make tiny clips slower than 8).
                // Production code is untouched; this affects only the
                // bench binary.
                let stt: Arc<dyn fono_stt::SpeechToText> = if let Some(t) =
                    std::env::var("FONO_WHISPER_THREADS")
                        .ok()
                        .and_then(|s| s.parse::<i32>().ok())
                        .filter(|&t| t > 0)
                {
                    Arc::new(fono_stt::whisper_local::WhisperLocal::with_threads(path, t))
                } else {
                    Arc::new(fono_stt::whisper_local::WhisperLocal::new(path))
                };
                stt
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
            caps = ModelCapabilities { english_only: false, model_label: "fake".to_string() };
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
                caps = ModelCapabilities { english_only: false, model_label: model.clone() };
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

        match run_fixture(fx, &fixtures_dir, Arc::clone(&stt), streaming.clone(), &caps, quick)
            .await
        {
            Ok(r) => {
                // run_fixture does batch + streaming internally; advance
                // by 2 unless the fixture was skipped (no streaming pass).
                let steps = if r.verdict == fono_bench::Verdict::Skipped { 1 } else { 2 };
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
        let lev_str = fmt_lev(r.metrics.stt_levenshtein_norm, report.threshold_levenshtein, &s);
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
        let s: Arc<dyn fono_stt::StreamingStt> = if let Some(t) =
            std::env::var("FONO_WHISPER_THREADS")
                .ok()
                .and_then(|s| s.parse::<i32>().ok())
                .filter(|&t| t > 0)
        {
            Arc::new(fono_stt::whisper_local::WhisperLocal::with_threads(path, t))
        } else {
            Arc::new(fono_stt::whisper_local::WhisperLocal::new(path))
        };
        Ok(Some(Arc::new(fono_bench::equivalence::WhisperStreamingHandle::new(s))))
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

fn build_polish(
    p: LlmProvider,
    model: Option<&str>,
) -> Result<Option<Arc<dyn fono_polish::traits::TextFormatter>>> {
    build_polish_with_options(p, model, None, None, None, 2048, None)
}

fn build_polish_with_options(
    p: LlmProvider,
    model: Option<&str>,
    endpoint: Option<&str>,
    secrets_path: Option<&std::path::Path>,
    model_path: Option<&std::path::Path>,
    ctx_size: u32,
    threads: Option<i32>,
) -> Result<Option<Arc<dyn fono_polish::traits::TextFormatter>>> {
    use fono_polish::openai_compat::OpenAiCompat;
    let secrets = load_optional_secrets(secrets_path)?;
    Ok(match p {
        LlmProvider::None => None,
        LlmProvider::Fake => Some(Arc::new(FakePolish::new())),
        LlmProvider::Cerebras => {
            let key = resolve_key(&secrets, "CEREBRAS_API_KEY")?;
            let m = model.unwrap_or("gpt-oss-120b").to_string();
            Some(Arc::new(OpenAiCompat::cerebras(key, m)))
        }
        LlmProvider::Groq => {
            let key = resolve_key(&secrets, "GROQ_API_KEY")?;
            let m = model.unwrap_or("openai/gpt-oss-20b").to_string();
            Some(Arc::new(OpenAiCompat::groq(key, m)))
        }
        LlmProvider::Openai => {
            let key = resolve_key(&secrets, "OPENAI_API_KEY")?;
            let m = model.unwrap_or("gpt-5.4-nano").to_string();
            Some(Arc::new(OpenAiCompat::openai(key, m)))
        }
        LlmProvider::Ollama => {
            let m = model.unwrap_or("llama3.2").to_string();
            let endpoint = endpoint.unwrap_or("http://localhost:11434/v1/chat/completions");
            Some(Arc::new(OpenAiCompat::ollama(endpoint, m)))
        }
        LlmProvider::Openrouter => {
            let key = resolve_key(&secrets, "OPENROUTER_API_KEY")?;
            let m = model.unwrap_or("openai/gpt-5.4-nano").to_string();
            Some(Arc::new(OpenAiCompat::openrouter(key, m)))
        }
        LlmProvider::Anthropic => {
            let key = resolve_key(&secrets, "ANTHROPIC_API_KEY")?;
            let m = model.unwrap_or("claude-haiku-4-5-20251001").to_string();
            Some(Arc::new(fono_polish::anthropic::AnthropicLlm::new(key, m)))
        }
        LlmProvider::LlamaCpp => {
            #[cfg(feature = "llama-local")]
            {
                let path = model_path.ok_or_else(|| {
                    anyhow!("--provider llama-cpp requires --model-path /path/to/model.gguf")
                })?;
                if !path.exists() {
                    return Err(anyhow!("llama.cpp GGUF model not found at {}", path.display()));
                }
                let formatter: Arc<dyn fono_polish::traits::TextFormatter> = match threads {
                    Some(t) if t > 0 => {
                        Arc::new(fono_polish::llama_local::LlamaLocal::with_threads(
                            path.to_path_buf(),
                            ctx_size,
                            t,
                        ))
                    }
                    Some(t) => return Err(anyhow!("--threads must be positive, got {t}")),
                    None => Arc::new(fono_polish::llama_local::LlamaLocal::new(
                        path.to_path_buf(),
                        ctx_size,
                    )),
                };
                Some(formatter)
            }
            #[cfg(not(feature = "llama-local"))]
            {
                let _ = (model_path, ctx_size, threads);
                return Err(anyhow!(
                    "compiled without --features llama-local; rebuild with \
                     `cargo run -p fono-bench --features llama-local -- polish-text \
                     --provider llama-cpp --model-path /path/to/model.gguf`"
                ));
            }
        }
    })
}

fn build_assistant_with_options(
    p: LlmProvider,
    options: AssistantBuildOptions<'_>,
) -> Result<Arc<dyn fono_assistant::Assistant>> {
    let secrets = load_optional_secrets(options.secrets_path)?;
    Ok(match p {
        LlmProvider::None => {
            return Err(anyhow!("assistant-factual requires an assistant provider, not `none`"))
        }
        LlmProvider::Fake => Arc::new(FakeAssistant),
        LlmProvider::Cerebras => {
            let key = resolve_key(&secrets, "CEREBRAS_API_KEY")?;
            let m = options.model.unwrap_or("zai-glm-4.7").to_string();
            Arc::new(fono_assistant::openai_compat_chat::OpenAiCompatChat::cerebras(key, m))
        }
        LlmProvider::Groq => {
            let key = resolve_key(&secrets, "GROQ_API_KEY")?;
            let m = options.model.unwrap_or("openai/gpt-oss-120b").to_string();
            Arc::new(fono_assistant::openai_compat_chat::OpenAiCompatChat::groq(key, m))
        }
        LlmProvider::Openai => {
            let key = resolve_key(&secrets, "OPENAI_API_KEY")?;
            let m = options.model.unwrap_or("gpt-5.4-mini").to_string();
            Arc::new(fono_assistant::openai_compat_chat::OpenAiCompatChat::openai(key, m))
        }
        LlmProvider::Ollama => {
            let m = options.model.unwrap_or("llama3.2").to_string();
            let endpoint = options.endpoint.unwrap_or("http://localhost:11434/v1/chat/completions");
            Arc::new(fono_assistant::openai_compat_chat::OpenAiCompatChat::ollama(endpoint, m))
        }
        LlmProvider::Openrouter => {
            let key = resolve_key(&secrets, "OPENROUTER_API_KEY")?;
            let m = options.model.unwrap_or("anthropic/claude-haiku-4.5").to_string();
            Arc::new(fono_assistant::openai_compat_chat::OpenAiCompatChat::openrouter(key, m))
        }
        LlmProvider::Anthropic => {
            let key = resolve_key(&secrets, "ANTHROPIC_API_KEY")?;
            let m = options.model.unwrap_or("claude-haiku-4-5-20251001").to_string();
            Arc::new(fono_assistant::anthropic_chat::AnthropicChat::new(key, m))
        }
        LlmProvider::LlamaCpp => {
            #[cfg(feature = "llama-local")]
            {
                let path = options.llama.model_path.ok_or_else(|| {
                    anyhow!("--provider llama-cpp requires --model-path /path/to/model.gguf")
                })?;
                if !path.exists() {
                    return Err(anyhow!("llama.cpp GGUF model not found at {}", path.display()));
                }
                let assistant: Arc<dyn fono_assistant::Assistant> = match options.llama.threads {
                    Some(t) if t > 0 => Arc::new(
                        fono_assistant::llama_local::LlamaLocalAssistant::with_runtime_options(
                            path.to_path_buf(),
                            options.llama.ctx_size,
                            t,
                            options.llama.batch_size,
                            options.llama.ubatch_size,
                        ),
                    ),
                    Some(t) => return Err(anyhow!("--threads must be positive, got {t}")),
                    None => Arc::new(
                        fono_assistant::llama_local::LlamaLocalAssistant::with_runtime_options(
                            path.to_path_buf(),
                            options.llama.ctx_size,
                            std::thread::available_parallelism()
                                .map(|n| i32::try_from(n.get()).unwrap_or(4))
                                .unwrap_or(4),
                            options.llama.batch_size,
                            options.llama.ubatch_size,
                        ),
                    ),
                };
                assistant
            }
            #[cfg(not(feature = "llama-local"))]
            {
                let _ = options.llama;
                return Err(anyhow!(
                    "compiled without --features llama-local; rebuild with \
                     `cargo run -p fono-bench --features llama-local -- assistant-factual \
                     --provider llama-cpp --model-path /path/to/model.gguf`"
                ));
            }
        }
    })
}

struct FakeAssistant;

#[async_trait]
impl fono_assistant::Assistant for FakeAssistant {
    async fn reply_stream(
        &self,
        user_text: &str,
        _ctx: &fono_assistant::AssistantContext,
    ) -> Result<BoxStream<'static, Result<fono_assistant::TokenDelta>>> {
        let answer = if user_text.to_lowercase().contains("france")
            || user_text.to_lowercase().contains("franței")
        {
            "Paris"
        } else {
            "fake"
        };
        Ok(Box::pin(futures::stream::once(async move {
            Ok(fono_assistant::TokenDelta::text(answer.to_string()))
        })))
    }

    fn name(&self) -> &'static str {
        "fake"
    }
}

fn default_assistant_model(provider: LlmProvider) -> String {
    match provider {
        LlmProvider::None | LlmProvider::Fake => "fake".to_string(),
        LlmProvider::Cerebras => "zai-glm-4.7".to_string(),
        LlmProvider::Groq => "openai/gpt-oss-120b".to_string(),
        LlmProvider::Openai => "gpt-5.4-mini".to_string(),
        LlmProvider::Ollama => "llama3.2".to_string(),
        LlmProvider::Openrouter => "anthropic/claude-haiku-4.5".to_string(),
        LlmProvider::Anthropic => "claude-haiku-4-5-20251001".to_string(),
        LlmProvider::LlamaCpp => "llama-cpp".to_string(),
    }
}

fn load_optional_secrets(path: Option<&std::path::Path>) -> Result<fono_core::Secrets> {
    let path = path.map(PathBuf::from).unwrap_or_else(default_secrets_file);
    if path.exists() {
        fono_core::Secrets::load(&path).with_context(|| format!("load secrets {}", path.display()))
    } else {
        Ok(fono_core::Secrets::default())
    }
}

fn resolve_key(secrets: &fono_core::Secrets, name: &str) -> Result<String> {
    secrets.resolve(name).with_context(|| format!("{name} not set in secrets file or environment"))
}

fn default_polish_model(provider: LlmProvider, model_path: Option<&std::path::Path>) -> String {
    match provider {
        LlmProvider::None | LlmProvider::Fake => "fake".to_string(),
        LlmProvider::Cerebras => "gpt-oss-120b".to_string(),
        LlmProvider::Groq => "openai/gpt-oss-20b".to_string(),
        LlmProvider::Openai => "gpt-5.4-nano".to_string(),
        LlmProvider::Ollama => "llama3.2".to_string(),
        LlmProvider::Openrouter => "openai/gpt-5.4-nano".to_string(),
        LlmProvider::Anthropic => "claude-haiku-4-5-20251001".to_string(),
        LlmProvider::LlamaCpp => model_path
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or("llama-cpp")
            .to_string(),
    }
}

fn resolve_polish_endpoint(provider: LlmProvider, endpoint: Option<&str>) -> Option<String> {
    match (provider, endpoint) {
        (LlmProvider::Ollama, Some(e)) => Some(e.to_string()),
        (LlmProvider::Ollama, None) => {
            Some("http://localhost:11434/v1/chat/completions".to_string())
        }
        (LlmProvider::LlamaCpp, _) => None,
        (LlmProvider::Openrouter, Some(e)) => Some(e.to_string()),
        (LlmProvider::Anthropic, Some(e)) => Some(e.to_string()),
        (_, Some(e)) => Some(e.to_string()),
        _ => None,
    }
}

fn resolve_assistant_system_prompt(value: &str) -> String {
    match value {
        "default" => fono_core::config::default_assistant_prompt().to_string(),
        "empty" => String::new(),
        other => other.to_string(),
    }
}

fn assistant_runtime_metadata(
    options: AssistantRuntimeMetadataOptions<'_>,
) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    if options.provider == LlmProvider::LlamaCpp {
        out.insert("runtime".to_string(), "llama.cpp via llama-cpp-2".to_string());
        out.insert("ctx_size".to_string(), options.llama.ctx_size.to_string());
        out.insert(
            "batch_size".to_string(),
            options.llama.batch_size.map_or_else(|| "ctx".to_string(), |v| v.to_string()),
        );
        out.insert(
            "ubatch_size".to_string(),
            options.llama.ubatch_size.map_or_else(|| "auto".to_string(), |v| v.to_string()),
        );
        if let Some(t) = options.llama.threads {
            out.insert("threads".to_string(), t.to_string());
        } else {
            let detected = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
            out.insert("threads".to_string(), format!("auto:{detected}"));
        }
        if let Some(path) = options.llama.model_path {
            out.insert("model_path".to_string(), path.display().to_string());
            if let Ok(meta) = std::fs::metadata(path) {
                out.insert("model_size_bytes".to_string(), meta.len().to_string());
            }
        }
    }
    if let Some(system_prompt) = options.system_prompt {
        out.insert("system_prompt".to_string(), system_prompt.to_string());
    }
    out.insert("history_turns".to_string(), options.history_turns.to_string());
    out
}

fn sha256_text(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

fn polish_runtime_metadata(
    provider: LlmProvider,
    model_path: Option<&std::path::Path>,
    ctx_size: u32,
    threads: Option<i32>,
) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    if provider != LlmProvider::LlamaCpp {
        return out;
    }
    out.insert("runtime".to_string(), "llama.cpp via llama-cpp-2".to_string());
    out.insert("ctx_size".to_string(), ctx_size.to_string());
    if let Some(t) = threads {
        out.insert("threads".to_string(), t.to_string());
    } else {
        let detected = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        out.insert("threads".to_string(), format!("auto:{detected}"));
    }
    if let Some(path) = model_path {
        out.insert("model_path".to_string(), path.display().to_string());
        if let Ok(meta) = std::fs::metadata(path) {
            out.insert("model_size_bytes".to_string(), meta.len().to_string());
        }
    }
    out
}

fn parse_languages(languages: &str) -> Vec<String> {
    languages.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

fn default_bench_dir() -> PathBuf {
    let cache = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|| {
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

fn default_polish_text_fixtures() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join("tests").join("fixtures").join("polish_text").join("fixtures.toml"))
        .unwrap_or_else(|| PathBuf::from("tests/fixtures/polish_text/fixtures.toml"))
}

fn default_assistant_factual_fixtures() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|root| {
            root.join("tests").join("fixtures").join("assistant_factual").join("fixtures.toml")
        })
        .unwrap_or_else(|| PathBuf::from("tests/fixtures/assistant_factual/fixtures.toml"))
}

fn default_assistant_tool_use_fixtures() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|root| {
            root.join("tests")
                .join("fixtures")
                .join("assistant_tool_use")
                .join("homeassistant_lights.toml")
        })
        .unwrap_or_else(|| {
            PathBuf::from("tests/fixtures/assistant_tool_use/homeassistant_lights.toml")
        })
}

fn default_secrets_file() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let test_secrets = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join("tests").join("secrets.toml"));
    if let Some(path) = test_secrets.filter(|p| p.exists()) {
        path
    } else {
        fono_core::Paths::resolve()
            .map(|p| p.secrets_file())
            .unwrap_or_else(|_| PathBuf::from("tests/secrets.toml"))
    }
}

#[cfg(feature = "whisper-local")]
fn default_models_dir() -> PathBuf {
    let cache = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|| {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".cache"))
            .unwrap_or_else(|| PathBuf::from("/tmp"))
    });
    cache.join("fono").join("models").join("whisper")
}
