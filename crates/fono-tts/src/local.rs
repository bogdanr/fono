// SPDX-License-Identifier: GPL-3.0-only
//! Local ONNX voice-stack runtime support (feature `tts-local`).
//!
//! Shared `ort` (ONNX Runtime) plumbing that every local voice engine
//! builds on — Piper TTS first (Phase 2 of the v3 plan), then Kokoro,
//! Silero VAD, wake-word, and streaming STT, all on the *same* runtime
//! (ADR 0032).
//!
//! The neural runtime is **statically linked** from a pinned, minimally
//! built `libonnxruntime.a` (`scripts/build-onnxruntime-minimal.sh`,
//! `ORT_LIB_LOCATION`), never the full CDN runtime — see
//! `docs/binary-size.md`. A `--minimal_build` runtime cannot load plain
//! `.onnx`, so engines ship `.ort` flatbuffer models produced by
//! `scripts/gen-ort-models.sh`.

/// The ONNX Runtime C API version this build of `ort` targets.
///
/// Matches the pinned onnxruntime release (`1.<MINOR>.x`); the linked
/// `libonnxruntime.a` must be compatible or `ort` aborts at first use
/// (ADR 0032). Pinned today: onnxruntime 1.24.2 ⇒ API version 24.
pub const RUNTIME_API_VERSION: u32 = ort::MINOR_VERSION;

/// Initialise the process-wide ONNX Runtime environment once.
///
/// Idempotent and racy-safe: the first caller commits the global
/// environment and gets `true`; later calls are no-ops and return `false`.
/// The owning **application** (`fono`) should call this during startup,
/// before any [`ort::session::Session`] is built, so logging/threading
/// options take effect; individual engines must not assume they own the
/// environment.
pub fn ensure_runtime() -> bool {
    ort::init().with_name("fono").commit()
}

#[cfg(test)]
mod tests {
    // These tests link the static `libonnxruntime.a`, so they only build
    // when `tts-local` is enabled *and* `ORT_LIB_LOCATION` points at a
    // compatible archive — i.e. the dedicated voice-stack CI job, never the
    // default `--workspace` build. See plan v3 Phase 1.4.
    #[test]
    fn runtime_links_and_initialises() {
        // Forces the static runtime to load; proves the pinned
        // `libonnxruntime.a` links and `OrtGetApiBase` is reachable. The
        // bool result depends on global-env commit ordering across tests,
        // so we only require that initialising does not panic.
        let _committed = super::ensure_runtime();
        // Surface the targeted API version so a regression in the pin shows
        // up in test output. It is a compile-time constant, so we print
        // rather than assert (clippy rejects asserting on constants).
        println!("ort targets ONNX Runtime API version {}", super::RUNTIME_API_VERSION);
    }
}
