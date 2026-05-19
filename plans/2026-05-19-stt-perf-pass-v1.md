# Local STT Performance Pass — v1

## Objective

Cut the local STT footprint by ~50% and make the **best model the user's
hardware can run** the default — across English-only and multilingual
modes — without expanding the wizard's decision tree or making the
registry harder to reason about.

Anchored on the bench data captured 2026-05-18 / 2026-05-19 across four
reference hosts (`i7-7500u`, `i7-1255u`, `ultra7-258v`, `ryzen-5950x`)
covering CPU + Vulkan, AC. Raw runs at
`docs/bench/2026-05-19-perf-pass/runs/` (514 files);
matrices at `docs/bench/2026-05-19-perf-pass/summary/matrix.md` and
`accuracy.md`.

## Outcome

A **3-rung ladder × 2 language modes = 5 distinct model files** in the
default registry, all picked by the existing affordability predicate
(`HardwareSnapshot::affords_model`) and the existing accuracy-bucket
filter (`AccuracyBucket::Inaccurate`).

| Rung | English-only default | Multilingual default | File size |
|---|---|---|---:|
| **T1 — minimal** | `tiny.en-q5_1.bin` | `tiny-q5_1.bin` | 31 MB |
| **T2 — sweet spot** | `small.en-q8_0.bin` | `small-q5_1.bin` | 253 / 182 MB |
| **T3 — quality** | `large-v3-turbo-q8_0.bin` | `large-v3-turbo-q8_0.bin` | 834 MB |

Worst-case fresh install (all three rungs of one mode): ~1.1 GB English
or ~1.3 GB multilingual; today the same coverage averages ~3.1 GB.

The fp16 `large-v3-turbo.bin` (1.6 GB) stays in the registry as an
**opt-in** for users who pass `[stt.local].quantization = "fp16"` and
accept the disk cost; it is not advertised in the wizard.

## Decision rule (single, applied to all data)

For each `(base_model, quantization)`, on the user's target language
fixtures only, compute Δ Levenshtein-accuracy vs the fp16 baseline. The
smallest quantization that meets **both**:

- **mean Δ ≤ +0.05** (absorbs run-to-run noise ≈ ±0.02)
- **max per-fixture Δ ≤ +0.20** (catches catastrophic single-fixture
  failures — e.g. `en-narrative-pause` on `base-q8_0`)

…wins. If no quantization passes, fall back to fp16. If two pass, the
smaller wins.

This rule is **uniform across the registry** and was applied
mechanically to produce the table above. See
`docs/bench/2026-05-19-perf-pass/summary/accuracy.md` and
`docs/bench/2026-05-19-perf-pass/summary/matrix.md` for the underlying
numbers.

## What gets dropped from the registry

Removed entries (currently present in `crates/fono-stt/src/registry.rs`
or implicitly fetchable):

- `base`, `base.en` (entire family) — strictly dominated on the
  speed-vs-quality frontier by `small-q5_1` / `small.en-q8_0`. Saves
  142 MB per model.
- `tiny` fp16 — `tiny-q5_1` matches it on every measurable axis.
- `tiny-q8_0` / `tiny.en-q8_0` — q5_1 is equal quality at smaller size.
- `small` fp16 / `small-q8_0` — q5_1 dominates on every fixture we measured.
- All `*-q5_0` files (`large-v3-turbo-q5_0` in particular, which breaks
  `en-conversational`: acc 0.354 vs 0.046 fp16).

`large-v3-turbo` fp16 is kept in the registry but **not in the wizard's
shortlist** — reachable only via `[stt.local].quantization = "fp16"`.

## Caveat: `tiny` multilingual is only usable for low-WER languages

The data shows `tiny` multilingual gives acc ≤ 0.10 on English and
Spanish, ~0.17 on French, but **0.20–0.38 on Romanian** and **~0.50 on
Chinese**. The existing `wer_by_lang` table on
`crates/fono-stt/src/registry.rs:65` already encodes this (e.g.
`("ja", 34.0)` for tiny multilingual), and the wizard's
`AccuracyBucket::Inaccurate` filter already pushes such users off the T1
multilingual rung when they have hard languages selected. The plan does
not add new gating — it relies on the existing predicate to refuse T1
for users whose languages cannot be served by it, and the wizard then
recommends T2 (`small-q5_1`) or falls through to cloud STT.

Action item under Phase 1: refresh the `wer_by_lang` table entries from
the new measurements (especially the q5_1 rows) so the existing filter
keeps doing the right thing.

## Verdict-bucket flips this delivers

| host / build / model | before | after |
|---|---|---|
| `ultra7-258v / cpu / large-v3-turbo` | unsuitable (RTF 0.62, fp16) | **borderline** (RTF 2.31, `q8_0`) |
| `ryzen-5950x / cpu / large-v3-turbo` | borderline (RTF 0.6 estimate / 5.14 measured `q5_0`) | **comfortable** (RTF 5.11, `q8_0` — same speed, higher quality than `q5_0`) |
| `ultra7-258v / vulkan / large-v3-turbo` | comfortable (RTF 8.93, fp16) | **comfortable, smaller RSS** (RTF 12.39, `q8_0`, RSS 267 → 228 MiB) |
| `i7-7500u / cpu / small` | borderline (RTF 1.06, fp16) | **comfortable enough** (RTF 2.51, `q5_1`) — first time `small` clears 2.0 on the weakest tier |

## Already merged this session

(Not yet committed; pre-commit gate still to run before commit.)

- `crates/fono-stt/src/whisper_local.rs` — `set_audio_ctx()` hard-coded
  on for release builds; bench-only override behind
  `cfg(debug_assertions)` via `FONO_WHISPER_AUDIO_CTX`. Threads default
  to physical-core count parsed from `/proc/cpuinfo`, clamped to
  `1..=16`, overridable via `FONO_WHISPER_THREADS`.
- `scripts/bench-accuracy.py` — accuracy aggregator (TODO: per-language
  split for multilingual models, see Task 4.2).
- `ROADMAP.md` — "Custom-quantized `large-v3-turbo`" added to
  *On the horizon* as a research item (we will not self-quantize a
  `turbo-q5_1` until upstream publishes it or a user complaint
  motivates the build / sign / host pipeline).

## Phase 1 — Registry rewrite

### Task 1.1 — Reshape `ModelInfo` to carry quantization

In `crates/fono-stt/src/registry.rs`:

- Add a `Quantization { Fp16, Q5_1, Q8_0 }` enum.
- Add `quantization: Quantization` to `ModelInfo`.
- Continue to key entries by `name` (the user-facing model name like
  `"small"`), but allow multiple `ModelInfo` rows per name — one per
  quantization the registry knows about.
- Add `is_default: bool` on each row; exactly one row per `name` is the
  default for `[stt.local].quantization = "auto"`.

The single source of truth becomes: pick the row where
`name == user_choice && (quantization == user_quant || (user_quant == Auto && is_default))`.

### Task 1.2 — Populate the new registry

Replace the current 7-entry list with the following 11 entries (each
`url_path` extends `ggerganov/whisper.cpp/resolve/main/`):

| name | quantization | is_default | url_path | size MiB |
|---|---|:---:|---|---:|
| tiny | Q5_1 | ✓ | `ggml-tiny-q5_1.bin` | 31 |
| tiny.en | Q5_1 | ✓ | `ggml-tiny.en-q5_1.bin` | 31 |
| small | Q5_1 | ✓ | `ggml-small-q5_1.bin` | 182 |
| small | Q8_0 |  | `ggml-small-q8_0.bin` | 253 |
| small | Fp16 |  | `ggml-small.bin` | 466 |
| small.en | Q8_0 | ✓ | `ggml-small.en-q8_0.bin` | 253 |
| small.en | Q5_1 |  | `ggml-small.en-q5_1.bin` | 182 |
| small.en | Fp16 |  | `ggml-small.en.bin` | 466 |
| large-v3-turbo | Q8_0 | ✓ | `ggml-large-v3-turbo-q8_0.bin` | 834 |
| large-v3-turbo | Fp16 |  | `ggml-large-v3-turbo.bin` | 1 620 |

(`tiny` and `tiny.en` ship q5_1 only — the wizard wants exactly one row
per name in the shortlist; the q8_0/fp16 of tiny don't appear because
nothing on the user side would pick them.)

The four currently-present `base`, `base.en`, fp16 `small`, fp16 `tiny`
entries are removed.

### Task 1.3 — Refit `realtime_factor_cpu_avx2` and `wer_by_lang`

Each `ModelInfo.realtime_factor_cpu_avx2` is refitted from the
2026-05-19 perf-pass batch RTFs on the `ultra7-258v` 8-core AVX2
reference host (matching the existing convention in
`registry.rs:212-219`). New values:

| name (default quant) | new `realtime_factor_cpu_avx2` |
|---|---:|
| tiny (q5_1) | 30.0 |
| tiny.en (q5_1) | 36.0 |
| small (q5_1) | 8.3 |
| small.en (q8_0) | 3.3 |
| large-v3-turbo (q8_0) | 2.3 |

`wer_by_lang` is recomputed from the equivalence harness: for each
default row, take the per-language Levenshtein accuracy across our
fixtures, convert to a percentage WER, and store it. Non-default
quantization rows reuse the same `wer_by_lang` only if the Δacc gate
holds; otherwise carry a slightly worse value.

### Task 1.4 — `min_ram_mb` from peak RSS

Per-cell peak RSS from
`docs/bench/2026-05-19-perf-pass/summary/matrix.json`, rounded up to a
conservative ceiling:

| default row | peak RSS observed | `min_ram_mb` (with headroom) |
|---|---:|---:|
| tiny-q5_1 | 311 MiB | 1024 |
| tiny.en-q5_1 | 309 MiB | 1024 |
| small-q5_1 | 805 MiB | 1536 |
| small.en-q8_0 | 875 MiB | 1536 |
| large-v3-turbo-q8_0 | 2.24 GiB CPU / 0.27 GiB Vulkan | 3 072 |

### Task 1.5 — Tests

- Update `default_small_model_is_multilingual` and the other tests at
  `registry.rs:300-425` to the new entries.
- Add `default_picks_q5_1_for_tiny`, `default_picks_q8_0_for_small_en`,
  `default_picks_q8_0_for_turbo`.
- Add `dropped_models_not_in_registry` covering `base`, `base.en`, plus
  the q5_0 family.
- Add `fp16_turbo_reachable_only_via_explicit_quant_override`.

## Phase 2 — Wizard integration

### Task 2.1 — Add the `[stt.local].quantization` config field

In `crates/fono-app/src/config.rs`:

```toml
[stt.local]
model = "small"           # user-facing name (unchanged)
quantization = "auto"     # "auto" | "fp16" | "q8_0" | "q5_1"
```

`"auto"` resolves through the registry's `is_default` rows;
explicit values pin to the matching `(name, quantization)` row, with a
clear `anyhow::bail!` if no such combination exists in the registry.

### Task 2.2 — Wizard shortlist (no architectural change)

`pick_local_stt_model` and `build_local_stt_shortlist` in
`crates/fono/src/wizard.rs:1460-1620` already filter by
`multilingual != english_only`, `Affordability`, and
`AccuracyBucket`. After Task 1.2 they collapse to "is_default rows
only" automatically, because the non-default rows have `is_default ==
false` and are excluded from the shortlist filter.

No new logic; just verify the unit tests pass and that
`pick_default_local_scales_to_hardware` (registry.rs:412) still picks
the right rung on the four reference hosts.

### Task 2.3 — Friendly labels

Update `friendly_model_label` at `wizard.rs:1542` to reflect the new
defaults:

```
"tiny" | "tiny.en" => "Tiny (fastest, English/Spanish-grade quality)",
"small" | "small.en" => "Small (balanced — recommended sweet spot)",
"large-v3-turbo" => "Turbo (top quality, needs a GPU or beefy CPU)",
```

The "(advanced)" `[stt.local].quantization = "fp16"` override is
documented in `docs/providers.md` rather than surfaced in the wizard.

## Phase 3 — Downloader + cache

### Task 3.1 — Resolve `quantization` to URL in the downloader

`crates/fono-stt`'s model downloader currently builds a URL from
`name`. Switch to resolving `(name, quantization)` through the registry
and use `ModelInfo.url_path`.

### Task 3.2 — Cache path scheme

Cache file becomes
`~/.cache/fono/models/whisper/ggml-<name>-<quant>.bin`, matching the
upstream file naming exactly. fp16 of any model uses
`ggml-<name>.bin` (no `-fp16` suffix — upstream convention).

### Task 3.3 — Eviction / GC of dropped models

Optional: when the daemon notices a cached file for a name that is no
longer in the registry (`base.bin`, `base.en.bin`, etc.), log a
`fono.cache=info` line announcing the unused file and its size, but do
not delete. Users who run `fono cache gc` (future task, out of scope
here) will pick it up.

## Phase 4 — Bench harness hygiene

### Task 4.1 — Hard-code `set_audio_ctx()`, drop the env knob

✅ done in this session — `whisper_local.rs:187-208`. Verify the
release binary no longer reads `FONO_WHISPER_AUDIO_CTX`; bench-only
override stays under `cfg(debug_assertions)`.

### Task 4.2 — Per-language accuracy split

`scripts/bench-accuracy.py` currently averages across all fixtures
present in a run. For multilingual models this hides English-language
quantization regressions when non-English fixtures sit near the model's
quality floor. Update the script to:

- For each model row, emit separate columns per ISO language code
  detected from the fixture metadata.
- Aggregate the acceptance-rule check on each model's *dominant*
  target language (default: `en`; configurable via `--target-lang`).

This is the same fix the manual analysis in this session did by hand
when caught by the `base` regression on `en-narrative-pause`.

### Task 4.3 — Re-run battery sweep

Today's perf-pass sweep was AC-only. Re-run the same MODELS set on
battery on `i7-7500u`, `i7-1255u`, `ultra7-258v` to refresh the
affordability rows for battery profiles before the registry change
ships. Budget: ~30 min per host.

## Phase 5 — Documentation

### Task 5.1 — Update `docs/providers.md`

Replace the local STT section to describe the 3-rung ladder by
language mode, the `[stt.local].quantization` override, and link to
`accuracy.md` for the underlying numbers.

### Task 5.2 — Update `docs/status.md`

Add a "Local STT perf pass (2026-05-19)" entry referencing the new
default model variants, the verdict-bucket flips, and the dropped
families.

### Task 5.3 — ADR

Add `docs/decisions/0026-stt-quantization-ladder.md` covering:

- Acceptance rule (mean Δ ≤ +0.05, max Δ ≤ +0.20, smallest wins).
- Why we ship five files, not 11 or 21.
- Why `base` is dropped despite the wide installed base of the name.
- Why `tiny` multilingual is kept despite poor non-English accuracy
  (delegated to existing `wer_by_lang` gating).
- Why fp16 turbo remains opt-in.

## Phase 6 — Release gate

### Task 6.1 — Pre-commit gate

`cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --tests --lib`.

### Task 6.2 — DCO-signed commit

One commit covering registry + wizard + downloader + tests + docs.
Subject: `stt: collapse local model registry to a 3-rung quantization ladder`.

### Task 6.3 — `CHANGELOG.md`

Add a `## [Unreleased]` (or pinned next-version) section noting:

- Default local STT models now ship quantized (q5_1 / q8_0). Existing
  users keep their currently-cached fp16 files; nothing is auto-deleted.
- `base` and `base.en` are no longer offered by the wizard.
- New `[stt.local].quantization` config knob (auto / fp16 / q8_0 / q5_1).

### Task 6.4 — `ROADMAP.md`

The custom-quantized turbo entry is already added (this session).

## Out of scope (deferred)

- Self-quantizing `large-v3-turbo-q5_1` — roadmap research item.
- Distil-whisper integration — separate plan; revisit after this ships.
- Renaming the user-facing model names ("small" / "tiny") to be
  quality-tier-oriented ("balanced" / "minimal") — UX-only change,
  separable.
- Replacing the bench harness's accuracy metric (Levenshtein) with a
  proper WER tokenizer — accurate enough for our acceptance rule
  today; revisit if we add German / Japanese fixtures.

## Verification

For each task above, the gate is one of:

- **Unit test** added/updated and passing.
- **Bench cell** matches the expected verdict bucket (validated against
  `docs/bench/2026-05-19-perf-pass/summary/matrix.json`).
- **Wizard interactive smoke**: on each of the four reference hosts,
  run `fono setup` against a fresh `~/.config/fono/config.toml` and
  confirm the recommended model matches the table at the top of this
  document.
