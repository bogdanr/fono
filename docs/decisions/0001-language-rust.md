# ADR 0001 — Implementation language: Rust

## Status

Accepted 2026-04-24.

## Context

Fono needs to ship as a lightweight single-binary voice dictation tool. The target is
any x86_64 / aarch64 Linux kernel ≥ 3.2 with **no runtime dependencies** (static musl
build), plus Windows and macOS. The binary must handle global hotkeys, text injection,
and a tray icon under both X11 and Wayland, and it must vendor whisper.cpp and
llama.cpp for local inference.

## Decision

**Rust (stable 1.82+)**, with whisper.cpp and llama.cpp vendored as C++ submodules via
the established Rust FFI crates (`whisper-rs`, `llama-cpp-2`).

## Consequences

### Positive

- Static musl + rustls + bundled SQLite yields a truly portable single binary.
- `cargo` gives one-command contributor onboarding (`cargo build`, `cargo test`).
- Cross-compilation via `cross` is trivial and well supported in CI.
- `cargo-deny` enforces license hygiene from day one (critical for GPL-3.0 hygiene).
- Memory safety reduces the CVE surface for a project handling microphone audio and
  API keys.

### Negative

- Rust compile times are slow (5–15 min for a clean first build, especially with
  whisper.cpp / llama.cpp C++ code paths).
- The Wayland global-hotkey story still depends on compositor portals and will remain
  compositor-specific for the foreseeable future — not a Rust problem, but Rust does
  not magically solve it.

## Alternatives rejected

- **C++** — static linking, TLS, and the cross-DE hotkey/inject/tray story cost
  roughly 3× the release-engineering effort over the project's lifetime. OpenSSL
  static-linking is also legally messy under GPL-3.0 without an explicit linking
  exception.
- **Go** — cgo FFI into whisper.cpp is material (cgo tax, toolchain friction); the
  tray and global-hotkey ecosystem on Linux is weaker than Rust's.
- **Electron / Tauri** — precisely what Fono is replacing. WebKit2GTK on Linux
  (required by Tauri) is the distribution problem that motivates this project in the
  first place.
- **Zig** — ecosystem is too young for tray, hotkey, ORT, and SQLite bindings of the
  quality Fono needs.
