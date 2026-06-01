#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
#
# Regenerate the vendored espeak-ng G2P core under
# crates/fono-tts/assets/espeak-core/.
#
# Piper phonemizes via the pure-Rust `espeak-ng` crate, which needs a data
# directory. The full upstream `espeak-ng-data-phonemes` payload is ~2.3 MiB
# (mostly the 554 KB `phondata` waveform body plus `lang/`, `voices/` and
# `mbrola_ph/`), none of which the text-to-IPA path reads. Fono therefore
# vendors only the four files the phonemizer actually touches and embeds them
# in the binary via include_bytes! (see ADR 0033):
#
#   phontab      phoneme name/attribute table   (~58 KiB)
#   phonindex    phoneme bytecode index         (~43 KiB)
#   intonations  intonation contour data        (~2 KiB)
#   phondata     8-byte HEADER ONLY             (version magic + sample rate)
#
# The `phondata` body is dropped: the IPA renderer only reads bytes 0-7 of it
# (the VERSION_PHDATA magic and the 22.05 kHz sample rate). The full body is
# consumed solely by espeak's own synthesizer, which Fono never invokes — Piper
# generates the audio.
#
# Re-run this only when bumping the pinned `espeak-ng` data version, then
# commit the regenerated files. The data is espeak-ng's (GPL-3.0-or-later,
# compatible with Fono's GPL-3.0-only).
#
# Usage: scripts/gen-espeak-core.sh
set -eu

ESPEAK_DATA_VERSION="${ESPEAK_DATA_VERSION:-0.1.0}"
DEST="$(CDPATH= cd "$(dirname "$0")/.." && pwd)/crates/fono-tts/assets/espeak-core"

# Locate the unpacked espeak-ng-data-phonemes crate in the Cargo registry. A
# `cargo fetch` in the workspace populates it; we read its `data/` payload.
PH_SRC=$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -maxdepth 1 -type d \
    -name "espeak-ng-data-phonemes-${ESPEAK_DATA_VERSION}" 2>/dev/null | head -1)

if [ -z "$PH_SRC" ] || [ ! -d "$PH_SRC/data" ]; then
    echo "error: espeak-ng-data-phonemes-${ESPEAK_DATA_VERSION} not found in the" >&2
    echo "       Cargo registry. Run a build with the 'tts-local' feature (or" >&2
    echo "       'cargo fetch') first so the crate is unpacked, then retry." >&2
    exit 1
fi

mkdir -p "$DEST"
for f in phontab phonindex intonations; do
    cp "$PH_SRC/data/$f" "$DEST/$f"
done
# phondata: keep only the 8-byte header (4-byte VERSION_PHDATA + 4-byte rate).
head -c 8 "$PH_SRC/data/phondata" > "$DEST/phondata"

echo "regenerated espeak G2P core in $DEST:"
ls -l "$DEST"
echo "phondata header:"
od -An -tx1 "$DEST/phondata"
