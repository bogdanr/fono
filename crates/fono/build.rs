// SPDX-License-Identifier: GPL-3.0-only

//! Build script for the `fono` binary.
//!
//! Its sole job today is a Windows-only link fix for the ONNX Runtime
//! (`ort`) static archive pulled in by the `tts-local` / `wakeword-onnx`
//! features.
//!
//! ## Why the second `onnxruntime.lib` reference is needed
//!
//! `ort-sys` links the merged ONNX Runtime archive once
//! (`cargo:rustc-link-lib=static=onnxruntime`). That archive contains
//! circular references *between its own members* — onnxruntime's core
//! objects reference the bundled FetchContent dependencies (onnx,
//! protobuf, abseil, flatbuffers, cpuinfo) and vice-versa.
//!
//! On Linux (`ld`) and macOS (`libtool`) the archive links fine because
//! those linkers resolve archive members iteratively. MSVC's `link.exe`
//! instead pulls archive members on-demand in a **single pass** and never
//! revisits a member it has already scanned, so those intra-archive cycles
//! surface as `LNK1120` "unresolved external symbol" for symbols that are
//! demonstrably present *and* in the archive's linker index.
//!
//! The standard, size-safe MSVC remedy is to place the archive on the link
//! line a **second time**: the extra pass resolves the cycle, and normal
//! dead-strip still runs (unlike `/WHOLEARCHIVE`, which would force every
//! member — including dormant test/interop objects — into the binary and
//! bloat it). Adding the second reference here costs only ~3 MiB in the
//! shipped binary versus the non-TTS build.
//!
//! We emit the bare name `onnxruntime.lib` (not an absolute path) so the
//! fix is independent of how `ort-sys` obtained the library: the linker
//! resolves it against the `/LIBPATH` search directory `ort-sys` already
//! adds via its own `cargo:rustc-link-search`, whether the lib was
//! pre-fetched (`ORT_LIB_LOCATION`) or downloaded by `ort-sys` itself.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let links_ort = std::env::var_os("CARGO_FEATURE_TTS_LOCAL").is_some()
        || std::env::var_os("CARGO_FEATURE_WAKEWORD_ONNX").is_some();

    if target_os == "windows" && links_ort {
        // Second reference to the ONNX Runtime archive, appended after all
        // rlibs (including `ort-sys`), giving `link.exe` a second
        // resolution pass over the merged archive's intra-archive cycles.
        println!("cargo:rustc-link-arg-bins=onnxruntime.lib");
    }
}
