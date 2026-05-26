# calibration3.html — Group multi + .en variants within each family slot

## Objective

In the **Speed Sweep — batch RTF by model variant, faceted per host**
chart of `docs/bench/calibration/summary/calibration3.html`, collapse the
current 7‑slot x‑axis
(`tiny`, `tiny.en`, `base`, `base.en`, `small`, `small.en`, `turbo`)
into a 4‑slot x‑axis (`tiny`, `base`, `small`, `turbo`) where every
family slot occupies the same horizontal space.

Within each non‑turbo slot, render the `.en` variant of each quant as
a paired bar in the same colour at reduced opacity + dashed border, so
the multi/.en pair reads as a single colour group with a subtle
variant marker. Turbo has no `.en`; emit `null` for its `.en` data and
turn on Chart.js' `skipNull` so the remaining 3 multi bars expand to
fill turbo's slot — preserving equal slot widths across all families
while visually communicating "turbo has only one language build".

Rationale: the user observed that `.en` and multilingual variants of
the same family perform nearly identically, so the side‑by‑side
columns waste chart real‑estate and inflate visual complexity. The
performance‑relevant axis the reader cares about is **family ×
quant**, not **family × language × quant**; language belongs as a
secondary, de‑emphasised visual layer.

## Implementation Plan

- [ ] Task 1. In `scripts/bench-decision-page3.py:760-792` (the speed
      sweep dataset builder) replace the existing
      `variants`/`datasets` block with a family‑first build that emits
      one x‑axis label per family and up to six datasets per chart
      (three quants × two languages). The replacement is the
      following exact block:

      ```js
      // X-axis = one slot per family (tiny / base / small / turbo).
      // Within each slot we render up to 6 bars: 3 quants × 2 languages
      // (multi + .en). The .en variant of each quant uses the same hue
      // at lower opacity with a dashed border so the multi/.en pair
      // reads as a single colour group with a subtle variant marker.
      // Turbo has no .en, so its .en values are null and Chart.js'
      // `skipNull` redistributes the remaining 3 multi bars to fill
      // the slot — every family slot thus occupies the same horizontal
      // space regardless of whether it has an English-only build.
      const families = FAMILY_ORDER.filter(fam =>
        hostCells.some(c => c.model_family === fam));
      if (!families.length) return;

      const datasets = [];
      QUANT_ORDER.forEach(q => {
        ['multi', 'en'].forEach(lang => {
          const data = families.map(fam => {
            if (lang === 'en' && fam === 'turbo') return null;
            const c = selectBest(
              hostCells,
              x => x.model_family===fam && x.language===lang &&
                   (x.quant_display || quantDisplay(x.quantization)) === q,
              `sweep ${host}/${fam}/${lang}/${q}`);
            return c ? c.batch_rtf_median : null;
          });
          if (data.every(v => v == null)) return;
          datasets.push({
            label: lang === 'en' ? `${q} .en` : q,
            data,
            backgroundColor: QUANT_CLR[q] + (lang === 'en' ? '55' : 'cc'),
            borderColor:     QUANT_CLR[q],
            borderWidth: 1,
            borderDash: lang === 'en' ? [3, 3] : undefined,
            skipNull: true,
            _quant: q, _lang: lang,
          });
        });
      });
      if (!datasets.length) return;
      ```

      Rationale: family is now the x‑category, and dataset count
      auto‑adapts to data availability (a host with only multi data
      still works — `.en` datasets are silently dropped at the
      `every(v => v == null)` guard).

- [ ] Task 2. In `scripts/bench-decision-page3.py:801-808` (the Chart
      constructor for the speed sweep panel) update the `data.labels`,
      `data.datasets`, legend filter, and tooltip callback to match
      the new dataset shape. Replace the existing block from
      `data: { labels: variants.map(...) }` through the end of the
      `tooltip:` config with:

      ```js
      data: { labels: families, datasets },
      options: {
        responsive: true,
        plugins: {
          legend: {
            position: 'bottom',
            // Hide the per-quant .en duplicates from the legend (same
            // colour as the multi entry); they're disambiguated
            // visually by opacity/dash and by the tooltip suffix.
            labels: {
              color: '#8b949e',
              boxWidth: 12,
              filter: (item, data) => {
                const ds = data.datasets[item.datasetIndex];
                return !ds._lang || ds._lang === 'multi';
              },
            },
          },
          tooltip: {
            callbacks: {
              label: i => {
                const ds = i.dataset;
                const lang = ds._lang === 'en'    ? ' (.en)'
                           : ds._lang === 'multi' ? ' (multi)'
                           : '';
                return `${ds._quant || ds.label}${lang}: ${fmt2(i.raw)}× batch RTF`;
              },
            },
          },
      ```

      Rationale: legend stays a clean 3‑colour key (fp16/q8/q5);
      hover tooltip still resolves the multi/.en distinction
      precisely so the "subtle" visual marker isn't ambiguous.

- [ ] Task 3. Update the speed‑sweep section caption at
      `scripts/bench-decision-page3.py:439-447` so the reader knows
      what the paired bars mean. Replace it with:

      ```html
      <p class="desc">
        One panel per host. X-axis is one slot per family
        (<code>tiny</code> / <code>base</code> / <code>small</code> /
        <code>turbo</code>); each quant colour shows the
        <strong>multilingual</strong> and <strong>.en</strong> build
        as two adjacent bars (the .en bar is dashed and at lower
        opacity). <code>turbo</code> ships no .en build, so its 3
        bars fill the slot. <code>q5</code> resolves to the raw
        <code>q5_1</code> for tiny/base/small and <code>q5_0</code>
        for <code>large-v3-turbo</code> (whisper.cpp's publishing
        artefact, not a meaningful precision split). Y axis is log;
        horizontal lines mark batch RTF = 1.0 and 2.0.
      </p>
      ```

      Rationale: every visual encoding in the chart should be
      explained in the caption — opacity, dash, slot equivalence,
      and the q5 fan‑out.

- [ ] Task 4. Regenerate the page by running
      `python3 scripts/bench-decision-page3.py` from the repo root.
      Expected output ends with `Wrote
      docs/bench/calibration/summary/calibration3.html  (~168 KiB)`.

- [ ] Task 5. Open the regenerated page in a browser and visually
      confirm: (a) the speed sweep chart shows 4 equal‑width x‑axis
      slots labelled `tiny / base / small / turbo`; (b) tiny/base/small
      slots each contain six bars (paired multi/.en per quant); (c)
      turbo slots contain three bars filling the slot; (d) legend
      lists exactly three entries (fp16, q8, q5); (e) hovering a thin
      dashed bar shows `…(.en): X.YY× batch RTF`; (f) hovering a solid
      bar shows `…(multi): X.YY× batch RTF`; (g) all other charts on
      the page (verdict heatmap, quant speedup, CPU vs Vulkan,
      coverage matrix, data table) are unchanged.

- [ ] Task 6. Append a one‑paragraph entry to `docs/status.md` under
      today's date noting that the speed‑sweep faceting was folded
      from family×language slots into family‑only slots with
      multi/.en rendered as opacity‑paired bars, citing
      `scripts/bench-decision-page3.py` and this plan.

## Verification Criteria

- The speed‑sweep chart has exactly 4 categorical x‑axis labels
  per host panel (or fewer, if the host data lacks a family),
  matching `FAMILY_ORDER` intersected with what the host measured.
- For each non‑turbo family, every coloured quant group renders as
  a pair of adjacent bars: one solid (`alpha=cc`, no dash) for
  multilingual, one translucent (`alpha=55`, `borderDash=[3,3]`)
  for `.en`. Bars share a hue per quant.
- For turbo, only the multi bars are present and their width is
  visibly larger than the same quant's multi bar in tiny/base/small
  (Chart.js `skipNull` redistribution).
- Legend at the bottom of each panel renders 3 entries: `fp16`,
  `q8`, `q5`. The `.en` datasets do **not** appear as separate
  legend entries.
- Tooltip on any `.en` bar reads `<quant> (.en): <n>× batch RTF`;
  tooltip on a multi bar reads `<quant> (multi): <n>× batch RTF`.
- Running `python3 -m py_compile scripts/bench-decision-page3.py`
  exits 0; running the script writes the HTML without errors.
- A diff of the generated `calibration3.html` against the previous
  version shows no changes outside the speed‑sweep section's
  JavaScript and the section caption HTML.

## Potential Risks and Mitigations

1. **`skipNull` redistribution differs between Chart.js minor
   versions.** Chart.js 4.4.3 supports per‑dataset `skipNull`
   (introduced in 3.7), but the redistribution algorithm has had
   edge cases with mixed null/non‑null datasets in the past.
   Mitigation: verify the turbo slot visually in Task 5; if turbo
   bars don't expand to fill the slot, fall back to
   `barPercentage: 0.95, categoryPercentage: 0.95` on the chart
   `scales.x` config and accept slightly thinner turbo bars.

2. **The 0x55 alpha on `.en` bars may be too faint on the dark
   theme**, especially for the `fp16` purple. Mitigation: if the
   `.en` bars are illegible against the `#161b22` chart background,
   bump the alpha to `77` or `88`. The dashed border is the
   primary distinguishing feature; opacity is a secondary cue.

3. **Tooltip uses `ds._quant` as a custom dataset field** —
   Chart.js preserves unknown dataset fields through render, but
   if a future Chart.js upgrade strips them, the tooltip would
   fall back to `ds.label` which already encodes `q8 .en` etc.
   Mitigation: the fallback `${ds._quant || ds.label}` is already
   in place.

4. **Legend filter callback receives a `LegendItem`, not a
   dataset directly.** The `item.datasetIndex` indirection is
   correct in Chart.js 4.x but was different in 2.x. Since the
   page pins 4.4.3 via CDN this is fine; document the dependency
   in a JS comment so future toolchain upgrades catch the issue.

## Alternative Approaches

1. **Render `.en` as a hatch pattern instead of opacity+dash**
   (e.g. via the `chartjs-plugin-patternomaly` plugin). Trade‑off:
   stronger visual distinction at any size, but adds a CDN
   dependency and is harder to read on small bars. Rejected for
   now — opacity+dash hits the "subtle" brief better.

2. **Keep family×language slots but visually group each pair
   with a thin connector below the x‑axis tick.** Trade‑off:
   no code restructure, but doesn't actually reclaim chart space
   — the user's core complaint (slot count) goes unaddressed.

3. **Use a stacked diff bar**: render the multi bar full‑height
   and overlay a thin `.en − multi` delta on top. Trade‑off:
   directly shows the "subtle difference" magnitude, but loses
   the absolute `.en` value and requires a non‑obvious mental
   model. Worth revisiting if a future "delta view" panel is
   added but inappropriate here as the user wants both absolute
   values visible.
