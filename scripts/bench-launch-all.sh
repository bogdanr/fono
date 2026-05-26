#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
# Launches all 5 per-host bench drivers as background nohup processes.
set -eu
REPO_ROOT=/mnt/nvme0n1p5/Work/fono
RUNS_LOCAL="$REPO_ROOT/docs/bench/calibration/runs"
DRIVER="$REPO_ROOT/scripts/bench-remote-driver.sh"

# Common env exported into each driver
export REPO_ROOT RUNS_LOCAL POWER=ac ITERS=3 COOLDOWN=60 MIN_FREE_GIB=2

launch() {
    name=$1; ip=$2; backends=$3
    extra=""
    if [ "$ip" = "localhost" ]; then
        extra="REMOTE_ROOT=$REPO_ROOT RUNS_REMOTE=$RUNS_LOCAL"
    fi
    pidfile="/tmp/sweep-${name}.pid"
    outfile="/tmp/sweep-${name}.stdout"
    env $extra HOST_ID="$name" HOST_IP="$ip" BACKENDS="$backends" \
        nohup bash "$DRIVER" >"$outfile" 2>&1 &
    echo $! >"$pidfile"
    echo "launched $name (ip=$ip backends='$backends') pid=$(cat $pidfile)"
}

launch i7-1255u     localhost      "cpu vulkan"
launch ultra7-258v  192.168.0.251  "cpu vulkan"
launch i7-8550u     192.168.0.252  "cpu"
launch i7-7500u     192.168.0.253  "cpu vulkan"
launch ryzen-5950x  192.168.0.74   "cpu vulkan"

echo "all 5 drivers launched"
