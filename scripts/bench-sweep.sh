#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
# Phase-0 bench sweep helper. Runs the equivalence harness for every
# model in MODELS, 3 iterations per cell, 60 s cooldown between, with
# the getrusage wrapper. Writes one report + one sidecar per iteration.
#
# Usage: HOST=<id> POWER=ac BUILD=cpu RUNS_DIR=docs/bench/calibration/runs \
#        BENCH=target/release/fono-bench \
#        WRAPPER=scripts/bench-with-rusage.py \
#        sh scripts/bench-sweep.sh
set -eu

: "${HOST:?HOST id required (matches inventory filename stem)}"
: "${POWER:?POWER required (ac|battery)}"
: "${BUILD:?BUILD required (cpu|vulkan|cuda|rocm)}"
: "${RUNS_DIR:?RUNS_DIR required}"
: "${BENCH:?BENCH binary path required}"
: "${WRAPPER:?WRAPPER python script path required}"

MODELS="${MODELS:-tiny tiny.en base base.en small small.en large-v3-turbo}"
ITERS="${ITERS:-3}"
COOLDOWN="${COOLDOWN:-60}"

mkdir -p "$RUNS_DIR"
echo "sweep: host=$HOST power=$POWER build=$BUILD models='$MODELS' iters=$ITERS cooldown=${COOLDOWN}s"
echo "runs_dir=$RUNS_DIR bench=$BENCH wrapper=$WRAPPER"

for m in $MODELS; do
    model_file="${HOME}/.cache/fono/models/whisper/ggml-${m}.bin"
    if [ ! -f "$model_file" ]; then
        echo "WARN: model $m missing at $model_file — skipping cell"
        continue
    fi
    i=1
    while [ "$i" -le "$ITERS" ]; do
        out="$RUNS_DIR/${HOST}__${POWER}__${BUILD}__${m}__iter${i}.json"
        sidecar="$RUNS_DIR/${HOST}__${POWER}__${BUILD}__${m}__iter${i}.time.json"
        label="${HOST}/${POWER}/${BUILD}/${m}/iter${i}"
        echo "----"
        echo "$(date -u +%FT%TZ) START $label"
        # Free-disk pressure guard: if the partition holding ~/.cache/fono
        # drops below 1 GiB free, log and continue (per decision 4).
        free_kib=$(df -kP "$HOME/.cache" 2>/dev/null | awk 'NR==2 {print $4}')
        if [ -n "${free_kib:-}" ] && [ "$free_kib" -lt 1048576 ]; then
            echo "WARN: $HOME/.cache free=${free_kib} KiB < 1 GiB"
        fi
        if python3 "$WRAPPER" --sidecar "$sidecar" --label "$label" -- \
            "$BENCH" equivalence --stt local --model "$m" --output "$out" \
            --no-legend; then
            echo "$(date -u +%FT%TZ) OK $label"
        else
            rc=$?
            echo "$(date -u +%FT%TZ) FAIL $label rc=$rc"
        fi
        i=$((i + 1))
        if [ "$i" -le "$ITERS" ]; then
            echo "cooldown ${COOLDOWN}s"
            sleep "$COOLDOWN"
        fi
    done
    # Inter-cell cooldown is also $COOLDOWN — thermal headroom matters
    # most between large→large transitions.
    sleep "$COOLDOWN"
done
echo "sweep complete: host=$HOST"
