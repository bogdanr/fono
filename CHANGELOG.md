# Changelog

All notable changes to Fono are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.2] — 2026-04-28

First release in which the streaming live-dictation pipeline is
actually reachable from the shipped binary, plus supply-chain
hardening for `fono update`, a typed accuracy-gate API for
`fono-bench`, and the doc-reconciliation pass that closed out the
half-shipped plans inherited from v0.2.1.

### Changed — `interactive` is now a default release feature

- `crates/fono/Cargo.toml` flips `interactive` into the default
  feature set. **Before v0.2.2 the released binary contained none of
  the Slice A streaming code** — `record --live`, the live overlay,
  `test-overlay`, and the `[interactive].enabled` config knob were
  all `#[cfg(feature = "interactive")]`-gated and the release
  workflow built without that feature. Existing v0.2.1 users will
  see the live mode work for the first time after upgrading.
- Slim cloud-only builds remain available via
  `cargo build --no-default-features --features tray,cloud-all`.

### Added — self-update supply-chain hardening

- `apply_update` now verifies each downloaded asset against a
  per-asset `<asset>.sha256` sidecar published alongside the
  aggregate `SHA256SUMS` file. Mismatches fail closed (no rename,
  original binary untouched). Legacy releases without sidecars fall
  back to TLS-only trust with a `warn!` log.
- `parse_sha256_sidecar` accepts bare-digest, text-mode
  (`<hex>  <name>`), binary-mode (`<hex> *<name>`), and multi-entry
  sidecars; rejects too-short or non-hex inputs.
- New `--bin-dir <path>` flag on `fono update` overrides the install
  directory (matches the install-script `BIN_DIR` semantics). Useful
  when running with elevated privileges or when `current_exe()`
  resolves to a non-writable path. Still refuses to overwrite
  package-managed paths (`/usr/bin`, `/bin`, `/usr/sbin`).
- `.github/workflows/release.yml` now emits a `<asset>.sha256` file
  per artefact alongside the aggregate `SHA256SUMS`.

### Added — `fono-bench` typed capability surface

- New `crates/fono-bench/src/capabilities.rs` with
  `ModelCapabilities::for_local_whisper(model_stem)` and
  `for_cloud(provider, model)` resolvers. Replaces the inline
  `english_only` boolean previously sprinkled through `fono-bench`'s
  CLI.
- `ManifestFixture` schema split into `equivalence_threshold` and
  `accuracy_threshold` (with a `serde(alias = "levenshtein_threshold")`
  for back-compat). The two gates can now be tightened
  independently. `requires_multilingual: Option<bool>` lets fixtures
  override the derived `language != "en"` default.
- `EquivalenceReport` carries a populated `model_capabilities` block
  on every run; skipped rows now carry a typed `SkipReason`
  (`Capability` / `Quick` / `NoStreaming` / `RuntimeError`) instead
  of stringly-typed note fingerprints.
- New mock-STT capability-skip integration test asserts
  `transcribe` is never invoked on English-only models against
  non-English fixtures.

### Added — real-fixture CI bench gate

- `.github/workflows/ci.yml` replaces the prior `cargo bench --no-run`
  compile-only sanity step with a real-fixture equivalence run on
  every PR. The workflow fetches the whisper `tiny.en` GGML weights
  (cached via `actions/cache@v4` keyed on the model SHA), runs
  `fono-bench equivalence --stt local --model tiny.en --baseline
  --no-legend`, and diffs per-fixture verdicts against
  `docs/bench/baseline-comfortable-tiny-en.json`. Verdict divergence
  fails the build.
- New `--baseline` flag on `fono-bench equivalence` strips the
  non-deterministic timing fields (`elapsed_ms`, `ttff_ms`,
  `duration_s`) so the committed JSON is stable across runners.
- `tests/check.sh` mirrors the CI build/clippy/test matrix locally
  (full / `--quick` / `--slim` / `--no-test`) so contributors can
  run the same gate before pushing.

### Documentation

- Three obsolete plans superseded by the
  `--allow-multiple-definition` link trick (already live in
  `.cargo/config.toml`) moved to `plans/closed/` with `Status:
  Superseded` headers: `2026-04-27-candle-backend-benchmark-v1`,
  `2026-04-27-llama-dynamic-link-sota-v1`,
  `2026-04-27-shared-ggml-static-binary-v1`.
- `docs/decisions/` backfilled to numbers `0001`–`0019`. Recovered
  ADRs for `0005`–`0008` and `0010`–`0014` carry explicit
  `Status: Reconstructed` headers; new `0017` (auto-translation
  forward-reference), `0018` (`--allow-multiple-definition` link
  trick), `0019` (Linux-multi-package platform scope).
- `docs/dev/update-qa.md` lists the ten manual verification scenarios
  for self-update changes (bare binary, `/usr/local/bin`,
  distro-packaged, offline, rate-limited, mismatched sidecar,
  prerelease, `--bin-dir`, rollback).
- `docs/bench/README.md` documents how to regenerate the committed
  baseline anchor and how the CI gate interprets it.
- `docs/plans/2026-04-25-fono-roadmap-v2.md` Tier-1 R5.1 + R5.2
  ticked as fully shipped.

### Fixed — clippy violations exposed by `interactive` default

- `crates/fono-stt/src/whisper_local.rs:336` redundant clone removed
  on `effective_selection`'s already-owned return.
- `crates/fono-stt/src/whisper_local.rs:464-471` two `match` blocks
  rewritten as `let...else` per the `manual_let_else` lint.
- `crates/fono-audio/src/stream.rs:209-230` three `vec!` calls in
  test code replaced with array literals.

## [0.2.1] — 2026-04-28

Streaming/interactive dictation lands as a first-class mode, the
overlay stops stealing focus, and Whisper finally listens to a
language allow-list instead of free-styling into the wrong tongue.

### Added — interactive (streaming) dictation

- Slice A foundation: streaming STT, latency budget, overlay live
  text, and the equivalence harness (`fono-bench`) that gates
  stream↔batch consistency per fixture.
- v7 boundary heuristics — prosody, punctuation, filler-word and
  dangling-word handling — so partial commits feel natural rather
  than mid-phrase.
- `[interactive].enabled` is now wired end-to-end through the
  `StreamingStt` factory; flipping it on actually engages the
  streaming path.
- Equivalence harness gains a real accuracy gate (batch transcript vs
  manifest reference) on top of the stream↔batch gate, plus ten
  multilingual fixtures (EN/ES/FR/ZH/RO) and a `tests/bench.sh`
  runner.

### Added — STT language allow-list

- New `[general].languages: Vec<String>` (and `[stt.local].languages`
  override) replaces the single-language `language` scalar with a
  proper allow-list. Empty = unconstrained Whisper auto-detect; one
  entry = forced; two-or-more = constrained auto-detect (Whisper picks
  from the allow-list and **bans** every other language). The legacy
  `language` scalar still parses and is migrated automatically.
- `crates/fono-stt/src/lang.rs` exposes a `LanguageSelection` enum
  threaded through `SpeechToText` / `StreamingStt` so backends never
  compare sentinel strings.
- Local Whisper backend (`crates/fono-stt/src/whisper_local.rs`)
  runs `WhisperState::lang_detect` on the prefix mel, masks
  probabilities to allow-list members, then runs `full()` with the
  picked code locked. Forced and Auto paths keep the previous one-pass
  cost.
- Cloud STT (`groq.rs`, `openai.rs`) honours the allow-list
  best-effort via two opt-in `[general]` knobs:
  `cloud_force_primary_language` and
  `cloud_rerun_on_language_mismatch`.
- Wizard now persists the language prompt into `general.languages`
  (previously discarded).

### Fixed — overlay

- Real text rendering, lifecycle and visual overhaul; live-mode UX
  fixes (`1f23194`).
- Eliminated focus theft on X11 by setting override-redirect on the
  overlay window — tooltips/dmenu/rofi-style. The overlay no longer
  intercepts the synthesized `Shift+Insert` paste on its second map
  (`f94250e`).

## [0.2.0] — 2026-04-27

Single-binary local stack: STT (`whisper.cpp`) and LLM cleanup
(`llama.cpp`) now ship together in one statically-linked `fono` binary,
out of the box, with hardware-accelerated CPU SIMD selected at runtime.

### Added — single-binary local STT + LLM

- `llama-local` is now part of the `default` features set. The previous
  `compile_error!` guard in `crates/fono/src/lib.rs` is gone — both
  `whisper-rs` and `llama-cpp-2` link into the same ELF.
- `.cargo/config.toml` adds `-Wl,--allow-multiple-definition` to
  deduplicate the otherwise-colliding `ggml` symbols vendored by both sys
  crates. Both copies originate from the same `ggerganov` upstream and
  are ABI-compatible; the linker keeps one set, no UB at runtime.
- New `accel-cuda` / `accel-metal` / `accel-vulkan` / `accel-rocm` /
  `accel-coreml` / `accel-openblas` features on `crates/fono` that
  forward to matching `whisper-rs` / `llama-cpp-2` features for opt-in
  GPU acceleration.
- Startup banner prints a new `hw accel : <accelerators> + CPU <SIMD>`
  line (runtime SIMD probe: AVX512 / AVX2 / AVX / SSE4.2 + FMA + F16C on
  x86; NEON + DotProd + FP16 on aarch64).
- `LlamaLocal::run_inference` redirects llama.cpp / ggml's internal
  `printf`-style logging through `tracing` (matches the existing
  `whisper_rs::install_logging_hooks` pattern). Default verbosity now
  emits a single `LLM ready: <model> (<MB>, <threads> threads, ctx=<n>)
  in <ms>` line; cosmetic load-time warnings (control-token type,
  `n_ctx_seq < n_ctx_train`) are silenced. Re-enable on demand with
  `FONO_LOG=llama-cpp-2=info`.
- New smoke test `crates/fono/tests/local_backends_coexist.rs` boots
  `WhisperLocal` and `LlamaLocal` in the same process to lock in the
  no-collision contract.

### Added — wizard local LLM path

- First-run wizard now offers `Local LLM cleanup (qwen2.5, private,
  offline)` as a top-level option in both the Local and Mixed paths, in
  addition to `Skip` and `Cloud`. New `configure_local_llm` helper picks
  a tier-aware model: `qwen2.5-3b-instruct` (HighEnd),
  `qwen2.5-1.5b-instruct` (Recommended/Comfortable),
  `qwen2.5-0.5b-instruct` (Minimum/Unsuitable). All Apache-2.0 per
  ADR 0004.
- The wizard's auto-download now fires for either local STT *or* local
  LLM (was STT-only).

### Added — tray UX

- Tray STT and LLM submenus now show a `●` marker beside the active
  backend (was missing — `active_backends()` returned the trait `name()`
  while the comparison logic expected the canonical config-string
  identifier).
- Switching to the local STT or LLM backend from the tray now ensures
  the corresponding model file is on disk first, with a "downloading…"
  notification, a "ready" notification on completion, and a clear error
  notification on failure (with the orchestrator reload skipped to keep
  the user on a working backend).

### Changed — hotkey defaults

- `toggle = "F9"` (was `Ctrl+Alt+Space`). Single key, no default
  binding on any major desktop, easy to fire blind.
- `hold = "F8"` (was `Ctrl+Alt+Grave`). Adjacent to F9 for natural
  push-to-talk muscle memory.
- `cancel = "Escape"` unchanged (only grabbed while recording).
- `paste_last` hotkey **removed**. The tray's "Recent transcriptions"
  submenu and the `fono paste-last` CLI cover the same need with a
  better UX (re-paste any of the last 10, not just the newest).
  `Request::PasteLast` IPC and `Cmd::PasteLast` CLI are preserved and
  now route directly to `orch.on_paste_last()`.

### Changed — release profile size

- `[profile.release]` now sets `strip = "symbols"` and `lto = "thin"`,
  trimming the dev `cargo build --release` artifact from ~23 MB → ~19 MB
  (no code removal — only `.symtab` / `.strtab` deduplication).
  `release-slim` (used by packaging CI) is unchanged at ~15 MB.

### Documented

- `docs/status.md` — new entries for hotkey ergonomics and the
  single-binary local-stack resolution.
- `docs/troubleshooting.md`, `docs/wayland.md`, `README.md` updated for
  the new default hotkeys.
- New plans: `plans/closed/2026-04-27-shared-ggml-static-binary-v1.md` (the
  shared-ggml strategy that informed the linker-dedupe shortcut; later
  superseded by `--allow-multiple-definition`),
  `plans/closed/2026-04-27-llama-dynamic-link-sota-v1.md`,
  `plans/closed/2026-04-27-candle-backend-benchmark-v1.md`,
  `plans/2026-04-27-local-stt-llm-resolution-v1.md`.

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

[Unreleased]: https://github.com/bogdanr/fono/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/bogdanr/fono/releases/tag/v0.2.0
[0.1.0]: https://github.com/bogdanr/fono/releases/tag/v0.1.0
