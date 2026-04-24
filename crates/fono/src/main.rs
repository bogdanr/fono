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
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("FONO_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let args = cli::Cli::parse();
    cli::run(args).await
}
