# Bench commit, README update, and calibration page publish

## Objective

Commit all pending benchmark work, clean up stale paths, update both READMEs with the
wizard auto-selection story, and publish `calibration/index.html` to `fono.page` via the
`site` branch so it can be linked from the project README.

---

## Implementation Plan

### Part A — Stage and commit the directory simplification

The user ran a plain `mv summary/matrix.json .` inside `calibration/`. Git sees this as a
delete + untracked add. Stage everything together so Git can detect the rename:

- [ ] A1. From `docs/bench/`:
  ```sh
  git add -A calibration/
  ```
  Confirm `git status` shows `renamed: calibration/summary/matrix.json → calibration/matrix.json`
  (or `deleted` + `new file` if Git misses the rename — both are fine to commit).

- [ ] A2. Commit:
  ```sh
  git commit -s -m "bench: move calibration/summary/matrix.json to calibration root; rename decision page to index.html"
  ```

### Part B — Remove stale snapshot directories

`2026-05-18-baseline/` and `2026-05-19-perf-pass/` hold pre-optimisation fp16-only data
superseded by the full `calibration/` sweep. The earlier `git rm -r` failed (exit 1) because
tracked files inside have local modifications — use `--force`:

- [ ] B1. From `docs/bench/`:
  ```sh
  git rm -rf 2026-05-18-baseline 2026-05-19-perf-pass
  ```

- [ ] B2. Commit:
  ```sh
  git commit -s -m "bench: remove stale 2026-05-18 baseline and 2026-05-19 perf-pass snapshots

  Both directories held pre-set_audio_ctx() fp16-only data collected before the
  May-19 thread and audio-context optimisations. The calibration/ sweep (210-cell,
  full quant ladder, all 5 hosts) is the canonical source; these two snapshots are
  now noise rather than signal."
  ```

### Part C — Commit the calibration HTML visual fixes

The session-2026-05-25 edits to `calibration/index.html` (piecewise RTF scale relative
to each chart's max; capability-ladder Y axes respect "Hide RTF < 1"):

- [ ] C1.
  ```sh
  git add calibration/index.html
  git commit -s -m "calibration: piecewise RTF scale relative to chart max; capability ladder respects Hide RTF < 1

  * RTF_TOP is now a per-chart variable (was a global const 200). Each chart
    calls updateRtfTop() with its own data; axis ceiling snaps to the smallest
    nice value (15/20/30/50/75/100/150/200) with 10% headroom.
  * Axis title updates dynamically: '0-10x linear, 10-Nx log'.
  * Capability-ladder charts (cap-gpu, cap-vnni, cap-cores) set min:1 on their
    log Y axes when 'Hide RTF < 1' is active.
  * Fixed cap-vnni Y-axis title from 'CPU q8_0 batch RTF' to
    'CPU q8_0 RTF / predicted' (values are normalised ratios, not raw RTF)."
  ```

### Part D — Rewrite `docs/bench/README.md`

Replace the current CI-gate-only lead with a brief orientation, then keep the existing
CI gate content as a `##` subsection.

- [ ] D1. Edit `docs/bench/README.md` to have this structure:

  ```markdown
  # Fono benchmark suite

  Two distinct uses share this directory:

  1. **Wizard calibration data** — the 210-cell (host × backend × model × quant)
     matrix that drives the first-run wizard's model-selection algorithm.
     Interactive decision page: [`calibration/index.html`](calibration/index.html)
     · live at **https://fono.page/bench/calibration.html**
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
  Then keep **all existing content** from the old `README.md` starting at "This directory
  holds the deterministic baseline…" verbatim, demoted one heading level.

- [ ] D2. Commit:
  ```sh
  git commit -s -m "bench: restructure README — lead with calibration goal, keep CI gate section"
  ```

### Part E — Update the root project `README.md`

- [ ] E1. In `README.md` (main fono repo root), inside the `## What Fono does` bullet
  list, add after the "Local or cloud speech-to-text" bullet:

  ```markdown
  - **Automatic model selection.** The first-run wizard probes your CPU and GPU, then
    picks the heaviest local Whisper model that runs comfortably in real time on your
    hardware — no manual tuning. The selection algorithm was validated across five
    machines; see the [model calibration data][bench-page] for the full decision matrix.
  ```

- [ ] E2. At the end of `README.md`, before `## License`, add the reference link:

  ```markdown
  [bench-page]: https://fono.page/bench/calibration.html
  ```

- [ ] E3. Commit in the **main fono repo**:
  ```sh
  git commit -s -m "readme: mention first-run wizard auto-selects best local model; link calibration page"
  ```

### Part F — Publish `calibration/index.html` to the `site` branch

`fono.page` is served from the `site` branch. The file is a self-contained single-page
app (all JS inline; CDN-only external scripts). Target path: `bench/calibration.html`.

- [ ] F1. From the main fono repo root, switch to (or create a worktree for) the `site`
  branch without disturbing the `main` working tree:
  ```sh
  git worktree add /tmp/fono-site site
  ```

- [ ] F2. Copy the file:
  ```sh
  mkdir -p /tmp/fono-site/bench
  cp docs/bench/calibration/index.html /tmp/fono-site/bench/calibration.html
  ```

- [ ] F3. Stage and commit in the site worktree:
  ```sh
  cd /tmp/fono-site
  git add bench/calibration.html
  git commit -s -m "site: add model calibration decision page at bench/calibration.html"
  ```

- [ ] F4. Push:
  ```sh
  git push origin site
  ```

- [ ] F5. Verify: `curl -sI https://fono.page/bench/calibration.html | head -3` should
  return `HTTP/2 200` and `content-type: text/html`.

- [ ] F6. Clean up the worktree:
  ```sh
  cd -
  git worktree remove /tmp/fono-site
  ```

---

## Verification Criteria

- `git ls-files docs/bench/2026-05-18-baseline docs/bench/2026-05-19-perf-pass` returns empty.
- `git ls-files docs/bench/calibration/` shows `index.html` at `calibration/index.html` and `matrix.json` at `calibration/matrix.json` (not under `summary/`).
- Root `README.md` contains the auto-selection bullet and `[bench-page]` link.
- `https://fono.page/bench/calibration.html` returns HTTP 200; all five chart sections render; toggling "Piecewise RTF" shows per-chart axis ceilings; "Hide RTF < 1" pins capability-ladder Y axes at 1.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace --tests --lib` all pass (no Rust changes in any of these commits).

## Potential Risks and Mitigations

1. **`git rm -rf` on modified tracked files (Part B)**
   Mitigation: preview with `git rm -r --dry-run 2026-05-18-baseline 2026-05-19-perf-pass` first to see exactly which files would be removed before committing.

2. **`docs/bench/` is a git submodule of the main repo**
   If confirmed, commits made inside `docs/bench/` do not update the parent repo's submodule pointer automatically. After all bench-repo commits, run `git add docs/bench && git commit -s -m "bench: bump submodule"` from the main repo root.

3. **GitHub Pages build delay**
   After pushing the `site` branch, GitHub Pages may take 30–90 seconds to rebuild. If the verify step in F5 returns 404 immediately, wait and retry.

4. **CDN availability**
   `calibration/index.html` loads Chart.js and the annotation plugin from `cdn.jsdelivr.net`. These are standard CDN assets that fono.page visitors will fetch live. No action needed unless the page is expected to work offline.
