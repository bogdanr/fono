// SPDX-License-Identifier: GPL-3.0-only
//! `fono` — daemon + CLI entry point. Phase 8 of
//! `docs/plans/2026-04-24-fono-design-v1.md`.

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

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
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let args = cli::Cli::parse();
    init_tracing(args.verbosity());
    cli::run(args).await
}

/// Initialise the global `tracing` subscriber.
///
/// Precedence (highest first):
///  1. `FONO_LOG` env var (`tracing-subscriber` `Targets` syntax:
///     comma-separated `target=level` directives, optionally with a
///     bare default level — e.g. `info,whisper_rs=warn`).
///  2. `--debug` / `-v` / `-vv` CLI flags.
///  3. Default = `info`.
///
/// Regardless of the above, a small set of noisy third-party warnings
/// is silenced unconditionally — see `BASELINE_QUIET` below. User
/// directives are parsed *after* the baseline, so the user's own
/// directive for the same target wins (the parser keeps the last
/// occurrence of duplicate targets).
///
/// Historically this routed through `tracing-subscriber`'s
/// `EnvFilter`, which pulls a full regex engine + DFA (`regex_automata`,
/// `regex_syntax`, `aho_corasick`) into the binary — ~1.0 MiB of
/// `.text` for a feature we never use (spans, regex targets,
/// per-field filtering). `Targets` provides the directive syntax we
/// actually rely on (`target=level`, with longest-prefix match) for
/// almost no code.
fn init_tracing(verbosity: cli::Verbosity) {
    /// Third-party log lines that fire on every startup with no
    /// actionable signal. Silenced so they only appear when something
    /// is actually wrong.
    ///
    /// * `winit::platform_impl::linux::x11::xdisplay` emits
    ///   "error setting XSETTINGS; Xft options won't reload
    ///   automatically" on any X session that doesn't run an XSETTINGS
    ///   manager (most minimal WM setups, including NimbleX). The
    ///   message is misleading — Xft *static* options still load fine;
    ///   only live re-loading on theme change is affected, which is
    ///   irrelevant for fono's overlay.
    /// * `winit::platform_impl::linux::x11::window` emits an `info!`
    ///   line on every overlay window creation reporting the scale
    ///   factor it guessed from the cursor's current monitor (e.g.
    ///   "Guessed window scale factor: 1.25"). winit's X11 backend
    ///   has to guess because X11 lacks a per-window scale-factor
    ///   protocol, and it self-corrects via `ScaleFactorChanged`
    ///   once the window maps — so the initial guess is purely
    ///   informational. Demoting to `warn` keeps any real warnings
    ///   from this module visible without printing the guess on
    ///   every daemon start.
    const BASELINE_QUIET: &[&str] = &[
        "winit::platform_impl::linux::x11::xdisplay=error",
        "winit::platform_impl::linux::x11::window=warn",
    ];

    let user_or_default = std::env::var("FONO_LOG")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| verbosity.as_filter().to_string());
    // BASELINE_QUIET first so user directives (parsed after) win on
    // exact-target collisions; for non-colliding more-specific user
    // targets, `Targets` resolves by longest-prefix match anyway.
    let combined = format!("{},{}", BASELINE_QUIET.join(","), user_or_default);
    let targets: Targets =
        combined.parse().unwrap_or_else(|_| Targets::new().with_default(LevelFilter::INFO));

    // Tracing must go to **stderr**, never stdout. The `fono mcp serve`
    // subcommand uses stdout exclusively for JSON-RPC frames; any log line
    // that leaks onto stdout corrupts the MCP transport and causes the
    // client (e.g. Forge's rmcp) to tear down the connection on the first
    // parse error. Stderr is also the convention for every other CLI/TUI
    // subcommand, so this default is universally correct.
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(true)
        .with_target(verbosity.is_trace())
        .with_file(verbosity.is_trace())
        .with_line_number(verbosity.is_trace());
    tracing_subscriber::registry().with(fmt_layer).with(targets).init();
}
