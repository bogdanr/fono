# en-self-* fixtures — `.en` vs multilingual whisper builds

Focused side report from the 2026-05-23 sweep against the two new first-person dictation fixtures `en-self-dictation` and `en-self-casual`. Each cell averages `stt_accuracy_levenshtein` across **both** new fixtures and all available iterations (target: 2 fixtures × 3 iters × 3 quant variants = 18 samples per `.en_acc`/`multi_acc` cell at full coverage).

* `accuracy_levenshtein` is the normalized edit distance between the batch decode and the manifest's `reference`. **Lower = better.**
* `delta = .en_acc - multi_acc`. **Negative ⇒ `.en` family is more accurate** on these fixtures (the headline finding of interest).

## Per-tier comparison

| host | build | tier | .en_acc_mean | multi_acc_mean | delta | n_en | n_multi | winner |
|---|---|---|---:|---:|---:|---:|---:|---|
| i7-1255u | vulkan | base | 0.0081 | 0.0174 | -0.0093 | 18 | 18 | **.en** ⭐ |
| i7-1255u | vulkan | small | 0.0190 | 0.2316 | -0.2126 | 18 | 18 | **.en** ⭐ |
| i7-1255u | vulkan | tiny | 0.0267 | 0.0090 | +0.0177 | 18 | 18 | **multi** |
| i7-7500u | cpu | base | 0.0097 | 0.0174 | -0.0076 | 10 | 18 | **.en** ⭐ |
| i7-7500u | cpu | tiny | 0.0267 | 0.0090 | +0.0177 | 18 | 18 | **multi** |
| ultra7-258v | cpu | base | 0.0081 | 0.0174 | -0.0093 | 18 | 18 | **.en** ⭐ |
| ultra7-258v | cpu | small | 0.0206 | 0.1551 | -0.1345 | 18 | 18 | **.en** ⭐ |
| ultra7-258v | cpu | tiny | 0.0267 | 0.0090 | +0.0177 | 18 | 18 | **multi** |

**Summary:** `.en` wins 5 rows, multilingual wins 3 rows (of 8 per-tier cells with data on both sides).

## Turbo baseline (multilingual only)

| host | build | turbo_acc_mean | n |
|---|---|---:|---:|
| i7-1255u | vulkan | 0.0121 | 18 |
| i7-7500u | cpu | 0.0100 | 18 |
| ultra7-258v | cpu | 0.0100 | 18 |

Note: cells where only one side (.en or multi) produced results — usually because download budget or model availability differed between hosts — are omitted from the per-tier table rather than reported with an empty column.
