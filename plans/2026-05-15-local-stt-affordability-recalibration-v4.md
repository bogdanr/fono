# Local STT Affordability Recalibration — v4

## Objective

Same as v3: replace the single-point affordability heuristic that lets the
wizard recommend `large-v3-turbo` on machines that cannot run it, with
calibrated numbers and a predicate fitted to measured behaviour on four
hosts (one desktop + three laptops) under both AC and battery power.

v4 narrows the bench protocol so a full per-host sweep fits inside 10
minutes wall-clock, replacing the v3 multi-language / three-iteration /
60-second-cooldown matrix that was budgeted at several hours per host. The
classifier only needs three affordability buckets per (host, model) cell,
not WER or language-by-language RTF, so the bench is tightened to measure
exactly what the predicate consumes and nothing more.

## What changes from v3

- Phase 0 Tasks 0.4 and 0.5 are rewritten (tight bench spec below). All
  other Phase 0 tasks (0.1 inventory, 0.2 builds, 0.3 model staging, 0.6
  commit) are unchanged.
- Phase 1 (Tasks 1.1–1.8) is unchanged.
- New Task 0.0 archives the v3 multi-language runs under
  `docs/bench/calibration/runs/_legacy-multilang/` rather than discarding
  them.

## Tight bench spec

The classifier has three buckets (`comfortable` / `borderline` /
`unsuitable`). To land each (host, model) cell in the right bucket we need:

- **Batch real-time factor** within ±25% (bucket boundaries are at 1.0×
  and 6.0×; 25% noise is below the bucket width).
- **Peak RSS** within ±10% (used vs host total RAM).

We do **not** need WER, statistical confidence, multi-language RTF
variation (whisper compute is language-independent within ~3%), or
stream-vs-batch divergence (covered separately by
`crates/fono-bench/src/equivalence.rs`).

The spec:

| Knob | v3 setting | v4 setting | Rationale |
|---|---|---|---|
| Fixtures per cell | 14 (multilingual set) | **1** (~30 s English clip) | Whisper compute is language-independent; a single ~30 s clip amortises startup and bounds wall time |
| Iterations per cell | 3 fresh-process cold-starts | **2**: one cold, one warm | Cold measures TTFF + model load; warm measures steady-state RTF. Cold ≥ warm is the throttling sanity check |
| Cooldown | 60 s between iterations | **10 s** | Total bench wall is short enough that 60 s is overkill |
| Models per host | 7 wizard-visible | **4 representatives**: `tiny.en`, `base.en`, `small.en`, `large-v3-turbo` | `.en` and multilingual variants are architecturally identical (same parameter count, same kernels); RTF/RSS are within ~1%. Summariser extrapolates the multilingual rows |
| Peak RSS source | `getrusage` wrapper | unchanged | `scripts/bench-with-rusage.py` works as-is |
| Pre-decode RAM check | none | **new** | For `large-v3-turbo`, compare `1.5 × model_file_size_mib` against `MemAvailable`; skip the cell with `verdict: unsuitable_ram` if it would not fit |

Budget per host:

| Model | Audio | Est. RTF | Cold | Warm | Per-model wall |
|---|---:|---:|---:|---:|---:|
| `tiny.en` | 30 s | 20× | ~5 s | ~1.5 s | ~7 s |
| `base.en` | 30 s | 10× | ~7 s | ~3 s | ~10 s |
| `small.en` | 30 s | 4× | ~12 s | ~7.5 s | ~20 s |
| `large-v3-turbo` | 30 s | ~1× CPU / ~30× GPU | ~60 s | ~30 s | ~90 s |
| **Subtotal decode** | | | | | **~127 s** |
| Cooldowns (3 × 10 s) | | | | | 30 s |
| Bench-script overhead | | | | | ~10 s |
| **Per-host total** | | | | | **~3 minutes** |

Four hosts in parallel finish AC sweep inside 10 minutes wall-clock.

## Updated artefact layout

```
docs/bench/calibration/
├── README.md                  (updated to describe v4 protocol)
├── inventory/                 (reused from v3)
├── runs/
│   ├── <host>__<power>__<build>__<model>__<iter>.json   (v4; iter ∈ {cold, warm})
│   ├── <...>__<iter>.time.json
│   └── _legacy-multilang/     (archived v3 multi-language runs; ignored by summariser)
└── summary/
    ├── matrix.json            (v4 schema: cells with verdict + verdict_reason)
    └── matrix.md
```

Summary cell schema:

```json
{
  "host_id": "192.168.0.79",
  "power": "ac",
  "build": "cpu",
  "model": "large-v3-turbo",
  "batch_rtf_cold": 0.42,
  "batch_rtf_warm": 0.48,
  "ttff_cold_s": 12.1,
  "peak_rss_mib_max": 4820,
  "verdict": "unsuitable",
  "verdict_reason": "batch_rtf_below_1",
  "extrapolated_from": null,
  "suspect_throttle": false
}
```

## Phase 0 (v4)

- [ ] Task 0.0 — Archive v3 runs to `runs/_legacy-multilang/`.
- [ ] Task 0.1 — Inventory (already complete on disk; reuse).
- [ ] Task 0.2 — Builds (CPU done on all 4 hosts; GPU not attempted, see inventories).
- [ ] Task 0.3 — Stage 4 representative models on each host.
- [ ] Task 0.4 — Tight AC bench sweep (4 hosts × 4 models × cold+warm).
- [ ] Task 0.5 — Summarise into `matrix.json` + `matrix.md`, extrapolate multilingual rows.
- [ ] Task 0.6 — Commit `docs/bench/calibration/**` + `scripts/bench-*.{py,sh}`.

## Phase 1 (unchanged from v3)

Tasks 1.1–1.8 unchanged.

## Verification (delta from v3)

- Per-host wall-clock for a full AC sweep ≤ 10 minutes.
- Summary matrix has 7 model rows per host: 4 measured (`tiny.en`,
  `base.en`, `small.en`, `large-v3-turbo`) plus 3 extrapolated (`tiny`,
  `base`, `small`) with `extrapolated_from` populated.
- `large-v3-turbo` row's `verdict_reason` matches the gate it tripped.
- `_legacy-multilang/README.md` documents the archived data.

## Risks (delta from v3)

1. **Single 30 s clip under-represents long-decode workloads.** Steady-state
   speech, well above first-chunk startup window. Phase 1 Task 1.7's
   override path lets users record their own measurement.
2. **Extrapolating multilingual rows from `.en` cells is wrong.**
   Architectures identical; `extrapolated_from` flagged explicitly so a
   reviewer can spot the inference.
3. **Cold-warm pair too small to detect throttling.** Cold ≥ warm + 10%
   tolerance asserted in summariser; violators get `suspect_throttle: true`.
4. **Pre-decode RAM check too conservative.** Uses `MemAvailable` not
   `MemFree`; 1.5× factor matches whisper.cpp's empirical working set.
