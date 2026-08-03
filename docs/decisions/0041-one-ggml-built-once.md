# ADR 0041 — one ggml, built once

## Status

Accepted 2026-08-04. Supersedes ADR 0018.

## Context

`whisper-rs-sys` and `llama-cpp-sys-2` each statically vendor their own
copy of ggml. ADR 0018 let both into the binary and told the linker to
keep one and discard the other —
`-Wl,--allow-multiple-definition` on GNU ld, `/FORCE:MULTIPLE` on MSVC —
on the grounds that both copies come from the same upstream and are
therefore interchangeable.

They are not. `whisper-rs-sys` 0.15.0 vendors a ggml with
`GGML_TYPE_COUNT` 40; the llama fork vendors one with 42, and their
`ggml-backend.h` differ by roughly 100 lines. `GGML_TYPE_COUNT` sizes
ggml's type-traits table, so half the binary indexes a 42-entry table
through code compiled against 40 entries.

Which copy survives is decided by link order, which varies with the
machine. The same commit, feature set and compiler produced a working
binary on a developer machine (4264 `LNK4006` duplicate-symbol warnings,
whisper's copy surviving) and `STATUS_HEAP_CORRUPTION` on the CI runner,
where the test does nothing but construct both backends and drop them.
Linux was not safer, only luckier: with the flags removed, `rust-lld`
refuses the link outright with `duplicate symbol: ggml_backend_buft_name`
and others.

## Decision

Build ggml once. `llama-cpp-sys-2` already installs a ggml CMake package;
a patch to it prints where (`cargo:ggml_cmake_dir`, offered upstream as
utilityai/llama-cpp-rs#1091). `whisper-rs-sys` gains a `llama-ggml`
feature that depends on `llama-cpp-sys-2`, so cargo builds llama first,
reads `DEP_LLAMA_GGML_CMAKE_DIR`, and passes it to whisper.cpp's own
`WHISPER_USE_SYSTEM_GGML` (upstream tazz4843/whisper-rs#260). Both crates
are pinned through `[patch.crates-io]` in `Cargo.toml` until the changes
land upstream.

`--allow-multiple-definition` and `/FORCE:MULTIPLE` are deleted from
`.cargo/config.toml`. If a future dependency reintroduces duplicate
symbols, fix the duplication; do not restore the flags.

## Verification

- `nm target/debug/fono | grep -w ggml_init` returns exactly one entry.
- whisper's `OUT_DIR` holds zero `libggml*.a`; its CMake cache records
  `WHISPER_USE_SYSTEM_GGML:BOOL=ON`.
- The link succeeds with no duplicate-symbol flag on any target.
- `crates/fono/tests/local_backends_coexist.rs` constructs both backends
  in one process. It is now a real guard rather than a coin toss.

## Trade-offs

- Two forks to track instead of one, until both changes land upstream.
  The two sys crates must now be bumped together, because whisper builds
  against llama's ggml.
- A dependency edge from `whisper-rs-sys` to `llama-cpp-sys-2` forces
  build order. A build that wants whisper without llama must leave the
  `llama-ggml` feature off, and then vendors its own ggml as before.

## Surviving artefacts

- `.cargo/config.toml` (the flags, and why they are absent)
- `Cargo.toml` `[patch.crates-io]` (both forks, and when to drop them)
- `crates/fono/tests/local_backends_coexist.rs`
- ADR 0018 (the superseded decision and why it failed)
