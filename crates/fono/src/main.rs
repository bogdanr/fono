// SPDX-License-Identifier: GPL-3.0-only
//! `fono` — daemon + CLI entry point. Phase 8 of
//! `docs/plans/2026-04-24-fono-design-v1.md`.

// On Windows, build the shipped tray daemon as a GUI-subsystem binary
// so a double-click or login-autostart launch shows ONLY the tray icon
// — no stray console window (which, as a console-subsystem exe, would
// otherwise pop up and, if closed, kill the daemon). This mirrors the
// Linux experience where launching the daemon never spawns a terminal.
// CLI subcommands run from an existing terminal still print, because
// `attach_parent_console()` re-attaches to the parent console at
// startup. Gated on `tray`: a tray-less Windows build keeps the console
// subsystem so it is never silently invisible.
#![cfg_attr(all(target_os = "windows", feature = "tray"), windows_subsystem = "windows")]

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
    //
    // It also MUST run before `attach_parent_console()` below: the
    // probe child's stdout is a pipe the parent captures, and
    // re-pointing its std handles at a console would break that
    // capture. The probe path exits the process, so it never reaches
    // the console attach.
    fono_core::vulkan_probe::run_subprocess_probe_if_requested();

    // GUI-subsystem binaries start with no std handles; if we were
    // launched from a terminal, reconnect to it so CLI output appears.
    // No-op (and windowless) when there is no parent console.
    #[cfg(all(target_os = "windows", feature = "tray"))]
    attach_parent_console();

    // Parse + tracing before the runtime: on macOS the daemon path
    // needs the parsed command to decide whether the main thread must
    // become the AppKit event pump instead of the tokio driver.
    let args = cli::Cli::parse();
    init_tracing(args.verbosity());

    // macOS daemon in a graphical session: AppKit (NSStatusItem menus,
    // and later the NSPanel overlay) is main-thread-only and needs a
    // running NSApplication event loop, so the roles swap — the main
    // thread parks in the AppKit pump and the daemon runs on a second
    // thread with its own tokio runtime. Subcommands and headless
    // launches (SSH, launchd without Aqua) keep the plain path: no
    // NSApplication, byte-identical behaviour to before.
    #[cfg(all(target_os = "macos", feature = "tray"))]
    if args.cmd.is_none() && fono::is_graphical_session() {
        return macos_daemon_main(args);
    }

    // Now build the runtime for the real entry point. Mirrors what
    // `#[tokio::main]` would have produced.
    run_on_worker(args)
}

/// Build the multi-threaded tokio runtime and drive the CLI/daemon
/// future to completion.
fn run_runtime(args: cli::Cli) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(cli::run(args))
}

/// Run the entry point, choosing a stack large enough for daemon init.
///
/// On Windows the default **main-thread** stack is 1 MiB (Linux and
/// macOS give 8 MiB). The synchronous parts of daemon startup plus the
/// top-level future driven by `block_on` all run on this thread, which
/// overflows the 1 MiB budget (observed as `thread 'main' has
/// overflowed its stack` during daemon boot). So on Windows we run the
/// whole entry point on a dedicated thread with a generous stack —
/// mirroring the macOS daemon-thread pattern in [`macos_daemon_main`].
/// This applies to every invocation (subcommands are short-lived, so
/// the extra thread is harmless). Linux/macOS keep the plain path, so
/// their behaviour is byte-identical.
#[cfg(target_os = "windows")]
fn run_on_worker(args: cli::Cli) -> Result<()> {
    let worker = std::thread::Builder::new()
        .name("fono-main".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || run_runtime(args))?;
    match worker.join() {
        Ok(result) => result,
        Err(_) => anyhow::bail!("fono main worker thread panicked"),
    }
}

/// Re-attach the GUI-subsystem binary to the console it was launched
/// from, if any, so CLI subcommands print to that terminal.
///
/// The shipped Windows tray build is compiled `windows_subsystem =
/// "windows"` (see the crate attribute) so a double-click / autostart
/// launch never opens a console window — the daemon lives only in the
/// tray, like on Linux. The side effect is that a GUI-subsystem process
/// starts with no valid standard handles, so `println!` / `eprintln!`
/// (and the tracing writer) would go nowhere even when the user *did*
/// launch `fono doctor` from PowerShell. This attaches to the parent
/// process's console and repoints stdout/stderr/stdin at it. Every call
/// fails harmlessly when there is no parent console (Explorer /
/// login autostart), leaving the process cleanly windowless.
#[cfg(all(target_os = "windows", feature = "tray"))]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{
        AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE,
        STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    // `CreateFileW` is declared directly here rather than imported from
    // windows-sys: its binding is gated behind a shifting union of module
    // features (Storage_FileSystem + Security for the SECURITY_ATTRIBUTES
    // parameter) that drift between windows-sys releases. A raw kernel32
    // extern is stable across versions and keeps the feature list minimal.
    // `HANDLE` is `*mut c_void`, matching windows-sys so the pointers pass
    // straight into `SetStdHandle`.
    type Handle = *mut core::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            lpfilename: *const u16,
            dwdesiredaccess: u32,
            dwsharemode: u32,
            lpsecurityattributes: *const core::ffi::c_void,
            dwcreationdisposition: u32,
            dwflagsandattributes: u32,
            htemplatefile: Handle,
        ) -> Handle;
    }

    // Win32 numeric constants, spelled out to avoid importing from
    // module paths that drift between windows-sys releases.
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const OPEN_EXISTING: u32 = 3;
    // INVALID_HANDLE_VALUE == (HANDLE)-1.
    let invalid = usize::MAX as *mut core::ffi::c_void;

    // SAFETY: a thin FFI shim. The wide strings are NUL-terminated and
    // owned for the duration of each call; the security-attributes and
    // template-file pointers are null; every returned handle is checked
    // against INVALID_HANDLE_VALUE before it is installed. A std handle is
    // only replaced when it is currently null (a GUI-subsystem launch with
    // no inherited handle) so an explicit `> file` / `| pipe` redirection
    // the user set up is preserved.
    unsafe {
        // Attach to the launching terminal. Returns 0 (fails) when the
        // parent has no console — a double-click / autostart launch —
        // in which case we stay windowless with no std handles, exactly
        // what a background tray daemon wants.
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }

        // A handle already pointing somewhere valid (redirected to a file
        // or pipe by the shell) must be left untouched.
        let unset = |id| {
            let h = GetStdHandle(id);
            h.is_null() || h == invalid
        };

        let conout: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
        let conin: Vec<u16> = "CONIN$\0".encode_utf16().collect();

        if unset(STD_OUTPUT_HANDLE) || unset(STD_ERROR_HANDLE) {
            let out = CreateFileW(
                conout.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                core::ptr::null(),
                OPEN_EXISTING,
                0,
                core::ptr::null_mut(),
            );
            if out != invalid {
                if unset(STD_OUTPUT_HANDLE) {
                    SetStdHandle(STD_OUTPUT_HANDLE, out);
                }
                if unset(STD_ERROR_HANDLE) {
                    SetStdHandle(STD_ERROR_HANDLE, out);
                }
            }
        }

        if unset(STD_INPUT_HANDLE) {
            let inp = CreateFileW(
                conin.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                core::ptr::null(),
                OPEN_EXISTING,
                0,
                core::ptr::null_mut(),
            );
            if inp != invalid {
                SetStdHandle(STD_INPUT_HANDLE, inp);
            }
        }
    }
}

/// Non-Windows entry point: the main thread already has an 8 MiB stack,
/// so drive the runtime directly with no extra thread.
#[cfg(not(target_os = "windows"))]
fn run_on_worker(args: cli::Cli) -> Result<()> {
    run_runtime(args)
}

/// Daemon entry point for macOS graphical sessions: install the tray's
/// main-thread job pump, run the daemon on a dedicated thread, and park
/// the real main thread in the AppKit run loop until the daemon exits.
#[cfg(all(target_os = "macos", feature = "tray"))]
fn macos_daemon_main(args: cli::Cli) -> Result<()> {
    let Some(jobs) = fono_tray::install_main_pump() else {
        anyhow::bail!("AppKit main-thread pump installed twice — daemon started twice in-process");
    };

    // Hand the overlay's NSPanel backend a way to reach the same
    // pump. fono-overlay deliberately doesn't depend on fono-tray;
    // the binary owns this one-line wiring instead.
    #[cfg(feature = "interactive")]
    fono_overlay::backends::macos::set_main_thread_dispatcher(Box::new(fono_tray::dispatch_main));

    let daemon = std::thread::Builder::new().name("fono-daemon".into()).spawn(move || {
        // Stop the pump when the daemon finishes — including on panic
        // (Drop runs during unwind), so the main thread never hangs in
        // `NSApplication::run` after the daemon is gone.
        struct StopPumpOnDrop;
        impl Drop for StopPumpOnDrop {
            fn drop(&mut self) {
                fono_tray::stop_main_pump();
            }
        }
        let _stop = StopPumpOnDrop;
        tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(cli::run(args))
    })?;

    fono_tray::run_main_pump(jobs);
    match daemon.join() {
        Ok(result) => result,
        Err(_) => anyhow::bail!("daemon thread panicked"),
    }
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
    let ansi = fono::session::log_color_enabled();
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(ansi)
        .with_ansi_sanitization(!ansi)
        .with_target(verbosity.is_trace())
        .with_file(verbosity.is_trace())
        .with_line_number(verbosity.is_trace());
    tracing_subscriber::registry().with(fmt_layer).with(targets).init();
}
