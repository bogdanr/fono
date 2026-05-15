#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Wrap a child command with resource.getrusage() accounting.

This is a portable replacement for `/usr/bin/time -v` for hosts that ship
without GNU time (LXC images, minimal Slackware/NimbleX rootfs, etc.).
The output schema mirrors the subset of `time -v` fields the Phase 0
calibration matrix consumes, plus host context fields captured at the
moment the wrapper starts.

Usage:
    bench-with-rusage.py --sidecar PATH -- <command> [args...]

The wrapper:

* execs the child via subprocess.run(),
* reads `RUSAGE_CHILDREN` *before and after* (delta = this child only —
  important when the wrapper is invoked back-to-back in a loop),
* normalises `ru_maxrss` to KiB (Linux returns KiB; macOS returns bytes),
* records AC online state, battery percentage, power profile, package
  temperature, governor, and a UTC timestamp,
* writes a single JSON file to `--sidecar`.

The child's stdout / stderr are not captured — they stream through so the
caller sees the bench's normal output in real time. Exit code mirrors the
child's exit code.
"""

import argparse
import json
import os
import pathlib
import resource
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone


def _read_first_line(path: str) -> str | None:
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            return f.readline().strip()
    except OSError:
        return None


def _read_int(path: str) -> int | None:
    raw = _read_first_line(path)
    if raw is None:
        return None
    try:
        return int(raw)
    except ValueError:
        return None


def _ac_online() -> str | None:
    """Return "1" / "0" / None. Picks the first AC* supply on Linux."""
    base = pathlib.Path("/sys/class/power_supply")
    if not base.is_dir():
        return None
    for entry in sorted(base.iterdir()):
        name = entry.name
        if name.startswith("AC") or name.startswith("ADP") or name == "ACAD":
            val = _read_first_line(str(entry / "online"))
            if val is not None:
                return val
    return None


def _battery_pct() -> int | None:
    base = pathlib.Path("/sys/class/power_supply")
    if not base.is_dir():
        return None
    for entry in sorted(base.iterdir()):
        if entry.name.startswith("BAT"):
            cap = _read_int(str(entry / "capacity"))
            if cap is not None:
                return cap
    return None


def _power_profile() -> str | None:
    # Try, in order: powerprofilesctl, tuned-adm, tlp-stat, /sys cpufreq.
    if shutil.which("powerprofilesctl"):
        try:
            return subprocess.check_output(
                ["powerprofilesctl", "get"],
                stderr=subprocess.DEVNULL,
                timeout=2,
                text=True,
            ).strip()
        except Exception:
            pass
    if shutil.which("tuned-adm"):
        try:
            out = subprocess.check_output(
                ["tuned-adm", "active"],
                stderr=subprocess.DEVNULL,
                timeout=2,
                text=True,
            ).strip()
            return out.split(":", 1)[-1].strip() if ":" in out else out
        except Exception:
            pass
    gov = _read_first_line("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
    if gov:
        return f"governor={gov}"
    return None


def _pkg_temp_c() -> float | None:
    """Look for an x86 package or k10temp Tctl sensor under /sys/class/hwmon."""
    base = pathlib.Path("/sys/class/hwmon")
    if not base.is_dir():
        return None
    candidates: list[tuple[int, float]] = []
    for hw in base.iterdir():
        name = _read_first_line(str(hw / "name")) or ""
        # We accept the common CPU sensors: coretemp (Intel), k10temp/zenpower
        # (AMD). Other sensors (nvme, iwlwifi, acpitz) are explicitly skipped
        # because they aren't representative of CPU thermal headroom.
        if name not in {"coretemp", "k10temp", "zenpower", "k8temp"}:
            continue
        for entry in hw.iterdir():
            if not entry.name.startswith("temp"):
                continue
            if not entry.name.endswith("_input"):
                continue
            label_path = hw / entry.name.replace("_input", "_label")
            label = _read_first_line(str(label_path)) or ""
            raw = _read_int(str(entry))
            if raw is None:
                continue
            celsius = raw / 1000.0
            # Prefer "Package id" / "Tctl"; fall back to whichever temp we
            # see first (priority encoded as the tuple's first element).
            if "Package" in label or label.startswith("Tctl"):
                priority = 0
            elif label.startswith("Tdie"):
                priority = 1
            else:
                priority = 2
            candidates.append((priority, celsius))
    if not candidates:
        return None
    candidates.sort()
    return candidates[0][1]


def _host_context() -> dict:
    return {
        "captured_at_utc": datetime.now(timezone.utc).isoformat(),
        "ac_online": _ac_online(),
        "battery_pct": _battery_pct(),
        "power_profile": _power_profile(),
        "package_temp_c": _pkg_temp_c(),
        "platform": sys.platform,
        "hostname": os.uname().nodename,
    }


def _maxrss_kib(rusage_obj: resource.struct_rusage) -> int:
    """Return ru_maxrss in KiB. Linux reports KiB; macOS reports bytes."""
    raw = int(rusage_obj.ru_maxrss)
    if sys.platform == "darwin":
        return raw // 1024
    return raw


def _delta_rusage(
    before: resource.struct_rusage, after: resource.struct_rusage
) -> dict:
    return {
        "user_s": round(after.ru_utime - before.ru_utime, 6),
        "sys_s": round(after.ru_stime - before.ru_stime, 6),
        # ru_maxrss is a high-water mark, so the "delta" is just the new
        # max — but RUSAGE_CHILDREN aggregates across all reaped children
        # since process start. We report the after-value as the peak for
        # this child because subprocess.run() reaps exactly one child and
        # nothing else in this wrapper reaps before us. This is correct
        # for our use (single child per wrapper invocation) and matches
        # what `time -v`'s Maximum RSS reports.
        "max_rss_kib": _maxrss_kib(after),
    }


def main() -> int:
    parser = argparse.ArgumentParser(
        description="getrusage() wrapper, time -v replacement",
        allow_abbrev=False,
    )
    parser.add_argument(
        "--sidecar",
        required=True,
        help="Path to write the JSON metrics sidecar.",
    )
    parser.add_argument(
        "--label",
        default=None,
        help="Optional label embedded into the sidecar (e.g. cell name).",
    )
    parser.add_argument(
        "argv",
        nargs=argparse.REMAINDER,
        help="The command to run. Use `--` to separate from wrapper args.",
    )
    args = parser.parse_args()

    cmd = list(args.argv)
    if cmd and cmd[0] == "--":
        cmd = cmd[1:]
    if not cmd:
        print("bench-with-rusage: no command given", file=sys.stderr)
        return 2

    context_before = _host_context()
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    wall_start = time.monotonic()
    rc: int
    try:
        rc = subprocess.run(cmd, check=False).returncode
    except FileNotFoundError as exc:
        print(f"bench-with-rusage: cannot exec: {exc}", file=sys.stderr)
        return 127
    wall_end = time.monotonic()
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    context_after = {
        "ac_online": _ac_online(),
        "battery_pct": _battery_pct(),
        "package_temp_c": _pkg_temp_c(),
    }

    payload = {
        "schema": "fono-bench.rusage/1",
        "label": args.label,
        "command": cmd,
        "exit_code": rc,
        "wall_clock_s": round(wall_end - wall_start, 6),
        **_delta_rusage(before, after),
        "context_start": context_before,
        "context_end": context_after,
    }

    sidecar_path = pathlib.Path(args.sidecar)
    sidecar_path.parent.mkdir(parents=True, exist_ok=True)
    sidecar_path.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )

    return rc


if __name__ == "__main__":
    sys.exit(main())
