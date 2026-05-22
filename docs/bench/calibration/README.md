# Local STT affordability calibration — Phase 0 artefacts

This directory holds the raw and reduced bench data that Phase 0 of
[`plans/2026-05-15-local-stt-affordability-recalibration-v4.md`](../../../plans/2026-05-15-local-stt-affordability-recalibration-v4.md)
produces (plan rev v3 → v4; v4 tightens the protocol so a future re-run
fits inside 10 minutes per host). Downstream Phase 1 tasks read from
`summary/matrix.json` to refit the affordability predicate, the
`core_scale` curve, and the per-model registry numbers.

## Host roster

The reference hosts are identified by short, stable CPU-based slugs.
Each `inventory/<host_id>.json` records the IP / hostname from the
session it was benched in as `legacy_session_ip` for traceability only;
the canonical key is `host_id`.

| host_id | role | CPU | released | tier | RAM | GPU build | session IP |
|---|---|---|---|---|---:|---|---|
| `ryzen-5950x` | desktop | AMD Ryzen 9 5950X (16p/32l, Zen 3) | 2020-11 | high-end desktop (2020 flagship, still strong in 2026) | 48 GiB | ✓ Vulkan (RTX 4090, run from PVE host) | 192.168.0.79 (CPU baseline, LXC) / 192.168.0.74 (Vulkan + CPU quants, PVE host) |
| `ultra7-258v` | laptop | Intel Core Ultra 7 258V (8p/8l, Lunar Lake) | 2024-09 | premium ultraportable (current Intel flagship for thin-and-light) | 31 GiB | ✓ Vulkan (Arc 130V/140V Xe2 Battlemage) | 192.168.0.251 |
| `i7-1255u` | laptop | Intel i7-1255U (10p hybrid 2P+8E / 12l, Alder Lake-UP3, 15 W) | 2022-02 | mid-range ultraportable (mainstream business ultrabook) | 15 GiB | ✓ Vulkan (Iris Xe 96 EUs) | localhost (session-relative) |
| `i7-8550u` | laptop | Intel i7-8550U (4p/8l, Kaby Lake-R, 15 W) | 2017-08 | legacy ultraportable (mid-decade quad-core ULV; ThinkPad X1 Carbon Gen 6) | 15 GiB | not attempted (UHD 620, same class as `i7-7500u` where Vulkan build failed) | 192.168.0.127 |
| `i7-7500u` | laptop | Intel i7-7500U (2p/4l, Kaby Lake, 15 W) | 2016-08 | legacy ultraportable (~10 years old; weakest tier we expect to support) | 15 GiB | build failed (see below) | 192.168.0.112 |

> **`i7-8550u` partial sweep (2026-05-21):** CPU AC sweep ran for
> `tiny`, `tiny.en`, `base`, `base.en`, `small`, `small.en` (3 iters
> each, all `iters 3/3` in `matrix.json`). The `large-v3-turbo` cell
> never completed — the live Ubuntu 26.04 session ran out of memory
> mid-sweep, `sshd` was OOM-killed, and the host had to be rebooted.
> Recovered run files were copied from `/root/runs/` post-recovery.
> Re-running just the turbo cell needs ~10 minutes once `fono-bench`
> is back on the box; based on every other CPU laptop in the matrix
> (`i7-7500u` 0.21, `i7-1255u` 0.33, `ultra7-258v` 0.61, even the
> 16-core `ryzen-5950x` only 1.75) it will land **`unsuitable`**
> with batch RTF in the 0.4–0.6 range — no plausible 4P/8T Kaby
> Lake-R configuration moves that into `borderline` territory.

## Headline findings (AC sweep)

The summary that motivated the recalibration:

* `large-v3-turbo` **on CPU never lands `comfortable`** on any of the
  four hosts. Even the Ryzen 9 5950X 16-core desktop reaches only
  batch RTF 1.75 / stream 0.60 = `borderline`. Every laptop is
  `unsuitable`: batch RTF 0.61 (`ultra7-258v`), 0.33 (`i7-1255u`),
  0.21 (`i7-7500u`). CPU **quants do not rescue turbo** either —
  even on the 16-core desktop the best quant (turbo-q8_0) only
  reaches batch RTF 5.71 / stream 0.74 = `borderline`, and on every
  laptop the same picture holds (quant kernel class doesn't matter:
  the `avx2-fallback` 16-core desktop and the `vnni` 4-core laptop
  both plateau at stream RTF < 1).
* The registry's current `realtime_factor_cpu_avx2 = 2.5` for turbo is
  therefore wrong by **roughly 1.5–10×** depending on the host. The
  Phase 1 refit will replace it with ~1.0 and add the `BATCH_REALTIME_MIN`
  gate.
* Peak RSS for turbo on CPU lands at ~3.6 GiB across hosts — close to the
  current `min_ram_mb = 3400` but with no headroom; Phase 1 will bump it
  to 4000 MiB. **On Vulkan host RSS drops to ~300 MiB** because most
  state lives in GPU memory.
* `small` and `small.en` are `borderline` on every CPU laptop and
  `comfortable` only on the 16-core desktop and on every Vulkan cell.
* `base` and `tiny` are universally `comfortable`, even on the 2-core
  `i7-7500u` CPU.

### GPU acceleration (Vulkan) findings

The Vulkan sweep changes the picture for three of four hosts:

* **NVIDIA RTX 4090 (run from the bare-metal Proxmox host `proxmox4`,
  not the LXC container that produced the CPU baseline):**
  `large-v3-turbo` jumps from batch RTF 1.75 (`borderline`) to
  **76.00 (`comfortable`)** — a **~43× speedup**, the largest in the
  matrix. Streaming RTF goes 0.60 → 29.73 (50×). The quant variants
  see no further uplift over fp16 (turbo-q8_0 at 86.93 vs fp16 76.00
  is bandwidth-noise, not kernel speedup), confirming that on a
  high-end discrete GPU the bottleneck is no longer model-weight
  bandwidth.


* **Intel Arc 130V/140V (Xe2 Battlemage iGPU on the Core Ultra 7 258V):**
  `large-v3-turbo` jumps from batch RTF 0.61 (`unsuitable`) to **8.72
  (`comfortable`)** — a **14× speedup**, and the first `comfortable`
  turbo cell in the entire matrix. Streaming RTF goes 0.20 → 3.16 (16×),
  clearing the live-mode threshold. Every smaller model also doubles or
  triples on Vulkan.
* **Intel Iris Xe (Alder Lake-UP3, 96 EUs on the i7-1255U):**
  `large-v3-turbo` improves from 0.33 (`unsuitable`) to 1.56
  (`borderline`) — useful 5× lift but **not** enough for `comfortable`.
  Confirms the predicate must differentiate GPU classes, not assume
  every Vulkan ICD implies turbo is fast.
* The Iris Xe vs Arc Battlemage delta (5× vs 14× for turbo) means Phase
  1's `accelerated()` path needs a **per-GPU-class** modifier rather
  than a single accel boolean.

### Battery vs AC findings

Battery sweep added 2026-05-15 evening on the two modern laptops
(`i7-1255u` 2022, `ultra7-258v` 2024) — both CPU and Vulkan builds,
1 iteration per (host, build, model). Headline:

* **Zero verdict bucket flips across 26 AC↔battery cells.** Every
  affordability verdict on AC reproduces exactly on battery, on both
  CPU and Vulkan, on both laptops, for every model in the matrix.
* **Batch RTF deltas are within ±10% on average** across all cells —
  in the same noise range as the 15–30 % stddev measured between the
  three AC iterations for the same cell. The one outlier
  (i7-1255u tiny.en CPU: −31 %) is single-iteration measurement noise
  (AC stddev was already 21 %).
* **Vulkan GPU acceleration does NOT throttle on battery** on either
  laptop. Arc Battlemage (`ultra7-258v` Lunar Lake on-package iGPU)
  delivered turbo at batch RTF 9.03 on battery vs 8.72 on AC. Iris Xe
  (`i7-1255u`) showed −7 % on turbo Vulkan, still well inside the
  `borderline` bucket.
* **CPU performance also does not visibly throttle** on either modern
  Intel laptop. The Lunar Lake CPU package limits appear to be the
  same whether on AC or DC, and the modest power budget (~30 W
  sustained) is already low enough that DC operation does not reduce
  it further.

**Phase 1 implication:** the proposed `battery_aware_affordability`
gate (plan v4 Task 1.5) can be **dropped**. The empirical data shows
no DC-vs-AC verdict instability on the laptop generations we are
trying to support. A future re-evaluation should re-run this sweep on
older hardware (e.g. an `i7-7500u`-class machine) if user reports
suggest the assumption breaks down on much older laptops with more
aggressive battery thermal management.


## Layout

```
calibration/
├── README.md                              # this file
├── inventory/                             # per-host hardware snapshot
│   ├── ryzen-5950x.json
│   ├── ultra7-258v.json
│   ├── i7-1255u.json
│   └── i7-7500u.json
├── runs/                                  # raw per-iteration JSONs
│   ├── <host_id>__<power>__<build>__<model>__iter<N>.json
│   └── <host_id>__<power>__<build>__<model>__iter<N>.time.json
├── summary/
│   ├── matrix.json                        # per-cell aggregated numbers
│   └── matrix.md                          # human-readable grouped table
└── logs/                                  # sweep stdout per host
```

## Methodology

* **Driver.** Each cell is `fono-bench equivalence --stt local --model <m>`,
  invoked from `scripts/bench-sweep.sh` once per iteration in a fresh
  process so model load time is captured fresh on iter 1 and warm on
  iters 2–3. The equivalence harness reads ten public-domain WAV
  fixtures committed at `tests/fixtures/equivalence/` (en, es, fr, ro,
  zh; total ≈ 100 s of audio) and emits per-fixture batch and streaming
  latencies. RTF (real-time factor) is computed as
  `audio_seconds_processed / wall_clock_seconds`; higher = faster than
  realtime. English-only models (`*.en`) skip the non-English fixtures,
  so their throughput numbers are derived from the four English
  fixtures only — still a fair within-model comparison across hosts
  because the same four clips are used everywhere.

  **Why `equivalence` rather than the legacy `bench` subcommand the
  plan names.** The legacy `bench` command in
  `crates/fono-bench/src/bin/fono-bench.rs` reads from a static
  `FIXTURES` table that points at LibriVox MP3 URLs but requires an
  authored `fixtures.tsv` of clip offsets that has never been
  populated. Authoring those time offsets is a content-curation task
  outside Phase 0's scope. The `equivalence` subcommand consumes the
  already-committed WAV fixture set and produces the same kind of
  per-fixture batch/streaming timings, which is sufficient input for
  the affordability predicate.

* **Iterations.** Three per cell for every model except `large-v3-turbo`
  on `i7-7500u` and `i7-1255u`, where only one iteration was run (the
  bench was 5–24 minutes per iteration on those two hosts; a single
  iter is sufficient when the verdict is `unsuitable` and the gating
  signal is batch RTF < 1, which has no plausible 3× variation).
  `summary/matrix.json` reports the median RTF and the iteration stddev
  as a percentage of the median. Cells whose RTF spread exceeds 15 %
  carry a `notes` entry; operator decides whether to re-bench. (The
  cold-cache penalty on iter 1 on small models means tiny clips can
  naturally show 20–35 % spread on laptop hosts — informational, not a
  defect.)

* **Cooldown.** 20–30 s between iterations and between cells, to keep
  package temperature in a similar band across runs. Logged via the
  rusage wrapper's `context_start.package_temp_c`.

* **Thread cap.** `FONO_WHISPER_THREADS` overrides whisper.cpp's default
  `available_parallelism()` thread count. We use it on `ryzen-5950x`
  (Proxmox LXC with 32 logical threads) where the default 32-thread
  configuration *slows whisper down by ~20× on short clips* due to
  per-fixture thread-spawn and barrier-sync overhead in the GGML
  matmul kernels. The cap there is 16 (physical core count of the
  underlying Ryzen 5950X). Other hosts have ≤ 12 logical cores and
  run unconstrained. The cap is set per-sweep in the host's environment;
  see `logs/<host_id>.sweep.log` for the launch environment.

* **Resource accounting.** Each cell run is wrapped by
  `scripts/bench-with-rusage.py`, which captures peak RSS, user / sys
  / wall time via `resource.getrusage(RUSAGE_CHILDREN)` (Linux KiB,
  macOS bytes — normalised), plus host context (AC-online state,
  battery %, power profile, package temp at start, hostname, UTC
  timestamp). `/usr/bin/time -v` is intentionally not used: it's not
  installed on Proxmox LXCs and minimal NimbleX rootfs, and the
  Python wrapper produces a uniform JSON schema across every host.

* **Power state.** Phase 0 captured the AC half of the matrix only.
  The three laptops (`i7-1255u`, `i7-7500u`, `ultra7-258v`) need a
  follow-up battery sweep: unplug the charger, drop battery to 60–80 %,
  record the active power profile, then re-run `scripts/bench-sweep.sh`
  with `POWER=battery`. The desktop (`ryzen-5950x`) has no battery row
  — it's a Proxmox container on a workstation.

## Build status per host

| host_id | toolchain | CPU build | GPU build | notes |
|---|---|---|---|---|
| `ryzen-5950x` | rustc 1.88.0 (rustup, 2026-05-21 PVE host) for the Vulkan + CPU-quant re-run; original CPU fp16 baseline was rustc 1.95.0 (rustup) in the LXC | ✓ | ✓ Vulkan | RTX 4090 unblocked once the PVE host was upgraded to kernel `6.17.13-9-pve` and NVIDIA driver `595.71.05` landed (Debian trixie). The Vulkan sweep + the CPU-quant sweep were both run from the bare-metal Proxmox host `proxmox4` (`192.168.0.74`), not from the LXC container `ai` that produced the original CPU fp16 baseline. The CPU-quant binary is a separate `fono-bench` built `--no-default-features --features whisper-local,equivalence` (no `accel-vulkan`) so the kernels actually execute on CPU; the originally-attempted "CPU" cells were discarded because the unified Vulkan-linked binary was auto-dispatching to the 4090. |
| `ultra7-258v` | rustc 1.88.0 (system) | ✓ | ✓ Vulkan | Intel Arc 130V/140V (Xe2 Battlemage iGPU). Built in 1m48s using cached whisper.cpp from prior session; runs with `XDG_RUNTIME_DIR=/run/user/0` to work around root sshd lacking a logind session. |
| `i7-1255u` | rustc 1.88.0 (system) | ✓ | ✓ Vulkan | Iris Xe Graphics (Alder Lake-UP3 GT2, gen 12 Xe-LP, 96 EUs). Built `--features accel-vulkan equivalence` in 3m54s. |
| `i7-7500u` | rustc 1.95.0 (rustup) | ✓ | **build failed** | Vulkan SDK installed (`vulkan-tools`, `libvulkan-dev`, `glslang-tools`, `spirv-tools`, `glslc`) but `whisper-rs 0.16.0` references symbols (`ggml_backend_vk_buffer_type`, `ggml_backend_vk_get_device_count`, …) that have been renamed/removed in the current whisper.cpp upstream that `whisper-rs-sys` cmake-fetches. `ultra7-258v` built successfully only because it had a stale whisper.cpp checkout cached in `target/`. Needs either a pinned whisper.cpp version or a whisper-rs API update — both out of Phase 0 scope. HD 620 (Kaby Lake) was always the lowest-value GPU bench in this matrix. |

GPU build rows are recorded as `gpu_build` in each inventory JSON.

## How to re-run a specific cell

CPU build:

```sh
HOST=<id> POWER=ac BUILD=cpu \
  RUNS_DIR=docs/bench/calibration/runs \
  BENCH=target/release/fono-bench \
  WRAPPER=scripts/bench-with-rusage.py \
  MODELS=large-v3-turbo \
  ITERS=3 \
  COOLDOWN=60 \
  FONO_WHISPER_THREADS=<physical_cores> \
  sh scripts/bench-sweep.sh
```

Vulkan build (binary rebuilt with `cargo build --release -p fono-bench
--features 'accel-vulkan equivalence'`):

```sh
HOST=<id> POWER=ac BUILD=vulkan \
  RUNS_DIR=docs/bench/calibration/runs \
  BENCH=target/release/fono-bench \
  WRAPPER=scripts/bench-with-rusage.py \
  MODELS=large-v3-turbo \
  ITERS=3 \
  COOLDOWN=20 \
  XDG_RUNTIME_DIR=/run/user/0 \
  sh scripts/bench-sweep.sh
```

Outputs land back in `runs/`; re-running the summariser is one shot:

```sh
python3 scripts/bench-summarise.py \
  --runs docs/bench/calibration/runs \
  --inventory docs/bench/calibration/inventory \
  --out-json docs/bench/calibration/summary/matrix.json \
  --out-md   docs/bench/calibration/summary/matrix.md
```

## Verdict semantics (consumed by Phase 1)

* `comfortable`  — median batch RTF ≥ 2.0 **and** median streaming RTF ≥ 1.5.
* `borderline`   — median batch RTF ≥ 1.0 but streaming below the accel threshold.
* `unsuitable`   — median batch RTF < 1.0, **or** peak RSS > 90 % of host RAM.
* `errored`      — no successful iterations.

These thresholds are the Phase 1 starting point; the ADR (Task 1.8 of
the plan) records the final numbers actually shipped into
`crates/fono-core/src/hwcheck.rs`.
