#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""
Generate docs/bench/calibration/summary/calibration3.html — corrected,
filter-driven model decision page.

Fixes the family-collapse, ac/battery-collapse, and inverted-threshold
bugs in calibration.html (v1) and calibration2.html (v2). Data-derived
key findings; explicit coverage matrix; deterministic best-cell pick.
"""

from __future__ import annotations

import argparse
import html
import json
import re
import statistics
import sys
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

# ───────────────────── constants ────────────────────────────────────────────

APPROX_SIZE_MIB: dict[str, int] = {
    "tiny": 78, "tiny-q8_0": 44, "tiny-q5_1": 32,
    "tiny.en": 78, "tiny.en-q8_0": 44, "tiny.en-q5_1": 32,
    "base": 148, "base-q8_0": 82, "base-q5_1": 60,
    "base.en": 148, "base.en-q8_0": 82, "base.en-q5_1": 60,
    "small": 466, "small-q8_0": 264, "small-q5_1": 181,
    "small.en": 466, "small.en-q8_0": 264, "small.en-q5_1": 181,
    "large-v3-turbo": 1543, "large-v3-turbo-q8_0": 834, "large-v3-turbo-q5_0": 547,
}

QUANT_RE = re.compile(r"^(.+?)(?:-(q5_0|q5_1|q8_0))?$")
FILE_RE = re.compile(
    r"^(?P<host>[^_]+(?:_[^_]+)*)__(?P<power>[^_]+)__(?P<build>[^_]+)__"
    r"(?P<model>.+?)__iter\d+\.json$"
)

THRESH = {
    "batch_comfort":  2.0,
    "batch_ok":       1.0,
    "stream_comfort": 1.5,
    "stream_ok":      1.0,
    "delta_mean_max": 0.05,
    "delta_max_max":  0.20,
}


def split_quant(model: str) -> tuple[str, str]:
    m = QUANT_RE.match(model)
    assert m, model
    return m.group(1), m.group(2) or "fp16"


def family_of(base: str) -> str:
    if "large-v3-turbo" in base: return "turbo"
    if base.startswith("small"): return "small"
    if base.startswith("base"):  return "base"
    if base.startswith("tiny"):  return "tiny"
    return base


def language_of(base: str) -> str:
    return "en" if base.endswith(".en") else "multi"


# ───────────────────── loaders ──────────────────────────────────────────────

def load_matrix(path: str) -> list[dict]:
    d = json.load(open(path))
    return d["cells"] if isinstance(d, dict) else d


def load_inventory(inv_dir: Path) -> dict[str, dict]:
    out: dict[str, dict] = {}
    for f in sorted(inv_dir.glob("*.json")):
        try:
            d = json.load(open(f))
            out[d.get("host_id", f.stem)] = d
        except Exception as e:
            print(f"  warn: inventory {f.name}: {e}", file=sys.stderr)
    return out


def load_accuracy(runs_dir: Path) -> dict[tuple, dict]:
    """Per (host, power, build, model) accuracy stats from per-iteration runs."""
    bucket: dict[tuple, list[float]] = defaultdict(list)
    en_bucket: dict[tuple, list[float]] = defaultdict(list)

    for path in sorted(runs_dir.glob("*.json")):
        if ".time." in path.name:
            continue
        m = FILE_RE.match(path.name)
        if not m:
            continue
        try:
            data = json.load(open(path))
        except Exception:
            continue
        for r in data.get("results", []):
            if r.get("synthetic_placeholder") or r.get("skip_reason"):
                continue
            metrics = r.get("metrics") or {}
            lev = metrics.get("stt_accuracy_levenshtein")
            if lev is None:
                lev = metrics.get("stt_levenshtein_norm")
            if lev is None:
                continue
            key = (m["host"], m["power"], m["build"], m["model"])
            bucket[key].append(float(lev))
            lang = (r.get("language") or "").lower()
            if lang.startswith("en"):
                en_bucket[key].append(float(lev))

    out: dict[tuple, dict] = {}
    for k, vs in bucket.items():
        en_vs = en_bucket.get(k, [])
        out[k] = {
            "all_mean": round(statistics.mean(vs), 4),
            "all_max":  round(max(vs), 4),
            "en_mean":  round(statistics.mean(en_vs), 4) if en_vs else None,
            "en_max":   round(max(en_vs), 4) if en_vs else None,
            "n":    len(vs),
            "en_n": len(en_vs),
        }
    return out


def compute_deltas(accuracy: dict[tuple, dict]) -> dict[tuple, dict]:
    """Δ vs fp16 baseline per (host, power, build, base_model, quant)."""
    groups: dict[tuple, dict] = defaultdict(dict)
    for (host, power, build, model), stats in accuracy.items():
        base, quant = split_quant(model)
        groups[(host, power, build, base)][quant] = stats

    deltas: dict[tuple, dict] = {}
    for (host, power, build, base), quants in groups.items():
        fp16 = quants.get("fp16", {})
        fp16_en_mean = fp16.get("en_mean")
        fp16_en_max  = fp16.get("en_max")
        for quant, stats in quants.items():
            if quant == "fp16":
                continue
            if fp16_en_mean is None or stats.get("en_mean") is None:
                continue
            d_mean = stats["en_mean"] - fp16_en_mean
            d_max  = (stats.get("en_max") or 0.0) - (fp16_en_max or 0.0)
            ok = d_mean <= THRESH["delta_mean_max"] and d_max <= THRESH["delta_max_max"]
            deltas[(host, power, build, base, quant)] = {
                "delta_en_mean": round(d_mean, 4),
                "delta_en_max":  round(d_max, 4),
                "pass": ok,
            }
    return deltas


def enrich_cell(c: dict, accuracy: dict, deltas: dict, inv: dict) -> dict:
    model = c["model"]
    base, quant = split_quant(model)
    power = c.get("power", "ac")
    acc = accuracy.get((c["host"], power, c["build"], model), {})
    delta = deltas.get((c["host"], power, c["build"], base, quant))
    hi = inv.get(c["host"], {})
    return {
        **c,
        "model_family": family_of(base),
        "model_base":   base,
        "language":     language_of(base),
        "quantization": quant,
        "approx_size_mib": APPROX_SIZE_MIB.get(model),
        "accuracy_en_mean": acc.get("en_mean"),
        "accuracy_en_max":  acc.get("en_max"),
        "accuracy_all_mean": acc.get("all_mean"),
        "delta_en_mean": delta["delta_en_mean"] if delta else None,
        "delta_en_max":  delta["delta_en_max"]  if delta else None,
        "accuracy_pass": delta["pass"]          if delta else None,
        "host_released":   hi.get("released"),
        "host_cpu_model":  hi.get("cpu_model"),
        "host_cores":      hi.get("physical_cores"),
        "host_has_avx_vnni":     bool(hi.get("has_avx_vnni", False)),
        "host_has_avx512_vnni":  bool(hi.get("has_avx512_vnni", False)),
        "host_quant_kernel": (
            "vnni" if hi.get("has_avx_vnni") or hi.get("has_avx512_vnni")
            else "avx2-fallback"
        ),
        "host_gpu": (hi.get("gpu") or [""])[0][:80] if hi.get("gpu") else None,
    }


# ───────────────────── validation ───────────────────────────────────────────

def validate(cells: list[dict]) -> list[str]:
    errors: list[str] = []
    seen: dict[tuple, int] = {}
    for i, c in enumerate(cells):
        for k in ("host", "power", "build", "model"):
            if not c.get(k):
                errors.append(f"cell #{i}: missing key {k!r}")
        key = (c.get("host"), c.get("power"), c.get("build"), c.get("model"))
        if key in seen:
            errors.append(f"duplicate cell key {key} at #{i} (first seen #{seen[key]})")
        seen[key] = i
        if c.get("iterations_kept") is None:
            errors.append(f"cell #{i} {key}: missing iterations_kept")
    return errors


# ───────────────────── findings (data-derived) ──────────────────────────────

def derive_findings(cells: list[dict]) -> dict:
    """Compute key findings used in the page banner."""
    # Quant speedup vs fp16 per (host, build, model_base) — AC only.
    ac = [c for c in cells if c.get("power") == "ac" and c.get("batch_rtf_median")]
    fp16_index: dict[tuple, float] = {}
    for c in ac:
        if c["quantization"] == "fp16":
            fp16_index[(c["host"], c["build"], c["model_base"])] = c["batch_rtf_median"]

    by_kernel_build: dict[tuple, list[float]] = defaultdict(list)
    for c in ac:
        if c["quantization"] == "fp16":
            continue
        fp = fp16_index.get((c["host"], c["build"], c["model_base"]))
        if not fp:
            continue
        ratio = c["batch_rtf_median"] / fp
        by_kernel_build[(c["host_quant_kernel"], c["build"], c["quantization"])].append(ratio)

    speedup_summary: list[dict] = []
    for (kernel, build, quant), ratios in sorted(by_kernel_build.items()):
        speedup_summary.append({
            "kernel": kernel, "build": build, "quant": quant,
            "n": len(ratios),
            "median": round(statistics.median(ratios), 2),
            "max": round(max(ratios), 2),
            "min": round(min(ratios), 2),
        })

    # Coverage: which (host, ac, cpu, family, language, quant) entries are missing.
    EXPECTED_QUANTS = {
        "tiny": ["fp16","q8_0","q5_1"], "base": ["fp16","q8_0","q5_1"],
        "small": ["fp16","q8_0","q5_1"], "turbo": ["fp16","q8_0","q5_0"],
    }
    hosts = sorted({c["host"] for c in cells})
    present = {(c["host"], c["build"], c["model"]) for c in ac}

    def model_name(fam: str, lang: str, quant: str) -> str:
        bases = {"tiny":"tiny","base":"base","small":"small","turbo":"large-v3-turbo"}
        suffix = ".en" if lang == "en" and fam != "turbo" else ""
        base = bases[fam] + suffix
        return base if quant == "fp16" else f"{base}-{quant}"

    gaps: list[dict] = []
    for h in hosts:
        for build in ("cpu", "vulkan"):
            for fam, quants in EXPECTED_QUANTS.items():
                langs = ["multi"] if fam == "turbo" else ["multi","en"]
                for lang in langs:
                    for q in quants:
                        m = model_name(fam, lang, q)
                        if (h, build, m) not in present:
                            gaps.append({"host":h,"build":build,"model":m,"family":fam,"language":lang,"quant":q})

    return {"speedup_summary": speedup_summary, "gaps": gaps, "n_gaps": len(gaps)}


# ───────────────────── HTML template ────────────────────────────────────────

HTML_TEMPLATE = r"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Fono — Model Decision Page (v3)</title>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.3/dist/chart.umd.min.js"></script>
<script src="https://cdn.jsdelivr.net/npm/chartjs-plugin-annotation@3.1.0/dist/chartjs-plugin-annotation.min.js"></script>
<style>
:root {
  --bg:#0d1117; --bg2:#161b22; --bg3:#21262d; --border:#30363d;
  --text:#e6edf3; --muted:#8b949e;
  --green:#3fb950; --yellow:#d29922; --red:#f85149;
  --green-bg:#0d2a18; --yellow-bg:#2d2208; --red-bg:#2d0f0e;
  --green-dim:#1b3f29; --yellow-dim:#4a3a14; --red-dim:#4a1a1a;
  --blue:#58a6ff; --purple:#bc8cff; --orange:#ff8c00; --teal:#00c3ad;
}
*,*::before,*::after{box-sizing:border-box;margin:0;padding:0}
body{background:var(--bg);color:var(--text);font:14px/1.55 -apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,sans-serif}
a{color:var(--blue)}
.page{max-width:1280px;margin:0 auto;padding:24px 16px 64px}
h1{font-size:1.6rem;font-weight:700;margin-bottom:4px}
.subtitle{color:var(--muted);margin-bottom:18px}
h2{font-size:1.15rem;font-weight:600;margin:36px 0 6px;color:var(--blue);border-bottom:1px solid var(--border);padding-bottom:6px}
h3{font-size:.95rem;font-weight:600;margin:18px 0 8px;color:var(--muted);text-transform:uppercase;letter-spacing:.04em}
p.desc{color:var(--muted);font-size:13px;margin-bottom:14px;max-width:880px}

/* Sticky filter bar */
.filter-bar{position:sticky;top:0;z-index:50;background:var(--bg2);border:1px solid var(--border);border-radius:8px;padding:12px 14px;margin-bottom:20px;display:flex;flex-wrap:wrap;gap:10px;align-items:center}
.filter-bar label{font-size:12px;color:var(--muted);display:flex;align-items:center;gap:4px}
.filter-bar select,.filter-bar input{background:var(--bg3);border:1px solid var(--border);color:var(--text);border-radius:4px;padding:4px 8px;font-size:12px}
.filter-bar .presets{display:flex;gap:6px;margin-left:auto}
.filter-bar button{background:var(--bg3);border:1px solid var(--border);color:var(--text);border-radius:4px;padding:4px 10px;font-size:12px;cursor:pointer}
.filter-bar button:hover{background:var(--border)}

.findings{background:var(--bg2);border:1px solid var(--border);border-radius:8px;padding:14px 16px;margin-bottom:20px;font-size:13px}
.findings h3{margin-top:0;color:var(--blue)}
.findings ul{margin:6px 0 0 18px}
.findings li{margin-bottom:3px}
.findings code{background:var(--bg3);padding:1px 5px;border-radius:3px;font-size:12px}

.chart-box{background:var(--bg2);border:1px solid var(--border);border-radius:8px;padding:14px;margin-bottom:18px}
.chart-box canvas{width:100%!important}
.two-col{display:grid;grid-template-columns:1fr 1fr;gap:18px}
@media (max-width:900px){.two-col{grid-template-columns:1fr}}

/* Verdict heatmap */
.heatmap-wrap{overflow-x:auto}
table.heatmap{border-collapse:collapse;width:100%;min-width:660px}
table.heatmap th,table.heatmap td{border:1px solid var(--border);padding:8px 10px;text-align:center;vertical-align:middle;font-size:12px}
table.heatmap th{background:var(--bg3);font-weight:600;color:var(--muted);text-transform:uppercase;font-size:11px}
table.heatmap td.host-th{text-align:left;background:var(--bg3);min-width:130px}
.host-th .hw{font-weight:600;font-size:13px;color:var(--text)}
.host-th .meta{font-size:11px;color:var(--muted)}
.host-th .kernel-vnni{color:var(--blue);font-size:11px}
.host-th .kernel-fallback{color:var(--muted);font-size:11px}
.cell-comfortable{background:var(--green-dim)}
.cell-borderline{background:var(--yellow-dim)}
.cell-unsuitable{background:var(--red-dim)}
.cell-na{background:var(--bg3);color:var(--muted)}
.verdict-label{font-size:11px;font-weight:700;text-transform:uppercase;letter-spacing:.04em}
.verdict-label.comfortable{color:var(--green)}
.verdict-label.borderline{color:var(--yellow)}
.verdict-label.unsuitable{color:var(--red)}
.cell-detail{font-size:11px;color:var(--muted);margin-top:2px}
.vk-badge{font-size:10px;background:#1c2d4a;color:#79c0ff;border-radius:3px;padding:1px 4px;margin-top:2px;display:inline-block}

/* Coverage matrix */
table.coverage{border-collapse:collapse;width:100%;font-size:11px}
table.coverage th,table.coverage td{border:1px solid var(--border);padding:4px 6px;text-align:center}
table.coverage th{background:var(--bg3);color:var(--muted);font-size:10px;text-transform:uppercase;font-weight:600}
table.coverage td.host-cell{text-align:left;background:var(--bg3);font-weight:600}
.cov-2plus{background:var(--green-dim);color:var(--green)}
.cov-1{background:var(--yellow-dim);color:var(--yellow)}
.cov-0{background:var(--bg3);color:var(--muted)}
.cov-err{background:var(--red-dim);color:var(--red)}

/* Table */
.tbl-wrap{overflow-x:auto;margin-top:8px}
table.data-tbl{border-collapse:collapse;width:100%;font-size:12px}
table.data-tbl th{background:var(--bg3);border:1px solid var(--border);padding:6px 10px;cursor:pointer;white-space:nowrap;font-size:11px;text-transform:uppercase;color:var(--muted)}
table.data-tbl th:hover{color:var(--text)}
table.data-tbl td{border:1px solid var(--border);padding:5px 10px;white-space:nowrap}
table.data-tbl tr:hover td{background:var(--bg3)}
.verdict-comfortable{color:var(--green)}.verdict-borderline{color:var(--yellow)}.verdict-unsuitable{color:var(--red)}
.quant-fp16{color:var(--purple)}.quant-q8_0{color:#ffce6a}.quant-q5_1{color:var(--blue)}.quant-q5_0{color:var(--teal)}

footer{margin-top:48px;padding-top:18px;border-top:1px solid var(--border);color:var(--muted);font-size:12px}
.banner{background:var(--bg2);border-left:3px solid var(--blue);padding:10px 14px;margin-bottom:14px;font-size:13px;color:var(--muted)}
.banner strong{color:var(--text)}
</style>
</head>
<body>
<div class="page">
  <h1>Fono — Model Decision Page <span style="color:var(--muted);font-size:.7em">v3</span></h1>
  <p class="subtitle">
    Generated [[GENERATED_AT]] · [[NCELLS]] cells · [[NHOSTS]] hosts ·
    Higher RTF = faster (audio-s / wall-s). Comfortable: batch ≥ 2.0 AND stream ≥ 1.5.
  </p>

  <div class="banner">
    <strong>What's new in v3.</strong> Fixes silent data collapse in v1/v2 (where
    <code>.en</code> vs multilingual and <code>ac</code> vs <code>battery</code> cells
    overwrote each other), inverted RTF threshold annotations, and the inverted
    <code>cpu/vk</code> "speedup" ratio. All thresholds flow from a single
    <code>THRESH</code> table; the "key findings" section is computed from the data.
  </div>

  <!-- Sticky filter bar -->
  <div class="filter-bar" id="filter-bar">
    <label>Host
      <select id="f-host" multiple size="1" style="min-width:140px"></select>
    </label>
    <label>Power
      <select id="f-power"><option value="ac" selected>AC only</option><option value="battery">Battery only</option><option value="">Both</option></select>
    </label>
    <label>Backend
      <select id="f-build"><option value="">CPU + Vulkan</option><option value="cpu">CPU</option><option value="vulkan">Vulkan</option></select>
    </label>
    <label>Family
      <select id="f-family"><option value="">All</option><option>tiny</option><option>base</option><option>small</option><option>turbo</option></select>
    </label>
    <label>Language
      <select id="f-lang"><option value="">Both</option><option value="en">English (.en)</option><option value="multi">Multilingual</option></select>
    </label>
    <label>Quant
      <select id="f-quant"><option value="">All</option><option>fp16</option><option>q8_0</option><option>q5_1</option><option>q5_0</option></select>
    </label>
    <div class="presets">
      <button onclick="presetReset()">Reset</button>
      <button onclick="presetVNNI()">VNNI hosts</button>
      <button onclick="presetPreVNNI()">Pre-VNNI hosts</button>
      <button onclick="presetLive()">Live-viable</button>
    </div>
  </div>

  <!-- Findings (data-derived) -->
  <div class="findings" id="findings"><h3>Key findings (data-derived)</h3><div id="findings-body">…</div></div>

  <!-- 1. Verdict heatmap -->
  <h2>1 · Decision Heatmap — best CPU pick per (host, family, language)</h2>
  <p class="desc">
    Rows = hosts sorted by release year. Columns = model family × language.
    Each cell shows the best-scoring CPU build at AC power. ⚡ badge marks where
    the Vulkan backend improves the verdict for the same (host, family, language).
    Battery data hidden by default — toggle via the filter bar.
  </p>
  <div class="heatmap-wrap"><table class="heatmap" id="verdict-heatmap"></table></div>

  <!-- 2. Speed sweep (faceted small multiples per host) -->
  <h2>2 · Speed Sweep — batch RTF by model variant, faceted per host</h2>
  <p class="desc">
    One panel per host. Bars within a model variant show fp16 / q8_0 / q5_1
    (q5_0 only for <code>large-v3-turbo</code>). Empty slots = not measured.
    Y axis is log; horizontal lines mark batch RTF = 1.0 and 2.0.
  </p>
  <div id="speed-sweep"></div>

  <!-- 3. Quant speedup -->
  <h2>3 · Quant Speedup vs fp16 — VNNI vs avx2-fallback hosts</h2>
  <p class="desc">
    Ratio = <code>(quant batch RTF) ÷ (fp16 batch RTF)</code>. A value of 1.0 means
    no speedup; values &gt;1 mean the quantised model runs faster. Hosts with
    AVX-VNNI (Alder Lake+, Ryzen Zen3+) execute integer-quant kernels in vector
    units; pre-VNNI hosts fall back to scalar paths.
  </p>
  <div class="chart-box"><canvas id="quant-speedup" height="280"></canvas></div>

  <!-- 4. CPU vs Vulkan paired -->
  <h2>4 · CPU vs Vulkan — batch RTF and speedup</h2>
  <p class="desc">
    Paired bars per (host, model, quant) where both backends have data.
    Speedup = <code>Vulkan RTF ÷ CPU RTF</code> (&gt;1 means Vulkan faster).
  </p>
  <div class="chart-box"><canvas id="cpu-vulkan" height="320"></canvas></div>

  <!-- 5. Coverage matrix -->
  <h2>5 · Coverage Matrix — what's measured, what's missing</h2>
  <p class="desc">
    Rows = hosts. Columns = (backend × family × language × quant). Green ≥2
    iterations, yellow = 1, grey = not measured. Use this to plan the next
    bench session.
  </p>
  <div class="heatmap-wrap"><table class="coverage" id="coverage"></table></div>

  <!-- 6. Full table -->
  <h2>6 · Full Data Table</h2>
  <p class="desc">Click headers to sort. Filters above apply here too.</p>
  <div class="tbl-wrap">
    <table class="data-tbl" id="data-table">
      <thead><tr>
        <th data-col="host">Host</th><th data-col="power">Pwr</th>
        <th data-col="build">Build</th><th data-col="model_family">Fam</th>
        <th data-col="language">Lang</th><th data-col="model">Model</th>
        <th data-col="quantization">Quant</th>
        <th data-col="batch_rtf_median">Batch RTF</th>
        <th data-col="stream_rtf_median">Stream RTF</th>
        <th data-col="ttff_s_median">TTFF s</th>
        <th data-col="peak_rss_mib_median">RSS MiB</th>
        <th data-col="accuracy_en_mean">Acc (EN↓)</th>
        <th data-col="delta_en_mean">Δacc</th>
        <th data-col="approx_size_mib">Disk MiB</th>
        <th data-col="iterations_kept">Iters</th>
        <th data-col="verdict">Verdict</th>
      </tr></thead>
      <tbody id="table-body"></tbody>
    </table>
  </div>

  <footer>
    Fono benchmark · GPL-3.0-only · v3 supersedes
    <a href="calibration.html">calibration.html</a> and
    <a href="../../2026-05-19-perf-pass/summary/calibration2.html">calibration2.html</a>.
    Higher RTF = faster. All annotations sourced from <code>THRESH</code>:
    batch_comfort=[[BATCH_COMFORT]], stream_comfort=[[STREAM_COMFORT]].
  </footer>
</div>

<script>
// ─── Embedded data ─────────────────────────────────────────────────────────
const RAW_CELLS = [[CELLS_JSON]];
const HOST_META = [[HOST_META_JSON]];
const FINDINGS = [[FINDINGS_JSON]];
const THRESH = [[THRESH_JSON]];

// ─── Constants ─────────────────────────────────────────────────────────────
const FAMILY_ORDER = ['tiny','base','small','turbo'];
const FAMILY_LABELS = {tiny:'Tiny',base:'Base',small:'Small',turbo:'Turbo'};
const QUANT_ORDER  = ['fp16','q8_0','q5_1','q5_0'];
const QUANT_CLR    = {fp16:'#bc8cff',q8_0:'#ffce6a',q5_1:'#58a6ff',q5_0:'#00c3ad'};
const VERDICT_CLR  = {comfortable:'#3fb950',borderline:'#d29922',unsuitable:'#f85149'};
const FAMILY_CLR   = {tiny:'#bc8cff',base:'#58a6ff',small:'#3fb950',turbo:'#ff8c00'};

// Hosts sorted by release year ascending (default ordering)
const HOSTS_BY_YEAR = [...new Set(RAW_CELLS.map(c => c.host))].sort((a,b) => {
  const ya = (HOST_META[a]||{}).released || '9999';
  const yb = (HOST_META[b]||{}).released || '9999';
  return String(ya).localeCompare(String(yb));
});

const fmt1 = v => v == null ? '—' : Number(v).toFixed(1);
const fmt2 = v => v == null ? '—' : Number(v).toFixed(2);
const fmt3 = v => v == null ? '—' : Number(v).toFixed(3);
const fmtSign = v => v == null ? '—' : (v >= 0 ? '+' : '') + Number(v).toFixed(3);

const verdictScore = v => ({comfortable:3,borderline:2,unsuitable:1}[v]||0);

// Deterministic best-cell selector. Filters must produce a unique winner;
// when they don't, we log and pick by (ac > battery, iterations_kept desc).
function selectBest(cells, predicate, label) {
  const matches = cells.filter(predicate);
  if (matches.length === 0) return null;
  if (matches.length === 1) return matches[0];
  matches.sort((a,b) => {
    if (a.power !== b.power) return a.power === 'ac' ? -1 : 1;
    return (b.iterations_kept||0) - (a.iterations_kept||0);
  });
  if (label) console.debug(`selectBest[${label}]: ${matches.length} candidates, picked`, matches[0]);
  return matches[0];
}

// ─── Filter state (URL-hash backed) ────────────────────────────────────────
const FILTER_STATE = {hosts:[],power:'ac',build:'',family:'',language:'',quant:''};

function readHash() {
  try {
    const h = new URLSearchParams((location.hash||'').slice(1));
    if (h.has('hosts')) FILTER_STATE.hosts = h.get('hosts').split(',').filter(Boolean);
    if (h.has('power'))  FILTER_STATE.power  = h.get('power');
    if (h.has('build'))  FILTER_STATE.build  = h.get('build');
    if (h.has('family')) FILTER_STATE.family = h.get('family');
    if (h.has('lang'))   FILTER_STATE.language = h.get('lang');
    if (h.has('quant'))  FILTER_STATE.quant  = h.get('quant');
  } catch(e){}
}
function writeHash() {
  const p = new URLSearchParams();
  if (FILTER_STATE.hosts.length) p.set('hosts', FILTER_STATE.hosts.join(','));
  if (FILTER_STATE.power !== '')  p.set('power', FILTER_STATE.power);
  if (FILTER_STATE.build)  p.set('build', FILTER_STATE.build);
  if (FILTER_STATE.family) p.set('family', FILTER_STATE.family);
  if (FILTER_STATE.language) p.set('lang', FILTER_STATE.language);
  if (FILTER_STATE.quant)  p.set('quant', FILTER_STATE.quant);
  const s = p.toString();
  history.replaceState(null,'', s ? '#'+s : location.pathname);
}

function filteredCells() {
  return RAW_CELLS.filter(c =>
    (FILTER_STATE.hosts.length === 0 || FILTER_STATE.hosts.includes(c.host)) &&
    (FILTER_STATE.power === '' || c.power === FILTER_STATE.power) &&
    (FILTER_STATE.build === '' || c.build === FILTER_STATE.build) &&
    (FILTER_STATE.family === '' || c.model_family === FILTER_STATE.family) &&
    (FILTER_STATE.language === '' || c.language === FILTER_STATE.language) &&
    (FILTER_STATE.quant === '' || c.quantization === FILTER_STATE.quant)
  );
}

// ─── Filter bar UI ─────────────────────────────────────────────────────────
function initFilterBar() {
  const hostSel = document.getElementById('f-host');
  hostSel.innerHTML = HOSTS_BY_YEAR.map(h => {
    const m = HOST_META[h] || {};
    return `<option value="${h}">${h} (${m.released||'?'})</option>`;
  }).join('');
  hostSel.size = Math.min(HOSTS_BY_YEAR.length, 6);
  // Sync DOM → state
  function sync() {
    FILTER_STATE.hosts = [...hostSel.selectedOptions].map(o => o.value);
    FILTER_STATE.power = document.getElementById('f-power').value;
    FILTER_STATE.build = document.getElementById('f-build').value;
    FILTER_STATE.family = document.getElementById('f-family').value;
    FILTER_STATE.language = document.getElementById('f-lang').value;
    FILTER_STATE.quant = document.getElementById('f-quant').value;
    writeHash();
    renderAll();
  }
  // Sync state → DOM (on initial load from hash)
  function dump() {
    [...hostSel.options].forEach(o => o.selected = FILTER_STATE.hosts.includes(o.value));
    document.getElementById('f-power').value = FILTER_STATE.power;
    document.getElementById('f-build').value = FILTER_STATE.build;
    document.getElementById('f-family').value = FILTER_STATE.family;
    document.getElementById('f-lang').value = FILTER_STATE.language;
    document.getElementById('f-quant').value = FILTER_STATE.quant;
  }
  ['f-host','f-power','f-build','f-family','f-lang','f-quant'].forEach(id =>
    document.getElementById(id).addEventListener('change', sync));
  dump();
}

function presetReset() {
  Object.assign(FILTER_STATE, {hosts:[],power:'ac',build:'',family:'',language:'',quant:''});
  refilter();
}
function presetVNNI() {
  FILTER_STATE.hosts = HOSTS_BY_YEAR.filter(h => (HOST_META[h]||{}).quant_kernel === 'vnni');
  refilter();
}
function presetPreVNNI() {
  FILTER_STATE.hosts = HOSTS_BY_YEAR.filter(h => (HOST_META[h]||{}).quant_kernel !== 'vnni');
  refilter();
}
function presetLive() {
  FILTER_STATE.build = 'cpu'; FILTER_STATE.power = 'ac';
  refilter();
}
function refilter() {
  const hostSel = document.getElementById('f-host');
  [...hostSel.options].forEach(o => o.selected = FILTER_STATE.hosts.includes(o.value));
  document.getElementById('f-power').value = FILTER_STATE.power;
  document.getElementById('f-build').value = FILTER_STATE.build;
  document.getElementById('f-family').value = FILTER_STATE.family;
  document.getElementById('f-lang').value = FILTER_STATE.language;
  document.getElementById('f-quant').value = FILTER_STATE.quant;
  writeHash(); renderAll();
}

// ─── Findings ──────────────────────────────────────────────────────────────
function buildFindings() {
  const body = document.getElementById('findings-body');
  const items = [];
  if (FINDINGS.speedup_summary && FINDINGS.speedup_summary.length) {
    const grouped = {};
    FINDINGS.speedup_summary.forEach(s => {
      const k = `${s.kernel} · ${s.build}`;
      (grouped[k] = grouped[k] || []).push(s);
    });
    Object.entries(grouped).forEach(([k, arr]) => {
      const parts = arr.map(s => `<code>${s.quant}</code> median ${s.median}× (max ${s.max}×, n=${s.n})`).join(', ');
      items.push(`<li><strong>${k}</strong>: ${parts}</li>`);
    });
  }
  items.push(`<li><strong>Coverage gaps</strong>: ${FINDINGS.n_gaps} missing (host × backend × model × quant) entries — see Coverage Matrix below.</li>`);
  body.innerHTML = '<ul>'+items.join('')+'</ul>';
}

// ─── 1. Verdict heatmap ────────────────────────────────────────────────────
function buildVerdictHeatmap() {
  const tbl = document.getElementById('verdict-heatmap');
  const cells = filteredCells();
  const hosts = HOSTS_BY_YEAR.filter(h => cells.some(c => c.host === h));
  const families = FAMILY_ORDER.filter(f => cells.some(c => c.model_family === f));

  // Columns = family × language (skip .en for turbo)
  const cols = [];
  families.forEach(f => {
    if (f === 'turbo') cols.push({fam:f, lang:'multi'});
    else { cols.push({fam:f, lang:'multi'}); cols.push({fam:f, lang:'en'}); }
  });

  let h = '<thead><tr><th>Host</th>';
  cols.forEach(col => { h += `<th>${FAMILY_LABELS[col.fam]} · ${col.lang}</th>`; });
  h += '</tr></thead><tbody>';

  hosts.forEach(host => {
    const meta = HOST_META[host] || {};
    h += `<tr><td class="host-th"><div class="hw">${host}</div>`
       + `<div class="meta">${meta.released||'?'} · ${meta.cpu_model||''}</div>`
       + `<div class="${meta.quant_kernel==='vnni'?'kernel-vnni':'kernel-fallback'}">${meta.quant_kernel||'?'}</div>`
       + `</td>`;
    cols.forEach(col => {
      const cpu = bestCellFor(cells, host, 'cpu', col.fam, col.lang);
      const vk  = bestCellFor(cells, host, 'vulkan', col.fam, col.lang);
      if (!cpu) { h += '<td class="cell-na">—</td>'; return; }
      const v = cpu.verdict || 'unsuitable';
      const vkBetter = vk && verdictScore(vk.verdict) > verdictScore(v);
      h += `<td class="cell-${v}">`
         + `<div class="verdict-label ${v}">${v}</div>`
         + `<div class="cell-detail">${cpu.quantization} · ${cpu.model}</div>`
         + `<div class="cell-detail">batch ${fmt2(cpu.batch_rtf_median)}× · stream ${fmt2(cpu.stream_rtf_median)}× · ${Math.round(cpu.peak_rss_mib_median||0)} MiB</div>`
         + (vkBetter ? `<div class="vk-badge">⚡ Vulkan: ${vk.verdict}</div>` : '')
         + `</td>`;
    });
    h += '</tr>';
  });
  h += '</tbody>';
  tbl.innerHTML = h;
}

function bestCellFor(cells, host, build, family, language) {
  const matches = cells.filter(c =>
    c.host === host && c.build === build &&
    c.model_family === family && c.language === language
  );
  if (!matches.length) return null;
  // Score: verdict desc, accuracy_pass true, stream_rtf desc, rss asc
  matches.sort((a,b) =>
    verdictScore(b.verdict) - verdictScore(a.verdict) ||
    ((b.accuracy_pass===true ? 1 : 0) - (a.accuracy_pass===true ? 1 : 0)) ||
    (b.stream_rtf_median||0) - (a.stream_rtf_median||0) ||
    (a.peak_rss_mib_median||9999) - (b.peak_rss_mib_median||9999)
  );
  return matches[0];
}

// ─── 2. Speed sweep (faceted) ──────────────────────────────────────────────
const speedCharts = [];
function buildSpeedSweep() {
  const container = document.getElementById('speed-sweep');
  speedCharts.forEach(c => c.destroy());
  speedCharts.length = 0;
  container.innerHTML = '';

  const cells = filteredCells();
  const hosts = HOSTS_BY_YEAR.filter(h => cells.some(c => c.host === h));
  const build = FILTER_STATE.build || 'cpu';  // default cpu for the panel

  // Build per-host panels
  hosts.forEach(host => {
    const hostCells = cells.filter(c => c.host === host && c.build === build);
    if (!hostCells.length) return;

    // Build (family, language) groups as x-axis labels
    const variants = [];
    FAMILY_ORDER.forEach(fam => {
      const langs = fam === 'turbo' ? ['multi'] : ['multi','en'];
      langs.forEach(lang => {
        const has = hostCells.some(c => c.model_family === fam && c.language === lang);
        if (has) variants.push({fam, lang, label: `${fam}${lang==='en'?'.en':''}`});
      });
    });
    if (!variants.length) return;

    const QUANTS_FOR_FAM = {tiny:['fp16','q8_0','q5_1'],base:['fp16','q8_0','q5_1'],small:['fp16','q8_0','q5_1'],turbo:['fp16','q8_0','q5_0']};
    // For each quant: array of values per variant (null if n/a)
    const allQuants = ['fp16','q8_0','q5_1','q5_0'];
    const datasets = allQuants.map(q => ({
      label: q,
      data: variants.map(v => {
        if (!QUANTS_FOR_FAM[v.fam].includes(q)) return null;
        const c = selectBest(hostCells, x => x.model_family===v.fam && x.language===v.lang && x.quantization===q, `sweep ${host}/${v.fam}/${v.lang}/${q}`);
        return c ? c.batch_rtf_median : null;
      }),
      backgroundColor: QUANT_CLR[q]+'cc',
      borderColor: QUANT_CLR[q],
      borderWidth: 1,
    }));

    // Skip dataset if all-null
    const nonEmpty = datasets.filter(d => d.data.some(v => v != null));
    if (!nonEmpty.length) return;

    const box = document.createElement('div');
    box.className = 'chart-box';
    const meta = HOST_META[host] || {};
    box.innerHTML = `<h3>${host} · ${meta.released||'?'} · ${meta.quant_kernel||'?'} · ${build}</h3>`
                  + `<canvas height="220"></canvas>`;
    container.appendChild(box);
    const ctx = box.querySelector('canvas').getContext('2d');
    speedCharts.push(new Chart(ctx, {
      type: 'bar',
      data: { labels: variants.map(v => v.label), datasets: nonEmpty },
      options: {
        responsive: true,
        plugins: {
          legend: { position:'bottom', labels:{color:'#8b949e',boxWidth:12} },
          tooltip: { callbacks: { label: i => `${i.dataset.label}: ${fmt2(i.raw)}× batch RTF` } },
          annotation: {
            annotations: {
              ok:      { type:'line', yMin:THRESH.batch_ok,      yMax:THRESH.batch_ok,      borderColor:'#d2992288', borderWidth:1, borderDash:[4,4], label:{content:`batch=${THRESH.batch_ok} (keeps up)`, display:true, color:'#d29922', font:{size:9}} },
              comfort: { type:'line', yMin:THRESH.batch_comfort, yMax:THRESH.batch_comfort, borderColor:'#3fb95088', borderWidth:1, borderDash:[4,4], label:{content:`batch≥${THRESH.batch_comfort} (comfortable)`, display:true, color:'#3fb950', font:{size:9}} },
            }
          }
        },
        scales: {
          x: { ticks:{color:'#8b949e',font:{size:11}}, grid:{color:'#21262d'} },
          y: { type:'logarithmic', title:{display:true,text:'Batch RTF (higher = faster, log)',color:'#8b949e'}, ticks:{color:'#8b949e'}, grid:{color:'#21262d'} }
        }
      }
    }));
  });

  if (!container.children.length) container.innerHTML = '<p class="desc">No data matches current filters.</p>';
}

// ─── 3. Quant speedup (vs fp16) ────────────────────────────────────────────
let quantSpeedupChart = null;
function buildQuantSpeedup() {
  const cells = filteredCells();
  // For each (host, build, model_base): compute quant/fp16 ratio
  const groups = {};  // (host, build, model_base) → {fp16, q8_0, q5_1, q5_0}
  cells.forEach(c => {
    if (!c.batch_rtf_median) return;
    const k = `${c.host}||${c.build}||${c.model_base}`;
    if (!groups[k]) groups[k] = {host:c.host, build:c.build, base:c.model_base, family:c.model_family, kernel:(HOST_META[c.host]||{}).quant_kernel};
    if (groups[k][c.quantization] == null) groups[k][c.quantization] = c.batch_rtf_median;
  });

  // Build bar chart: x = (host/family/lang/build), bars per quant
  const entries = Object.values(groups).filter(g => g.fp16 != null);
  // Sort by kernel (vnni first), then host year, then family
  entries.sort((a,b) => {
    if ((a.kernel==='vnni') !== (b.kernel==='vnni')) return a.kernel==='vnni' ? -1 : 1;
    const ya = (HOST_META[a.host]||{}).released||'9999';
    const yb = (HOST_META[b.host]||{}).released||'9999';
    if (ya !== yb) return String(ya).localeCompare(String(yb));
    return a.base.localeCompare(b.base);
  });

  const labels = entries.map(g => `${g.host}/${g.build}/${g.base}`);
  const datasets = ['q8_0','q5_1','q5_0'].map(q => ({
    label: q,
    data: entries.map(g => g[q] != null ? +(g[q]/g.fp16).toFixed(3) : null),
    backgroundColor: QUANT_CLR[q]+'cc',
    borderColor: QUANT_CLR[q],
    borderWidth: 1,
  }));

  const ctx = document.getElementById('quant-speedup').getContext('2d');
  if (quantSpeedupChart) quantSpeedupChart.destroy();
  quantSpeedupChart = new Chart(ctx, {
    type: 'bar',
    data: { labels, datasets },
    options: {
      responsive: true,
      plugins: {
        legend: { position:'bottom', labels:{color:'#8b949e',boxWidth:12} },
        tooltip: { callbacks: { label: i => `${i.dataset.label}: ${fmt2(i.raw)}× vs fp16` } },
        annotation: {
          annotations: {
            unity: { type:'line', yMin:1, yMax:1, borderColor:'#8b949e88', borderWidth:1, borderDash:[4,4], label:{content:'1× (no speedup)',display:true,color:'#8b949e',font:{size:10}} }
          }
        }
      },
      scales: {
        x: { ticks:{color:'#8b949e',maxRotation:60,font:{size:10}}, grid:{color:'#21262d'} },
        y: { type:'logarithmic', title:{display:true,text:'Quant RTF ÷ fp16 RTF (higher = quant faster)',color:'#8b949e'}, ticks:{color:'#8b949e'}, grid:{color:'#21262d'} }
      }
    }
  });
}

// ─── 4. CPU vs Vulkan paired ───────────────────────────────────────────────
let cpuVkChart = null;
function buildCpuVulkan() {
  const cells = filteredCells();
  // Group by (host, model_base, quantization, language)
  const groups = {};
  cells.forEach(c => {
    if (!c.batch_rtf_median) return;
    const k = `${c.host}||${c.model_base}||${c.quantization}||${c.language}`;
    groups[k] = groups[k] || {host:c.host, base:c.model_base, quant:c.quantization, lang:c.language};
    groups[k][c.build] = c.batch_rtf_median;
  });
  const both = Object.values(groups).filter(g => g.cpu != null && g.vulkan != null);
  both.sort((a,b) => {
    const ya = (HOST_META[a.host]||{}).released||'9999';
    const yb = (HOST_META[b.host]||{}).released||'9999';
    if (ya !== yb) return String(ya).localeCompare(String(yb));
    return a.base.localeCompare(b.base);
  });

  const labels = both.map(g => `${g.host}/${g.base}${g.lang==='en'?'.en':''}/${g.quant}`);
  const cpuData = both.map(g => g.cpu);
  const vkData  = both.map(g => g.vulkan);
  const speedup = both.map(g => +(g.vulkan/g.cpu).toFixed(2));

  const ctx = document.getElementById('cpu-vulkan').getContext('2d');
  if (cpuVkChart) cpuVkChart.destroy();
  cpuVkChart = new Chart(ctx, {
    type: 'bar',
    data: {
      labels,
      datasets: [
        { label:'CPU batch RTF',    data:cpuData, backgroundColor:'#58a6ff99', borderColor:'#58a6ff', borderWidth:1 },
        { label:'Vulkan batch RTF', data:vkData,  backgroundColor:'#ff8c0099', borderColor:'#ff8c00', borderWidth:1 },
      ]
    },
    options: {
      responsive: true,
      plugins: {
        legend: { position:'bottom', labels:{color:'#8b949e',boxWidth:12} },
        tooltip: {
          callbacks: {
            afterBody: items => {
              const i = items[0].dataIndex;
              const s = speedup[i];
              return s != null ? `Vulkan speedup = Vulkan ÷ CPU = ${s}× ${s>=1?'(Vulkan faster)':'(CPU faster)'}` : '';
            }
          }
        }
      },
      scales: {
        x: { ticks:{color:'#8b949e',maxRotation:60,font:{size:9}}, grid:{color:'#21262d'} },
        y: { type:'logarithmic', title:{display:true,text:'Batch RTF (higher = faster, log)',color:'#8b949e'}, ticks:{color:'#8b949e'}, grid:{color:'#21262d'} }
      }
    }
  });
}

// ─── 5. Coverage matrix ────────────────────────────────────────────────────
function buildCoverage() {
  const tbl = document.getElementById('coverage');
  const hosts = HOSTS_BY_YEAR;
  const FAMS = ['tiny','base','small','turbo'];
  const cols = [];
  ['cpu','vulkan'].forEach(build => {
    FAMS.forEach(fam => {
      const langs = fam === 'turbo' ? ['multi'] : ['multi','en'];
      const quants = fam === 'turbo' ? ['fp16','q8_0','q5_0'] : ['fp16','q8_0','q5_1'];
      langs.forEach(lang => quants.forEach(q => cols.push({build, fam, lang, quant:q})));
    });
  });

  let h = '<thead><tr><th>Host</th>';
  let lastBuild = '';
  cols.forEach(c => {
    h += `<th>${c.build===lastBuild?'':c.build+'<br>'}${c.fam}${c.lang==='en'?'.en':''}<br>${c.quant}</th>`;
    lastBuild = c.build;
  });
  h += '</tr></thead><tbody>';

  hosts.forEach(host => {
    h += `<tr><td class="host-cell">${host}</td>`;
    cols.forEach(col => {
      // Find AC cell matching
      const matches = RAW_CELLS.filter(c =>
        c.host === host && c.build === col.build &&
        c.power === 'ac' &&
        c.model_family === col.fam && c.language === col.lang && c.quantization === col.quant
      );
      if (!matches.length) { h += '<td class="cov-0">·</td>'; return; }
      const m = matches[0];
      const iters = m.iterations_kept || 0;
      const cls = iters >= 2 ? 'cov-2plus' : iters === 1 ? 'cov-1' : 'cov-0';
      const errs = (m.errors||[]).length > 0;
      h += `<td class="${errs?'cov-err':cls}" title="${host}/${col.build}/${col.fam}/${col.lang}/${col.quant}: ${iters} iter${iters===1?'':'s'}${errs?' (errors!)':''}">${errs?'!':iters}</td>`;
    });
    h += '</tr>';
  });
  h += '</tbody>';
  tbl.innerHTML = h;
}

// ─── 6. Table ──────────────────────────────────────────────────────────────
let sortCol = 'host', sortDir = 1;
function buildTable() {
  let rows = filteredCells();
  rows.sort((a,b) => {
    const av = a[sortCol], bv = b[sortCol];
    if (av == null && bv == null) return 0;
    if (av == null) return 1;
    if (bv == null) return -1;
    if (typeof av === 'string') return av.localeCompare(bv) * sortDir;
    return (av - bv) * sortDir;
  });
  const tbody = document.getElementById('table-body');
  tbody.innerHTML = rows.map(c => {
    const q = c.quantization || 'fp16';
    const v = c.verdict || 'unsuitable';
    return `<tr>
      <td>${c.host}</td><td>${c.power}</td><td>${c.build}</td>
      <td>${c.model_family}</td><td>${c.language}</td>
      <td>${c.model}</td><td class="quant-${q}">${q}</td>
      <td>${fmt2(c.batch_rtf_median)}</td><td>${fmt2(c.stream_rtf_median)}</td>
      <td>${fmt1(c.ttff_s_median)}</td>
      <td>${c.peak_rss_mib_median?Math.round(c.peak_rss_mib_median):'—'}</td>
      <td>${fmt3(c.accuracy_en_mean)}</td>
      <td>${c.delta_en_mean!=null?fmtSign(c.delta_en_mean):'—'}</td>
      <td>${c.approx_size_mib||'—'}</td>
      <td>${c.iterations_kept||0}/${c.iterations_total||0}</td>
      <td class="verdict-${v}">${v}</td>
    </tr>`;
  }).join('') || '<tr><td colspan="16" style="text-align:center;color:var(--muted)">No data</td></tr>';
}

// ─── Render all (called by filter changes) ────────────────────────────────
function renderAll() {
  buildVerdictHeatmap();
  buildSpeedSweep();
  buildQuantSpeedup();
  buildCpuVulkan();
  buildCoverage();
  buildTable();
}

// ─── Boot ──────────────────────────────────────────────────────────────────
function waitForChart(tries) {
  if (typeof Chart !== 'undefined') {
    readHash();
    initFilterBar();
    buildFindings();
    renderAll();
    document.querySelectorAll('#data-table th[data-col]').forEach(th => {
      th.addEventListener('click', () => {
        const col = th.dataset.col;
        sortDir = (col === sortCol) ? -sortDir : 1;
        sortCol = col;
        buildTable();
      });
    });
    // Sanity assertion
    console.assert(RAW_CELLS.every(c => c.host && c.power && c.build && c.model),
      'cell missing required key');
  } else if (tries < 30) {
    setTimeout(() => waitForChart(tries+1), 200);
  } else {
    console.error('Chart.js failed to load from CDN');
  }
}
document.addEventListener('DOMContentLoaded', () => waitForChart(0));
</script>
</body>
</html>
"""

# ───────────────────── HTML emission ────────────────────────────────────────

JS_FIELDS = [
    "host", "power", "build", "model",
    "model_family", "model_base", "language", "quantization",
    "batch_rtf_median", "stream_rtf_median", "ttff_s_median",
    "peak_rss_mib_median", "wall_clock_s_median",
    "verdict", "iterations_kept", "iterations_total", "errors",
    "approx_size_mib",
    "accuracy_en_mean", "accuracy_en_max", "accuracy_all_mean",
    "delta_en_mean", "delta_en_max", "accuracy_pass",
]


def generate_html(cells: list[dict], inventory: dict, findings: dict) -> str:
    generated = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")

    host_meta: dict[str, dict] = {}
    for host, inv in inventory.items():
        kernel = "vnni" if inv.get("has_avx_vnni") or inv.get("has_avx512_vnni") else "avx2-fallback"
        host_meta[host] = {
            "cpu_model":    inv.get("cpu_model"),
            "released":     inv.get("released"),
            "cores":        inv.get("physical_cores"),
            "quant_kernel": kernel,
            "has_avx_vnni": bool(inv.get("has_avx_vnni", False)),
            "gpu": (inv.get("gpu") or [""])[0][:80] if inv.get("gpu") else None,
        }

    slim = [{k: c.get(k) for k in JS_FIELDS} for c in cells]
    hosts = {c["host"] for c in cells}

    return (
        HTML_TEMPLATE
        .replace("[[GENERATED_AT]]", html.escape(generated))
        .replace("[[NCELLS]]", str(len(cells)))
        .replace("[[NHOSTS]]", str(len(hosts)))
        .replace("[[BATCH_COMFORT]]", str(THRESH["batch_comfort"]))
        .replace("[[STREAM_COMFORT]]", str(THRESH["stream_comfort"]))
        .replace("[[CELLS_JSON]]", json.dumps(slim, separators=(",", ":")))
        .replace("[[HOST_META_JSON]]", json.dumps(host_meta, separators=(",", ":")))
        .replace("[[FINDINGS_JSON]]", json.dumps(findings, separators=(",", ":")))
        .replace("[[THRESH_JSON]]", json.dumps(THRESH, separators=(",", ":")))
    )


# ───────────────────── main ─────────────────────────────────────────────────

def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--matrix",    default="docs/bench/calibration/summary/matrix.json")
    ap.add_argument("--runs",      default="docs/bench/calibration/runs")
    ap.add_argument("--inventory", default="docs/bench/calibration/inventory")
    ap.add_argument("--out",       default="docs/bench/calibration/summary/calibration3.html")
    ap.add_argument("--validate-only", action="store_true",
                    help="Validate matrix without writing HTML; exit non-zero on schema errors")
    args = ap.parse_args()

    print(f"Loading matrix {args.matrix} ...")
    cells = load_matrix(args.matrix)
    print(f"  {len(cells)} cells")

    errors = validate(cells)
    if errors:
        print(f"VALIDATION FAILED ({len(errors)} errors):", file=sys.stderr)
        for e in errors[:20]:
            print(f"  {e}", file=sys.stderr)
        return 1
    print("  validation OK — all keys unique on (host,power,build,model)")

    if args.validate_only:
        print("validate-only mode: skipping HTML generation")
        return 0

    print(f"Loading inventory from {args.inventory} ...")
    inventory = load_inventory(Path(args.inventory))
    print(f"  {len(inventory)} hosts")

    print(f"Loading accuracy from {args.runs} ...")
    accuracy = load_accuracy(Path(args.runs))
    print(f"  {len(accuracy)} accuracy entries")

    print("Computing accuracy deltas vs fp16 ...")
    deltas = compute_deltas(accuracy)
    print(f"  {len(deltas)} delta entries")

    print("Enriching cells ...")
    enriched = [enrich_cell(c, accuracy, deltas, inventory) for c in cells]

    print("Deriving findings ...")
    findings = derive_findings(enriched)
    print(f"  {len(findings['speedup_summary'])} speedup buckets · {findings['n_gaps']} coverage gaps")

    print("Generating HTML ...")
    page = generate_html(enriched, inventory, findings)

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(page, encoding="utf-8")
    print(f"Wrote {out}  ({len(page):,} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())


