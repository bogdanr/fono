# Fono — Project Status

Last updated: 2026-04-24

## Current milestone

**v0.1 scaffolding** — Phases 0–8 complete (library + CLI skeleton). Phase 9
packaging + Phase 10 polish remain.

## Phase progress

| Phase | Description | Status |
|-------|-------------|--------|
| 0     | Repo bootstrap + workspace + CI skeleton | ✅ Complete |
| 1     | fono-core: config, secrets, XDG paths, SQLite schema | ✅ Complete |
| 2     | fono-audio: cpal capture + VAD stub + resampler | ✅ Complete |
| 3     | fono-hotkey: global-hotkey parser + hold/toggle FSM | ✅ Complete |
| 4     | fono-stt: trait + WhisperLocal stub + Groq/OpenAI | ✅ Complete |
| 5     | fono-llm: trait + LlamaLocal stub + OpenAI-compat/Anthropic | ✅ Complete |
| 6     | fono-inject: enigo wrapper + focus detection | ✅ Complete |
| 7     | fono-tray + fono-overlay stubs | ✅ Complete |
| 8     | First-run wizard + CLI (`fono run/wizard/doctor/history/models`) | ✅ Complete |
| 9     | Packaging: GitHub release + NimbleX SlackBuild | ⏳ Pending |
| 10    | Docs + v0.1.0 tag | ⏳ Pending |

## What landed in this session (Phases 1–8)

- **fono-core**: XDG paths, atomic TOML `Config`/`Secrets` (0600), SQLite+FTS5
  history with retention cleanup. 11 unit tests.
- **fono-audio**: `cpal` mono f32 capture to ring buffer, linear resampler to
  16 kHz, zero-crossing-rate VAD stub, PipeWire-aware auto-mute hook.
- **fono-hotkey**: accelerator parser (`ctrl+alt+space`, `F9`, etc.), FSM for
  hold vs toggle modes, `global-hotkey` wrapper. 6 tests.
- **fono-stt**: async `SpeechToText` trait, registry, Groq + OpenAI HTTP
  backends, WhisperLocal compile-gated stub.
- **fono-llm**: async `TextCleanup` trait, OpenAI-compatible (Cerebras/Groq)
  + Anthropic backends, LlamaLocal compile-gated stub.
- **fono-inject**: `enigo`-based typing + clipboard-paste fallback, X11/Wayland
  focus detection stub.
- **fono-tray / fono-overlay**: event-channel stubs ready for `tray-icon` +
  `winit` wiring in a later phase.
- **fono-ipc**: single-instance Unix-socket protocol (length-prefixed JSON
  frames). **fono-download**: streaming HTTPS downloader with SHA-256
  verification.
- **fono (binary)**: clap CLI with `run`, `wizard`, `doctor`, `history
  {list,search,clear}`, `models {list,pull}`. Interactive first-run wizard,
  `doctor` health report, daemon scaffold.

Build status: `cargo build --workspace` ✅, `cargo test --workspace --lib` →
**26 passed / 0 failed**, `cargo clippy --workspace --no-deps -- -D warnings`
clean with pedantic+nursery enabled.

## Next session

- **Phase 9** — packaging: wire real release asset names, craft NimbleX
  SlackBuild at `packaging/slackbuild/fono/`, smoke-test on target host.
- **Phase 10** — flesh out `docs/providers.md`, `docs/troubleshooting.md`,
  user-facing `README` screenshots, tag `v0.1.0`.
- Follow-up integrations deferred into later milestones: real `tray-icon`
  lifecycle, `winit` overlay window, Silero ONNX VAD, `whisper-rs` local
  engine, `llama-cpp-2` local engine, full daemon wiring (audio → STT → LLM
  → inject pipeline).

## Session log

- **2026-04-24 (Phase 0)**: Bootstrap complete.
- **2026-04-24 (Phases 1–8)**: Implemented all ten library crates + CLI
  skeleton per the design plan. 26 unit tests green; clippy pedantic clean.
