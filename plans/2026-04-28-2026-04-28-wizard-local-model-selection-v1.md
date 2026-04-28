# Wizard Local Model Selection — language-aware, hardware-aware, expectation-set

## Objective

Replace the current "tier picks a multilingual whisper model" flow at
`crates/fono/src/wizard.rs:404-438` with a three-step funnel:

1. **Language scope first.** Ask up-front "English only or multilingual?"
   — English-only models (`tiny.en` / `base.en` / `small.en` / `medium.en`)
   are quality-better and resource-cheaper at every size tier. Today the
   wizard never offers them.
2. **Hardware-aware shortlist.** From the snapshot already produced by
   `HardwareSnapshot::tier()` at `crates/fono-core/src/hwcheck.rs:128`,
   compute the largest model the host can run *comfortably*, the largest
   it can run *at the edge*, and an explicit "lighter / faster" rung. Use
   GPU presence as an upgrade hint, not a requirement.
3. **Per-language quality estimate.** For the model selected, render a
   per-selected-language WER (Word Error Rate) estimate sourced from
   published whisper benchmarks (FLEURS / Common Voice). The user sees
   "small (en): ≈4 % WER" before confirming, not after. If the language
   set is non-English and the user accidentally picked an `.en` model,
   the picker refuses with an explanation.

This addresses three concrete UX gaps:

- A native English speaker on a laptop with 4 GB RAM is currently
  pushed to `whisper base` multilingual (~10 % en WER) when
  `base.en` (same size, ≈6 % en WER) would serve them better.
- A bilingual user (en + ro) is offered `medium` only on `HighEnd`,
  but `small` is the realistic best-quality choice for ro on a
  Recommended-tier laptop (medium is GPU-bound for live mode).
- No expectation-setting — users discover error rates by trying.

## Implementation Plan

- [ ] Task 1. **Extend the model registry with quality data.**
  Add `wer_by_lang: &'static [(LangCode, f32)]` to
  `ModelInfo` at `crates/fono-stt/src/registry.rs:5-13`. Populate
  from FLEURS (en + 12 most-common dictation languages: es, fr, de,
  it, pt, nl, ro, pl, ru, uk, tr, zh, ja). For languages not in
  FLEURS, leave the entry absent — the UI renders "no published
  benchmark" instead of guessing. Add `min_ram_mb: u32` and
  `realtime_factor_cpu_avx2: f32` (audio-seconds processed per
  wall-second on an AVX2 reference) so the picker can compute
  "this model will keep up with live mode on your machine: yes /
  no / borderline." Keep the field optional via `Option<f32>` so
  unknown / unmeasured rows don't lie. Cite the data source in a
  doc-comment block; `WHISPER_MODELS` becomes the single source of
  truth for "what does this model cost / deliver."

- [ ] Task 2. **Hardware-aware affordability function.** New
  `crates/fono-core/src/hwcheck.rs::HardwareSnapshot::affords(model: &ModelInfo) -> Affordability`
  returning `{ Comfortable, Borderline, Unsuitable }`. Logic:
  - `Unsuitable` if `available_ram_mb < model.min_ram_mb` or free
    disk < 2 × `model.approx_mb` (room for redownload).
  - `Borderline` if it fits in RAM but
    `model.realtime_factor_cpu_avx2 < 1.5` on the snapshot's
    detected CPU class (live streaming will lag).
  - `Comfortable` otherwise.
  Pure function on `HardwareSnapshot` + `ModelInfo`; covered by
  unit tests with synthetic snapshots (no live probe). Replaces
  the hard-coded `LocalTier::default_whisper_model()` heuristic
  with data-driven recommendations; `default_whisper_model()`
  stays for one release with `#[deprecated]`.

- [ ] Task 3. **Reorder the wizard.** In `configure_local` /
  `configure_mixed` (`crates/fono/src/wizard.rs:198-307`), swap
  the order so `pick_languages` runs *before* `pick_local_stt_model`.
  Today languages are picked at step 5 of `configure_local`; they
  have to be known at step 1 (STT) so the model picker can hide
  `.en` variants when the language set isn't `["en"]`. This is a
  pure plumbing change — both pickers exist already.

- [ ] Task 4. **English-only fast-path question.** Before
  `pick_languages`, add a `Confirm` prompt:
  > "Will you dictate only in English? (English-only models are
  > smaller and more accurate per MB.)"
  - **Yes** → `config.general.languages = vec!["en".into()]`,
    skip the multi-language checkbox UI, and the STT picker only
    shows `.en` variants.
  - **No** → fall through to the existing checkbox picker; STT
    picker shows multilingual variants only.
  This is the bilingual-friction-vs-quality trade-off made
  explicit. Default cursor is "Yes" because most first-time users
  are mono-lingual; OS locale `en_*` reinforces that default.

- [ ] Task 5. **Rewrite `pick_local_stt_model`.** Replaces the
  three-tier hand-coded match at `crates/fono/src/wizard.rs:404-438`
  with a data-driven shortlist:
  - Filter `WHISPER_MODELS` by english-only flag matching the
    user's choice from Task 4.
  - For each remaining model, compute `affords(model)`.
  - Render a `Select` whose default cursor is on the *largest*
    `Comfortable` model. Items show:
    `small.en (~466 MB) — ≈4 % WER on en, fits comfortably`
    `base.en  (~142 MB) — ≈6 % WER on en, fits comfortably`
    `medium.en (~1.5 GB) — ≈3 % WER on en, borderline (live mode may lag)`
    `Borderline` items render but with a yellow warning suffix.
    `Unsuitable` items are filtered out entirely with a footer
    line ("medium.en hidden — needs ≥4 GB RAM, you have 2 GB").
  - When the user has multiple languages, render WER for each
    selected language: `small (~466 MB) — ≈4 % en, ≈8 % ro`.
    Languages with no FLEURS data render `(no published benchmark)`
    so the user knows the estimate is partial.

- [ ] Task 6. **Picker validation.** Refuse `.en` variants when
  `config.general.languages` contains anything other than `["en"]`
  (with a clear error message pointing at Task 4). Refuse `Unsuitable`
  variants outright. Both refusals route back to the picker rather
  than persisting an unrunnable config.

- [ ] Task 7. **Surface the same shortlist in `models install`.**
  `crates/fono/src/cli.rs` currently lists every variant; add a
  `--recommend` flag that filters by the same `affords()` +
  english-only logic and prints the shortlist with WER rows. Lets
  CLI users get the same recommendation without going through the
  wizard. Out of scope to change the default `models list` output.

- [ ] Task 8. **Tests.**
  - `registry.rs` — for each model, `wer_by_lang` includes `en`
    (sanity check); `min_ram_mb` is monotonic with `approx_mb`
    within the en-only family and within the multilingual family.
  - `hwcheck.rs` — `affords()` table-driven tests with synthetic
    `HardwareSnapshot` rows covering every cell of (RAM × disk ×
    AVX2 × model_size).
  - `wizard.rs` — extract `pick_local_stt_model` into a
    pure-function helper that takes `(english_only: bool, langs:
    &[String], snapshot: &HardwareSnapshot) -> Vec<ShortlistEntry>`
    so the shortlist construction is unit-testable without a TTY.
    Cover three scenarios: HighEnd + en-only → `[medium.en (default),
    small.en, base.en]`; Recommended + multilingual → `[small
    (default), base]`; Unsuitable + multilingual → `[base]` only.

- [ ] Task 9. **Doc updates.**
  - `docs/providers.md` — new "Local STT model selection" section
    explaining the en-only vs multilingual quality/cost trade-off
    with a table of WER + size + min-RAM rows.
  - `docs/wizard.md` (new) or extend an existing wizard doc —
    document the three-step funnel and the `Affordability`
    classification, with a worked example for each tier.
  - `CHANGELOG.md` — `Changed` entry summarising the wizard
    overhaul; `Added` entry for the registry quality data.

- [ ] Task 10. **Migration note.** Existing configs with
  `stt.local.model = "small"` keep working; the wizard only runs on
  first-launch / `fono init`. No config-format change. The
  `LocalTier::default_whisper_model()` deprecation gives one
  release of warning before removal.

## Verification Criteria

- **Native English speaker, mid-tier laptop (8 GB RAM, AVX2,
  Recommended).** Wizard offers `[small.en (default), base.en,
  medium.en (borderline)]` with WER rows; user sees `≈4 % WER`
  before confirming. Picking `medium.en` shows the
  borderline-warning copy.

- **Bilingual user (en + ro), mid-tier laptop.** After Task 4
  "No" + checkboxes for en + ro, the STT picker offers
  `[small (default), base]` with two WER columns each
  (`≈4 % en, ≈8 % ro`). `medium` filtered as borderline. No `.en`
  variants offered.

- **Low-RAM machine (2 GB).** All `medium*` and `small*` filtered
  out as Unsuitable; footer line tells the user *why* each was
  hidden. Picker degrades gracefully to `[base.en (default)]` (or
  `base` if multilingual).

- **High-end workstation (32 GB RAM, AVX-512).** Defaults to
  `medium.en` for English-only users, `medium` for multilingual.
  No `Borderline` rows.

- **Refusing inconsistent picks.** A user who manually edits
  `stt.local.model = "small.en"` while `general.languages = ["ro"]`
  triggers a startup error pointing at the constraint, not silent
  garbage transcription.

- `cargo test --workspace --all-features` green; clippy clean;
  `target/debug/fono` builds without warnings.

## Potential Risks and Mitigations

1. **WER estimates are slippery numbers** (different test sets give
   different results; a user's accent / mic / domain may halve or
   double the reported figure).
   Mitigation: render every WER as `≈X %` with a footer caveat
   ("Real-world accuracy varies — see docs/providers.md"). Cite
   the FLEURS source in `registry.rs`. Don't promise WER for
   languages we don't have data for.

2. **Realtime-factor benchmarks are platform-dependent** (an AVX-512
   server differs from an Apple M1 differs from a Pi). The
   `realtime_factor_cpu_avx2` field is a single reference number;
   live performance may vary.
   Mitigation: classify into three buckets (Comfortable /
   Borderline / Unsuitable) rather than report a number directly,
   and document the reference platform in `registry.rs`. The cost
   of a wrong "Borderline" call is one re-run of the wizard.

3. **Adding GPU detection feature-creeps the plan.** GPU-accelerated
   whisper requires `accel-cuda` / `accel-metal` / `accel-vulkan`
   feature flags; the binary may not have them.
   Mitigation: limit Task 2 to *available* hardware in the current
   binary's feature set. If `accel-cuda` is compiled in and a GPU
   is present, mark the next-larger model as Comfortable;
   otherwise treat the host as CPU-only. Don't probe GPUs we
   can't use.

4. **English-only-first question may feel patronising to
   sophisticated users.**
   Mitigation: copy is a single sentence with an explanation
   parenthetical; default cursor is "Yes" (covers ~70 % case);
   "No" path is one extra Enter. Power users editing
   `config.toml` directly aren't affected.

5. **Tests that depend on `WHISPER_MODELS` data may rot when WER
   numbers are re-measured.**
   Mitigation: tests assert *relationships* (e.g. "`small.en` WER
   < `tiny.en` WER", "`medium` RAM > `small` RAM") rather than
   exact values. Re-measuring updates the constants without
   touching the tests.

## Alternative Approaches

1. **Ship a benchmark mode that measures the user's own machine.**
   Run a 10-second self-test with each candidate model on first
   launch and pick the largest one that hits real-time. Most
   accurate; defeats the "Apple-seamless" goal because it adds
   ~30 s to first-run and downloads 2 GB of model files just to
   run the benchmark. Defer to a `fono benchmark` subcommand for
   power users.

2. **Hide the choice entirely; pick automatically.** Combine
   en-only detection (from `LANG`) + tier + language set into a
   single recommendation, no Select dialog at all. Less friction;
   loses the expectation-setting (no WER shown). Compromise: do
   this only when `--unattended` is set (CI / scripts).

3. **Use `quality_score` instead of WER.** A 0-10 score is easier
   to read but less honest — users can't compare it against any
   external benchmark. WER is the industry standard; ship the
   real number with caveats.

4. **Add a separate "speed vs accuracy" slider.** Two-dimensional
   pickers (size × quantization × language scope) are too rich
   for first-run; quantization can stay a power-user feature in
   `config.toml`. Slider defers to a later wave once we have
   quantized models in the registry.
