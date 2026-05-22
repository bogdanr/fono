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
| base | ac | cpu | 3/3 | 6.04 | 1.2 | 2.14 | 3.3 | 1.55 | 584 | comfortable |  |
| base-q5_1 | ac | cpu | 2/2 | 17.16 | 4.7 | 9.37 | 1.2 | 0.30 | 180 | comfortable |  |
| base-q8_0 | ac | cpu | 2/2 | 26.36 | 0.5 | 7.97 | 0.7 | 0.31 | 188 | comfortable |  |
| base.en | ac | cpu | 3/3 | 5.94 | 6.7 | 2.24 | 42.1 | 1.24 | 591 | comfortable | stream_rtf spread 42.1% > 15% |
| base.en-q5_1 | ac | cpu | 2/2 | 25.76 | 0.0 | 9.44 | 0.3 | 0.29 | 185 | comfortable |  |
| base.en-q8_0 | ac | cpu | 2/2 | 22.08 | 0.6 | 8.65 | 0.9 | 0.31 | 194 | comfortable |  |
| large-v3-turbo | ac | cpu | 1/1 | 0.33 | — | 0.10 | — | 36.63 | 3649 | unsuitable |  |
| large-v3-turbo-q5_0 | ac | cpu | 2/2 | 4.51 | 16.2 | 0.84 | 3.5 | 4.40 | 212 | borderline | batch_rtf spread 16.2% > 15% |
| large-v3-turbo-q8_0 | ac | cpu | 2/2 | 4.43 | 4.8 | 0.81 | 2.4 | 4.57 | 211 | borderline |  |
| small | ac | cpu | 3/3 | 1.62 | 1.3 | 0.56 | 9.2 | 6.07 | 1356 | borderline |  |
| small-q5_1 | ac | cpu | 2/2 | 15.72 | 1.4 | 3.33 | 1.1 | 1.00 | 198 | comfortable |  |
| small-q8_0 | ac | cpu | 2/2 | 13.91 | 0.6 | 3.11 | 0.4 | 1.07 | 178 | comfortable |  |
| small.en | ac | cpu | 3/3 | 1.94 | 3.6 | 0.66 | 1.6 | 5.46 | 1361 | borderline |  |
| small.en-q5_1 | ac | cpu | 2/2 | 14.75 | 1.6 | 3.95 | 1.6 | 0.83 | 200 | comfortable |  |
| small.en-q8_0 | ac | cpu | 2/2 | 4.52 | 1.7 | 3.74 | 0.1 | 0.90 | 180 | comfortable |  |
| tiny | ac | cpu | 3/3 | 11.49 | 34.2 | 4.46 | 26.9 | 0.65 | 420 | comfortable | batch_rtf spread 34.2% > 15%; stream_rtf spread 26.9% > 15% |
| tiny-q5_1 | ac | cpu | 2/2 | 38.09 | 29.9 | 10.47 | 3.8 | 0.45 | 174 | comfortable | batch_rtf spread 29.9% > 15% |
| tiny-q8_0 | ac | cpu | 2/2 | 38.93 | 1.0 | 12.45 | 0.6 | 0.20 | 184 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 21.12 | 21.4 | 6.72 | 1.1 | 0.47 | 411 | comfortable | batch_rtf spread 21.4% > 15% |
| tiny.en-q5_1 | ac | cpu | 2/2 | 51.34 | 1.6 | 17.03 | 0.6 | 0.19 | 176 | comfortable |  |
| tiny.en-q8_0 | ac | cpu | 2/2 | 47.04 | 2.3 | 15.69 | 0.3 | 0.18 | 182 | comfortable |  |
| base | ac | vulkan | 3/3 | 25.43 | 16.4 | 7.19 | 0.1 | 0.40 | 180 | comfortable | batch_rtf spread 16.4% > 15% |
| base-q5_1 | ac | vulkan | 2/2 | 17.36 | 2.7 | 9.42 | 0.1 | 0.29 | 188 | comfortable |  |
| base-q8_0 | ac | vulkan | 2/2 | 27.17 | 2.1 | 8.22 | 0.2 | 0.30 | 202 | comfortable |  |
| base.en | ac | vulkan | 3/3 | 16.74 | 4.1 | 7.18 | 0.1 | 0.39 | 182 | comfortable |  |
| base.en-q5_1 | ac | vulkan | 2/2 | 26.56 | 0.3 | 9.71 | 0.0 | 0.28 | 193 | comfortable |  |
| base.en-q8_0 | ac | vulkan | 2/2 | 23.31 | 1.7 | 8.89 | 0.4 | 0.30 | 201 | comfortable |  |
| large-v3-turbo | ac | vulkan | 3/3 | 2.95 | 31.2 | 0.52 | 3.2 | 6.81 | 276 | borderline | batch_rtf spread 31.2% > 15% |
| large-v3-turbo-q5_0 | ac | vulkan | 2/2 | 7.01 | 3.0 | 0.93 | 6.6 | 3.75 | 227 | borderline |  |
| large-v3-turbo-q8_0 | ac | vulkan | 2/2 | 6.33 | 6.4 | 0.90 | 6.1 | 3.87 | 224 | borderline |  |
| small | ac | vulkan | 3/3 | 9.89 | 27.9 | 2.20 | 8.4 | 1.36 | 201 | comfortable | batch_rtf spread 27.9% > 15% |
| small-q5_1 | ac | vulkan | 2/2 | 12.67 | 8.8 | 2.98 | 17.1 | 1.13 | 206 | comfortable | stream_rtf spread 17.1% > 15% |
| small-q8_0 | ac | vulkan | 2/2 | 13.26 | 5.5 | 2.90 | 9.4 | 1.15 | 186 | comfortable |  |
| small.en | ac | vulkan | 3/3 | 7.11 | 1.7 | 2.74 | 0.1 | 1.31 | 200 | comfortable |  |
| small.en-q5_1 | ac | vulkan | 2/2 | 14.71 | 2.1 | 4.02 | 0.4 | 0.82 | 207 | comfortable |  |
| small.en-q8_0 | ac | vulkan | 2/2 | 4.29 | 7.7 | 3.68 | 2.7 | 0.91 | 188 | comfortable |  |
| tiny | ac | vulkan | 3/3 | 37.69 | 15.1 | 10.68 | 1.3 | 0.30 | 180 | comfortable | batch_rtf spread 15.1% > 15% |
| tiny-q5_1 | ac | vulkan | 2/2 | 42.50 | 13.6 | 10.87 | 0.5 | 0.44 | 189 | comfortable |  |
| tiny-q8_0 | ac | vulkan | 2/2 | 37.21 | 1.8 | 12.65 | 0.4 | 0.19 | 209 | comfortable |  |
| tiny.en | ac | vulkan | 3/3 | 30.74 | 2.9 | 13.53 | 2.4 | 0.24 | 176 | comfortable |  |
| tiny.en-q5_1 | ac | vulkan | 2/2 | 45.54 | 6.9 | 16.96 | 1.8 | 0.20 | 187 | comfortable |  |
| tiny.en-q8_0 | ac | vulkan | 2/2 | 44.85 | 3.2 | 15.60 | 1.0 | 0.18 | 189 | comfortable |  |
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

**Quant kernel class:** `avx2-fallback` (no AVX-VNNI). No AVX-VNNI: ggml int8 dot products use the AVX2 fallback (vpmaddubsw + vpmaddwd + vpaddd + dequantise shifts). Per-op cost is similar to fp16 FMA; quant's benefits collapse to RSS and weight bandwidth on this host. See 2026-05-21 diagnostic in summary/quant-anomaly.md.

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 3/3 | 1.70 | 73.1 | 1.16 | 10.6 | 3.44 | 585 | borderline | batch_rtf spread 73.1% > 15% |
| base.en | ac | cpu | 3/3 | 3.15 | 0.3 | 1.14 | 0.3 | 2.81 | 590 | borderline |  |
| large-v3-turbo | ac | cpu | 1/1 | 0.21 | — | — | — | — | 1959 | unsuitable |  |
| small | ac | cpu | 3/3 | 0.99 | 0.4 | 0.33 | 0.5 | 10.87 | 1356 | unsuitable |  |
| small.en | ac | cpu | 3/3 | 1.08 | 14.8 | 0.32 | 17.2 | 12.23 | 1362 | borderline | stream_rtf spread 17.2% > 15% |
| tiny | ac | cpu | 3/3 | 7.29 | 1.7 | 2.52 | 1.5 | 1.33 | 419 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 8.60 | 0.3 | 2.97 | 0.3 | 1.24 | 400 | comfortable |  |
| base | ac | vulkan | 2/2 | 9.44 | 0.3 | 2.01 | 0.4 | 1.59 | 187 | comfortable |  |
| base-q5_1 | ac | vulkan | 2/2 | 8.71 | 0.2 | 1.96 | 0.1 | 1.62 | 195 | comfortable |  |
| base-q8_0 | ac | vulkan | 2/2 | 8.31 | 0.2 | 1.87 | 0.1 | 1.69 | 204 | comfortable |  |
| large-v3-turbo | ac | vulkan | 2/2 | 0.96 | 0.3 | 0.13 | 0.2 | 27.04 | 279 | unsuitable |  |
| large-v3-turbo-q5_0 | ac | vulkan | 2/2 | 1.00 | 0.2 | 0.13 | 1.4 | 28.27 | 260 | borderline |  |
| large-v3-turbo-q8_0 | ac | vulkan | 2/2 | 0.81 | 0.8 | 0.13 | 0.7 | 28.23 | 226 | unsuitable |  |
| small | ac | vulkan | 2/2 | 3.63 | 0.7 | 0.62 | 0.2 | 5.29 | 207 | borderline |  |
| small-q5_1 | ac | vulkan | 2/2 | 3.66 | 0.4 | 0.58 | 0.3 | 6.03 | 211 | borderline |  |
| small-q8_0 | ac | vulkan | 2/2 | 3.32 | 0.6 | 0.56 | 0.0 | 6.22 | 194 | borderline |  |
| tiny | ac | vulkan | 2/2 | 15.26 | 3.8 | 3.40 | 2.1 | 0.95 | 180 | comfortable |  |
| tiny-q5_1 | ac | vulkan | 2/2 | 12.38 | 23.1 | 2.95 | 1.7 | 1.40 | 224 | comfortable | batch_rtf spread 23.1% > 15% |
| tiny-q8_0 | ac | vulkan | 2/2 | 13.23 | 5.6 | 3.36 | 0.5 | 0.84 | 222 | comfortable |  |

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
| large-v3-turbo | ac | vulkan | 2/2 | 0.88 | 1.7 | 0.13 | 0.8 | 28.46 | 272 | unsuitable |  |
| large-v3-turbo-q5_0 | ac | vulkan | 2/2 | 1.00 | 2.4 | 0.13 | 3.2 | 28.45 | 238 | borderline |  |
| large-v3-turbo-q8_0 | ac | vulkan | 2/2 | 0.88 | 1.6 | 0.12 | 0.9 | 28.88 | 222 | unsuitable |  |
| small | ac | vulkan | 2/2 | 3.66 | 0.7 | 0.61 | 0.3 | 5.43 | 199 | borderline |  |
| small-q5_1 | ac | vulkan | 2/2 | 3.71 | 0.2 | 0.58 | 0.0 | 6.08 | 205 | borderline |  |
| small-q8_0 | ac | vulkan | 2/2 | 3.43 | 0.9 | 0.56 | 0.1 | 6.20 | 186 | borderline |  |
| tiny | ac | vulkan | 2/2 | 14.01 | 3.7 | 3.21 | 0.2 | 1.00 | 179 | comfortable |  |
| tiny-q5_1 | ac | vulkan | 2/2 | 11.95 | 20.1 | 2.77 | 0.4 | 1.55 | 198 | comfortable | batch_rtf spread 20.1% > 15% |
| tiny-q8_0 | ac | vulkan | 2/2 | 11.94 | 6.4 | 3.39 | 1.0 | 0.81 | 212 | comfortable |  |

## ryzen-5950x — AMD Ryzen 9 5950X 16-Core Processor (16p/32l, 49152 MiB, container) — released 2020-11, high-end desktop

_2020 Zen 3 16-core enthusiast desktop; AMD's flagship consumer CPU at launch, still strong in 2026._

**Quant kernel class:** `avx2-fallback` (no AVX-VNNI). Zen 3 (2020): no AVX-VNNI (added in Zen 4, 2022). ggml int8 path uses the AVX2 fallback. Quant speedup vs fp16 is bandwidth-only here; on 16 fast cores + dual-channel DDR4 the headroom is large enough that quant still wins, but not by the VNNI multiplier.

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 3/3 | 16.15 | 0.1 | 5.97 | 0.2 | 0.50 | 580 | comfortable |  |
| base-q5_1 | ac | cpu | 2/2 | 46.09 | 0.2 | 6.68 | 1.8 | 0.41 | 428 | comfortable |  |
| base-q8_0 | ac | cpu | 2/2 | 46.17 | 0.9 | 8.50 | 0.1 | 0.38 | 469 | comfortable |  |
| base.en | ac | cpu | 3/3 | 15.57 | 0.4 | 5.96 | 0.2 | 0.50 | 587 | comfortable |  |
| base.en-q5_1 | ac | cpu | 2/2 | 36.07 | 0.3 | 8.55 | 0.7 | 0.42 | 436 | comfortable |  |
| base.en-q8_0 | ac | cpu | 2/2 | 38.72 | 0.5 | 8.37 | 2.4 | 0.38 | 472 | comfortable |  |
| large-v3-turbo | ac | cpu | 3/3 | 1.75 | 1.0 | 0.60 | 0.4 | 5.82 | 3642 | borderline |  |
| large-v3-turbo-q5_0 | ac | cpu | 2/2 | 5.20 | 0.7 | 0.61 | 0.2 | 6.02 | 1655 | borderline |  |
| large-v3-turbo-q8_0 | ac | cpu | 2/2 | 5.71 | 2.2 | 0.74 | 0.1 | 4.93 | 2227 | borderline |  |
| small | ac | cpu | 3/3 | 5.62 | 0.1 | 1.97 | 0.1 | 1.72 | 1352 | comfortable |  |
| small-q5_1 | ac | cpu | 2/2 | 18.35 | 0.5 | 2.59 | 0.3 | 1.29 | 793 | comfortable |  |
| small-q8_0 | ac | cpu | 2/2 | 19.18 | 0.4 | 2.94 | 0.1 | 1.09 | 934 | comfortable |  |
| small.en | ac | cpu | 3/3 | 6.42 | 0.3 | 2.32 | 0.1 | 1.51 | 1358 | comfortable |  |
| small.en-q5_1 | ac | cpu | 2/2 | 13.75 | 1.9 | 2.80 | 1.0 | 1.33 | 797 | comfortable |  |
| small.en-q8_0 | ac | cpu | 2/2 | 19.06 | 1.1 | 3.22 | 0.2 | 1.13 | 936 | comfortable |  |
| tiny | ac | cpu | 3/3 | 28.68 | 1.2 | 9.61 | 0.4 | 0.34 | 416 | comfortable |  |
| tiny-q5_1 | ac | cpu | 2/2 | 84.74 | 0.0 | 12.90 | 1.0 | 0.30 | 304 | comfortable |  |
| tiny-q8_0 | ac | cpu | 2/2 | 77.43 | 3.9 | 13.93 | 0.8 | 0.23 | 336 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 33.48 | 1.0 | 12.40 | 4.1 | 0.27 | 409 | comfortable |  |
| tiny.en-q5_1 | ac | cpu | 2/2 | 87.65 | 11.8 | 15.83 | 2.9 | 0.23 | 308 | comfortable |  |
| tiny.en-q8_0 | ac | cpu | 2/2 | 98.12 | 0.9 | 16.75 | 0.9 | 0.20 | 327 | comfortable |  |
| base | ac | vulkan | 2/2 | 107.46 | 2.7 | 58.32 | 0.8 | 0.04 | 336 | comfortable |  |
| base-q5_1 | ac | vulkan | 2/2 | 102.68 | 2.2 | 62.14 | 1.9 | 0.03 | 260 | comfortable |  |
| base-q8_0 | ac | vulkan | 2/2 | 109.02 | 1.9 | 59.57 | 1.6 | 0.03 | 268 | comfortable |  |
| base.en | ac | vulkan | 2/2 | 92.35 | 1.4 | 64.39 | 1.6 | 0.04 | 308 | comfortable |  |
| base.en-q5_1 | ac | vulkan | 2/2 | 80.85 | 2.5 | 66.62 | 0.5 | 0.04 | 266 | comfortable |  |
| base.en-q8_0 | ac | vulkan | 2/2 | 98.58 | 3.0 | 67.93 | 1.8 | 0.04 | 275 | comfortable |  |
| large-v3-turbo | ac | vulkan | 2/2 | 76.00 | 0.4 | 29.73 | 0.3 | 0.11 | 465 | comfortable |  |
| large-v3-turbo-q5_0 | ac | vulkan | 2/2 | 58.14 | 80.1 | 22.64 | 55.4 | 0.33 | 610 | comfortable | batch_rtf spread 80.1% > 15%; stream_rtf spread 55.4% > 15% |
| large-v3-turbo-q8_0 | ac | vulkan | 2/2 | 86.93 | 0.4 | 31.27 | 0.3 | 0.10 | 346 | comfortable |  |
| small | ac | vulkan | 2/2 | 74.00 | 0.4 | 30.02 | 0.6 | 0.11 | 346 | comfortable |  |
| small-q5_1 | ac | vulkan | 2/2 | 81.63 | 0.7 | 35.64 | 0.7 | 0.06 | 302 | comfortable |  |
| small-q8_0 | ac | vulkan | 2/2 | 78.97 | 1.1 | 31.42 | 1.0 | 0.10 | 300 | comfortable |  |
| small.en | ac | vulkan | 2/2 | 61.85 | 1.5 | 40.05 | 0.7 | 0.09 | 346 | comfortable |  |
| small.en-q5_1 | ac | vulkan | 2/2 | 65.65 | 1.6 | 40.58 | 2.4 | 0.07 | 305 | comfortable |  |
| small.en-q8_0 | ac | vulkan | 2/2 | 67.05 | 0.2 | 43.66 | 0.2 | 0.07 | 300 | comfortable |  |
| tiny | ac | vulkan | 2/2 | 109.49 | 32.2 | 59.49 | 12.2 | 0.05 | 283 | comfortable | batch_rtf spread 32.2% > 15% |
| tiny-q5_1 | ac | vulkan | 2/2 | 81.15 | 94.9 | 31.56 | 63.6 | 0.29 | 546 | comfortable | batch_rtf spread 94.9% > 15%; stream_rtf spread 63.6% > 15% |
| tiny-q8_0 | ac | vulkan | 2/2 | 82.52 | 85.2 | 42.99 | 68.9 | 0.18 | 463 | comfortable | batch_rtf spread 85.2% > 15%; stream_rtf spread 68.9% > 15% |
| tiny.en | ac | vulkan | 2/2 | 106.22 | 0.8 | 89.20 | 1.9 | 0.03 | 255 | comfortable |  |
| tiny.en-q5_1 | ac | vulkan | 2/2 | 85.78 | 44.0 | 89.61 | 0.7 | 0.03 | 291 | comfortable | batch_rtf spread 44.0% > 15% |
| tiny.en-q8_0 | ac | vulkan | 2/2 | 107.31 | 0.7 | 88.35 | 0.8 | 0.03 | 263 | comfortable |  |

## ultra7-258v — Intel(R) Core(TM) Ultra 7 258V (8p/8l, 31572 MiB, laptop) — released 2024-09, premium ultraportable (current)

_2024 Lunar Lake 4P+4LP-E (no SMT); Intel's current efficiency flagship for thin-and-light laptops, with on-package LPDDR5X and Xe2 Battlemage iGPU._

**Quant kernel class:** `vnni` (AVX-VNNI). Lunar Lake Lion Cove P-cores have AVX-VNNI (no AVX-512; Intel removed it from consumer Core Ultra). Int8 dot products use vpdpbusd; quant gives the full 1.5-3x throughput uplift over fp16, plus small.en-q8_0 promotes from borderline to comfortable for live streaming.

| model | power | build | iters | batch RTF | b-σ% | stream RTF | s-σ% | TTFF s | RSS MiB | verdict | notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| base | ac | cpu | 3/3 | 11.39 | 2.4 | 4.23 | 3.2 | 0.68 | 585 | comfortable |  |
| base.en | ac | cpu | 3/3 | 13.20 | 0.8 | 5.14 | 4.1 | 0.57 | 591 | comfortable |  |
| large-v3-turbo | ac | cpu | 3/3 | 0.61 | 0.6 | 0.20 | 0.1 | 18.49 | 3654 | unsuitable |  |
| small | ac | cpu | 3/3 | 3.13 | 1.5 | 0.72 | 30.6 | 3.39 | 1360 | borderline | stream_rtf spread 30.6% > 15% |
| small.en | ac | cpu | 3/3 | 3.90 | 3.0 | 1.23 | 1.7 | 3.01 | 1367 | borderline |  |
| tiny | ac | cpu | 3/3 | 20.49 | 1.7 | 7.83 | 8.9 | 0.41 | 420 | comfortable |  |
| tiny.en | ac | cpu | 3/3 | 26.81 | 17.3 | 10.78 | 1.1 | 0.28 | 414 | comfortable | batch_rtf spread 17.3% > 15% |
| base | ac | vulkan | 3/3 | 49.30 | 7.5 | 18.36 | 1.2 | 0.13 | 172 | comfortable |  |
| base-q5_1 | ac | vulkan | 2/2 | 46.81 | 1.6 | 19.81 | 0.7 | 0.12 | 182 | comfortable |  |
| base-q8_0 | ac | vulkan | 2/2 | 50.76 | 0.0 | 19.81 | 1.6 | 0.12 | 191 | comfortable |  |
| base.en | ac | vulkan | 3/3 | 40.66 | 0.8 | 19.90 | 0.6 | 0.12 | 172 | comfortable |  |
| base.en-q5_1 | ac | vulkan | 2/2 | 42.44 | 1.7 | 20.16 | 0.2 | 0.14 | 188 | comfortable |  |
| base.en-q8_0 | ac | vulkan | 2/2 | 50.08 | 3.2 | 20.09 | 2.3 | 0.13 | 197 | comfortable |  |
| large-v3-turbo | ac | vulkan | 3/3 | 12.74 | 19.4 | 3.27 | 4.7 | 1.02 | 264 | comfortable | batch_rtf spread 19.4% > 15% |
| large-v3-turbo-q5_0 | ac | vulkan | 2/2 | 20.59 | 0.1 | 3.61 | 1.1 | 0.94 | 228 | comfortable |  |
| large-v3-turbo-q8_0 | ac | vulkan | 2/2 | 15.17 | 0.0 | 3.51 | 2.0 | 0.96 | 222 | comfortable |  |
| small | ac | vulkan | 3/3 | 26.23 | 15.8 | 8.04 | 0.2 | 0.29 | 193 | comfortable | batch_rtf spread 15.8% > 15% |
| small-q5_1 | ac | vulkan | 2/2 | 30.71 | 1.3 | 8.97 | 0.1 | 0.27 | 204 | comfortable |  |
| small-q8_0 | ac | vulkan | 2/2 | 30.93 | 0.2 | 8.85 | 0.1 | 0.28 | 182 | comfortable |  |
| small.en | ac | vulkan | 3/3 | 21.95 | 0.8 | 9.33 | 0.2 | 0.34 | 191 | comfortable |  |
| small.en-q5_1 | ac | vulkan | 2/2 | 26.53 | 0.8 | 9.60 | 0.1 | 0.32 | 206 | comfortable |  |
| small.en-q8_0 | ac | vulkan | 2/2 | 12.46 | 0.7 | 9.69 | 0.0 | 0.34 | 185 | comfortable |  |
| tiny | ac | vulkan | 3/3 | 69.27 | 9.5 | 23.63 | 1.1 | 0.13 | 174 | comfortable |  |
| tiny-q5_1 | ac | vulkan | 2/2 | 73.14 | 0.1 | 19.31 | 3.2 | 0.22 | 174 | comfortable |  |
| tiny-q8_0 | ac | vulkan | 2/2 | 71.06 | 3.3 | 26.25 | 4.0 | 0.09 | 184 | comfortable |  |
| tiny.en | ac | vulkan | 3/3 | 64.92 | 1.4 | 33.50 | 1.4 | 0.07 | 164 | comfortable |  |
| tiny.en-q5_1 | ac | vulkan | 2/2 | 79.48 | 2.5 | 32.53 | 0.6 | 0.10 | 177 | comfortable |  |
| tiny.en-q8_0 | ac | vulkan | 2/2 | 81.34 | 2.7 | 33.80 | 0.6 | 0.08 | 182 | comfortable |  |
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

