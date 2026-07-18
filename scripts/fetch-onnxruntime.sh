#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
# Fono voice-stack: fetch the prebuilt minimal static onnxruntime library.
#
# This is the convenience counterpart to scripts/build-onnxruntime-minimal.sh
# (Phase 1.1 of
# plans/2026-05-31-local-tts-onnx-voice-stack-and-wyoming-server-v3.md).
# Instead of the ~30-45 min from-source minimal build, it downloads the
# already-built, op-set-matched `libonnxruntime.a` we host on the fono-voice
# release mirror, verifies its SHA-256, and extracts it to a cache dir.
#
# The archive is byte-identical to what build-onnxruntime-minimal.sh produces
# (onnxruntime 1.24.2, matching the `ort` 2.0.0-rc.12 ABI). It is consumed by
# `ort` via ORT_LIB_LOCATION with `download-binaries` disabled, so builds never
# pull the full upstream CDN runtime.
#
# Usage:
#   export ORT_LIB_LOCATION="$(sh scripts/fetch-onnxruntime.sh)"
#   cargo build -p fono --features tts-local
#
# The script prints the directory containing libonnxruntime.a on stdout (all
# diagnostics go to stderr), so it composes directly into ORT_LIB_LOCATION.
# Re-runs are cheap: an already-verified cached library is reused.
#
# Prereqs (host tooling, NOT shipped): curl, xz, and sha256sum (or `shasum`,
# the stock macOS equivalent). Note: bsdtar's raw-xz mode is NOT a substitute
# for xz here — it silently truncates multi-stream .xz files (verified on
# macOS 15: 34,240,800 of 34,326,760 bytes extracted, wrong SHA).
set -eu

ORT_VERSION="${ORT_VERSION:-1.24.2}"
RELEASE_TAG="${RELEASE_TAG:-onnxruntime-${ORT_VERSION}}"
BASE_URL="${BASE_URL:-https://github.com/bogdanr/fono-voice/releases/download}"

# Resolve the host target triple (the asset is per-triple).
TRIPLE="${TARGET:-}"
if [ -z "${TRIPLE}" ]; then
	if command -v rustc >/dev/null 2>&1; then
		TRIPLE="$(rustc -vV | awk '/^host:/ {print $2}')"
	else
		echo "fetch-onnxruntime: cannot determine target triple (no TARGET, no rustc)" >&2
		exit 1
	fi
fi

# Per-triple SHA-256 of the EXTRACTED static library. Add a row here when a
# new platform's library is published to the release.
#
# These values are the `raw_sha256` published in each `sha-<triple>.txt` asset
# on the `onnxruntime-<version>` release (that field names the EXTRACTED
# library, not the .xz). When the hosted libs are rebuilt (e.g. an ABI or
# link-flag change such as the static-libstdc++ fix), re-pin every row from the
# updated sha files: curl "$BASE_URL/$RELEASE_TAG/sha-<triple>.txt".
#
# WAKE-WORD REBUILD DONE (2026-06-24): the union ops.config
# (calibration/voice-models/ort/ops.config) includes the openWakeWord ops for
# the REAL upstream dscripka classifiers (hey_jarvis / alexa / hey_mycroft
# v0.1) -- Gemm(13), LayerNormalization, GreaterOrEqual, Clip, plus the
# melspectrogram/embedding ops (MaxPool, Log, Pow, ReduceMax, Max, Add, Div)
# and the com.microsoft FusedGemm contrib op. An earlier rebuild used a
# plain-MLP classifier whose op-set was too small (hey_jarvis.ort failed to
# load with "Could not find an implementation for Gemm(13)"). All five triples
# were rebuilt from the corrected config by fono-voice's build-onnxruntime
# workflow and re-published under RELEASE_TAG, and the rows below are re-pinned
# to the `raw_sha256` from each updated sha-<triple>.txt. These wake-capable
# libs load the full wake `.ort` stack (melspectrogram + embedding + the real
# upstream classifiers) as well as every voice model (the op-set is a superset
# of the voice-only one).
#
# SUPERTONIC REBUILD DONE (2026-07-14): the union ops.config now also covers
# the Supertonic 3 multilingual TTS graphs (int8, opset 19). Net-new vs the
# wake-capable set: Erf(13), BatchNormalization(15), PRelu(16), and the
# com.microsoft QLinearConv contrib op (introduced by the graph optimizer for
# the int8 Conv layers), plus int64_t type widenings on Clip/Div/Pow. All five
# triples were rebuilt by fono-voice's build-onnxruntime workflow and
# re-published under RELEASE_TAG; every row below is re-pinned to the new
# `raw_sha256`. This lib is a strict superset of the wake-capable one, so it
# still loads every existing voice + the wake stack, and now Supertonic too.
#
# SPEAKER REBUILD DONE (2026-07-19): shipping the ReDimNet2 speaker-
# verification embeddings (redimnet2-b3 / -b6, PalabraAI, MIT) unions three
# net-new operators into ../fono-voice/onnxruntime/ops.config:
# InstanceNormalization(6), ReduceProd(13, int64_t), and the com.microsoft
# FastGelu contrib op (verified locally via scripts/gen-ort-models.sh +
# scripts/merge-ort-configs.py; B3 and B6 are operator-identical). The minimal
# runtime was rebuilt from the unioned config by fono-voice's
# build-onnxruntime workflow and re-published under RELEASE_TAG; every row
# below is re-pinned to the new `raw_sha256` from each sha-<triple>.txt. This
# lib is a strict superset of the Supertonic-era one, so it still loads every
# voice + the wake stack + Supertonic, and now the ReDimNet2 speaker models.
sha_for_triple() {
	case "$1" in
	x86_64-unknown-linux-gnu)
		echo "3b324a9a46cd9f3b41fa4d051cf90303c069c6d99c4ac44bdde743b58fdbd451"
		;;
	aarch64-unknown-linux-gnu)
		echo "71421eb9a006d42e80f22ff8b86030569db76a934b0d567f8cbee636296530a5"
		;;
	aarch64-apple-darwin)
		echo "be9b1db2bbc2bdcd514032983da7d3db9f19557fb08772bead9489367463a67d"
		;;
	x86_64-apple-darwin)
		echo "5c9136439fe6d29f80f441d925bff725c75cd211501537888b42270bf7f931f2"
		;;
	x86_64-pc-windows-msvc)
		echo "d4a8b27573bc86a35655b2eb6f56c2cfe49f01d5fb6bfc37ad6ca4a8d3a0a857"
		;;
	*)
		echo ""
		;;
	esac
}

# The static-library filename is platform-specific: MSVC links onnxruntime.lib,
# every other target links libonnxruntime.a. `ort` resolves the correct name
# inside ORT_LIB_LOCATION automatically, so we only have to drop the right file.
case "${TRIPLE}" in
*-pc-windows-*)
	LIB_FILE="onnxruntime.lib"
	ASSET="onnxruntime-${TRIPLE}.lib.xz"
	;;
*)
	LIB_FILE="libonnxruntime.a"
	ASSET="libonnxruntime-${TRIPLE}.a.xz"
	;;
esac

EXPECTED_SHA="$(sha_for_triple "${TRIPLE}")"
if [ -z "${EXPECTED_SHA}" ]; then
	echo "fetch-onnxruntime: no hosted ${LIB_FILE} for triple '${TRIPLE}'" >&2
	echo "  build it from source instead: sh scripts/build-onnxruntime-minimal.sh" >&2
	exit 1
fi

URL="${BASE_URL}/${RELEASE_TAG}/${ASSET}"

# Cache under the workspace target dir by default so it survives across builds
# but is cleaned by `cargo clean`. Override with ORT_CACHE_DIR.
CACHE_DIR="${ORT_CACHE_DIR:-target/onnxruntime-${ORT_VERSION}/${TRIPLE}}"
LIB_PATH="${CACHE_DIR}/${LIB_FILE}"

sha256() {
	# $1 = file -> hex digest on stdout. Prefers coreutils sha256sum,
	# falls back to `shasum -a 256` (stock on macOS, which has no sha256sum).
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | cut -d' ' -f1
	else
		shasum -a 256 "$1" | cut -d' ' -f1
	fi
}

verify() {
	# $1 = file, $2 = expected sha256 -> 0 if match
	[ -f "$1" ] || return 1
	got="$(sha256 "$1")"
	[ "${got}" = "$2" ]
}

# Fast path: reuse a previously-verified library.
if verify "${LIB_PATH}" "${EXPECTED_SHA}"; then
	echo "fetch-onnxruntime: cached ${LIB_PATH} (sha ok)" >&2
	# Absolute path so ORT_LIB_LOCATION works from any cwd.
	( cd "${CACHE_DIR}" && pwd )
	exit 0
fi

for t in curl xz; do
	command -v "$t" >/dev/null 2>&1 || {
		echo "fetch-onnxruntime: missing required tool '$t'" >&2
		exit 1
	}
done
if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
	echo "fetch-onnxruntime: missing required tool 'sha256sum' (or 'shasum')" >&2
	exit 1
fi

mkdir -p "${CACHE_DIR}"
TMP_XZ="${CACHE_DIR}/${ASSET}.tmp"

echo "fetch-onnxruntime: downloading ${URL}" >&2
curl -fsSL -o "${TMP_XZ}" "${URL}"

echo "fetch-onnxruntime: extracting" >&2
xz -dc "${TMP_XZ}" > "${LIB_PATH}"
rm -f "${TMP_XZ}"

if ! verify "${LIB_PATH}" "${EXPECTED_SHA}"; then
	got="$(sha256 "${LIB_PATH}")"
	echo "fetch-onnxruntime: SHA-256 mismatch for ${LIB_PATH}" >&2
	echo "  expected ${EXPECTED_SHA}" >&2
	echo "  got      ${got}" >&2
	rm -f "${LIB_PATH}"
	exit 1
fi

echo "fetch-onnxruntime: verified ${LIB_PATH}" >&2
( cd "${CACHE_DIR}" && pwd )
