# Bench commit, README update, and calibration page publish — v3

## Objective

Commit all pending benchmark work locally, clean up stale paths, update both READMEs
with the wizard auto-selection story, and place `calibration/index.html` into the `site`
branch so it will be served at `https://fono.page/calibration` —
**no remote push; everything stays local for squash review before going to origin**.

---

## Implementation Plan

### Part A — Stage and commit the directory simplification

The user ran a plain `mv summary/matrix.json .` inside `calibration/`. Git sees this as
a delete + untracked add. Stage everything together so Git can detect the rename:

- [ ] A1. From `docs/bench/`:
  ```sh
  git add -A calibration/
  ```
  Confirm `git status` shows `renamed: calibration/summary/matrix.json → calibration/matrix.json`.

- [ ] A2. Commit:
  ```sh
  git commit -s -m "bench: move matrix.json to calibration root; rename decision page to index.html"
  ```

### Part B — Remove stale snapshot directories

`2026-05-18-baseline/` and `2026-05-19-perf-pass/` hold pre-optimisation fp16-only data
superseded by the full `calibration/` sweep. Use `--force` because tracked files inside
have local modifications (plain `git rm -r` returned exit 1):

- [ ] B1. From `docs/bench/`:
  ```sh
  git rm -rf 2026-05-18-baseline 2026-05-19-perf-pass
  git commit -s -m "bench: remove stale 2026-05-18 baseline and 2026-05-19 perf-pass snapshots

  Both directories held pre-set_audio_ctx() fp16-only data collected before the
  May-19 thread and audio-context optimisations. The calibration/ sweep (210-cell,
  full quant ladder, all 5 hosts) is the canonical source."
  ```

### Part C — Commit the calibration HTML visual fixes

- [ ] C1. From `docs/bench/`:
  ```sh
  git add calibration/index.html
  git commit -s -m "calibration: piecewise RTF scale relative to chart max; capability ladder respects Hide RTF < 1

  * RTF_TOP is now a per-chart variable (was global const 200). Each chart calls
    updateRtfTop() before building datasets; axis ceiling snaps to the smallest
    nice value (15/20/30/50/75/100/150/200) with 10% headroom.
  * Axis title updates dynamically: '0-10x linear, 10-Nx log'.
  * Capability-ladder charts set min:1 on their log Y axes when Hide RTF < 1 is on.
  * Fixed cap-vnni Y axis title: 'CPU q8_0 RTF / predicted' (was 'CPU q8_0 batch RTF')."
  ```

### Part D — Rewrite `docs/bench/README.md`

- [ ] D1. Replace the current CI-gate-only lead. New structure:

  ```markdown
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
  ```
  Then keep all existing content from the old file verbatim, demoted one heading level.

- [ ] D2. Commit:
  ```sh
  git commit -s -m "bench: restructure README — lead with calibration goal, keep CI gate section"
  ```

### Part E — Update the root project `README.md`

- [ ] E1. In `README.md` (main fono repo root), inside `## What Fono does`, add after
  the "Local or cloud speech-to-text" bullet:

  ```markdown
  - **Automatic model selection.** The first-run wizard probes your CPU and GPU, then
    picks the heaviest local Whisper model that runs comfortably in real time on your
    hardware — no manual tuning. The selection algorithm was validated across five
    machines; see the [model calibration data][bench-page] for the full decision matrix.
  ```

- [ ] E2. At the bottom of `README.md`, before `## License`, add:

  ```markdown
  [bench-page]: https://fono.page/calibration
  ```

- [ ] E3. Commit in the **main fono repo**:
  ```sh
  git commit -s -m "readme: mention first-run wizard auto-selects best local model; link calibration page"
  ```

### Part F — Stage `calibration/index.html` into the `site` branch (local only, no push)

The `site` branch serves `fono.page`. The file at path `calibration/index.html` within
that branch will be reachable at `https://fono.page/calibration`. Use a worktree to
avoid disturbing the `main` working tree.

- [ ] F1. From the main fono repo root:
  ```sh
  git worktree add /tmp/fono-site site
  ```

- [ ] F2. Copy the file into place:
  ```sh
  mkdir -p /tmp/fono-site/calibration
  cp docs/bench/calibration/index.html /tmp/fono-site/calibration/index.html
  ```

- [ ] F3. Commit in the site worktree:
  ```sh
  cd /tmp/fono-site
  git add calibration/index.html
  git commit -s -m "site: add model calibration decision page at calibration/index.html

  Interactive decision matrix for Fono's first-run wizard model-selection algorithm.
  Self-contained single-page app; CDN-only external deps (Chart.js,
  chartjs-plugin-annotation). Linked from README as https://fono.page/calibration."
  ```

- [ ] F4. Clean up the worktree — **do not push**:
  ```sh
  cd -
  git worktree remove /tmp/fono-site
  ```

---

## After review: squash and push

Once everything looks correct locally, squash the bench-repo commits into a clean set
and push all branches:

```sh
# Bench repo — interactive rebase to squash A2, B1, C1, D2
git -C docs/bench rebase -i <first-bench-commit-sha>

# Main repo — push main (with E3) and site (with F3)
git push origin main
git push origin site
```

---

## Verification Criteria (all local)

- `git -C docs/bench ls-files 2026-05-18-baseline 2026-05-19-perf-pass` returns empty.
- `git -C docs/bench ls-files calibration/` shows `index.html` at `calibration/index.html`
  and `matrix.json` at `calibration/matrix.json` (not under `summary/`).
- Root `README.md` has the auto-selection bullet and `[bench-page]: https://fono.page/calibration`.
- Opening `calibration/index.html` locally: Piecewise RTF shows per-chart ceilings (not
  all 200×); "Hide RTF < 1" pins the three capability-ladder Y axes at 1.
- `git log --oneline site` shows the new calibration commit at the tip.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace --tests --lib` all pass (no Rust changes in these commits).

## Key memory notes

- `fono.page` is served from the **`site` branch** of the `bogdanr/fono` repo.
- Target URL for the calibration page: **`https://fono.page/calibration`**
  (file lives at `calibration/index.html` in the `site` branch).
- Do not push to remote until the user has reviewed the squashed commits locally.
