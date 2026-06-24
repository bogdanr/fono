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
#   ./tests/check.sh --size-budget  # ONLY the CI size gate: release-slim glibc cpu, assert ≤ 28 MiB + 4-entry NEEDED (skips the matrix)
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

# When invoked purely for the size gate, skip the fmt/build/clippy/test
# matrix below and jump straight to it — the gate does its own dedicated
# release-slim build, so the matrix would be wasted work.
if [[ "$SIZE_BUDGET" == false ]]; then

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

fi  # end of fmt/build/clippy/test matrix (skipped under --size-budget)

# ── Optional: size-budget gate (mirrors CI) ───────────────────────────
# Builds the **canonical CI ship artefact** — `release-slim`, glibc
# `x86_64-unknown-linux-gnu` (or `aarch64` on arm hosts), default features
# (which include `tts-local`) — and asserts exactly what the
# `.github/workflows/ci.yml` `size-budget` job asserts:
#   - binary size ≤ the `cpu` budget (28 MiB / 29 360 128 B)
#   - `readelf -d` NEEDED set ⊆ the universal four-entry glibc allowlist
#     (libc, libm, libgcc_s, the dynamic linker) — any extra entry fails
#
# This is the **same target + profile + features + budget + allowlist** as
# CI, so a green run here means the CI size gate will pass. Run it before
# every push / tag / release (see AGENTS.md). The numbers below are kept
# in lockstep with the ci.yml `cpu` matrix rows — change them together.
#
# `tts-local` is a default feature, so the build pins the static
# `libonnxruntime.a` via `ORT_LIB_LOCATION` exactly as CI does; if the env
# var is unset we resolve it through `scripts/fetch-onnxruntime.sh` (cached
# under `target/`, no CDN when already present).
if [[ "$SIZE_BUDGET" == true ]]; then
    step "size-budget gate (release-slim, glibc cpu — mirrors ci.yml)"

    # Keep in lockstep with .github/workflows/ci.yml `size-budget` `cpu` rows.
    BUDGET_BYTES=29360128     # 28 MiB

    ARCH=$(uname -m)
    case "$ARCH" in
        x86_64)  TARGET=x86_64-unknown-linux-gnu;  DYN_LINKER=ld-linux-x86-64.so.2 ;;
        aarch64) TARGET=aarch64-unknown-linux-gnu; DYN_LINKER=ld-linux-aarch64.so.1 ;;
        *) red "size-budget gate mirrors CI only on x86_64/aarch64 (host is ${ARCH})"; exit 4 ;;
    esac

    # Universal glibc + libgcc_s ABI — identical to the ci.yml allowlist.
    # (On aarch64 the dynamic linker is PT_INTERP-only, not NEEDED; listing
    # it is a harmless superset.)
    ALLOWLIST="libc.so.6 libm.so.6 libgcc_s.so.1 ${DYN_LINKER}"

    if command -v rustup >/dev/null 2>&1; then
        if ! rustup target list --installed | grep -q "^${TARGET}\$"; then
            red "rust target ${TARGET} not installed"
            echo "  install with: rustup target add ${TARGET}"
            exit 4
        fi
    else
        # Distro toolchain (no rustup): ask rustc where the target's std
        # lives and confirm libstd is actually present there. `target-libdir`
        # resolves the real path (e.g. /usr/lib64/rustlib/...) regardless of
        # lib vs lib64 layout.
        TLD=$(rustc --print target-libdir --target "$TARGET" 2>/dev/null || true)
        if [[ -z "$TLD" ]] || ! compgen -G "${TLD}/libstd-*.rlib" >/dev/null; then
            red "rust std for ${TARGET} not installed (looked in ${TLD:-<unknown>})"
            echo "  install your distro's ${TARGET} rust-std package, or use a rustup toolchain"
            exit 4
        fi
    fi

    # Pin onnxruntime exactly as CI (default features include tts-local).
    if [[ -z "${ORT_LIB_LOCATION:-}" ]]; then
        step "resolve ORT_LIB_LOCATION (scripts/fetch-onnxruntime.sh)"
        ORT_LIB_LOCATION="$(sh scripts/fetch-onnxruntime.sh)"
        export ORT_LIB_LOCATION
    fi
    echo "  ORT_LIB_LOCATION=${ORT_LIB_LOCATION}"

    run cargo build -p fono \
        --profile release-slim \
        --target "$TARGET"

    BIN="target/${TARGET}/release-slim/fono"
    if [[ ! -f "$BIN" ]]; then
        red "binary not found at ${BIN}"
        exit 4
    fi

    SIZE=$(stat -c%s "$BIN")
    printf '  size: %s bytes (%.2f MiB) — budget %s bytes (%.2f MiB)\n' \
        "$SIZE" "$(awk -v s="$SIZE" 'BEGIN{printf "%.2f", s/1048576}')" \
        "$BUDGET_BYTES" "$(awk -v b="$BUDGET_BYTES" 'BEGIN{printf "%.2f", b/1048576}')"
    if (( SIZE > BUDGET_BYTES )); then
        red "FAIL: ${TARGET} binary exceeds budget by $((SIZE - BUDGET_BYTES)) bytes"
        exit 5
    fi

    # NEEDED allowlist — fail on any entry outside the universal set.
    if command -v readelf >/dev/null 2>&1; then
        ACTUAL_NEEDED=$(readelf -d "$BIN" \
            | awk '/\(NEEDED\)/ { gsub(/[][]/,"",$NF); print $NF }' | sort -u)
        ALLOWED_SORTED=$(printf '%s\n' $ALLOWLIST | sort -u)
        EXTRAS=$(comm -23 <(printf '%s\n' "$ACTUAL_NEEDED") <(printf '%s\n' "$ALLOWED_SORTED") || true)
        if [[ -n "$EXTRAS" ]]; then
            red "FAIL: unexpected NEEDED entries (not in allowlist):"
            printf '  %s\n' $EXTRAS
            echo "  full NEEDED set:"
            printf '    %s\n' $ACTUAL_NEEDED
            exit 5
        fi
        echo "  NEEDED (⊆ 4-entry allowlist) ✓:"
        printf '    %s\n' $ACTUAL_NEEDED
    fi

    green "Size-budget gate passed (${TARGET}, ≤ 28 MiB, NEEDED allowlist clean) — CI size gate will pass."
fi
