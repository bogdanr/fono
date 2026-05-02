// SPDX-License-Identifier: GPL-3.0-only
//! `fono` — daemon + CLI entry point. Phase 8 of
//! `docs/plans/2026-04-24-fono-design-v1.md`.

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use fono::cli;

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

// musl + libgomp link shim. The libgomp `.a` shipped by
// `messense/rust-musl-cross` references `memalign(3)`, a glibc-only
// allocator; musl ships only `aligned_alloc`/`posix_memalign`. Provide
// `memalign` as a thin wrapper so the static-musl link succeeds.
// Compiled into the binary on every musl target — harmless if libgomp
// isn't linked, and resolves the symbol if it is.
#[cfg(all(target_os = "linux", target_env = "musl"))]
#[no_mangle]
pub unsafe extern "C" fn memalign(alignment: usize, size: usize) -> *mut core::ffi::c_void {
    extern "C" {
        fn posix_memalign(
            memptr: *mut *mut core::ffi::c_void,
            alignment: usize,
            size: usize,
        ) -> i32;
    }
    let mut ptr: *mut core::ffi::c_void = core::ptr::null_mut();
    // SAFETY: forwarding the contract of memalign to posix_memalign.
    if unsafe { posix_memalign(&mut ptr, alignment, size) } != 0 {
        return core::ptr::null_mut();
    }
    ptr
}

// musl + libgomp link shim, part 2. libgomp's env parsing calls
// `secure_getenv(3)`, a glibc extension. fono is a user-level CLI never
// run setuid/setgid, so plain `getenv` is semantically equivalent.
#[cfg(all(target_os = "linux", target_env = "musl"))]
#[no_mangle]
pub unsafe extern "C" fn secure_getenv(name: *const core::ffi::c_char) -> *mut core::ffi::c_char {
    extern "C" {
        fn getenv(name: *const core::ffi::c_char) -> *mut core::ffi::c_char;
    }
    // SAFETY: caller passes a NUL-terminated C string; we forward as-is.
    unsafe { getenv(name) }
}
