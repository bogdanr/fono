# ADR 0027 — Local STT model quantization ladder

- **Status:** Accepted
- **Date:** 2026-05-19
- **Plan:** [`plans/2026-05-19-stt-perf-pass-v1.md`](../../plans/2026-05-19-stt-perf-pass-v1.md)
- **Supersedes (partially):** [ADR 0004 — Default models](0004-default-models.md) — the model name choices stand; the *quantization* picked per model is what's new here.

## Context

The 2026-05-18 / 2026-05-19 perf-pass swept four reference hosts
(i7-7500u, i7-1255U, ultra7-258V, ryzen-5950x) across every
combination of {fp16, q8_0, q5_1, q5_0} × {tiny, tiny.en, base,
base.en, small, small.en, large-v3-turbo}. Results:

- `set_audio_ctx()` on clips < 30 s gives +70–160 % CPU batch RTF
  with no measurable quality regression — already merged
  unconditionally in `crates/fono-stt/src/whisper_local.rs`.
- Quantization changes the **disk × speed × quality** trade-off
  per model. The earlier all-language mean Δ accuracy was misleading
  for multilingual models: non-English fixtures sit at the model's
  quality floor where quantization noise is invisible, while
  English fixtures sit near the ceiling where any drift shows.
  `base-q8_0` looked acceptable on the all-language mean (+0.04 pp)
  but degraded `en-narrative-pause` from acc 0.114 → 0.513
  (`bench/2026-05-19-perf-pass/summary/accuracy.md`).
- The `base` family is dominated by `small-q5_1` / `small.en-q8_0`
  on every reference host once quantization is in play: 40 MB more
  disk for strictly better quality and similar RTF.

The previous registry exposed 7 models × ~3 quantizations × fp16 as
freely-pickable strings (`small-q5_1`, `small`, etc.) with no
guidance. There were no users to break.

## Decision

### Acceptance rule

For each base model, a quantization may default in the registry iff
**both** hold on the equivalence harness (`stt_accuracy_levenshtein`,
deterministic Vulkan lane on `ultra7-258v`, n ≥ 8 fixture-runs):

| gate | threshold |
|---|---|
| English-only **mean** Δ acc vs fp16 | ≤ +0.05 |
| English-only **max per-fixture** Δ acc vs fp16 | ≤ +0.20 |

The smallest passing quantization wins. If no quantization passes,
the default is fp16. The English-only split is enforced by
`scripts/bench-accuracy.py` so future sweeps surface regressions of
the kind that hid `base-q8_0` initially.

For `.en` models the harness already skips non-English fixtures via
`SkipReason::Capability`; the rule applies to the full evaluated set.

### Ladder

The registry ships **5 user-facing model names** with one defaulted
quantization each (3-rung quality ladder × 2 language modes, with
T3 shared):

| Rung | Multilingual | English-only | Approx size |
|---|---|---|---:|
| **T1 — minimal** | `tiny` → `q5_1` | `tiny.en` → `q5_1` | 31 MB |
| **T2 — sweet spot** | `small` → `q5_1` | `small.en` → `q8_0` | 182 / 253 MB |
| **T3 — quality** | `large-v3-turbo` → `q8_0` | `large-v3-turbo` → `q8_0` | 834 MB |

Reachable alternatives via `[stt.local].quantization`:

- `small` supports `q5_1` (default), `q8_0`, `fp16`.
- `small.en` supports `q8_0` (default), `q5_1`, `fp16`.
- `large-v3-turbo` supports `q8_0` (default), `fp16`.
- `tiny` / `tiny.en` ship `q5_1` only — fp16 quality delta is
  invisible to users and not worth the maintenance.

### Dropped

- All `base` / `base.en` entries — dominated by T2.
- All `*-q5_0` files — `large-v3-turbo-q5_0` broke
  `en-conversational` (acc 0 → 0.354). q5_1 is strictly better
  whenever published.
- `tiny-q8_0`, `tiny.en-q8_0` — equal quality to q5_1 at larger
  size.
- `small`, `small-q8_0` (multilingual) — both dominated by `small-q5_1`.

### `tiny` multilingual caveat

`tiny` multilingual is unusable for Romanian (acc 0.20–0.38),
Chinese (acc 0.50), Japanese (per registry `wer_by_lang`). The
existing `AccuracyBucket::Inaccurate` filter in
`crates/fono/src/wizard.rs` already refuses such pairings — we keep
`tiny-q5_1` in T1 and rely on the predicate to gate it per
configured language. Users on weak hardware with hard languages
get escalated to T2 (borderline) or cloud STT.

### Config surface

The user-visible knobs are exactly two:

```toml
[stt.local]
model        = "small"            # name from the registry
quantization = "auto"             # "auto" | "fp16" | "q8_0" | "q5_1"
```

`"auto"` resolves to `default_quantization` for the model. A pinned
quantization that the model doesn't ship falls back to the closest
available with a logged warning; an entirely unknown name is a
hard error from `resolve_local_model_path` with the install hint.

### Roadmap deferral

Self-quantizing `large-v3-turbo` to `q5_1` (would sit between T2 and
T3 at ~548 MB) is on the roadmap (`ROADMAP.md` "On the horizon")
as a research item. Not blocking any current user need; revisit
when ggerganov publishes upstream or the T2→T3 gap triggers a
complaint.

## Consequences

- **Disk footprint** of all three rungs in one language mode drops
  from ~3 GB (mixed fp16) to ~1.1 GB (English) / ~1.3 GB
  (multilingual). T1 alone is now 31 MB.
- **Verdict bucket flips** confirmed:
  - `ultra7-258v / cpu / large-v3-turbo`: unsuitable (RTF 0.62) →
    borderline (RTF 2.31 via q8_0 + audio_ctx).
  - `ryzen-5950x / cpu / large-v3-turbo`: borderline → comfortable.
- **Affordability registry rows** must be refit from
  perf-pass measurements (separate ticket inside the plan); the
  recalibration ADRs (0023, 0025 on the affordability side) remain
  the source of truth for the predicate itself.
- **Bench harness** must continue producing per-language accuracy
  splits before any future change to defaults — codified in
  `scripts/bench-accuracy.py`.
- **No user data migration** needed: pre-release; we are free to
  reshape config keys and registry entries without a compat shim.
