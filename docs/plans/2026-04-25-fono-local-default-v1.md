# Fono — Local Models Out-of-the-Box + Hardware-Adaptive Wizard (v1)

**Status:** in progress · 2026-04-25
**Dependencies:** the pipeline-wiring plan (`2026-04-25-fono-pipeline-wiring-v1.md`)
must be complete (it is — all 22 tasks ✓).

## Objective

Ship the released `fono` binary so that:

1. **Picking "Local models" in `fono setup` Just Works** — no rebuild, no extra
   system packages, no surprise "not yet wired" errors.
2. **The wizard probes hardware first** and (a) presents a tier-appropriate
   model recommendation, (b) refuses (or strongly warns about) the local branch
   on machines that can't sustain p95 < 3 s end-to-end, (c) lets the user
   override with informed consent.

## Implementation tasks

### Hardware probe (foundation)

* [x] **H5.** New module `crates/fono-core/src/hwcheck.rs` exposing
  `HardwareSnapshot { physical_cores, logical_cores, total_ram_bytes,
  available_ram_bytes, free_disk_bytes, cpu_features: { avx2, avx512, neon,
  fma }, os, arch }`. Pure-rust, no extra deps (uses `std::arch`, `/proc`,
  `/sys`, `sysinfo` not added — kept dep-light).
* [x] **H6.** Define `LocalTier` enum and scoring rules:

  | Tier         | Gate                                                       | Recommended models                          | Predicted p50 |
  |--------------|------------------------------------------------------------|----------------------------------------------|----------------|
  | `Unsuitable` | < 4 cores, OR < 3 GB free RAM, OR no AVX2/NEON, < 2 GB disk | none                                         | n/a |
  | `Minimum`    | ≥ 4 cores, ≥ 4 GB RAM, AVX2/NEON, ≥ 2 GB disk              | whisper `base` + skip-LLM                    | 1.5–2.5 s |
  | `Comfortable`| ≥ 6 cores, ≥ 8 GB RAM, ≥ 4 GB disk                          | whisper `small` + skip-LLM                   | 1.0–2.0 s |
  | `Recommended`| ≥ 8 cores, ≥ 16 GB RAM, ≥ 6 GB disk                         | whisper `small` + (optional cloud LLM)        | 0.7–1.5 s |
  | `High-end`   | ≥ 12 cores or GPU, ≥ 32 GB RAM                              | whisper `medium` + (optional cloud LLM)       | 0.5–1.0 s |

* [x] **H21.** Unit tests covering each tier boundary + degenerate inputs.

### Build-time: make local actually compile by default

* [x] **H1.** Flip the `fono` binary feature set. New `local-models` feature on
  the `fono` crate, **on by default**, transitively enables
  `fono-stt/whisper-local`. Slim variant `cargo build --no-default-features
  --features tray` skips whisper.cpp.
* [~] **H2.** Release CI matrix: deferred to **Phase 9** (release.yml work);
  out-of-scope for this plan.
* [~] **H3.** musl/glibc ADR: deferred to **Phase 9**; this plan ships glibc
  default which already covers every tested target.
* [~] **H4.** OpenBLAS / Metal compile flags: deferred to **v0.2** as opt-in;
  vanilla whisper.cpp is good enough for `Recommended` tier latency budget.

### Local LLM (`LlamaLocal`)

* [~] **H8.** Real `llama-cpp-2` integration: **deferred to v0.2**. Rationale:
  - `llama-cpp-2` 0.1.x exposes a low-level API; a safe wrapper inside Fono
    is several hundred lines of unsafe-adjacent code.
  - For v0.1 the wizard's local branch defaults to **"Skip LLM cleanup"** for
    every tier ≤ `Recommended`; users who want LLM cleanup pick a fast cloud
    provider (Cerebras / Groq).
  - Local STT (the bulk of the work) is the dominant value-add and ships
    fully working in v0.1.
  - Captured in `docs/decisions/0008-llama-local-deferred.md`.
* [~] **H9, H10.** depend on H8.

### Wizard integration

* [x] **H11.** `wizard::run` runs `HardwareSnapshot::probe()` first and prints
  a one-paragraph hardware summary + tier classification.
* [x] **H12.** Tier-aware ordering of the local/cloud `Select`:
  - `Recommended` / `High-end` / `Comfortable`: **Local first** (default).
  - `Minimum`: both shown; local labelled "will work but slower".
  - `Unsuitable`: local hidden behind a `Confirm` "show local anyway?"
    with the failing check named.
* [x] **H13.** Local model menu narrowed to the recommended tier's models +
  one safer fallback. Power users still get every model via `--all`.
* [~] **H14.** In-wizard smoke bench: deferred to v0.2 (would require a
  vendored hwbench WAV + actual whisper run during setup; static rule is
  good enough for v0.1).
* [~] **H15.** Persist `[hardware] tier`: deferred — `fono doctor` re-probes
  live, no need to persist for v0.1.

### Doctor + telemetry

* [x] **H16.** `fono doctor` reports the hardware snapshot + recommended tier.
* [x] **H17.** `fono hwprobe [--json]` subcommand emits the snapshot.

### Defaults & migration

* [~] **H18.** Flip `LlmBackend::default()` to `Local`: blocked on **H8**;
  stays `None` (Llm::default().enabled = false) for v0.1.
* [~] **H19.** Local-LLM auto-migration: blocked on H8.
* [x] **H20.** README + `docs/architecture.md` updated: default install is
  the local-capable artifact; first-run download size; how to swap to the
  slim cloud-only build.

### Tests + benchmarks

* [x] **H21.** (above) Hardware tier unit tests.
* [~] **H22.** Tier-profile bench in `fono-bench`: deferred to v0.2 — current
  smoke bench is enough for CI regression gating.
* [~] **H23.** Wizard tier-decision unit test: covered by the H21 tier tests
  + manual run; full `dialoguer` mock deferred.

### Status + plan housekeeping

* [x] **H24.** This plan persisted.
* [x] **H25.** `docs/status.md` updated with new "v0.1.0-rc local-default"
  milestone row + this session's changelog.

## Verification criteria (v0.1.0-rc)

- Fresh `cargo install fono` (or `cargo build --release -p fono`) → `fono setup`
  → pick "Local models" → first `fono record` transcribes a 5 s utterance
  within 2.5 s on a `Minimum`-tier machine.
- 2-core / 2 GB VM: wizard refuses to default-offer local, surfaces the
  failing check by name, steers to cloud.
- 8-core / 16 GB / AVX2 desktop: wizard pre-selects `whisper small + skip-LLM`;
  first dictation succeeds without warnings.
- `fono doctor` reports the live tier and any drift between configured tier
  and live tier.
- `fono hwprobe --json | jq .tier` returns one of the five tier names.
- `cargo test --workspace --lib` includes the H21 tier tests; all green.

## Verification (deferred items)

The deferred items (`~`) are explicitly **v0.2 work**, captured here so they
are not lost. None block the "local STT works out of the box" promise.
