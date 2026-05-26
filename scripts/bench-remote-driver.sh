#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
# Controller-side per-host driver for the clean-slate calibration sweep
# (2026-05-24). One invocation drives ONE host through its configured
# backends, model-by-model, with download-bench-rsync-delete lifecycle.
#
# Usage: HOST_ID=i7-1255u HOST_IP=localhost BACKENDS="cpu vulkan" \
#        REPO_ROOT=/mnt/nvme0n1p5/Work/fono \
#        sh scripts/bench-remote-driver.sh
#
# Env knobs:
#   HOST_ID       : matches inventory filename stem (e.g. i7-1255u)
#   HOST_IP       : "localhost" or remote IP
#   BACKENDS      : space-separated subset of "cpu vulkan"
#   POWER         : default "ac"
#   ITERS         : default 3
#   COOLDOWN      : default 60 (between iters and between cells)
#   MIN_FREE_GIB  : abort cell if free disk falls below this (default 2)
#   REPO_ROOT     : controller-side repo root for rsync target
#   REMOTE_ROOT   : remote-side path; default /root/Work/fono
#   RUNS_REMOTE   : remote runs dir; default /root/Work/fono/runs-local
#   RUNS_LOCAL    : controller runs dir; default $REPO_ROOT/docs/bench/calibration/runs

set -u
: "${HOST_ID:?HOST_ID required}"
: "${HOST_IP:?HOST_IP required}"
: "${BACKENDS:?BACKENDS required (e.g. \"cpu vulkan\")}"
: "${REPO_ROOT:?REPO_ROOT required}"
POWER="${POWER:-ac}"
ITERS="${ITERS:-3}"
COOLDOWN="${COOLDOWN:-60}"
MIN_FREE_GIB="${MIN_FREE_GIB:-2}"
REMOTE_ROOT="${REMOTE_ROOT:-/root/Work/fono}"
RUNS_REMOTE="${RUNS_REMOTE:-$REMOTE_ROOT/runs-local}"
RUNS_LOCAL="${RUNS_LOCAL:-$REPO_ROOT/docs/bench/calibration/runs}"
DATE=2026-05-24
LOG="$REPO_ROOT/docs/bench/calibration/logs/sweep-${HOST_ID}-${DATE}.log"

# 21-cell model matrix
MODELS="tiny tiny-q8_0 tiny-q5_1 \
tiny.en tiny.en-q8_0 tiny.en-q5_1 \
base base-q8_0 base-q5_1 \
base.en base.en-q8_0 base.en-q5_1 \
small small-q8_0 small-q5_1 \
small.en small.en-q8_0 small.en-q5_1 \
large-v3-turbo large-v3-turbo-q8_0 large-v3-turbo-q5_0"

HF_BASE="https://huggingface.co/ggerganov/whisper.cpp/resolve/main"

logf() {
    printf '%s %s\n' "$(date -u +%FT%TZ)" "$*" >>"$LOG"
}

remote() {
    if [ "$HOST_IP" = "localhost" ]; then
        sh -c "$*"
    else
        ssh -o BatchMode=yes -o ServerAliveInterval=30 -o ConnectTimeout=10 \
            root@"$HOST_IP" "$*"
    fi
}

rsync_pull() {
    # pull JSON outputs for a finished cell back to controller
    if [ "$HOST_IP" = "localhost" ]; then
        # localhost driver writes directly into RUNS_LOCAL, no rsync needed
        :
    else
        rsync -az \
            "root@$HOST_IP:$RUNS_REMOTE/${HOST_ID}__${POWER}__$1__$2__iter*.json" \
            "$RUNS_LOCAL/" 2>>"$LOG" || true
    fi
}

logf "=== driver start host=$HOST_ID ip=$HOST_IP backends='$BACKENDS' ==="

# Make sure remote runs dir exists and is empty of stale per-host JSON
if [ "$HOST_IP" != "localhost" ]; then
    remote "mkdir -p '$RUNS_REMOTE' && rm -f '$RUNS_REMOTE/${HOST_ID}__'*.json"
else
    mkdir -p "$RUNS_LOCAL"
fi

mkdir -p "$RUNS_LOCAL"

for BACKEND in $BACKENDS; do
    case "$BACKEND" in
        cpu)    BENCH="$REMOTE_ROOT/target/release/fono-bench-cpu" ;;
        vulkan) BENCH="$REMOTE_ROOT/target/release/fono-bench-vulkan" ;;
        *) logf "skip unknown BACKEND=$BACKEND"; continue ;;
    esac
    # Localhost uses the local repo paths
    if [ "$HOST_IP" = "localhost" ]; then
        case "$BACKEND" in
            cpu)    BENCH="$REPO_ROOT/target-cpu/release/fono-bench" ;;
            vulkan) BENCH="$REPO_ROOT/target/release/fono-bench" ;;
        esac
    fi

    if ! remote "test -x '$BENCH'"; then
        logf "ERROR backend=$BACKEND missing binary $BENCH on $HOST_ID — skipping backend"
        continue
    fi
    logf "--- backend=$BACKEND bench=$BENCH ---"

    for m in $MODELS; do
        cell="${HOST_ID}/${POWER}/${BACKEND}/${m}"
        model_file='$HOME/.cache/fono/models/whisper/ggml-'"${m}"'.bin'

        # Disk guard
        free_gib=$(remote "df -BG --output=avail \$HOME 2>/dev/null | tail -1 | tr -dc 0-9")
        if [ -n "$free_gib" ] && [ "$free_gib" -lt "$MIN_FREE_GIB" ]; then
            logf "ABORT-CELL $cell free=${free_gib}G < ${MIN_FREE_GIB}G"
            continue
        fi

        # Download if missing
        if ! remote "test -f $model_file"; then
            logf "DOWNLOAD $cell ggml-${m}.bin"
            if ! remote "mkdir -p \$HOME/.cache/fono/models/whisper && curl -fsSL --retry 3 -o $model_file '$HF_BASE/ggml-${m}.bin' && test -s $model_file"; then
                logf "FAIL-DOWNLOAD $cell"
                remote "rm -f $model_file" || true
                continue
            fi
        fi

        logf "START $cell"
        # Run the sweep for this single model on this backend
        remote "cd $REMOTE_ROOT && HOST='$HOST_ID' POWER='$POWER' BUILD='$BACKEND' \
            RUNS_DIR='$RUNS_REMOTE' BENCH='$BENCH' \
            WRAPPER='$REMOTE_ROOT/scripts/bench-with-rusage.py' \
            FIXTURES='$REMOTE_ROOT/tests/fixtures/equivalence' \
            MODELS='$m' ITERS='$ITERS' COOLDOWN='$COOLDOWN' \
            sh $REMOTE_ROOT/scripts/bench-sweep.sh" \
            >>"$LOG" 2>&1
        rc=$?
        if [ "$rc" -ne 0 ]; then
            logf "WARN $cell rc=$rc (continuing)"
        fi
        logf "END $cell rc=$rc"

        # rsync produced JSONs back
        if [ "$HOST_IP" = "localhost" ]; then
            # localhost wrote into a remote-shaped dir; copy to RUNS_LOCAL
            cp -f "$RUNS_REMOTE/${HOST_ID}__${POWER}__${BACKEND}__${m}__iter"*.json \
                "$RUNS_LOCAL/" 2>>"$LOG" || true
        else
            rsync -az \
                "root@$HOST_IP:$RUNS_REMOTE/${HOST_ID}__${POWER}__${BACKEND}__${m}__iter"*.json \
                "$RUNS_LOCAL/" 2>>"$LOG" || true
        fi

        # Delete model
        remote "rm -f $model_file" || true
        logf "DELETED $cell ggml-${m}.bin"
    done
done

logf "=== driver done host=$HOST_ID ==="
