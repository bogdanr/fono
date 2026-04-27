#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# fono-bench runner — convenient wrapper around `fono-bench equivalence`
# that handles building with the right features, selecting models, and
# saving JSON reports.
#
# Usage:
#   ./bench.sh                          # tiny.en, local, quick
#   ./bench.sh tiny.en                  # tiny.en, local, full
#   ./bench.sh small                    # small, local, full
#   ./bench.sh groq                     # groq cloud (needs GROQ_API_KEY)
#   ./bench.sh openai                   # openai cloud (needs OPENAI_API_KEY)
#   ./bench.sh --all-local              # run every local model found
#   ./bench.sh --quick small            # quick mode (skip >5s fixtures)
#   ./bench.sh --output results/ small  # save JSON report
#
# The script auto-detects which local whisper models are installed under
# ~/.cache/fono/models/whisper/ and which cloud API keys are available.

set -euo pipefail

cd "$(dirname "$0")/.."

# ── Defaults ──────────────────────────────────────────────────────────
FEATURES="equivalence,whisper-local,groq,openai"
QUICK_FLAG=""
OUTPUT_ARG=""
ALL_LOCAL=false

# ── Parse arguments ───────────────────────────────────────────────────
POSITIONAL=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --quick|-q)
            QUICK_FLAG="--quick"
            shift
            ;;
        --output|-o)
            OUTPUT_ARG="--output $2"
            mkdir -p "$(dirname "$2")"
            shift 2
            ;;
        --all-local)
            ALL_LOCAL=true
            shift
            ;;
        --help|-h)
            sed -n '3,14p' "$0" | sed 's/^# //'
            exit 0
            ;;
        *)
            POSITIONAL+=("$1")
            shift
            ;;
    esac
done

MODEL="${POSITIONAL[0]:-tiny.en}"

# ── Helpers ───────────────────────────────────────────────────────────
models_dir="${XDG_CACHE_HOME:-$HOME/.cache}/fono/models/whisper"

bold()  { printf '\033[1m%s\033[0m\n' "$*"; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }
red()   { printf '\033[31m%s\033[0m\n' "$*"; }

run_equiv() {
    local stt="$1" model="$2" label="$3"

    bold "=== $label ==="

    # For cloud providers, check API key
    case "$stt" in
        groq)
            if [[ -z "${GROQ_API_KEY:-}" ]]; then
                red "  SKIP: GROQ_API_KEY not set"
                echo
                return 0
            fi
            ;;
        openai)
            if [[ -z "${OPENAI_API_KEY:-}" ]]; then
                red "  SKIP: OPENAI_API_KEY not set"
                echo
                return 0
            fi
            ;;
        local)
            if [[ ! -f "$models_dir/ggml-${model}.bin" ]]; then
                red "  SKIP: model $model not found ($models_dir/ggml-${model}.bin)"
                echo
                return 0
            fi
            ;;
    esac

    # Build (skip if already built)
    cargo build -p fono-bench --features "$FEATURES" 2>&1 | tail -1

    # Build the command
    local cmd="cargo run -p fono-bench --features $FEATURES -- equivalence --stt $stt --model $model --no-legend"
    [[ -n "$QUICK_FLAG" ]] && cmd+=" $QUICK_FLAG"
    [[ -n "$OUTPUT_ARG" ]] && cmd+=" $OUTPUT_ARG"

    # Run
    eval "$cmd"
    local rc=$?
    echo
    return $rc
}

# ── Print legend once at the end ──────────────────────────────────────
print_legend() {
    echo "Legend:"
    echo "  audio_s    Duration of the audio clip (seconds)"
    echo "  batch_s    Batch transcription total time (seconds)"
    echo "  stream_s   Streaming transcription total time (seconds)"
    echo "  ttff_s     Time to first feedback from streaming (seconds)"
    echo "  ttff_r     Streaming TTFF / batch TTC  (< 1.0 = streaming shows first word sooner)"
    echo "  ttc_r      Streaming TTC / batch TTC   (< 1.0 = streaming completes faster overall)"
    echo "  lev        Stream↔batch Levenshtein (0.0 = streaming and batch agree)"
    echo "  acc        Batch↔reference Levenshtein (0.0 = batch matches the canonical text)"
    echo
    if [[ -t 1 ]]; then
        printf 'Color key:
'
        printf '  \033[32mgreen\033[0m = good        \033[33myellow\033[0m = marginal / caution        \033[31mred\033[0m = bad / over threshold
'
    else
        echo "Color key:"
        echo "  green = good        yellow = marginal / caution        red = bad / over threshold"
    fi
}

# ── --all-local: run every model found ────────────────────────────────
if [[ "$ALL_LOCAL" == true ]]; then
    if [[ ! -d "$models_dir" ]]; then
        red "No local models found in $models_dir"
        exit 1
    fi

    bold "Local models found:"
    for f in "$models_dir"/ggml-*.bin; do
        [[ -f "$f" ]] || continue
        name=$(basename "$f" .bin)
        name=${name#ggml-}
        echo "  $name"
    done
    echo

    for f in "$models_dir"/ggml-*.bin; do
        [[ -f "$f" ]] || continue
        name=$(basename "$f" .bin)
        name=${name#ggml-}
        run_equiv local "$name" "local / $name"
    done

    print_legend
    bold "=== All local models done ==="
    exit 0
fi

# ── Single model run ──────────────────────────────────────────────────
case "$MODEL" in
    groq)
        run_equiv groq whisper-large-v3-turbo "groq / whisper-large-v3-turbo"
        ;;
    openai)
        run_equiv openai whisper-1 "openai / whisper-1"
        ;;
    *)
        # Treat as a local whisper model name
        run_equiv local "$MODEL" "local / $MODEL"
        ;;
esac

print_legend
