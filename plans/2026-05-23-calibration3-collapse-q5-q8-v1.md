# calibration3.html — Collapse q5_0/q5_1 → q5 and q8_0 → q8 in display

## Objective

Reduce visual noise in `docs/bench/calibration/summary/calibration3.html` by
treating `q5_0` and `q5_1` as a single display bucket `q5` and renaming
`q8_0` to `q8` in all user‑facing surfaces (legends, axes, filter dropdown,
coverage column headers, detail table, per‑host sweep panels, findings
prose). Underlying data files and on‑disk model names are unchanged; the
exact `q5_0` / `q5_1` / `q8_0` tag is preserved in cell records and only
projected to the short form at render time.

Rationale: `q5_0` and `q5_1` differ only in block metadata (scale vs.
scale+min), share the same SIMD/VNNI kernel cost, and never co‑occur for a
given family in our matrix (whisper.cpp ships `q5_1` for tiny/base/small
and `q5_0` for `large-v3-turbo`). Showing them as separate columns/colors
creates phantom empty slots and implies a precision/perf trade‑off that
does not exist for our purposes. `q8_0` is the only 8‑bit variant
whisper.cpp ships, so the `_0` suffix is purely cosmetic noise.

## Implementation Plan

- [ ] Task 1. Add a pure helper `display_quant(quant: str) -> str` in
      `scripts/bench-decision-page3.py` near the existing `split_quant`
      (line 52) that maps `q5_0` and `q5_1` → `"q5"`, `q8_0` → `"q8"`,
      and leaves `"fp16"` untouched. Keep `split_quant` itself returning
      the precise token so `model_name(...)` (line 250) and
      `APPROX_SIZE_MIB` (line 26) keep resolving real model filenames.
      Rationale: a single chokepoint avoids drift between Python and
      embedded JS branches.

- [ ] Task 2. In the cell‑decoration loop (around lines 162–186), add a
      `"quant_display"` field alongside the existing `"quantization"`
      field on every emitted cell. Do not change `"quantization"`. This
      makes the projected value available to the JS layer through
      `RAW_CELLS` without re‑deriving it client‑side.

- [ ] Task 3. Mirror the helper in the embedded JS: add a top‑level
      `function quantDisplay(q)` near `QUANT_ORDER` (line 500) so the
      same mapping is available to JS code paths that work from
      `c.quantization` directly. Prefer reading `c.quant_display` when
      present and falling back to `quantDisplay(c.quantization)`.

- [ ] Task 4. Update the JS constants at lines 500–501:
      `QUANT_ORDER` becomes `['fp16','q8','q5']`; `QUANT_CLR` keys
      become `fp16`, `q8`, `q5` (drop the `q5_0` teal entry, keep the
      q8 amber and the q5 blue). Pick one stable color for `q5` —
      reusing the existing `q5_1` blue (`#58a6ff`) is the least
      disruptive choice.

- [ ] Task 5. Update the CSS rule at line 357 to define
      `.quant-fp16`, `.quant-q8`, `.quant-q5` classes (drop
      `.quant-q8_0`, `.quant-q5_1`, `.quant-q5_0`). Update the detail
      table cell render at line 966 to emit `quant-${quantDisplay(q)}`.

- [ ] Task 6. Replace the quant filter `<select>` options at line 398
      with `fp16`, `q8`, `q5`. Update `filteredCells()` at line 566 so
      the filter compares against the displayed bucket
      (`quantDisplay(c.quantization) === FILTER_STATE.quant`) rather
      than the raw token. Filter‑state init/serialize/restore at lines
      534, 585, 596, 626 keeps the same field name `quant` but its
      value domain shrinks to the three‑element set — no schema
      migration needed because the URL hash is ephemeral.

- [ ] Task 7. Per‑host sweep panel (`QUANTS_FOR_FAM` at lines 244–245
      Python and 736–738 JS): unify all families to
      `['fp16','q8','q5']`. The selectBest call at line 743 must still
      look up cells by the raw token, so resolve via two candidates:
      for `q === 'q5'` search both `q5_0` and `q5_1` and take whichever
      exists for that host/family; for `q === 'q8'` search `q8_0`.
      Document this fan‑out with an inline comment.

- [ ] Task 8. Quant‑speedup chart (lines 790–842): the `groups`
      accumulator at line 793 should key its quant slots by the
      displayed bucket. Because q5_0 and q5_1 never coexist for one
      `(host, build, model_base)`, the merge is unambiguous and
      lossless — assert this in a comment and silently last‑write‑wins
      if the assertion is ever violated. Datasets array at line 813
      becomes `['q8','q5']`.

- [ ] Task 9. CPU‑vs‑Vulkan paired chart (lines 847–901): label
      construction at line 865 should use `quantDisplay(g.quant)` so
      x‑axis ticks read `…/q5` instead of `…/q5_1`. No grouping change
      needed because the pairing key already includes the raw quant
      and the two backends use matching tokens.

- [ ] Task 10. Coverage matrix (lines 904–940): collapse the column
      generator at lines 907–913 so every family iterates
      `['fp16','q8','q5']`. The cell lookup at lines 929–933 must
      match against either underlying token when the column is `q5`
      (search both `q5_0` and `q5_1`) or `q8` (match `q8_0`). Tooltip
      text at line 939 should still surface the precise underlying
      token so debugging by hovering remains possible.

- [ ] Task 11. Detail‑table column header (line 466) stays "Quant" but
      the cell value at line 966 renders the displayed bucket; add the
      raw token to the cell's `title` attribute as a tooltip so the
      precise `_0`/`_1` distinction is one hover away.

- [ ] Task 12. Findings prose at lines 424–425, 433–438, 451 and the
      auto‑generated bullet at line 641 should use `q5` / `q8`
      everywhere. Specifically the chart caption at line 424 should
      read "fp16 / q8 / q5" and drop the parenthetical "(q5_0 only for
      large-v3-turbo)" sentence at lines 424–425 — the whole point of
      this change is that the reader no longer needs to know that.

- [ ] Task 13. Regenerate `docs/bench/calibration/summary/calibration3.html`
      by running the script with the existing input matrix and
      inventory directory (paths documented in the existing plan
      `plans/2026-05-22-calibration3-decision-page-v1.md`). Verify the
      file diff is confined to display strings and chart config — no
      data values should move.

- [ ] Task 14. Sanity‑check the regenerated page in a browser:
      filter dropdown lists three values; legends on quant‑speedup
      and per‑host sweep panels show three colors; coverage matrix
      has no empty `q5_1` column for turbo rows and no empty `q5_0`
      column for non‑turbo rows; detail‑table tooltip on a q5 cell
      reveals the underlying `q5_0` or `q5_1` token; URL hash filter
      `#quant=q5` round‑trips correctly.

## Verification Criteria

- The string `q5_0` and `q5_1` appears nowhere in the rendered HTML
  outside (a) `<title>` tooltip attributes that intentionally surface
  provenance and (b) the raw cell records embedded in the
  `RAW_CELLS` JS literal. Grep the generated file to confirm.
- `q8_0` likewise appears only inside `RAW_CELLS` tooltips, never in
  legends, axis labels, filter options, or column headers.
- The quant filter `<select>` contains exactly four `<option>`
  elements: `All`, `fp16`, `q8`, `q5`.
- Quant‑speedup chart renders exactly two non‑fp16 bar series.
- Coverage matrix has 3 quant columns per (build × family × language)
  group instead of the previous 3 (the count is unchanged but the
  per‑family quant set is uniform — no family‑specific column swap).
- Per‑host sweep panel for `large-v3-turbo` shows a `q5` bar populated
  from the underlying `q5_0` data; a non‑turbo family shows a `q5` bar
  populated from `q5_1` data; both use the same blue color.
- `scripts/bench-decision-page3.py` still passes `python -m py_compile`
  and produces a byte‑stable HTML when run twice on the same inputs.

## Potential Risks and Mitigations

1. **Silent data merge if `q5_0` and `q5_1` ever coexist for the same
   (host, build, family).** Today they don't, but a future model
   release could add both. Mitigation: in Task 8 add an explicit
   assertion / console.warn when the bucket slot is already populated,
   so a future regression is surfaced loudly instead of silently
   last‑write‑wins.

2. **URL hash filter values from older bookmarks
   (`#quant=q5_1`) become orphaned and silently match nothing.**
   Mitigation: in `filteredCells()` (line 566) normalize the
   incoming `FILTER_STATE.quant` through `quantDisplay()` before
   comparison so legacy URLs gracefully upgrade. Document this in
   a one‑line comment.

3. **`APPROX_SIZE_MIB` lookup regression** if anyone refactors
   `model_name(...)` to use the displayed bucket. Mitigation: keep
   `split_quant` returning raw tokens and add a code comment on
   `display_quant` warning that it is display‑only and must never
   be used to construct on‑disk model filenames.

4. **Chart color collision** if the chosen q5 color (currently
   `#58a6ff`) clashes with the CPU‑vs‑Vulkan paired chart's CPU
   color (also `#58a6ff` at line 877). Mitigation: the two charts
   live in different panels and use different legends, so visual
   collision is acceptable; if reviewers object, swap q5 to the
   former q5_0 teal (`#00c3ad`) which is currently unused after
   collapse.

## Alternative Approaches

1. **Collapse in the matrix‑builder upstream** (rewrite
   `model_base`/`quantization` on disk to `q5` / `q8` before
   `bench-decision-page3.py` ever sees the data). Trade‑off: loses
   the `_0` / `_1` provenance permanently and forces a re‑run of
   every benchmark to regenerate normalized inputs. Rejected — the
   display projection is reversible and cheaper.

2. **Keep four buckets but force a shared color for `q5_0` and
   `q5_1`** (still two columns, one color). Trade‑off: removes the
   color confusion but keeps the phantom empty columns the user
   specifically flagged. Rejected — does not address the core
   readability complaint.

3. **Hide `q5_*` from non‑turbo families and `q8_0` everywhere
   else by default**, gated behind a "show all quants" toggle.
   Trade‑off: reduces the default view to the most actionable
   subset but adds UI complexity (toggle state, default choice
   debate). Worth revisiting later if even the three‑bucket view
   feels cluttered — orthogonal to this change.
