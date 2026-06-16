#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
# Merge several onnxruntime reduced-build operator configs into their union.
#
# Fono's minimal `libonnxruntime` is built with `--include_ops_by_config`, which
# compiles ONLY the operators (and, with type reduction, the tensor types) the
# config lists. Each shipped voice model contributes a slightly different op set:
# Kokoro q8f16 needs `Greater`(13) + `If`; some Piper voices need `LSTM` /
# `MatMulInteger`; etc. A config covering only a SUBSET silently produces a
# runtime that cannot load the omitted models (the Kokoro `Greater(13)` load
# failure this tool fixes).
#
# onnxruntime's `create_reduced_build_config.py` regenerates the union from a
# directory of models, but that needs every `.ort`/`.onnx` on disk. When you
# only have the per-set configs (e.g. a mirror config + a freshly generated one
# for a new model), this tool unions them directly: the operator set per
# (domain, opset) AND the per-operator type constraints (inputs/outputs/custom).
# Over-approximation is intentional and safe — a superset only enlarges the
# compiled runtime marginally, whereas a subset fails to load a shipped model.
#
# Usage:
#   python3 scripts/merge-ort-configs.py in1.config in2.config [...] out.config
import json
import sys
from collections import defaultdict


def parse_ops(operators_str):
    """Yield (op_name, type_json_or_None), handling brace-nested type JSON."""
    cur, end = 0, len(operators_str)
    while cur < end:
        next_comma = operators_str.find(",", cur)
        next_open = operators_str.find("{", cur)
        if next_comma == -1:
            next_comma = end
        if 0 < next_open < next_comma:
            name = operators_str[cur:next_open].strip()
            i, depth = next_open + 1, 1
            while depth > 0 and i < end:
                if operators_str[i] == "{":
                    depth += 1
                elif operators_str[i] == "}":
                    depth -= 1
                i += 1
            if depth != 0:
                raise RuntimeError("unbalanced braces: " + operators_str[next_open:])
            yield name, operators_str[next_open:i]
            cur = i + 1
        else:
            yield operators_str[cur:next_comma].strip(), None
            cur = next_comma + 1


def merge_types(acc, entry):
    """Union one type-JSON entry into the accumulator (inputs/outputs/custom)."""
    info = json.loads(entry)
    for io in ("inputs", "outputs"):
        if io in info:
            for idx, types in info[io].items():
                acc[io].setdefault(idx, set()).update(types)
    if "custom" in info:
        for triple in info["custom"]:
            acc["custom"].add(tuple(triple))


def emit_types(acc):
    out = {}
    for io in ("inputs", "outputs"):
        if acc[io]:
            out[io] = {idx: sorted(t) for idx, t in sorted(acc[io].items(), key=lambda kv: int(kv[0]))}
    if acc["custom"]:
        out["custom"] = [list(t) for t in sorted(acc["custom"])]
    return json.dumps(out, separators=(", ", ": ")) if out else None


def main(argv):
    if len(argv) < 3:
        print("usage: merge-ort-configs.py in1.config [in2.config ...] out.config", file=sys.stderr)
        return 2

    ops = defaultdict(lambda: defaultdict(set))
    types = defaultdict(lambda: {"inputs": {}, "outputs": {}, "custom": set()})

    for path in argv[1:-1]:
        with open(path) as fh:
            for raw in fh:
                line = raw.strip()
                if not line or line.startswith("#") or line.startswith("!"):
                    continue
                domain, opset_str, operators_str = (s.strip() for s in line.split(";"))
                opsets = [int(s) for s in opset_str.split(",")]
                for name, type_json in parse_ops(operators_str):
                    if not name:
                        continue
                    for opset in opsets:
                        ops[domain][opset].add(name)
                    if type_json:
                        merge_types(types[(domain, name)], type_json)

    with open(argv[-1], "w") as out:
        out.write("# Union operator set for all shipped fono voice models (onnxruntime 1.24.2).\n")
        out.write("# Merged union of every per-set config (mirror + fono) so the minimal\n")
        out.write("# runtime can load EVERY shipped model: Piper voices (incl. LSTM /\n")
        out.write("# MatMulInteger variants) AND Kokoro q8f16 (needs Greater(13), If).\n")
        out.write("# Regenerate with scripts/merge-ort-configs.py over the per-set configs,\n")
        out.write("# or from the full model set via create_reduced_build_config.py\n")
        out.write("# --format ORT --enable_type_reduction.\n")
        for domain in sorted(ops):
            for opset in sorted(ops[domain]):
                parts = []
                for name in sorted(ops[domain][opset]):
                    entry = emit_types(types[(domain, name)]) if (domain, name) in types else None
                    parts.append(f"{name}{entry}" if entry else name)
                out.write(f"{domain};{opset};{','.join(parts)}\n")

    print("merged ->", argv[-1])
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
