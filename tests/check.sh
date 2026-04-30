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
#   ./tests/check.sh --size-budget  # build release-slim musl + assert ≤ 20 MB and `ldd` empty
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
SIZE_BUDGET=false

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
        --size-budget)
            SIZE_BUDGET=true
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

# ── Step 2.5: default release tree must not pull ALSA/cpal ────────────
step "dependency tree (no cpal/ALSA in default Linux build)"
if cargo tree -p fono -i cpal >/dev/null 2>&1; then
    red "FAIL: default fono dependency tree still includes cpal (and therefore ALSA/libasound)"
    exit 5
fi
if cargo tree -p fono -i alsa >/dev/null 2>&1; then
    red "FAIL: default fono dependency tree still includes alsa/libasound"
    exit 5
fi
if cargo tree -p fono -i alsa-sys >/dev/null 2>&1; then
    red "FAIL: default fono dependency tree still includes alsa-sys/libasound"
    exit 5
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

# ── Optional: size-budget gate ────────────────────────────────────────
# Builds the canonical ship artefact (musl-static, release-slim, all
# default features) and asserts:
#   - binary size ≤ 20 MB (20 971 520 bytes)
#   - `readelf -d` contains no `NEEDED` entries (therefore no libasound,
#     libgomp, libstdc++, or glibc runtime dependency)
#   - `ggml_init` symbol appears exactly once (no duplicate ggml link)
#
# Skipped in the default invocation because it requires the
# x86_64-unknown-linux-musl Rust standard library and a musl-aware C/C++
# toolchain. On rustup-based hosts, install the Rust std with:
# `rustup target add x86_64-unknown-linux-musl`; distro-packaged Rust users
# need the equivalent rust-std package plus a musl cross C/C++ toolchain.
#
# See `plans/2026-04-30-fono-single-binary-size-v1.md` Phase 5.
if [[ "$SIZE_BUDGET" == true ]]; then
    step "size-budget gate (release-slim, x86_64-musl)"

    TARGET=x86_64-unknown-linux-musl
    BUDGET_BYTES=20971520     # 20 MiB

    SYSROOT=$(rustc --print sysroot)
    if command -v rustup >/dev/null 2>&1; then
        if ! rustup target list --installed | grep -q "^${TARGET}\$"; then
            red "rust target ${TARGET} not installed"
            echo "  install with: rustup target add ${TARGET}"
            exit 4
        fi
    elif ! compgen -G "${SYSROOT}/lib/rustlib/${TARGET}/lib/libstd-*.rlib" >/dev/null; then
        red "rust std for ${TARGET} not installed in ${SYSROOT}"
        echo "  install your distro's ${TARGET} rust-std package, or use a rustup toolchain"
        exit 4
    fi

    run cargo build -p fono \
        --profile release-slim \
        --target "$TARGET"

    BIN="target/${TARGET}/release-slim/fono"
    if [[ ! -f "$BIN" ]]; then
        red "binary not found at ${BIN}"
        exit 4
    fi

    SIZE=$(stat -c%s "$BIN")
    printf '  size: %s bytes (%.2f MiB) — budget %s bytes (20.00 MiB)\n' \
        "$SIZE" "$(awk -v s="$SIZE" 'BEGIN{printf "%.2f", s/1048576}')" \
        "$BUDGET_BYTES"
    if (( SIZE > BUDGET_BYTES )); then
        red "FAIL: binary exceeds 20 MB budget by $((SIZE - BUDGET_BYTES)) bytes"
        exit 5
    fi

    LDD_OUT=$(ldd "$BIN" 2>&1 || true)
    if ! grep -q "not a dynamic executable" <<< "$LDD_OUT"; then
        red "FAIL: binary is dynamically linked"
        echo "$LDD_OUT" | head -20
        exit 5
    fi
    echo "  ldd: not a dynamic executable ✓"

    if command -v readelf >/dev/null 2>&1; then
        NEEDED=$(readelf -d "$BIN" 2>/dev/null | grep 'NEEDED' || true)
        if [[ -n "$NEEDED" ]]; then
            red "FAIL: binary has dynamic NEEDED entries"
            echo "$NEEDED"
            exit 5
        fi
        echo "  readelf: no NEEDED entries ✓"
    fi

    if command -v nm >/dev/null 2>&1; then
        DUP=$(nm "$BIN" 2>/dev/null | grep -c '^[0-9a-f]\+ [Tt] ggml_init$' || true)
        if [[ "$DUP" -gt 1 ]]; then
            red "FAIL: ${DUP} copies of ggml_init in binary (expected 1)"
            exit 5
        fi
        echo "  ggml_init symbols: ${DUP} ✓"
    fi

    green "Size-budget gate passed (binary ≤ 20 MB, ldd empty, single ggml)."
fi
