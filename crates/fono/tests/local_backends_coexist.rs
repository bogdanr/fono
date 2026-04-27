// SPDX-License-Identifier: GPL-3.0-only
//! Smoke test: confirm that `whisper-rs` and `llama-cpp-2` can be linked
//! into the same binary and instantiated in the same process without
//! segfaulting on the shared (and de-duplicated by
//! `-Wl,--allow-multiple-definition`) `ggml` symbols.
//!
//! Both crates statically vendor a copy of `ggml`. Our workspace-level
//! `.cargo/config.toml` tells the GNU/musl linker to keep the first
//! definition and discard the rest. This test exists to catch the day
//! when an upstream bump makes the two `ggml` copies ABI-incompatible —
//! at that point we'd fall back to the longer-term shared-`ggml`
//! refactor outlined in
//! `plans/2026-04-27-shared-ggml-static-binary-v1.md`.
//!
//! We only construct the cheapest possible objects from each backend
//! and immediately drop them. We do not load any actual models — that
//! would require shipping a >100 MB GGUF in the test fixtures. The
//! test passes if the binary links and the lazy `ggml_init` /
//! `LlamaBackend::init` calls do not segfault.

#![cfg(all(feature = "local-models", feature = "llama-local"))]

use fono_llm::llama_local::LlamaLocal;
use fono_stt::whisper_local::WhisperLocal;

#[test]
fn whisper_and_llama_coexist_in_one_process() {
    // Both factories are lazy: constructing the wrapper does NOT load
    // a model. The model file is only read on the first `transcribe()`
    // / `format()` call. That gives us a process-level smoke test that
    // exercises both crates' static initialisers (which is where any
    // residual ggml symbol clash would manifest as a segfault) without
    // needing a multi-GB model fixture.
    let whisper = WhisperLocal::new("/nonexistent/whisper.bin");
    let llama = LlamaLocal::new("/nonexistent/llama.gguf", 2048);

    // Force them into different stack frames so neither is optimised
    // away by LTO.
    drop(whisper);
    drop(llama);
}
