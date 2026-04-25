#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
# Fetch + transcode the fono-bench fixture set.
#
# Each fixture in `crates/fono-bench/src/fixtures.rs` declares an HTTPS
# URL pointing at a public-domain audio source (typically a LibriVox
# chapter on archive.org) and a *reference transcript* of one short
# excerpt within that recording. This script is the contract between
# those URLs and the on-disk WAVs that the runner compares SHA-256s
# against.
#
# Maintainer workflow (run once when adding/changing fixtures):
#   1. For each fixture id, identify the start time + duration in the
#      source recording where the canonical transcript is spoken.
#      Encode them as TSV rows in `fixtures.tsv` (id, url, start_seconds,
#      duration_seconds).
#   2. Run this script. It downloads each source, runs:
#         ffmpeg -ss <start> -t <dur> -ac 1 -ar 16000 -c:a pcm_s16le \
#                <id>.wav
#      and prints the resulting SHA-256s.
#   3. Paste the SHA-256s into `fixtures.rs` (replacing UNPINNED).
#   4. Commit the fixture diff.
#
# CI never runs this script — it consumes the pinned SHA-256s and trusts
# the cache.
#
# Requirements: curl, ffmpeg, sha256sum.

set -eu

OUT_DIR="${OUT_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/fono/bench}"
TSV="${TSV:-$(dirname "$0")/fixtures.tsv}"

if [ ! -f "$TSV" ]; then
    cat <<EOF
$TSV is missing. Create it with one TSV row per fixture:

    id<TAB>url<TAB>start_seconds<TAB>duration_seconds

For example:

    en_alice_01<TAB>https://archive.org/download/alice_in_wonderland_librivox/wonderland_ch_01_carroll.mp3<TAB>3.5<TAB>6.4

Then re-run this script. The pinned URLs in fixtures.rs are the source
URLs for these rows.
EOF
    exit 2
fi

mkdir -p "$OUT_DIR"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

while IFS="$(printf '\t')" read -r id url start dur; do
    case "$id" in '#'*|'') continue ;; esac
    out="$OUT_DIR/$id.wav"
    src="$TMP/$id.src"
    echo "==> $id  ($url, +${start}s, ${dur}s)"
    if [ ! -f "$out" ]; then
        curl -fSL --retry 3 -o "$src" "$url"
        ffmpeg -nostdin -loglevel error -y \
            -ss "$start" -t "$dur" \
            -i "$src" \
            -ac 1 -ar 16000 -c:a pcm_s16le \
            "$out"
    else
        echo "    cached at $out"
    fi
    sha=$(sha256sum "$out" | cut -d' ' -f1)
    echo "    sha256 = $sha"
done < "$TSV"

echo
echo "Done. Paste the sha256 values into crates/fono-bench/src/fixtures.rs"
echo "(replacing UNPINNED for each id) and commit."
