#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Aggregate per-fixture stt_accuracy_levenshtein across equivalence
runs to compare quality across (model, quantization) cells.

Reads docs/bench/.../runs/<host>__<power>__<build>__<model>__iter<N>.json
files, groups by (host, build, base_model, quant) and reports:

  - Mean and max per-fixture accuracy (lower = closer to reference text;
    proxy for WER).
  - Verdict pass/fail counts.
  - Δ vs the fp16 baseline (same host, build, base_model).
  - **English-only** mean / max / Δ for multilingual models, so the
    acceptance rule applied at the registry level is not diluted by
    non-Latin fixtures that sit at the model's quality floor regardless
    of quantization. The `base` family was originally defaulted to
    `q8_0` because its all-language mean Δ looked acceptable, but the
    English-only split showed +12.8 pp mean Δ and +40 pp max — exactly
    the kind of regression this split now surfaces in one place.

`base_model` and `quant` are split from the trailing `-q5_1`, `-q5_0`,
`-q8_0` suffix; everything else (e.g. `small.en`) is treated as fp16.

Usage:
  python3 scripts/bench-accuracy.py \
      --runs docs/bench/2026-05-19-perf-pass/runs \
      --out  docs/bench/2026-05-19-perf-pass/summary/accuracy.md
"""
from __future__ import annotations

import argparse
import json
import re
import statistics as stats
from collections import defaultdict
from pathlib import Path

QUANT_RE = re.compile(r"^(.*?)(?:-(q5_0|q5_1|q8_0))?$")
FILE_RE = re.compile(r"^(?P<host>[^_]+)__(?P<power>[^_]+)__(?P<build>[^_]+)__(?P<model>.+?)__iter\d+\.json$")


def parse_filename(name: str):
    m = FILE_RE.match(name)
    if not m or name.endswith(".time.json"):
        return None
    model = m.group("model")
    qm = QUANT_RE.match(model)
    base = qm.group(1)
    quant = qm.group(2) or "fp16"
    return {
        "host": m.group("host"),
        "power": m.group("power"),
        "build": m.group("build"),
        "base_model": base,
        "quant": quant,
        "full_model": model,
    }


def load_run(path: Path):
    try:
        return json.loads(path.read_text())
    except Exception:
        return None


def collect(runs_dir: Path):
    cells = defaultdict(list)
    for f in sorted(runs_dir.iterdir()):
        if f.suffix != ".json" or f.name.endswith(".time.json"):
            continue
        meta = parse_filename(f.name)
        if not meta:
            continue
        d = load_run(f)
        if not d or "results" not in d:
            continue
        key = (meta["host"], meta["build"], meta["base_model"], meta["quant"])
        for r in d["results"]:
            if r.get("synthetic_placeholder"):
                continue
            acc = r.get("metrics", {}).get("stt_accuracy_levenshtein")
            verdict = r.get("verdict")
            if acc is None:
                continue
            cells[key].append({
                "fixture": r.get("fixture"),
                "language": r.get("language") or "",
                "acc": acc,
                "verdict": verdict,
            })
    return cells


def _stats(accs):
    if not accs:
        return None
    return {
        "n": len(accs),
        "mean": stats.mean(accs),
        "median": stats.median(accs),
        "max": max(accs),
        "p90": stats.quantiles(accs, n=10)[8] if len(accs) >= 10 else max(accs),
    }


def summarise(cells):
    out = {}
    for key, rows in cells.items():
        all_accs = [r["acc"] for r in rows]
        en_accs = [r["acc"] for r in rows if r["language"].lower().startswith("en")]
        verdicts = [r["verdict"] for r in rows]
        out[key] = {
            "all": _stats(all_accs),
            "en": _stats(en_accs),
            "pass": sum(1 for v in verdicts if v == "pass"),
            "fail": sum(1 for v in verdicts if v == "fail"),
            "skip": sum(1 for v in verdicts if v == "skip"),
        }
    return out


def _delta(cur, ref):
    if cur is None or ref is None:
        return None
    return cur - ref


def _fmt(x, prec=4, signed=False):
    if x is None:
        return "—"
    if signed:
        return f"{x:+.{prec}f}"
    return f"{x:.{prec}f}"


def render(summary):
    """Render markdown grouped by (host, build, base_model).

    For multilingual models (base name without `.en`), the English-only
    columns drive the acceptance rule — they are the signal we use to
    select a default quantization. The "all-language" mean is kept as
    context only.
    """
    groups = defaultdict(dict)
    for (host, build, base, quant), s in summary.items():
        groups[(host, build, base)][quant] = s

    lines = []
    lines.append("# Quantization accuracy comparison")
    lines.append("")
    lines.append("Lower mean accuracy (normalised Levenshtein distance to reference text) = better.")
    lines.append("Δ = (quant − fp16) in absolute points; positive Δ means quantization degraded quality.")
    lines.append("")
    lines.append("**English-only** columns (`en_*`) are the gate for multilingual models. "
                 "Non-English fixtures often sit at the model's quality floor where "
                 "quantization noise is masked; English-only Δ is the signal that "
                 "drives `default_quantization` in the registry.")
    lines.append("")

    for (host, build, base) in sorted(groups):
        quants = groups[(host, build, base)]
        if "fp16" not in quants:
            continue
        is_multilingual = not base.endswith(".en")
        lines.append(f"## {host} / {build} / {base}")
        lines.append("")
        header = (
            "| quant | n | all_mean | all_max | Δ_all_mean | "
            "en_n | en_mean | en_max | Δ_en_mean | Δ_en_max | pass | fail |"
        )
        sep = "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
        lines.append(header)
        lines.append(sep)
        fp = quants["fp16"]
        fp_all_mean = fp["all"]["mean"] if fp["all"] else None
        fp_en_mean = fp["en"]["mean"] if fp["en"] else None
        fp_en_max = fp["en"]["max"] if fp["en"] else None
        order = [q for q in ["fp16", "q8_0", "q5_1", "q5_0"] if q in quants]
        for q in order:
            s = quants[q]
            a = s["all"] or {"n": 0, "mean": None, "max": None}
            e = s["en"] or {"n": 0, "mean": None, "max": None}
            d_all = _delta(a["mean"], fp_all_mean)
            d_en_m = _delta(e["mean"], fp_en_mean)
            d_en_x = _delta(e["max"], fp_en_max)
            lines.append(
                f"| {q} | {a['n']} | {_fmt(a['mean'])} | {_fmt(a['max'])} | {_fmt(d_all, signed=True)} | "
                f"{e['n']} | {_fmt(e['mean'])} | {_fmt(e['max'])} | "
                f"{_fmt(d_en_m, signed=True)} | {_fmt(d_en_x, signed=True)} | "
                f"{s['pass']} | {s['fail']} |"
            )
        lines.append("")
        # Acceptance-rule verdict (English-only Δ ≤ +0.05 mean AND ≤ +0.20 max).
        if is_multilingual or base.endswith(".en"):
            lines.append("Acceptance rule (registry default candidate): "
                         "English-only mean Δ ≤ +0.05 AND max Δ ≤ +0.20 vs fp16.")
            for q in order:
                if q == "fp16":
                    continue
                e = quants[q]["en"]
                if not e or fp_en_mean is None or fp_en_max is None:
                    continue
                d_m = e["mean"] - fp_en_mean
                d_x = e["max"] - fp_en_max
                ok_m = d_m <= 0.05
                ok_x = d_x <= 0.20
                badge = "PASS" if (ok_m and ok_x) else "FAIL"
                lines.append(f"- `{q}`: Δ_en_mean = {d_m:+.4f} ({'≤' if ok_m else '>'} +0.05), "
                             f"Δ_en_max = {d_x:+.4f} ({'≤' if ok_x else '>'} +0.20) — {badge}")
            lines.append("")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    args = ap.parse_args()

    cells = collect(args.runs)
    if not cells:
        raise SystemExit(f"no runs found under {args.runs}")
    summary = summarise(cells)
    md = render(summary)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(md)
    print(f"wrote {args.out} ({len(summary)} cells)")


if __name__ == "__main__":
    main()
