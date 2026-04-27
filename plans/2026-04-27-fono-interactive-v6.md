# Fono — Interactive / Live Dictation + Equivalence Harness (R-plan v6)

Date: 2026-04-27
Status: Proposed (supersedes v5)
Scope changes from v5:

1. **New R18 module — Streaming↔Batch Equivalence Harness.** Promoted
   from "test consideration" to a first-class deliverable inside Slice
   A. Becomes the primary regression gate for every later slice.
2. **Streaming is a runtime config toggle**, not just a build feature.
   `[interactive].enabled` flips behavior at daemon start with no
   rebuild. The cargo `interactive` feature gates the *compilation* of
   streaming code (so slim builds stay slim); a build with the feature
   compiled in still respects `[interactive].enabled = false` at
   runtime.
3. **Slice A acceptance gate** updated to require equivalence-harness
   PASS on all curated fixtures across both Tier-1 (whisper-only) and
   Tier-2 (full-pipeline) test scopes.

## Locked architectural decisions

(All v1–v5 decisions carry over.)

19. **Streaming is runtime-toggleable.** The cargo feature controls
    code presence; the config toggle controls active behavior. Both
    paths (streaming on/off) compile from the same binary when the
    feature is enabled, allowing a single artifact to A/B test.
20. **Equivalence is checked on committed text only**, not on preview
    text. Preview is lossy by design; finalize-lane output must match
    batch output within tight tolerance.
21. **Equivalence harness is part of Slice A**, not Slice B+. Every
    later slice extends the harness fixture set and CI matrix.

## Implementation Plan

### R1 — R15 (carryover from v5)

[Unchanged.]

### R16 — Tray icon-state palette (carryover; ships in Slice B)

[Unchanged.]

### R17 — Docs + ADRs (extended numbering)

[Carryover from v5; new ADR added below.]

- [ ] R17.7. ADR `0014-equivalence-harness.md` — records the
  streaming↔batch equivalence guarantee, the tolerance taxonomy,
  fixture curation policy, CI matrix design, and the rule that every
  future slice extends rather than replaces the harness.

### R18 — Streaming↔Batch Equivalence Harness (NEW; in Slice A)

#### R18a — Tolerance taxonomy

- [ ] R18.1. Define `EquivalenceMetric` enum + threshold constants in
  `crates/fono-bench/src/equivalence/metrics.rs`:
  - `LevenshteinNorm` — committed-vs-batch character distance, ≤ 0.01
    for whisper-only, ≤ 0.02 for full-pipeline at temp=0.
  - `WordErrorRate` — informational metric for preview text only;
    threshold ≤ 0.05 absolute, **does not block CI**.
  - `TtffRatio` — streaming time-to-first-feedback / batch
    time-to-commit. Target ≤ 0.4 on at least 10/12 fixtures.
  - `TtcRatio` — streaming time-to-commit / batch time-to-commit.
    Hard cap ≤ 1.5 on every fixture.
  - `CpuRatio` — streaming CPU-seconds / batch CPU-seconds. Hard cap
    ≤ 1.5 on every fixture.
- [ ] R18.2. Per-metric threshold is overridable per-fixture in the
  manifest, so noisy/edge fixtures can document their looser bar
  rather than being silently excluded.

#### R18b — Fixture curation

- [ ] R18.3. Curate 12-fixture starter set committed at
  `tests/fixtures/equivalence/`. Each fixture is ≤ 30 s, CC0 / CC-BY /
  consented, totalling ~10 MB. Coverage targets (one fixture each):
  short-clean (3 s), medium-with-pauses (15 s), long-monologue (45 s),
  noisy-cafe (10 s), accented-EN (10 s), numbers/commands (8 s),
  whispered (5 s), with-music (12 s), multi-speaker (15 s),
  code-dictation (10 s), long-with-pauses (60 s),
  short-noisy-quick (2 s).
- [ ] R18.4. Manifest at `tests/fixtures/equivalence/manifest.toml`
  records per fixture: SHA-256, source URL, license, expected
  reference transcription, expected language, per-fixture metric
  threshold overrides (default = global thresholds).
- [ ] R18.5. Fixture-fetcher script at
  `tests/fixtures/equivalence/fetch.sh` — pulls from upstream sources
  on first checkout (LibriSpeech / Common Voice mirrors), verifies
  SHA-256, populates the local fixture dir. CI caches the fetched dir
  by manifest hash.

#### R18c — Bench harness

- [ ] R18.6. New module `crates/fono-bench/src/equivalence/`.
  Sub-command `fono bench equivalence` with flags:
  ```
  --fixtures <dir>           (default tests/fixtures/equivalence)
  --stt local|cloud-mock     (default local)
  --llm none|local|cloud-mock (default none → Tier-1 mode)
  --mode batch|streaming|both (default both)
  --output <report.json>     (also emits stdout table by default)
  --baseline <baseline.json> (compare to baseline; fail on > 10%
                             regression on any per-fixture metric)
  ```
- [ ] R18.7. Per-fixture per-mode execution:
  - **Batch mode**: feeds full PCM to the configured `SpeechToText`
    via `transcribe()`; runs LLM if configured.
  - **Streaming mode**: feeds frame-by-frame to `stream_transcribe()`;
    captures preview updates, finalize updates, latency markers;
    runs LLM cleanup once at end on the assembled committed text.
- [ ] R18.8. Per-fixture report record (JSON-serialisable) — see v5
  plan section "Test harness" for the schema. Verdict computed from
  R18.1 thresholds + per-fixture overrides.
- [ ] R18.9. Aggregate report emits PASS/FAIL exit code and a
  human-readable summary table. Markdown variant written to
  `docs/bench/equivalence-<git-sha>.md` for traceability.

#### R18d — Determinism controls

- [ ] R18.10. Pin all probabilistic decode parameters at harness
  startup: whisper beam size, temperature schedule, `n_threads`; LLM
  temperature=0 + seed-pinning; VAD threshold; resampler config. Each
  pinned value recorded in the report so a future failure is
  reproducible from the report alone.
- [ ] R18.11. Mock-clock provider for non-latency code paths so
  history-write timestamps and other clock-dependent fields are
  deterministic. Latency measurements still use real `Instant`.
- [ ] R18.12. **Mock-cloud STT/LLM backends** (`cloud-mock` flag):
  replay recorded HTTP/SSE/WebSocket exchanges from `tests/fixtures/
  cloud-recordings/` for deterministic CI without API keys. Each
  recording is captured once with `fono bench record-cloud --provider
  <name>` (run manually with real keys); replay is byte-for-byte
  deterministic.

#### R18e — Two-tier test scope (Tier-1 + Tier-2)

- [ ] R18.13. **Tier-1 — whisper-only**: `--llm none`. Compares
  streaming committed text vs batch text directly off the STT layer.
  Failure here = streaming decoder regression. Strictest tolerance
  (`LevenshteinNorm ≤ 0.01`). Required PASS on all 12 fixtures.
- [ ] R18.14. **Tier-2 — full-pipeline**: `--llm local` with a
  pinned small model (e.g., `qwen2.5-0.5b-instruct-q4_k_m.gguf`).
  Compares streaming committed-and-cleaned vs batch
  cleaned. Failure here that passes Tier-1 = LLM context-handling
  sensitive to streaming finalize timing. Looser tolerance
  (`LevenshteinNorm ≤ 0.02`). Required PASS on all 12 fixtures.
- [ ] R18.15. Tier-1 and Tier-2 run independently per CI row;
  failures in either block the slice.

#### R18f — Latency comparison

- [ ] R18.16. Per-fixture latency markers captured by the harness:
  - `batch.ttc_ms` — batch wall-clock from PCM-in to text-out.
  - `streaming.ttff_ms` — first preview update emission.
  - `streaming.first_stable_ms` — first finalize update emission.
  - `streaming.ttc_ms` — last finalize update + LLM cleanup
    completion.
- [ ] R18.17. Aggregate metrics in the report: TTFF ratio, TTC ratio,
  CPU ratio. The harness asserts the latency-win contract that
  justifies the slice (TTFF ≤ 0.4× batch TTC on ≥ 10/12 fixtures).
  Falling below this on more than 2 fixtures FAILS the slice — the
  perceived-latency win is the whole point.

#### R18g — CI matrix

- [ ] R18.18. New workflow `.github/workflows/equivalence.yml`. Trigger:
  PRs touching `crates/fono-stt/**`, `crates/fono-audio/**`,
  `crates/fono-overlay/**`, `crates/fono/src/session.rs`, or this
  plan. Matrix:
  | Row | `cargo --features` | `[interactive].enabled` | STT | LLM |
  |---|---|---|---|---|
  | A1 | `tray` | n/a (off-build) | whisper-base | none |
  | A2 | `tray,interactive` | true | whisper-base | none |
  | B1 | `tray,llama-local` | false | whisper-base | qwen-0.5b |
  | B2 | `tray,interactive,llama-local` | true | whisper-base | qwen-0.5b |
  Tier-1 gate: A1 vs A2. Tier-2 gate: B1 vs B2. Both gates required.
- [ ] R18.19. Cloud equivalence rows (Groq, OpenAI realtime,
  later Deepgram/AssemblyAI) run **nightly with secrets**, not on
  PR. Failure gates the next release tag but not PR merges.
- [ ] R18.20. Baseline JSON committed at
  `docs/bench/equivalence-baseline-v0.2.0-alpha.json`. PRs that
  worsen any per-fixture metric by > 10% relative to baseline FAIL
  with a structured diff in the PR comment. Baselines refresh
  manually on tag (no auto-overwrite).

#### R18h — Runtime toggle alignment

- [ ] R18.21. `[interactive].enabled` toggle is read at daemon start
  and at `Reload` IPC. Toggling at runtime requires a `Reload` (no
  daemon restart). Document in `docs/interactive.md`.
- [ ] R18.22. CLI `fono bench equivalence` toggles `[interactive]
  .enabled` per-mode programmatically — does not depend on the
  user's persistent config. Manual `fono record --live` /
  `fono record --no-live` still respects the persistent config when
  not overridden.

## Sequencing (deliverable slices, revised)

1. **Slice A** — Streaming + budget engine + overlay + **equivalence
   harness**:
   R1–R3, R5, R7 (partial), R10 (partial), R12, **R18**. v0.2.0-alpha.
2. **Slice B** — Cloud streaming + app context + tray icon palette:
   R4, R8.3–R8.4, R9.5, R10.4, R11, R13, R16.1+R16.2+R16.5+R16.6+R16.7.
   Extends R18 with cloud equivalence rows. v0.2.0.
3. **Slice C** — Voice command macros (independent): R9.6, R14, R16.3.
   v0.3.0.
4. **Slice D** — Wake-word activation (independent): R15, R16.4.
   v0.3.x or v0.4.0.
5. **Slice E** — Polish: R6, R4.3, richer app context. post-v0.3.

## Verification Criteria

(All v5 criteria carry over.)

### Slice A acceptance gate (revised)

- All existing 79 tests still green.
- ≥ 8 new unit/integration tests covering streaming, budget, overlay
  (carryover from v5).
- **R18 equivalence harness PASSES on all 12 fixtures**:
  - Tier-1 (whisper-only): `LevenshteinNorm ≤ 0.01` on every fixture.
  - Tier-2 (full-pipeline, local LLM): `LevenshteinNorm ≤ 0.02` on
    every fixture.
  - TTFF ratio ≤ 0.4× batch TTC on ≥ 10/12 fixtures.
  - TTC ratio ≤ 1.5× on every fixture.
  - CPU ratio ≤ 1.5× on every fixture.
- Baseline JSON committed at
  `docs/bench/equivalence-baseline-v0.2.0-alpha.json`.
- CI workflow `equivalence.yml` green on the slice's tagging commit.
- `cargo build --no-default-features --features tray` builds clean
  (slim build excludes streaming entirely).
- `cargo build --features tray,interactive` builds clean and the
  binary respects `[interactive].enabled = false` at runtime.

## Potential Risks and Mitigations

(All v5 risks carry over; new ones below.)

29. **Whisper greedy-decode is "deterministic enough" but not
    bit-identical across whisper-rs version bumps.** A whisper-rs
    bump may invalidate the baseline.
    Mitigation: pin whisper-rs version per release; baseline refresh
    is manual on every whisper-rs bump; PR that bumps whisper-rs
    must also refresh the baseline in the same commit.
30. **Local LLM (qwen-0.5b) determinism at temp=0 is high but not
    perfect on multi-thread inference** — float-add ordering varies
    across runs.
    Mitigation: pin `n_threads = 1` in equivalence harness even
    though production uses more; documented as a harness-only
    constraint; production performance unaffected.
31. **Fixture set bias** — 12 fixtures may not cover edge cases that
    matter to a specific user group (e.g., heavy code dictation,
    medical jargon).
    Mitigation: harness accepts a `--fixtures-extra <dir>` flag for
    user-supplied fixtures; community PRs adding fixtures encouraged
    in `CONTRIBUTING.md`.
32. **CI runtime cost** — running 12 fixtures × 4 matrix rows × both
    modes on every PR could blow the CI budget.
    Mitigation: equivalence workflow runs only on PRs touching
    streaming code paths (path-filter in `equivalence.yml`); other
    PRs skip it; nightly cloud-equivalence job amortizes cost.
33. **Cloud-mock recordings drift from live provider behavior.**
    Mitigation: nightly job hits real providers; mock recordings
    refreshed on a quarterly cadence or on observed drift; mock
    paths documented as "smoke test, not contract".

## Alternative Approaches

(All v5 alternatives carry over; new ones below.)

18. **Skip equivalence harness; rely on manual fixture inspection.**
    Faster to ship Slice A, but every later slice silently re-tests
    the same correctness questions. Rejected — equivalence is the
    only mechanism that catches a quiet quality regression.
19. **Bit-identical equivalence (string equality)** instead of
    tolerance-based.
    Tempting for clarity, but unattainable in practice: VAD
    segmentation differences alone produce small whitespace /
    punctuation variations that don't represent real quality
    regressions. Rejected.
20. **Run equivalence as a benchmark, not a gate.** Inform but don't
    block. Rejected — silent regressions are exactly what kills
    streaming pipelines over time. Hard gate is the point.
21. **Single-tier test (full-pipeline only).** Simpler harness, but
    failures don't tell us whether the bug is in STT or LLM
    handling. Two-tier scope produces materially better signal at
    minimal extra cost. Rejected single-tier.
