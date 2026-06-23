#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
# Fono wake-word: one-time OFFLINE training pipeline for an openWakeWord
# per-phrase classifier (Phase B of
# plans/2026-06-23-wake-word-openwakeword-v2.md).
#
# Fono SYNTHESIZES ITS OWN POSITIVES. Positive clips of the wake phrase are
# produced by `fono speak` across the ACTIVE TTS backend's palette voices, so
# the same engine you ship is the one that makes the training audio — local
# (Piper/Kokoro, clean-license) OR cloud (OpenAI/Gemini/ElevenLabs/…). The
# driver classifies the run from that backend:
#
#   * LOCAL backend (`local`)  -> CLEAN: may train the shippable default
#     "hey_fono". On-device voices, no network, clean license.
#   * CLOUD backend (anything that needs an API key) -> PRIVATE: proprietary,
#     ToS-bound audio that must NEVER feed "hey_fono"; the model is marked
#     PRIVATE (PROVENANCE.txt) and you must accept the provider terms
#     (CLOUD_TTS_ACCEPT_TERMS=1).
#
# Switch the synthesizer with Fono's own config, e.g.:
#   fono use tts local       # clean, on-device Piper/Kokoro
#   fono use tts openai      # cloud (PRIVATE models only)
#
# End-to-end flow:
#   1. Synthesize POSITIVE clips with `fono speak` (one per palette voice).
#   2. AUGMENT them (speed + gain) up to N_POSITIVE in the python trainer.
#   3. Assemble NEGATIVE / background audio (openly-licensed for hey_fono;
#      an auto-downloaded TESTING corpus for PRIVATE custom keywords).
#   4. Compute features through the FROZEN Apache graphs
#      (melspectrogram.onnx + the Apache Google speech_embedding backbone).
#   5. Train the small per-phrase classifier and export <MODEL_ID>.onnx.
#   6. Convert to .ort via the EXISTING scripts/gen-ort-models.sh, emitting
#      melspectrogram.ort / embedding.ort / <MODEL_ID>.ort with the basenames
#      the Phase-G registry expects (crates/fono-audio/src/wake_registry.rs).
#
# IMPORTANT — this is a HOST tool, NOT shipped in the binary, and it does NOT
# install anything. Steps that need an external tool (a built `fono`, the
# openwakeword python package, torch, a venv) DETECT-and-INSTRUCT: they print
# the exact command for you to run and exit, rather than auto-installing (cf.
# AGENTS.md: never install system packages). Nothing here fabricates a model or
# a SHA-256 — once you run this for real and host the output, the only remaining
# human step is uploading the files and pinning their real SHA-256 in
# crates/fono-audio/src/wake_registry.rs (leave the zero sentinels until then).
#
# Easiest PRIVATE custom keyword (uses your configured CLOUD backend):
#   fono use tts openai     # (once) point Fono at a cloud TTS backend
#   MODEL_ID=house PHRASE="house" CLOUD_TTS_ACCEPT_TERMS=1 \
#     sh scripts/train-wakeword-model.sh
#
# Clean, shippable default (on-device voices):
#   fono use tts local      # (once) Piper/Kokoro on-device
#   NEGATIVE_AUDIO_DIR=/path/to/open-negatives \
#     sh scripts/train-wakeword-model.sh
#
# Env-var inputs (defaults in parentheses):
#   PHRASE                 ("hey fono")  spoken wake phrase to synthesize.
#   MODEL_ID               ("hey_fono")  registry id; sets output basenames.
#                          Any id other than hey_fono is a PRIVATE model.
#   FONO_BIN               (auto)  the built `fono` binary used to synthesize
#                          positives. Auto-detects target/release/fono,
#                          target/debug/fono, then `fono` on PATH.
#   FONO_VOICES            (unset)  explicit voice labels to synthesize with,
#                          ';' or ',' separated (e.g. "Female 1;Male 2").
#                          Defaults to every voice in `fono voices list`.
#   CLOUD_TTS_ACCEPT_TERMS (unset)  must be "1" when the active backend is a
#                          CLOUD backend: you accept the provider ToS and that
#                          the model is PRIVATE / non-shippable.
#   FONO_TTS_CLEAN         (unset)  set "1" to assert a `wyoming` backend is a
#                          local, clean-license relay (treated as CLEAN).
#   OWW_GRAPHS_DIR         (calibration/wakeword/graphs)  frozen Apache graphs
#                          melspectrogram.onnx + embedding_model.onnx; auto-
#                          fetched (Apache-2.0, openWakeWord v0.5.1) if absent.
#   NEGATIVE_AUDIO_DIR     (unset)  dir of openly-licensed negative WAVs. For
#                          PRIVATE models, falls back to an auto-downloaded
#                          TESTING corpus if unset. REQUIRED for hey_fono.
#   NEGATIVE_FEATURES_DIR  (unset)  dir of pre-computed negative .npy features.
#   NEGATIVES_URL          (Speech Commands test set)  .tar.gz used for the
#                          PRIVATE-model TESTING negatives fallback.
#   N_POSITIVE             (2000)   target positive clips after augmentation.
#   N_VALIDATION           (200)    held-out positive clips for the metrics bar.
#   WORK_DIR               (calibration/wakeword/work)  scratch.
#   OUT_DIR                (calibration/wakeword/out)    final .onnx + .ort.
#   PYTHON                 (auto)   auto-detects .venv-wakeword/bin/python.
#   DRY_RUN                (unset)  "1" prints the resolved backend + voices +
#                          plan and stops (no synthesis, no training).
#
# Recommended OPENLY-LICENSED negative corpora for hey_fono (you fetch + verify
# the license yourself; this script will NOT auto-download them for hey_fono):
#   - Free Music Archive (FMA) — keep only CC-BY / CC-BY-SA / CC0 tracks.
#   - Mozilla Common Voice — CC0.  - Freesound CC0/CC-BY ambient/TV noise.
#   - MUSAN (noise/music/speech) — CC-BY-4.0. https://www.openslr.org/17/
#   See calibration/wakeword/README.md.
set -eu

usage() {
    cat <<'EOF'
train-wakeword-model.sh — offline trainer for openWakeWord wake-word models.

Fono synthesizes its OWN positives via `fono speak` across the ACTIVE TTS
backend's voices. All inputs are ENV VARS (there are no positional args).

  # Easiest PRIVATE custom keyword (point Fono at a cloud backend first):
  fono use tts openai
  MODEL_ID=house PHRASE="house" CLOUD_TTS_ACCEPT_TERMS=1 \
    sh scripts/train-wakeword-model.sh

  # Preview the resolved backend + voices + plan only (no synth, no training):
  MODEL_ID=house PHRASE="house" DRY_RUN=1 sh scripts/train-wakeword-model.sh

  # Clean, shippable default (on-device voices + a REAL negative corpus):
  fono use tts local
  NEGATIVE_AUDIO_DIR=/path/to/open-negatives sh scripts/train-wakeword-model.sh

Key env vars (defaults in parentheses):
  PHRASE ("hey fono")            spoken wake phrase.
  MODEL_ID ("hey_fono")          registry id / output basename. Any id other
                                 than hey_fono is a PRIVATE model.
  FONO_BIN (auto)                built `fono` used to synthesize positives.
  FONO_VOICES (all palette)      ';'/',' separated voice labels to use.
  CLOUD_TTS_ACCEPT_TERMS=1       required when the active backend is CLOUD.
  FONO_TTS_CLEAN=1               treat a `wyoming` backend as clean/local.
  OWW_GRAPHS_DIR (calibration/wakeword/graphs)   auto-fetched if absent.
  NEGATIVE_AUDIO_DIR / NEGATIVE_FEATURES_DIR     negatives (auto TESTING set
                                 for PRIVATE models; required for hey_fono).
  N_POSITIVE (2000) N_VALIDATION (200)
  WORK_DIR / OUT_DIR             scratch / outputs (under calibration/wakeword).
  PYTHON (auto)                  auto-detects .venv-wakeword.
  DRY_RUN=1                      print the plan and stop.

See calibration/wakeword/README.md for the full guide and licensing rules.
EOF
}
case "${1:-}" in
    -h | --help | help) usage; exit 0 ;;
esac

PHRASE="${PHRASE:-hey fono}"
MODEL_ID="${MODEL_ID:-hey_fono}"
REPO_ROOT="$(CDPATH= cd "$(dirname "$0")/.." && pwd)"
OWW_GRAPHS_DIR="${OWW_GRAPHS_DIR:-${REPO_ROOT}/calibration/wakeword/graphs}"
WORK_DIR="${WORK_DIR:-${REPO_ROOT}/calibration/wakeword/work}"
OUT_DIR="${OUT_DIR:-${REPO_ROOT}/calibration/wakeword/out}"
N_POSITIVE="${N_POSITIVE:-2000}"
N_VALIDATION="${N_VALIDATION:-200}"
# Prefer the repo's training venv if it exists, so you do NOT have to activate
# it: once you run the one-time `pip install` the script prints, the deps live
# in .venv-wakeword and are picked up automatically on the next run.
if [ -z "${PYTHON:-}" ] && [ -x "${REPO_ROOT}/.venv-wakeword/bin/python" ]; then
    PYTHON="${REPO_ROOT}/.venv-wakeword/bin/python"
else
    PYTHON="${PYTHON:-python3}"
fi
NEGATIVE_AUDIO_DIR="${NEGATIVE_AUDIO_DIR:-}"
NEGATIVE_FEATURES_DIR="${NEGATIVE_FEATURES_DIR:-}"
# Negative-corpus auto-download (TESTING ONLY — license NOT verified). For
# PRIVATE models, when no negatives are supplied the driver downloads this
# corpus into calibration/wakeword/negatives so a detector can be trained
# without hand-assembling audio. Override to point at any .tar.gz of audio.
NEGATIVES_URL="${NEGATIVES_URL:-http://download.tensorflow.org/data/speech_commands_test_set_v0.02.tar.gz}"
FONO_BIN="${FONO_BIN:-}"
FONO_VOICES="${FONO_VOICES:-}"
FONO_TTS_CLEAN="${FONO_TTS_CLEAN:-}"
CLOUD_TTS_ACCEPT_TERMS="${CLOUD_TTS_ACCEPT_TERMS:-}"
DRY_RUN="${DRY_RUN:-}"

say() { echo "[train-wakeword] $*"; }
die() { echo "[train-wakeword] ERROR: $*" >&2; exit 1; }

# True if $1 is a directory holding at least one audio file (any depth).
_dir_has_audio() {
    [ -d "$1" ] || return 1
    _found="$(find "$1" -type f \( -name '*.wav' -o -name '*.flac' -o -name '*.ogg' \) 2>/dev/null | head -n 1 || true)"
    [ -n "$_found" ]
}

# Download + extract the TESTING negative corpus into $1. The license is NOT
# verified: this is for local experimentation only — never ship or pin a model
# trained on it. Set NEGATIVES_URL to use a different .tar.gz of audio.
fetch_negatives() {
    _neg_dir="$1"
    command -v curl >/dev/null 2>&1 || die "curl is required to auto-download negatives"
    mkdir -p "$_neg_dir"
    say "downloading a negative corpus for TESTING (license NOT verified):"
    say "  $NEGATIVES_URL"
    say "  -> $_neg_dir  (never ship or pin a model trained on this)"
    _neg_tar="$_neg_dir/.download.tar.gz"
    curl -fL --progress-bar -o "$_neg_tar" "$NEGATIVES_URL" || die "negative-corpus download failed"
    say "extracting (this can take a minute)..."
    tar -xzf "$_neg_tar" -C "$_neg_dir" || die "negative-corpus extraction failed"
    rm -f "$_neg_tar"
    # Avoid poisoning: drop any top-level folder named exactly like the phrase
    # (some speech corpora include the very word we train as the positive).
    _phrase_dir="$_neg_dir/$(printf '%s' "$PHRASE" | tr 'A-Z' 'a-z' | tr -d ' ')"
    if [ -d "$_phrase_dir" ]; then
        rm -rf "$_phrase_dir"
        say "removed '$_phrase_dir' (it matches the wake phrase)"
    fi
    _neg_count="$(find "$_neg_dir" -type f -name '*.wav' 2>/dev/null | wc -l | tr -d ' ')"
    say "negative corpus ready: $_neg_count wav file(s)"
}

# Emit the voice labels to synthesize with, one per line. Explicit FONO_VOICES
# wins; otherwise parse the gender-labelled palette from `fono voices list`.
enumerate_voices() {
    if [ -n "$FONO_VOICES" ]; then
        printf '%s\n' "$FONO_VOICES" | tr ';,' '\n\n' | sed '/^[[:space:]]*$/d'
        return
    fi
    "$FONO_BIN" voices list 2>/dev/null |
        sed -n 's/^  \(Female [0-9][0-9]*\|Male [0-9][0-9]*\).*/\1/p'
}

# --- 0+1. Locate a WORKING fono binary and resolve the active TTS backend --
# A candidate is only usable if `fono use show` actually succeeds: a STALE
# build (e.g. an old target/release/fono) can fail to parse a newer config and
# would otherwise be picked over a current target/debug/fono. So we probe each
# candidate and keep the first whose `use show` reports a TTS backend. With
# FONO_BIN set, that is the only candidate. FONO_SHOW_OUT keeps the last
# `use show` output (incl. errors) for diagnostics.
FONO_BIN_OK=""
TTS_BACKEND=""
FONO_SHOW_OUT=""
try_fono() { # $1 = candidate; on success sets FONO_BIN_OK + TTS_BACKEND
    [ -n "$1" ] && [ -x "$1" ] || return 1
    _out="$("$1" use show 2>&1)" || { FONO_SHOW_OUT="$_out"; return 1; }
    _b="$(printf '%s\n' "$_out" |
        sed -n 's/^[[:space:]]*tts[[:space:]]*:[[:space:]]*\([^[:space:]]*\).*/\1/p' | head -n 1)"
    [ -n "$_b" ] || { FONO_SHOW_OUT="$_out"; return 1; }
    FONO_BIN_OK="$1"; TTS_BACKEND="$_b"; FONO_SHOW_OUT="$_out"; return 0
}
if [ -n "$FONO_BIN" ]; then
    try_fono "$FONO_BIN" || true
else
    _path_fono="$(command -v fono 2>/dev/null || true)"
    for _cand in "$REPO_ROOT/target/release/fono" "$REPO_ROOT/target/debug/fono" "$_path_fono"; do
        try_fono "$_cand" && break
    done
fi
if [ -z "$FONO_BIN_OK" ]; then
    if [ -n "$FONO_BIN" ]; then
        say "the fono binary '$FONO_BIN' could not report its active TTS backend."
    else
        say "no working fono binary found (tried target/release, target/debug, PATH)."
    fi
    if [ -n "$FONO_SHOW_OUT" ]; then
        say "'fono use show' said:"
        printf '%s\n' "$FONO_SHOW_OUT" | sed 's/^/    /'
    fi
    say "This usually means the binary is STALE relative to your config (rebuild"
    say "it) or no TTS backend is selected. Fixes:"
    say "  cargo build --release --features tts-local   # rebuild a current fono"
    say "  fono use tts local                           # pick a backend"
    say "  # or point FONO_BIN at a known-good binary"
    die "could not determine the active TTS backend"
fi
FONO_BIN="$FONO_BIN_OK"
say "using fono binary: $FONO_BIN"

# Classify the resolved backend. CLEAN = local on-device voices (may train the
# shippable default). CLOUD = anything that needs an API key (PRIVATE models
# only). `wyoming` is ambiguous (could relay a local Piper), so it is CLOUD
# unless FONO_TTS_CLEAN=1.
case "$TTS_BACKEND" in
    local) BACKEND_CLASS="clean" ;;
    none | "") die "TTS backend is 'none'; pick one first, e.g. 'fono use tts local'" ;;
    wyoming) [ "$FONO_TTS_CLEAN" = "1" ] && BACKEND_CLASS="clean" || BACKEND_CLASS="cloud" ;;
    *) BACKEND_CLASS="cloud" ;;
esac
say "active TTS backend: $TTS_BACKEND ($BACKEND_CLASS)"

# Licensing guardrails. The shippable clean default must come from CLEAN,
# on-device voices; CLOUD audio is proprietary/ToS-bound and PRIVATE-only.
IS_PRIVATE=0
if [ "$BACKEND_CLASS" = "cloud" ]; then
    IS_PRIVATE=1
    case " hey_fono " in
        *" $MODEL_ID "*)
            say "MODEL_ID='$MODEL_ID' is the shippable clean-license default, but the"
            say "active TTS backend '$TTS_BACKEND' is a CLOUD backend. Cloud audio is"
            say "proprietary and provider terms typically forbid training on it, so it"
            say "must NEVER feed hey_fono. Switch to on-device voices first:"
            say "  fono use tts local"
            die "refusing to train hey_fono from a cloud TTS backend"
            ;;
    esac
    if [ "$CLOUD_TTS_ACCEPT_TERMS" != "1" ] && [ "$DRY_RUN" != "1" ]; then
        say "the active TTS backend '$TTS_BACKEND' is a CLOUD backend: its audio is"
        say "NOT clean-license and NOT redistributable, the resulting model is PRIVATE,"
        say "and you must comply with the provider's terms of service (many forbid"
        say "training on their output). Set CLOUD_TTS_ACCEPT_TERMS=1 once you accept this."
        die "cloud TTS terms not accepted"
    fi
fi

# --- 2. Resolve the synthesis voices --------------------------------------
VOICES="$(enumerate_voices || true)"
N_VOICES="$(printf '%s\n' "$VOICES" | sed '/^[[:space:]]*$/d' | wc -l | tr -d ' ')"
if [ "$N_VOICES" -eq 0 ]; then
    say "no palette voices resolved — will use the backend DEFAULT voice (1 clip)."
else
    say "synthesis voices ($N_VOICES): $(printf '%s' "$VOICES" | paste -sd ',' - 2>/dev/null || echo "$VOICES")"
fi

# --- DRY RUN: print the plan and stop -------------------------------------
if [ "$DRY_RUN" = "1" ]; then
    say "----"
    say "DRY RUN — plan only (no synthesis, no training):"
    say "  phrase            : \"$PHRASE\""
    say "  model id          : $MODEL_ID$( [ "$IS_PRIVATE" -eq 1 ] && echo '  (PRIVATE)' || echo '  (clean default)')"
    say "  tts backend       : $TTS_BACKEND ($BACKEND_CLASS)"
    say "  base voices       : ${N_VOICES} (augmented up to N_POSITIVE=$N_POSITIVE)"
    say "  graphs dir        : $OWW_GRAPHS_DIR"
    say "done (dry run)."
    exit 0
fi

# --- 3. Detect the frozen Apache graphs (auto-fetch; Apache-2.0) ----------
MELSPEC_ONNX="$OWW_GRAPHS_DIR/melspectrogram.onnx"
EMBED_ONNX="$OWW_GRAPHS_DIR/embedding_model.onnx"
if [ ! -f "$MELSPEC_ONNX" ] || [ ! -f "$EMBED_ONNX" ]; then
    # The shared graphs are Apache-2.0 (openWakeWord v0.5.1) and clean to
    # redistribute, so — unlike negatives — we CAN fetch them automatically.
    say "frozen Apache graphs not found in $OWW_GRAPHS_DIR — fetching them"
    say "(Apache-2.0, openWakeWord v0.5.1; clean-license, safe to download)."
    command -v curl >/dev/null 2>&1 || die "missing frozen graphs (and curl unavailable to fetch them)"
    mkdir -p "$OWW_GRAPHS_DIR"
    _oww_base="https://github.com/dscripka/openWakeWord/releases/download/v0.5.1"
    curl -fLo "$MELSPEC_ONNX" "$_oww_base/melspectrogram.onnx" ||
        die "failed to download melspectrogram.onnx"
    curl -fLo "$EMBED_ONNX" "$_oww_base/embedding_model.onnx" ||
        die "failed to download embedding_model.onnx"
    say "fetched melspectrogram.onnx + embedding_model.onnx"
fi

# --- 4. Resolve negatives -------------------------------------------------
if [ -z "$NEGATIVE_AUDIO_DIR" ] && [ -z "$NEGATIVE_FEATURES_DIR" ]; then
    if [ "$MODEL_ID" != "hey_fono" ]; then
        # PRIVATE model: auto-provide negatives so a custom keyword trains out
        # of the box. Download a TESTING corpus if the dir holds no audio yet.
        NEGATIVE_AUDIO_DIR="$REPO_ROOT/calibration/wakeword/negatives"
        if _dir_has_audio "$NEGATIVE_AUDIO_DIR"; then
            say "using existing negatives in $NEGATIVE_AUDIO_DIR"
        else
            fetch_negatives "$NEGATIVE_AUDIO_DIR"
        fi
        say "NOTE: these negatives are for TESTING (license unverified); a"
        say "shippable detector needs an openly-licensed corpus you verify."
    else
        say "No negatives provided. Set NEGATIVE_AUDIO_DIR (raw clips) or"
        say "NEGATIVE_FEATURES_DIR (pre-computed .npy features)."
        say "The clean default 'hey_fono' must use OPENLY-LICENSED corpora you"
        say "verify (FMA CC-BY/CC0, Common Voice CC0, MUSAN CC-BY-4.0); this"
        say "script will NOT auto-download unverified audio for the default."
        die "negative corpus not provided"
    fi
fi
if [ -n "$NEGATIVE_AUDIO_DIR" ] && [ ! -d "$NEGATIVE_AUDIO_DIR" ]; then
    die "NEGATIVE_AUDIO_DIR=$NEGATIVE_AUDIO_DIR is not a directory"
fi
if [ -n "$NEGATIVE_FEATURES_DIR" ] && [ ! -d "$NEGATIVE_FEATURES_DIR" ]; then
    die "NEGATIVE_FEATURES_DIR=$NEGATIVE_FEATURES_DIR is not a directory"
fi

# --- 5. Detect the python training deps (detect, don't install) -----------
if ! "$PYTHON" -c 'import openwakeword' >/dev/null 2>&1; then
    say "python package 'openwakeword' not importable with $PYTHON."
    say "Create the training venv ONCE (NOT installed by this script); the"
    say "script auto-detects .venv-wakeword on the next run — no activation:"
    say "  python3 -m venv ${REPO_ROOT}/.venv-wakeword"
    say "  ${REPO_ROOT}/.venv-wakeword/bin/pip install \\"
    say "    openwakeword torch onnx onnxruntime==1.24.2"
    say "Then re-run this script (it will use that venv automatically)."
    say "Or point PYTHON=/path/to/python at an env that already has them."
    die "openwakeword not available"
fi

mkdir -p "$WORK_DIR" "$OUT_DIR"
POSITIVES_DIR="$WORK_DIR/positives"
rm -rf "$POSITIVES_DIR"
mkdir -p "$POSITIVES_DIR"

# --- 6. Synthesize POSITIVES with `fono speak` ----------------------------
# One clip per palette voice (the trainer augments these up to N_POSITIVE).
say "synthesizing positives for \"$PHRASE\" with fono ($TTS_BACKEND)"
if [ "$N_VOICES" -eq 0 ]; then
    printf '%s\n' "$PHRASE" | "$FONO_BIN" speak stream --out "$POSITIVES_DIR/voice_000.wav" ||
        die "fono speak failed (backend '$TTS_BACKEND'); check 'fono use show' + keys"
else
    _i=0
    printf '%s\n' "$VOICES" | sed '/^[[:space:]]*$/d' | while IFS= read -r _v; do
        _v="$(printf '%s' "$_v" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
        [ -n "$_v" ] || continue
        _out="$POSITIVES_DIR/voice_$(printf '%03d' "$_i").wav"
        if printf '%s\n' "$PHRASE" | "$FONO_BIN" speak stream --voice "$_v" --out "$_out" 2>/dev/null; then
            say "  synthesized: $_v"
        else
            say "  WARNING: synth failed for voice '$_v' (skipped)"
        fi
        _i=$((_i + 1))
    done
fi
_pos_count="$(find "$POSITIVES_DIR" -type f -name '*.wav' 2>/dev/null | wc -l | tr -d ' ')"
[ "$_pos_count" -gt 0 ] || die "fono produced no positive clips (backend '$TTS_BACKEND')"
say "synthesized $_pos_count base positive clip(s) into $POSITIVES_DIR"

# --- 6b. PRIVATE-model provenance -----------------------------------------
if [ "$IS_PRIVATE" -eq 1 ]; then
    {
        echo "PRIVATE wake-word model — NOT clean-license, NOT redistributable."
        echo "phrase:      $PHRASE"
        echo "model_id:    $MODEL_ID"
        echo "tts_backend: $TTS_BACKEND (cloud / proprietary)"
        echo "voices:      ${VOICES:-<backend default>}"
        echo ""
        echo "Positives were synthesized with a CLOUD TTS backend. That provider's"
        echo "terms of service govern the audio; many forbid using generated speech"
        echo "to train models. You are responsible for compliance. Do NOT ship or"
        echo "redistribute this model or the audio it was trained on."
    } >"$OUT_DIR/PROVENANCE.txt"
    say "wrote PRIVATE-model provenance notice: $OUT_DIR/PROVENANCE.txt"
fi

# --- 7. Train + export the classifier .onnx (python glue) -----------------
# Feature extraction through the frozen graphs, augmentation, training and ONNX
# export live in scripts/wakeword_train.py.
say "training '$MODEL_ID' for phrase \"$PHRASE\" (positives target=$N_POSITIVE)"
"$PYTHON" "$REPO_ROOT/scripts/wakeword_train.py" \
    --phrase "$PHRASE" \
    --model-id "$MODEL_ID" \
    --melspec "$MELSPEC_ONNX" \
    --embedding "$EMBED_ONNX" \
    --positives-dir "$POSITIVES_DIR" \
    --negative-audio-dir "$NEGATIVE_AUDIO_DIR" \
    --negative-features-dir "$NEGATIVE_FEATURES_DIR" \
    --n-positive "$N_POSITIVE" \
    --n-validation "$N_VALIDATION" \
    --work-dir "$WORK_DIR" \
    --out-dir "$OUT_DIR"

CLS_ONNX="$OUT_DIR/${MODEL_ID}.onnx"
[ -f "$CLS_ONNX" ] || die "training did not produce $CLS_ONNX"

# --- 8. Convert .onnx -> .ort via the EXISTING pipeline -------------------
# Stage the three graphs under one dir with the EXACT basenames the registry
# expects, so gen-ort-models.sh emits melspectrogram.ort / embedding.ort /
# <MODEL_ID>.ort. ALLOW_PARTIAL=1 because this is the wake-word subset, not the
# full voice union (the Kokoro-union guard does not apply here).
ORT_STAGE="$WORK_DIR/ort-stage"
rm -rf "$ORT_STAGE"
mkdir -p "$ORT_STAGE"
cp "$MELSPEC_ONNX" "$ORT_STAGE/melspectrogram.onnx"
cp "$EMBED_ONNX" "$ORT_STAGE/embedding.onnx"   # registry expects embedding.ort
cp "$CLS_ONNX" "$ORT_STAGE/${MODEL_ID}.onnx"

say "converting graphs to .ort via scripts/gen-ort-models.sh (ALLOW_PARTIAL=1)"
PYTHON="$PYTHON" ALLOW_PARTIAL=1 MODELS_DIR="$ORT_STAGE" OUT_DIR="$OUT_DIR/ort" \
    sh "$REPO_ROOT/scripts/gen-ort-models.sh"

say "----"
say "outputs in $OUT_DIR/ort/ :"
say "  melspectrogram.ort   (shared, Apache-2.0 -> registry MELSPEC)"
say "  embedding.ort        (shared, Apache-2.0 -> registry EMBEDDING)"
say "  ${MODEL_ID}.ort      (classifier -> registry '${MODEL_ID}' entry)"
say ""
say "MANUAL operator steps remaining (NOT done by this script):"
if [ "$IS_PRIVATE" -eq 1 ]; then
    say "  NOTE: this is a PRIVATE model (cloud TTS positives) — do NOT ship or"
    say "  pin it as the clean-license default. See $OUT_DIR/PROVENANCE.txt."
fi
say "  1. Upload the three .ort files to the fono-voice release mirror under"
say "     the ort-<version> tag (the same ABI-tagged release the voices use)."
say "  2. Compute each file's real SHA-256 and pin it in"
say "     crates/fono-audio/src/wake_registry.rs (replace the UNPINNED zeros)."
say "  3. Record provenance per calibration/wakeword/README.md."
