# Phase 0 calibration matrix

Each row aggregates 3 iterations (medians; spread = stddev/median × 100%).

RTF = (audio seconds processed) / (wall clock seconds). Higher = faster than realtime.

Verdict: `comfortable` (batch ≥ 2.0 AND stream ≥ 1.5); `borderline` (batch ≥ 1.0); `unsuitable` (batch < 1.0 OR RSS > 90% host RAM).

## i7-7500u — Intel(R) Core(TM) i7-7500U CPU @ 2.70GHz (2p/4l, 15752 MiB, laptop) — released 2016-08, legacy ultraportable (~10 years old)

_2016 Kaby Lake dual-core / 4 threads, 15 W; ~10-year-old ultrabook CPU; weakest tier we expect to support._

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 3/3 | 3.71 | 0.8 | 1.28 | 0.7 | 2.70 | 584 | borderline |  |
| base.en | ac | cpu | 3/3 | 3.30 | 0.5 | 1.20 | 0.4 | 2.67 | 589 | borderline |  |
| large-v3-turbo | ac | cpu | 1/1 | 0.21 | — | 0.07 | — | 53.26 | 3646 | unsuitable |  |
| small | ac | cpu | 3/3 | 1.06 | 6.2 | 0.36 | 0.2 | 10.17 | 1356 | borderline |  |
| small.en | ac | cpu | 3/3 | 1.14 | 0.6 | 0.38 | 0.4 | 9.95 | 1363 | borderline |  |
| tiny | ac | cpu | 3/3 | 7.27 | 0.7 | 2.51 | 3.3 | 1.33 | 419 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 8.76 | 1.1 | 3.03 | 0.2 | 1.20 | 399 | comfortable |  |

## ultra7-258v — Intel(R) Core(TM) Ultra 7 258V (8p/8l, 31572 MiB, laptop) — released 2024-09, premium ultraportable (current)

_2024 Lunar Lake 4P+4LP-E (no SMT); Intel's current efficiency flagship for thin-and-light laptops, with on-package LPDDR5X and Xe2 Battlemage iGPU._

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 3/3 | 13.04 | 3.2 | 4.51 | 1.1 | 0.67 | 586 | comfortable |  |
| base.en | ac | cpu | 3/3 | 13.07 | 10.1 | 5.18 | 3.2 | 0.58 | 592 | comfortable |  |
| large-v3-turbo | ac | cpu | 3/3 | 0.62 | 0.3 | 0.20 | 0.2 | 17.80 | 3655 | unsuitable |  |
| small | ac | cpu | 3/3 | 3.37 | 2.4 | 1.12 | 1.6 | 3.13 | 1360 | borderline |  |
| small.en | ac | cpu | 3/3 | 4.23 | 1.3 | 1.39 | 1.5 | 2.58 | 1367 | borderline |  |
| tiny | ac | cpu | 3/3 | 23.78 | 5.3 | 8.11 | 2.8 | 0.39 | 420 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 30.30 | 11.3 | 10.44 | 8.7 | 0.26 | 413 | comfortable |  |
| base | ac | vulkan | 3/3 | 42.94 | 0.5 | 18.75 | 0.3 | 0.12 | 172 | comfortable |  |
| base.en | ac | vulkan | 3/3 | 40.45 | 1.5 | 19.86 | 0.5 | 0.12 | 176 | comfortable |  |
| large-v3-turbo | ac | vulkan | 3/3 | 8.93 | 1.5 | 3.28 | 1.4 | 1.01 | 267 | comfortable |  |
| small | ac | vulkan | 3/3 | 19.14 | 0.3 | 8.03 | 0.2 | 0.29 | 190 | comfortable |  |
| small.en | ac | vulkan | 3/3 | 21.81 | 0.5 | 9.34 | 0.6 | 0.34 | 191 | comfortable |  |
| tiny | ac | vulkan | 3/3 | 57.55 | 1.4 | 23.18 | 0.8 | 0.13 | 170 | comfortable |  |
| tiny.en | ac | vulkan | 3/3 | 65.19 | 2.4 | 33.74 | 2.8 | 0.07 | 166 | comfortable |  |

