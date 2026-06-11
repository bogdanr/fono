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
//! It ALSO guards the shared-`LlamaBackend` invariant: `fono-polish`
//! (cleanup) and `fono-assistant` (voice chat) both embed llama.cpp,
//! and `LlamaBackend::init()` may run at most once per process. They
//! now share the single `fono_core::llama_backend::backend()`
//! singleton; if a future refactor reintroduces a per-crate
//! `LlamaBackend::init()`, the second init would panic at runtime
//! ("llama-local mutex poisoned" on whichever path loads second). The
//! `shared_llama_backend_inits_once` test below exercises that init
//! path directly and confirms repeated calls reuse one handle.
//!
//! We only construct the cheapest possible objects from each backend
//! and immediately drop them. We do not load any actual models — that
//! would require shipping a >100 MB GGUF in the test fixtures. The
//! test passes if the binary links and the lazy `ggml_init` /
//! `LlamaBackend::init` calls do not segfault.

#![cfg(all(feature = "local-models", feature = "llama-local"))]

use fono_assistant::llama_local::LlamaLocalAssistant;
use fono_polish::llama_local::LlamaLocal;
use fono_stt::whisper_local::WhisperLocal;

#[test]
fn whisper_and_llama_coexist_in_one_process() {
    // All three factories are lazy: constructing the wrapper does NOT
    // load a model. The model file is only read on the first
    // `transcribe()` / `format()` / `stream()` call. That gives us a
    // process-level smoke test that exercises both crates' static
    // initialisers (which is where any residual ggml symbol clash would
    // manifest as a segfault) without needing a multi-GB model fixture.
    let whisper = WhisperLocal::new("/nonexistent/whisper.bin");
    let polish = LlamaLocal::new("/nonexistent/polish.gguf", 2048);
    let assistant = LlamaLocalAssistant::new("/nonexistent/assistant.gguf", 2048);

    // Force them into different stack frames so none is optimised
    // away by LTO.
    drop(whisper);
    drop(polish);
    drop(assistant);
}

#[test]
fn shared_llama_backend_inits_once() {
    // Both the polish and assistant embedded paths resolve their
    // backend through this one singleton. Calling it repeatedly must
    // not re-init llama.cpp (a second `LlamaBackend::init()` returns
    // `BackendAlreadyInitialized`, and the old per-crate `.expect()`
    // panicked, poisoning the loser's model mutex). A `&'static`
    // handle returned twice with the same address proves single init.
    let first = fono_core::llama_backend::backend();
    let second = fono_core::llama_backend::backend();
    assert!(std::ptr::eq(first, second), "backend() must return one shared singleton");
}
