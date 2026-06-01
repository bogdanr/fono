// SPDX-License-Identifier: GPL-3.0-only
//
// Build script for `fono-tts`. Its sole job is to make the static
// `libstdc++` link for the `ort`/`ort-sys` ONNX Runtime work cleanly on
// Linux GNU targets, keeping the shipped binary's `NEEDED` set at the
// four-entry allowlist (ADR 0022, `docs/binary-size.md`).
//
// Background: `ort-sys` links the prebuilt static `libonnxruntime.a`, whose
// C++ symbols must be satisfied by a C++ runtime. By default `ort-sys`
// emits a *dynamic* `-lstdc++`, which pulls `libstdc++.so.6` into `NEEDED`
// and breaks the allowlist. We instead set
// `ORT_CXX_STDLIB=static:-bundle=stdc++` (in `.cargo/config.toml`) so
// `ort-sys` emits `cargo:rustc-link-lib=static:-bundle=stdc++` adjacent to
// its own objects on the final link line. The `-bundle` modifier defers the
// archive to the final binary link (rather than bundling it into the
// `ort-sys` rlib at compile time, where no search path is visible). For
// the static linker to find `libstdc++.a`, rustc needs a search path that
// contains it — that path is host/toolchain specific, so we discover it
// here via `<cxx> --print-file-name=libstdc++.a` and emit it as a
// `rustc-link-search` directive. Cargo propagates link-search directives
// from any build script in the dependency graph to the final binary link,
// so this covers `ort-sys`'s `static=stdc++` even though that crate is
// compiled in isolation.
//
// This only runs when the `tts-local` feature is enabled (the ONNX runtime
// is otherwise never linked) and only on `linux-gnu`, where `static=stdc++`
// and GNU ld archive semantics apply. On every other configuration the
// script is a no-op.

use std::path::Path;
use std::process::Command;

fn main() {
    // Re-run only when the inputs that affect this logic change.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CXX");
    println!("cargo:rerun-if-env-changed=ORT_CXX_STDLIB");

    // Feature gate: the ONNX runtime (and thus the libstdc++ link) only
    // exists when `tts-local` is on. Cargo exposes enabled features to
    // build scripts as `CARGO_FEATURE_<UPPER_SNAKE>`.
    if std::env::var_os("CARGO_FEATURE_TTS_LOCAL").is_none() {
        return;
    }

    // Target gate: `static=stdc++` + the archive search path only make
    // sense on Linux GNU. musl, Windows, and macOS use different C++
    // runtime/linkage models and are handled elsewhere (or not at all).
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "linux" || target_env != "gnu" {
        return;
    }

    // Discover the directory containing `libstdc++.a`. Prefer the
    // toolchain's own answer via `--print-file-name`, honouring an explicit
    // `CXX` override, then falling back to the usual driver names.
    let candidates = [
        std::env::var("CXX").ok(),
        Some("c++".to_string()),
        Some("g++".to_string()),
        Some("gcc".to_string()),
    ];

    for compiler in candidates.into_iter().flatten() {
        let Ok(output) = Command::new(&compiler).arg("--print-file-name=libstdc++.a").output()
        else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let reported = String::from_utf8_lossy(&output.stdout);
        let path = Path::new(reported.trim());
        // `--print-file-name` echoes the bare query back when it cannot
        // resolve the archive, so require both an absolute path and that
        // the file actually exists before trusting it.
        if path.is_absolute() && path.is_file() {
            if let Some(dir) = path.parent() {
                println!("cargo:rustc-link-search=native={}", dir.display());
                return;
            }
        }
    }

    // No `libstdc++.a` found: warn rather than fail. The link will surface
    // a clear "cannot find -lstdc++" error, which is more actionable than a
    // build-script panic, and a contributor on an unusual toolchain can
    // supply the path manually via RUSTFLAGS.
    println!(
        "cargo:warning=fono-tts: could not locate libstdc++.a; the static \
         ONNX Runtime libstdc++ link may fail. Install the C++ static \
         runtime (e.g. libstdc++-static) or add its directory via \
         RUSTFLAGS=\"-L native=/path/to/libstdc++.a/dir\"."
    );
}
