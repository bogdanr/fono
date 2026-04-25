// SPDX-License-Identifier: GPL-3.0-only
//! Clap-powered CLI surface + dispatch to daemon / subcommands.

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use fono_core::{Config, Paths, Secrets};
use fono_ipc::{Request, Response};

use crate::{daemon, doctor, wizard};

#[derive(Debug, Parser)]
#[command(
    name = "fono",
    version,
    about = "Lightweight native voice dictation for Linux, Windows, and macOS."
)]
pub struct Cli {
    /// Enable debug logging (shorthand for `FONO_LOG=debug`).
    /// Pass twice (`-vv`) for trace-level + file/line annotations.
    #[arg(long = "debug", short = 'v', action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Silence everything below `warn`.
    #[arg(long = "quiet", short = 'q', global = true)]
    pub quiet: bool,

    /// Skip the tray icon (use on TTY-only machines or when the compositor
    /// has no system tray). Only affects the daemon.
    #[arg(long = "no-tray", global = true)]
    pub no_tray: bool,

    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

/// Effective log verbosity derived from `--debug` / `--quiet` flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    Quiet,
    Info,
    Debug,
    Trace,
}

impl Verbosity {
    pub fn as_filter(self) -> &'static str {
        match self {
            Self::Quiet => "warn",
            Self::Info => "info",
            Self::Debug => {
                "fono=debug,fono_core=debug,fono_hotkey=debug,fono_tray=debug,\
                fono_audio=debug,fono_stt=debug,fono_llm=debug,fono_inject=debug,\
                fono_ipc=debug,fono_download=debug,info"
            }
            Self::Trace => {
                "fono=trace,fono_core=trace,fono_hotkey=trace,fono_tray=trace,\
                fono_audio=trace,fono_stt=trace,fono_llm=trace,fono_inject=trace,\
                fono_ipc=trace,fono_download=trace,debug"
            }
        }
    }

    pub fn is_trace(self) -> bool {
        matches!(self, Self::Trace)
    }
}

impl Cli {
    pub fn verbosity(&self) -> Verbosity {
        if self.quiet {
            Verbosity::Quiet
        } else {
            match self.verbose {
                0 => Verbosity::Info,
                1 => Verbosity::Debug,
                _ => Verbosity::Trace,
            }
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Start the daemon (default when no subcommand is given).
    Daemon {
        /// Run without a tray icon (TTY-only machines).
        #[arg(long)]
        no_tray: bool,
    },
    /// IPC: toggle recording on the running daemon.
    Toggle,
    /// One-shot: record until silence/Esc, transcribe, inject, exit.
    Record {
        /// Skip text injection — print the cleaned text to stdout only.
        #[arg(long)]
        no_inject: bool,
        /// Maximum recording duration in seconds (0 = unbounded).
        #[arg(long, default_value_t = 30)]
        max_seconds: u64,
    },
    /// Transcribe a WAV file (16-bit PCM mono, any sample rate) without
    /// touching the microphone. Useful for verifying API keys.
    Transcribe {
        /// Path to a WAV file.
        path: std::path::PathBuf,
        /// Skip the LLM cleanup step.
        #[arg(long)]
        no_llm: bool,
    },
    /// Re-type the last cleaned transcription.
    PasteLast,
    /// Browse the transcription history.
    History {
        #[arg(long)]
        search: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
        /// Show only the most recent entry with full STT/LLM I/O detail
        /// (raw transcript, cleaned LLM output, app context, backends,
        /// timestamps). Combine with `--json` for machine-readable output.
        #[arg(long)]
        last: bool,
    },
    /// Manage configuration.
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
    /// Manage models.
    Models {
        #[command(subcommand)]
        action: ModelsCmd,
    },
    /// Re-run the first-run wizard.
    Setup,
    /// Diagnostic report.
    Doctor,
    /// Probe the host's hardware and print the recommended local-model tier.
    Hwprobe {
        /// Emit machine-readable JSON instead of the default text report.
        #[arg(long)]
        json: bool,
    },
    /// Print shell completions (bash, zsh, fish, powershell, elvish).
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    Edit,
    Show,
    Path,
}

#[derive(Debug, Subcommand)]
pub enum ModelsCmd {
    List,
    Install { name: String },
    Remove { name: String },
    Verify,
}

#[allow(clippy::large_stack_frames)]
pub async fn run(cli: Cli) -> Result<()> {
    let paths = Paths::resolve().context("resolve XDG paths")?;
    paths.ensure()?;

    // Implicit first-run wizard: if there's no config and the user didn't
    // explicitly pick a non-interactive subcommand, enter the wizard.
    let needs_wizard = !paths.config_file().exists();

    match cli.cmd {
        None | Some(Cmd::Daemon { .. }) => {
            if needs_wizard {
                wizard::run(&paths).await?;
            }
            let no_tray = cli.no_tray || matches!(cli.cmd, Some(Cmd::Daemon { no_tray: true }));
            daemon::run(&paths, no_tray, cli.verbosity()).await
        }
        Some(Cmd::Setup) => Box::pin(wizard::run(&paths)).await,
        Some(Cmd::Toggle) => ipc_simple(&paths, Request::Toggle).await,
        Some(Cmd::PasteLast) => ipc_simple(&paths, Request::PasteLast).await,
        Some(Cmd::Doctor) => {
            let report = doctor::report(&paths).await?;
            println!("{report}");
            Ok(())
        }
        Some(Cmd::Hwprobe { json }) => {
            hwprobe_cmd(&paths, json);
            Ok(())
        }
        Some(Cmd::Record {
            no_inject,
            max_seconds,
        }) => record_cmd(&paths, no_inject, max_seconds).await,
        Some(Cmd::Transcribe { path, no_llm }) => transcribe_cmd(&paths, &path, no_llm).await,
        Some(Cmd::History {
            search,
            limit,
            json,
            last,
        }) => history_cmd(&paths, search.as_deref(), limit, json, last),
        Some(Cmd::Config { action }) => config_cmd(&paths, action),
        Some(Cmd::Models { action }) => models_cmd(&paths, action).await,
        Some(Cmd::Completions { shell }) => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "fono", &mut std::io::stdout());
            Ok(())
        }
    }
}

async fn ipc_simple(paths: &Paths, req: Request) -> Result<()> {
    match fono_ipc::request(&paths.ipc_socket(), &req).await {
        Ok(Response::Ok) => Ok(()),
        Ok(Response::Text(t)) => {
            println!("{t}");
            Ok(())
        }
        Ok(Response::Error(e)) => Err(anyhow::anyhow!(e)),
        Err(e) => Err(e),
    }
}

fn history_cmd(
    paths: &Paths,
    search: Option<&str>,
    limit: usize,
    json: bool,
    last: bool,
) -> Result<()> {
    let db = fono_core::history::HistoryDb::open(&paths.history_db())?;
    let rows = if last {
        db.recent(1)?
    } else if let Some(q) = search {
        db.search(q, limit)?
    } else {
        db.recent(limit)?
    };
    if last {
        if rows.is_empty() {
            println!("(no history yet)");
            return Ok(());
        }
        let t = &rows[0];
        if json {
            let v = serde_json::json!({
                "id": t.id,
                "ts": t.ts,
                "duration_ms": t.duration_ms,
                "raw": t.raw,
                "cleaned": t.cleaned,
                "app_class": t.app_class,
                "app_title": t.app_title,
                "stt_backend": t.stt_backend,
                "llm_backend": t.llm_backend,
                "language": t.language,
            });
            println!("{}", serde_json::to_string_pretty(&v)?);
        } else {
            println!("id           : {:?}", t.id);
            println!("ts           : {}", t.ts);
            println!("duration_ms  : {:?}", t.duration_ms);
            println!("language     : {:?}", t.language);
            println!("app_class    : {:?}", t.app_class);
            println!("app_title    : {:?}", t.app_title);
            println!("stt_backend  : {:?}", t.stt_backend);
            println!("llm_backend  : {:?}", t.llm_backend);
            println!("raw          : {}", t.raw);
            println!(
                "cleaned      : {}",
                t.cleaned.as_deref().unwrap_or("(none — no LLM cleanup)")
            );
        }
        return Ok(());
    }
    if json {
        let arr: Vec<_> = rows
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "ts": t.ts,
                    "raw": t.raw,
                    "cleaned": t.cleaned,
                    "language": t.language,
                    "stt_backend": t.stt_backend,
                    "llm_backend": t.llm_backend,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
    } else if rows.is_empty() {
        println!("(no history yet)");
    } else {
        for t in rows {
            let text = t.cleaned.as_deref().unwrap_or(&t.raw);
            println!("[{}] {}", t.ts, text);
        }
    }
    Ok(())
}

fn config_cmd(paths: &Paths, action: ConfigCmd) -> Result<()> {
    match action {
        ConfigCmd::Path => {
            println!("{}", paths.config_file().display());
        }
        ConfigCmd::Show => {
            let cfg = Config::load(&paths.config_file())?;
            println!("{}", toml::to_string_pretty(&cfg)?);
        }
        ConfigCmd::Edit => {
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nano".to_string());
            let status = std::process::Command::new(&editor)
                .arg(paths.config_file())
                .status()?;
            if !status.success() {
                return Err(anyhow::anyhow!("{editor} exited with {status}"));
            }
        }
    }
    let _ = Secrets::load(&paths.secrets_file())?; // surface mode errors
    Ok(())
}

async fn models_cmd(paths: &Paths, action: ModelsCmd) -> Result<()> {
    use fono_stt::ModelRegistry;
    match action {
        ModelsCmd::List => {
            for m in ModelRegistry::all() {
                let marker = if paths
                    .whisper_models_dir()
                    .join(format!("ggml-{}.bin", m.name))
                    .exists()
                {
                    "[installed]"
                } else {
                    "           "
                };
                println!(
                    "{marker} whisper:{:<10} {:>5} MB  multilingual={}",
                    m.name, m.approx_mb, m.multilingual
                );
            }
        }
        ModelsCmd::Install { name } => {
            let m = ModelRegistry::get(&name)
                .ok_or_else(|| anyhow::anyhow!("unknown model {name:?}"))?;
            let dest = paths
                .whisper_models_dir()
                .join(format!("ggml-{}.bin", m.name));
            if dest.exists() {
                println!("already installed: {}", dest.display());
                return Ok(());
            }
            let url = ModelRegistry::url_for(m);
            println!(
                "Downloading {} ({} MB)\n  from {url}\n  to   {}",
                m.name,
                m.approx_mb,
                dest.display()
            );
            fono_download::download(&url, &dest, m.sha256).await?;
            println!("Installed: {}", dest.display());
        }
        ModelsCmd::Remove { name } => {
            let path = paths.whisper_models_dir().join(format!("ggml-{name}.bin"));
            if path.exists() {
                std::fs::remove_file(&path)?;
                println!("removed {}", path.display());
            } else {
                println!("not installed: {name}");
            }
        }
        ModelsCmd::Verify => {
            println!("model verification scheduled for a follow-up phase");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------
// `fono record` — one-shot capture → STT → LLM → inject from CLI.
// ---------------------------------------------------------------------
async fn record_cmd(paths: &Paths, no_inject: bool, max_seconds: u64) -> Result<()> {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use fono_audio::{AudioCapture, CaptureConfig};
    use fono_core::{Config, Secrets};

    let config = Arc::new(Config::load(&paths.config_file())?);
    let secrets = Secrets::load(&paths.secrets_file())?;

    let cap_cfg = CaptureConfig {
        input_device: config.audio.input_device.clone(),
        target_sample_rate: config.audio.sample_rate,
    };
    let cap = AudioCapture::new(cap_cfg.clone());
    let handle = cap.start().context("start audio capture")?;
    eprintln!(
        "fono record: capturing from default input ({} Hz). Press Ctrl-C or wait \
         {max_seconds}s to stop.",
        cap_cfg.target_sample_rate
    );

    let started = Instant::now();
    let max = if max_seconds == 0 {
        Duration::from_secs(60 * 60)
    } else {
        Duration::from_secs(max_seconds)
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            eprintln!("fono record: stopped by Ctrl-C");
        }
        () = tokio::time::sleep(max) => {
            eprintln!("fono record: hit {max_seconds}s timeout");
        }
    }
    let elapsed = started.elapsed();
    let pcm = {
        let buf = handle.buffer.lock().expect("buffer mutex");
        buf.samples().to_vec()
    };
    drop(handle);

    let stt = fono_stt::build_stt(&config.stt, &secrets, &paths.whisper_models_dir())?;
    let llm = fono_llm::build_llm(&config.llm, &secrets)?;

    eprintln!(
        "fono record: captured {} samples ({} ms); running STT…",
        pcm.len(),
        elapsed.as_millis()
    );
    let lang = if config.general.language == "auto" {
        None
    } else {
        Some(config.general.language.as_str())
    };
    let trans = stt
        .transcribe(&pcm, cap_cfg.target_sample_rate, lang)
        .await?;
    let raw = trans.text.trim().to_string();
    if raw.is_empty() {
        eprintln!("fono record: STT returned empty text");
        return Ok(());
    }
    let final_text = if let Some(l) = llm.as_ref() {
        let ctx = fono_llm::FormatContext {
            main_prompt: config.llm.prompt.main.clone(),
            advanced_prompt: config.llm.prompt.advanced.clone(),
            dictionary: config.llm.prompt.dictionary.clone(),
            language: trans.language.clone(),
            ..Default::default()
        };
        match l.format(&raw, &ctx).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("fono record: LLM cleanup failed ({e:#}); using raw transcript");
                raw.clone()
            }
        }
    } else {
        raw.clone()
    };
    println!("{final_text}");
    if !no_inject {
        if let Err(e) = fono_inject::type_text(&final_text) {
            eprintln!("fono record: inject failed: {e:#}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------
// `fono transcribe <PATH>` — WAV file → STT (+optional LLM) → stdout.
// ---------------------------------------------------------------------
async fn transcribe_cmd(paths: &Paths, wav: &std::path::Path, no_llm: bool) -> Result<()> {
    use fono_core::{Config, Secrets};

    let config = Config::load(&paths.config_file())?;
    let secrets = Secrets::load(&paths.secrets_file())?;
    let (pcm, sample_rate) =
        read_wav_mono_f32(wav).with_context(|| format!("read wav {}", wav.display()))?;
    let stt = fono_stt::build_stt(&config.stt, &secrets, &paths.whisper_models_dir())?;
    let llm = if no_llm {
        None
    } else {
        fono_llm::build_llm(&config.llm, &secrets)?
    };
    let trans = stt.transcribe(&pcm, sample_rate, None).await?;
    let raw = trans.text.trim().to_string();
    if let Some(l) = llm.as_ref() {
        let ctx = fono_llm::FormatContext {
            main_prompt: config.llm.prompt.main.clone(),
            advanced_prompt: config.llm.prompt.advanced.clone(),
            dictionary: config.llm.prompt.dictionary.clone(),
            language: trans.language.clone(),
            ..Default::default()
        };
        match l.format(&raw, &ctx).await {
            Ok(c) => println!("{c}"),
            Err(e) => {
                eprintln!("LLM cleanup failed ({e:#}); raw transcript follows:");
                println!("{raw}");
            }
        }
    } else {
        println!("{raw}");
    }
    Ok(())
}

/// Minimal 16-bit-PCM mono WAV reader (no `hound` dep). Supports stereo
/// by averaging channels. Returns `(f32 samples in [-1.0, 1.0], rate)`.
fn read_wav_mono_f32(path: &std::path::Path) -> Result<(Vec<f32>, u32)> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        anyhow::bail!("not a RIFF/WAVE file: {}", path.display());
    }
    let mut i = 12;
    let mut fmt_chans: u16 = 1;
    let mut fmt_rate: u32 = 16_000;
    let mut fmt_bps: u16 = 16;
    let mut data_off = 0;
    let mut data_len = 0;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let sz =
            u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
        let body = i + 8;
        if id == b"fmt " {
            fmt_chans = u16::from_le_bytes([bytes[body + 2], bytes[body + 3]]);
            fmt_rate = u32::from_le_bytes([
                bytes[body + 4],
                bytes[body + 5],
                bytes[body + 6],
                bytes[body + 7],
            ]);
            fmt_bps = u16::from_le_bytes([bytes[body + 14], bytes[body + 15]]);
        } else if id == b"data" {
            data_off = body;
            data_len = sz;
            break;
        }
        i = body + sz;
    }
    if data_off == 0 {
        anyhow::bail!("no `data` chunk in {}", path.display());
    }
    if fmt_bps != 16 {
        anyhow::bail!("only 16-bit PCM supported (got {fmt_bps}-bit)");
    }
    let body = &bytes[data_off..data_off + data_len];
    let frames = body.len() / 2 / fmt_chans as usize;
    let mut out = Vec::with_capacity(frames);
    for f_i in 0..frames {
        let mut sum = 0f32;
        for c in 0..fmt_chans {
            let off = (f_i * fmt_chans as usize + c as usize) * 2;
            let s = i16::from_le_bytes([body[off], body[off + 1]]);
            sum += f32::from(s) / f32::from(i16::MAX);
        }
        out.push(sum / f32::from(fmt_chans));
    }
    Ok((out, fmt_rate))
}

// ---------------------------------------------------------------------
// `fono hwprobe` — print the hardware snapshot + recommended local tier.
// ---------------------------------------------------------------------
fn hwprobe_cmd(paths: &Paths, json: bool) {
    use fono_core::hwcheck;
    let snap = hwcheck::probe(&paths.cache_dir);
    let tier = snap.tier();
    if json {
        let v = serde_json::json!({
            "snapshot": snap,
            "tier": tier.as_str(),
            "default_whisper_model": tier.default_whisper_model(),
            "local_default": tier.local_default(),
            "suitability": match snap.suitability() {
                Ok(()) => serde_json::Value::Null,
                Err(reason) => serde_json::Value::String(reason.to_string()),
            },
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    } else {
        let ram_gb = snap.total_ram_bytes / (1024 * 1024 * 1024);
        let disk_gb = snap.free_disk_bytes / (1024 * 1024 * 1024);
        let isa = if snap.cpu_features.avx2 {
            "AVX2"
        } else if snap.cpu_features.neon {
            "NEON"
        } else {
            "no-vec"
        };
        println!(
            "cores : {} physical / {} logical  ({isa})",
            snap.physical_cores, snap.logical_cores
        );
        println!(
            "ram   : {ram_gb} GB total · disk free : {disk_gb} GB · {}/{}",
            snap.os, snap.arch
        );
        println!(
            "tier  : {} (recommends whisper-{})",
            tier.as_str(),
            tier.default_whisper_model()
        );
        if let Err(reason) = snap.suitability() {
            println!("note  : unsuitable for local — {reason}");
        }
    }
}
