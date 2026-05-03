// SPDX-License-Identifier: GPL-3.0-only
//! `fono` — daemon + CLI entry point. Phase 8 of
//! `docs/plans/2026-04-24-fono-design-v1.md`.

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use fono::cli;

fn main() -> Result<()> {
    // Vulkan probe re-exec hook (see `fono_core::vulkan_probe` module
    // docs). When the parent process spawns us with
    // `FONO_INTERNAL_VULKAN_PROBE=1` we run the in-process probe,
    // print the result line on stdout, and exit before clap, tokio,
    // tracing, or anything else gets a chance to start. This isolates
    // the well-known shutdown segfault triggered by Mesa's `vulkan-
    // mesa-lvp` ICD (and some buggy NVIDIA driver builds) into a
    // disposable subprocess so the daemon itself shuts down cleanly.
    //
    // Crucially this MUST run before the tokio runtime is built —
    // otherwise the probe child inherits worker threads it doesn't
    // need and which only widen the shutdown-race surface.
    fono_core::vulkan_probe::run_subprocess_probe_if_requested();

    // Now build the runtime for the real entry point. Mirrors what
    // `#[tokio::main]` would have produced.
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main())
}

async fn async_main() -> Result<()> {
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
///
/// Regardless of the above, a small set of noisy third-party warnings
/// is silenced unconditionally — see `BASELINE_QUIET` below. Users
/// can still re-enable them via `FONO_LOG` (e.g. `FONO_LOG=info,winit=warn`).
fn init_tracing(verbosity: cli::Verbosity) {
    /// Third-party log lines that fire on every startup with no
    /// actionable signal. Silenced at `error` so they only appear
    /// when something is actually wrong.
    ///
    /// * `winit::platform_impl::linux::x11::xdisplay` emits
    ///   "error setting XSETTINGS; Xft options won't reload
    ///   automatically" on any X session that doesn't run an XSETTINGS
    ///   manager (most minimal WM setups, including NimbleX). The
    ///   message is misleading — Xft *static* options still load fine;
    ///   only live re-loading on theme change is affected, which is
    ///   irrelevant for fono's overlay.
    const BASELINE_QUIET: &[&str] = &["winit::platform_impl::linux::x11::xdisplay=error"];

    let default_filter = verbosity.as_filter();
    let mut filter =
        EnvFilter::try_from_env("FONO_LOG").unwrap_or_else(|_| EnvFilter::new(default_filter));
    for d in BASELINE_QUIET {
        if let Ok(dir) = d.parse() {
            filter = filter.add_directive(dir);
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(verbosity.is_trace())
        .with_file(verbosity.is_trace())
        .with_line_number(verbosity.is_trace())
        .init();
}
