#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
# Fono voice-stack: ORT-format conversion + operator-config generation.
#
# This is Phase 1.3 of
# plans/2026-05-31-local-tts-onnx-voice-stack-and-wyoming-server-v3.md and
# the standing pipeline EVERY future voice model plugs into (Piper, Kokoro,
# Silero VAD, KWS, Zipformer). See docs/binary-size.md: the size of the
# minimal onnxruntime build is determined ENTIRELY by the union of operators
# this script discovers across all shipped models.
#
# What it does, for every `*.onnx` under MODELS_DIR:
#   1. Converts the float ONNX graph to the optimised `.ort` flatbuffer
#      format that a `--minimal_build` runtime can load (a minimal build
#      CANNOT load plain `.onnx`).
#   2. Emits a single `required_operators_and_types.config` describing the
#      exact operator+type set used by the union of those models. That file
#      is the `--include_ops_by_config` input to
#      scripts/build-onnxruntime-minimal.sh (Phase 1.1).
#
# The onnxruntime version MUST match the one `ort-sys` links against, so the
# generated `.ort` flatbuffer schema and the operator config match the
# runtime ABI. Verified 2026-05-31: ort 2.0.0-rc.12 -> ort-sys 2.0.0-rc.12
# pins onnxruntime 1.24.2 (pyke `ms@1.24.2`).
#
# Usage:
#   MODELS_DIR=calibration/voice-models OUT_DIR=calibration/voice-models/ort \
#     sh scripts/gen-ort-models.sh
#
# IMPORTANT — the emitted ops.config MUST be the union of EVERY shipped voice
# model: all Piper voices in crates/fono-tts/voices/catalog.json PLUS Kokoro
# (English) PLUS any other ONNX model the binary loads (Silero VAD, …). The
# minimal onnxruntime build compiles ONLY the operators this config lists, so a
# config generated from a SUBSET silently ships a runtime that cannot load the
# omitted models. This bit us once: a Piper-only config produced a lib missing
# Kokoro's `Greater`(opset-13) kernel, and every English (Kokoro) synthesis
# failed with "Could not find an implementation for Greater(13) node". Populate
# MODELS_DIR with the FULL set before running; the Kokoro guard below refuses a
# partial run unless you set ALLOW_PARTIAL=1 for ad-hoc single-model inspection.
#
# Prereqs (host tooling, NOT shipped — see docs/providers.md):
#   - python3 with a venv: `pip install onnxruntime==1.24.2`
#   - curl (only needed for the first-run Piper seed download)
set -eu

ORT_PY_VERSION="${ORT_PY_VERSION:-1.24.2}"
MODELS_DIR="${MODELS_DIR:-calibration/voice-models}"
OUT_DIR="${OUT_DIR:-${MODELS_DIR}/ort}"
PYTHON="${PYTHON:-python3}"
# Set ALLOW_PARTIAL=1 to skip the full-set guards (single-model inspection).
ALLOW_PARTIAL="${ALLOW_PARTIAL:-0}"

# Seed voice: the Romanian Piper voice Phase 2 ships first. Pinned to the
# rhasspy/piper-voices HF repo. SHA-256 is checked by `fono-download` at
# runtime; here we only need the bytes to discover operators, so we verify a
# non-empty download and leave content pinning to the runtime cache layer.
SEED_VOICE_URL="${SEED_VOICE_URL:-https://huggingface.co/rhasspy/piper-voices/resolve/main/ro/ro_RO/mihai/medium/ro_RO-mihai-medium.onnx}"
SEED_VOICE_NAME="ro_RO-mihai-medium.onnx"

mkdir -p "$MODELS_DIR" "$OUT_DIR"

# --- 1. Seed the Piper Romanian voice on first run ------------------------
if ! ls "$MODELS_DIR"/*.onnx >/dev/null 2>&1; then
    echo "no .onnx models in $MODELS_DIR — seeding Piper $SEED_VOICE_NAME"
    if ! command -v curl >/dev/null 2>&1; then
        echo "ERROR: curl required to seed the first model" >&2
        exit 2
    fi
    curl -fSL --retry 3 -o "$MODELS_DIR/$SEED_VOICE_NAME" "$SEED_VOICE_URL"
    if [ ! -s "$MODELS_DIR/$SEED_VOICE_NAME" ]; then
        echo "ERROR: seed download produced an empty file" >&2
        exit 3
    fi
fi

# --- 2. Sanity-check the python onnxruntime version -----------------------
have_ver="$("$PYTHON" -c 'import onnxruntime as o; print(o.__version__)' 2>/dev/null || true)"
if [ -z "$have_ver" ]; then
    echo "ERROR: python onnxruntime not importable. Run:" >&2
    echo "  $PYTHON -m pip install onnxruntime==$ORT_PY_VERSION" >&2
    exit 4
fi
if [ "$have_ver" != "$ORT_PY_VERSION" ]; then
    echo "WARN: python onnxruntime $have_ver != pinned $ORT_PY_VERSION." >&2
    echo "      The .ort flatbuffer schema is version-coupled to the runtime;" >&2
    echo "      a mismatch can produce models the linked runtime cannot load." >&2
fi

# --- 3. Convert to ORT format + emit the operator/type config -------------
# `--enable_type_reduction` records not just which operators are used but
# which tensor *types* per operator, so the runtime built with
# `--enable_reduced_operator_type_support` can drop unused type kernels —
# the second-biggest size lever after operator pruning.
echo "converting $MODELS_DIR/*.onnx -> $OUT_DIR/*.ort (onnxruntime $have_ver)"
"$PYTHON" -m onnxruntime.tools.convert_onnx_models_to_ort \
    "$MODELS_DIR" \
    --output_dir "$OUT_DIR" \
    --enable_type_reduction \
    --optimization_style Fixed

# The converter writes the config next to the output. Normalise its name so
# Phase 1.1 has a stable path to consume.
CONFIG_SRC="$(find "$OUT_DIR" -name 'required_operators_and_types.config' -print -quit 2>/dev/null || true)"
if [ -z "$CONFIG_SRC" ]; then
    # Fall back to the operators-only config if type reduction was skipped.
    CONFIG_SRC="$(find "$OUT_DIR" -name 'required_operators.config' -print -quit 2>/dev/null || true)"
fi
if [ -z "$CONFIG_SRC" ]; then
    echo "ERROR: conversion did not emit an operator config" >&2
    exit 5
fi
cp "$CONFIG_SRC" "$OUT_DIR/ops.config"

# --- 4. Regression guard: refuse a partial (non-union) operator config -----
# Kokoro is the model with the broadest operator footprint; its `Greater`
# (opset 13) kernel is the canonical marker that distinguishes a full-union
# config from a Piper-only one. If Kokoro's op is absent, the build would
# reproduce the shipped-lib regression, so fail loudly instead.
if [ "$ALLOW_PARTIAL" != "1" ]; then
    if ! grep -Eq '(^|;|,)Greater($|;|,|\{)' "$OUT_DIR/ops.config"; then
        echo "ERROR: generated ops.config lacks the Kokoro 'Greater'(13) op." >&2
        echo "       MODELS_DIR ($MODELS_DIR) is missing Kokoro and/or other" >&2
        echo "       shipped models — this would build a runtime that cannot" >&2
        echo "       load them (the Kokoro 'Greater(13)' regression)." >&2
        echo "       Populate MODELS_DIR with the FULL shipped set (every" >&2
        echo "       catalog.json Piper voice + Kokoro), or set ALLOW_PARTIAL=1" >&2
        echo "       for a deliberate single-model inspection run." >&2
        exit 7
    fi
fi

echo "----"
echo "ORT models:  $OUT_DIR/*.ort"
echo "ops.config:  $OUT_DIR/ops.config  (feed to build-onnxruntime-minimal.sh)"
echo "operators:   $(grep -cv '^#' "$OUT_DIR/ops.config" 2>/dev/null || echo '?') config lines"
