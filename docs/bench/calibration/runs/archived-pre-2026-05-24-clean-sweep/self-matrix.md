# Phase 0 calibration matrix

Each row aggregates 3 iterations (medians; spread = stddev/median × 100%).

RTF = (audio seconds processed) / (wall clock seconds). Higher = faster than realtime.

Verdict: `comfortable` (batch ≥ 2.0 AND stream ≥ 1.5); `borderline` (batch ≥ 1.0); `unsuitable` (batch < 1.0 OR RSS > 90% host RAM).

**Quant kernel class** (per-host header): `vnni` = the CPU has AVX-VNNI (`vpdpbusd`); int8 dot products run in 1 instruction per packed pair, giving the textbook 1.5-3× quant speedup over fp16. `avx2-fallback` = no AVX-VNNI; ggml emits a multi-instruction AVX2 chain whose per-op cost is comparable to fp16 FMA, so quantisation saves RSS / weight bandwidth but not throughput on these hosts.

## i7-1255u — 12th Gen Intel(R) Core(TM) i7-1255U (10p/12l, 15686 MiB, laptop) — released 2022-02, mid-range ultraportable

_2022 Alder Lake-UP3 hybrid (2P+8E, 12 threads, 15 W); mainstream business ultrabook CPU; representative of typical 2022-2024 laptops._

**Quant kernel class:** `vnni` (AVX-VNNI). AVX-VNNI (vpdpbusd) available: int8 dot products run in 1 instruction per packed pair, giving the textbook 1.5-3x quant speedup over fp16 FMA.

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | vulkan | 3/3 | 20.82 | 2.4 | 7.46 | 0.5 | 0.43 | 161 | comfortable |  |
| base-q5_1 | ac | vulkan | 3/3 | 25.22 | 7.6 | 9.79 | 0.4 | 0.31 | 172 | comfortable |  |
| base-q8_0 | ac | vulkan | 3/3 | 23.84 | 1.1 | 9.08 | 1.0 | 0.33 | 180 | comfortable |  |
| base.en | ac | vulkan | 3/3 | 20.11 | 7.8 | 7.30 | 2.1 | 0.46 | 164 | comfortable |  |
| base.en-q5_1 | ac | vulkan | 3/3 | 25.61 | 1.2 | 9.52 | 0.8 | 0.35 | 175 | comfortable |  |
| base.en-q8_0 | ac | vulkan | 3/3 | 25.76 | 3.7 | 8.72 | 1.1 | 0.34 | 184 | comfortable |  |
| large-v3-turbo | ac | vulkan | 3/3 | 3.73 | 0.7 | 0.62 | 0.5 | 6.06 | 255 | borderline |  |
| large-v3-turbo-q5_0 | ac | vulkan | 3/3 | 7.22 | 2.6 | 1.14 | 0.0 | 3.16 | 198 | borderline |  |
| large-v3-turbo-q8_0 | ac | vulkan | 3/3 | 6.11 | 4.0 | 1.03 | 0.3 | 3.55 | 209 | borderline |  |
| small | ac | vulkan | 3/3 | 8.89 | 1.9 | 2.58 | 1.0 | 1.43 | 188 | comfortable |  |
| small-q5_1 | ac | vulkan | 3/3 | 11.68 | 3.4 | 3.91 | 2.1 | 0.83 | 189 | comfortable |  |
| small-q8_0 | ac | vulkan | 3/3 | 11.01 | 2.7 | 3.60 | 0.2 | 0.88 | 173 | comfortable |  |
| small.en | ac | vulkan | 3/3 | 9.32 | 7.3 | 2.57 | 0.4 | 1.32 | 187 | comfortable |  |
| small.en-q5_1 | ac | vulkan | 3/3 | 13.29 | 2.9 | 3.69 | 2.3 | 0.98 | 189 | comfortable |  |
| small.en-q8_0 | ac | vulkan | 3/3 | 9.97 | 36.9 | 3.46 | 0.6 | 0.93 | 172 | comfortable | batch_rtf spread 36.9% > 15% |
| tiny | ac | vulkan | 3/3 | 28.15 | 1.3 | 12.42 | 1.6 | 0.25 | 154 | comfortable |  |
| tiny-q5_1 | ac | vulkan | 3/3 | 34.25 | 2.6 | 15.75 | 1.4 | 0.19 | 166 | comfortable |  |
| tiny-q8_0 | ac | vulkan | 3/3 | 31.57 | 6.3 | 13.96 | 3.6 | 0.20 | 172 | comfortable |  |
| tiny.en | ac | vulkan | 3/3 | 28.33 | 1.0 | 11.93 | 0.9 | 0.26 | 158 | comfortable |  |
| tiny.en-q5_1 | ac | vulkan | 3/3 | 33.31 | 9.7 | 14.71 | 1.9 | 0.21 | 170 | comfortable |  |
| tiny.en-q8_0 | ac | vulkan | 3/3 | 31.30 | 3.9 | 13.70 | 1.0 | 0.21 | 176 | comfortable |  |

## i7-7500u — Intel(R) Core(TM) i7-7500U CPU @ 2.70GHz (2p/4l, 15752 MiB, laptop) — released 2016-08, legacy ultraportable (~10 years old)

_2016 Kaby Lake dual-core / 4 threads, 15 W; ~10-year-old ultrabook CPU; weakest tier we expect to support._

**Quant kernel class:** `avx2-fallback` (no AVX-VNNI). No AVX-VNNI: ggml int8 dot products use the AVX2 fallback (vpmaddubsw + vpmaddwd + vpaddd + dequantise shifts). Per-op cost is similar to fp16 FMA; quant's benefits collapse to RSS and weight bandwidth on this host. See 2026-05-21 diagnostic in summary/quant-anomaly.md.

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 3/3 | 9.65 | 0.7 | 1.20 | 0.2 | 2.89 | 510 | borderline |  |
| base-q5_1 | ac | cpu | 3/3 | 9.15 | 0.6 | 1.08 | 0.2 | 3.24 | 342 | borderline |  |
| base-q8_0 | ac | cpu | 3/3 | 11.50 | 0.1 | 1.34 | 0.2 | 2.62 | 384 | borderline |  |
| base.en | ac | cpu | 3/3 | 9.42 | 0.1 | 1.17 | 0.4 | 2.98 | 568 | borderline |  |
| base.en-q8_0 | ac | cpu | 2/2 | 11.12 | 0.0 | 1.31 | 0.3 | 2.67 | 442 | borderline |  |
| large-v3-turbo | ac | cpu | 3/3 | 0.49 | 2.2 | 0.05 | 1.3 | 66.04 | 3561 | unsuitable |  |
| large-v3-turbo-q5_0 | ac | cpu | 3/3 | 0.49 | 0.2 | 0.05 | 0.0 | 66.23 | 1556 | unsuitable |  |
| large-v3-turbo-q8_0 | ac | cpu | 3/3 | 0.67 | 0.1 | 0.07 | 2.7 | 51.12 | 2129 | unsuitable |  |
| tiny | ac | cpu | 3/3 | 18.65 | 1.1 | 2.55 | 0.8 | 1.35 | 329 | comfortable |  |
| tiny-q5_1 | ac | cpu | 3/3 | 18.71 | 0.9 | 2.38 | 0.2 | 1.46 | 241 | comfortable |  |
| tiny-q8_0 | ac | cpu | 3/3 | 22.96 | 1.5 | 2.85 | 0.7 | 1.22 | 264 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 18.94 | 0.5 | 2.54 | 0.3 | 1.37 | 328 | comfortable |  |
| tiny.en-q5_1 | ac | cpu | 3/3 | 18.06 | 0.3 | 2.34 | 0.2 | 1.50 | 241 | comfortable |  |
| tiny.en-q8_0 | ac | cpu | 3/3 | 22.26 | 1.7 | 2.81 | 0.5 | 1.24 | 263 | comfortable |  |

## ultra7-258v — Intel(R) Core(TM) Ultra 7 258V (8p/8l, 31572 MiB, laptop) — released 2024-09, premium ultraportable (current)

_2024 Lunar Lake 4P+4LP-E (no SMT); Intel's current efficiency flagship for thin-and-light laptops, with on-package LPDDR5X and Xe2 Battlemage iGPU._

**Quant kernel class:** `vnni` (AVX-VNNI). Lunar Lake Lion Cove P-cores have AVX-VNNI (no AVX-512; Intel removed it from consumer Core Ultra). Int8 dot products use vpdpbusd; quant gives the full 1.5-3x throughput uplift over fp16, plus small.en-q8_0 promotes from borderline to comfortable for live streaming.

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 3/3 | 29.09 | 0.2 | 4.43 | 0.1 | 0.78 | 511 | comfortable |  |
| base-q5_1 | ac | cpu | 3/3 | 32.66 | 0.1 | 4.76 | 0.3 | 0.72 | 343 | comfortable |  |
| base-q8_0 | ac | cpu | 3/3 | 37.55 | 0.5 | 5.49 | 0.1 | 0.63 | 385 | comfortable |  |
| base.en | ac | cpu | 3/3 | 28.48 | 0.1 | 4.35 | 0.2 | 0.80 | 519 | comfortable |  |
| base.en-q5_1 | ac | cpu | 3/3 | 31.90 | 0.7 | 4.64 | 0.7 | 0.76 | 350 | comfortable |  |
| base.en-q8_0 | ac | cpu | 3/3 | 36.73 | 0.9 | 5.42 | 0.3 | 0.64 | 393 | comfortable |  |
| large-v3-turbo | ac | cpu | 3/3 | 1.60 | 0.1 | 0.17 | 0.5 | 19.79 | 3567 | borderline |  |
| large-v3-turbo-q5_0 | ac | cpu | 3/3 | 1.84 | 0.3 | 0.18 | 0.2 | 17.75 | 1566 | borderline |  |
| large-v3-turbo-q8_0 | ac | cpu | 3/3 | 2.31 | 0.1 | 0.23 | 0.2 | 14.41 | 2211 | borderline |  |
| small | ac | cpu | 3/3 | 8.68 | 0.3 | 1.24 | 0.8 | 2.77 | 1282 | borderline |  |
| small-q5_1 | ac | cpu | 3/3 | 10.13 | 0.1 | 1.31 | 0.2 | 2.61 | 716 | borderline |  |
| small-q8_0 | ac | cpu | 3/3 | 12.87 | 0.3 | 1.66 | 0.1 | 2.10 | 857 | comfortable |  |
| small.en | ac | cpu | 3/3 | 8.92 | 0.4 | 1.22 | 0.1 | 2.82 | 1290 | borderline |  |
| small.en-q5_1 | ac | cpu | 3/3 | 10.37 | 0.1 | 1.28 | 0.2 | 2.66 | 780 | borderline |  |
| small.en-q8_0 | ac | cpu | 3/3 | 12.54 | 0.3 | 1.63 | 0.1 | 2.14 | 926 | comfortable |  |
| tiny | ac | cpu | 3/3 | 53.34 | 0.3 | 9.32 | 0.2 | 0.36 | 332 | comfortable |  |
| tiny-q5_1 | ac | cpu | 3/3 | 57.87 | 1.8 | 9.91 | 0.1 | 0.34 | 245 | comfortable |  |
| tiny-q8_0 | ac | cpu | 3/3 | 63.79 | 1.5 | 11.10 | 0.3 | 0.31 | 267 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 52.19 | 1.1 | 9.09 | 0.2 | 0.38 | 331 | comfortable |  |
| tiny.en-q5_1 | ac | cpu | 3/3 | 57.26 | 1.6 | 9.64 | 0.2 | 0.36 | 244 | comfortable |  |
| tiny.en-q8_0 | ac | cpu | 3/3 | 62.16 | 1.0 | 10.85 | 0.2 | 0.32 | 265 | comfortable |  |

