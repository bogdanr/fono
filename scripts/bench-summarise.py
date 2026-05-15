#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Reduce per-iteration equivalence runs to the Phase-0 summary matrix.

Reads every `*__iter<N>.json` under `RUNS_DIR` plus its sibling
`*__iter<N>.time.json` rusage sidecar, groups them by
`(host, power, build, model)`, and emits:

* `summary/matrix.json` — machine-readable, one record per cell.
* `summary/matrix.md`   — human-readable grouped table.

Each cell record carries:

    host, power, build, model, iterations_kept, iterations_total,
    batch_rtf_median, batch_rtf_stddev_pct,
    stream_rtf_median, stream_rtf_stddev_pct,
    ttff_s_median, ttff_s_stddev_pct,
    peak_rss_mib_median, peak_rss_mib_worst,
    wall_clock_s_median, total_audio_s,
    fixtures_processed (excludes SKIP/error),
    verdict ∈ {comfortable, borderline, unsuitable, errored, insufficient_data},
    notes (list of strings: e.g. "spread>15% on stream_rtf",
                                 "RSS exceeds 90% of host RAM").

The verdict mirrors plan Task 0.5: `comfortable` if median batch RTF ≥ 2.0
and median streaming RTF ≥ 1.5; `borderline` if batch RTF ≥ 1.0 but
streaming < 1.5; `unsuitable` if batch RTF < 1.0 or peak RSS > 90 % of
host RAM. RTF = audio_seconds_processed / wall_clock_seconds.

Cells whose stddev exceeds 15 % of the median on any RTF series are
flagged in `notes` but not auto-rerun by this script — operator decides
whether to re-bench based on the summary.

Usage:
    python3 scripts/bench-summarise.py \\
        --runs docs/bench/calibration/runs \\
        --inventory docs/bench/calibration/inventory \\
        --out-json docs/bench/calibration/summary/matrix.json \\
        --out-md   docs/bench/calibration/summary/matrix.md
"""

import argparse
import json
import math
import pathlib
import re
import statistics
import sys
from collections import defaultdict

CELL_RE = re.compile(
    r"^(?P<host>[^/]+?)__(?P<power>[^_]+)__(?P<build>[^_]+)__"
    r"(?P<model>.+)__iter(?P<iter>\d+)\.json$"
)


def _load_inventory(inv_dir: pathlib.Path) -> dict[str, dict]:
    out = {}
    for p in sorted(inv_dir.glob("*.json")):
        try:
            out[p.stem] = json.loads(p.read_text())
        except Exception as e:
            print(f"WARN: failed to parse {p}: {e}", file=sys.stderr)
    return out


def _stddev_pct(values: list[float]) -> float | None:
    if len(values) < 2:
        return None
    med = statistics.median(values)
    if med == 0:
        return None
    sd = statistics.stdev(values)
    return round(sd / abs(med) * 100.0, 2)


def _median(values: list[float]) -> float | None:
    return round(statistics.median(values), 4) if values else None


def _cell_metrics(run_paths: list[pathlib.Path]) -> dict:
    """Aggregate one cell from N iteration JSONs + sidecars."""
    batch_rtfs: list[float] = []
    stream_rtfs: list[float] = []
    ttffs: list[float] = []
    peak_rss_kibs: list[int] = []
    walls: list[float] = []
    total_audio: list[float] = []
    fixtures_each: list[int] = []
    errors: list[str] = []
    for json_path in run_paths:
        try:
            data = json.loads(json_path.read_text())
        except Exception as e:
            errors.append(f"{json_path.name}: parse: {e}")
            continue
        audio = 0.0
        batch_ms = 0
        stream_ms = 0
        ttff_ms_sum = 0
        ttff_n = 0
        fcount = 0
        for r in data.get("results", []):
            if r.get("verdict") == "Skipped":
                continue
            d = float(r.get("duration_s", 0.0) or 0.0)
            modes = r.get("modes", {})
            batch = modes.get("batch", {})
            elapsed = int(batch.get("elapsed_ms", 0) or 0)
            if elapsed <= 0:
                continue
            audio += d
            batch_ms += elapsed
            fcount += 1
            sm = modes.get("streaming") or {}
            sms = int(sm.get("elapsed_ms", 0) or 0)
            if sms > 0:
                stream_ms += sms
            tms = int(sm.get("ttff_ms", 0) or 0)
            if tms > 0:
                ttff_ms_sum += tms
                ttff_n += 1
        total_audio.append(audio)
        fixtures_each.append(fcount)
        if batch_ms > 0:
            batch_rtfs.append(audio / (batch_ms / 1000.0))
        if stream_ms > 0:
            stream_rtfs.append(audio / (stream_ms / 1000.0))
        if ttff_n > 0:
            ttffs.append((ttff_ms_sum / ttff_n) / 1000.0)
        # rusage sidecar
        side = json_path.with_suffix(".time.json")
        if not side.exists():
            # iter1.time.json suffix swap doesn't include "iter1" — naming is
            # `<stem>__iter1.json` and `<stem>__iter1.time.json`. with_suffix
            # only replaces `.json`, which becomes `.time.json`. Good.
            side = json_path.with_name(
                json_path.name.replace(".json", ".time.json")
            )
        if side.exists():
            try:
                rs = json.loads(side.read_text())
                rss = int(rs.get("max_rss_kib", 0) or 0)
                if rss > 0:
                    peak_rss_kibs.append(rss)
                w = float(rs.get("wall_clock_s", 0.0) or 0.0)
                if w > 0:
                    walls.append(w)
            except Exception as e:
                errors.append(f"{side.name}: parse rusage: {e}")
    return {
        "iterations_total": len(run_paths),
        "iterations_kept": sum(1 for f in fixtures_each if f > 0),
        "fixtures_processed_median": _median([float(x) for x in fixtures_each]),
        "total_audio_s_median": _median(total_audio),
        "batch_rtf_median": _median(batch_rtfs),
        "batch_rtf_stddev_pct": _stddev_pct(batch_rtfs),
        "stream_rtf_median": _median(stream_rtfs),
        "stream_rtf_stddev_pct": _stddev_pct(stream_rtfs),
        "ttff_s_median": _median(ttffs),
        "ttff_s_stddev_pct": _stddev_pct(ttffs),
        "peak_rss_mib_median": (
            round(statistics.median(peak_rss_kibs) / 1024.0, 1)
            if peak_rss_kibs
            else None
        ),
        "peak_rss_mib_worst": (
            round(max(peak_rss_kibs) / 1024.0, 1) if peak_rss_kibs else None
        ),
        "wall_clock_s_median": _median(walls),
        "errors": errors,
    }


def _verdict(cell: dict, host_ram_mib: float | None) -> tuple[str, list[str]]:
    notes: list[str] = []
    b = cell["batch_rtf_median"]
    s = cell["stream_rtf_median"]
    rss = cell["peak_rss_mib_worst"]
    if cell["iterations_kept"] == 0:
        return "errored", ["no successful iterations"]
    if b is None:
        return "errored", ["no batch latency captured"]
    if host_ram_mib and rss and rss > 0.9 * host_ram_mib:
        notes.append(
            f"peak RSS {rss:.0f} MiB > 90% of host {host_ram_mib:.0f} MiB"
        )
    for label, sd in (
        ("batch_rtf", cell["batch_rtf_stddev_pct"]),
        ("stream_rtf", cell["stream_rtf_stddev_pct"]),
    ):
        if sd is not None and sd > 15.0:
            notes.append(f"{label} spread {sd:.1f}% > 15%")
    if b < 1.0 or (host_ram_mib and rss and rss > 0.9 * host_ram_mib):
        return "unsuitable", notes
    if b >= 2.0 and (s is not None and s >= 1.5):
        return "comfortable", notes
    return "borderline", notes


def _markdown(cells: list[dict], inv: dict[str, dict]) -> str:
    out = ["# Phase 0 calibration matrix", ""]
    out.append(
        "Each row aggregates 3 iterations (medians; spread = stddev/median × 100%)."
    )
    out.append("")
    out.append(
        "RTF = (audio seconds processed) / (wall clock seconds). "
        "Higher = faster than realtime."
    )
    out.append("")
    out.append(
        "Verdict: `comfortable` (batch ≥ 2.0 AND stream ≥ 1.5); "
        "`borderline` (batch ≥ 1.0); `unsuitable` (batch < 1.0 OR RSS > 90% host RAM)."
    )
    out.append("")
    # Group by host
    by_host: dict[str, list[dict]] = defaultdict(list)
    for c in cells:
        by_host[c["host"]].append(c)
    for host in sorted(by_host):
        meta = inv.get(host, {})
        cpu = meta.get("cpu_model", "?")
        cores = "{}p/{}l".format(
            meta.get("physical_cores", "?"), meta.get("logical_cores", "?")
        )
        ram = meta.get("mem_total_mib", "?")
        chassis = meta.get("chassis", "?")
        released = meta.get("released")
        tier = meta.get("cpu_tier")
        summary = meta.get("cpu_summary")
        header_extra = ""
        if released or tier:
            bits = []
            if released:
                bits.append(f"released {released}")
            if tier:
                bits.append(tier)
            header_extra = " — " + ", ".join(bits)
        out.append(f"## {host} — {cpu} ({cores}, {ram} MiB, {chassis}){header_extra}")
        if summary:
            out.append("")
            out.append(f"_{summary}_")
        out.append("")
        out.append(
            "| model | power | build | iters | batch RTF | b-σ% | stream RTF | "
            "s-σ% | TTFF s | RSS MiB | verdict | notes |"
        )
        out.append(
            "|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|"
        )
        rows = by_host[host]
        rows.sort(key=lambda r: (r["power"], r["build"], r["model"]))
        for c in rows:
            def fmt(v, n=2):
                return "—" if v is None else f"{v:.{n}f}"
            out.append(
                "| {model} | {power} | {build} | {ik}/{it} | {b} | {bp} | "
                "{s} | {sp} | {t} | {r} | {v} | {notes} |".format(
                    model=c["model"],
                    power=c["power"],
                    build=c["build"],
                    ik=c["iterations_kept"],
                    it=c["iterations_total"],
                    b=fmt(c["batch_rtf_median"], 2),
                    bp=fmt(c["batch_rtf_stddev_pct"], 1),
                    s=fmt(c["stream_rtf_median"], 2),
                    sp=fmt(c["stream_rtf_stddev_pct"], 1),
                    t=fmt(c["ttff_s_median"], 2),
                    r=fmt(c["peak_rss_mib_worst"], 0),
                    v=c["verdict"],
                    notes="; ".join(c["notes"]) if c["notes"] else "",
                )
            )
        out.append("")
    return "\n".join(out)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", required=True)
    ap.add_argument("--inventory", required=True)
    ap.add_argument("--out-json", required=True)
    ap.add_argument("--out-md", required=True)
    args = ap.parse_args()

    runs_dir = pathlib.Path(args.runs)
    inv = _load_inventory(pathlib.Path(args.inventory))

    grouped: dict[tuple[str, str, str, str], list[pathlib.Path]] = defaultdict(list)
    for p in sorted(runs_dir.glob("*.json")):
        if p.name.endswith(".time.json"):
            continue
        m = CELL_RE.match(p.name)
        if not m:
            print(f"skip (no match): {p.name}", file=sys.stderr)
            continue
        key = (m.group("host"), m.group("power"), m.group("build"), m.group("model"))
        grouped[key].append(p)

    cells: list[dict] = []
    for (host, power, build, model), paths in sorted(grouped.items()):
        paths.sort(key=lambda q: int(CELL_RE.match(q.name).group("iter")))
        agg = _cell_metrics(paths)
        host_ram = None
        if host in inv:
            try:
                host_ram = float(inv[host].get("mem_total_mib", 0) or 0) or None
            except Exception:
                pass
        verdict, notes = _verdict(agg, host_ram)
        cells.append(
            {
                "host": host,
                "power": power,
                "build": build,
                "model": model,
                **agg,
                "verdict": verdict,
                "notes": notes,
            }
        )

    out_json = pathlib.Path(args.out_json)
    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(
        json.dumps({"cells": cells}, indent=2, sort_keys=True) + "\n"
    )
    md = _markdown(cells, inv)
    pathlib.Path(args.out_md).write_text(md + "\n")
    print(f"wrote {out_json} ({len(cells)} cells)")
    print(f"wrote {args.out_md}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
