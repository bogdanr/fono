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
        // Targets demoted at default verbosity so the INFO-level model-load
        // chatter from whisper.cpp / llama.cpp / ggml stays out of normal
        // startup output. Re-enable any of them on demand with e.g.
        // `FONO_LOG=llama-cpp-2=info`.
        //
        // `llama-cpp-2=error` (rather than `=warn`) intentionally hides two
        // chronic, harmless warnings that fire on every model load and every
        // inference call respectively:
        //   1. `control-looking token: ... '</s>' was not control-type` —
        //      cosmetic Qwen2.5 GGUF metadata quirk; llama.cpp overrides
        //      the type internally and continues correctly.
        //   2. `n_ctx_seq (N) < n_ctx_train (M) -- the full capacity of the
        //      model will not be utilized` — informational, not an error;
        //      cleanup prompts never need the model's full training ctx.
        // Real load / inference errors propagate via `anyhow` from
        // `LlamaModel::load_from_file` / `ctx.decode` and surface with full
        // context regardless of this filter.
        match self {
            Self::Quiet => {
                "warn,whisper_rs::ggml_logging_hook=warn,whisper_rs::whisper_logging_hook=warn,\
                 llama-cpp-2=error"
            }
            Self::Info => {
                "info,whisper_rs::ggml_logging_hook=warn,whisper_rs::whisper_logging_hook=warn,\
                 llama-cpp-2=error"
            }
            Self::Debug => {
                "fono=debug,fono_core=debug,fono_hotkey=debug,fono_tray=debug,\
                fono_audio=debug,fono_stt=debug,fono_llm=debug,fono_inject=debug,\
                fono_ipc=debug,fono_download=debug,whisper_rs::ggml_logging_hook=warn,\
                whisper_rs::whisper_logging_hook=warn,llama-cpp-2=warn,info"
            }
            Self::Trace => {
                "fono=trace,fono_core=trace,fono_hotkey=trace,fono_tray=trace,\
                fono_audio=trace,fono_stt=trace,fono_llm=trace,fono_inject=trace,\
                fono_ipc=trace,fono_download=trace,whisper_rs::ggml_logging_hook=warn,\
                whisper_rs::whisper_logging_hook=warn,llama-cpp-2=info,debug"
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
        /// Override the persisted STT backend for this call only
        /// (provider-switching plan task S6).
        #[arg(long)]
        stt: Option<String>,
        /// Override the persisted LLM backend for this call only.
        /// Use `none` to skip cleanup.
        #[arg(long)]
        llm: Option<String>,
        /// Run in live (streaming) mode for this call only. Requires
        /// the binary to have been built with the `interactive` cargo
        /// feature; without it, this flag prints a helpful error and
        /// falls back to batch mode. Plan v6 R7.4 / R18.22.
        #[arg(long)]
        live: bool,
    },
    /// Transcribe a WAV file (16-bit PCM mono, any sample rate) without
    /// touching the microphone. Useful for verifying API keys.
    Transcribe {
        /// Path to a WAV file.
        path: std::path::PathBuf,
        /// Skip the LLM cleanup step.
        #[arg(long)]
        no_llm: bool,
        /// Override the persisted STT backend for this call only.
        #[arg(long)]
        stt: Option<String>,
        /// Override the persisted LLM backend for this call only.
        #[arg(long)]
        llm: Option<String>,
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
    /// Smoke-test the inject + clipboard delivery path with literal
    /// text (no audio, no STT, no LLM). Use this to verify that text
    /// can actually reach your focused window or clipboard.
    TestInject {
        /// Text to inject and copy to clipboard.
        text: String,
        /// Skip key-injection (only copy to clipboard).
        #[arg(long)]
        no_inject: bool,
        /// Skip clipboard copy (only key-injection).
        #[arg(long)]
        no_clipboard: bool,
        /// Override the X11 XTEST paste shortcut for this run.
        /// Accepted: `shift-insert` (default — universal),
        /// `ctrl-v` (GUI-only), `ctrl-shift-v` (modern terminals).
        /// Sets `FONO_PASTE_SHORTCUT` for the duration of the call.
        #[arg(long, value_name = "SHORTCUT")]
        shortcut: Option<String>,
    },
    /// Smoke-test the live-dictation overlay: open the real overlay
    /// window for ~3 seconds with a sample status sequence, then exit.
    /// Useful for verifying the `interactive` feature was compiled in
    /// and that winit/softbuffer can paint on this compositor. Plan
    /// R5 / Slice A. Without `--features interactive`, prints a hint
    /// and exits.
    TestOverlay,
    /// Probe the host's hardware and print the recommended local-model tier.
    Hwprobe {
        /// Emit machine-readable JSON instead of the default text report.
        #[arg(long)]
        json: bool,
    },
    /// Switch active STT / LLM backend (no daemon restart needed).
    /// Provider-switching plan task S4.
    Use {
        #[command(subcommand)]
        action: UseCmd,
    },
    /// Manage API keys in `~/.config/fono/secrets.toml`.
    /// Provider-switching plan task S7.
    Keys {
        #[command(subcommand)]
        action: KeysCmd,
    },
    /// Print shell completions (bash, zsh, fish, powershell, elvish).
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Check for a newer release on GitHub and (optionally) self-update
    /// the running binary in place. Background tray-icon equivalent
    /// runs automatically on the daemon when `[update] auto_check`
    /// is enabled (the default).
    Update {
        /// Only print the status; never download.
        /// Exit code is `0` when up-to-date and `1` when an update
        /// is available — handy for scripts.
        #[arg(long)]
        check: bool,
        /// Skip the interactive confirmation prompt.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Resolve and verify the new binary, but do not replace the
        /// running executable.
        #[arg(long)]
        dry_run: bool,
        /// `stable` (default) or `prerelease`.
        #[arg(long, default_value = "stable")]
        channel: String,
        /// After a successful update, do **not** re-exec into the new
        /// binary. The next manual start picks it up.
        #[arg(long)]
        no_restart: bool,
        /// Override the install directory. Useful when running with
        /// elevated privileges and the autodetected `current_exe()`
        /// is in `/usr/local/bin/fono`. Equivalent to `BIN_DIR`
        /// semantics from the install script. The package-managed
        /// path refusal still applies (`/usr/bin`, `/bin`,
        /// `/usr/sbin`).
        #[arg(long)]
        bin_dir: Option<std::path::PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum UseCmd {
    /// Switch the active STT backend.
    Stt {
        /// One of: local, groq, openai, deepgram, assemblyai, cartesia,
        /// azure, speechmatics, google, nemotron.
        backend: String,
    },
    /// Switch the active LLM backend.
    Llm {
        /// One of: none, local, cerebras, groq, openai, anthropic,
        /// openrouter, ollama, gemini.
        backend: String,
    },
    /// Switch STT + LLM to a paired cloud preset. Known presets:
    /// groq, cerebras, openai, anthropic, openrouter, deepgram,
    /// assemblyai.
    Cloud { provider: String },
    /// Switch to local STT (whisper) and disable LLM cleanup.
    Local,
    /// Print the currently active STT/LLM and the running daemon's view.
    Show,
}

#[derive(Debug, Subcommand)]
pub enum KeysCmd {
    /// List all API keys in secrets.toml (values are masked).
    List,
    /// Add or replace an API key. Prompts on stdin if --value isn't given.
    Add {
        /// Key name, e.g. `GROQ_API_KEY`.
        name: String,
        /// Inline value (avoid in shell history; prefer the prompt).
        #[arg(long)]
        value: Option<String>,
    },
    /// Remove a key from secrets.toml.
    Remove { name: String },
    /// Probe each configured provider's API for reachability.
    Check,
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
        Some(Cmd::TestInject {
            text,
            no_inject,
            no_clipboard,
            shortcut,
        }) => {
            test_inject_cmd(&text, no_inject, no_clipboard, shortcut.as_deref());
            Ok(())
        }
        Some(Cmd::Hwprobe { json }) => {
            hwprobe_cmd(&paths, json);
            Ok(())
        }
        Some(Cmd::Use { action }) => use_cmd(&paths, action).await,
        Some(Cmd::Keys { action }) => keys_cmd(&paths, action).await,
        Some(Cmd::TestOverlay) => {
            test_overlay_cmd();
            Ok(())
        }
        Some(Cmd::Record {
            no_inject,
            max_seconds,
            stt,
            llm,
            live,
        }) => {
            record_cmd(
                &paths,
                no_inject,
                max_seconds,
                stt.as_deref(),
                llm.as_deref(),
                live,
            )
            .await
        }
        Some(Cmd::Transcribe {
            path,
            no_llm,
            stt,
            llm,
        }) => transcribe_cmd(&paths, &path, no_llm, stt.as_deref(), llm.as_deref()).await,
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
        Some(Cmd::Update {
            check,
            yes,
            dry_run,
            channel,
            no_restart,
            bin_dir,
        }) => update_cmd(check, yes, dry_run, &channel, no_restart, bin_dir).await,
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
async fn record_cmd(
    paths: &Paths,
    no_inject: bool,
    max_seconds: u64,
    stt_override: Option<&str>,
    llm_override: Option<&str>,
    live: bool,
) -> Result<()> {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use fono_audio::{AudioCapture, CaptureConfig};
    use fono_core::{Config, Secrets};

    let mut config = Config::load(&paths.config_file())?;
    apply_backend_overrides(&mut config, stt_override, llm_override)?;
    let config = Arc::new(config);
    let secrets = Secrets::load(&paths.secrets_file())?;

    if live {
        return record_cmd_live(paths, &config, &secrets, max_seconds, no_inject).await;
    }

    let cap_cfg = CaptureConfig {
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

    let stt = fono_stt::build_stt(
        &config.stt,
        &config.general,
        &secrets,
        &paths.whisper_models_dir(),
    )?;
    let llm = fono_llm::build_llm(&config.llm, &secrets, &paths.llm_models_dir())?;

    eprintln!(
        "fono record: captured {} samples ({} ms); running STT…",
        pcm.len(),
        elapsed.as_millis()
    );
    let lang = config.general.language_override();
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
async fn transcribe_cmd(
    paths: &Paths,
    wav: &std::path::Path,
    no_llm: bool,
    stt_override: Option<&str>,
    llm_override: Option<&str>,
) -> Result<()> {
    use fono_core::{Config, Secrets};

    let mut config = Config::load(&paths.config_file())?;
    apply_backend_overrides(&mut config, stt_override, llm_override)?;
    let secrets = Secrets::load(&paths.secrets_file())?;
    let (pcm, sample_rate) =
        read_wav_mono_f32(wav).with_context(|| format!("read wav {}", wav.display()))?;
    let stt = fono_stt::build_stt(
        &config.stt,
        &config.general,
        &secrets,
        &paths.whisper_models_dir(),
    )?;
    let llm = if no_llm {
        None
    } else {
        fono_llm::build_llm(&config.llm, &secrets, &paths.llm_models_dir())?
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

// ---------------------------------------------------------------------
// `fono use …` — switch active STT / LLM (provider-switching plan S4).
// ---------------------------------------------------------------------

/// Mutate `config` so that future `build_stt` / `build_llm` calls pick
/// up the requested backend. Used both by `fono use` (persisted) and
/// the per-call `--stt` / `--llm` overrides on `record` / `transcribe`
/// (provider-switching plan task S6).
fn apply_backend_overrides(cfg: &mut Config, stt: Option<&str>, llm: Option<&str>) -> Result<()> {
    use fono_core::providers::{parse_llm_backend, parse_stt_backend};
    if let Some(s) = stt {
        let backend = parse_stt_backend(s).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown STT backend {s:?}; valid: local, groq, openai, deepgram, \
                 assemblyai, cartesia, azure, speechmatics, google, nemotron"
            )
        })?;
        set_active_stt(cfg, backend);
    }
    if let Some(l) = llm {
        let backend = parse_llm_backend(l).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown LLM backend {l:?}; valid: none, local, cerebras, groq, \
                 openai, anthropic, openrouter, ollama, gemini"
            )
        })?;
        set_active_llm(cfg, backend);
    }
    Ok(())
}

/// Atomically swap the active STT backend in the config struct without
/// touching unrelated fields. Provider-switching plan task S5 — never
/// drop user customisations (hotkeys, prompts, history settings).
pub fn set_active_stt(cfg: &mut Config, backend: fono_core::config::SttBackend) {
    cfg.stt.backend = backend;
    // Clear stale cloud sub-block so the factory falls through to the
    // canonical env-var. Local STT keeps cfg.stt.local.* intact.
    cfg.stt.cloud = None;
}

/// Atomically swap the active LLM backend. Enables/disables cleanup as
/// appropriate (None → disabled, anything else → enabled).
pub fn set_active_llm(cfg: &mut Config, backend: fono_core::config::LlmBackend) {
    use fono_core::config::LlmBackend;
    let none = matches!(backend, LlmBackend::None);
    cfg.llm.backend = backend;
    cfg.llm.enabled = !none;
    cfg.llm.cloud = None;
}

async fn use_cmd(paths: &Paths, action: UseCmd) -> Result<()> {
    use fono_core::config::{LlmBackend, SttBackend};
    use fono_core::providers::{
        cloud_pair, llm_backend_str, parse_llm_backend, parse_stt_backend, stt_backend_str,
    };

    let path = paths.config_file();
    let mut cfg = Config::load(&path)?;
    let summary: String = match action {
        UseCmd::Stt { backend } => {
            let b = parse_stt_backend(&backend).ok_or_else(|| {
                anyhow::anyhow!("unknown STT backend {backend:?}; try local, groq, openai, …")
            })?;
            set_active_stt(&mut cfg, b.clone());
            cfg.save(&path)?;
            format!("stt = {}", stt_backend_str(&b))
        }
        UseCmd::Llm { backend } => {
            let b = parse_llm_backend(&backend).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown LLM backend {backend:?}; try none, cerebras, groq, openai, …"
                )
            })?;
            set_active_llm(&mut cfg, b.clone());
            cfg.save(&path)?;
            format!("llm = {}", llm_backend_str(&b))
        }
        UseCmd::Cloud { provider } => {
            let (s, l) = cloud_pair(&provider).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown cloud preset {provider:?}; try groq, cerebras, openai, anthropic, \
                     openrouter, deepgram, assemblyai"
                )
            })?;
            set_active_stt(&mut cfg, s.clone());
            set_active_llm(&mut cfg, l.clone());
            cfg.save(&path)?;
            format!(
                "cloud preset {provider}: stt = {}, llm = {}",
                stt_backend_str(&s),
                llm_backend_str(&l),
            )
        }
        UseCmd::Local => {
            set_active_stt(&mut cfg, SttBackend::Local);
            set_active_llm(&mut cfg, LlmBackend::None);
            cfg.save(&path)?;
            "local: stt = local (whisper), llm = none".to_string()
        }
        UseCmd::Show => {
            print_show(paths, &cfg).await;
            return Ok(());
        }
    };

    println!("{summary}");

    // Hot-reload the running daemon (provider-switching plan S11). When
    // the daemon is not running this is a no-op with a friendly hint.
    match fono_ipc::request(&paths.ipc_socket(), &fono_ipc::Request::Reload).await {
        Ok(fono_ipc::Response::Text(t)) => println!("daemon: {t}"),
        Ok(fono_ipc::Response::Ok) => println!("daemon: reloaded"),
        Ok(fono_ipc::Response::Error(e)) => println!("daemon reload error: {e}"),
        Err(_) => println!("daemon: not running (config saved; will apply on next start)"),
    }
    Ok(())
}

async fn print_show(paths: &Paths, cfg: &Config) {
    use fono_core::providers::{llm_backend_str, stt_backend_str};
    println!("config: {}", paths.config_file().display());
    println!("  stt  : {}", stt_backend_str(&cfg.stt.backend));
    println!(
        "  llm  : {}{}",
        llm_backend_str(&cfg.llm.backend),
        if cfg.llm.enabled { "" } else { " (disabled)" }
    );
    match fono_ipc::request(&paths.ipc_socket(), &fono_ipc::Request::Status).await {
        Ok(fono_ipc::Response::Text(t)) => println!("daemon: {t}"),
        Ok(_) => println!("daemon: running"),
        Err(_) => println!("daemon: not running"),
    }
}

// ---------------------------------------------------------------------
// `fono keys …` — manage secrets.toml (provider-switching plan S7).
// ---------------------------------------------------------------------

async fn keys_cmd(paths: &Paths, action: KeysCmd) -> Result<()> {
    let secrets_path = paths.secrets_file();
    match action {
        KeysCmd::List => {
            let secrets = Secrets::load(&secrets_path)?;
            print_keys_list(&secrets);
        }
        KeysCmd::Add { name, value } => {
            let mut secrets = Secrets::load(&secrets_path).unwrap_or_default();
            let val = match value {
                Some(v) => v,
                None => prompt_for_secret(&name)?,
            };
            secrets.insert(&name, val);
            secrets.save(&secrets_path)?;
            println!("added {name} → {}", secrets_path.display());
            // Hot-reload so the daemon picks up the new key.
            let _ = fono_ipc::request(&paths.ipc_socket(), &fono_ipc::Request::Reload).await;
        }
        KeysCmd::Remove { name } => {
            let mut secrets = Secrets::load(&secrets_path).unwrap_or_default();
            if secrets.keys.remove(&name).is_some() {
                secrets.save(&secrets_path)?;
                println!("removed {name}");
                let _ = fono_ipc::request(&paths.ipc_socket(), &fono_ipc::Request::Reload).await;
            } else {
                println!("not found: {name}");
            }
        }
        KeysCmd::Check => {
            // Lightweight: list which env-keys are present; full
            // network reachability is in `fono doctor`.
            let secrets = Secrets::load(&secrets_path).unwrap_or_default();
            print_keys_list(&secrets);
            println!("\nFor live reachability probes, run `fono doctor`.");
        }
    }
    Ok(())
}

fn print_keys_list(secrets: &Secrets) {
    use fono_core::providers::{
        all_llm_backends, all_stt_backends, llm_key_env, llm_requires_key, stt_key_env,
        stt_requires_key,
    };
    println!("api keys (config + environment):");
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for b in all_stt_backends() {
        if !stt_requires_key(&b) {
            continue;
        }
        seen.insert(stt_key_env(&b).to_string());
    }
    for b in all_llm_backends() {
        if !llm_requires_key(&b) {
            continue;
        }
        seen.insert(llm_key_env(&b).to_string());
    }
    for name in seen {
        let from_secrets = secrets.keys.get(&name).cloned();
        let from_env = std::env::var(&name).ok();
        let v = from_secrets.or(from_env);
        match v {
            Some(val) => println!("  {name:<24} = {}", mask(&val)),
            None => println!("  {name:<24} = (unset)"),
        }
    }
    // Also print any extra keys not in the canonical set (e.g.,
    // user-added entries).
    for (k, v) in &secrets.keys {
        if !is_canonical_key(k) {
            println!("  {k:<24} = {} (custom)", mask(v));
        }
    }
}

fn is_canonical_key(name: &str) -> bool {
    use fono_core::providers::{all_llm_backends, all_stt_backends, llm_key_env, stt_key_env};
    all_stt_backends().iter().any(|b| stt_key_env(b) == name)
        || all_llm_backends().iter().any(|b| llm_key_env(b) == name)
}

fn mask(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "(empty)".to_string();
    }
    let n = trimmed.chars().count();
    if n <= 6 {
        return "*".repeat(n);
    }
    let head: String = trimmed.chars().take(3).collect();
    let tail: String = trimmed
        .chars()
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

fn prompt_for_secret(name: &str) -> Result<String> {
    use std::io::Write;
    eprint!("Enter value for {name}: ");
    std::io::stderr().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    let trimmed = s.trim().to_string();
    if trimmed.is_empty() {
        anyhow::bail!("empty value; aborting");
    }
    Ok(trimmed)
}

/// Smoke-test the inject + clipboard delivery path. Bypasses STT/LLM
/// so users can quickly verify whether text can actually reach their
/// focused window or clipboard.
fn test_inject_cmd(text: &str, no_inject: bool, no_clipboard: bool, shortcut: Option<&str>) {
    use std::time::Instant;
    // Apply --shortcut override before any inject path runs so the
    // xtest-paste backend reads the right value when it next
    // synthesizes a key sequence.
    if let Some(s) = shortcut {
        if fono_inject::PasteShortcut::parse(s).is_none() {
            println!(
                "warning: --shortcut={s:?} is not recognised; \
                 xtest-paste will fall back to Shift+Insert"
            );
        }
        std::env::set_var("FONO_PASTE_SHORTCUT", s);
    }
    println!("Fono — test-inject");
    println!("Build: v{}", env!("CARGO_PKG_VERSION"));
    println!(
        "Detected key-injector: {:?}",
        fono_inject::Injector::detect()
    );
    println!(
        "Paste shortcut       : {} (env FONO_PASTE_SHORTCUT={:?})",
        fono_inject::PasteShortcut::from_env_or_default().label(),
        std::env::var("FONO_PASTE_SHORTCUT").ok()
    );
    println!("Text ({} chars): {text:?}", text.chars().count());
    println!();

    if no_inject {
        println!("[1/2] Skipping key injection (--no-inject)");
    } else {
        println!("[1/2] Trying key injection (5s for you to focus a text field)...");
        std::thread::sleep(std::time::Duration::from_secs(5));
        let started = Instant::now();
        match fono_inject::type_text_with_outcome(text) {
            Ok(fono_inject::InjectOutcome::Typed(b)) => {
                println!(
                    "      ✓ typed via {b} in {}ms",
                    started.elapsed().as_millis()
                );
            }
            Ok(fono_inject::InjectOutcome::Clipboard(t)) => {
                println!(
                    "      ↳ key injection failed; fell back to clipboard via {t} \
                     in {}ms (press Ctrl+V to paste)",
                    started.elapsed().as_millis()
                );
            }
            Err(e) => {
                println!("      ✗ inject + clipboard both failed: {e:#}");
            }
        }
    }

    if no_clipboard {
        println!("[2/2] Skipping clipboard copy (--no-clipboard)");
    } else {
        println!("[2/2] Forcing clipboard copy via every available tool...");
        println!(
            "      DISPLAY         = {:?}",
            std::env::var("DISPLAY").ok()
        );
        println!(
            "      WAYLAND_DISPLAY = {:?}",
            std::env::var("WAYLAND_DISPLAY").ok()
        );
        println!(
            "      XDG_SESSION_TYPE= {:?}",
            std::env::var("XDG_SESSION_TYPE").ok()
        );
        let started = Instant::now();
        let attempts = fono_inject::copy_to_clipboard_all(text);
        for a in &attempts {
            let mark = if a.success { "✓" } else { "✗" };
            println!("      {mark} {:<8} [{:<9}] {}", a.tool, a.target, a.detail);
        }
        let any_ok = attempts.iter().any(|a| a.success);
        println!(
            "      {} total in {}ms",
            if any_ok {
                "at least one tool wrote the clipboard"
            } else {
                "NO tool wrote the clipboard"
            },
            started.elapsed().as_millis()
        );
        if let Some(readback) = readback_clipboard() {
            let ok = readback.trim() == text;
            println!(
                "      readback: {} ({} bytes via {})",
                if ok { "MATCHES" } else { "DIFFERS" },
                readback.trim().len(),
                if which("wl-paste").is_some() {
                    "wl-paste"
                } else if which("xclip").is_some() {
                    "xclip -o"
                } else {
                    "xsel -o"
                }
            );
        } else {
            println!("      readback: no read-tool installed (install wl-paste or xclip)");
        }
    }
}

fn which(cmd: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .map(|d| std::path::Path::new(d).join(cmd))
        .find(|p| p.is_file())
}

/// Best-effort readback of the X11/Wayland clipboard for verification.
/// Returns None when no read tool is available.
fn readback_clipboard() -> Option<String> {
    use std::process::{Command, Stdio};
    let candidates: &[(&str, &[&str])] = &[
        ("wl-paste", &["--no-newline"]),
        ("xclip", &["-selection", "clipboard", "-o"]),
        ("xsel", &["--clipboard", "--output"]),
    ];
    for (tool, args) in candidates {
        let Ok(out) = Command::new(tool)
            .args(*args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .output()
        else {
            // Tool not installed or spawn failed — try the next one.
            continue;
        };
        if out.status.success() {
            return Some(String::from_utf8_lossy(&out.stdout).to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------
// `fono update` — check and (optionally) self-replace the binary.
// ---------------------------------------------------------------------
#[allow(clippy::fn_params_excessive_bools)]
async fn update_cmd(
    check_only: bool,
    yes: bool,
    dry_run: bool,
    channel: &str,
    no_restart: bool,
    bin_dir: Option<std::path::PathBuf>,
) -> Result<()> {
    use fono_update::{apply_update, check, ApplyOpts, Channel, UpdateStatus};
    use std::io::Write;

    let chan = Channel::parse(channel).ok_or_else(|| {
        anyhow::anyhow!("unknown channel {channel:?}; try `stable` or `prerelease`")
    })?;

    let current = env!("CARGO_PKG_VERSION");
    println!("fono {current} — checking for updates on the {channel} channel…");
    let status = check(current, chan).await;
    match &status {
        UpdateStatus::UpToDate { .. } => {
            println!("up-to-date {current}");
            if check_only {
                std::process::exit(0);
            }
            return Ok(());
        }
        UpdateStatus::CheckFailed { error, .. } => {
            return Err(anyhow::anyhow!("update check failed: {error}"));
        }
        UpdateStatus::Available { info, .. } => {
            println!(
                "available {current}->{} ({} MB)",
                info.version,
                info.asset_size / 1_048_576
            );
            println!("  asset:  {}", info.asset_name);
            println!("  notes:  {}", info.html_url);
            if check_only {
                std::process::exit(1);
            }
        }
    }
    let info = status
        .available()
        .ok_or_else(|| anyhow::anyhow!("no update available"))?;

    if !yes {
        eprint!("Apply update now? [y/N] ");
        std::io::stderr().flush().ok();
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        let confirmed = matches!(s.trim().to_ascii_lowercase().as_str(), "y" | "yes");
        if !confirmed {
            println!("aborted");
            return Ok(());
        }
    }

    let opts = ApplyOpts {
        dry_run,
        // Wave 2 Thread B: --bin-dir <path> overrides the autodetected
        // current_exe(). The is_package_managed refusal in apply_update
        // still fires for system paths regardless of the override.
        target_override: bin_dir.map(|d| d.join("fono")),
    };
    let outcome = apply_update(info, opts).await?;
    println!(
        "installed {} bytes (sha256={}) at {}",
        outcome.bytes,
        outcome.sha256,
        outcome.installed_at.display()
    );
    if let Some(bak) = outcome.backup_at.as_ref() {
        println!("previous binary kept at {}", bak.display());
    }
    if dry_run {
        println!("(dry-run; running binary unchanged)");
        return Ok(());
    }
    if no_restart {
        println!("restart fono to use the new binary");
        return Ok(());
    }
    println!("re-executing into new binary…");
    // `restart_in_place`'s Ok variant is `Infallible`; on success
    // execv replaces the process image and never returns.
    let Err(e) = fono_update::restart_in_place();
    Err(e)
}

// ---------------------------------------------------------------------
// `fono record --live` and `fono test-overlay`. Both are only fully
// functional when the binary was built with `--features interactive`;
// the slim build provides stubs that print a helpful hint.
// Plan v6 / Slice A.
// ---------------------------------------------------------------------

#[cfg(not(feature = "interactive"))]
async fn record_cmd_live(
    _paths: &Paths,
    _config: &fono_core::Config,
    _secrets: &fono_core::Secrets,
    _max_seconds: u64,
    _no_inject: bool,
) -> Result<()> {
    Err(anyhow::anyhow!(
        "live mode requires the `interactive` cargo feature; rebuild with \
         `cargo build --features interactive` (Slice A — see plans/2026-04-27-fono-interactive-v6.md)"
    ))
}

#[cfg(not(feature = "interactive"))]
fn test_overlay_cmd() {
    println!(
        "test-overlay: this binary was built without the `interactive` cargo feature.\n\
         Rebuild with `cargo build --features interactive` to exercise the real \
         winit+softbuffer overlay (plan v6 / Slice A)."
    );
}

#[cfg(feature = "interactive")]
#[allow(clippy::too_many_lines)]
async fn record_cmd_live(
    paths: &Paths,
    config: &fono_core::Config,
    _secrets: &fono_core::Secrets,
    max_seconds: u64,
    no_inject: bool,
) -> Result<()> {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use fono_audio::{AudioCapture, CaptureConfig};
    use fono_core::config::SttBackend;
    use fono_overlay::RealOverlay;
    use fono_stt::StreamingStt;

    use crate::live::{LiveSession, Pump};

    // Slice A is local-first. Cloud streaming lands in Slice B.
    if !matches!(config.stt.backend, SttBackend::Local) {
        return Err(anyhow::anyhow!(
            "live mode in Slice A only supports the local whisper backend; \
             active backend is {:?}. Run `fono use stt local` first, or wait for Slice B \
             cloud streaming.",
            config.stt.backend
        ));
    }

    // Build WhisperLocal directly so we get the StreamingStt impl
    // (the generic `build_stt` factory returns `Arc<dyn SpeechToText>`,
    // which doesn't expose the streaming method).
    let model = &config.stt.local.model;
    let model_path = paths.whisper_models_dir().join(format!("ggml-{model}.bin"));
    if !model_path.exists() {
        return Err(anyhow::anyhow!(
            "local whisper model {model:?} not found at {} — \
             run `fono models install {model}`",
            model_path.display()
        ));
    }
    let stt: Arc<dyn StreamingStt> =
        Arc::new(fono_stt::whisper_local::WhisperLocal::new(model_path));

    // Open the overlay; tolerate failure gracefully (headless / hostile compositor).
    let overlay = match RealOverlay::spawn() {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("fono record --live: overlay unavailable ({e:#}); continuing without it");
            None
        }
    };

    let cap_cfg = CaptureConfig {
        target_sample_rate: 16_000, // streaming pipeline operates at 16 kHz
    };
    let cap = AudioCapture::new(cap_cfg.clone());
    let handle = cap.start().context("start audio capture")?;
    eprintln!(
        "fono record --live: capturing from default input ({} Hz). Press Ctrl-C or wait \
         {max_seconds}s to stop.",
        cap_cfg.target_sample_rate
    );

    // Slice A capture loop: record-then-replay-through-streaming.
    // True real-time push (cpal callback -> AudioFrameStream) lands in
    // Slice B alongside the cpal-callback refactor; the streaming code
    // path is still fully exercised below.
    let started = Instant::now();
    let max = if max_seconds == 0 {
        Duration::from_secs(60 * 60)
    } else {
        Duration::from_secs(max_seconds)
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            eprintln!("fono record --live: stopped by Ctrl-C");
        }
        () = tokio::time::sleep(max) => {
            eprintln!("fono record --live: hit {max_seconds}s timeout");
        }
    }
    let elapsed = started.elapsed();
    let pcm = {
        let buf = handle.buffer.lock().expect("buffer mutex");
        buf.samples().to_vec()
    };
    drop(handle);
    eprintln!(
        "fono record --live: captured {} samples ({} ms); running streaming STT…",
        pcm.len(),
        elapsed.as_millis()
    );

    // Replay the captured PCM through the streaming pipeline in
    // ~30 ms chunks so the preview/finalize lanes still exercise their
    // full code path.
    //
    // The receiver is taken from the pump *before* the run task is
    // spawned and *before* the first push, so the broadcast channel
    // has a live subscriber for every frame and nothing is lost.
    let mut pump = Pump::new(fono_audio::StreamConfig::default());
    let frame_rx = pump.take_receiver()?;
    let session =
        LiveSession::new(Arc::clone(&stt), cap_cfg.target_sample_rate).with_language(match config
            .general
            .languages
            .as_slice()
        {
            [] => None,
            [single] => Some(single.clone()),
            _ => None,
        });
    let session = if let Some(o) = overlay.as_ref() {
        session.with_overlay(o.clone())
    } else {
        session
    };

    let task = tokio::spawn(session.run(frame_rx, fono_core::QualityFloor::Max));

    let chunk = (cap_cfg.target_sample_rate as usize / 1000) * 30; // 30 ms
    for window in pcm.chunks(chunk.max(1)) {
        pump.push(window);
        // Yield so the run task can drain the broadcast buffer between
        // pushes; otherwise a long replay can outpace the channel
        // capacity (default 64 frames).
        tokio::task::yield_now().await;
    }
    pump.finish();
    drop(pump);

    let transcript = task.await??;

    if let Some(o) = overlay.as_ref() {
        o.shutdown();
    }

    let final_text = transcript.committed.trim().to_string();
    if final_text.is_empty() {
        eprintln!("fono record --live: streaming STT returned empty text");
        return Ok(());
    }
    println!("{final_text}");
    if !no_inject {
        if let Err(e) = fono_inject::type_text(&final_text) {
            eprintln!("fono record --live: inject failed: {e:#}");
        }
    }
    Ok(())
}

#[cfg(feature = "interactive")]
fn test_overlay_cmd() {
    use std::time::Duration;

    use fono_overlay::{OverlayState, RealOverlay};

    println!("fono test-overlay: spawning real overlay window…");
    let handle = match RealOverlay::spawn() {
        Ok(h) => h,
        Err(e) => {
            println!("test-overlay: overlay failed to spawn: {e:#}");
            return;
        }
    };
    println!("[1/3] Recording (red), 1s");
    handle.set_state(OverlayState::Recording { db: -20 });
    std::thread::sleep(Duration::from_secs(1));
    println!("[2/3] LiveDictating with sample text (blue), 1s");
    handle.set_state(OverlayState::LiveDictating);
    handle.update_text("Hello from fono live mode");
    std::thread::sleep(Duration::from_secs(1));
    println!("[3/3] Processing (amber), 1s");
    handle.set_state(OverlayState::Processing);
    std::thread::sleep(Duration::from_secs(1));
    println!("test-overlay: shutting down");
    handle.shutdown();
}
