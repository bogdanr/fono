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
# Prereqs (host tooling, NOT shipped): curl, xz, sha256sum.
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
sha_for_triple() {
	case "$1" in
	x86_64-unknown-linux-gnu)
		echo "9b084ea566faac4e78c54187b61014bcb8c3986abc974d8b284e4b868c39ac34"
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

verify() {
	# $1 = file, $2 = expected sha256 -> 0 if match
	[ -f "$1" ] || return 1
	got="$(sha256sum "$1" | cut -d' ' -f1)"
	[ "${got}" = "$2" ]
}

# Fast path: reuse a previously-verified library.
if verify "${LIB_PATH}" "${EXPECTED_SHA}"; then
	echo "fetch-onnxruntime: cached ${LIB_PATH} (sha ok)" >&2
	# Absolute path so ORT_LIB_LOCATION works from any cwd.
	( cd "${CACHE_DIR}" && pwd )
	exit 0
fi

for t in curl xz sha256sum; do
	command -v "$t" >/dev/null 2>&1 || {
		echo "fetch-onnxruntime: missing required tool '$t'" >&2
		exit 1
	}
done

mkdir -p "${CACHE_DIR}"
TMP_XZ="${CACHE_DIR}/${ASSET}.tmp"

echo "fetch-onnxruntime: downloading ${URL}" >&2
curl -fsSL -o "${TMP_XZ}" "${URL}"

echo "fetch-onnxruntime: extracting" >&2
xz -dc "${TMP_XZ}" > "${LIB_PATH}"
rm -f "${TMP_XZ}"

if ! verify "${LIB_PATH}" "${EXPECTED_SHA}"; then
	got="$(sha256sum "${LIB_PATH}" | cut -d' ' -f1)"
	echo "fetch-onnxruntime: SHA-256 mismatch for ${LIB_PATH}" >&2
	echo "  expected ${EXPECTED_SHA}" >&2
	echo "  got      ${got}" >&2
	rm -f "${LIB_PATH}"
	exit 1
fi

echo "fetch-onnxruntime: verified ${LIB_PATH}" >&2
( cd "${CACHE_DIR}" && pwd )
