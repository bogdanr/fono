#!/usr/bin/env python3
"""Diff a fresh `fono-bench equivalence --stt groq` report against the
committed baseline.

Compares per-fixture verdicts only — absolute timings are stripped by
the harness's `--baseline` flag because they flap on shared CI runners.

Exit codes:
  0  — verdicts match the baseline.
  1  — one or more verdicts diverged. Treat as a release blocker.
  2  — baseline file is missing. Print the bootstrap command and
       exit non-zero so the release job fails clearly. Bootstrap is
       a one-time human-in-the-loop step (see
       `docs/dev/release-checklist.md`).
"""

import json
import sys
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 3:
        print(f"usage: {sys.argv[0]} <fresh.json> <baseline.json>", file=sys.stderr)
        return 2

    fresh_path = Path(sys.argv[1])
    baseline_path = Path(sys.argv[2])

    if not fresh_path.exists():
        print(f"error: fresh report {fresh_path} not found", file=sys.stderr)
        return 2

    if not baseline_path.exists():
        print(
            f"error: baseline {baseline_path} not committed yet.\n\n"
            "This is expected on the first release after wiring the cloud\n"
            "equivalence gate. Bootstrap the baseline locally with:\n\n"
            "  GROQ_API_KEY=gsk_... \\\n"
            "    cargo run --release -p fono-bench --features equivalence -- \\\n"
            "    equivalence \\\n"
            f"      --stt groq \\\n"
            f"      --output {baseline_path} \\\n"
            "      --baseline --no-legend\n\n"
            "Sanity-check the verdicts in the resulting JSON, commit it,\n"
            "and re-tag.",
            file=sys.stderr,
        )
        return 2

    with fresh_path.open() as f:
        fresh = json.load(f)
    with baseline_path.open() as f:
        baseline = json.load(f)

    fresh_by = {r["fixture"]: r["verdict"] for r in fresh.get("results", [])}
    base_by = {r["fixture"]: r["verdict"] for r in baseline.get("results", [])}

    diffs = []
    for k in sorted(set(fresh_by) | set(base_by)):
        if fresh_by.get(k) != base_by.get(k):
            diffs.append(
                f"  {k}: fresh={fresh_by.get(k, '<missing>')} "
                f"baseline={base_by.get(k, '<missing>')}"
            )

    if diffs:
        print(
            "cloud equivalence verdicts diverged from committed baseline:",
            file=sys.stderr,
        )
        for d in diffs:
            print(d, file=sys.stderr)
        print(
            "\nIf the divergence is intentional (intentional accuracy\n"
            "improvement, manifest threshold change, fixture refresh),\n"
            "regenerate the baseline locally and commit it. Otherwise\n"
            "investigate before tagging.",
            file=sys.stderr,
        )
        return 1

    print(f"OK — {len(fresh_by)} fixture verdicts match baseline.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
