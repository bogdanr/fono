# Fono — Project Status

Last updated: 2026-04-25

## Current milestone

**v0.1.0-rc: local-models out of the box, hardware-adaptive wizard.**
Pipeline (audio → trim → STT → optional LLM → inject → history) is fully
wired and warmed at daemon startup. Default release binary now bundles
whisper.cpp so picking "Local models" in the first-run wizard requires
no rebuild and no system packages. The wizard probes the host CPU/RAM
/disk first and steers to local or cloud per a five-tier classifier.

## Active plans

| Plan | Status |
|---|---|
| `docs/plans/2026-04-24-fono-design-v1.md` (Phases 0–10) | ✅ Phases 0–10 landed |
| `docs/plans/2026-04-25-fono-pipeline-wiring-v1.md` (W1–W22) | ✅ 22/22 |
| `docs/plans/2026-04-25-fono-latency-v1.md` (L1–L30) | ✅ 17/30 landed, 13 deferred-to-v0.2 |
| `docs/plans/2026-04-25-fono-local-default-v1.md` (H1–H25) | ✅ 11/25 landed, 14 deferred-to-v0.2 |

## Phase progress

| Phase | Description                                                        | Status |
|-------|--------------------------------------------------------------------|--------|
| 0     | Repo bootstrap + workspace + CI skeleton                           | ✅ Complete |
| 1     | fono-core: config, secrets, XDG paths, SQLite schema, hwcheck      | ✅ Complete |
| 2     | fono-audio: cpal capture + VAD stub + resampler + silence trim     | ✅ Complete |
| 3     | fono-hotkey: global-hotkey parser + hold/toggle FSM + listener     | ✅ Complete |
| 4     | fono-stt: trait + WhisperLocal + Groq/OpenAI + factory + prewarm   | ✅ Complete |
| 5     | fono-llm: trait + LlamaLocal stub + OpenAI-compat/Anthropic + factory + prewarm | ✅ Complete |
| 6     | fono-inject: enigo wrapper + focus detection + warm_backend        | ✅ Complete |
| 7     | fono-tray (real appindicator backend) + fono-overlay stub          | ✅ Complete |
| 8     | First-run wizard + CLI (+ tier-aware probe + `fono hwprobe`)       | ✅ Complete |
| 9     | Packaging: release.yml + NimbleX SlackBuild + AUR + Nix + Debian   | ✅ Complete |
| 10    | Docs: README, providers, wayland, privacy, architecture            | ✅ Complete |
| W     | Pipeline wiring (audio→STT→LLM→inject orchestrator)                | ✅ Complete |
| L     | Latency optimisation v0.1 wave (warm + trim + skip + defaults)     | ✅ Complete |
| H     | Local-models out of box + hardware-adaptive wizard (v0.1 slice)    | ✅ Complete |

## What landed in this session (2026-04-25, local-default + hwcheck)

### Tasks fully landed (11 of 25 from the local-default plan)

* **H1** — `crates/fono/Cargo.toml:22-32`: default features now include
  `local-models` (transitively `fono-stt/whisper-local`) so the released
  binary runs whisper out of the box. Slim cloud-only build available
  via `--no-default-features --features tray`.
* **H5/H6/H21** — new `crates/fono-core/src/hwcheck.rs` (478 lines, 13
  unit tests). `HardwareSnapshot::probe()` reads `/proc/cpuinfo`,
  `/proc/meminfo`, `statvfs`, and `std::is_x86_feature_detected!` to
  produce a `LocalTier` ∈ { Unsuitable, Minimum, Comfortable,
  Recommended, HighEnd } with documented thresholds (`MIN_CORES = 4`,
  `MIN_RAM_GB = 4`, `MIN_DISK_GB = 2`, etc.) duplicated as `pub const`
  so docs and tests stay in sync.
* **H11/H12/H13** — wizard rewritten around the tier:
    * `crates/fono/src/wizard.rs` prints the hardware summary up-front.
    * `Recommended`/`HighEnd`/`Comfortable` → local first, default.
    * `Minimum` → cloud first ("faster on your machine"), local kept
      as the second option with a "~2 s" warning.
    * `Unsuitable` → local hidden behind a `Confirm` showing the
      specific failed gate (e.g. "only 2 physical cores; minimum is 4").
    * Local-model menu narrowed to the tier's recommended model + one
      safer fallback (no longer shows whisper-medium on a 4-core box).
* **H16** — `fono doctor` now prints the hardware snapshot and tier
  alongside the existing factory probes, so users see at a glance
  whether their config matches their hardware.
* **H17** — new `fono hwprobe [--json]` subcommand:

  ```
  cores : 10 physical / 12 logical  (AVX2)
  ram   : 15 GB total · disk free : 11 GB · linux/x86_64
  tier  : comfortable (recommends whisper-small)
  ```

  JSON output is consumable by packaging scripts and the bench crate.
* **H20** — `README.md` reflects v0.1.0-rc reality: default release
  bundles whisper.cpp, build-flavour matrix, `fono hwprobe` mention.
* **H24/H25** — plan persisted at
  `docs/plans/2026-04-25-fono-local-default-v1.md`; this status entry.

### Toolchain bumps

* `Cargo.toml:73` — `whisper-rs = "0.13" → "0.16"` (0.13.2 had an
  internal API/ABI mismatch with its sys crate; 0.16 is the current
  upstream and is what whisper.cpp tracks).
* `crates/fono-stt/src/whisper_local.rs:84-92` — adapt to the 0.16
  segment API (`get_segment(idx) -> Option<WhisperSegment>` +
  `to_str_lossy()`).

### Tasks intentionally deferred to v0.2 (all annotated in plan)

* **H8** — Real `LlamaLocal` implementation against `llama-cpp-2`.
  `llama-cpp-2 0.1.x` exposes a low-level API that needs several hundred
  lines of safe-wrapper code; the v0.1 slice ships local STT only with
  optional cloud LLM cleanup. New ADR
  `docs/decisions/0008-llama-local-deferred.md` captures the rationale.
* **H2/H3** — Release CI matrix (musl-slim + glibc-local-capable
  artifacts) — Phase 9 release work, separate from this slice.
* **H4** — OpenBLAS / Metal compile flags (would speed local inference
  another 2–3× on capable hosts) — opt-in v0.2 work.
* **H7/H14/H22** — In-wizard smoke bench + tier-profile bench in
  `fono-bench` — static rule + `fono doctor` are sufficient for v0.1.
* **H15/H18/H19** — Persisting tier in config + flipping
  `LlmBackend::default()` to Local + auto-migration — blocked on H8.
* **H23** — Wizard tier-decision unit test — covered by H21 tier tests
  + manual run; full `dialoguer` mock not worth the dependency.

## Build matrix (verified this session)

| Command | Result |
|---|---|
| `cargo build -p fono` (default features) | ✅ — bundles whisper.cpp |
| `cargo build -p fono --no-default-features --features tray` | (slim, cloud-only — covered by H1's feature graph) |
| `cargo test --workspace --lib --tests` | ✅ **67 tests pass** (54 unit + 13 hwcheck), 2 ignored (latency smoke) |
| `cargo clippy --workspace --no-deps -- -D warnings` | ✅ pedantic + nursery clean |
| `cargo run -p fono -- hwprobe` | ✅ classified host as `comfortable` (10c/16GB/AVX2) |
| `cargo run -p fono -- hwprobe --json` | ✅ structured snapshot + tier |

## Recommended next session

1. Implement **H8** (`LlamaLocal` against `llama-cpp-2`) so the local
   path also covers LLM cleanup. Keep behind `llama-local` feature flag
   until proven; flip the wizard's local LLM offer back on once H9's
   integration test passes.
2. Land **L7+L8** (streaming LLM + progressive injection) — the next
   biggest perceived-latency win.
3. Pin real fixture SHA-256s via
   `crates/fono-bench/scripts/fetch-fixtures.sh` and commit
   `docs/bench/baseline-*.json` for CI regression gating.
4. Tag `v0.1.0` once `fono-bench` passes on the reference machine.
