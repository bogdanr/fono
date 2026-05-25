# ro-bogdan Romanian sweep — best per host

**Selection criteria** (applied per host/build):

1. Hard gate: `batch_RTF_mean >= 1.5` (can keep up with audio in real time)
2. Hard gate: `batch_WER_mean <= 0.30` (acceptable accuracy)
3. Sort: WER asc, RTF desc; tiebreak alphabetical.

Means are over the two fixtures (`ro-bogdan-10s`, `ro-bogdan-30s`). RTF uses median across iterations. WER is word-level Levenshtein.

## i7-1255u

### cpu

**Recommended (passing gates):**

| rank | model | mean WER | mean RTF | peak RSS MiB |
|---:|---|---:|---:|---:|
| 1 | `small-q5_1` | 0.177 | 6.30 | 714 |
| 2 | `small-q8_0` | 0.273 | 7.78 | 856 |
| 3 | `small` | 0.281 | 4.42 | 1282 |

**Best quality (no speed gate):**

`small-q5_1` — WER 0.177, RTF 6.30, RSS 714 MiB


### vulkan

**Recommended (passing gates):**

| rank | model | mean WER | mean RTF | peak RSS MiB |
|---:|---|---:|---:|---:|
| 1 | `large-v3-turbo-q5_0` | 0.211 | 4.94 | 204 |
| 2 | `large-v3-turbo-q8_0` | 0.253 | 4.82 | 221 |
| 3 | `large-v3-turbo` | 0.253 | 2.60 | 270 |
| 4 | `small-q5_1` | 0.281 | 15.60 | 207 |
| 5 | `small-q8_0` | 0.281 | 6.81 | 163 |

**Best quality (no speed gate):**

`large-v3-turbo-q5_0` — WER 0.211, RTF 4.94, RSS 204 MiB


## i7-7500u

### cpu

**Recommended (passing gates):**

| rank | model | mean WER | mean RTF | peak RSS MiB |
|---:|---|---:|---:|---:|
| 1 | `small-q5_1` | 0.177 | 2.28 | 713 |
| 2 | `small-q8_0` | 0.273 | 2.98 | 855 |
| 3 | `small` | 0.281 | 2.41 | 1280 |

**Best quality (no speed gate):**

`small-q5_1` — WER 0.177, RTF 2.28, RSS 713 MiB


### vulkan

**Recommended (passing gates):**

| rank | model | mean WER | mean RTF | peak RSS MiB |
|---:|---|---:|---:|---:|
| 1 | `small-q5_1` | 0.273 | 3.79 | 211 |
| 2 | `small-q8_0` | 0.281 | 3.51 | 194 |
| 3 | `small` | 0.281 | 3.11 | 207 |

**Best quality (no speed gate):**

`large-v3-turbo-q5_0` — WER 0.211, RTF 0.77, RSS 220 MiB


## i7-8550u

### cpu

**Recommended (passing gates):**

| rank | model | mean WER | mean RTF | peak RSS MiB |
|---:|---|---:|---:|---:|
| 1 | `small-q5_1` | 0.177 | 3.88 | 738 |
| 2 | `small-q8_0` | 0.273 | 4.47 | 879 |
| 3 | `small` | 0.281 | 3.46 | 1305 |

**Best quality (no speed gate):**

`small-q5_1` — WER 0.177, RTF 3.88, RSS 738 MiB


### vulkan

**Recommended (passing gates):**

| rank | model | mean WER | mean RTF | peak RSS MiB |
|---:|---|---:|---:|---:|
| 1 | `small-q5_1` | 0.281 | 3.97 | 205 |
| 2 | `small-q8_0` | 0.281 | 3.71 | 187 |
| 3 | `small` | 0.281 | 3.20 | 202 |

**Best quality (no speed gate):**

`large-v3-turbo-q5_0` — WER 0.148, RTF 0.78, RSS 236 MiB


## ryzen-5950x

### cpu

**Recommended (passing gates):**

| rank | model | mean WER | mean RTF | peak RSS MiB |
|---:|---|---:|---:|---:|
| 1 | `small-q5_1` | 0.177 | 17.14 | 712 |
| 2 | `large-v3-turbo-q5_0` | 0.203 | 3.98 | 1629 |
| 3 | `large-v3-turbo-q8_0` | 0.253 | 4.86 | 2203 |
| 4 | `large-v3-turbo` | 0.253 | 4.12 | 3633 |
| 5 | `small-q8_0` | 0.273 | 18.05 | 854 |

**Best quality (no speed gate):**

`small-q5_1` — WER 0.177, RTF 17.14, RSS 712 MiB


### vulkan

**Recommended (passing gates):**

| rank | model | mean WER | mean RTF | peak RSS MiB |
|---:|---|---:|---:|---:|
| 1 | `small-q5_1` | 0.198 | 73.86 | 300 |
| 2 | `large-v3-turbo-q5_0` | 0.211 | 82.09 | 314 |
| 3 | `large-v3-turbo-q8_0` | 0.253 | 81.48 | 347 |
| 4 | `large-v3-turbo` | 0.253 | 72.92 | 468 |
| 5 | `small-q8_0` | 0.281 | 76.99 | 289 |

**Best quality (no speed gate):**

`small-q5_1` — WER 0.198, RTF 73.86, RSS 300 MiB


## ultra7-258v

### cpu

**Recommended (passing gates):**

| rank | model | mean WER | mean RTF | peak RSS MiB |
|---:|---|---:|---:|---:|
| 1 | `small-q5_1` | 0.177 | 8.94 | 720 |
| 2 | `small-q8_0` | 0.273 | 10.75 | 859 |
| 3 | `small` | 0.281 | 7.46 | 1285 |

**Best quality (no speed gate):**

`small-q5_1` — WER 0.177, RTF 8.94, RSS 720 MiB


### vulkan

**Recommended (passing gates):**

| rank | model | mean WER | mean RTF | peak RSS MiB |
|---:|---|---:|---:|---:|
| 1 | `large-v3-turbo-q5_0` | 0.211 | 16.75 | 211 |
| 2 | `large-v3-turbo-q8_0` | 0.253 | 16.20 | 215 |
| 3 | `large-v3-turbo` | 0.253 | 14.71 | 271 |
| 4 | `small-q8_0` | 0.281 | 30.90 | 176 |
| 5 | `small-q5_1` | 0.281 | 30.15 | 198 |

**Best quality (no speed gate):**

`large-v3-turbo-q5_0` — WER 0.211, RTF 16.75, RSS 211 MiB


