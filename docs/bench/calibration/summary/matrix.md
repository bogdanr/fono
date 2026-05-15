# Phase 0 calibration matrix

Each row aggregates 3 iterations (medians; spread = stddev/median × 100%).

RTF = (audio seconds processed) / (wall clock seconds). Higher = faster than realtime.

Verdict: `comfortable` (batch ≥ 2.0 AND stream ≥ 1.5); `borderline` (batch ≥ 1.0); `unsuitable` (batch < 1.0 OR RSS > 90% host RAM).

## i7-1255u — 12th Gen Intel(R) Core(TM) i7-1255U (10p/12l, 15686 MiB, laptop) — released 2022-02, mid-range ultraportable

_2022 Alder Lake-UP3 hybrid (2P+8E, 12 threads, 15 W); mainstream business ultrabook CPU; representative of typical 2022-2024 laptops._

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 3/3 | 6.04 | 1.2 | 2.14 | 3.3 | 1.55 | 584 | comfortable |  |
| base.en | ac | cpu | 3/3 | 5.94 | 6.7 | 2.24 | 42.1 | 1.24 | 591 | comfortable | stream_rtf spread 42.1% > 15% |
| large-v3-turbo | ac | cpu | 1/1 | 0.33 | — | 0.10 | — | 36.63 | 3649 | unsuitable |  |
| small | ac | cpu | 3/3 | 1.62 | 1.3 | 0.56 | 9.2 | 6.07 | 1356 | borderline |  |
| small.en | ac | cpu | 3/3 | 1.94 | 3.6 | 0.66 | 1.6 | 5.46 | 1361 | borderline |  |
| tiny | ac | cpu | 3/3 | 11.49 | 34.2 | 4.46 | 26.9 | 0.65 | 420 | comfortable | batch_rtf spread 34.2% > 15%; stream_rtf spread 26.9% > 15% |
| tiny.en | ac | cpu | 3/3 | 21.12 | 21.4 | 6.72 | 1.1 | 0.47 | 411 | comfortable | batch_rtf spread 21.4% > 15% |
| base | ac | vulkan | 3/3 | 18.40 | 16.4 | 7.17 | 0.7 | 0.41 | 180 | comfortable | batch_rtf spread 16.4% > 15% |
| base.en | ac | vulkan | 3/3 | 16.74 | 4.1 | 7.18 | 0.1 | 0.39 | 182 | comfortable |  |
| large-v3-turbo | ac | vulkan | 3/3 | 1.56 | 4.0 | 0.52 | 1.2 | 6.64 | 276 | borderline |  |
| small | ac | vulkan | 3/3 | 5.90 | 6.9 | 2.20 | 8.4 | 1.36 | 199 | comfortable |  |
| small.en | ac | vulkan | 3/3 | 7.11 | 1.7 | 2.74 | 0.1 | 1.31 | 200 | comfortable |  |
| tiny | ac | vulkan | 3/3 | 27.80 | 28.4 | 10.58 | 2.1 | 0.30 | 262 | comfortable | batch_rtf spread 28.4% > 15% |
| tiny.en | ac | vulkan | 3/3 | 30.74 | 2.9 | 13.53 | 2.4 | 0.24 | 176 | comfortable |  |
| base | battery | cpu | 1/1 | 6.32 | — | 2.31 | — | 1.28 | 584 | comfortable |  |
| base.en | battery | cpu | 1/1 | 5.19 | — | 2.51 | — | 1.23 | 590 | comfortable |  |
| small | battery | cpu | 1/1 | 1.69 | — | 0.60 | — | 6.26 | 1356 | borderline |  |
| small.en | battery | cpu | 1/1 | 1.86 | — | 0.59 | — | 6.71 | 1363 | borderline |  |
| tiny | battery | cpu | 1/1 | 11.73 | — | 4.28 | — | 0.74 | 420 | comfortable |  |
| tiny.en | battery | cpu | 1/1 | 14.48 | — | 6.29 | — | 0.53 | 412 | comfortable |  |
| base | battery | vulkan | 1/1 | 18.05 | — | 7.15 | — | 0.41 | 184 | comfortable |  |
| base.en | battery | vulkan | 1/1 | 16.18 | — | 7.11 | — | 0.40 | 185 | comfortable |  |
| large-v3-turbo | battery | vulkan | 1/1 | 1.45 | — | 0.47 | — | 7.33 | 275 | borderline |  |
| small | battery | vulkan | 1/1 | 6.06 | — | 2.27 | — | 1.33 | 203 | comfortable |  |
| small.en | battery | vulkan | 1/1 | 7.00 | — | 2.70 | — | 1.33 | 203 | comfortable |  |
| tiny | battery | vulkan | 1/1 | 24.32 | — | 10.52 | — | 0.30 | 182 | comfortable |  |
| tiny.en | battery | vulkan | 1/1 | 30.52 | — | 13.58 | — | 0.23 | 180 | comfortable |  |

## i7-7500u — Intel(R) Core(TM) i7-7500U CPU @ 2.70GHz (2p/4l, 15752 MiB, laptop) — released 2016-08, legacy ultraportable (~10 years old)

_2016 Kaby Lake dual-core / 4 threads, 15 W; ~10-year-old ultrabook CPU; weakest tier we expect to support._

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 3/3 | 1.70 | 73.1 | 1.16 | 10.6 | 3.44 | 585 | borderline | batch_rtf spread 73.1% > 15% |
| base.en | ac | cpu | 3/3 | 3.15 | 0.3 | 1.14 | 0.3 | 2.81 | 590 | borderline |  |
| large-v3-turbo | ac | cpu | 1/1 | 0.21 | — | — | — | — | 1959 | unsuitable |  |
| small | ac | cpu | 3/3 | 0.99 | 0.4 | 0.33 | 0.5 | 10.87 | 1356 | unsuitable |  |
| small.en | ac | cpu | 3/3 | 1.08 | 14.8 | 0.32 | 17.2 | 12.23 | 1362 | borderline | stream_rtf spread 17.2% > 15% |
| tiny | ac | cpu | 3/3 | 7.29 | 1.7 | 2.52 | 1.5 | 1.33 | 419 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 8.60 | 0.3 | 2.97 | 0.3 | 1.24 | 400 | comfortable |  |

## ryzen-5950x — AMD Ryzen 9 5950X 16-Core Processor (16p/32l, 49152 MiB, container) — released 2020-11, high-end desktop

_2020 Zen 3 16-core enthusiast desktop; AMD's flagship consumer CPU at launch, still strong in 2026._

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 3/3 | 16.15 | 0.1 | 5.97 | 0.2 | 0.50 | 580 | comfortable |  |
| base.en | ac | cpu | 3/3 | 15.57 | 0.4 | 5.96 | 0.2 | 0.50 | 587 | comfortable |  |
| large-v3-turbo | ac | cpu | 3/3 | 1.75 | 1.0 | 0.60 | 0.4 | 5.82 | 3642 | borderline |  |
| small | ac | cpu | 3/3 | 5.62 | 0.1 | 1.97 | 0.1 | 1.72 | 1352 | comfortable |  |
| small.en | ac | cpu | 3/3 | 6.42 | 0.3 | 2.32 | 0.1 | 1.51 | 1358 | comfortable |  |
| tiny | ac | cpu | 3/3 | 28.68 | 1.2 | 9.61 | 0.4 | 0.34 | 416 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 33.48 | 1.0 | 12.40 | 4.1 | 0.27 | 409 | comfortable |  |

## ultra7-258v — Intel(R) Core(TM) Ultra 7 258V (8p/8l, 31572 MiB, laptop) — released 2024-09, premium ultraportable (current)

_2024 Lunar Lake 4P+4LP-E (no SMT); Intel's current efficiency flagship for thin-and-light laptops, with on-package LPDDR5X and Xe2 Battlemage iGPU._

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 3/3 | 11.39 | 2.4 | 4.23 | 3.2 | 0.68 | 585 | comfortable |  |
| base.en | ac | cpu | 3/3 | 13.20 | 0.8 | 5.14 | 4.1 | 0.57 | 591 | comfortable |  |
| large-v3-turbo | ac | cpu | 3/3 | 0.61 | 0.6 | 0.20 | 0.1 | 18.49 | 3654 | unsuitable |  |
| small | ac | cpu | 3/3 | 3.13 | 1.5 | 0.72 | 30.6 | 3.39 | 1360 | borderline | stream_rtf spread 30.6% > 15% |
| small.en | ac | cpu | 3/3 | 3.90 | 3.0 | 1.23 | 1.7 | 3.01 | 1367 | borderline |  |
| tiny | ac | cpu | 3/3 | 20.49 | 1.7 | 7.83 | 8.9 | 0.41 | 420 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 26.81 | 17.3 | 10.78 | 1.1 | 0.28 | 414 | comfortable | batch_rtf spread 17.3% > 15% |
| base | ac | vulkan | 3/3 | 43.01 | 0.3 | 18.73 | 0.6 | 0.12 | 173 | comfortable |  |
| base.en | ac | vulkan | 3/3 | 40.66 | 0.8 | 19.90 | 0.6 | 0.12 | 172 | comfortable |  |
| large-v3-turbo | ac | vulkan | 3/3 | 8.72 | 3.4 | 3.16 | 3.9 | 1.04 | 301 | comfortable |  |
| small | ac | vulkan | 3/3 | 19.10 | 0.1 | 8.02 | 0.7 | 0.29 | 191 | comfortable |  |
| small.en | ac | vulkan | 3/3 | 21.95 | 0.8 | 9.33 | 0.2 | 0.34 | 191 | comfortable |  |
| tiny | ac | vulkan | 3/3 | 57.10 | 6.3 | 23.64 | 2.4 | 0.13 | 181 | comfortable |  |
| tiny.en | ac | vulkan | 3/3 | 64.92 | 1.4 | 33.50 | 1.4 | 0.07 | 164 | comfortable |  |
| base | battery | cpu | 1/1 | 11.72 | — | 3.97 | — | 0.75 | 585 | comfortable |  |
| base.en | battery | cpu | 1/1 | 12.79 | — | 4.08 | — | 0.71 | 592 | comfortable |  |
| large-v3-turbo | battery | cpu | 1/1 | 0.60 | — | 0.20 | — | 18.23 | 3652 | unsuitable |  |
| small | battery | cpu | 1/1 | 3.33 | — | 1.12 | — | 3.15 | 1360 | borderline |  |
| small.en | battery | cpu | 1/1 | 3.51 | — | 1.11 | — | 3.15 | 1365 | borderline |  |
| tiny | battery | cpu | 1/1 | 21.81 | — | 8.20 | — | 0.35 | 421 | comfortable |  |
| tiny.en | battery | cpu | 1/1 | 26.86 | — | 11.27 | — | 0.26 | 415 | comfortable |  |
| base | battery | vulkan | 1/1 | 42.71 | — | 18.73 | — | 0.12 | 171 | comfortable |  |
| base.en | battery | vulkan | 1/1 | 40.45 | — | 19.82 | — | 0.12 | 170 | comfortable |  |
| large-v3-turbo | battery | vulkan | 1/1 | 9.03 | — | 3.29 | — | 1.01 | 260 | comfortable |  |
| small | battery | vulkan | 1/1 | 19.17 | — | 8.01 | — | 0.29 | 190 | comfortable |  |
| small.en | battery | vulkan | 1/1 | 21.79 | — | 9.30 | — | 0.34 | 192 | comfortable |  |
| tiny | battery | vulkan | 1/1 | 57.37 | — | 23.12 | — | 0.13 | 171 | comfortable |  |
| tiny.en | battery | vulkan | 1/1 | 64.56 | — | 33.84 | — | 0.07 | 166 | comfortable |  |

