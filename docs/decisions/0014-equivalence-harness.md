# ADR 0014 — Equivalence harness

## Status

Reconstructed (original lost in filter-branch rewrite; rationale
recovered from `docs/status.md` Slice A entry and plan history at
`plans/2026-04-27-fono-interactive-v6.md:48`, 2026-04-28).

## Context

Slice A of the interactive plan introduced a streaming STT lane on top
of the existing batch lane. Two lanes that produce two transcripts for
the same audio will inevitably drift; without a regression gate, a
streaming-lane refactor can silently degrade quality and the only
signal will be user complaints.

## Decision

Build an equivalence harness in `crates/fono-bench` that, for each
fixture audio file, runs both lanes (batch and streaming) and gates
the report on `levenshtein_norm(stream_text, batch_text) ≤
fixture.equivalence_threshold`. The harness:

- Lives in `crates/fono-bench/src/equivalence.rs` and ships behind
  the `equivalence` cargo feature.
- Reads a TOML manifest at
  `tests/fixtures/equivalence/manifest.toml` listing fixtures with
  per-fixture `language`, `reference`, and `levenshtein_threshold`.
- Emits a JSON report with per-fixture `Verdict` (`Pass`, `Fail`,
  `Skipped`) and an aggregate `overall_verdict`.
- Is invoked via `fono-bench equivalence --stt local --model tiny.en
  --output report.json`, with `tests/bench.sh` as a thin wrapper for
  the canonical CI invocation.

## Consequences

- Streaming-lane regressions are caught before they reach users.
- Adding a fixture is a pure-data change (drop a WAV + manifest row).
- The harness later grew a second gate (`accuracy` against the manifest
  reference) — see
  `plans/2026-04-28-equivalence-harness-language-gating-and-accuracy-v1.md`.
- The "real-fixture CI gate" piece of this is still partial: today CI
  runs `cargo bench --no-run` (compile sanity) only; the run-and-compare
  gate is tracked as Wave 2 Task 9 of the doc-reconciliation plan.

## Surviving artefacts

- `crates/fono-bench/src/equivalence.rs`
- `tests/fixtures/equivalence/manifest.toml`
- `tests/bench.sh`
- `plans/2026-04-27-fono-interactive-v6.md:48`
- `docs/status.md` Slice A entry (R17.1 / R18 foundation)
