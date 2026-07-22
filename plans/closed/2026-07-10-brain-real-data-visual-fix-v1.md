# Brain Visualization — Fix the Broken Thinking / Speaking Scenes (Real-Data Regressions)

## Status: Completed (Tasks 0–5, 7 shipped; Task 6 / RC5 live retest overtaken by the from-scratch Glas Cortex rewrite)

Status: executed 2026-07-10 (Tasks 0–5, 7 done; Task 6 awaits a live-desktop retest)
Date: 2026-07-10
Follows: `plans/2026-07-07-brain-thinking-speaking-redesign-v1.md`

## Objective

The live overlay looks nothing like the design gallery. Three user screenshots
(`/root/a.png`, `/root/b.png`, `/root/c.png`) show:

1. **Thinking** — a completely black panel; the only lit element is a small
   segmented column at the far right edge.
2. **Speaking** — a "ruled notebook paper" look: uniform thin horizontal cyan
   lines with zero texture, a lone cursor line, and the whole trace
   hard-stopping at ~76 % of the panel width, leaving the right quarter empty.

Meanwhile the offline gallery (`cargo run --release -p fono-overlay --features
backend-x11 --example cortex_gallery`) renders exactly what the design intends:
chunky, textured heat-grid cells. The gap is **real data vs synthetic data**:
the gallery feeds 40 wobbly keyframes; real replies feed hundreds of
near-constant-norm keyframes.

## Where the code lives right now (important)

- The thinking/speaking redesign (`draw_thinking_prefill` /
  `draw_thinking_decode`, +313/−69 in `crates/fono-overlay/src/cortex.rs`) is
  **parked in `stash@{0}`** ("WIP on main: b52e31a"); the working tree is clean
  at `b52e31a`. The broken screenshots were produced by a build of that WIP.
- A throwaway repro harness (`crates/fono-overlay/examples/cortex_repro.rs`)
  reproduced all closed root causes below offline; it was deleted with the
  stash cleanup. Its renders are still in `/tmp/cortex_repro/` and the
  intended-look renders in `/tmp/cortex_gallery/`. Task 1 recreates its
  scenarios permanently inside the gallery.
- First execution step: `git stash pop` and continue on the WIP — do not
  reimplement from scratch; the WIP's scene structure is sound, its data
  handling is not.

## Root causes (verified by offline reproduction on the WIP, not assumed)

- **RC1 — Contrast collapse from running-peak normalisation.**
  `CortexState::frame_act` normalises each layer by its running max
  (`layer_peak`). Real transformer per-layer L2 norms are nearly constant
  across tokens, so every cell normalises to ≈ 1.0 → the decode/replay scenes
  paint a flat saturated slab with no per-token/per-layer texture (repro
  `r1/r2/r4` vs the textured gallery scenes). The converse also holds: one
  outlier frame (e.g. the BOS attention-sink token, whose norms can be 10–100×
  the rest) inflates `layer_peak` and crushes every later frame toward 0 —
  which is what makes the real speaking trace read as dark cells whose row
  *gaps* become bright ruled lines over a bright desktop.
- **RC2 — Sub-pixel replay columns merge and alias.** In replay mode
  `col_w = (grid_w / frames.len()).max(1.25)`; a few-hundred-frame reply gives
  1–4 px columns whose `fill_cell` floor/ceil rasterisation merges them into a
  continuous slab, with periodic double-coverage seams that read as spurious
  bright vertical bands (visible in repro `r2/r4` and in `b/c.png`). The
  per-token column grammar is unreadable at real reply lengths.
- **RC3 — Live decode starts from a black panel.** The instant the first
  keyframe lands, the prefill field is retired (`decode_latched`) and the
  chart-recorder view has exactly one column at the right edge — the other
  ~95 % of the strip goes black (repro `r3` reproduces `a.png` exactly).
  Combined with RC1's outlier-crush, later columns can stay under the
  `energy < 0.055` skip, so Thinking can look dead for a whole reply.
- **RC4 — Palette/alpha tuned only against a dark background.** The panel is
  translucent (`0xCC` bg). Low-`t` heat-ramp fills are *darker* than a bright
  desktop showing through, so the scene's polarity inverts (dark cells +
  bright gaps = ruled lines). The gallery composites over dark grey only and
  never catches this.
- **RC5 — (open) trace hard-stops at ~76 % of the panel width.** In `b/c.png`
  both the frame columns and the entropy ribbon stop at the same x while the
  panel continues. The WIP code cannot mathematically produce this (replay
  columns always span `grid_w`; verified in the repro at 810×96). Prime
  suspects: the running daemon binary was stale relative to the WIP tree, or a
  geometry/scale path difference on the live backend. Verify live before
  assuming a code bug.

## Implementation Plan

- [x] Task 0. **Restore the WIP**: `git stash pop` (stash@{0}, cortex.rs only);
      rebuild and confirm the gallery still matches `/tmp/cortex_gallery/`.
- [x] Task 1. **Make the offline harness honest.** Add permanent real-shaped
      scenes to `cortex_gallery.rs`: (a) long reply (≥ 200 frames / ≥ 800
      tokens), (b) realistic near-constant layer norms, (c) an outlier
      (BOS-sink) first frame with 20× norms, (d) every scene rendered over BOTH
      a dark and a bright background, (e) a fractional-scale render (e.g.
      1.25×) at the real strip geometry. Acceptance: the current WIP visibly
      reproduces the black-thinking and flat/ruled-speaking symptoms in these
      scenes (baseline images for the fixes below).
- [x] Task 2. **Robust per-layer normalisation (RC1).** Replace the
      running-max scale with an outlier-resistant window: per layer, track a
      running low/high band (e.g. EMA of mean ± k·σ, or windowed p10/p90 over
      recent frames) and map `act = clamp((norm − lo)/(hi − lo))`; exclude or
      winsorise the first-token frame when updating the scale. Add a mild
      display-side contrast stretch so low-variance real data still spans the
      ramp. Unit tests: an outlier first frame must not crush later frames'
      `frame_act` dynamic range; near-constant norms must yield mid-ramp
      variation, not uniform ≈ 1.0.
- [x] Task 3. **Integer column binning for the decode trace (RC2).** Both live
      and replay modes draw at most as many trace columns as are legible:
      derive an integer column width + ≥ 1 px gap from the panel (clamp ~28–96
      columns, Lattice-style pixel alignment) and bin multiple keyframes per
      column (max-aggregate per layer band). Cursor maps playback fraction →
      binned column. Acceptance: no sub-pixel columns, no moiré seams, visible
      column gaps at 600-frame replies.
- [x] Task 4. **Never-black Thinking decode (RC3).** While the live trace has
      fewer columns than fill the strip, keep the prefill resting field (or an
      equivalent dim ambient floor) under it instead of retiring it at latch —
      or stretch the available columns across the full strip. Acceptance
      (gallery + unit test): with exactly 1 keyframe, a meaningful fraction
      (≥ ~30 %) of trace cells sit above the true-OFF threshold — the panel
      never reads as dead.
- [x] Task 5. **Background-robust rendering (RC4).** Give the trace area an
      opaque-enough dark stage backing before drawing cells (same trick as the
      status-label scrim), so lit cells always read as light-emitting on any
      desktop and row gaps never read brighter than cells. Re-tune fill alphas
      against the new bright-background gallery scenes. Acceptance: bright-bg
      renders keep the same visual hierarchy as dark-bg renders.
- [ ] Task 6. **Resolve RC5 live.** (Open — needs the live desktop.) The
      working tree is rebuilt; retest a real local-LLM reply on the real
      desktop. If the ~76 % hard stop persists with the fresh binary, add
      temporary debug logging of `(panel_w, grid_w, cols, col_w,
      frames.len, scale)` in the decode draw path, capture one occurrence,
      fix the geometry bug, and remove the logging.
- [x] Task 7. **Self-review + gate.** Re-render the full gallery and compare
      against the `a/b/c.png` failure modes; run the pre-commit gate
      (`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --
      -D warnings`, `cargo test --workspace --tests --lib`); update
      `docs/status.md`; commit the redesign + fixes together (user-friendly
      message, DCO sign-off; no push without explicit instruction).

## Verification Criteria

- Gallery scenes with real-shaped data (long reply, constant norms, BOS
  outlier) show textured, structured grids in Thinking and Speaking — no flat
  slabs, no ruled lines, no black panels.
- Thinking is visibly alive from prefill through the last token: prefill wave →
  snap → accumulating textured columns, with no all-black interval.
- Speaking replay spans the full waveform area; the cursor is clearly
  distinguishable from any column structure; the entropy ribbon aligns with the
  binned columns.
- Bright-background renders preserve the same read as dark-background renders.
- All unit tests pass; live overlay confirmed on the real desktop for a real
  local-LLM assistant reply (the `a/b/c.png` scenario).

## Risks / Notes

- `stash@{0}` holds only `cortex.rs`; the earlier session also touched
  `backend.rs` / `fono-tray` / `fono-inject` — after popping, re-check the
  build for anything those companion changes provided.
- Changing normalisation alters every phase that consumes `frame_act`
  (thinking decode, speaking replay, HUD energy); gallery diffing before/after
  is the guard.
- RC5 may be a non-bug (stale binary). Verify before spending code effort.
- No new dependencies anywhere in this plan (binary-size budget unaffected).
