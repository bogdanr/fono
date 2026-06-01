#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
#
# Stage the per-language espeak-ng phonemizer dictionaries for the fono-voice
# mirror and print the catalog `dicts` array to paste into
# crates/fono-tts/voices/catalog.json.
#
# The shared G2P core (phontab/phonindex/intonations + an 8-byte phondata stub,
# ~102 KiB) is embedded in the binary by scripts/gen-espeak-core.sh. Everything
# language-specific lives in the `<lang>_dict` files, which range from ~50 KiB
# (most languages) to 8.5 MiB (Russian) — far too much to bundle, so they are
# hosted on the mirror and downloaded on first use (ADR 0033). This script
# collects those dict files and emits their pinned SHA-256 + size.
#
# `lang` is the espeak-ng voice code (the dict file's prefix), which matches the
# `espeak.voice` field of a voice's .onnx.json. That is NOT always the BCP-47
# language code (Chinese is `cmn`, Norwegian Bokmal is `nb`/`no`); the catalog
# keys dicts by this espeak code so ensure_voice_dict can resolve them directly.
#
# Upload everything under OUT_DIR to the mirror release tagged "$DICT_TAG",
# then replace the `dicts` array in catalog.json with the printed JSON.
#
# Usage: scripts/gen-espeak-dicts.sh [OUT_DIR]
set -eu

DICT_TAG="${DICT_TAG:-espeak-ng-1.52}"
OUT_DIR="${1:-$(CDPATH= cd "$(dirname "$0")/.." && pwd)/tmp/espeak-dicts}"
REG="${CARGO_HOME:-$HOME/.cargo}/registry/src"

# Each language's data ships as its own `espeak-ng-data-dict-<lang>` crate; a
# build with the matching bundled-data features (or `cargo fetch`) unpacks them.
crates=$(find "$REG" -maxdepth 1 -type d -name 'espeak-ng-data-dict-*' 2>/dev/null | sort)
if [ -z "$crates" ]; then
    echo "error: no espeak-ng-data-dict-* crates found in $REG." >&2
    echo "       Run a build that enables the bundled-data-<lang> features (or" >&2
    echo "       'cargo fetch') so the dict crates are unpacked, then retry." >&2
    exit 1
fi

mkdir -p "$OUT_DIR"
entries=""
for crate in $crates; do
    # The single payload file is named "<lang>_dict".
    dict=$(find "$crate/data" -maxdepth 1 -type f -name '*_dict' 2>/dev/null | head -1)
    [ -n "$dict" ] || continue
    file=$(basename "$dict")
    lang=${file%_dict}
    cp "$dict" "$OUT_DIR/$file"
    sha=$(sha256sum "$OUT_DIR/$file" | cut -d' ' -f1)
    size=$(wc -c < "$OUT_DIR/$file" | tr -d ' ')
    entry=$(printf '    {\n      "lang": "%s",\n      "release_tag": "%s",\n      "file": "%s",\n      "sha256": "%s",\n      "size": %s\n    }' \
        "$lang" "$DICT_TAG" "$file" "$sha" "$size")
    if [ -z "$entries" ]; then entries="$entry"; else entries="$entries,\n$entry"; fi
done

echo "staged dict assets in $OUT_DIR:"
ls -l "$OUT_DIR"
echo
echo "Upload the above to the mirror release tagged '$DICT_TAG', then set the"
echo "\"dicts\" array in crates/fono-tts/voices/catalog.json to:"
echo
printf '  "dicts": [\n%b\n  ]\n' "$entries"
