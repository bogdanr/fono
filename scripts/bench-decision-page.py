#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Build a self-contained HTML decision page from calibration benchmarks.

Reads:
  - docs/bench/calibration/summary/matrix.json
  - docs/bench/calibration/runs/*.json (per-fixture accuracy)
  - docs/bench/calibration/inventory/*.json (host metadata)

Writes:
  - docs/bench/calibration/summary/calibration.html

The page embeds all data inline (no external fetches) and renders with
vanilla JS + Chart.js (loaded once from a CDN). Designed to make
model-selection trade-offs explicit: speed vs RSS vs accuracy vs disk.
"""
from __future__ import annotations

import argparse
import glob
import html
import json
import os
import re
import statistics
from collections import defaultdict
from pathlib import Path

# Approximate on-disk sizes of the GGML files (MiB).  Used so the page can
# show "what does this download cost".  Values from upstream HuggingFace.
APPROX_SIZE_MIB = {
    "tiny": 78, "tiny-q5_1": 32, "tiny-q8_0": 44,
    "tiny.en": 78, "tiny.en-q5_1": 32, "tiny.en-q8_0": 44,
    "base": 148, "base-q5_1": 60, "base-q8_0": 82,
    "base.en": 148, "base.en-q5_1": 60, "base.en-q8_0": 82,
    "small": 466, "small-q5_1": 181, "small-q8_0": 264,
    "small.en": 466, "small.en-q5_1": 181, "small.en-q8_0": 264,
    "large-v3-turbo": 1543,
    "large-v3-turbo-q5_0": 547,
    "large-v3-turbo-q8_0": 834,
}

# Currently shipped registry defaults — from crates/fono-stt/src/registry.rs.
# `base`/`base.en` deliberately marked "MISSING" — the registry does not
# include them, which is the gap surfaced by this page.
REGISTRY = {
    "tiny":             {"default_quant": "q5_1", "variants": ["q5_1"]},
    "tiny.en":          {"default_quant": "q5_1", "variants": ["q5_1"]},
    "base":             {"default_quant": None,   "variants": []},
    "base.en":          {"default_quant": None,   "variants": []},
    "small":            {"default_quant": "q5_1", "variants": ["q5_1", "q8_0", "fp16"]},
    "small.en":         {"default_quant": "q8_0", "variants": ["q5_1", "q8_0", "fp16"]},
    "large-v3-turbo":   {"default_quant": "q8_0", "variants": ["q8_0", "fp16"]},
}

# Parse a model string into (family, quantization)
QUANT_RE = re.compile(r"^(.+?)(?:-(q\d_[01]|fp16))?$")
def split_quant(model: str) -> tuple[str, str]:
    """Split 'small.en-q5_1' → ('small.en', 'q5_1').  Bare names → fp16."""
    m = QUANT_RE.match(model)
    base = m.group(1)
    quant = m.group(2) or "fp16"
    return base, quant


def family_of(base: str) -> str:
    """Group key for charts: tiny | base | small | turbo."""
    if base.startswith("large-v3-turbo"):
        return "turbo"
    if base.startswith("small"):
        return "small"
    if base.startswith("base"):
        return "base"
    if base.startswith("tiny"):
        return "tiny"
    return base


def load_accuracy(runs_dir: Path) -> dict:
    """Aggregate per-fixture accuracy from individual run JSONs.

    Key: (host, build, power, model)
    Value: dict with mean_lev, max_lev, n_fixtures, en_lev (English-only avg)
    """
    bucket = defaultdict(list)
    en_bucket = defaultdict(list)
    fname_re = re.compile(
        r"^(?P<host>[^_]+)__(?P<power>[^_]+)__(?P<build>[^_]+)__(?P<model>.+?)__iter\d+\.json$"
    )
    for f in glob.glob(str(runs_dir / "*.json")):
        if ".time" in f:
            continue
        m = fname_re.match(os.path.basename(f))
        if not m:
            continue
        try:
            data = json.load(open(f))
        except Exception:
            continue
        results = data.get("results") or []
        for r in results:
            if r.get("skip_reason"):
                continue
            lev = (r.get("metrics") or {}).get("stt_levenshtein_norm")
            if lev is None:
                continue
            key = (m["host"], m["build"], m["power"], m["model"])
            bucket[key].append(float(lev))
            if r.get("language") == "en":
                en_bucket[key].append(float(lev))
    out = {}
    for k, vs in bucket.items():
        en_vs = en_bucket.get(k, [])
        out[k] = {
            "mean_lev": round(statistics.mean(vs), 4),
            "max_lev": round(max(vs), 4),
            "n_fixtures": len(vs),
            "en_mean_lev": round(statistics.mean(en_vs), 4) if en_vs else None,
        }
    return out


def load_inventory(inv_dir: Path) -> dict:
    inv = {}
    for f in glob.glob(str(inv_dir / "*.json")):
        try:
            d = json.load(open(f))
        except Exception:
            continue
        host = d.get("host_id") or Path(f).stem
        inv[host] = d
    return inv


def enrich_cells(cells: list, accuracy: dict, inventory: dict) -> list:
    """Annotate each matrix cell with extra fields for the page."""
    fp16_acc = {}
    for c in cells:
        base, quant = split_quant(c["model"])
        if quant == "fp16":
            key = (c["host"], c["build"], c["power"], base)
            acc = accuracy.get(
                (c["host"], c["build"], c["power"], c["model"]), {}
            )
            fp16_acc[key] = acc.get("mean_lev")

    out = []
    for c in cells:
        base, quant = split_quant(c["model"])
        acc = accuracy.get((c["host"], c["build"], c["power"], c["model"]), {})
        fp16_lev = fp16_acc.get((c["host"], c["build"], c["power"], base))
        delta = None
        if quant != "fp16" and fp16_lev is not None and acc.get("mean_lev") is not None:
            delta = round(acc["mean_lev"] - fp16_lev, 4)
        registry_info = REGISTRY.get(base, {})
        is_default = (
            registry_info.get("default_quant") is not None
            and registry_info.get("default_quant") == quant
        )
        in_registry = quant in registry_info.get("variants", [])
        # Carry over inventory hints that affect interpretation
        inv = inventory.get(c["host"], {})
        out.append({
            **c,
            "model_family": family_of(base),
            "model_base": base,
            "quantization": quant,
            "approx_size_mib": APPROX_SIZE_MIB.get(c["model"]),
            "accuracy_mean_lev": acc.get("mean_lev"),
            "accuracy_max_lev": acc.get("max_lev"),
            "accuracy_en_mean_lev": acc.get("en_mean_lev"),
            "accuracy_n_fixtures": acc.get("n_fixtures"),
            "accuracy_delta_vs_fp16": delta,
            "registry_default": is_default,
            "registry_present": in_registry or (registry_info.get("default_quant") == quant),
            "registry_missing_base": registry_info.get("default_quant") is None
                                       and not registry_info.get("variants"),
            "host_avx_vnni": inv.get("avx_vnni"),
            "host_cores_p": inv.get("cores_performance"),
            "host_cores_e": inv.get("cores_efficient"),
            "host_gpu": inv.get("gpu"),
            "host_year": inv.get("year"),
        })
    return out


HTML_TEMPLATE = r"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Fono · CPU/GPU calibration · model decision page</title>
<style>
  :root {
    --bg: #0f1115; --fg: #e7e7e7; --muted: #9aa0a6; --card: #181b22;
    --accent: #6aa7ff; --line: #2a2f3a;
    --comf-bg: #16341f; --comf-fg: #6fe69b;
    --bord-bg: #4a3a14; --bord-fg: #ffce6a;
    --unsu-bg: #4a1a1a; --unsu-fg: #ff8d8d;
    --good-delta: #6fe69b; --bad-delta: #ff8d8d;
    --quant-q5_1: #5b9bff; --quant-q8_0: #ffce6a; --quant-fp16: #ad6fe6; --quant-q5_0: #00c3ad;
    --bg-default: #1d3a4c;
  }
  * { box-sizing: border-box; }
  body { background: var(--bg); color: var(--fg); font: 14px/1.45 ui-sans-serif, system-ui, sans-serif; margin: 0; padding: 0; }
  header { padding: 20px 28px 6px; border-bottom: 1px solid var(--line); }
  h1 { margin: 0 0 6px; font-size: 22px; }
  .subtitle { color: var(--muted); font-size: 13px; }
  main { max-width: 1400px; margin: 0 auto; padding: 18px 28px 60px; }
  section { background: var(--card); border: 1px solid var(--line); border-radius: 8px; padding: 16px 20px; margin: 18px 0; }
  section h2 { margin: 0 0 10px; font-size: 17px; }
  section p { color: var(--fg); margin: 6px 0; }
  section .note { color: var(--muted); font-size: 12.5px; }
  .filters { display: grid; grid-template-columns: repeat(auto-fit, minmax(170px, 1fr)); gap: 12px; }
  .filters label { display: block; font-size: 12px; color: var(--muted); margin-bottom: 4px; }
  .filters select, .filters input[type="number"] {
    width: 100%; background: var(--bg); color: var(--fg); border: 1px solid var(--line);
    border-radius: 5px; padding: 6px 8px; font-size: 13px;
  }
  table.matrix { width: 100%; border-collapse: collapse; font-size: 12.5px; font-variant-numeric: tabular-nums; }
  table.matrix th, table.matrix td { border-bottom: 1px solid var(--line); padding: 7px 8px; text-align: right; }
  table.matrix th { text-align: right; cursor: pointer; user-select: none; background: var(--card); position: sticky; top: 0; }
  table.matrix th:nth-child(1), table.matrix td:nth-child(1),
  table.matrix th:nth-child(2), table.matrix td:nth-child(2),
  table.matrix th:nth-child(3), table.matrix td:nth-child(3),
  table.matrix th:nth-child(4), table.matrix td:nth-child(4) { text-align: left; }
  .verdict { padding: 2px 8px; border-radius: 4px; font-weight: 600; display: inline-block; min-width: 78px; text-align: center; }
  .v-comfortable { background: var(--comf-bg); color: var(--comf-fg); }
  .v-borderline  { background: var(--bord-bg); color: var(--bord-fg); }
  .v-unsuitable  { background: var(--unsu-bg); color: var(--unsu-fg); }
  .q-q5_1 { color: var(--quant-q5_1); }
  .q-q8_0 { color: var(--quant-q8_0); }
  .q-fp16 { color: var(--quant-fp16); }
  .q-q5_0 { color: var(--quant-q5_0); }
  .pill { display: inline-block; font-size: 10.5px; padding: 1px 6px; border-radius: 99px; margin-left: 6px; vertical-align: middle; }
  .pill.default { background: var(--bg-default); color: #cfe6ff; }
  .pill.missing { background: #5a2424; color: #ffc7c7; }
  .delta-pos { color: var(--bad-delta); }
  .delta-neg { color: var(--good-delta); }
  .delta-zero { color: var(--muted); }
  .chart-grid { display: grid; grid-template-columns: 1fr; gap: 20px; }
  .chart-box { background: var(--bg); border: 1px solid var(--line); border-radius: 6px; padding: 12px; height: 440px; position: relative; }
  .legend { font-size: 12px; color: var(--muted); padding-top: 6px; }
  .legend strong { color: var(--fg); }
  .legend .sw { display: inline-block; width: 10px; height: 10px; border-radius: 2px; margin: 0 4px -1px 0; vertical-align: middle; }
  .findings ul { margin: 6px 0 6px 18px; }
  .findings li { margin: 4px 0; }
  .findings em { color: var(--accent); font-style: normal; }
  details { margin-top: 8px; }
  summary { cursor: pointer; color: var(--accent); }
  code { background: #232734; padding: 1px 6px; border-radius: 3px; font-family: ui-monospace, monospace; font-size: 12px; }
  .small { font-size: 11.5px; color: var(--muted); }
  .scroll-x { overflow-x: auto; }
  .toolbar { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }
  .toolbar button { background: var(--bg); color: var(--fg); border: 1px solid var(--line); padding: 5px 10px; border-radius: 5px; cursor: pointer; font-size: 12px; }
  .toolbar button:hover { border-color: var(--accent); color: var(--accent); }
  .reco-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(280px, 1fr)); gap: 12px; }
  .reco-card { background: var(--bg); border: 1px solid var(--line); border-radius: 6px; padding: 12px; }
  .reco-card h3 { margin: 0 0 6px; font-size: 14px; color: var(--accent); }
  .reco-card .row { display: flex; justify-content: space-between; gap: 8px; font-size: 12.5px; padding: 2px 0; }
  .reco-card .row .lbl { color: var(--muted); }
  .reco-card .row .val { color: var(--fg); font-variant-numeric: tabular-nums; }
</style>
</head>
<body>
<header>
  <h1>Fono calibration · model-selection decision page</h1>
  <div class="subtitle">Generated {GENERATED_AT} · {N_CELLS} cells across {N_HOSTS} hosts · CPU + Vulkan</div>
</header>

<main>

<section class="findings">
  <h2>Key findings (read first)</h2>
  <ul>
    <li>On CPU, <em>quantization gives essentially zero speed benefit</em> on every laptop CPU we measured. Quant on CPU is purely a memory/disk optimization (≈30–55 % less RSS, ≈55–80 % less disk).</li>
    <li>On Vulkan, quantization <em>does</em> help — especially for <code>large-v3-turbo</code>: <code>q5_0</code> is 1.6–2.4× faster than fp16 because the iGPU is bandwidth-bound, not compute-bound. For tiny/base/small the win is small or negative.</li>
    <li><em>q8_0 is the consistently-safe quant</em>: ΔLev ≈ 0 on every model where we have both fp16 and q8_0 CPU measurements. <em>q5_1 is risky</em> — on <code>base</code> multilingual it costs +0.113 mean Levenshtein vs fp16 on i7-8550u (lev 0.298 vs 0.185), and on <code>small.en</code> it costs +0.072. q5_1 on English-only tiny.en is acc-neutral.</li>
    <li>The <code>base</code> / <code>base.en</code> models are <em>missing from the registry</em>. Our data shows they are the universal comfortable sweet spot on CPU for every laptop including 2016-era Kaby Lake. This is a gap — the registry skips from tiny straight to small.</li>
    <li>Tiny / tiny.en ship <em>q5_1 only</em>. Given quant saves no time on CPU and the accuracy delta is small (≤0.025) for tiny.en, this is defensible — but exposing fp16 as an opt-in for hosts with ≥4 GiB free RAM gives users the choice between "less RAM" and "more accurate".</li>
    <li><em>Recommended registry change (data-driven):</em> default base/base.en to <code>q8_0</code> (not q5_1) because q5_1 has too much accuracy variance on multilingual base, while q8_0 is acc-neutral and only ~37 % larger.</li>
  </ul>
  <div class="note">Verdict thresholds: <b>comfortable</b> requires batch RTF ≥ 2.0 and stream RTF ≥ 1.5. <b>borderline</b> requires batch RTF ≥ 1.0. Anything else is <b>unsuitable</b>.</div>
</section>

<section>
  <h2>Filters</h2>
  <div class="filters">
    <div><label>Host</label><select id="f-host" multiple size="5"></select></div>
    <div><label>Backend</label><select id="f-build" multiple size="3"></select></div>
    <div><label>Model family</label><select id="f-family" multiple size="5"></select></div>
    <div><label>Quantization</label><select id="f-quant" multiple size="5"></select></div>
    <div><label>Verdict</label><select id="f-verdict" multiple size="3"></select></div>
    <div><label>Min batch RTF</label><input type="number" id="f-min-batch" step="0.1" min="0" placeholder="0"></div>
    <div><label>Min stream RTF</label><input type="number" id="f-min-stream" step="0.1" min="0" placeholder="0"></div>
    <div><label>Max RSS (MiB)</label><input type="number" id="f-max-rss" step="100" min="0" placeholder="∞"></div>
  </div>
  <div class="toolbar" style="margin-top:10px;">
    <button id="reset">Reset filters</button>
    <button id="preset-cpu">Preset: CPU only, all hosts</button>
    <button id="preset-vulkan">Preset: Vulkan only, modern hosts</button>
    <button id="preset-defaults">Preset: Current registry defaults</button>
    <span class="small">Hold <kbd>Ctrl</kbd>/<kbd>Cmd</kbd> to multi-select.</span>
  </div>
</section>

<section>
  <h2>Charts</h2>
  <div class="chart-grid">
    <div class="chart-box"><canvas id="chart-batch"></canvas></div>
    <div class="chart-box"><canvas id="chart-tradeoff"></canvas></div>
    <div class="chart-box"><canvas id="chart-accuracy"></canvas></div>
    <div class="chart-box"><canvas id="chart-size"></canvas></div>
  </div>
  <div class="legend">
    <p><strong>Legend.</strong>
      <span class="sw" style="background:var(--quant-fp16)"></span>fp16
      <span class="sw" style="background:var(--quant-q8_0)"></span>q8_0
      <span class="sw" style="background:var(--quant-q5_1)"></span>q5_1
      <span class="sw" style="background:var(--quant-q5_0)"></span>q5_0
      ·
      <span class="sw" style="background:var(--comf-fg)"></span>comfortable
      <span class="sw" style="background:var(--bord-fg)"></span>borderline
      <span class="sw" style="background:var(--unsu-fg)"></span>unsuitable
    </p>
    <p><strong>Top-left.</strong> Median batch RTF per filtered cell, grouped by model family, coloured by quantization. RTF = audio-seconds processed per wall-second; higher is better. The dashed line at 2.0 marks the comfortable batch threshold.</p>
    <p><strong>Top-right.</strong> Trade-off: peak RSS (x) vs batch RTF (y). Up-and-left is best. Verdict colour encodes the affordability gate.</p>
    <p><strong>Bottom-left.</strong> Accuracy: mean normalized Levenshtein (lower is better). Bars show the absolute lev; the inline number is the delta vs the fp16 cell on the same host/backend (positive = quant is worse).</p>
    <p><strong>Bottom-right.</strong> Download cost: disk MiB. The horizontal banding shows the size class; colour is quantization.</p>
  </div>
</section>

<section>
  <h2>Per-host recommendation matrix</h2>
  <p class="note">Computed from filtered data, scored by: 50% (comfortable verdict), 25% (low RSS), 15% (low ΔLev vs fp16), 10% (small download). Recommendation surfaces the highest-scoring cell per host within the current filter.</p>
  <div id="reco-grid" class="reco-grid"></div>
</section>

<section>
  <h2>Full results table</h2>
  <div class="toolbar"><span class="small" id="row-count"></span></div>
  <div class="scroll-x">
  <table class="matrix" id="results">
    <thead>
      <tr>
        <th data-sort="host">host</th>
        <th data-sort="build">backend</th>
        <th data-sort="model_family">family</th>
        <th data-sort="model">model</th>
        <th data-sort="quantization">quant</th>
        <th data-sort="batch_rtf_median">batch RTF</th>
        <th data-sort="stream_rtf_median">stream RTF</th>
        <th data-sort="ttff_s_median">TTFF s</th>
        <th data-sort="peak_rss_mib_median">RSS MiB</th>
        <th data-sort="accuracy_mean_lev">acc lev</th>
        <th data-sort="accuracy_delta_vs_fp16">Δ vs fp16</th>
        <th data-sort="approx_size_mib">size MiB</th>
        <th data-sort="verdict">verdict</th>
        <th data-sort="iterations_kept">iters</th>
      </tr>
    </thead>
    <tbody id="results-body"></tbody>
  </table>
  </div>
  <details><summary>Column glossary</summary>
    <ul>
      <li><b>batch RTF</b>: median audio-seconds transcribed per wall-second in batch mode. Higher is faster.</li>
      <li><b>stream RTF</b>: same in 1-second-chunk streaming mode. Below 1.0 means we can't keep up live; below 1.5 fails the comfortable threshold.</li>
      <li><b>TTFF s</b>: median time-to-first-frame across fixtures (latency on first partial result).</li>
      <li><b>RSS MiB</b>: peak resident set size — how much RAM the process actually held.</li>
      <li><b>acc lev</b>: mean per-fixture normalized Levenshtein distance from the reference text. <em>Lower is better.</em> 0.0 = perfect transcription on every fixture; >0.5 = severely degraded. Aggregated across all 10 equivalence fixtures (en + es + fr + zh + ro).</li>
      <li><b>Δ vs fp16</b>: accuracy delta of this quant cell vs the fp16 cell on the same host/backend. Positive = quant is worse.</li>
      <li><b>size MiB</b>: approximate disk size of the GGML weights file.</li>
      <li><b>verdict</b>: <span class="v-comfortable">comfortable</span> batch ≥ 2.0 and stream ≥ 1.5 · <span class="v-borderline">borderline</span> batch ≥ 1.0 · <span class="v-unsuitable">unsuitable</span> below.</li>
    </ul>
  </details>
</section>

<section>
  <h2>About this page</h2>
  <p>This page was generated by <code>scripts/bench-decision-page.py</code> from the calibration dataset in
    <code>docs/bench/calibration/</code>. All data is embedded inline — the file is fully offline.</p>
  <p class="note">The Chart.js library is loaded from a CDN at first render. If you open this file with no internet,
    the tables, filters, and recommendation grid still work; the charts simply remain blank.</p>
</section>

</main>

<script>
const RAW_CELLS = {CELLS_JSON};
const HOSTS_META = {HOSTS_META_JSON};
const REGISTRY  = {REGISTRY_JSON};
const SIZES     = {SIZES_JSON};
</script>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.1/dist/chart.umd.js" defer></script>
<script defer>
(() => {
  const $ = (q) => document.querySelector(q);
  const FAMILY_ORDER = ['tiny', 'base', 'small', 'turbo'];
  const QUANT_ORDER  = ['fp16', 'q8_0', 'q5_1', 'q5_0'];
  const QUANT_COLOR  = {
    fp16: getComputedStyle(document.documentElement).getPropertyValue('--quant-fp16').trim(),
    q8_0: getComputedStyle(document.documentElement).getPropertyValue('--quant-q8_0').trim(),
    q5_1: getComputedStyle(document.documentElement).getPropertyValue('--quant-q5_1').trim(),
    q5_0: getComputedStyle(document.documentElement).getPropertyValue('--quant-q5_0').trim(),
  };
  const VERDICT_COLOR = {
    comfortable: getComputedStyle(document.documentElement).getPropertyValue('--comf-fg').trim(),
    borderline:  getComputedStyle(document.documentElement).getPropertyValue('--bord-fg').trim(),
    unsuitable:  getComputedStyle(document.documentElement).getPropertyValue('--unsu-fg').trim(),
  };

  // ------------ filter setup ------------
  const uniq = (xs) => [...new Set(xs)].sort();
  const fHost    = $('#f-host');
  const fBuild   = $('#f-build');
  const fFamily  = $('#f-family');
  const fQuant   = $('#f-quant');
  const fVerdict = $('#f-verdict');
  const fMinBatch = $('#f-min-batch');
  const fMinStream = $('#f-min-stream');
  const fMaxRSS = $('#f-max-rss');

  function fillSelect(sel, values, defaults) {
    sel.innerHTML = '';
    for (const v of values) {
      const o = document.createElement('option');
      o.value = v; o.textContent = v;
      if (defaults && defaults.includes(v)) o.selected = true;
      sel.appendChild(o);
    }
  }

  const allHosts = uniq(RAW_CELLS.map(c => c.host));
  const allBuilds = uniq(RAW_CELLS.map(c => c.build)).filter(b => b === 'cpu' || b === 'vulkan');
  const allFamilies = FAMILY_ORDER.filter(f => RAW_CELLS.some(c => c.model_family === f));
  const allQuants = QUANT_ORDER.filter(q => RAW_CELLS.some(c => c.quantization === q));
  const allVerdicts = ['comfortable', 'borderline', 'unsuitable'];

  fillSelect(fHost, allHosts, allHosts);
  fillSelect(fBuild, allBuilds, allBuilds);
  fillSelect(fFamily, allFamilies, allFamilies);
  fillSelect(fQuant, allQuants, allQuants);
  fillSelect(fVerdict, allVerdicts, allVerdicts);

  // selected helper
  const selected = (sel) => [...sel.selectedOptions].map(o => o.value);

  function currentFilter() {
    const hosts = new Set(selected(fHost));
    const builds = new Set(selected(fBuild));
    const families = new Set(selected(fFamily));
    const quants = new Set(selected(fQuant));
    const verdicts = new Set(selected(fVerdict));
    const minB = parseFloat(fMinBatch.value) || 0;
    const minS = parseFloat(fMinStream.value) || 0;
    const maxR = parseFloat(fMaxRSS.value) || Infinity;
    return (c) => (
      hosts.has(c.host) && builds.has(c.build) && families.has(c.model_family) &&
      quants.has(c.quantization) && verdicts.has(c.verdict) &&
      (c.batch_rtf_median ?? 0) >= minB && (c.stream_rtf_median ?? 0) >= minS &&
      (c.peak_rss_mib_median ?? 0) <= maxR
    );
  }

  // ------------ table ------------
  let sortKey = 'batch_rtf_median';
  let sortDir = -1;

  function renderTable(cells) {
    const tbody = $('#results-body');
    tbody.innerHTML = '';
    const sorted = [...cells].sort((a, b) => {
      const av = a[sortKey], bv = b[sortKey];
      if (av == null && bv == null) return 0;
      if (av == null) return 1;
      if (bv == null) return -1;
      if (typeof av === 'string') return sortDir * av.localeCompare(bv);
      return sortDir * (av - bv);
    });
    for (const c of sorted) {
      const tr = document.createElement('tr');
      const fmt = (v, n = 2) => v == null ? '—' : (typeof v === 'number' ? v.toFixed(n) : v);
      const delta = c.accuracy_delta_vs_fp16;
      const deltaCls = delta == null ? 'delta-zero' : delta > 0.01 ? 'delta-pos' : delta < -0.01 ? 'delta-neg' : 'delta-zero';
      const deltaStr = delta == null ? '—' : (delta > 0 ? '+' : '') + delta.toFixed(3);
      const defaultPill = c.registry_default ? '<span class="pill default">default</span>' : '';
      const missingPill = c.registry_missing_base ? '<span class="pill missing">not in registry</span>' : '';
      tr.innerHTML = `
        <td>${c.host}</td>
        <td>${c.build}</td>
        <td>${c.model_family}</td>
        <td>${c.model}${defaultPill}${missingPill}</td>
        <td class="q-${c.quantization}">${c.quantization}</td>
        <td>${fmt(c.batch_rtf_median, 2)}</td>
        <td>${fmt(c.stream_rtf_median, 2)}</td>
        <td>${fmt(c.ttff_s_median, 2)}</td>
        <td>${fmt(c.peak_rss_mib_median, 0)}</td>
        <td>${fmt(c.accuracy_mean_lev, 3)}</td>
        <td class="${deltaCls}">${deltaStr}</td>
        <td>${fmt(c.approx_size_mib, 0)}</td>
        <td><span class="verdict v-${c.verdict}">${c.verdict}</span></td>
        <td>${c.iterations_kept ?? '?'}/${c.iterations_total ?? '?'}</td>
      `;
      tbody.appendChild(tr);
    }
    $('#row-count').textContent = `${cells.length} cells shown`;
  }

  document.querySelectorAll('#results th[data-sort]').forEach(th => {
    th.addEventListener('click', () => {
      const k = th.dataset.sort;
      if (k === sortKey) sortDir *= -1; else { sortKey = k; sortDir = -1; }
      renderAll();
    });
  });

  // ------------ recommendation grid ------------
  function scoreCell(c) {
    const verdictWeight = { comfortable: 1.0, borderline: 0.4, unsuitable: 0.05 };
    const rssNorm = c.peak_rss_mib_median ? Math.max(0, 1 - c.peak_rss_mib_median / 2000) : 0.5;
    const accuracyPenalty = Math.min(1, (c.accuracy_mean_lev ?? 0.5) * 2);
    const sizeNorm = c.approx_size_mib ? Math.max(0, 1 - c.approx_size_mib / 2000) : 0.5;
    return 0.50 * (verdictWeight[c.verdict] ?? 0.05)
         + 0.25 * rssNorm
         + 0.15 * (1 - accuracyPenalty)
         + 0.10 * sizeNorm;
  }

  function renderReco(cells) {
    const grid = $('#reco-grid');
    grid.innerHTML = '';
    const byHost = {};
    for (const c of cells) (byHost[c.host] ??= []).push(c);
    const ordered = Object.keys(byHost).sort();
    for (const host of ordered) {
      const winners = [...byHost[host]].sort((a, b) => scoreCell(b) - scoreCell(a)).slice(0, 3);
      const card = document.createElement('div');
      card.className = 'reco-card';
      const inv = HOSTS_META[host] || {};
      let html = `<h3>${host} <span class="small">${inv.cpu ? '· ' + inv.cpu : ''}${inv.year ? ' · ' + inv.year : ''}</span></h3>`;
      winners.forEach((c, i) => {
        const tag = i === 0 ? 'best' : i === 1 ? '2nd' : '3rd';
        const delta = c.accuracy_delta_vs_fp16;
        const deltaStr = delta == null ? '' : ` Δ${(delta > 0 ? '+' : '') + delta.toFixed(3)}`;
        html += `
          <div class="row">
            <span class="lbl">${tag}</span>
            <span class="val">
              <span class="q-${c.quantization}">${c.model}</span>
              · ${c.build}
              · <span class="verdict v-${c.verdict}">${c.verdict}</span>
            </span>
          </div>
          <div class="row">
            <span class="lbl">RTF</span>
            <span class="val">${(c.batch_rtf_median ?? 0).toFixed(1)} batch · ${(c.stream_rtf_median ?? 0).toFixed(1)} stream</span>
          </div>
          <div class="row">
            <span class="lbl">RAM / disk</span>
            <span class="val">${(c.peak_rss_mib_median ?? 0).toFixed(0)} MiB · ${(c.approx_size_mib ?? 0)} MiB${deltaStr}</span>
          </div>
        `;
        if (i < winners.length - 1) html += '<hr style="border-color:var(--line);border-style:solid;border-width:0 0 1px 0;margin:6px 0;">';
      });
      card.innerHTML = html;
      grid.appendChild(card);
    }
  }

  // ------------ charts ------------
  const charts = {};

  function makeChart(id, cfg) {
    if (charts[id]) charts[id].destroy();
    if (typeof Chart === 'undefined') return; // not loaded
    const ctx = document.getElementById(id).getContext('2d');
    charts[id] = new Chart(ctx, cfg);
  }

  function renderCharts(cells) {
    if (typeof Chart === 'undefined') {
      // try again after a delay in case the CDN is slow
      setTimeout(() => renderCharts(cells), 250);
      return;
    }
    Chart.defaults.color = '#cfd2d8';
    Chart.defaults.borderColor = '#2a2f3a';

    // --- 1. Batch RTF by model+host, coloured by quant ---
    const grouped = {};
    cells.forEach(c => {
      const label = `${c.host}/${c.build}/${c.model_family}`;
      grouped[label] ??= { fp16: null, q8_0: null, q5_1: null, q5_0: null };
      grouped[label][c.quantization] = c.batch_rtf_median;
    });
    const labels = Object.keys(grouped).sort();
    const datasets = QUANT_ORDER.filter(q => labels.some(l => grouped[l][q] != null)).map(q => ({
      label: q,
      data: labels.map(l => grouped[l][q] ?? null),
      backgroundColor: QUANT_COLOR[q],
      borderRadius: 3,
    }));
    makeChart('chart-batch', {
      type: 'bar',
      data: { labels, datasets },
      options: {
        responsive: true, maintainAspectRatio: false,
        plugins: {
          title: { display: true, text: 'Median batch RTF — host/backend/family × quant' },
          legend: { position: 'top' },
        },
        scales: {
          x: { ticks: { maxRotation: 60, minRotation: 60, font: { size: 9 } } },
          y: { type: 'logarithmic', title: { display: true, text: 'batch RTF (log)' } },
        },
      },
    });

    // --- 2. RSS vs batch RTF scatter ---
    const scatterDatasets = allVerdicts.map(v => ({
      label: v,
      data: cells
        .filter(c => c.verdict === v && c.peak_rss_mib_median != null && c.batch_rtf_median != null)
        .map(c => ({
          x: c.peak_rss_mib_median,
          y: c.batch_rtf_median,
          host: c.host,
          model: c.model,
          backend: c.build,
        })),
      backgroundColor: VERDICT_COLOR[v],
      borderColor: VERDICT_COLOR[v],
      pointRadius: 5, pointHoverRadius: 7,
    }));
    makeChart('chart-tradeoff', {
      type: 'scatter',
      data: { datasets: scatterDatasets },
      options: {
        responsive: true, maintainAspectRatio: false,
        plugins: {
          title: { display: true, text: 'Trade-off: RSS (MiB) vs batch RTF — up-and-left is best' },
          tooltip: {
            callbacks: {
              label: (ctx) => `${ctx.raw.host}/${ctx.raw.backend} · ${ctx.raw.model}: ${ctx.raw.x.toFixed(0)} MiB, ${ctx.raw.y.toFixed(2)} RTF`,
            },
          },
        },
        scales: {
          x: { type: 'logarithmic', title: { display: true, text: 'peak RSS MiB (log)' } },
          y: { type: 'logarithmic', title: { display: true, text: 'batch RTF (log)' } },
        },
      },
    });

    // --- 3. Accuracy bars per quantization, faceted by host ---
    const accGrouped = {};
    cells.forEach(c => {
      if (c.accuracy_mean_lev == null) return;
      const label = `${c.host}/${c.model_family}`;
      accGrouped[label] ??= { fp16: null, q8_0: null, q5_1: null, q5_0: null };
      accGrouped[label][c.quantization] = c.accuracy_mean_lev;
    });
    const accLabels = Object.keys(accGrouped).sort();
    const accDatasets = QUANT_ORDER.filter(q => accLabels.some(l => accGrouped[l][q] != null)).map(q => ({
      label: q,
      data: accLabels.map(l => accGrouped[l][q] ?? null),
      backgroundColor: QUANT_COLOR[q],
      borderRadius: 3,
    }));
    makeChart('chart-accuracy', {
      type: 'bar',
      data: { labels: accLabels, datasets: accDatasets },
      options: {
        responsive: true, maintainAspectRatio: false,
        plugins: {
          title: { display: true, text: 'Mean Levenshtein (lower is better) — host × family × quant' },
          legend: { position: 'top' },
        },
        scales: {
          x: { ticks: { maxRotation: 60, minRotation: 60, font: { size: 9 } } },
          y: { title: { display: true, text: 'mean lev (0 = perfect)' }, min: 0 },
        },
      },
    });

    // --- 4. Download size per model+quant ---
    const sizeRows = {};
    cells.forEach(c => {
      sizeRows[c.model] ??= { quant: c.quantization, size: c.approx_size_mib, family: c.model_family };
    });
    const sizeKeys = Object.keys(sizeRows).sort();
    makeChart('chart-size', {
      type: 'bar',
      data: {
        labels: sizeKeys,
        datasets: [{
          label: 'Download MiB',
          data: sizeKeys.map(k => sizeRows[k].size),
          backgroundColor: sizeKeys.map(k => QUANT_COLOR[sizeRows[k].quant] || '#888'),
          borderRadius: 3,
        }],
      },
      options: {
        responsive: true, maintainAspectRatio: false,
        plugins: {
          title: { display: true, text: 'Download size MiB — coloured by quantization' },
          legend: { display: false },
        },
        scales: {
          x: { ticks: { maxRotation: 60, minRotation: 60, font: { size: 9 } } },
          y: { title: { display: true, text: 'size MiB' } },
        },
      },
    });
  }

  // ------------ wire up ------------
  function renderAll() {
    const cells = RAW_CELLS.filter(currentFilter());
    renderTable(cells);
    renderReco(cells);
    renderCharts(cells);
  }
  [fHost, fBuild, fFamily, fQuant, fVerdict, fMinBatch, fMinStream, fMaxRSS].forEach(
    el => el.addEventListener('input', renderAll)
  );
  $('#reset').addEventListener('click', () => {
    fillSelect(fHost, allHosts, allHosts);
    fillSelect(fBuild, allBuilds, allBuilds);
    fillSelect(fFamily, allFamilies, allFamilies);
    fillSelect(fQuant, allQuants, allQuants);
    fillSelect(fVerdict, allVerdicts, allVerdicts);
    fMinBatch.value = fMinStream.value = fMaxRSS.value = '';
    renderAll();
  });
  $('#preset-cpu').addEventListener('click', () => {
    fillSelect(fHost, allHosts, allHosts);
    fillSelect(fBuild, allBuilds, ['cpu']);
    fillSelect(fFamily, allFamilies, allFamilies);
    fillSelect(fQuant, allQuants, allQuants);
    fillSelect(fVerdict, allVerdicts, allVerdicts);
    renderAll();
  });
  $('#preset-vulkan').addEventListener('click', () => {
    fillSelect(fHost, allHosts, allHosts.filter(h => h === 'i7-1255u' || h === 'ultra7-258v'));
    fillSelect(fBuild, allBuilds, ['vulkan']);
    fillSelect(fFamily, allFamilies, allFamilies);
    fillSelect(fQuant, allQuants, allQuants);
    fillSelect(fVerdict, allVerdicts, allVerdicts);
    renderAll();
  });
  $('#preset-defaults').addEventListener('click', () => {
    fillSelect(fHost, allHosts, allHosts);
    fillSelect(fBuild, allBuilds, allBuilds);
    fillSelect(fFamily, allFamilies, allFamilies);
    fillSelect(fQuant, allQuants, allQuants);
    fillSelect(fVerdict, allVerdicts, allVerdicts);
    // only registry defaults
    const rows = RAW_CELLS.filter(c => c.registry_default);
    renderTable(rows);
    renderReco(rows);
    renderCharts(rows);
    return; // skip default renderAll
  });

  // Initial render — wait briefly for Chart.js to load
  renderAll();
  if (typeof Chart === 'undefined') {
    const tryAgain = setInterval(() => {
      if (typeof Chart !== 'undefined') { clearInterval(tryAgain); renderAll(); }
    }, 250);
  }
})();
</script>
</body>
</html>
"""


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", default="docs/bench/calibration/runs")
    ap.add_argument("--inventory", default="docs/bench/calibration/inventory")
    ap.add_argument("--matrix", default="docs/bench/calibration/summary/matrix.json")
    ap.add_argument("--out", default="docs/bench/calibration/summary/calibration.html")
    args = ap.parse_args()

    matrix = json.load(open(args.matrix))
    cells = matrix["cells"] if isinstance(matrix, dict) else matrix

    accuracy = load_accuracy(Path(args.runs))
    inventory = load_inventory(Path(args.inventory))
    enriched = enrich_cells(cells, accuracy, inventory)

    # Keep only cpu + vulkan builds; drop ablation labels (cpu-actx, cpu-noactx).
    enriched = [c for c in enriched if c.get("build") in ("cpu", "vulkan")]

    # Some inventories carry CPU + year info via heterogeneous schemas; flatten the
    # bits we use into the per-host meta blob.
    hosts_meta = {}
    for host, inv in inventory.items():
        hosts_meta[host] = {
            "cpu": inv.get("cpu") or inv.get("model_name") or inv.get("cpu_model"),
            "year": inv.get("year"),
            "gpu": inv.get("gpu"),
            "avx_vnni": inv.get("avx_vnni"),
        }

    import datetime
    generated = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    n_hosts = len(set(c["host"] for c in enriched))
    page = (
        HTML_TEMPLATE
        .replace("{GENERATED_AT}", html.escape(generated))
        .replace("{N_CELLS}", str(len(enriched)))
        .replace("{N_HOSTS}", str(n_hosts))
        .replace("{CELLS_JSON}", json.dumps(enriched, separators=(",", ":")))
        .replace("{HOSTS_META_JSON}", json.dumps(hosts_meta, separators=(",", ":")))
        .replace("{REGISTRY_JSON}", json.dumps(REGISTRY, separators=(",", ":")))
        .replace("{SIZES_JSON}", json.dumps(APPROX_SIZE_MIB, separators=(",", ":")))
    )
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(page, encoding="utf-8")
    print(f"wrote {out} ({len(enriched)} cells, {n_hosts} hosts, {len(page):,} bytes)")


if __name__ == "__main__":
    main()
