#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
# Fono macOS port: drive the remote Mac build sandbox from the Linux side.
#
# Task 0.7 of plans/2026-07-03-macos-port-v1.md. The dev Mac is headless
# (SSH only) and its identity is deliberately kept out of the repo
# (guiding constraint 7): the host comes exclusively from FONO_MAC_HOST
# — an ssh destination or ssh_config alias, e.g. `root@<address>` — set
# in your shell or an untracked local file you source. There is no
# default and there must never be one.
#
# Everything on the Mac lives under ~/fono-dev (toolchain, repo mirror,
# tools, caches); ~/fono-dev/env.sh exports RUSTUP_HOME, CARGO_HOME,
# PATH (cargo + standalone CMake), ORT_LIB_LOCATION (pinned static
# onnxruntime for aarch64-apple-darwin) and ORT_CXX_STDLIB=c++ (cancels
# the workspace [env] static-libstdc++ Linux-ism; cargo [env] cannot be
# target-scoped). See docs/build-macos.md for the full layout.
#
# Usage:
#   scripts/mac-remote.sh push               # rsync working tree -> Mac
#   scripts/mac-remote.sh check              # push + cargo check --workspace
#   scripts/mac-remote.sh build [args…]      # push + cargo build [args…]
#   scripts/mac-remote.sh test  [args…]      # push + cargo test --workspace --tests --lib [args…]
#   scripts/mac-remote.sh cargo <args…>      # push + arbitrary cargo command
#   scripts/mac-remote.sh sh <command…>      # raw shell in the repo dir (env sourced), no push
#
# `push` mirrors the local working tree (tracked + untracked-but-not-
# ignored files, per .gitignore; minus .git/) into ~/fono-dev/fono with
# --delete, so the remote always matches what you see locally —
# including uncommitted edits.

set -eu

usage() {
    cat <<'EOF'
usage: scripts/mac-remote.sh <command> [args…]   (requires FONO_MAC_HOST)

  push               rsync working tree -> Mac sandbox
  check              push + cargo check --workspace
  build [args…]      push + cargo build [args…]
  test  [args…]      push + cargo test --workspace --tests --lib [args…]
  cargo <args…>      push + arbitrary cargo command
  sh <command…>      raw shell in the repo dir (env sourced), no push
EOF
}

[ "${1:-}" ] || { usage >&2; exit 2; }

if [ -z "${FONO_MAC_HOST:-}" ]; then
    echo "error: FONO_MAC_HOST is not set." >&2
    echo "Set it to the Mac's ssh destination (user@host or an ssh_config" >&2
    echo "alias). It is intentionally not stored anywhere in the repo." >&2
    exit 2
fi

repo_root=$(cd "$(dirname "$0")/.." && pwd)
remote_dir='~/fono-dev/fono'
remote_env='source ~/fono-dev/env.sh'

push() {
    # `--filter ':- .gitignore'` honours every .gitignore in the tree, so
    # local bulk (target/, tmp/, bench runs, models…) never crosses the
    # wire — only tracked files and untracked-but-not-ignored ones (e.g.
    # a new source file you haven't `git add`ed yet).
    # /target is excluded explicitly (not just via the .gitignore
    # dir-merge filter) because the remote target dir holds the pinned
    # onnxruntime static lib and hours of build cache: a global exclude
    # rule reliably protects it from --delete, which the per-directory
    # merge filter alone failed to do (learned the hard way — one push
    # wiped the remote target/ and forced a full rebuild).
    rsync -az --delete \
        --exclude '/target' \
        --filter ':- .gitignore' \
        --exclude '/.git/' \
        --exclude '*~' \
        "$repo_root"/ "$FONO_MAC_HOST:$remote_dir/"
}

run_remote() {
    # shellcheck disable=SC2029 # remote-side expansion is intentional
    ssh "$FONO_MAC_HOST" "$remote_env && cd $remote_dir && $*"
}

cmd=$1
shift
case "$cmd" in
push) push ;;
check) push && run_remote cargo check --workspace "$@" ;;
build) push && run_remote cargo build "$@" ;;
test) push && run_remote cargo test --workspace --tests --lib "$@" ;;
cargo) push && run_remote cargo "$@" ;;
sh) run_remote "$@" ;;
*)
    usage >&2
    exit 2
    ;;
esac
