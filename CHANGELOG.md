# Changelog

All notable changes to Fono are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] — 2026-04-25

First public release. Pipeline (audio → STT → LLM → inject) is fully wired
end-to-end; default release ships local whisper.cpp out of the box.

### Added — pipeline

- `SessionOrchestrator` (`crates/fono/src/session.rs`) glues hotkey FSM →
  cpal capture → silence trim → STT → optional LLM cleanup → text injection
  → SQLite history. Hot-swappable backends behind `RwLock<Arc<dyn …>>`.
- `fono record` — one-shot CLI dictation (microphone → stdout / inject).
- `fono transcribe <wav>` — runs a WAV file through the same pipeline; useful
  for verifying API keys without a microphone.

### Added — providers

- **STT**: local whisper.cpp (small / base / medium models), Groq cloud
  (`whisper-large-v3-turbo`), OpenAI cloud, optional Deepgram / AssemblyAI /
  Cartesia stubs.
- **LLM cleanup**: optional, off-by-default. OpenAI-compatible endpoints
  (Cerebras, Groq, OpenAI, OpenRouter, Ollama) and Anthropic.
- `STT` and `TextFormatter` traits with `prewarm()` so the first dictation
  after daemon start is not cold (latency plan L2/L3).
- `fono use {stt,llm,cloud,local,show}` — one-command provider switching;
  rewrites config atomically and hot-reloads the orchestrator (no restart).
- `fono keys {list,add,remove,check}` — multi-provider API-key vault with
  reachability probes.
- Per-call overrides: `fono record --stt openai --llm anthropic`.

### Added — hardware-adaptive setup

- `crates/fono-core/src/hwcheck.rs` — pure-Rust probe of physical/logical
  cores, RAM, free disk, and CPU features (AVX2/NEON/FMA). Maps to a
  five-level `LocalTier` (`Unsuitable`, `Minimum`, `Comfortable`,
  `Recommended`, `High-end`).
- Wizard prints the live tier and steers the user toward local vs cloud
  based on what the machine can sustain.
- `fono hwprobe [--json]` exposes the snapshot for scripts.
- `fono doctor` shows the active hardware tier alongside provider
  reachability and the chosen injector.

### Added — input / output

- Default key-injection backend `Injector::XtestPaste` — pure-Rust X11 XTEST
  paste via `x11rb` + `xsel`/`wl-copy`/`xclip` clipboard write. No system
  dependencies beyond a clipboard tool. **Shift+Insert** is the default paste
  shortcut (universal X11 binding).
- Override paste shortcut via `[inject].paste_shortcut = "ctrl-v"` in config
  or `FONO_PASTE_SHORTCUT=ctrl-shift-v` env var.
- Always-clipboard safety net: every successful dictation also writes to both
  CLIPBOARD and PRIMARY selections (`general.also_copy_to_clipboard = true`).
- Always-notify: `notify-rust` toast on every dictation
  (`general.notify_on_dictation = true`).
- `fono test-inject "<text>" [--shortcut <variant>]` — smoke-tests injection
  and clipboard delivery without speaking.

### Added — tray

- `Recent transcriptions ▸` submenu with the last 10 dictations; click to
  re-paste.
- `STT: <active> ▸` and `LLM: <active> ▸` submenus for live provider
  switching from the tray (same code path as `fono use`).
- Open history folder (was misrouted to Dolphin in pre-release; now opens
  the directory itself via `xdg-open`).

### Added — safety + observability

- Per-stage tracing breadcrumbs at `info`: `capture=…ms trim=…ms stt=…ms
  llm=…ms inject=…ms (raw_chars → cleaned_chars)`.
- Pipeline in-flight guard refuses concurrent recordings with a toast.
- Skip-LLM-when-short heuristic (configurable `llm.skip_if_words_lt`) saves
  150–800 ms per short dictation.
- Trim leading/trailing silence pre-STT (`audio.trim_silence`); ~30 % faster
  STT on 5 s utterances with 1.5 s of tail silence.

### Added — benchmark harness

- New `crates/fono-bench/` crate: 6-language LibriVox fixture set (en, es,
  fr, de, it, ro), Word Error Rate + per-stage latency report, criterion
  benchmark, regression gate. CI-fast (network-free) and full-stack modes.

### Documented

- `docs/plans/2026-04-25-fono-pipeline-wiring-v1.md` (W1–W22, all landed).
- `docs/plans/2026-04-25-fono-latency-v1.md` (L1–L30, 17 landed, 13
  deferred-to-v0.2 with rationale).
- `docs/plans/2026-04-25-fono-local-default-v1.md` (H1–H25).
- `docs/plans/2026-04-25-fono-provider-switching-v1.md` (S1–S27).
- `docs/plans/2026-04-25-fono-roadmap-v2.md` (post-v0.1 roadmap).
- ADR `docs/decisions/0007-local-models-build.md` — glibc-linked default
  release vs musl-slim cloud-only artifact.

### Models locked in v0.1.0

| Provider | Model | License | First-run download |
|---|---|---|---|
| Whisper local | `ggml-small.bin` (multilingual) | MIT | ~466 MB |
| Whisper local (light) | `ggml-base.bin` | MIT | ~142 MB |
| Groq cloud STT | `whisper-large-v3-turbo` | (cloud, no license) | n/a |
| OpenAI cloud STT | `whisper-1` | (cloud) | n/a |
| Cerebras cloud LLM | `llama-3.3-70b` | (cloud) | n/a |
| Groq cloud LLM | `llama-3.3-70b-versatile` | (cloud) | n/a |

Local LLM (Qwen2.5 / SmolLM2) is opt-in behind the `llama-local` Cargo
feature and ships fully wired in v0.2.

### Verification

- 86 unit + integration tests; 2 latency-smoke `#[ignore]` tests.
- `cargo clippy --workspace --no-deps -- -D warnings` clean (pedantic +
  nursery).
- DCO sign-off enforced on every commit.

### Known limitations

- No streaming STT/LLM yet (latency plan L6/L7/L8 deferred to v0.2). Latency
  on cloud Groq+Cerebras is ~1 s end-to-end on a 5 s utterance.
- Wayland global hotkey requires compositor binding to `fono toggle`
  (`org.freedesktop.portal.GlobalShortcuts` not yet stable in upstream
  compositors).
- Local LLM cleanup (Qwen / SmolLM) is opt-in / preview.
- Real `winit + softbuffer` overlay window is a stub (event channel only).

[Unreleased]: https://github.com/bogdanr/fono/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/bogdanr/fono/releases/tag/v0.1.0
