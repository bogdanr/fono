# Phase 0 calibration matrix

Each row aggregates 3 iterations (medians; spread = stddev/median × 100%).

RTF = (audio seconds processed) / (wall clock seconds). Higher = faster than realtime.

Verdict: `comfortable` (batch ≥ 2.0 AND stream ≥ 1.5); `borderline` (batch ≥ 1.0); `unsuitable` (batch < 1.0 OR RSS > 90% host RAM).

## i7-7500u — Intel(R) Core(TM) i7-7500U CPU @ 2.70GHz (2p/4l, 15752 MiB, laptop) — released 2016-08, legacy ultraportable (~10 years old)

_2016 Kaby Lake dual-core / 4 threads, 15 W; ~10-year-old ultrabook CPU; weakest tier we expect to support._

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 3/3 | 3.71 | 0.8 | 1.28 | 0.7 | 2.70 | 584 | borderline |  |
| base-q5_1 | ac | cpu | 2/2 | 7.47 | 1.1 | 1.11 | 0.4 | 3.06 | 431 | borderline |  |
| base-q8_0 | ac | cpu | 2/2 | 9.30 | 0.3 | 1.44 | 0.7 | 2.44 | 474 | borderline |  |
| base.en | ac | cpu | 3/3 | 3.30 | 0.5 | 1.20 | 0.4 | 2.67 | 589 | borderline |  |
| base.en-q5_1 | ac | cpu | 2/2 | 5.32 | 0.6 | 1.20 | 1.4 | 3.10 | 436 | borderline |  |
| base.en-q8_0 | ac | cpu | 2/2 | 7.04 | 1.4 | 1.33 | 0.2 | 2.42 | 478 | borderline |  |
| large-v3-turbo | ac | cpu | 1/1 | 0.21 | — | 0.07 | — | 53.26 | 3646 | unsuitable |  |
| large-v3-turbo-q5_0 | ac | cpu | 1/1 | 0.52 | — | 0.06 | — | 59.92 | 1658 | unsuitable |  |
| small | ac | cpu | 3/3 | 1.06 | 6.2 | 0.36 | 0.2 | 10.17 | 1356 | borderline |  |
| small-q5_1 | ac | cpu | 2/2 | 2.51 | 0.6 | 0.32 | 0.4 | 11.20 | 794 | borderline |  |
| small-q8_0 | ac | cpu | 2/2 | 3.40 | 0.2 | 0.42 | 0.4 | 8.43 | 936 | borderline |  |
| small.en | ac | cpu | 3/3 | 1.14 | 0.6 | 0.38 | 0.4 | 9.95 | 1363 | borderline |  |
| small.en-q5_1 | ac | cpu | 2/2 | 1.75 | 0.1 | 0.34 | 0.1 | 11.41 | 800 | borderline |  |
| small.en-q8_0 | ac | cpu | 2/2 | 3.30 | 0.0 | 0.44 | 0.1 | 8.62 | 942 | borderline |  |
| tiny | ac | cpu | 3/3 | 7.27 | 0.7 | 2.51 | 3.3 | 1.33 | 419 | comfortable |  |
| tiny-q5_1 | ac | cpu | 2/2 | 16.79 | 2.0 | 2.42 | 1.1 | 1.54 | 311 | comfortable |  |
| tiny-q8_0 | ac | cpu | 2/2 | 18.44 | 0.3 | 2.86 | 1.7 | 1.18 | 335 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 8.76 | 1.1 | 3.03 | 0.2 | 1.20 | 399 | comfortable |  |
| tiny.en-q5_1 | ac | cpu | 2/2 | 12.84 | 34.1 | 2.60 | 1.8 | 1.43 | 292 | comfortable | batch_rtf spread 34.1% > 15% |
| tiny.en-q8_0 | ac | cpu | 2/2 | 23.39 | 5.4 | 3.35 | 0.2 | 1.09 | 320 | comfortable |  |
| small | ac | cpu-actx | 2/2 | 2.72 | 0.6 | 0.35 | 0.8 | 10.23 | 1362 | borderline |  |
| tiny.en | ac | cpu-actx | 2/2 | 19.91 | 4.5 | 2.97 | 0.7 | 1.23 | 385 | comfortable |  |
| small | ac | cpu-noactx | 2/2 | 1.05 | 0.3 | 0.35 | 0.2 | 10.26 | 1356 | borderline |  |
| tiny.en | ac | cpu-noactx | 2/2 | 8.54 | 0.3 | 2.96 | 0.0 | 1.23 | 400 | comfortable |  |

## ryzen-5950x — AMD Ryzen 9 5950X 16-Core Processor (16p/32l, 49152 MiB, container) — released 2020-11, high-end desktop

_2020 Zen 3 16-core enthusiast desktop; AMD's flagship consumer CPU at launch, still strong in 2026._

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base-q5_1 | ac | cpu | 3/3 | 41.66 | 3.8 | 6.23 | 0.5 | 0.48 | 429 | comfortable |  |
| base-q8_0 | ac | cpu | 2/2 | 40.31 | 1.3 | 7.05 | 0.1 | 0.46 | 471 | comfortable |  |
| base.en-q5_1 | ac | cpu | 2/2 | 32.08 | 2.9 | 7.14 | 1.9 | 0.50 | 433 | comfortable |  |
| base.en-q8_0 | ac | cpu | 2/2 | 31.21 | 3.4 | 6.99 | 0.0 | 0.45 | 474 | comfortable |  |
| large-v3-turbo-q5_0 | ac | cpu | 3/3 | 5.14 | 0.2 | 0.58 | 0.1 | 6.23 | 1654 | borderline |  |
| large-v3-turbo-q8_0 | ac | cpu | 2/2 | 5.11 | 0.8 | 0.68 | 0.1 | 5.22 | 2228 | borderline |  |
| small-q5_1 | ac | cpu | 3/3 | 15.80 | 0.7 | 2.29 | 0.2 | 1.43 | 789 | comfortable |  |
| small-q8_0 | ac | cpu | 2/2 | 14.88 | 0.5 | 2.48 | 0.2 | 1.24 | 934 | comfortable |  |
| small.en-q5_1 | ac | cpu | 3/3 | 12.12 | 0.8 | 2.50 | 0.4 | 1.46 | 799 | comfortable |  |
| small.en-q8_0 | ac | cpu | 2/2 | 15.06 | 3.9 | 2.74 | 0.8 | 1.29 | 933 | comfortable |  |
| tiny-q5_1 | ac | cpu | 3/3 | 75.43 | 0.2 | 11.13 | 0.5 | 0.35 | 309 | comfortable |  |
| tiny-q8_0 | ac | cpu | 2/2 | 67.90 | 1.2 | 11.95 | 1.9 | 0.28 | 336 | comfortable |  |
| tiny.en-q5_1 | ac | cpu | 2/2 | 80.84 | 4.6 | 13.66 | 3.1 | 0.26 | 309 | comfortable |  |
| tiny.en-q8_0 | ac | cpu | 2/2 | 81.36 | 9.9 | 14.48 | 1.5 | 0.24 | 327 | comfortable |  |
| small | ac | cpu-actx | 2/2 | 10.92 | 0.1 | 2.03 | 0.1 | 1.66 | 1363 | comfortable |  |
| small.en | ac | cpu-actx | 2/2 | 11.36 | 4.3 | 2.41 | 0.7 | 1.43 | 1362 | comfortable |  |
| tiny.en | ac | cpu-actx | 2/2 | 61.49 | 4.2 | 12.48 | 2.0 | 0.27 | 393 | comfortable |  |
| small | ac | cpu-noactx | 2/2 | 5.73 | 0.9 | 2.03 | 0.2 | 1.66 | 1353 | comfortable |  |
| small.en | ac | cpu-noactx | 2/2 | 6.63 | 0.6 | 2.39 | 0.1 | 1.43 | 1359 | comfortable |  |
| tiny.en | ac | cpu-noactx | 2/2 | 32.57 | 2.3 | 12.17 | 1.6 | 0.28 | 409 | comfortable |  |
| large-v3-turbo | ac | cpu-t16 | 2/2 | 3.99 | 0.2 | 0.64 | 0.0 | 5.47 | 3658 | borderline |  |
| small | ac | cpu-t16 | 2/2 | 11.00 | 0.1 | 2.04 | 0.1 | 1.65 | 1361 | comfortable |  |
| large-v3-turbo | ac | cpu-t32 | 2/2 | 3.17 | 1.4 | 0.48 | 0.0 | 7.16 | 3661 | borderline |  |
| small | ac | cpu-t32 | 2/2 | 5.77 | 9.0 | 1.15 | 2.8 | 2.98 | 1357 | borderline |  |
| large-v3-turbo | ac | cpu-t8 | 2/2 | 3.21 | 0.2 | 0.48 | 0.3 | 7.43 | 3656 | borderline |  |
| small | ac | cpu-t8 | 2/2 | 10.28 | 0.2 | 1.78 | 0.1 | 1.92 | 1359 | comfortable |  |

## ultra7-258v — Intel(R) Core(TM) Ultra 7 258V (8p/8l, 31572 MiB, laptop) — released 2024-09, premium ultraportable (current)

_2024 Lunar Lake 4P+4LP-E (no SMT); Intel's current efficiency flagship for thin-and-light laptops, with on-package LPDDR5X and Xe2 Battlemage iGPU._

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 3/3 | 13.04 | 3.2 | 4.51 | 1.1 | 0.67 | 586 | comfortable |  |
| base-q5_1 | ac | cpu | 3/3 | 20.02 | 6.8 | 3.87 | 1.4 | 0.70 | 434 | comfortable |  |
| base-q8_0 | ac | cpu | 2/2 | 27.63 | 5.5 | 5.73 | 0.7 | 0.53 | 473 | comfortable |  |
| base.en | ac | cpu | 3/3 | 13.07 | 10.1 | 5.18 | 3.2 | 0.58 | 592 | comfortable |  |
| base.en-q5_1 | ac | cpu | 2/2 | 22.41 | 16.0 | 5.98 | 3.0 | 0.59 | 437 | comfortable | batch_rtf spread 16.0% > 15% |
| base.en-q8_0 | ac | cpu | 2/2 | 23.62 | 5.4 | 6.37 | 3.1 | 0.45 | 480 | comfortable |  |
| large-v3-turbo | ac | cpu | 3/3 | 0.62 | 0.3 | 0.20 | 0.2 | 17.80 | 3655 | unsuitable |  |
| large-v3-turbo-q5_0 | ac | cpu | 3/3 | 1.88 | 0.8 | 0.22 | 0.3 | 16.05 | 1671 | borderline |  |
| large-v3-turbo-q8_0 | ac | cpu | 2/2 | 2.31 | 1.8 | 0.28 | 0.8 | 12.59 | 2238 | borderline |  |
| small | ac | cpu | 3/3 | 3.37 | 2.4 | 1.12 | 1.6 | 3.13 | 1360 | borderline |  |
| small-q5_1 | ac | cpu | 3/3 | 8.32 | 5.0 | 1.12 | 0.9 | 2.96 | 805 | borderline |  |
| small-q8_0 | ac | cpu | 2/2 | 10.32 | 1.5 | 1.48 | 2.7 | 2.19 | 945 | borderline |  |
| small.en | ac | cpu | 3/3 | 4.23 | 1.3 | 1.39 | 1.5 | 2.58 | 1367 | borderline |  |
| small.en-q5_1 | ac | cpu | 3/3 | 6.67 | 1.2 | 1.31 | 0.6 | 2.83 | 804 | borderline |  |
| small.en-q8_0 | ac | cpu | 2/2 | 12.05 | 3.7 | 1.82 | 0.2 | 1.88 | 948 | comfortable |  |
| tiny | ac | cpu | 3/3 | 23.78 | 5.3 | 8.11 | 2.8 | 0.39 | 420 | comfortable |  |
| tiny-q5_1 | ac | cpu | 3/3 | 37.52 | 10.9 | 8.06 | 1.6 | 0.44 | 320 | comfortable |  |
| tiny-q8_0 | ac | cpu | 2/2 | 33.06 | 6.8 | 9.14 | 0.4 | 0.35 | 345 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 30.30 | 11.3 | 10.44 | 8.7 | 0.26 | 413 | comfortable |  |
| tiny.en-q5_1 | ac | cpu | 2/2 | 46.23 | 8.2 | 10.66 | 2.2 | 0.28 | 304 | comfortable |  |
| tiny.en-q8_0 | ac | cpu | 2/2 | 38.98 | 6.2 | 12.62 | 2.5 | 0.23 | 337 | comfortable |  |
| small | ac | cpu-actx | 2/2 | 7.26 | 1.6 | 1.05 | 3.5 | 3.37 | 1368 | borderline |  |
| small.en | ac | cpu-actx | 2/2 | 8.09 | 7.8 | 1.22 | 4.6 | 3.02 | 1374 | borderline |  |
| tiny.en | ac | cpu-actx | 2/2 | 45.88 | 11.2 | 11.06 | 3.5 | 0.27 | 401 | comfortable |  |
| small | ac | cpu-noactx | 2/2 | 3.09 | 3.8 | 1.03 | 0.8 | 3.42 | 1360 | borderline |  |
| small.en | ac | cpu-noactx | 2/2 | 3.58 | 6.1 | 1.19 | 2.8 | 3.05 | 1366 | borderline |  |
| tiny.en | ac | cpu-noactx | 2/2 | 26.77 | 24.7 | 9.82 | 4.0 | 0.29 | 413 | comfortable | batch_rtf spread 24.7% > 15% |
| base | ac | vulkan | 3/3 | 42.94 | 0.5 | 18.75 | 0.3 | 0.12 | 172 | comfortable |  |
| base-q5_1 | ac | vulkan | 3/3 | 47.03 | 1.1 | 19.88 | 0.5 | 0.12 | 183 | comfortable |  |
| base-q8_0 | ac | vulkan | 2/2 | 51.38 | 4.4 | 19.90 | 0.7 | 0.12 | 198 | comfortable |  |
| base.en | ac | vulkan | 3/3 | 40.45 | 1.5 | 19.86 | 0.5 | 0.12 | 176 | comfortable |  |
| base.en-q5_1 | ac | vulkan | 2/2 | 41.80 | 5.2 | 20.38 | 0.4 | 0.14 | 207 | comfortable |  |
| base.en-q8_0 | ac | vulkan | 2/2 | 47.09 | 14.0 | 20.19 | 0.5 | 0.14 | 216 | comfortable |  |
| large-v3-turbo | ac | vulkan | 3/3 | 8.93 | 1.5 | 3.28 | 1.4 | 1.01 | 267 | comfortable |  |
| large-v3-turbo-q5_0 | ac | vulkan | 3/3 | 18.63 | 4.0 | 3.42 | 5.5 | 0.98 | 228 | comfortable |  |
| large-v3-turbo-q8_0 | ac | vulkan | 2/2 | 12.39 | 1.2 | 3.50 | 2.7 | 0.96 | 228 | comfortable |  |
| small | ac | vulkan | 3/3 | 19.14 | 0.3 | 8.03 | 0.2 | 0.29 | 190 | comfortable |  |
| small-q5_1 | ac | vulkan | 3/3 | 30.44 | 0.5 | 8.91 | 0.3 | 0.27 | 202 | comfortable |  |
| small-q8_0 | ac | vulkan | 2/2 | 30.63 | 0.4 | 8.80 | 0.1 | 0.28 | 180 | comfortable |  |
| small.en | ac | vulkan | 3/3 | 21.81 | 0.5 | 9.34 | 0.6 | 0.34 | 191 | comfortable |  |
| small.en-q5_1 | ac | vulkan | 3/3 | 26.93 | 2.5 | 9.60 | 0.3 | 0.32 | 242 | comfortable |  |
| small.en-q8_0 | ac | vulkan | 2/2 | 12.46 | 0.3 | 9.72 | 0.1 | 0.34 | 185 | comfortable |  |
| tiny | ac | vulkan | 3/3 | 57.55 | 1.4 | 23.18 | 0.8 | 0.13 | 170 | comfortable |  |
| tiny-q5_1 | ac | vulkan | 3/3 | 72.95 | 13.8 | 21.35 | 2.4 | 0.18 | 179 | comfortable |  |
| tiny-q8_0 | ac | vulkan | 2/2 | 66.50 | 15.8 | 24.97 | 2.4 | 0.12 | 206 | comfortable | batch_rtf spread 15.8% > 15% |
| tiny.en | ac | vulkan | 3/3 | 65.19 | 2.4 | 33.74 | 2.8 | 0.07 | 166 | comfortable |  |
| tiny.en-q5_1 | ac | vulkan | 2/2 | 79.61 | 2.3 | 32.75 | 0.2 | 0.10 | 178 | comfortable |  |
| tiny.en-q8_0 | ac | vulkan | 2/2 | 76.86 | 3.9 | 33.05 | 1.0 | 0.09 | 182 | comfortable |  |
| large-v3-turbo | ac | vulkan-actx | 2/2 | 9.57 | 10.8 | 3.16 | 7.0 | 1.05 | 265 | comfortable |  |
| small | ac | vulkan-actx | 2/2 | 26.25 | 0.1 | 8.03 | 0.2 | 0.29 | 192 | comfortable |  |
| tiny.en | ac | vulkan-actx | 2/2 | 75.39 | 1.5 | 32.09 | 0.8 | 0.09 | 171 | comfortable |  |
| large-v3-turbo | ac | vulkan-noactx | 2/2 | 8.50 | 8.0 | 3.07 | 7.4 | 1.08 | 268 | comfortable |  |
| small | ac | vulkan-noactx | 2/2 | 19.18 | 0.2 | 8.03 | 0.4 | 0.29 | 190 | comfortable |  |
| tiny.en | ac | vulkan-noactx | 2/2 | 64.70 | 0.7 | 33.68 | 0.2 | 0.08 | 166 | comfortable |  |

