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
| base | ac | cpu | 3/3 | 13.30 | 4.6 | 2.34 | 5.3 | 1.60 | 601 | comfortable |  |
| base-q5_1 | ac | cpu | 3/3 | 11.93 | 0.2 | 2.18 | 0.7 | 1.58 | 454 | comfortable |  |
| base-q8_0 | ac | cpu | 3/3 | 18.12 | 0.4 | 3.10 | 0.9 | 1.19 | 474 | comfortable |  |
| base.en | ac | cpu | 3/3 | 15.44 | 0.3 | 2.59 | 0.1 | 1.27 | 600 | comfortable |  |
| base.en-q5_1 | ac | cpu | 3/3 | 15.28 | 0.7 | 3.18 | 0.0 | 1.18 | 432 | comfortable |  |
| base.en-q8_0 | ac | cpu | 3/3 | 18.34 | 0.9 | 3.42 | 0.2 | 0.95 | 473 | comfortable |  |
| large-v3-turbo | ac | cpu | 3/3 | 0.61 | 4.3 | 0.10 | 1.4 | 40.53 | 3665 | unsuitable |  |
| large-v3-turbo-q5_0 | ac | cpu | 3/3 | 0.70 | 1.8 | 0.11 | 0.3 | 35.71 | 1665 | unsuitable |  |
| large-v3-turbo-q8_0 | ac | cpu | 3/3 | 0.85 | 6.6 | 0.14 | 0.7 | 29.06 | 2236 | unsuitable |  |
| small | ac | cpu | 3/3 | 3.62 | 2.0 | 0.54 | 1.1 | 7.59 | 1368 | borderline |  |
| small-q5_1 | ac | cpu | 3/3 | 4.22 | 2.9 | 0.59 | 1.0 | 6.83 | 800 | borderline |  |
| small-q8_0 | ac | cpu | 3/3 | 5.17 | 1.4 | 0.75 | 0.5 | 5.16 | 941 | borderline |  |
| small.en | ac | cpu | 3/3 | 3.99 | 0.3 | 0.56 | 0.4 | 6.58 | 1371 | borderline |  |
| small.en-q5_1 | ac | cpu | 3/3 | 3.62 | 18.1 | 0.59 | 13.8 | 6.36 | 801 | borderline | batch_rtf spread 18.1% > 15% |
| small.en-q8_0 | ac | cpu | 3/3 | 4.73 | 27.8 | 0.57 | 25.5 | 5.99 | 944 | borderline | batch_rtf spread 27.8% > 15%; stream_rtf spread 25.5% > 15% |
| tiny | ac | cpu | 3/3 | 31.68 | 0.2 | 6.08 | 0.6 | 0.62 | 417 | comfortable |  |
| tiny-q5_1 | ac | cpu | 3/3 | 38.51 | 0.1 | 6.36 | 0.5 | 0.64 | 330 | comfortable |  |
| tiny-q8_0 | ac | cpu | 3/3 | 40.77 | 0.7 | 7.56 | 1.1 | 0.51 | 352 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 40.20 | 0.1 | 6.41 | 0.3 | 0.56 | 381 | comfortable |  |
| tiny.en-q5_1 | ac | cpu | 3/3 | 47.55 | 0.7 | 7.08 | 0.7 | 0.52 | 298 | comfortable |  |
| tiny.en-q8_0 | ac | cpu | 3/3 | 54.26 | 0.9 | 8.20 | 0.1 | 0.44 | 316 | comfortable |  |
| base | ac | vulkan | 3/3 | 23.52 | 2.0 | 7.29 | 0.2 | 0.41 | 183 | comfortable |  |
| base-q5_1 | ac | vulkan | 3/3 | 26.02 | 1.9 | 9.22 | 0.6 | 0.30 | 198 | comfortable |  |
| base-q8_0 | ac | vulkan | 3/3 | 24.67 | 1.9 | 8.20 | 1.7 | 0.32 | 198 | comfortable |  |
| base.en | ac | vulkan | 3/3 | 22.18 | 3.1 | 6.95 | 0.5 | 0.41 | 183 | comfortable |  |
| base.en-q5_1 | ac | vulkan | 3/3 | 24.36 | 4.7 | 9.16 | 0.5 | 0.32 | 193 | comfortable |  |
| base.en-q8_0 | ac | vulkan | 3/3 | 20.67 | 0.9 | 8.68 | 2.6 | 0.31 | 202 | comfortable |  |
| large-v3-turbo | ac | vulkan | 3/3 | 2.69 | 0.7 | 0.51 | 0.2 | 7.47 | 273 | borderline |  |
| large-v3-turbo-q5_0 | ac | vulkan | 3/3 | 5.55 | 0.4 | 0.92 | 0.1 | 3.98 | 220 | borderline |  |
| large-v3-turbo-q8_0 | ac | vulkan | 3/3 | 4.45 | 0.4 | 0.88 | 0.2 | 4.18 | 222 | borderline |  |
| small | ac | vulkan | 3/3 | 9.15 | 1.2 | 2.22 | 1.4 | 1.61 | 200 | comfortable |  |
| small-q5_1 | ac | vulkan | 3/3 | 14.30 | 1.6 | 3.43 | 0.4 | 1.02 | 206 | comfortable |  |
| small-q8_0 | ac | vulkan | 3/3 | 13.28 | 3.5 | 3.02 | 0.8 | 1.34 | 189 | comfortable |  |
| small.en | ac | vulkan | 3/3 | 5.60 | 1.8 | 2.65 | 0.7 | 1.37 | 199 | comfortable |  |
| small.en-q5_1 | ac | vulkan | 3/3 | 13.56 | 3.1 | 3.96 | 0.4 | 0.83 | 212 | comfortable |  |
| small.en-q8_0 | ac | vulkan | 3/3 | 4.92 | 1.1 | 3.60 | 0.2 | 0.91 | 189 | comfortable |  |
| tiny | ac | vulkan | 3/3 | 29.01 | 3.8 | 10.10 | 1.1 | 0.31 | 174 | comfortable |  |
| tiny-q5_1 | ac | vulkan | 3/3 | 39.61 | 1.6 | 9.87 | 3.2 | 0.46 | 183 | comfortable |  |
| tiny-q8_0 | ac | vulkan | 3/3 | 32.66 | 2.2 | 11.83 | 1.3 | 0.20 | 189 | comfortable |  |
| tiny.en | ac | vulkan | 3/3 | 37.42 | 3.1 | 12.41 | 0.8 | 0.24 | 173 | comfortable |  |
| tiny.en-q5_1 | ac | vulkan | 3/3 | 45.17 | 5.8 | 15.02 | 1.1 | 0.20 | 183 | comfortable |  |
| tiny.en-q8_0 | ac | vulkan | 3/3 | 40.42 | 4.2 | 14.19 | 0.8 | 0.19 | 189 | comfortable |  |

## i7-7500u — Intel(R) Core(TM) i7-7500U CPU @ 2.70GHz (2p/4l, 15752 MiB, laptop) — released 2016-08, legacy ultraportable (~10 years old)

_2016 Kaby Lake dual-core / 4 threads, 15 W; ~10-year-old ultrabook CPU; weakest tier we expect to support._

**Quant kernel class:** `avx2-fallback` (no AVX-VNNI). No AVX-VNNI: ggml int8 dot products use the AVX2 fallback (vpmaddubsw + vpmaddwd + vpaddd + dequantise shifts). Per-op cost is similar to fp16 FMA; quant's benefits collapse to RSS and weight bandwidth on this host. See 2026-05-21 diagnostic in summary/quant-anomaly.md.

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 3/3 | 7.48 | 0.6 | 1.23 | 1.1 | 3.11 | 602 | borderline |  |
| base-q5_1 | ac | cpu | 3/3 | 6.02 | 0.1 | 1.05 | 0.1 | 3.47 | 456 | borderline |  |
| base-q8_0 | ac | cpu | 3/3 | 8.51 | 0.2 | 1.39 | 0.1 | 2.83 | 475 | borderline |  |
| base.en | ac | cpu | 3/3 | 6.63 | 0.5 | 1.07 | 0.5 | 3.10 | 605 | borderline |  |
| base.en-q5_1 | ac | cpu | 3/3 | 5.33 | 0.3 | 1.09 | 0.0 | 3.48 | 436 | borderline |  |
| base.en-q8_0 | ac | cpu | 3/3 | 6.76 | 0.2 | 1.20 | 0.2 | 2.76 | 478 | borderline |  |
| large-v3-turbo | ac | cpu | 3/3 | 0.26 | 5.5 | 0.04 | 2.6 | 98.97 | 3666 | unsuitable |  |
| large-v3-turbo-q5_0 | ac | cpu | 3/3 | 0.21 | 20.1 | 0.03 | 16.0 | 121.33 | 1662 | unsuitable | batch_rtf spread 20.1% > 15%; stream_rtf spread 16.0% > 15% |
| large-v3-turbo-q8_0 | ac | cpu | 3/3 | 0.26 | 25.4 | 0.04 | 13.8 | 97.26 | 2235 | unsuitable | batch_rtf spread 25.4% > 15% |
| small | ac | cpu | 3/3 | 2.37 | 0.2 | 0.34 | 0.3 | 12.10 | 1367 | borderline |  |
| small-q5_1 | ac | cpu | 3/3 | 1.72 | 14.1 | 0.24 | 12.1 | 16.94 | 798 | borderline |  |
| small-q8_0 | ac | cpu | 3/3 | 2.95 | 0.3 | 0.41 | 0.0 | 9.76 | 941 | borderline |  |
| small.en | ac | cpu | 3/3 | 2.38 | 5.0 | 0.27 | 9.9 | 12.98 | 1366 | borderline |  |
| small.en-q5_1 | ac | cpu | 3/3 | 0.88 | 0.9 | 0.16 | 4.4 | 24.32 | 800 | unsuitable |  |
| small.en-q8_0 | ac | cpu | 3/3 | 2.76 | 10.5 | 0.36 | 8.9 | 11.39 | 940 | borderline |  |
| tiny | ac | cpu | 3/3 | 14.49 | 1.2 | 2.63 | 2.0 | 1.48 | 415 | comfortable |  |
| tiny-q5_1 | ac | cpu | 3/3 | 8.06 | 0.4 | 1.31 | 0.6 | 3.09 | 389 | borderline |  |
| tiny-q8_0 | ac | cpu | 3/3 | 9.62 | 8.5 | 1.68 | 5.6 | 2.36 | 412 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 10.79 | 0.9 | 1.52 | 0.4 | 2.48 | 399 | comfortable |  |
| tiny.en-q5_1 | ac | cpu | 3/3 | 8.98 | 0.8 | 1.28 | 0.4 | 2.93 | 367 | borderline |  |
| tiny.en-q8_0 | ac | cpu | 3/3 | 11.48 | 1.4 | 1.59 | 0.1 | 2.34 | 332 | comfortable |  |
| base | ac | vulkan | 3/3 | 6.97 | 0.9 | 1.76 | 0.5 | 1.91 | 224 | comfortable |  |
| base-q5_1 | ac | vulkan | 3/3 | 6.17 | 0.6 | 1.67 | 0.1 | 1.92 | 244 | comfortable |  |
| base-q8_0 | ac | vulkan | 3/3 | 6.52 | 0.2 | 1.67 | 0.3 | 1.97 | 239 | comfortable |  |
| base.en | ac | vulkan | 3/3 | 6.61 | 0.2 | 1.64 | 1.1 | 1.85 | 219 | comfortable |  |
| base.en-q5_1 | ac | vulkan | 3/3 | 6.49 | 0.9 | 1.70 | 0.1 | 1.85 | 229 | comfortable |  |
| base.en-q8_0 | ac | vulkan | 3/3 | 7.24 | 0.3 | 1.65 | 0.1 | 1.85 | 240 | comfortable |  |
| large-v3-turbo | ac | vulkan | 2/2 | 0.44 | 24.0 | 0.10 | 2.8 | 36.43 | 278 | unsuitable | batch_rtf spread 24.0% > 15% |
| large-v3-turbo-q5_0 | ac | vulkan | 3/3 | 0.73 | 1.5 | 0.12 | 0.1 | 33.33 | 269 | unsuitable |  |
| large-v3-turbo-q8_0 | ac | vulkan | 3/3 | 0.68 | 0.2 | 0.12 | 0.1 | 32.95 | 269 | unsuitable |  |
| small | ac | vulkan | 3/3 | 2.86 | 0.3 | 0.56 | 0.2 | 6.61 | 234 | borderline |  |
| small-q5_1 | ac | vulkan | 3/3 | 3.22 | 0.2 | 0.58 | 0.2 | 6.35 | 249 | borderline |  |
| small-q8_0 | ac | vulkan | 3/3 | 2.93 | 1.4 | 0.55 | 1.1 | 6.96 | 232 | borderline |  |
| small.en | ac | vulkan | 3/3 | 1.96 | 0.3 | 0.58 | 0.4 | 6.29 | 232 | borderline |  |
| small.en-q5_1 | ac | vulkan | 3/3 | 0.43 | 93.3 | 0.18 | 39.3 | 13.32 | 216 | unsuitable | batch_rtf spread 93.3% > 15%; stream_rtf spread 39.3% > 15% |
| small.en-q8_0 | ac | vulkan | 3/3 | 0.57 | 115.2 | 0.24 | 77.9 | 13.21 | 230 | unsuitable | batch_rtf spread 115.2% > 15%; stream_rtf spread 77.9% > 15% |
| tiny | ac | vulkan | 3/3 | 9.59 | 0.6 | 2.73 | 1.1 | 1.19 | 219 | comfortable |  |
| tiny-q5_1 | ac | vulkan | 3/3 | 9.18 | 0.4 | 2.32 | 0.3 | 1.82 | 223 | comfortable |  |
| tiny-q8_0 | ac | vulkan | 3/3 | 9.04 | 1.3 | 2.78 | 0.4 | 1.01 | 230 | comfortable |  |
| tiny.en | ac | vulkan | 3/3 | 11.74 | 1.7 | 3.11 | 0.5 | 1.00 | 213 | comfortable |  |
| tiny.en-q5_1 | ac | vulkan | 3/3 | 11.79 | 17.1 | 3.08 | 8.7 | 1.02 | 221 | comfortable | batch_rtf spread 17.1% > 15% |
| tiny.en-q8_0 | ac | vulkan | 3/3 | 11.43 | 0.9 | 3.07 | 1.2 | 1.00 | 195 | comfortable |  |

## i7-8550u — Intel(R) Core(TM) i7-8550U CPU @ 1.80GHz (4p/8l, 15750 MiB, laptop) — released 2017-08, legacy ultraportable

_2017 Kaby Lake-R quad-core / 8 threads, 15 W; first ULV quad-core generation; ThinkPad X1 Carbon Gen 6 (20KH) class; bridges the 2016 i7-7500u and the 2022 i7-1255u in our laptop roster._

**Quant kernel class:** `avx2-fallback` (no AVX-VNNI). No AVX-VNNI: ggml int8 dot products use the AVX2 fallback. Quantisation produces zero throughput gain over fp16 on this host (sometimes uses slightly more user-time CPU than fp16); only RSS reductions are real. Build also requires GGML_NATIVE=OFF GGML_AVX_VNNI=OFF when cross-compiled from a VNNI-capable host to avoid SIGILL.

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 3/3 | 5.52 | 5.6 | 0.94 | 7.8 | 4.10 | 685 | borderline |  |
| base-q5_1 | ac | cpu | 3/3 | 4.44 | 0.5 | 0.80 | 0.3 | 4.55 | 457 | borderline |  |
| base-q8_0 | ac | cpu | 3/3 | 6.28 | 2.0 | 1.08 | 3.9 | 3.63 | 475 | borderline |  |
| base.en | ac | cpu | 3/3 | 5.11 | 0.9 | 0.85 | 0.2 | 3.93 | 605 | borderline |  |
| base.en-q5_1 | ac | cpu | 3/3 | 3.81 | 1.9 | 0.81 | 1.8 | 4.66 | 432 | borderline |  |
| base.en-q8_0 | ac | cpu | 3/3 | 4.90 | 1.9 | 0.92 | 0.2 | 3.64 | 474 | borderline |  |
| large-v3-turbo | ac | cpu | 3/3 | 0.56 | 19.8 | 0.09 | 16.0 | 44.38 | 3666 | unsuitable | batch_rtf spread 19.8% > 15%; stream_rtf spread 16.0% > 15% |
| large-v3-turbo-q5_0 | ac | cpu | 3/3 | 0.60 | 0.1 | 0.09 | 0.0 | 43.55 | 1662 | unsuitable |  |
| large-v3-turbo-q8_0 | ac | cpu | 3/3 | 0.77 | 0.1 | 0.12 | 0.1 | 34.14 | 2235 | unsuitable |  |
| small | ac | cpu | 3/3 | 2.01 | 44.2 | 0.29 | 31.6 | 14.09 | 1367 | borderline | batch_rtf spread 44.2% > 15%; stream_rtf spread 31.6% > 15% |
| small-q5_1 | ac | cpu | 3/3 | 1.81 | 3.2 | 0.26 | 3.9 | 15.38 | 799 | borderline |  |
| small-q8_0 | ac | cpu | 3/3 | 2.46 | 42.2 | 0.35 | 27.2 | 11.37 | 1020 | borderline | batch_rtf spread 42.2% > 15%; stream_rtf spread 27.2% > 15% |
| small.en | ac | cpu | 3/3 | 2.00 | 13.4 | 0.28 | 8.9 | 13.68 | 1364 | borderline |  |
| small.en-q5_1 | ac | cpu | 3/3 | 1.74 | 17.7 | 0.28 | 10.1 | 13.62 | 800 | borderline | batch_rtf spread 17.7% > 15% |
| small.en-q8_0 | ac | cpu | 3/3 | 2.33 | 11.7 | 0.31 | 13.1 | 12.32 | 939 | borderline |  |
| tiny | ac | cpu | 3/3 | 11.83 | 6.4 | 2.26 | 3.7 | 1.70 | 417 | comfortable |  |
| tiny-q5_1 | ac | cpu | 3/3 | 10.63 | 5.0 | 1.77 | 2.9 | 2.30 | 329 | comfortable |  |
| tiny-q8_0 | ac | cpu | 3/3 | 11.33 | 7.7 | 2.09 | 2.4 | 1.85 | 352 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 13.49 | 1.8 | 2.03 | 0.2 | 1.81 | 401 | comfortable |  |
| tiny.en-q5_1 | ac | cpu | 3/3 | 12.09 | 3.8 | 1.76 | 2.4 | 2.12 | 313 | comfortable |  |
| tiny.en-q8_0 | ac | cpu | 3/3 | 14.69 | 1.1 | 2.17 | 2.9 | 1.74 | 337 | comfortable |  |
| base | ac | vulkan | 3/3 | 10.08 | 1.0 | 2.28 | 0.2 | 1.55 | 183 | comfortable |  |
| base-q5_1 | ac | vulkan | 3/3 | 8.82 | 0.2 | 2.19 | 0.1 | 1.55 | 201 | comfortable |  |
| base-q8_0 | ac | vulkan | 3/3 | 9.37 | 0.1 | 2.21 | 0.1 | 1.56 | 198 | comfortable |  |
| base.en | ac | vulkan | 3/3 | 9.27 | 0.5 | 2.06 | 0.2 | 1.54 | 183 | comfortable |  |
| base.en-q5_1 | ac | vulkan | 3/3 | 7.82 | 1.1 | 2.02 | 0.3 | 1.58 | 194 | comfortable |  |
| base.en-q8_0 | ac | vulkan | 3/3 | 9.02 | 0.2 | 2.00 | 0.2 | 1.57 | 203 | comfortable |  |
| large-v3-turbo | ac | vulkan | 3/3 | 0.80 | 0.1 | 0.14 | 0.2 | 28.74 | 273 | unsuitable |  |
| large-v3-turbo-q5_0 | ac | vulkan | 3/3 | 0.83 | 0.7 | 0.13 | 0.1 | 29.37 | 238 | unsuitable |  |
| large-v3-turbo-q8_0 | ac | vulkan | 3/3 | 0.77 | 0.1 | 0.13 | 0.1 | 29.27 | 222 | unsuitable |  |
| small | ac | vulkan | 3/3 | 3.29 | 1.0 | 0.64 | 1.5 | 5.87 | 200 | borderline |  |
| small-q5_1 | ac | vulkan | 3/3 | 3.75 | 0.1 | 0.67 | 0.3 | 5.49 | 208 | borderline |  |
| small-q8_0 | ac | vulkan | 3/3 | 3.45 | 0.5 | 0.64 | 0.3 | 6.06 | 190 | borderline |  |
| small.en | ac | vulkan | 3/3 | 2.39 | 0.2 | 0.68 | 0.5 | 5.45 | 200 | borderline |  |
| small.en-q5_1 | ac | vulkan | 3/3 | 3.05 | 1.2 | 0.68 | 1.1 | 5.34 | 210 | borderline |  |
| small.en-q8_0 | ac | vulkan | 3/3 | 1.80 | 0.3 | 0.66 | 0.2 | 5.51 | 189 | borderline |  |
| tiny | ac | vulkan | 3/3 | 14.48 | 2.2 | 3.69 | 1.0 | 0.93 | 178 | comfortable |  |
| tiny-q5_1 | ac | vulkan | 3/3 | 13.59 | 1.9 | 3.17 | 0.4 | 1.36 | 183 | comfortable |  |
| tiny-q8_0 | ac | vulkan | 3/3 | 14.07 | 1.2 | 3.84 | 0.4 | 0.80 | 189 | comfortable |  |
| tiny.en | ac | vulkan | 3/3 | 17.15 | 4.3 | 4.12 | 1.2 | 0.80 | 180 | comfortable |  |
| tiny.en-q5_1 | ac | vulkan | 3/3 | 17.39 | 7.5 | 4.04 | 0.1 | 0.82 | 187 | comfortable |  |
| tiny.en-q8_0 | ac | vulkan | 3/3 | 17.28 | 2.4 | 4.09 | 0.2 | 0.80 | 189 | comfortable |  |

## ryzen-5950x — AMD Ryzen 9 5950X 16-Core Processor (16p/32l, 49152 MiB, container) — released 2020-11, high-end desktop

_2020 Zen 3 16-core enthusiast desktop; AMD's flagship consumer CPU at launch, still strong in 2026._

**Quant kernel class:** `avx2-fallback` (no AVX-VNNI). Zen 3 (2020): no AVX-VNNI (added in Zen 4, 2022). ggml int8 path uses the AVX2 fallback. Quant speedup vs fp16 is bandwidth-only here; on 16 fast cores + dual-channel DDR4 the headroom is large enough that quant still wins, but not by the VNNI multiplier.

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 3/3 | 41.73 | 1.3 | 8.72 | 0.8 | 0.38 | 598 | comfortable |  |
| base-q5_1 | ac | cpu | 3/3 | 42.81 | 1.3 | 7.97 | 7.9 | 0.41 | 452 | comfortable |  |
| base-q8_0 | ac | cpu | 3/3 | 50.15 | 4.7 | 9.57 | 0.4 | 0.37 | 472 | comfortable |  |
| base.en | ac | cpu | 3/3 | 38.44 | 3.5 | 8.17 | 1.4 | 0.39 | 598 | comfortable |  |
| base.en-q5_1 | ac | cpu | 3/3 | 39.32 | 5.1 | 8.68 | 1.1 | 0.42 | 430 | comfortable |  |
| base.en-q8_0 | ac | cpu | 3/3 | 41.65 | 0.2 | 8.74 | 0.9 | 0.37 | 475 | comfortable |  |
| large-v3-turbo | ac | cpu | 3/3 | 4.64 | 0.6 | 0.73 | 0.1 | 5.22 | 3661 | borderline |  |
| large-v3-turbo-q5_0 | ac | cpu | 3/3 | 4.42 | 0.4 | 0.67 | 0.1 | 5.89 | 1658 | borderline |  |
| large-v3-turbo-q8_0 | ac | cpu | 3/3 | 5.04 | 0.5 | 0.79 | 0.2 | 4.89 | 2230 | borderline |  |
| small | ac | cpu | 3/3 | 14.72 | 0.2 | 2.76 | 0.2 | 1.47 | 1363 | comfortable |  |
| small-q5_1 | ac | cpu | 3/3 | 17.87 | 0.8 | 2.85 | 1.4 | 1.38 | 795 | comfortable |  |
| small-q8_0 | ac | cpu | 3/3 | 19.16 | 0.4 | 3.23 | 1.6 | 1.19 | 937 | comfortable |  |
| small.en | ac | cpu | 3/3 | 16.19 | 2.8 | 2.95 | 0.2 | 1.20 | 1364 | comfortable |  |
| small.en-q5_1 | ac | cpu | 3/3 | 15.10 | 0.8 | 2.83 | 0.6 | 1.31 | 797 | comfortable |  |
| small.en-q8_0 | ac | cpu | 3/3 | 20.67 | 1.8 | 3.23 | 0.5 | 1.11 | 940 | comfortable |  |
| tiny | ac | cpu | 3/3 | 69.36 | 0.8 | 14.97 | 2.8 | 0.24 | 414 | comfortable |  |
| tiny-q5_1 | ac | cpu | 3/3 | 85.70 | 0.7 | 14.79 | 4.7 | 0.28 | 325 | comfortable |  |
| tiny-q8_0 | ac | cpu | 3/3 | 79.98 | 3.4 | 16.16 | 1.3 | 0.22 | 354 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 83.43 | 0.6 | 15.43 | 3.9 | 0.22 | 394 | comfortable |  |
| tiny.en-q5_1 | ac | cpu | 3/3 | 101.58 | 1.6 | 16.71 | 2.7 | 0.21 | 308 | comfortable |  |
| tiny.en-q8_0 | ac | cpu | 3/3 | 101.76 | 1.4 | 16.58 | 3.2 | 0.20 | 330 | comfortable |  |
| base | ac | vulkan | 3/3 | 121.28 | 1.0 | 63.10 | 0.5 | 0.03 | 334 | comfortable |  |
| base-q5_1 | ac | vulkan | 3/3 | 121.91 | 2.0 | 66.69 | 0.7 | 0.03 | 269 | comfortable |  |
| base-q8_0 | ac | vulkan | 3/3 | 125.96 | 1.1 | 65.75 | 1.5 | 0.03 | 267 | comfortable |  |
| base.en | ac | vulkan | 3/3 | 95.64 | 3.4 | 67.84 | 1.0 | 0.04 | 304 | comfortable |  |
| base.en-q5_1 | ac | vulkan | 3/3 | 92.19 | 0.9 | 71.64 | 0.7 | 0.04 | 260 | comfortable |  |
| base.en-q8_0 | ac | vulkan | 3/3 | 107.13 | 2.5 | 72.79 | 0.8 | 0.04 | 270 | comfortable |  |
| large-v3-turbo | ac | vulkan | 3/3 | 87.59 | 0.7 | 32.62 | 0.2 | 0.10 | 460 | comfortable |  |
| large-v3-turbo-q5_0 | ac | vulkan | 3/3 | 100.24 | 19.1 | 34.04 | 28.0 | 0.10 | 574 | comfortable | batch_rtf spread 19.1% > 15%; stream_rtf spread 28.0% > 15% |
| large-v3-turbo-q8_0 | ac | vulkan | 3/3 | 97.49 | 4.2 | 34.06 | 0.2 | 0.10 | 347 | comfortable |  |
| small | ac | vulkan | 3/3 | 81.46 | 0.3 | 31.01 | 0.1 | 0.13 | 340 | comfortable |  |
| small-q5_1 | ac | vulkan | 3/3 | 90.10 | 0.5 | 39.47 | 0.1 | 0.06 | 301 | comfortable |  |
| small-q8_0 | ac | vulkan | 3/3 | 90.50 | 1.1 | 35.46 | 0.6 | 0.09 | 300 | comfortable |  |
| small.en | ac | vulkan | 3/3 | 70.95 | 0.2 | 40.42 | 0.6 | 0.08 | 341 | comfortable |  |
| small.en-q5_1 | ac | vulkan | 3/3 | 79.70 | 2.4 | 42.71 | 1.1 | 0.07 | 300 | comfortable |  |
| small.en-q8_0 | ac | vulkan | 3/3 | 77.29 | 1.1 | 43.54 | 1.5 | 0.07 | 296 | comfortable |  |
| tiny | ac | vulkan | 3/3 | 140.15 | 40.6 | 68.43 | 37.6 | 0.04 | 374 | comfortable | batch_rtf spread 40.6% > 15%; stream_rtf spread 37.6% > 15% |
| tiny-q5_1 | ac | vulkan | 3/3 | 138.13 | 43.8 | 58.32 | 35.8 | 0.07 | 540 | comfortable | batch_rtf spread 43.8% > 15%; stream_rtf spread 35.8% > 15% |
| tiny-q8_0 | ac | vulkan | 3/3 | 99.33 | 37.6 | 52.66 | 26.1 | 0.04 | 451 | comfortable | batch_rtf spread 37.6% > 15%; stream_rtf spread 26.1% > 15% |
| tiny.en | ac | vulkan | 3/3 | 116.70 | 18.1 | 91.61 | 4.4 | 0.03 | 274 | comfortable | batch_rtf spread 18.1% > 15% |
| tiny.en-q5_1 | ac | vulkan | 3/3 | 124.09 | 27.2 | 93.51 | 0.6 | 0.03 | 281 | comfortable | batch_rtf spread 27.2% > 15% |
| tiny.en-q8_0 | ac | vulkan | 3/3 | 128.70 | 24.0 | 94.71 | 1.7 | 0.03 | 288 | comfortable | batch_rtf spread 24.0% > 15% |

## ultra7-258v — Intel(R) Core(TM) Ultra 7 258V (8p/8l, 31572 MiB, laptop) — released 2024-09, premium ultraportable (current)

_2024 Lunar Lake 4P+4LP-E (no SMT); Intel's current efficiency flagship for thin-and-light laptops, with on-package LPDDR5X and Xe2 Battlemage iGPU._

**Quant kernel class:** `vnni` (AVX-VNNI). Lunar Lake Lion Cove P-cores have AVX-VNNI (no AVX-512; Intel removed it from consumer Core Ultra). Int8 dot products use vpdpbusd; quant gives the full 1.5-3x throughput uplift over fp16, plus small.en-q8_0 promotes from borderline to comfortable for live streaming.

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 3/3 | 22.40 | 1.3 | 4.09 | 1.6 | 0.90 | 603 | comfortable |  |
| base-q5_1 | ac | cpu | 3/3 | 19.44 | 0.6 | 3.74 | 0.5 | 0.90 | 457 | comfortable |  |
| base-q8_0 | ac | cpu | 3/3 | 27.33 | 0.2 | 5.05 | 0.1 | 0.74 | 478 | comfortable |  |
| base.en | ac | cpu | 3/3 | 22.35 | 0.4 | 4.15 | 0.2 | 0.78 | 607 | comfortable |  |
| base.en-q5_1 | ac | cpu | 3/3 | 21.21 | 0.6 | 4.94 | 0.4 | 0.75 | 436 | comfortable |  |
| base.en-q8_0 | ac | cpu | 3/3 | 24.57 | 0.1 | 5.16 | 0.1 | 0.63 | 482 | comfortable |  |
| large-v3-turbo | ac | cpu | 3/3 | 0.96 | 13.0 | 0.15 | 12.9 | 25.52 | 3674 | unsuitable |  |
| large-v3-turbo-q5_0 | ac | cpu | 3/3 | 0.73 | 28.3 | 0.13 | 15.9 | 29.35 | 1671 | unsuitable | batch_rtf spread 28.3% > 15%; stream_rtf spread 15.9% > 15% |
| large-v3-turbo-q8_0 | ac | cpu | 3/3 | 1.10 | 12.0 | 0.18 | 6.6 | 21.34 | 2243 | borderline |  |
| small | ac | cpu | 3/3 | 6.31 | 0.4 | 0.96 | 0.6 | 4.31 | 1370 | borderline |  |
| small-q5_1 | ac | cpu | 3/3 | 6.76 | 0.0 | 0.99 | 0.3 | 4.02 | 805 | borderline |  |
| small-q8_0 | ac | cpu | 3/3 | 8.68 | 0.3 | 1.26 | 0.2 | 3.09 | 945 | borderline |  |
| small.en | ac | cpu | 3/3 | 7.15 | 0.1 | 1.02 | 0.8 | 3.64 | 1376 | borderline |  |
| small.en-q5_1 | ac | cpu | 3/3 | 6.27 | 0.4 | 1.04 | 0.4 | 3.50 | 804 | borderline |  |
| small.en-q8_0 | ac | cpu | 3/3 | 10.52 | 1.2 | 1.35 | 1.0 | 2.73 | 948 | borderline |  |
| tiny | ac | cpu | 3/3 | 44.66 | 0.2 | 9.45 | 0.7 | 0.39 | 420 | comfortable |  |
| tiny-q5_1 | ac | cpu | 3/3 | 50.49 | 0.3 | 9.41 | 0.1 | 0.42 | 332 | comfortable |  |
| tiny-q8_0 | ac | cpu | 3/3 | 52.85 | 0.2 | 11.40 | 0.0 | 0.33 | 355 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 55.07 | 0.6 | 9.69 | 0.1 | 0.37 | 404 | comfortable |  |
| tiny.en-q5_1 | ac | cpu | 3/3 | 59.29 | 0.5 | 10.24 | 0.1 | 0.36 | 302 | comfortable |  |
| tiny.en-q8_0 | ac | cpu | 3/3 | 66.46 | 0.2 | 11.71 | 0.1 | 0.30 | 334 | comfortable |  |
| base | ac | vulkan | 3/3 | 26.91 | 0.4 | 9.00 | 0.2 | 0.29 | 212 | comfortable |  |
| base-q5_1 | ac | vulkan | 3/3 | 34.13 | 33.3 | 12.35 | 43.7 | 0.19 | 227 | comfortable | batch_rtf spread 33.3% > 15%; stream_rtf spread 43.7% > 15% |
| base-q8_0 | ac | vulkan | 3/3 | 57.37 | 30.8 | 21.70 | 34.5 | 0.12 | 194 | comfortable | batch_rtf spread 30.8% > 15%; stream_rtf spread 34.5% > 15% |
| base.en | ac | vulkan | 3/3 | 27.57 | 44.0 | 9.12 | 57.3 | 0.29 | 212 | comfortable | batch_rtf spread 44.0% > 15%; stream_rtf spread 57.3% > 15% |
| base.en-q5_1 | ac | vulkan | 3/3 | 22.30 | 0.4 | 8.82 | 0.1 | 0.31 | 219 | comfortable |  |
| base.en-q8_0 | ac | vulkan | 3/3 | 27.28 | 1.0 | 9.00 | 0.4 | 0.29 | 229 | comfortable |  |
| large-v3-turbo | ac | vulkan | 3/3 | 5.20 | 10.4 | 1.49 | 15.1 | 2.48 | 306 | borderline | stream_rtf spread 15.1% > 15% |
| large-v3-turbo-q5_0 | ac | vulkan | 3/3 | 18.85 | 2.5 | 3.65 | 0.1 | 0.97 | 232 | comfortable |  |
| large-v3-turbo-q8_0 | ac | vulkan | 3/3 | 8.20 | 3.0 | 1.98 | 7.2 | 1.95 | 231 | comfortable |  |
| small | ac | vulkan | 3/3 | 13.48 | 37.8 | 3.56 | 49.8 | 0.96 | 225 | comfortable | batch_rtf spread 37.8% > 15%; stream_rtf spread 49.8% > 15% |
| small-q5_1 | ac | vulkan | 3/3 | 13.46 | 0.6 | 3.82 | 0.2 | 0.72 | 243 | comfortable |  |
| small-q8_0 | ac | vulkan | 3/3 | 14.90 | 0.1 | 3.69 | 0.2 | 0.96 | 224 | comfortable |  |
| small.en | ac | vulkan | 3/3 | 14.32 | 0.2 | 3.91 | 0.1 | 0.82 | 193 | comfortable |  |
| small.en-q5_1 | ac | vulkan | 3/3 | 14.61 | 0.5 | 3.92 | 0.2 | 0.80 | 243 | comfortable |  |
| small.en-q8_0 | ac | vulkan | 3/3 | 6.79 | 66.0 | 3.91 | 85.7 | 0.85 | 223 | comfortable | batch_rtf spread 66.0% > 15%; stream_rtf spread 85.7% > 15% |
| tiny | ac | vulkan | 3/3 | 37.54 | 0.3 | 12.13 | 0.3 | 0.25 | 208 | comfortable |  |
| tiny-q5_1 | ac | vulkan | 3/3 | 37.04 | 0.8 | 10.40 | 0.2 | 0.36 | 210 | comfortable |  |
| tiny-q8_0 | ac | vulkan | 3/3 | 48.79 | 39.4 | 16.72 | 44.3 | 0.17 | 183 | comfortable | batch_rtf spread 39.4% > 15%; stream_rtf spread 44.3% > 15% |
| tiny.en | ac | vulkan | 3/3 | 47.78 | 2.1 | 15.60 | 0.5 | 0.17 | 205 | comfortable |  |
| tiny.en-q5_1 | ac | vulkan | 3/3 | 48.33 | 1.0 | 15.45 | 0.4 | 0.18 | 178 | comfortable |  |
| tiny.en-q8_0 | ac | vulkan | 3/3 | 47.62 | 0.7 | 15.61 | 0.2 | 0.17 | 215 | comfortable |  |

