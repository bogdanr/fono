#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
# Fono voice-stack: minimal static onnxruntime build.
#
# This is Phase 1.1 of
# plans/2026-05-31-local-tts-onnx-voice-stack-and-wyoming-server-v3.md.
# It produces a single statically-linkable `libonnxruntime.a` containing
# ONLY the operators Fono's shipped voice models actually use — the primary
# size lever documented in docs/binary-size.md (full prebuilt ~19 MiB ->
# target ~7-11 MiB once linked into `fono`).
#
# The output is consumed by `ort` via `ORT_LIB_LOCATION` (Phase 1.2) with
# `download-binaries` disabled, so release builds NEVER pull the full CDN
# runtime. Pin the produced archive as a CI artefact (plan Risk 2).
#
# Pinned version: onnxruntime v1.24.2. This MUST match the version `ort-sys`
# 2.0.0-rc.12 links against (verified 2026-05-31: pyke `ms@1.24.2`), or the
# generated symbol/ABI surface will not match the `ort` Rust bindings.
#
# Usage:
#   OPS_CONFIG=calibration/voice-models/ort/ops.config \
#   OUT_DIR=calibration/onnxruntime-minimal \
#     sh scripts/build-onnxruntime-minimal.sh
#
# The OPS_CONFIG is produced by scripts/gen-ort-models.sh (Phase 1.3); run
# that first. Prereqs (host tooling, NOT shipped — see docs/providers.md):
#   - git, cmake >= 3.28, a C++17 toolchain, python3, ninja (recommended)
#   - ~6 GiB scratch + ~30-45 min on a warm CI runner
set -eu

ORT_VERSION="${ORT_VERSION:-1.24.2}"
ORT_TAG="v${ORT_VERSION}"
OPS_CONFIG="${OPS_CONFIG:-calibration/voice-models/ort/ops.config}"
OUT_DIR="${OUT_DIR:-calibration/onnxruntime-minimal}"
SRC_DIR="${SRC_DIR:-${OUT_DIR}/onnxruntime-src}"
BUILD_DIR="${BUILD_DIR:-${OUT_DIR}/build}"
# XNNPACK: the statically-linkable CPU accelerator (ADR 0032). On by default
# to match plan task 1.2; set USE_XNNPACK=0 to measure the size delta.
USE_XNNPACK="${USE_XNNPACK:-1}"
PYTHON="${PYTHON:-python3}"

if [ ! -f "$OPS_CONFIG" ]; then
    echo "ERROR: ops.config not found at $OPS_CONFIG" >&2
    echo "       Run scripts/gen-ort-models.sh first (Phase 1.3)." >&2
    exit 2
fi

mkdir -p "$OUT_DIR"

# --- 1. Fetch the pinned onnxruntime source -------------------------------
if [ ! -d "$SRC_DIR/.git" ]; then
    echo "cloning onnxruntime $ORT_TAG (shallow, with submodules)"
    git clone --depth 1 --branch "$ORT_TAG" --recurse-submodules --shallow-submodules \
        https://github.com/microsoft/onnxruntime.git "$SRC_DIR"
else
    echo "reusing onnxruntime checkout at $SRC_DIR"
    git -C "$SRC_DIR" fetch --depth 1 origin "$ORT_TAG"
    git -C "$SRC_DIR" checkout "$ORT_TAG"
    git -C "$SRC_DIR" submodule update --init --recursive --depth 1
fi

# --- 2. Minimal static build ----------------------------------------------
# Flag rationale (see docs/binary-size.md + ADR 0032):
#   --minimal_build            : drop the full ONNX graph machinery; load
#                                only `.ort` flatbuffer models at runtime.
#   --include_ops_by_config    : compile ONLY the operators in ops.config.
#   --enable_reduced_operator_type_support : also drop unused per-op type
#                                kernels (pairs with gen-ort-models.sh's
#                                --enable_type_reduction).
#   --disable_ml_ops           : drop the classical-ML (sklearn-style) ops;
#                                voice models are all neural.
#   --disable_exceptions/--disable_rtti : C++ size shave (minimal-build only).
#   --config MinSizeRel        : -Os, the smallest codegen.
#   --allow_running_as_root    : permit the build under a root CI container.
#   CMAKE_SKIP_INSTALL_RULES   : we merge the `.a` files ourselves, no install.
#   FETCHCONTENT_TRY_FIND_PACKAGE_MODE=NEVER : force a hermetic submodule build
#                                (never resolve deps against system packages),
#                                so the archive's ABI matches `ort-sys` exactly.
xnnpack_flag=""
if [ "$USE_XNNPACK" = "1" ]; then
    xnnpack_flag="--use_xnnpack"
fi

echo "building minimal static onnxruntime ($ORT_TAG, MinSizeRel, xnnpack=$USE_XNNPACK)"
"$PYTHON" "$SRC_DIR/tools/ci_build/build.py" \
    --build_dir "$BUILD_DIR" \
    --config MinSizeRel \
    --parallel \
    --skip_tests \
    --compile_no_warning_as_error \
    --minimal_build \
    --disable_ml_ops \
    --disable_exceptions \
    --disable_rtti \
    --enable_reduced_operator_type_support \
    --include_ops_by_config "$OPS_CONFIG" \
    --allow_running_as_root \
    --cmake_extra_defines CMAKE_SKIP_INSTALL_RULES=ON \
    --cmake_extra_defines FETCHCONTENT_TRY_FIND_PACKAGE_MODE=NEVER \
    $xnnpack_flag

# --- 3. Merge the per-target static archives into one libonnxruntime.a -----
# A static onnxruntime build emits many object archives (session, framework,
# graph, optimizer, mlas, flatbuffers, onnx, protobuf, re2, abseil, ...).
# `ort`'s ORT_LIB_LOCATION expects ONE merged archive, so combine them. The
# exact set is enumerated at build time rather than hard-coded, since it
# varies with the flags above. The merge tool is OS-specific:
#   - Linux  : GNU `ar` MRI script (`create`/`addlib`/`save`).
#   - macOS  : BSD `ar` has no `-M`; use `libtool -static`.
#   - Windows: `lib.exe /OUT` (handled in the GitHub matrix, MSVC `.lib`).
cfg_dir="$BUILD_DIR/MinSizeRel"
os_name="$(uname -s)"

if [ "$os_name" = "Darwin" ]; then
    merged="$OUT_DIR/libonnxruntime.a"
    # shellcheck disable=SC2046 # intentional word-split of the archive list
    archives="$(find "$cfg_dir" -name '*.a' -type f 2>/dev/null || true)"
    if [ -z "$archives" ]; then
        echo "ERROR: no static archives found under $cfg_dir" >&2
        exit 6
    fi
    rm -f "$merged"
    # libtool -static dedups members and writes a ranlib'd fat-free archive.
    # shellcheck disable=SC2086
    libtool -static -o "$merged" $archives
else
    merged="$OUT_DIR/libonnxruntime.a"
    mri="$OUT_DIR/.merge.mri"
    archives="$(find "$cfg_dir" -name '*.a' -type f 2>/dev/null || true)"
    if [ -z "$archives" ]; then
        echo "ERROR: no static archives found under $cfg_dir" >&2
        exit 6
    fi
    {
        echo "create $merged"
        for a in $archives; do
            echo "addlib $a"
        done
        echo "save"
        echo "end"
    } > "$mri"
    rm -f "$merged"
    ar -M < "$mri"
    ranlib "$merged"
fi

echo "----"
echo "merged archive: $merged"
ls -l "$merged" | awk '{printf "  size: %s bytes (%.2f MiB)\n", $5, $5/1048576}'
echo "Point ort at it:  export ORT_LIB_LOCATION=$(cd "$OUT_DIR" && pwd)"
echo "and build fono with default-features-off ort (no download-binaries)."
