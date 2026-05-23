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
| base | ac | cpu | 2/2 | 26.56 | 1.2 | 7.72 | 1.4 | 0.40 | 181 | comfortable |  |
| base-q5_1 | ac | cpu | 2/2 | 17.16 | 4.7 | 9.37 | 1.2 | 0.30 | 180 | comfortable |  |
| base-q8_0 | ac | cpu | 2/2 | 26.36 | 0.5 | 7.97 | 0.7 | 0.31 | 188 | comfortable |  |
| base.en | ac | cpu | 2/2 | 23.22 | 3.4 | 6.81 | 4.8 | 0.43 | 182 | comfortable |  |
| base.en-q5_1 | ac | cpu | 2/2 | 25.76 | 0.0 | 9.44 | 0.3 | 0.29 | 185 | comfortable |  |
| base.en-q8_0 | ac | cpu | 2/2 | 22.08 | 0.6 | 8.65 | 0.9 | 0.31 | 194 | comfortable |  |
| large-v3-turbo | ac | cpu | 2/2 | 2.38 | 1.3 | 0.48 | 0.6 | 7.87 | 268 | borderline |  |
| large-v3-turbo-q5_0 | ac | cpu | 2/2 | 4.51 | 16.2 | 0.84 | 3.5 | 4.40 | 212 | borderline | batch_rtf spread 16.2% > 15% |
| large-v3-turbo-q8_0 | ac | cpu | 2/2 | 4.43 | 4.8 | 0.81 | 2.4 | 4.57 | 211 | borderline |  |
| small | ac | cpu | 2/2 | 9.12 | 1.0 | 2.17 | 1.4 | 1.65 | 200 | comfortable |  |
| small-q5_1 | ac | cpu | 2/2 | 15.72 | 1.4 | 3.33 | 1.1 | 1.00 | 198 | comfortable |  |
| small-q8_0 | ac | cpu | 2/2 | 13.91 | 0.6 | 3.11 | 0.4 | 1.07 | 178 | comfortable |  |
| small.en | ac | cpu | 2/2 | 4.59 | 2.2 | 2.62 | 0.1 | 1.41 | 199 | comfortable |  |
| small.en-q5_1 | ac | cpu | 2/2 | 14.75 | 1.6 | 3.95 | 1.6 | 0.83 | 200 | comfortable |  |
| small.en-q8_0 | ac | cpu | 2/2 | 4.52 | 1.7 | 3.74 | 0.1 | 0.90 | 180 | comfortable |  |
| tiny | ac | cpu | 2/2 | 34.62 | 8.9 | 11.19 | 0.0 | 0.29 | 178 | comfortable |  |
| tiny-q5_1 | ac | cpu | 2/2 | 38.09 | 29.9 | 10.47 | 3.8 | 0.45 | 174 | comfortable | batch_rtf spread 29.9% > 15% |
| tiny-q8_0 | ac | cpu | 2/2 | 38.93 | 1.0 | 12.45 | 0.6 | 0.20 | 184 | comfortable |  |
| tiny.en | ac | cpu | 2/2 | 39.47 | 1.7 | 13.25 | 1.7 | 0.24 | 172 | comfortable |  |
| tiny.en-q5_1 | ac | cpu | 2/2 | 51.34 | 1.6 | 17.03 | 0.6 | 0.19 | 176 | comfortable |  |
| tiny.en-q8_0 | ac | cpu | 2/2 | 47.04 | 2.3 | 15.69 | 0.3 | 0.18 | 182 | comfortable |  |
| base | ac | vulkan | 2/2 | 21.72 | 3.7 | 6.12 | 2.6 | 0.48 | 176 | comfortable |  |
| base-q5_1 | ac | vulkan | 2/2 | 17.36 | 2.7 | 9.42 | 0.1 | 0.29 | 188 | comfortable |  |
| base-q8_0 | ac | vulkan | 2/2 | 27.17 | 2.1 | 8.22 | 0.2 | 0.30 | 202 | comfortable |  |
| base.en | ac | vulkan | 2/2 | 23.69 | 5.0 | 6.95 | 2.3 | 0.41 | 182 | comfortable |  |
| base.en-q5_1 | ac | vulkan | 2/2 | 26.56 | 0.3 | 9.71 | 0.0 | 0.28 | 193 | comfortable |  |
| base.en-q8_0 | ac | vulkan | 2/2 | 23.31 | 1.7 | 8.89 | 0.4 | 0.30 | 201 | comfortable |  |
| large-v3-turbo | ac | vulkan | 2/2 | 2.39 | 1.2 | 0.50 | 0.5 | 7.74 | 270 | borderline |  |
| large-v3-turbo-q5_0 | ac | vulkan | 2/2 | 7.01 | 3.0 | 0.93 | 6.6 | 3.75 | 227 | borderline |  |
| large-v3-turbo-q8_0 | ac | vulkan | 2/2 | 6.33 | 6.4 | 0.90 | 6.1 | 3.87 | 224 | borderline |  |
| small | ac | vulkan | 2/2 | 8.93 | 2.8 | 2.16 | 0.9 | 1.69 | 200 | comfortable |  |
| small-q5_1 | ac | vulkan | 2/2 | 12.67 | 8.8 | 2.98 | 17.1 | 1.13 | 206 | comfortable | stream_rtf spread 17.1% > 15% |
| small-q8_0 | ac | vulkan | 2/2 | 13.26 | 5.5 | 2.90 | 9.4 | 1.15 | 186 | comfortable |  |
| small.en | ac | vulkan | 2/2 | 4.58 | 2.5 | 2.62 | 0.3 | 1.40 | 198 | comfortable |  |
| small.en-q5_1 | ac | vulkan | 2/2 | 14.71 | 2.1 | 4.02 | 0.4 | 0.82 | 207 | comfortable |  |
| small.en-q8_0 | ac | vulkan | 2/2 | 4.29 | 7.7 | 3.68 | 2.7 | 0.91 | 188 | comfortable |  |
| tiny | ac | vulkan | 2/2 | 30.43 | 28.9 | 10.98 | 0.0 | 0.29 | 177 | comfortable | batch_rtf spread 28.9% > 15% |
| tiny-q5_1 | ac | vulkan | 2/2 | 42.50 | 13.6 | 10.87 | 0.5 | 0.44 | 189 | comfortable |  |
| tiny-q8_0 | ac | vulkan | 2/2 | 37.21 | 1.8 | 12.65 | 0.4 | 0.19 | 209 | comfortable |  |
| tiny.en | ac | vulkan | 2/2 | 41.57 | 4.8 | 13.29 | 1.9 | 0.24 | 172 | comfortable |  |
| tiny.en-q5_1 | ac | vulkan | 2/2 | 45.54 | 6.9 | 16.96 | 1.8 | 0.20 | 187 | comfortable |  |
| tiny.en-q8_0 | ac | vulkan | 2/2 | 44.85 | 3.2 | 15.60 | 1.0 | 0.18 | 189 | comfortable |  |

## i7-7500u — Intel(R) Core(TM) i7-7500U CPU @ 2.70GHz (2p/4l, 15752 MiB, laptop) — released 2016-08, legacy ultraportable (~10 years old)

_2016 Kaby Lake dual-core / 4 threads, 15 W; ~10-year-old ultrabook CPU; weakest tier we expect to support._

**Quant kernel class:** `avx2-fallback` (no AVX-VNNI). No AVX-VNNI: ggml int8 dot products use the AVX2 fallback (vpmaddubsw + vpmaddwd + vpaddd + dequantise shifts). Per-op cost is similar to fp16 FMA; quant's benefits collapse to RSS and weight bandwidth on this host. See 2026-05-21 diagnostic in summary/quant-anomaly.md.

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 2/2 | 7.47 | 0.6 | 1.26 | 0.6 | 3.09 | 603 | borderline |  |
| base-q5_1 | ac | cpu | 2/2 | 5.93 | 0.4 | 1.06 | 0.3 | 3.45 | 455 | borderline |  |
| base-q8_0 | ac | cpu | 2/2 | 8.51 | 0.8 | 1.42 | 0.6 | 2.78 | 476 | borderline |  |
| base.en | ac | cpu | 2/2 | 6.34 | 0.4 | 1.04 | 0.1 | 3.06 | 605 | borderline |  |
| base.en-q5_1 | ac | cpu | 2/2 | 4.96 | 0.3 | 1.10 | 0.2 | 3.41 | 433 | borderline |  |
| base.en-q8_0 | ac | cpu | 2/2 | 6.29 | 0.1 | 1.17 | 0.4 | 2.73 | 477 | borderline |  |
| large-v3-turbo | ac | cpu | 2/2 | 0.37 | 0.4 | 0.06 | 0.4 | 67.24 | 3666 | unsuitable |  |
| large-v3-turbo-q5_0 | ac | cpu | 2/2 | 0.39 | 0.4 | 0.06 | 0.2 | 65.76 | 1662 | unsuitable |  |
| large-v3-turbo-q8_0 | ac | cpu | 2/2 | 0.51 | 0.3 | 0.08 | 0.4 | 50.76 | 2234 | unsuitable |  |
| small | ac | cpu | 2/2 | 2.36 | 0.3 | 0.34 | 0.5 | 12.19 | 1368 | borderline |  |
| small-q5_1 | ac | cpu | 2/2 | 2.27 | 0.1 | 0.32 | 0.0 | 12.76 | 798 | borderline |  |
| small-q8_0 | ac | cpu | 2/2 | 2.98 | 0.3 | 0.42 | 0.3 | 9.60 | 944 | borderline |  |
| small.en | ac | cpu | 2/2 | 2.37 | 0.2 | 0.33 | 0.0 | 11.59 | 1364 | borderline |  |
| small.en-q5_1 | ac | cpu | 2/2 | 1.61 | 0.2 | 0.30 | 0.1 | 12.54 | 796 | borderline |  |
| small.en-q8_0 | ac | cpu | 2/2 | 3.01 | 0.1 | 0.39 | 0.1 | 9.66 | 938 | borderline |  |
| tiny | ac | cpu | 2/2 | 14.47 | 2.2 | 2.54 | 1.9 | 1.46 | 417 | comfortable |  |
| tiny-q5_1 | ac | cpu | 2/2 | 14.42 | 0.2 | 2.34 | 0.3 | 1.75 | 328 | comfortable |  |
| tiny-q8_0 | ac | cpu | 2/2 | 16.16 | 0.1 | 2.74 | 0.4 | 1.36 | 352 | comfortable |  |
| tiny.en | ac | cpu | 2/2 | 18.21 | 0.5 | 2.66 | 0.2 | 1.38 | 382 | comfortable |  |
| tiny.en-q5_1 | ac | cpu | 2/2 | 17.09 | 0.1 | 2.43 | 0.1 | 1.54 | 292 | comfortable |  |
| tiny.en-q8_0 | ac | cpu | 2/2 | 20.94 | 0.1 | 2.94 | 0.5 | 1.25 | 333 | comfortable |  |
| base | ac | vulkan | 2/2 | 9.44 | 0.3 | 2.01 | 0.4 | 1.59 | 187 | comfortable |  |
| base-q5_1 | ac | vulkan | 2/2 | 8.71 | 0.2 | 1.96 | 0.1 | 1.62 | 195 | comfortable |  |
| base-q8_0 | ac | vulkan | 2/2 | 8.31 | 0.2 | 1.87 | 0.1 | 1.69 | 204 | comfortable |  |
| base.en | ac | vulkan | 2/2 | 8.85 | 0.1 | 1.95 | 0.3 | 1.56 | 191 | comfortable |  |
| base.en-q5_1 | ac | vulkan | 2/2 | 7.32 | 0.5 | 1.91 | 0.1 | 1.63 | 201 | comfortable |  |
| base.en-q8_0 | ac | vulkan | 2/2 | 8.27 | 0.7 | 1.85 | 0.1 | 1.62 | 210 | comfortable |  |
| large-v3-turbo | ac | vulkan | 2/2 | 0.96 | 0.3 | 0.13 | 0.2 | 27.04 | 279 | unsuitable |  |
| large-v3-turbo-q5_0 | ac | vulkan | 2/2 | 1.00 | 0.2 | 0.13 | 1.4 | 28.27 | 260 | borderline |  |
| large-v3-turbo-q8_0 | ac | vulkan | 2/2 | 0.81 | 0.8 | 0.13 | 0.7 | 28.23 | 226 | unsuitable |  |
| small | ac | vulkan | 2/2 | 3.63 | 0.7 | 0.62 | 0.2 | 5.29 | 207 | borderline |  |
| small-q5_1 | ac | vulkan | 2/2 | 3.66 | 0.4 | 0.58 | 0.3 | 6.03 | 211 | borderline |  |
| small-q8_0 | ac | vulkan | 2/2 | 3.32 | 0.6 | 0.56 | 0.0 | 6.22 | 194 | borderline |  |
| small.en | ac | vulkan | 2/2 | 1.99 | 0.2 | 0.69 | 0.3 | 5.39 | 207 | borderline |  |
| small.en-q5_1 | ac | vulkan | 2/2 | 2.89 | 0.1 | 0.67 | 0.1 | 5.51 | 218 | borderline |  |
| small.en-q8_0 | ac | vulkan | 2/2 | 1.45 | 1.0 | 0.65 | 0.1 | 5.67 | 195 | borderline |  |
| tiny | ac | vulkan | 2/2 | 15.26 | 3.8 | 3.40 | 2.1 | 0.95 | 180 | comfortable |  |
| tiny-q5_1 | ac | vulkan | 2/2 | 12.38 | 23.1 | 2.95 | 1.7 | 1.40 | 224 | comfortable | batch_rtf spread 23.1% > 15% |
| tiny-q8_0 | ac | vulkan | 2/2 | 13.23 | 5.6 | 3.36 | 0.5 | 0.84 | 222 | comfortable |  |
| tiny.en | ac | vulkan | 2/2 | 17.43 | 1.4 | 4.08 | 1.7 | 0.82 | 185 | comfortable |  |
| tiny.en-q5_1 | ac | vulkan | 2/2 | 15.63 | 11.4 | 3.97 | 1.3 | 0.86 | 193 | comfortable |  |
| tiny.en-q8_0 | ac | vulkan | 2/2 | 16.54 | 2.1 | 3.91 | 0.2 | 0.83 | 195 | comfortable |  |

## i7-8550u — Intel(R) Core(TM) i7-8550U CPU @ 1.80GHz (4p/8l, 15750 MiB, laptop) — released 2017-08, legacy ultraportable

_2017 Kaby Lake-R quad-core / 8 threads, 15 W; first ULV quad-core generation; ThinkPad X1 Carbon Gen 6 (20KH) class; bridges the 2016 i7-7500u and the 2022 i7-1255u in our laptop roster._

**Quant kernel class:** `avx2-fallback` (no AVX-VNNI). No AVX-VNNI: ggml int8 dot products use the AVX2 fallback. Quantisation produces zero throughput gain over fp16 on this host (sometimes uses slightly more user-time CPU than fp16); only RSS reductions are real. Build also requires GGML_NATIVE=OFF GGML_AVX_VNNI=OFF when cross-compiled from a VNNI-capable host to avoid SIGILL.

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 3/3 | 11.32 | 2.2 | 1.88 | 4.5 | 1.76 | 600 | comfortable |  |
| base-q5_1 | ac | cpu | 2/2 | 11.53 | 4.5 | 1.73 | 3.8 | 1.91 | 426 | comfortable |  |
| base-q8_0 | ac | cpu | 2/2 | 12.66 | 8.7 | 2.01 | 9.8 | 1.71 | 474 | comfortable |  |
| base.en | ac | cpu | 3/3 | 10.08 | 0.8 | 1.79 | 1.8 | 1.74 | 602 | comfortable |  |
| base.en-q5_1 | ac | cpu | 2/2 | 8.32 | 4.7 | 1.86 | 2.6 | 2.06 | 433 | comfortable |  |
| base.en-q8_0 | ac | cpu | 2/2 | 9.38 | 4.8 | 1.88 | 6.0 | 1.71 | 476 | comfortable |  |
| large-v3-turbo | ac | cpu | 2/2 | 0.56 | 0.5 | 0.09 | 0.5 | 43.57 | 3664 | unsuitable |  |
| large-v3-turbo-q5_0 | ac | cpu | 2/2 | 0.60 | 0.2 | 0.09 | 0.1 | 42.80 | 1660 | unsuitable |  |
| large-v3-turbo-q8_0 | ac | cpu | 2/2 | 0.76 | 0.0 | 0.12 | 0.2 | 33.33 | 2233 | unsuitable |  |
| small | ac | cpu | 3/3 | 3.44 | 0.7 | 0.45 | 0.4 | 7.91 | 1359 | borderline |  |
| small-q5_1 | ac | cpu | 2/2 | 3.47 | 0.6 | 0.43 | 0.9 | 8.20 | 791 | borderline |  |
| small-q8_0 | ac | cpu | 2/2 | 4.34 | 0.1 | 0.54 | 0.0 | 6.44 | 932 | borderline |  |
| small.en | ac | cpu | 3/3 | 3.55 | 1.7 | 0.51 | 1.8 | 7.23 | 1365 | borderline |  |
| small.en-q5_1 | ac | cpu | 2/2 | 2.55 | 0.9 | 0.46 | 1.3 | 8.36 | 798 | borderline |  |
| small.en-q8_0 | ac | cpu | 2/2 | 4.53 | 2.7 | 0.60 | 2.0 | 6.20 | 940 | borderline |  |
| tiny | ac | cpu | 3/3 | 21.13 | 1.2 | 3.61 | 0.5 | 0.92 | 405 | comfortable |  |
| tiny-q5_1 | ac | cpu | 2/2 | 25.14 | 0.1 | 3.69 | 0.1 | 1.02 | 310 | comfortable |  |
| tiny-q8_0 | ac | cpu | 2/2 | 24.46 | 0.3 | 4.00 | 0.5 | 0.83 | 340 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 25.92 | 0.8 | 4.29 | 0.3 | 0.83 | 403 | comfortable |  |
| tiny.en-q5_1 | ac | cpu | 2/2 | 27.93 | 0.1 | 4.17 | 0.2 | 0.90 | 313 | comfortable |  |
| tiny.en-q8_0 | ac | cpu | 2/2 | 30.47 | 6.3 | 4.79 | 2.5 | 0.75 | 337 | comfortable |  |
| base | ac | vulkan | 2/2 | 9.60 | 1.0 | 2.05 | 0.2 | 1.56 | 178 | comfortable |  |
| base-q5_1 | ac | vulkan | 2/2 | 8.88 | 1.5 | 2.04 | 0.1 | 1.56 | 187 | comfortable |  |
| base-q8_0 | ac | vulkan | 2/2 | 8.83 | 0.6 | 1.97 | 1.5 | 1.60 | 195 | comfortable |  |
| base.en | ac | vulkan | 2/2 | 6.91 | 7.1 | 1.94 | 2.6 | 1.67 | 166 | comfortable |  |
| base.en-q5_1 | ac | vulkan | 2/2 | 6.81 | 11.6 | 1.95 | 0.5 | 1.59 | 176 | comfortable |  |
| base.en-q8_0 | ac | vulkan | 2/2 | 7.83 | 13.5 | 1.92 | 0.0 | 1.57 | 186 | comfortable |  |
| large-v3-turbo | ac | vulkan | 2/2 | 0.88 | 1.7 | 0.13 | 0.8 | 28.46 | 272 | unsuitable |  |
| large-v3-turbo-q5_0 | ac | vulkan | 2/2 | 1.00 | 2.4 | 0.13 | 3.2 | 28.45 | 238 | borderline |  |
| large-v3-turbo-q8_0 | ac | vulkan | 2/2 | 0.88 | 1.6 | 0.12 | 0.9 | 28.88 | 222 | unsuitable |  |
| small | ac | vulkan | 2/2 | 3.66 | 0.7 | 0.61 | 0.3 | 5.43 | 199 | borderline |  |
| small-q5_1 | ac | vulkan | 2/2 | 3.71 | 0.2 | 0.58 | 0.0 | 6.08 | 205 | borderline |  |
| small-q8_0 | ac | vulkan | 2/2 | 3.43 | 0.9 | 0.56 | 0.1 | 6.20 | 186 | borderline |  |
| small.en | ac | vulkan | 2/2 | 1.99 | 3.7 | 0.71 | 0.1 | 5.30 | 183 | borderline |  |
| small.en-q5_1 | ac | vulkan | 2/2 | 2.93 | 5.1 | 0.71 | 0.1 | 5.26 | 193 | borderline |  |
| small.en-q8_0 | ac | vulkan | 2/2 | 1.90 | 3.2 | 0.69 | 0.1 | 5.33 | 172 | borderline |  |
| tiny | ac | vulkan | 2/2 | 14.01 | 3.7 | 3.21 | 0.2 | 1.00 | 179 | comfortable |  |
| tiny-q5_1 | ac | vulkan | 2/2 | 11.95 | 20.1 | 2.77 | 0.4 | 1.55 | 198 | comfortable | batch_rtf spread 20.1% > 15% |
| tiny-q8_0 | ac | vulkan | 2/2 | 11.94 | 6.4 | 3.39 | 1.0 | 0.81 | 212 | comfortable |  |
| tiny.en | ac | vulkan | 2/2 | 13.93 | 18.8 | 3.30 | 0.7 | 0.95 | 161 | comfortable | batch_rtf spread 18.8% > 15% |
| tiny.en-q5_1 | ac | vulkan | 2/2 | 11.88 | 1.7 | 3.73 | 11.3 | 1.00 | 166 | comfortable |  |
| tiny.en-q8_0 | ac | vulkan | 2/2 | 14.60 | 25.9 | 3.84 | 5.7 | 0.79 | 172 | comfortable | batch_rtf spread 25.9% > 15% |

## ryzen-5950x — AMD Ryzen 9 5950X 16-Core Processor (16p/32l, 49152 MiB, container) — released 2020-11, high-end desktop

_2020 Zen 3 16-core enthusiast desktop; AMD's flagship consumer CPU at launch, still strong in 2026._

**Quant kernel class:** `avx2-fallback` (no AVX-VNNI). Zen 3 (2020): no AVX-VNNI (added in Zen 4, 2022). ggml int8 path uses the AVX2 fallback. Quant speedup vs fp16 is bandwidth-only here; on 16 fast cores + dual-channel DDR4 the headroom is large enough that quant still wins, but not by the VNNI multiplier.

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 2/2 | 38.13 | 0.5 | 11.97 | 3.5 | 0.40 | 511 | comfortable |  |
| base-q5_1 | ac | cpu | 2/2 | 33.94 | 1.3 | 10.61 | 3.6 | 0.43 | 364 | comfortable |  |
| base-q8_0 | ac | cpu | 2/2 | 46.33 | 0.9 | 13.48 | 3.4 | 0.40 | 385 | comfortable |  |
| base.en | ac | cpu | 0/2 | — | — | — | — | — | 13 | errored | no successful iterations |
| base.en-q5_1 | ac | cpu | 2/2 | 36.07 | 0.3 | 8.55 | 0.7 | 0.42 | 436 | comfortable |  |
| base.en-q8_0 | ac | cpu | 2/2 | 38.72 | 0.5 | 8.37 | 2.4 | 0.38 | 472 | comfortable |  |
| large-v3-turbo | ac | cpu | 2/2 | 3.05 | 0.0 | 0.92 | 0.1 | 5.40 | 3633 | borderline |  |
| large-v3-turbo-q5_0 | ac | cpu | 2/2 | 2.91 | 1.5 | 0.84 | 0.1 | 5.92 | 1629 | borderline |  |
| large-v3-turbo-q8_0 | ac | cpu | 2/2 | 3.42 | 0.8 | 1.00 | 0.0 | 4.91 | 2202 | borderline |  |
| small | ac | cpu | 2/2 | 13.87 | 4.9 | 3.43 | 0.1 | 2.28 | 1280 | comfortable |  |
| small-q5_1 | ac | cpu | 2/2 | 16.25 | 5.2 | 3.83 | 0.1 | 1.94 | 712 | comfortable |  |
| small-q8_0 | ac | cpu | 2/2 | 17.42 | 3.0 | 4.84 | 1.2 | 1.14 | 854 | comfortable |  |
| small.en | ac | cpu | 0/2 | — | — | — | — | — | 13 | errored | no successful iterations |
| small.en-q5_1 | ac | cpu | 2/2 | 13.75 | 1.9 | 2.80 | 1.0 | 1.33 | 797 | comfortable |  |
| small.en-q8_0 | ac | cpu | 2/2 | 19.06 | 1.1 | 3.22 | 0.2 | 1.13 | 936 | comfortable |  |
| tiny | ac | cpu | 2/2 | 54.42 | 0.0 | 18.42 | 4.8 | 0.24 | 329 | comfortable |  |
| tiny-q5_1 | ac | cpu | 2/2 | 68.57 | 2.5 | 20.62 | 3.8 | 0.24 | 244 | comfortable |  |
| tiny-q8_0 | ac | cpu | 2/2 | 55.93 | 36.9 | 21.69 | 0.5 | 0.22 | 264 | comfortable | batch_rtf spread 36.9% > 15% |
| tiny.en | ac | cpu | 0/2 | — | — | — | — | — | 13 | errored | no successful iterations |
| tiny.en-q5_1 | ac | cpu | 2/2 | 87.65 | 11.8 | 15.83 | 2.9 | 0.23 | 308 | comfortable |  |
| tiny.en-q8_0 | ac | cpu | 2/2 | 98.12 | 0.9 | 16.75 | 0.9 | 0.20 | 327 | comfortable |  |
| base | ac | vulkan | 2/2 | 75.03 | 5.8 | 62.14 | 3.1 | 0.06 | 295 | comfortable |  |
| base-q5_1 | ac | vulkan | 2/2 | 80.58 | 6.7 | 68.32 | 0.8 | 0.05 | 261 | comfortable |  |
| base-q8_0 | ac | vulkan | 2/2 | 80.82 | 4.7 | 67.80 | 1.2 | 0.05 | 269 | comfortable |  |
| base.en | ac | vulkan | 2/2 | 92.35 | 1.4 | 64.39 | 1.6 | 0.04 | 308 | comfortable |  |
| base.en-q5_1 | ac | vulkan | 2/2 | 80.85 | 2.5 | 66.62 | 0.5 | 0.04 | 266 | comfortable |  |
| base.en-q8_0 | ac | vulkan | 2/2 | 98.58 | 3.0 | 67.93 | 1.8 | 0.04 | 275 | comfortable |  |
| large-v3-turbo | ac | vulkan | 2/2 | 38.43 | 24.5 | 31.48 | 0.4 | 0.23 | 468 | comfortable | batch_rtf spread 24.5% > 15% |
| large-v3-turbo-q5_0 | ac | vulkan | 2/2 | 54.75 | 13.7 | 36.53 | 3.0 | 0.15 | 314 | comfortable |  |
| large-v3-turbo-q8_0 | ac | vulkan | 2/2 | 49.06 | 19.2 | 35.54 | 0.1 | 0.17 | 347 | comfortable | batch_rtf spread 19.2% > 15% |
| small | ac | vulkan | 2/2 | 53.46 | 12.8 | 27.49 | 0.4 | 0.35 | 346 | comfortable |  |
| small-q5_1 | ac | vulkan | 2/2 | 61.64 | 6.8 | 45.56 | 0.0 | 0.09 | 300 | comfortable |  |
| small-q8_0 | ac | vulkan | 2/2 | 60.08 | 5.9 | 27.97 | 2.2 | 0.36 | 289 | comfortable |  |
| small.en | ac | vulkan | 2/2 | 61.85 | 1.5 | 40.05 | 0.7 | 0.09 | 346 | comfortable |  |
| small.en-q5_1 | ac | vulkan | 2/2 | 65.65 | 1.6 | 40.58 | 2.4 | 0.07 | 305 | comfortable |  |
| small.en-q8_0 | ac | vulkan | 2/2 | 67.05 | 0.2 | 43.66 | 0.2 | 0.07 | 300 | comfortable |  |
| tiny | ac | vulkan | 2/2 | 76.85 | 0.1 | 63.69 | 0.2 | 0.04 | 244 | comfortable |  |
| tiny-q5_1 | ac | vulkan | 2/2 | 82.76 | 2.5 | 65.64 | 2.0 | 0.04 | 255 | comfortable |  |
| tiny-q8_0 | ac | vulkan | 2/2 | 81.97 | 0.9 | 62.72 | 5.0 | 0.05 | 262 | comfortable |  |
| tiny.en | ac | vulkan | 2/2 | 106.22 | 0.8 | 89.20 | 1.9 | 0.03 | 255 | comfortable |  |
| tiny.en-q5_1 | ac | vulkan | 2/2 | 85.78 | 44.0 | 89.61 | 0.7 | 0.03 | 291 | comfortable | batch_rtf spread 44.0% > 15% |
| tiny.en-q8_0 | ac | vulkan | 2/2 | 107.31 | 0.7 | 88.35 | 0.8 | 0.03 | 263 | comfortable |  |

## ultra7-258v — Intel(R) Core(TM) Ultra 7 258V (8p/8l, 31572 MiB, laptop) — released 2024-09, premium ultraportable (current)

_2024 Lunar Lake 4P+4LP-E (no SMT); Intel's current efficiency flagship for thin-and-light laptops, with on-package LPDDR5X and Xe2 Battlemage iGPU._

**Quant kernel class:** `vnni` (AVX-VNNI). Lunar Lake Lion Cove P-cores have AVX-VNNI (no AVX-512; Intel removed it from consumer Core Ultra). Int8 dot products use vpdpbusd; quant gives the full 1.5-3x throughput uplift over fp16, plus small.en-q8_0 promotes from borderline to comfortable for live streaming.

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 2/2 | 22.10 | 1.2 | 4.21 | 1.5 | 0.88 | 603 | comfortable |  |
| base-q5_1 | ac | cpu | 2/2 | 19.96 | 0.2 | 3.86 | 0.8 | 0.87 | 458 | comfortable |  |
| base-q8_0 | ac | cpu | 2/2 | 27.17 | 0.6 | 5.21 | 1.1 | 0.72 | 477 | comfortable |  |
| base.en | ac | cpu | 2/2 | 21.25 | 0.3 | 4.01 | 0.2 | 0.78 | 608 | comfortable |  |
| base.en-q5_1 | ac | cpu | 2/2 | 19.58 | 0.3 | 4.93 | 0.2 | 0.74 | 437 | comfortable |  |
| base.en-q8_0 | ac | cpu | 2/2 | 22.54 | 0.3 | 4.98 | 0.2 | 0.63 | 480 | comfortable |  |
| large-v3-turbo | ac | cpu | 2/2 | 1.06 | 0.1 | 0.17 | 0.3 | 22.53 | 3673 | borderline |  |
| large-v3-turbo-q5_0 | ac | cpu | 2/2 | 1.21 | 0.0 | 0.19 | 0.1 | 20.74 | 1672 | borderline |  |
| large-v3-turbo-q8_0 | ac | cpu | 2/2 | 1.46 | 0.1 | 0.23 | 0.1 | 16.93 | 2242 | borderline |  |
| small | ac | cpu | 2/2 | 6.45 | 2.2 | 0.98 | 2.0 | 4.23 | 1370 | borderline |  |
| small-q5_1 | ac | cpu | 2/2 | 6.81 | 0.3 | 1.01 | 0.0 | 3.98 | 804 | borderline |  |
| small-q8_0 | ac | cpu | 2/2 | 8.66 | 0.7 | 1.29 | 0.7 | 3.00 | 944 | borderline |  |
| small.en | ac | cpu | 2/2 | 7.47 | 0.2 | 1.11 | 2.5 | 3.38 | 1375 | borderline |  |
| small.en-q5_1 | ac | cpu | 2/2 | 6.10 | 2.0 | 1.09 | 0.9 | 3.32 | 806 | borderline |  |
| small.en-q8_0 | ac | cpu | 2/2 | 10.98 | 0.2 | 1.45 | 0.0 | 2.55 | 952 | borderline |  |
| tiny | ac | cpu | 2/2 | 43.52 | 0.3 | 9.00 | 0.1 | 0.40 | 416 | comfortable |  |
| tiny-q5_1 | ac | cpu | 2/2 | 50.17 | 0.5 | 9.56 | 0.3 | 0.42 | 331 | comfortable |  |
| tiny-q8_0 | ac | cpu | 2/2 | 51.52 | 0.2 | 10.85 | 0.3 | 0.33 | 350 | comfortable |  |
| tiny.en | ac | cpu | 2/2 | 52.81 | 0.0 | 9.59 | 0.3 | 0.37 | 401 | comfortable |  |
| tiny.en-q5_1 | ac | cpu | 2/2 | 57.50 | 1.3 | 10.14 | 0.1 | 0.36 | 301 | comfortable |  |
| tiny.en-q8_0 | ac | cpu | 2/2 | 64.92 | 0.6 | 11.58 | 0.1 | 0.30 | 334 | comfortable |  |
| base | ac | vulkan | 2/2 | 49.44 | 0.4 | 18.33 | 0.2 | 0.13 | 172 | comfortable |  |
| base-q5_1 | ac | vulkan | 2/2 | 46.81 | 1.6 | 19.81 | 0.7 | 0.12 | 182 | comfortable |  |
| base-q8_0 | ac | vulkan | 2/2 | 50.76 | 0.0 | 19.81 | 1.6 | 0.12 | 191 | comfortable |  |
| base.en | ac | vulkan | 2/2 | 46.84 | 4.7 | 19.33 | 0.0 | 0.14 | 189 | comfortable |  |
| base.en-q5_1 | ac | vulkan | 2/2 | 42.44 | 1.7 | 20.16 | 0.2 | 0.14 | 188 | comfortable |  |
| base.en-q8_0 | ac | vulkan | 2/2 | 50.08 | 3.2 | 20.09 | 2.3 | 0.13 | 197 | comfortable |  |
| large-v3-turbo | ac | vulkan | 2/2 | 12.80 | 0.6 | 3.32 | 2.2 | 1.00 | 264 | comfortable |  |
| large-v3-turbo-q5_0 | ac | vulkan | 2/2 | 20.59 | 0.1 | 3.61 | 1.1 | 0.94 | 228 | comfortable |  |
| large-v3-turbo-q8_0 | ac | vulkan | 2/2 | 15.17 | 0.0 | 3.51 | 2.0 | 0.96 | 222 | comfortable |  |
| small | ac | vulkan | 2/2 | 26.31 | 0.4 | 8.05 | 0.1 | 0.29 | 193 | comfortable |  |
| small-q5_1 | ac | vulkan | 2/2 | 30.71 | 1.3 | 8.97 | 0.1 | 0.27 | 204 | comfortable |  |
| small-q8_0 | ac | vulkan | 2/2 | 30.93 | 0.2 | 8.85 | 0.1 | 0.28 | 182 | comfortable |  |
| small.en | ac | vulkan | 2/2 | 25.35 | 0.9 | 9.32 | 0.1 | 0.34 | 194 | comfortable |  |
| small.en-q5_1 | ac | vulkan | 2/2 | 26.53 | 0.8 | 9.60 | 0.1 | 0.32 | 206 | comfortable |  |
| small.en-q8_0 | ac | vulkan | 2/2 | 12.46 | 0.7 | 9.69 | 0.0 | 0.34 | 185 | comfortable |  |
| tiny | ac | vulkan | 2/2 | 69.66 | 0.8 | 23.39 | 1.4 | 0.13 | 170 | comfortable |  |
| tiny-q5_1 | ac | vulkan | 2/2 | 73.14 | 0.1 | 19.31 | 3.2 | 0.22 | 174 | comfortable |  |
| tiny-q8_0 | ac | vulkan | 2/2 | 71.06 | 3.3 | 26.25 | 4.0 | 0.09 | 184 | comfortable |  |
| tiny.en | ac | vulkan | 2/2 | 75.33 | 1.4 | 31.43 | 0.4 | 0.10 | 170 | comfortable |  |
| tiny.en-q5_1 | ac | vulkan | 2/2 | 79.48 | 2.5 | 32.53 | 0.6 | 0.10 | 177 | comfortable |  |
| tiny.en-q8_0 | ac | vulkan | 2/2 | 81.34 | 2.7 | 33.80 | 0.6 | 0.08 | 182 | comfortable |  |

