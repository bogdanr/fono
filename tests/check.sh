#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# fono CI-equivalent local check.
#
# Runs the same gate `CONTRIBUTING.md` and `docs/status.md` mandate before
# a PR lands: fmt + build + clippy + tests across the two feature combos
# (default and `fono/interactive`).
#
# Usage:
#   ./tests/check.sh                # full matrix (build + clippy + test, both feature sets)
#   ./tests/check.sh --quick        # default-features only, skip clippy and the slow integration tests
#   ./tests/check.sh --no-test      # skip the test phase (build + clippy only)
#   ./tests/check.sh --slim         # cloud-only slim build instead of default features
#   ./tests/check.sh --help         # this message
#
# Exit code: 0 if every step passes; non-zero on the first failure
# (`set -e`). Each step is announced before it runs so the failing
# command is obvious in the scrollback.
#
# Honours `CARGO_TARGET_DIR` if set; otherwise uses the workspace's
# default `target/` so artefacts are reused across invocations.

set -euo pipefail

cd "$(dirname "$0")/.."

# ── Defaults ──────────────────────────────────────────────────────────
QUICK=false
RUN_TESTS=true
SLIM=false

# ── Parse arguments ───────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --quick|-q)
            QUICK=true
            shift
            ;;
        --no-test)
            RUN_TESTS=false
            shift
            ;;
        --slim)
            SLIM=true
            shift
            ;;
        --help|-h)
            sed -n '4,16p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            echo "run with --help for usage" >&2
            exit 2
            ;;
    esac
done

# ── Pretty-printing ───────────────────────────────────────────────────
bold()  { printf '\033[1m%s\033[0m\n' "$*"; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }
red()   { printf '\033[31m%s\033[0m\n' "$*"; }
dim()   { printf '\033[2m%s\033[0m\n' "$*"; }

step() {
    echo
    bold "=== $* ==="
}

run() {
    dim "\$ $*"
    "$@"
}

# ── Step 1: formatting ────────────────────────────────────────────────
step "cargo fmt --check"
run cargo fmt --all -- --check

# ── Step 2: build matrix ──────────────────────────────────────────────
if [[ "$SLIM" == true ]]; then
    # Cloud-only slim build per `crates/fono/Cargo.toml:28`.
    step "build (slim, cloud-only)"
    run cargo build -p fono --no-default-features --features tray,cloud-all --all-targets
    if [[ "$QUICK" == false ]]; then
        step "build (slim + interactive)"
        run cargo build -p fono --no-default-features \
            --features tray,cloud-all,interactive --all-targets
    fi
else
    step "build (default features)"
    run cargo build --workspace --all-targets

    if [[ "$QUICK" == false ]]; then
        step "build (default + interactive)"
        run cargo build --workspace --all-targets --features fono/interactive
    fi
fi

# ── Step 3: clippy (skipped in --quick) ───────────────────────────────
if [[ "$QUICK" == false ]]; then
    if [[ "$SLIM" == true ]]; then
        step "clippy (slim, cloud-only)"
        run cargo clippy -p fono --no-default-features \
            --features tray,cloud-all --all-targets -- -D warnings

        step "clippy (slim + interactive)"
        run cargo clippy -p fono --no-default-features \
            --features tray,cloud-all,interactive --all-targets -- -D warnings
    else
        step "clippy (default features)"
        run cargo clippy --workspace --all-targets -- -D warnings

        step "clippy (default + interactive)"
        run cargo clippy --workspace --all-targets \
            --features fono/interactive -- -D warnings
    fi
fi

# ── Step 4: tests ─────────────────────────────────────────────────────
if [[ "$RUN_TESTS" == true ]]; then
    if [[ "$QUICK" == true ]]; then
        # Quick mode: lib tests only — skips the multi-second integration
        # tests under `crates/*/tests/` and the `--ignored` latency smoke
        # tests in `fono-bench`.
        step "test (lib only, default features)"
        run cargo test --workspace --lib
    elif [[ "$SLIM" == true ]]; then
        step "test (slim, cloud-only)"
        run cargo test -p fono --no-default-features --features tray,cloud-all --all-targets

        step "test (slim + interactive)"
        run cargo test -p fono --no-default-features \
            --features tray,cloud-all,interactive --all-targets
    else
        step "test (default features)"
        run cargo test --workspace --all-targets

        step "test (default + interactive)"
        run cargo test --workspace --all-targets --features fono/interactive
    fi
fi

# ── Done ──────────────────────────────────────────────────────────────
echo
green "All checks passed."
