# ADR 0005 — Static binary distribution

## Status

Reconstructed (original lost in filter-branch rewrite; rationale
recovered from `docs/status.md` and plan history, 2026-04-28).

## Context

Fono replaces Tambourine (Tauri + Python) and OpenWhispr (Electron)
with a "single static Rust binary" identity. The promise is `curl … |
sh` install, no Python runtime, no Node, no WebKit, no virtualenv —
the released `target/release/fono` is the ship vehicle. Early
discussions weighed shipping a Python sidecar for STT (the Tambourine
model) against an all-Rust statically-linked binary.

## Decision

Distribute Fono as a single statically-linked Rust binary. STT
(`whisper.cpp` via `whisper-rs`) and LLM cleanup (`llama.cpp` via
`llama-cpp-2`) link into the same ELF. No companion `.so` files. No
sidecar processes. The release matrix produces one ELF per
architecture plus distro-specific archives that wrap the same ELF.

## Consequences

- Install is `curl … | sh` + place one file on `$PATH`.
- The static-link constraint forced the `ggml` symbol-collision
  resolution (see ADR 0018).
- Cross-platform support (macOS / Windows) is gated on the same
  static-link ergonomics holding on those linkers; per ADR 0019 the
  v0.x release matrix is Linux-only.
- Distro packages (`.deb` / `.pkg.tar.zst` / `.txz` / `.lzm`) are
  marginal convenience wrappers, not the primary surface.
