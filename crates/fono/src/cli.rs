// SPDX-License-Identifier: GPL-3.0-only
//! Clap-powered CLI surface + dispatch to daemon / subcommands.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
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
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
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
    Record,
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
            let no_tray = matches!(cli.cmd, Some(Cmd::Daemon { no_tray: true }));
            daemon::run(&paths, no_tray).await
        }
        Some(Cmd::Setup) => wizard::run(&paths).await,
        Some(Cmd::Toggle) => ipc_simple(&paths, Request::Toggle).await,
        Some(Cmd::PasteLast) => ipc_simple(&paths, Request::PasteLast).await,
        Some(Cmd::Doctor) => {
            let report = doctor::report(&paths).await?;
            println!("{report}");
            Ok(())
        }
        Some(Cmd::Record) => {
            // Phase-future: wire up one-shot record+STT+LLM+inject.
            // For Phase 8 we print a clear placeholder rather than silently noop.
            println!(
                "fono record: one-shot capture is scheduled for a follow-up phase \
                 (see docs/plans/2026-04-24-fono-design-v1.md Phase 4-6 integration)."
            );
            Ok(())
        }
        Some(Cmd::History {
            search,
            limit,
            json,
        }) => history_cmd(&paths, search.as_deref(), limit, json),
        Some(Cmd::Config { action }) => config_cmd(&paths, action),
        Some(Cmd::Models { action }) => models_cmd(&paths, action),
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

fn history_cmd(paths: &Paths, search: Option<&str>, limit: usize, json: bool) -> Result<()> {
    let db = fono_core::history::HistoryDb::open(&paths.history_db())?;
    let rows = if let Some(q) = search {
        db.search(q, limit)?
    } else {
        db.recent(limit)?
    };
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

fn models_cmd(paths: &Paths, action: ModelsCmd) -> Result<()> {
    use fono_stt::ModelRegistry;
    match action {
        ModelsCmd::List => {
            for m in ModelRegistry::all() {
                let marker = if paths
                    .whisper_models_dir()
                    .join(format!("{}.bin", m.name))
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
            // Synchronous wrapper: download module is async, so we enter a
            // lightweight runtime. This subcommand is rare (CLI), so a fresh
            // runtime is fine.
            let m = ModelRegistry::get(&name)
                .ok_or_else(|| anyhow::anyhow!("unknown model {name:?}"))?;
            let dest = paths.whisper_models_dir().join(format!("{}.bin", m.name));
            let url = ModelRegistry::url_for(m);
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(fono_download::download(&url, &dest, m.sha256))?;
        }
        ModelsCmd::Remove { name } => {
            let path = paths.whisper_models_dir().join(format!("{name}.bin"));
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
