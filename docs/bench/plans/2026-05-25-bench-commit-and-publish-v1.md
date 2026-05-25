# Bench commit, README update, and calibration page publish

## Objective

Commit all pending benchmark work, delete the two superseded snapshot directories,
update the bench README and the project README with the wizard auto-selection story,
and publish `calibration.html` to a publicly accessible URL so it can be linked from
the README.

---

## Implementation Plan

### Part A — Clean up the bench repository

- [ ] A1. Force-remove the two stale snapshot directories (the earlier `git rm -r` failed with
  exit 1, meaning at least one tracked file inside has local modifications; use `--force`):
  ```
  git rm -rf 2026-05-18-baseline 2026-05-19-perf-pass
  ```
  Run this from `docs/bench/`. Both directories held pre-optimisation fp16-only data that
  has been superseded by the full quant-ladder `calibration/` sweep; keeping them inflates
  the repo by ~600 tracked JSON files and confuses the canonical data source.

- [ ] A2. Verify the `2026-05-22-ro-bogdan/` directory is **retained** — it is still the
  cross-host Vulkan sweep that feeds the heatmap "section 1" reference and is referenced
  in `docs/status.md`.

- [ ] A3. Commit the deletion in the bench repo:
  ```
  git commit -s -m "bench: remove stale 2026-05-18 baseline and 2026-05-19 perf-pass snapshots

  Both directories held pre-set_audio_ctx() fp16-only data collected before the
  May-19 thread and audio-context optimisations. The calibration/ sweep (210-cell,
  full quant ladder, all 5 hosts) is the canonical source; these two snapshots are
  now noise rather than signal."
  ```

### Part B — Commit the calibration.html visual fixes

- [ ] B1. Stage the updated `calibration/summary/calibration.html`:
  ```
  git add calibration/summary/calibration.html
  ```

- [ ] B2. Commit it:
  ```
  git commit -s -m "calibration: piecewise RTF scale relative to chart max; capability ladder respects Hide RTF < 1

  * RTF_TOP is now a per-chart variable (was a global const 200). Each chart
    calls updateRtfTop() with its own data before building datasets; the axis
    ceiling snaps to the smallest nice value (15/20/30/50/75/100/150/200) with
    10% headroom above the chart's actual maximum. The piecewise title label
    also updates dynamically.
  * Capability-ladder charts (cap-gpu, cap-vnni, cap-cores) now set min:1 on
    their log Y axes when 'Hide RTF < 1' is active, matching the behaviour of
    all other sections.
  * Fixed cap-vnni Y-axis title from 'CPU q8_0 batch RTF' (misleading) to
    'CPU q8_0 RTF / predicted' (accurate — axis shows the normalised ratio)."
  ```

### Part C — Rewrite `docs/bench/README.md`

The current file documents only the CI equivalence gate. It needs a new lead section
that explains the calibration goal and links to the interactive page. Keep the CI gate
section intact below.

- [ ] C1. Replace `docs/bench/README.md` content with the following structure
  (keeping all existing CI gate text from "## Files" onward verbatim):

  ```markdown
  # Fono benchmark suite

  Two distinct uses share this directory:

  1. **Wizard calibration data** — the 210-cell (host × backend × model × quant)
     matrix that drives the first-run wizard's model-selection algorithm. See
     [`calibration/`](calibration/) for raw runs, summary JSON, and the
     interactive decision page.

  2. **CI equivalence gate** — a deterministic per-PR baseline that catches
     regressions in transcript quality. The committed JSON files below are that
     baseline.

  ## Calibration goal and methodology

  The wizard's job is to pick, on first run, the heaviest local Whisper model
  that runs comfortably in real time on the user's CPU or GPU — without a live
  probe and without the user specifying anything. To validate and tune that
  algorithm we benchmarked five representative Linux machines across a full
  (model × quantisation × backend) grid:

  | Metric | Gate | Meaning |
  |--------|------|---------|
  | Batch RTF | ≥ 2.0 = comfortable | audio-seconds processed per wall-second (higher = faster) |
  | Peak RSS | ≤ 1024 MiB | resident memory ceiling enforced by the wizard |
  | Registry WER | ≤ 15 % (Open-ASR-Leaderboard mean) | accuracy gate; the wizard uses published means, not per-fixture worst-case |

  Each cell is three iterations of `fono-bench equivalence` over ten public-domain
  WAV fixtures (≈ 100 s audio, five languages). Medians are taken; cells with
  batch RTF spread > 15 % are flagged. See
  [`calibration/README.md`](calibration/README.md) for the full protocol.

  **Interactive decision page:** [`calibration/summary/calibration.html`](calibration/summary/calibration.html)
  — or at the hosted URL linked from the project README.

  ---
  ```
  Then continue with the existing "# `fono-bench` per-PR equivalence gate" content,
  demoted to a `##` section heading.

- [ ] C2. Commit:
  ```
  git commit -s -m "bench: restructure README — lead with calibration goal, keep CI gate section"
  ```

### Part D — Update the project `README.md`

The project README describes all other features but says nothing about the wizard's
automatic model selection. Add one bullet to the "What Fono does" list and a sentence
to the "First run" section.

- [ ] D1. In `README.md`, inside the `## What Fono does` bullet list, add after the
  "Local or cloud speech-to-text" bullet:
  ```
  - **Automatic model selection.** The first-run wizard probes your CPU and GPU,
    then selects the heaviest local Whisper model that runs comfortably in real
    time on your hardware — no manual tuning required. The algorithm was
    validated across five machines; see the [model calibration data][bench-page]
    for the full decision matrix.
  ```

- [ ] D2. At the bottom of `README.md`, before the `## License` section, add a
  reference-style link definition:
  ```
  [bench-page]: https://fono.page/bench/calibration.html
  ```
  (URL to be confirmed in Part E; update once the page is live.)

- [ ] D3. Commit to the **main fono repo** (not the bench sub-directory):
  ```
  git commit -s -m "readme: mention first-run wizard auto-selects the best local model"
  ```

### Part E — Host `calibration.html` on fono.page

The file is a self-contained single-page app (all JavaScript inline, CDN-only external
dependencies) — it can be dropped into any static host as-is.

- [ ] E1. **Determine the fono.page hosting method.** Two paths:

  - **Path 1 — fono.page is GitHub Pages with a custom domain** (most likely given
    the badge `[![Homepage](…fono.page…)]` and the CI workflows that don't deploy
    anywhere): confirm by checking whether `bogdanr/fono` has a `gh-pages` branch or
    a `docs/` Pages source configured in the repo settings. If so, copy
    `docs/bench/calibration/summary/calibration.html` to the Pages source root as
    `bench/calibration.html` and push — the file will be served at
    `https://fono.page/bench/calibration.html`.

  - **Path 2 — fono.page is hosted independently** (separate server or CDN): copy
    the file to the web root at `bench/calibration.html` via whatever deployment
    mechanism that site uses.

- [ ] E2. Once hosted, verify the page loads at the target URL and all charts render
  (CDN scripts from `cdn.jsdelivr.net` must be reachable).

- [ ] E3. Update the `[bench-page]` link in `README.md` to the confirmed URL, then
  amend or add a follow-up commit:
  ```
  git commit -s -m "readme: link calibration page to live fono.page URL"
  ```

---

## Verification Criteria

- `git log --oneline docs/bench/` shows commits A3, B2, C2 in order; `docs/bench/2026-05-18-baseline/` and `docs/bench/2026-05-19-perf-pass/` no longer appear in `git ls-files docs/bench/`.
- `calibration/summary/calibration.html` — toggling Piecewise RTF on a chart with max ≈ 60× shows axis ceiling at 75×, not 200×; enabling "Hide RTF < 1" on the capability ladder pins all three chart Y-axes at 1.
- Root `README.md` contains the new auto-selection bullet and the `[bench-page]` link.
- `curl -I https://fono.page/bench/calibration.html` returns HTTP 200 and `content-type: text/html`.
- CI (`cargo fmt --check`, `cargo clippy`, `cargo test`) still passes — these commits touch no Rust source.

## Potential Risks and Mitigations

1. **`git rm -rf` on partially-staged files**
   Mitigation: run `git status --short docs/bench/2026-05-18-baseline docs/bench/2026-05-19-perf-pass` first; if any file shows `M` in column 2, the `--force` flag is required and safe here (the data is superseded).

2. **Bench directory is a git submodule of the main repo**
   If `docs/bench/` has its own `.git/`, commits there do not automatically update the parent repo's submodule pointer. After committing inside `docs/bench/`, run `git add docs/bench && git commit -s -m "bench: bump submodule"` from the main fono repo root.

3. **fono.page hosting path unknown**
   Mitigation: inspect repo settings for GitHub Pages configuration before writing the README link; use a placeholder URL in the interim commit and amend once the real URL is confirmed.

4. **CDN availability for the hosted page**
   `calibration.html` loads Chart.js and the annotation plugin from `cdn.jsdelivr.net`. If the target audience is expected to use the page offline or from restricted networks, bundle both scripts inline before publishing. No change needed if `fono.page` is a public site with normal internet access.
