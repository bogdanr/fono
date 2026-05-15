# Local STT Affordability Recalibration

## Objective

Replace the current single-point affordability heuristic that lets the
wizard recommend `large-v3-turbo` on machines that cannot actually run it,
with calibrated numbers and a predicate fitted to **measured** behaviour on
four hosts (one desktop + three laptops), under both AC and battery power,
with GPU acceleration exercised on every host that supports it. The
benchmark artefacts are checked in so the calibration is reproducible and
re-evaluable.

This revision (v3) folds the benchmark execution into the plan as Phase 0
so the implementation agent has an explicit, scripted runbook instead of
ad-hoc commands. Phase 1 (registry + predicate changes) is unchanged in
intent but now consumes the Phase-0 artefacts as input.

## Hosts and power-state matrix

The four hosts available for calibration:

- `192.168.0.79` — desktop (always AC).
- `localhost` — laptop, will be benched on AC and on battery.
- `192.168.0.112` — laptop, will be benched on AC and on battery.
- `192.168.0.251` — laptop, will be benched on AC and on battery.

Per-host inventory (CPU, cores, RAM, GPU, OS, AVX flags, `fono-gpu` build
availability) is unknown at plan time; Phase 0 Task 0 captures it.

Power-state matrix:

| Host | AC | Battery | CPU build | GPU build (if GPU present) |
|---|:-:|:-:|:-:|:-:|
| `192.168.0.79` (desktop) | ✓ | n/a | ✓ | ✓ |
| `localhost` (laptop) | ✓ | ✓ | ✓ | ✓ (AC + battery) |
| `192.168.0.112` (laptop) | ✓ | ✓ | ✓ | ✓ (AC + battery) |
| `192.168.0.251` (laptop) | ✓ | ✓ | ✓ | ✓ (AC + battery) |

The laptop battery runs are first-class data points: laptop CPUs and GPUs
throttle hard on battery, and a user dictating off-grid is a real use case
the predicate must serve correctly. Each battery run is performed with the
charger physically unplugged, battery between 60–80% (above the
battery-saver cliff, below 100% so the charge controller is not
brake-pulsing), and the power-profile daemon (`power-profiles-daemon` /
`tuned` / `tlp`) recorded.

## Artefact layout

All benchmark artefacts are written **under the repo so they are committed**
and re-evaluable later, plus mirrored to the per-host cache so subsequent
plan tasks (Task 7 in Phase 1) can consume them without a network hop:

```
docs/bench/calibration/
├── README.md                           # methodology pointer to the ADR
├── inventory/
│   ├── localhost.json                  # CPU/RAM/GPU/OS/AVX/build features
│   ├── 192.168.0.112.json
│   ├── 192.168.0.251.json
│   └── 192.168.0.79.json
├── runs/
│   ├── <host-id>__<power>__<build>__<model>__iter<N>.json
│   └── …                               # one file per (host, power, build, model, iteration)
└── summary/
    ├── matrix.json                     # median per (host, power, build, model)
    └── matrix.md                       # human-readable table for review
```

`<host-id>` is the inventory file stem (`localhost`,
`192.168.0.112`, …). `<power>` is `ac` or `battery`. `<build>` is `cpu`
or one of `vulkan`/`cuda`/`rocm` (matching the `fono-bench` cargo feature
that was enabled). `<model>` is the whisper variant name. `<iter>` is the
1-based iteration index (three iterations per cell). File paths are
deterministic so re-runs overwrite cleanly.

## Phase 0 — Benchmark execution (runbook)

These tasks produce the data the rest of the plan consumes. Each task is
written so an implementation agent (Forge) can execute it without
judgement calls.

- [ ] Task 0.1. **SSH preflight and inventory.** For each remote host
  (`192.168.0.112`, `192.168.0.251`, `192.168.0.79`) confirm SSH access
  works with key auth, and on every host (including `localhost`) write
  `docs/bench/calibration/inventory/<host-id>.json` containing:
  CPU `model name` (from `/proc/cpuinfo`), physical and logical core counts,
  total RAM (MiB, from `/proc/meminfo`), free disk on the partition holding
  `~/.cache/fono` (MiB, from `statvfs`), kernel (`uname -srm`), OS release
  (`/etc/os-release`), AVX flags (`grep -oE 'avx[0-9]*|fma|neon' /proc/cpuinfo
  | sort -u`), chassis type (`hostnamectl chassis` — used to distinguish
  laptop vs desktop), GPU model if any (`lspci -nn | grep -Ei 'vga|3d|display'`
  on Linux, `system_profiler SPDisplaysDataType` on macOS), the current power
  source (`/sys/class/power_supply/AC*/online` or `pmset -g batt` on macOS),
  the active power profile (`powerprofilesctl get` or equivalent), and the
  host's `whisper.cpp` commit hash (recorded later from the linked
  `whisper-rs` crate). Each inventory JSON also embeds a stable
  `host_fingerprint` (SHA-256 of CPU model + cores + RAM bucket + OS) so the
  Phase 1 override cache (Task 7 below) can key off it. Rationale: every
  later analysis step depends on knowing what each host actually is; today
  the registry comments cannot point at a host because no inventory was ever
  recorded.

- [ ] Task 0.2. **Build the `fono-bench` binaries per host.** On each host,
  build two binaries from the workspace tip: `fono-bench-cpu` (no accel
  feature) and, if the host has a supported GPU, the matching
  `fono-bench-<gpu>` (`accel-vulkan` for NVIDIA + AMD + Intel via Mesa,
  `accel-cuda` for NVIDIA-only builds where the user prefers CUDA over
  Vulkan, `accel-hipblas` for AMD ROCm). The Cargo features are confirmed
  to exist at `crates/fono-bench/Cargo.toml:53-55`. The build commands are:
  `cargo build --release -p fono-bench --features whisper-local`
  (CPU baseline) and
  `cargo build --release -p fono-bench --features whisper-local,accel-vulkan`
  (Vulkan, for example). Record per host which builds succeeded so Task 0.4
  knows which cells of the matrix to skip. Rationale: testing GPU
  acceleration "where available" requires actually compiling the GPU build
  on each candidate host; we cannot rely on a cross-built binary because the
  whisper.cpp shim links the host's Vulkan / CUDA loader.

- [ ] Task 0.3. **Stage whisper models on every host.** Ensure
  `~/.cache/fono/models/whisper/ggml-<name>.bin` exists for every
  `wizard_visible: true` model in `WHISPER_MODELS`
  (`crates/fono-stt/src/registry.rs:76-260`): `tiny`, `tiny.en`, `base`,
  `base.en`, `small`, `small.en`, `large-v3-turbo`. Total ≈ 3.6 GiB per
  host. Use `cargo run -p fono --release -- models install <name>` to drive
  the existing downloader so the SHA-256 verification path is exercised
  (default mirror is HuggingFace; `FONO_MODEL_MIRROR` can override). On
  hosts with constrained downstream bandwidth, prefer rsyncing the model
  cache from `localhost`. Rationale: bench cells fail noisily without the
  model file (`crates/fono-bench/src/bin/fono-bench.rs:312-319`).

- [ ] Task 0.4. **Run the full bench matrix.** For each (host, power state,
  build, model) cell that the inventory and Task 0.2 say is applicable, run
  `fono-bench bench --provider local --model <name> --iterations 3 --out
  docs/bench/calibration/runs/<host-id>__<power>__<build>__<model>.json`
  three times with a fresh process per iteration (so model-load latency is
  measured fresh on the first iteration and warm-cache thereafter), wrapped
  in `/usr/bin/time -v` to capture peak RSS (`Maximum resident set size`).
  Use `--languages en,es,fr,de,it,pt,nl,ro,pl,ru,uk,tr,zh,ja` (every
  language with a WER entry in the registry) so streaming and batch RFs are
  averaged across language families. Forty-eight cells maximum (4 hosts × 8
  models × 2 power states × 1–2 builds); minus the laptop AC/battery × GPU
  cells on hosts without a GPU, and minus the desktop-battery row (n/a).
  Realistic wall-clock budget: ~6–10 hours total across the four hosts in
  parallel. Each run writes a per-iteration JSON plus a wrapper JSON
  recording `time -v` peak RSS, wall clock, CPU governor, power profile,
  battery percentage at start and end (where applicable), and ambient
  package temperature if `lm-sensors` is present. Rationale: this is the
  ground truth the rest of the plan rests on; capturing power state,
  battery level, and thermal context makes the data trustworthy under
  later scrutiny.

- [ ] Task 0.5. **Reduce raw runs to a summary matrix.** Aggregate the
  per-iteration JSONs into `docs/bench/calibration/summary/matrix.json`:
  for each (host, power, build, model) cell record the median batch RTF,
  median streaming RTF, median time-to-first-fixture, median and worst-
  case peak RSS, the iteration spread (stddev as % of median), and a
  `verdict` field (`comfortable` if median batch RTF ≥ 2.0 and live RTF ≥
  the relaxed accel threshold; `borderline` if batch RTF ≥ 1.0 but live <
  threshold; `unsuitable` if median batch RTF < 1.0 or peak RSS exceeds
  90% of total host RAM). Render the same data into
  `docs/bench/calibration/summary/matrix.md` as a grouped table for human
  review. Reject any cell whose iteration spread exceeds 15% and re-run
  it (likely cause: thermal throttling between back-to-back iterations;
  insert a 60 s cooldown between runs). Rationale: the predicate
  recalibration in Phase 1 reads from the summary, not the raw runs;
  consolidating once keeps the downstream tasks pure.

- [ ] Task 0.6. **Commit the artefacts.** Add the inventory JSONs, the raw
  per-iteration JSONs, the summary matrix (both formats), and a
  `docs/bench/calibration/README.md` describing the matrix, the
  power-state protocol, the cooldown rule, the iteration count, and how to
  re-run a specific cell. Use a single signed-off commit (DCO is enforced
  per `CONTRIBUTING.md` and `AGENTS.md`). Rationale: the user asked for
  results saved on disk for future re-evaluation; committing them is what
  makes that durable across machines and hard-drive failures.

## Phase 1 — Predicate and registry changes (consumes Phase 0)

- [ ] Task 1.1. **Re-derive registry numbers from the matrix.** In
  `crates/fono-stt/src/registry.rs:76-260`, replace each model's
  `realtime_factor_cpu_avx2` with the value measured on the host whose
  inventory most closely matches the existing reference profile
  (8 physical cores, AVX2 + FMA, ≥ 16 GB RAM, AC power, CPU build); replace
  `min_ram_mb` with the worst-case peak-RSS observed across all CPU
  matrix cells for that model, rounded up to the next 100 MiB with a
  10% headroom. Confirm `approx_mb` against the on-disk size after model
  staging. Preserve monotonicity inside each family; update the existing
  tests (`crates/fono-stt/src/registry.rs:371-395`) to assert ordering
  only, not literal values. Document the calibration date, reference host,
  and bench commit in the module-level doc comment
  (`crates/fono-stt/src/registry.rs:9-33`). Rationale: every downstream
  gate reads from this table; correcting the numbers at the source fixes
  the entire chain in one place.

- [ ] Task 1.2. **Fit and ship a sub-linear `core_scale` curve.** Using
  the ratios between the four hosts' CPU-build cells on a representative
  model (`small` is the right pick: fast enough to bench everywhere,
  slow enough that thread scaling shows up), fit a saturating curve such
  that predicted RTF on each host matches the measured RTF within ±20%.
  A reasonable two-parameter form is `core_scale(cores) = clamp(α + β *
  cores / 8, 0.5, 1 + γ)`. If the four data points are insufficient to
  distinguish curve shapes (likely), pick the most conservative monotone
  fit and record both the fit and the rejected alternatives in the ADR.
  Replace the current linear `clamp(cores / 8, 0.25, 2.0)` at
  `crates/fono-core/src/hwcheck.rs:230`. Update affected tests
  (`crates/fono-core/src/hwcheck.rs:653-664`,
  `crates/fono/src/wizard.rs:2462-2470`) to assert qualitative buckets
  rather than recomputing the new float. Rationale: today's linear-up-
  to-2× scaling is what lets a 12- or 16-core box clear the 6.0
  threshold for models that do not actually keep up; flattening the
  curve to match measurement is what makes `Borderline` mean what it
  says.

- [ ] Task 1.3. **Add a batch-realtime affordability gate.** Introduce
  `pub const BATCH_REALTIME_MIN: f32 = 1.0` in
  `crates/fono-core/src/hwcheck.rs`; modify `affords_model` so models
  whose `effective_rf < BATCH_REALTIME_MIN` return
  `Affordability::Unsuitable` regardless of the RAM / disk gates and
  regardless of the live-mode threshold. Tune the constant if Phase 0
  shows that batch dictation feels acceptable at slightly sub-realtime
  rates (the ADR records the exact rationale). Add unit tests covering
  the new boundary. Rationale: today's `Unsuitable` fires only on RAM /
  disk, which is why turbo survives on every host that physically has
  the RAM. The batch-realtime gate is the smallest predicate change
  that hides a model the host genuinely cannot run at conversational
  speed.

- [ ] Task 1.4. **Detect GPU acceleration on Linux / Windows builds.**
  Extend `HardwareSnapshot::accelerated()` at
  `crates/fono-core/src/hwcheck.rs:261-263` so it returns `true` when the
  running binary was compiled with any GPU feature (`accel-vulkan`,
  `accel-cuda`, `accel-hipblas`, `accel-coreml`, `accel-metal`; the names
  match `crates/fono/Cargo.toml:39-43` and `crates/fono-stt/Cargo.toml:
  41-45`). Expose `pub fn build_has_gpu_accel() -> bool` (driven by
  `cfg!(feature = "…")`) so it is independently testable. Update
  `acceleration_summary()` (`crates/fono-core/src/hwcheck.rs:278-293`) to
  mention the specific GPU backend. Cross-check against the GPU-build
  cells of the Phase 0 matrix: on hosts where the GPU build measured
  `comfortable`, the new predicate must also return `Comfortable` for the
  same (host, model) pair after the change. Rationale: the relaxed
  `LIVE_REALTIME_MIN_ACCEL = 1.5` is the right threshold for any host
  that actually runs whisper.cpp on a GPU, but today the gate is Apple-
  Silicon-only; the Phase 0 GPU cells will show that Vulkan / CUDA hosts
  belong in the same bucket.

- [ ] Task 1.5. **Battery-aware affordability (optional, gated on Phase 0
  data).** If the Phase 0 matrix shows that the AC-versus-battery gap on
  laptops is large enough to flip the affordability bucket for a
  given (host, model) pair (a plausible outcome for turbo: AC =
  Borderline, battery = Unsuitable), introduce a `power_state: PowerState`
  field on `HardwareSnapshot` and a `LIVE_REALTIME_MIN_CPU_BATTERY`
  constant (calibrated from the data, likely ≈ 1.4 × the AC threshold)
  used when `power_state == OnBattery`. Probe the power source at runtime
  (`/sys/class/power_supply/AC*/online` on Linux,
  `IOPSCopyPowerSourcesInfo` on macOS, `GetSystemPowerStatus` on
  Windows). If the AC-versus-battery delta is below the bucket-flip
  threshold for every cell in the matrix, skip this task and record the
  null result in the ADR. Rationale: the user explicitly asked for the
  battery runs, so the predicate must be capable of acting on the
  difference if it exists; the gate on actually shipping the field is
  whether the data justifies it.

- [ ] Task 1.6. **Re-rank the wizard shortlist around the new buckets.**
  In `crates/fono/src/wizard.rs:1610-1627` and
  `crates/fono/src/wizard.rs:1707-1722`, restrict the `(recommended)`
  suffix to entries whose affordability is `Comfortable`. When the
  shortlist's first entry is `Borderline`, prepend an explicit warning
  line that says no model is comfortable on this machine and suggest the
  cloud STT path. When every visible model fails the new batch-realtime
  gate and the fallback in `pick_local_stt_model`
  (`crates/fono/src/wizard.rs:1671-1688`) fires, surface the cloud
  alternative as the first-class recommendation rather than a smaller
  local model. Rationale: the wizard's framing primes the user to
  accept position 0; that framing must match the affordability of the
  pick, otherwise the recalibrated predicate buys us nothing.

- [ ] Task 1.7. **Bench-aware override path.** Teach the wizard to prefer
  measured numbers from a local `fono bench` JSON over the static
  registry numbers when one exists for the current host. Persist to
  `${XDG_DATA_HOME:-$HOME/.local/share}/fono/bench/<host-fingerprint>.
  json`, where the fingerprint matches the `host_fingerprint` recorded in
  the Phase 0 inventory JSON (so the committed calibration artefacts can
  seed user installs by symlink or by an explicit
  `fono bench --import-calibration <path>` command). Embed the
  whisper.cpp commit hash and a registry-checksum field; invalidate the
  cache when either changes. In `build_local_stt_shortlist`
  (`crates/fono/src/wizard.rs:1572-1631`), pass the cached snapshot
  through to a new `HardwareSnapshot::affords_model_with_overrides`. The
  override is advisory: it can downgrade `Comfortable` → `Borderline` /
  `Unsuitable`, but it cannot upgrade past the static RAM / disk gates.
  Rationale: the registry numbers will always be coarse defaults; users
  who already paid for a real benchmark deserve to act on it.

- [ ] Task 1.8. **Document the methodology and pin the calibration.** Add
  `docs/decisions/0021-local-stt-affordability-recalibration.md`
  (renumber if 0021 is taken) recording the calibration matrix, the
  measured numbers per (host, power, build, model) cell, the fitted
  `core_scale` curve and its rejected alternatives, the new
  `BATCH_REALTIME_MIN` constant, the GPU-build acceleration rule, the
  battery-state decision (whether shipped or not, with rationale), the
  override format, and the re-evaluation procedure. Reference the ADR
  from the module-level docs in `crates/fono-stt/src/registry.rs:9-33`
  and `crates/fono-core/src/hwcheck.rs:106-128`. Update
  `docs/providers.md` to describe the per-affordability-bucket wizard
  behaviour. Append a `## YYYY-MM-DD` entry to `docs/status.md`
  summarising the bench matrix and linking the ADR. Rationale: the
  heuristics are the user-visible contract for "what model does Fono
  recommend on my hardware"; future contributors need the calibration
  source so they can refresh without re-running this analysis.

## Verification Criteria

- `docs/bench/calibration/inventory/` contains one JSON per host with
  every field listed in Task 0.1 populated.
- `docs/bench/calibration/runs/` contains three JSON iterations per
  applicable (host, power, build, model) cell; cells whose iteration
  spread exceeds 15% are absent (re-run successfully or excluded with
  rationale in the README).
- `docs/bench/calibration/summary/matrix.json` and `matrix.md` exist
  and the matrix tables down to the bucket level reproduce what the
  post-change predicate computes on each host.
- For every (host, model) pair, the post-change predicate's affordability
  bucket equals the `verdict` field in the summary matrix. Concretely:
  `large-v3-turbo` on the desktop (AC, CPU build) = `Borderline` or
  better; on a laptop (AC, CPU build) = `Borderline` or `Unsuitable`
  depending on the laptop; on every laptop (battery, CPU build) ≤ the
  AC bucket; on every (host, GPU build) where the GPU build measured
  comfortable = `Comfortable`.
- `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, and `cargo deny check` are clean.
- The ADR exists, is referenced from the touched module docs, and
  records every decision listed in Task 1.8.
- A `fono setup` dry-run on the desktop with the calibration JSON
  imported into the per-host cache picks a different model than a
  dry-run with the cache deleted (proves the override path works).

## Potential Risks and Mitigations

1. **One or more hosts is unreachable, refuses key auth, or lacks the
   build toolchain.** Mitigation: Task 0.1 fails fast for that host with
   a clear error; downgrade the matrix in the ADR to "calibrated on N
   hosts" and proceed. The collapse rule from v2 still applies (drop the
   GPU row first, then merge laptop rows).

2. **Battery runs throttle so aggressively that iteration spread exceeds
   15%.** Mitigation: Task 0.5 explicitly re-runs cells beyond the
   spread threshold; insert a 60 s cooldown between iterations.
   Document which cells required cooldown in the matrix README.

3. **GPU build fails to compile on a host (missing Vulkan SDK, CUDA
   mismatch).** Mitigation: Task 0.2 records build status per host; the
   matrix simply omits that cell and the ADR notes the gap. Task 1.4
   still ships because the `cfg!(feature = …)` predicate is
   compile-time-driven, not host-driven.

4. **Calibration data shows the four-host curve is too noisy to fit a
   non-trivial `core_scale`.** Mitigation: Task 1.2 prescribes the
   most-conservative monotone fit and records the rejection of
   alternatives in the ADR; the override path (Task 1.7) covers users
   whose hardware sits outside the calibration cloud.

5. **Battery-on-laptop runs change so much between sessions that the
   measurement is not stable.** Mitigation: each battery run logs start
   and end battery percentage, package temperature, and the active
   power profile; the ADR pins the protocol so future re-evaluations can
   reproduce conditions. If even with the protocol the variance stays
   high, Task 1.5 falls through to "not shipped" and the ADR records
   why.

6. **Committing 30+ MB of bench JSON bloats the repo.** Mitigation:
   raw per-iteration JSONs are typically < 5 KB each (only metadata +
   per-fixture latency); the full matrix is well under 1 MB even with
   pretty-printing. If the size proves uncomfortable, the raw runs can
   be moved to a `docs/bench/calibration/.large/` directory and
   git-ignored, with the summary matrix remaining checked in.

## Alternative Approaches

1. **Skip Phase 0 and ship Phase 1 with editorial numbers.** Trade-off:
   one engineering session instead of ten; but the original problem is
   that editorial numbers are exactly what produced today's bug, so
   shipping that twice in a row is a regression in process. Rejected.

2. **Run benchmarks only on `localhost` and accept v2's collapse rule.**
   Trade-off: removes the SSH / coordination burden; but loses the
   battery data the user specifically asked for, and the `core_scale`
   curve becomes a one-point fit (i.e. cannot meaningfully change shape
   from today's linear). Rejected.

3. **Use synthetic benchmarks (matrix-multiply micro-bench plus a
   memory-bandwidth probe) instead of running real whisper inference.**
   Trade-off: faster, but whisper.cpp's actual performance depends on
   GGML kernel selection, KV-cache layout, and beam-search thread
   contention that no micro-bench predicts within useful tolerance.
   Rejected.
