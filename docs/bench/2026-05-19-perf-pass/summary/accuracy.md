# Quantization accuracy comparison

Lower mean accuracy (normalised Levenshtein distance to reference text) = better.
Δ = (quant − fp16) in absolute points; positive Δ means quantization degraded quality.

**English-only** columns (`en_*`) are the gate for multilingual models. Non-English fixtures often sit at the model's quality floor where quantization noise is masked; English-only Δ is the signal that drives `default_quantization` in the registry.

## i7-7500u / cpu / base

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 30 | 0.1472 | 0.4737 | +0.0000 | 12 | 0.0485 | 0.1136 | +0.0000 | +0.0000 | 27 | 3 |
| q8_0 | 20 | 0.1685 | 0.5000 | +0.0213 | 8 | 0.1134 | 0.2610 | +0.0650 | +0.1474 | 18 | 2 |
| q5_1 | 20 | 0.2637 | 0.7463 | +0.1165 | 8 | 0.3331 | 0.7463 | +0.2847 | +0.6326 | 16 | 4 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = +0.0650 (> +0.05), Δ_en_max = +0.1474 (≤ +0.20) — FAIL
- `q5_1`: Δ_en_mean = +0.2847 (> +0.05), Δ_en_max = +0.6326 (> +0.20) — FAIL

## i7-7500u / cpu / base.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 12 | 0.1233 | 0.2073 | +0.0000 | 12 | 0.1233 | 0.2073 | +0.0000 | +0.0000 | 9 | 3 |
| q8_0 | 8 | 0.3074 | 0.9878 | +0.1841 | 8 | 0.3074 | 0.9878 | +0.1841 | +0.7805 | 6 | 2 |
| q5_1 | 8 | 0.3022 | 0.9878 | +0.1788 | 8 | 0.3022 | 0.9878 | +0.1788 | +0.7805 | 6 | 2 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = +0.1841 (> +0.05), Δ_en_max = +0.7805 (> +0.20) — FAIL
- `q5_1`: Δ_en_mean = +0.1788 (> +0.05), Δ_en_max = +0.7805 (> +0.20) — FAIL

## i7-7500u / cpu / large-v3-turbo

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 10 | 0.1174 | 0.5789 | +0.0000 | 4 | 0.0265 | 0.0464 | +0.0000 | +0.0000 | 10 | 0 |
| q5_0 | 10 | 0.1509 | 0.6053 | +0.0335 | 4 | 0.1060 | 0.2059 | +0.0794 | +0.1595 | 10 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q5_0`: Δ_en_mean = +0.0794 (> +0.05), Δ_en_max = +0.1595 (≤ +0.20) — FAIL

## i7-7500u / cpu / small

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 30 | 0.2015 | 0.8529 | +0.0000 | 12 | 0.2852 | 0.8529 | +0.0000 | +0.0000 | 30 | 0 |
| q8_0 | 20 | 0.1144 | 0.3684 | -0.0871 | 8 | 0.0610 | 0.0992 | -0.2242 | -0.7537 | 20 | 0 |
| q5_1 | 20 | 0.1135 | 0.3684 | -0.0880 | 8 | 0.0610 | 0.0992 | -0.2242 | -0.7537 | 20 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = -0.2242 (≤ +0.05), Δ_en_max = -0.7537 (≤ +0.20) — PASS
- `q5_1`: Δ_en_mean = -0.2242 (≤ +0.05), Δ_en_max = -0.7537 (≤ +0.20) — PASS

## i7-7500u / cpu / small.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 12 | 0.2214 | 0.5183 | +0.0000 | 12 | 0.2214 | 0.5183 | +0.0000 | +0.0000 | 9 | 3 |
| q8_0 | 8 | 0.1874 | 0.2500 | -0.0340 | 8 | 0.1874 | 0.2500 | -0.0340 | -0.2683 | 6 | 2 |
| q5_1 | 8 | 0.2666 | 0.4884 | +0.0452 | 8 | 0.2666 | 0.4884 | +0.0452 | -0.0299 | 4 | 4 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = -0.0340 (≤ +0.05), Δ_en_max = -0.2683 (≤ +0.20) — PASS
- `q5_1`: Δ_en_mean = +0.0452 (≤ +0.05), Δ_en_max = -0.0299 (≤ +0.20) — PASS

## i7-7500u / cpu / tiny

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 30 | 0.1638 | 0.5000 | +0.0000 | 12 | 0.0219 | 0.0465 | +0.0000 | +0.0000 | 27 | 3 |
| q8_0 | 20 | 0.1855 | 0.5000 | +0.0217 | 8 | 0.0474 | 0.0882 | +0.0254 | +0.0417 | 16 | 4 |
| q5_1 | 20 | 0.1806 | 0.5263 | +0.0169 | 8 | 0.0474 | 0.0882 | +0.0254 | +0.0417 | 18 | 2 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = +0.0254 (≤ +0.05), Δ_en_max = +0.0417 (≤ +0.20) — PASS
- `q5_1`: Δ_en_mean = +0.0254 (≤ +0.05), Δ_en_max = +0.0417 (≤ +0.20) — PASS

## i7-7500u / cpu / tiny.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 12 | 0.0749 | 0.1618 | +0.0000 | 12 | 0.0749 | 0.1618 | +0.0000 | +0.0000 | 12 | 0 |
| q8_0 | 8 | 0.0780 | 0.1618 | +0.0030 | 8 | 0.0780 | 0.1618 | +0.0030 | +0.0000 | 8 | 0 |
| q5_1 | 8 | 0.0565 | 0.1618 | -0.0184 | 8 | 0.0565 | 0.1618 | -0.0184 | +0.0000 | 8 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = +0.0030 (≤ +0.05), Δ_en_max = +0.0000 (≤ +0.20) — PASS
- `q5_1`: Δ_en_mean = -0.0184 (≤ +0.05), Δ_en_max = +0.0000 (≤ +0.20) — PASS

## i7-7500u / cpu-actx / small

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 20 | 0.1134 | 0.3684 | +0.0000 | 8 | 0.0610 | 0.0992 | +0.0000 | +0.0000 | 20 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## i7-7500u / cpu-actx / tiny.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 8 | 0.0780 | 0.1618 | +0.0000 | 8 | 0.0780 | 0.1618 | +0.0000 | +0.0000 | 8 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## i7-7500u / cpu-noactx / small

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 20 | 0.2015 | 0.8529 | +0.0000 | 8 | 0.2852 | 0.8529 | +0.0000 | +0.0000 | 20 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## i7-7500u / cpu-noactx / tiny.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 8 | 0.0749 | 0.1618 | +0.0000 | 8 | 0.0749 | 0.1618 | +0.0000 | +0.0000 | 8 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ryzen-5950x / cpu-actx / small

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 20 | 0.1134 | 0.3684 | +0.0000 | 8 | 0.0610 | 0.0992 | +0.0000 | +0.0000 | 20 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ryzen-5950x / cpu-actx / small.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 8 | 0.1881 | 0.2500 | +0.0000 | 8 | 0.1881 | 0.2500 | +0.0000 | +0.0000 | 6 | 2 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ryzen-5950x / cpu-actx / tiny.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 8 | 0.0780 | 0.1618 | +0.0000 | 8 | 0.0780 | 0.1618 | +0.0000 | +0.0000 | 8 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ryzen-5950x / cpu-noactx / small

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 20 | 0.2015 | 0.8529 | +0.0000 | 8 | 0.2852 | 0.8529 | +0.0000 | +0.0000 | 20 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ryzen-5950x / cpu-noactx / small.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 8 | 0.2214 | 0.5183 | +0.0000 | 8 | 0.2214 | 0.5183 | +0.0000 | +0.0000 | 6 | 2 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ryzen-5950x / cpu-noactx / tiny.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 8 | 0.0749 | 0.1618 | +0.0000 | 8 | 0.0749 | 0.1618 | +0.0000 | +0.0000 | 8 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ryzen-5950x / cpu-t16 / large-v3-turbo

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 20 | 0.1259 | 0.5789 | +0.0000 | 8 | 0.0473 | 0.0685 | +0.0000 | +0.0000 | 20 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ryzen-5950x / cpu-t16 / small

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 20 | 0.1134 | 0.3684 | +0.0000 | 8 | 0.0610 | 0.0992 | +0.0000 | +0.0000 | 20 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ryzen-5950x / cpu-t32 / large-v3-turbo

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 20 | 0.1259 | 0.5789 | +0.0000 | 8 | 0.0473 | 0.0685 | +0.0000 | +0.0000 | 20 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ryzen-5950x / cpu-t32 / small

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 20 | 0.1134 | 0.3684 | +0.0000 | 8 | 0.0610 | 0.0992 | +0.0000 | +0.0000 | 20 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ryzen-5950x / cpu-t8 / large-v3-turbo

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 20 | 0.1259 | 0.5789 | +0.0000 | 8 | 0.0473 | 0.0685 | +0.0000 | +0.0000 | 20 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ryzen-5950x / cpu-t8 / small

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 20 | 0.1134 | 0.3684 | +0.0000 | 8 | 0.0610 | 0.0992 | +0.0000 | +0.0000 | 20 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ultra7-258v / cpu / base

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 30 | 0.1472 | 0.4737 | +0.0000 | 12 | 0.0485 | 0.1136 | +0.0000 | +0.0000 | 27 | 3 |
| q8_0 | 20 | 0.1685 | 0.5000 | +0.0213 | 8 | 0.1134 | 0.2610 | +0.0650 | +0.1474 | 18 | 2 |
| q5_1 | 30 | 0.2637 | 0.7463 | +0.1165 | 12 | 0.3331 | 0.7463 | +0.2847 | +0.6326 | 24 | 6 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = +0.0650 (> +0.05), Δ_en_max = +0.1474 (≤ +0.20) — FAIL
- `q5_1`: Δ_en_mean = +0.2847 (> +0.05), Δ_en_max = +0.6326 (> +0.20) — FAIL

## ultra7-258v / cpu / base.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 12 | 0.1233 | 0.2073 | +0.0000 | 12 | 0.1233 | 0.2073 | +0.0000 | +0.0000 | 9 | 3 |
| q8_0 | 8 | 0.3074 | 0.9878 | +0.1841 | 8 | 0.3074 | 0.9878 | +0.1841 | +0.7805 | 6 | 2 |
| q5_1 | 8 | 0.3022 | 0.9878 | +0.1788 | 8 | 0.3022 | 0.9878 | +0.1788 | +0.7805 | 6 | 2 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = +0.1841 (> +0.05), Δ_en_max = +0.7805 (> +0.20) — FAIL
- `q5_1`: Δ_en_mean = +0.1788 (> +0.05), Δ_en_max = +0.7805 (> +0.20) — FAIL

## ultra7-258v / cpu / large-v3-turbo

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 30 | 0.1174 | 0.5789 | +0.0000 | 12 | 0.0265 | 0.0464 | +0.0000 | +0.0000 | 30 | 0 |
| q8_0 | 20 | 0.1351 | 0.5789 | +0.0177 | 8 | 0.0705 | 0.1503 | +0.0440 | +0.1038 | 20 | 0 |
| q5_0 | 30 | 0.1509 | 0.6053 | +0.0335 | 12 | 0.1060 | 0.2059 | +0.0794 | +0.1595 | 30 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = +0.0440 (≤ +0.05), Δ_en_max = +0.1038 (≤ +0.20) — PASS
- `q5_0`: Δ_en_mean = +0.0794 (> +0.05), Δ_en_max = +0.1595 (≤ +0.20) — FAIL

## ultra7-258v / cpu / small

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 30 | 0.2015 | 0.8529 | +0.0000 | 12 | 0.2852 | 0.8529 | +0.0000 | +0.0000 | 30 | 0 |
| q8_0 | 20 | 0.1144 | 0.3684 | -0.0871 | 8 | 0.0610 | 0.0992 | -0.2242 | -0.7537 | 20 | 0 |
| q5_1 | 30 | 0.1135 | 0.3684 | -0.0880 | 12 | 0.0610 | 0.0992 | -0.2242 | -0.7537 | 30 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = -0.2242 (≤ +0.05), Δ_en_max = -0.7537 (≤ +0.20) — PASS
- `q5_1`: Δ_en_mean = -0.2242 (≤ +0.05), Δ_en_max = -0.7537 (≤ +0.20) — PASS

## ultra7-258v / cpu / small.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 12 | 0.2214 | 0.5183 | +0.0000 | 12 | 0.2214 | 0.5183 | +0.0000 | +0.0000 | 9 | 3 |
| q8_0 | 8 | 0.1874 | 0.2500 | -0.0340 | 8 | 0.1874 | 0.2500 | -0.0340 | -0.2683 | 6 | 2 |
| q5_1 | 12 | 0.2666 | 0.4884 | +0.0452 | 12 | 0.2666 | 0.4884 | +0.0452 | -0.0299 | 6 | 6 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = -0.0340 (≤ +0.05), Δ_en_max = -0.2683 (≤ +0.20) — PASS
- `q5_1`: Δ_en_mean = +0.0452 (≤ +0.05), Δ_en_max = -0.0299 (≤ +0.20) — PASS

## ultra7-258v / cpu / tiny

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 30 | 0.1638 | 0.5000 | +0.0000 | 12 | 0.0219 | 0.0465 | +0.0000 | +0.0000 | 27 | 3 |
| q8_0 | 20 | 0.1855 | 0.5000 | +0.0217 | 8 | 0.0474 | 0.0882 | +0.0254 | +0.0417 | 16 | 4 |
| q5_1 | 30 | 0.1806 | 0.5263 | +0.0169 | 12 | 0.0474 | 0.0882 | +0.0254 | +0.0417 | 27 | 3 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = +0.0254 (≤ +0.05), Δ_en_max = +0.0417 (≤ +0.20) — PASS
- `q5_1`: Δ_en_mean = +0.0254 (≤ +0.05), Δ_en_max = +0.0417 (≤ +0.20) — PASS

## ultra7-258v / cpu / tiny.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 12 | 0.0749 | 0.1618 | +0.0000 | 12 | 0.0749 | 0.1618 | +0.0000 | +0.0000 | 12 | 0 |
| q8_0 | 8 | 0.0780 | 0.1618 | +0.0030 | 8 | 0.0780 | 0.1618 | +0.0030 | +0.0000 | 8 | 0 |
| q5_1 | 8 | 0.0565 | 0.1618 | -0.0184 | 8 | 0.0565 | 0.1618 | -0.0184 | +0.0000 | 8 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = +0.0030 (≤ +0.05), Δ_en_max = +0.0000 (≤ +0.20) — PASS
- `q5_1`: Δ_en_mean = -0.0184 (≤ +0.05), Δ_en_max = +0.0000 (≤ +0.20) — PASS

## ultra7-258v / cpu-actx / small

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 20 | 0.1134 | 0.3684 | +0.0000 | 8 | 0.0610 | 0.0992 | +0.0000 | +0.0000 | 20 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ultra7-258v / cpu-actx / small.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 8 | 0.1881 | 0.2500 | +0.0000 | 8 | 0.1881 | 0.2500 | +0.0000 | +0.0000 | 6 | 2 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ultra7-258v / cpu-actx / tiny.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 8 | 0.0780 | 0.1618 | +0.0000 | 8 | 0.0780 | 0.1618 | +0.0000 | +0.0000 | 8 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ultra7-258v / cpu-noactx / small

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 20 | 0.2015 | 0.8529 | +0.0000 | 8 | 0.2852 | 0.8529 | +0.0000 | +0.0000 | 20 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ultra7-258v / cpu-noactx / small.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 8 | 0.2214 | 0.5183 | +0.0000 | 8 | 0.2214 | 0.5183 | +0.0000 | +0.0000 | 6 | 2 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ultra7-258v / cpu-noactx / tiny.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 8 | 0.0749 | 0.1618 | +0.0000 | 8 | 0.0749 | 0.1618 | +0.0000 | +0.0000 | 8 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ultra7-258v / vulkan / base

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 30 | 0.1465 | 0.4737 | +0.0000 | 12 | 0.0485 | 0.1136 | +0.0000 | +0.0000 | 27 | 3 |
| q8_0 | 20 | 0.1903 | 0.5135 | +0.0438 | 8 | 0.1765 | 0.5135 | +0.1281 | +0.3998 | 18 | 2 |
| q5_1 | 30 | 0.2619 | 0.7463 | +0.1154 | 12 | 0.3574 | 0.7463 | +0.3090 | +0.6326 | 24 | 6 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = +0.1281 (> +0.05), Δ_en_max = +0.3998 (> +0.20) — FAIL
- `q5_1`: Δ_en_mean = +0.3090 (> +0.05), Δ_en_max = +0.6326 (> +0.20) — FAIL

## ultra7-258v / vulkan / base.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 12 | 0.1218 | 0.2012 | +0.0000 | 12 | 0.1218 | 0.2012 | +0.0000 | +0.0000 | 9 | 3 |
| q8_0 | 8 | 0.3275 | 0.9878 | +0.2057 | 8 | 0.3275 | 0.9878 | +0.2057 | +0.7866 | 6 | 2 |
| q5_1 | 8 | 0.3022 | 0.9878 | +0.1804 | 8 | 0.3022 | 0.9878 | +0.1804 | +0.7866 | 6 | 2 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = +0.2057 (> +0.05), Δ_en_max = +0.7866 (> +0.20) — FAIL
- `q5_1`: Δ_en_mean = +0.1804 (> +0.05), Δ_en_max = +0.7866 (> +0.20) — FAIL

## ultra7-258v / vulkan / large-v3-turbo

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 30 | 0.1174 | 0.5789 | +0.0000 | 12 | 0.0265 | 0.0464 | +0.0000 | +0.0000 | 30 | 0 |
| q8_0 | 20 | 0.1255 | 0.5789 | +0.0081 | 8 | 0.0473 | 0.0685 | +0.0208 | +0.0221 | 18 | 2 |
| q5_0 | 30 | 0.1578 | 0.6053 | +0.0404 | 12 | 0.1215 | 0.3543 | +0.0950 | +0.3079 | 27 | 3 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = +0.0208 (≤ +0.05), Δ_en_max = +0.0221 (≤ +0.20) — PASS
- `q5_0`: Δ_en_mean = +0.0950 (> +0.05), Δ_en_max = +0.3079 (> +0.20) — FAIL

## ultra7-258v / vulkan / small

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 30 | 0.1957 | 0.8529 | +0.0000 | 12 | 0.2852 | 0.8529 | +0.0000 | +0.0000 | 30 | 0 |
| q8_0 | 20 | 0.1158 | 0.3684 | -0.0798 | 8 | 0.0647 | 0.0992 | -0.2205 | -0.7537 | 20 | 0 |
| q5_1 | 30 | 0.1149 | 0.3684 | -0.0807 | 12 | 0.0647 | 0.0992 | -0.2205 | -0.7537 | 30 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = -0.2205 (≤ +0.05), Δ_en_max = -0.7537 (≤ +0.20) — PASS
- `q5_1`: Δ_en_mean = -0.2205 (≤ +0.05), Δ_en_max = -0.7537 (≤ +0.20) — PASS

## ultra7-258v / vulkan / small.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 12 | 0.2214 | 0.5183 | +0.0000 | 12 | 0.2214 | 0.5183 | +0.0000 | +0.0000 | 9 | 3 |
| q8_0 | 8 | 0.2373 | 0.3038 | +0.0159 | 8 | 0.2373 | 0.3038 | +0.0159 | -0.2145 | 4 | 4 |
| q5_1 | 12 | 0.2451 | 0.4884 | +0.0237 | 12 | 0.2451 | 0.4884 | +0.0237 | -0.0299 | 9 | 3 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = +0.0159 (≤ +0.05), Δ_en_max = -0.2145 (≤ +0.20) — PASS
- `q5_1`: Δ_en_mean = +0.0237 (≤ +0.05), Δ_en_max = -0.0299 (≤ +0.20) — PASS

## ultra7-258v / vulkan / tiny

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 30 | 0.1643 | 0.5000 | +0.0000 | 12 | 0.0219 | 0.0465 | +0.0000 | +0.0000 | 27 | 3 |
| q8_0 | 20 | 0.1750 | 0.5000 | +0.0107 | 8 | 0.0488 | 0.0882 | +0.0269 | +0.0417 | 16 | 4 |
| q5_1 | 30 | 0.1774 | 0.5263 | +0.0131 | 12 | 0.0474 | 0.0882 | +0.0254 | +0.0417 | 27 | 3 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = +0.0269 (≤ +0.05), Δ_en_max = +0.0417 (≤ +0.20) — PASS
- `q5_1`: Δ_en_mean = +0.0254 (≤ +0.05), Δ_en_max = +0.0417 (≤ +0.20) — PASS

## ultra7-258v / vulkan / tiny.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 12 | 0.0749 | 0.1618 | +0.0000 | 12 | 0.0749 | 0.1618 | +0.0000 | +0.0000 | 12 | 0 |
| q8_0 | 8 | 0.0780 | 0.1618 | +0.0030 | 8 | 0.0780 | 0.1618 | +0.0030 | +0.0000 | 8 | 0 |
| q5_1 | 8 | 0.0565 | 0.1618 | -0.0184 | 8 | 0.0565 | 0.1618 | -0.0184 | +0.0000 | 8 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
- `q8_0`: Δ_en_mean = +0.0030 (≤ +0.05), Δ_en_max = +0.0000 (≤ +0.20) — PASS
- `q5_1`: Δ_en_mean = -0.0184 (≤ +0.05), Δ_en_max = +0.0000 (≤ +0.20) — PASS

## ultra7-258v / vulkan-actx / large-v3-turbo

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 20 | 0.1352 | 0.5789 | +0.0000 | 8 | 0.0705 | 0.1503 | +0.0000 | +0.0000 | 20 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ultra7-258v / vulkan-actx / small

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 20 | 0.1134 | 0.3684 | +0.0000 | 8 | 0.0610 | 0.0992 | +0.0000 | +0.0000 | 20 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ultra7-258v / vulkan-actx / tiny.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 8 | 0.0780 | 0.1618 | +0.0000 | 8 | 0.0780 | 0.1618 | +0.0000 | +0.0000 | 8 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ultra7-258v / vulkan-noactx / large-v3-turbo

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 20 | 0.1174 | 0.5789 | +0.0000 | 8 | 0.0265 | 0.0464 | +0.0000 | +0.0000 | 20 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ultra7-258v / vulkan-noactx / small

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 20 | 0.1957 | 0.8529 | +0.0000 | 8 | 0.2852 | 0.8529 | +0.0000 | +0.0000 | 20 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.

## ultra7-258v / vulkan-noactx / tiny.en

| quant | n | all_mean | all_max | Δ_all_mean | en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fp16 | 8 | 0.0749 | 0.1618 | +0.0000 | 8 | 0.0749 | 0.1618 | +0.0000 | +0.0000 | 8 | 0 |

Acceptance rule (registry default candidate): English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.
