# Fono benchmark suite

Two distinct uses share this directory:

1. **Wizard calibration data** — the 210-cell (host × backend × model × quant)
   matrix that drives the first-run wizard's model-selection algorithm.
   Interactive decision page: [`calibration/index.html`](calibration/index.html)
   · live at **https://fono.page/calibration**
2. **CI equivalence gate** — a deterministic per-PR baseline that catches
   regressions in transcript quality. The committed JSON files below are that
   baseline.

## Calibration goal

The wizard's job is to pick, on first run, the heaviest local Whisper model that
runs comfortably in real time on the user's CPU or GPU — without a live probe and
without the user specifying anything. To validate and tune that algorithm we
benchmarked five representative Linux machines across a full
(model × quantisation × backend) grid.

| Metric | Comfortable gate | Meaning |
|--------|-----------------|---------|
| Batch RTF | ≥ 2.0 | audio-seconds processed per wall-second (higher = faster) |
| Peak RSS | ≤ 1024 MiB | resident memory ceiling |
| Registry WER | ≤ 15 % (Open-ASR-Leaderboard) | accuracy gate; wizard uses published means |

Each cell is three iterations of `fono-bench equivalence` over ten public-domain
WAV fixtures (≈ 100 s audio, five languages). See
[`calibration/README.md`](calibration/README.md) for the full protocol, host
roster, and headline findings.

---

## `fono-bench` per-PR equivalence gate

This directory holds the deterministic baseline that the CI workflow
diffs against on every pull request. The gate replaces the prior
`cargo bench --no-run` compile-only sanity step
(`.github/workflows/ci.yml`, Wave 2 Thread C) with a real-fixture
equivalence run against `tests/fixtures/equivalence/manifest.toml`.

### Files

- `baseline-comfortable-tiny-en.json` — committed baseline. Captures
  the per-fixture `verdict` (`pass` / `fail` / `skipped`),
  `skip_reason`, `model_capabilities`, `pinned_params`, and ratios.
  **Strips absolute timings** (`elapsed_ms`, `ttff_ms`, `duration_s`)
  because those flap on shared CI runners and aren't part of the
  contract.

### Contract

The CI step runs:

```
cargo run --release -p fono-bench --features equivalence,whisper-local -- \
  equivalence --stt local --model tiny.en \
  --output ci-bench.json --baseline --no-legend
```

and asserts that for every fixture in
`tests/fixtures/equivalence/manifest.toml`, the verdict in
`ci-bench.json` matches the verdict in this directory's baseline.

For `tiny.en`, the expected shape today is **4 English fixtures Pass,
6 non-English fixtures Skipped (capability-induced)**. Cloud-provider
rows are deferred to Wave 3 and beyond per
`docs/plans/2026-04-25-fono-roadmap-v2.md` R5.

### Regeneration procedure

The whisper `tiny.en` GGML weights live at
`~/.cache/fono/models/whisper/ggml-tiny.en.bin`. If absent:

```
cargo run -p fono -- models install whisper-tiny.en
```

(or `cargo run -p fono-download -- whisper tiny.en` for the bare
download path). Verify the SHA-256:
`921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f`.

Then regenerate the baseline:

```
cargo run --release -p fono-bench --features equivalence,whisper-local -- \
  equivalence --stt local --model tiny.en \
  --output docs/bench/baseline-comfortable-tiny-en.json \
  --baseline --no-legend
```

Inspect the diff carefully. Verdict changes mean **either** a real
quality regression in the harness or a fixture / threshold tweak
landing in the same PR. Both deserve a callout in the PR description.

### Local assistant tool-use benchmarks

`fono-bench assistant-tool-use` measures whether assistant models can safely map
voice requests to a simulated Home Assistant light-control tool. The benchmark is
for model selection only: it uses a fake inventory and fake tool results, never a
real Home Assistant endpoint.

Local models are highly sensitive to prompt size. Large OpenAI-style tool schemas
and long inventories can dominate prompt-evaluation time, making tool use appear
10–20× slower than short factual answers even when reasoning is disabled. Keep
local tool schemas short, constrain the tool surface to allow-listed actions, and
record first-turn tool-selection latency separately from post-tool confirmation
latency.

---

### Flapping fixtures

If a per-PR run of `tiny.en` produces a verdict that differs from the
committed baseline because of beam-search non-determinism in
whisper.cpp (Risk 2 of `plans/2026-04-28-wave-2-close-out-v1.md`), the
mitigation is to **demote the offending fixture's `accuracy_threshold`
to `1.0`** in `tests/fixtures/equivalence/manifest.toml` (informational-
only, same as `en-single-sentence` and `zh-luxun-kuangren` are handled
today). Document the demotion in the commit message and append a
sentence to the bottom of this file. Do **not** disable the gate.

### When to update the baseline

Update the committed baseline in lockstep with any of:

- A new fixture added to the manifest.
- A threshold tightening / loosening.
- A whisper.cpp upgrade that legitimately changes verdicts.
- A `pinned_params` change (boundary knob defaults, chunk sizes).

The baseline is **append-only** in the sense that bumping it is a
review-able event: it should always be its own commit, with the
diff explained in the message body.

### Rationale for `tiny.en` over `small`

`tiny.en` is small (76 MiB), fast (≤30 seconds per full sweep on a
modest CI runner), deterministic on the English fixtures, and exercises
the capability-skip path on the multilingual fixtures. `small`
multilingual would cover all 10 fixtures with real inference but is
4-5× slower per PR; the strategic plan reserves it for a scheduled
nightly job. See `plans/2026-04-28-wave-2-close-out-v1.md` §"CI bench
gate (Thread C)" for the full trade-off.
