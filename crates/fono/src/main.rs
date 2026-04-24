// SPDX-License-Identifier: GPL-3.0-only
//! `fono` — daemon + CLI entry point. Phase 8 of
//! `docs/plans/2026-04-24-fono-design-v1.md`.

mod cli;
mod daemon;
mod doctor;
mod wizard;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let args = cli::Cli::parse();
    init_tracing(args.verbosity());
    cli::run(args).await
}

/// Initialise the global `tracing` subscriber.
///
/// Precedence (highest first):
///  1. `FONO_LOG` env var (tracing-subscriber EnvFilter syntax).
///  2. `--debug` / `-v` / `-vv` CLI flags.
///  3. Default = `info`.
fn init_tracing(verbosity: cli::Verbosity) {
    let default_filter = verbosity.as_filter();
    let filter =
        EnvFilter::try_from_env("FONO_LOG").unwrap_or_else(|_| EnvFilter::new(default_filter));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(verbosity.is_trace())
        .with_file(verbosity.is_trace())
        .with_line_number(verbosity.is_trace())
        .init();
}
