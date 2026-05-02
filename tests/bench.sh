#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
#
# fono-bench convenience runner.
#
# Auto-discovers whatever local models you already have on disk and
# runs them through the equivalence harness (and, with --llm, through
# llama.cpp's llama-bench). When the build host has the Vulkan SDK
# installed it also produces a CPU-vs-GPU comparison side-by-side.
#
# Usage:
#   ./tests/bench.sh                    # all local whisper models, CPU + GPU if available
#   ./tests/bench.sh --cpu              # CPU only (skip Vulkan even if available)
#   ./tests/bench.sh --quick            # skip fixtures > 5 s
#   ./tests/bench.sh small              # just one whisper model
#   ./tests/bench.sh groq               # cloud Groq (needs GROQ_API_KEY)
#   ./tests/bench.sh openai             # cloud OpenAI (needs OPENAI_API_KEY)
#   ./tests/bench.sh --llm              # also run llama.cpp llama-bench on cached ggufs
#   ./tests/bench.sh --llm-only         # ONLY the LLM bench, skip whisper
#
# Environment:
#   FONO_BENCH_LLAMA_REF   llama.cpp git ref to build (default: master)
#   FONO_BENCH_LLAMA_DIR   llama.cpp checkout/build cache (default: target/llama.cpp)
#   FONO_BENCH_NO_BUILD    1 = skip fono-bench rebuild if a binary exists
#
# Models are picked up from:
#   ~/.cache/fono/models/whisper/ggml-*.bin   (whisper)
#   ~/.cache/fono/models/llm/*.gguf           (llama / qwen / mistral / …)
#

set -euo pipefail
cd "$(dirname "$0")/.."

# ── colour helpers ────────────────────────────────────────────────────
if [[ -t 1 ]]; then
    bold()  { printf '\033[1m%s\033[0m\n' "$*"; }
    green() { printf '\033[32m%s\033[0m\n' "$*"; }
    yellow(){ printf '\033[33m%s\033[0m\n' "$*"; }
    red()   { printf '\033[31m%s\033[0m\n' "$*"; }
    dim()   { printf '\033[2m%s\033[0m\n' "$*"; }
else
    bold()  { printf '%s\n' "$*"; }
    green() { printf '%s\n' "$*"; }
    yellow(){ printf '%s\n' "$*"; }
    red()   { printf '%s\n' "$*"; }
    dim()   { printf '%s\n' "$*"; }
fi

# ── arg parsing ───────────────────────────────────────────────────────
QUICK_FLAG=""
RUN_LLM=false
LLM_ONLY=false
FORCE_CPU_ONLY=false
EXPLICIT_MODEL=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --quick|-q)   QUICK_FLAG="--quick"; shift ;;
        --cpu)        FORCE_CPU_ONLY=true; shift ;;
        --llm)        RUN_LLM=true; shift ;;
        --llm-only)   RUN_LLM=true; LLM_ONLY=true; shift ;;
        --help|-h)    sed -n '4,28p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        --*)          red "unknown flag: $1"; exit 2 ;;
        *)            EXPLICIT_MODEL="$1"; shift ;;
    esac
done

# ── paths ─────────────────────────────────────────────────────────────
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/fono"
WHISPER_DIR="$CACHE/models/whisper"
LLM_DIR="$CACHE/models/llm"
TARGET_CPU="target/bench-cpu"
TARGET_VULKAN="target/bench-vulkan"
LLAMA_BUILD="${FONO_BENCH_LLAMA_DIR:-target/llama.cpp}"
LLAMA_REF="${FONO_BENCH_LLAMA_REF:-master}"

# ── Vulkan availability ───────────────────────────────────────────────
have_vulkan=false
if [[ "$FORCE_CPU_ONLY" != true ]] \
    && [[ -e /usr/include/vulkan/vulkan.h || -e /usr/local/include/vulkan/vulkan.h ]] \
    && command -v glslc >/dev/null 2>&1; then
    have_vulkan=true
fi

if $have_vulkan; then
    bold "Backends: CPU + Vulkan"
else
    if $FORCE_CPU_ONLY; then
        bold "Backends: CPU (--cpu forced)"
    else
        bold "Backends: CPU (Vulkan SDK not detected: install vulkan headers + glslc)"
    fi
fi

# ── build helper ──────────────────────────────────────────────────────
build_bench() {
    local target_dir="$1"; shift
    local features="$1"; shift
    local label="$1"; shift
    local bin="$target_dir/release/fono-bench"

    if [[ -x "$bin" && "${FONO_BENCH_NO_BUILD:-0}" == "1" ]]; then
        dim "  reuse $bin (FONO_BENCH_NO_BUILD=1)"
        return
    fi

    bold "==> building fono-bench [$label] ($features)"
    CARGO_TARGET_DIR="$target_dir" cargo build \
        -p fono-bench --release --no-default-features \
        --features "$features" --bin fono-bench 2>&1 \
        | grep -E '^(error|warning|   Compiling fono-bench|    Finished|    Building)' \
        | tail -8 || true

    [[ -x "$bin" ]] || { red "build failed: $bin missing"; exit 1; }
}

# ── runner ────────────────────────────────────────────────────────────
run_one() {
    local bin="$1" stt="$2" model="$3" out_json="$4" label="$5"
    bold "── $label ──"
    # `|| true`: equivalence exits non-zero when any fixture fails its
    # accuracy/equivalence threshold. That's a benchmark verdict, not
    # a script error — keep going so we can compare CPU vs Vulkan.
    FONO_BENCH_LOG=warn "$bin" equivalence \
        --stt "$stt" --model "$model" --no-legend $QUICK_FLAG \
        --output "$out_json" || true
    echo
}

# ── extract avg ratios for the comparison summary ────────────────────
# Reads the JSON output of equivalence and prints "<batch_total>\t<stream_total>\t<ttff_avg>"
# in seconds, summed over fixtures that produced timings.
summarise_json() {
    python3 - "$1" <<'PY' 2>/dev/null || true
import json, sys
try:
    with open(sys.argv[1]) as f: r = json.load(f)
except Exception: sys.exit(0)
b = s = ttff = 0.0; n_t = 0
for fx in r.get("results", []):
    modes = fx.get("modes") or {}
    if fx.get("verdict") == "skip": continue
    bt = (modes.get("batch")     or {}).get("elapsed_ms") or 0
    st = (modes.get("streaming") or {}).get("elapsed_ms") or 0
    tf = (modes.get("streaming") or {}).get("ttff_ms")    or 0
    b += bt / 1000.0
    s += st / 1000.0
    if tf:
        ttff += tf / 1000.0
        n_t += 1
print(f"{b:.2f}\t{s:.2f}\t{(ttff/max(n_t,1)):.2f}")
PY
}

# ── whisper bench ─────────────────────────────────────────────────────
whisper_bench() {
    if ! $LLM_ONLY; then : ; else return 0; fi

    # Resolve model list
    local models=()
    if [[ -n "$EXPLICIT_MODEL" ]]; then
        case "$EXPLICIT_MODEL" in
            groq|openai) models=("$EXPLICIT_MODEL") ;;
            *) models=("$EXPLICIT_MODEL") ;;
        esac
    else
        if [[ -d "$WHISPER_DIR" ]]; then
            for f in "$WHISPER_DIR"/ggml-*.bin; do
                [[ -f "$f" ]] || continue
                local n; n=$(basename "$f" .bin); n=${n#ggml-}
                models+=("$n")
            done
        fi
    fi

    if [[ ${#models[@]} -eq 0 ]]; then
        yellow "no local whisper models in $WHISPER_DIR (run \`fono setup\` first)"
        return 0
    fi

    bold "Whisper models discovered: ${models[*]}"
    echo

    # Build CPU + Vulkan binaries
    build_bench "$TARGET_CPU" "whisper-local,equivalence" "CPU"
    if $have_vulkan; then
        build_bench "$TARGET_VULKAN" "whisper-local,equivalence,accel-vulkan" "Vulkan"
    fi
    echo

    # Run + summarise. Collect the rows first; print the comparison
    # table once at the end so it isn't interleaved with per-fixture
    # output from the equivalence runs.
    local report="target/bench-results"; mkdir -p "$report"
    local rows=()

    for m in "${models[@]}"; do
        local cpu_json="$report/whisper-${m}-cpu.json"
        run_one "$TARGET_CPU/release/fono-bench" local "$m" "$cpu_json" "whisper $m / CPU"
        local cpu_t cpu_b cpu_s cpu_f
        cpu_t=$(summarise_json "$cpu_json")
        cpu_b=$(echo "$cpu_t" | cut -f1); cpu_s=$(echo "$cpu_t" | cut -f2); cpu_f=$(echo "$cpu_t" | cut -f3)
        rows+=("$(printf '%-18s | %-7s | %5s / %5s / %5s s | (baseline)' "$m" "CPU" "$cpu_b" "$cpu_s" "$cpu_f")")

        if $have_vulkan; then
            local gpu_json="$report/whisper-${m}-vulkan.json"
            run_one "$TARGET_VULKAN/release/fono-bench" local "$m" "$gpu_json" "whisper $m / Vulkan"
            local gpu_t gpu_b gpu_s gpu_f sb ss sf
            gpu_t=$(summarise_json "$gpu_json")
            gpu_b=$(echo "$gpu_t" | cut -f1); gpu_s=$(echo "$gpu_t" | cut -f2); gpu_f=$(echo "$gpu_t" | cut -f3)
            sb=$(awk -v c="$cpu_b" -v g="$gpu_b" 'BEGIN{ if (g>0) printf "%.2fx", c/g; else print "-" }')
            ss=$(awk -v c="$cpu_s" -v g="$gpu_s" 'BEGIN{ if (g>0) printf "%.2fx", c/g; else print "-" }')
            sf=$(awk -v c="$cpu_f" -v g="$gpu_f" 'BEGIN{ if (g>0) printf "%.2fx", c/g; else print "-" }')
            rows+=("$(printf '%-18s | %-7s | %5s / %5s / %5s s | %s / %s / %s' "$m" "Vulkan" "$gpu_b" "$gpu_s" "$gpu_f" "$sb" "$ss" "$sf")")
        fi
    done

    bold "===== Whisper STT comparison ====="
    printf '%-18s | %-7s | %-23s | %s\n' "model" "backend" "batch / stream / ttff" "speedup vs CPU"
    printf '%s\n' "----------------------------------------------------------------------------------------"
    for r in "${rows[@]}"; do echo "$r"; done
    echo
    dim "JSON reports under: $report/"
}

# ── llama.cpp llama-bench (CPU vs Vulkan) ─────────────────────────────
build_llama_bench() {
    local kind="$1"   # cpu | vulkan
    local build_dir="$LLAMA_BUILD/build-$kind"
    local bin="$build_dir/bin/llama-bench"

    if [[ -x "$bin" ]]; then
        dim "  reuse $bin"
        return 0
    fi

    if [[ ! -d "$LLAMA_BUILD/.git" ]]; then
        bold "==> cloning llama.cpp into $LLAMA_BUILD"
        git clone --depth 1 https://github.com/ggml-org/llama.cpp.git "$LLAMA_BUILD"
    fi
    if [[ "$LLAMA_REF" != "master" ]]; then
        ( cd "$LLAMA_BUILD" && git fetch --depth 1 origin "$LLAMA_REF" && git checkout FETCH_HEAD )
    fi

    bold "==> building llama-bench [$kind]"
    mkdir -p "$build_dir"
    local cmake_args=(-S "$LLAMA_BUILD" -B "$build_dir"
        -DCMAKE_BUILD_TYPE=Release
        -DLLAMA_BUILD_TESTS=OFF
        -DLLAMA_BUILD_EXAMPLES=OFF
        -DLLAMA_BUILD_SERVER=OFF
        -DLLAMA_BUILD_TOOLS=ON)
    if [[ "$kind" == vulkan ]]; then
        cmake_args+=(-DGGML_VULKAN=ON)
    fi
    cmake "${cmake_args[@]}" >/dev/null
    cmake --build "$build_dir" --target llama-bench -j"$(nproc)" 2>&1 | tail -3

    [[ -x "$bin" ]] || { red "llama-bench build failed: $bin missing"; exit 1; }
}

llm_bench() {
    $RUN_LLM || return 0

    local ggufs=()
    if [[ -d "$LLM_DIR" ]]; then
        for f in "$LLM_DIR"/*.gguf; do
            [[ -f "$f" ]] && ggufs+=("$f")
        done
    fi

    if [[ ${#ggufs[@]} -eq 0 ]]; then
        yellow "no gguf models in $LLM_DIR"
        echo
        echo "  Drop a gguf in $LLM_DIR/ and rerun. Suggested small models:"
        echo "    qwen2.5-3b-instruct-q4_k_m.gguf  (~2.0 GB)"
        echo "    llama-3.2-3b-instruct-q4_k_m.gguf (~2.0 GB)"
        echo "  e.g.:"
        echo "    curl -fL --output-dir $LLM_DIR -O \\"
        echo "      https://huggingface.co/Qwen/Qwen2.5-3B-Instruct-GGUF/resolve/main/qwen2.5-3b-instruct-q4_k_m.gguf"
        return 0
    fi

    bold "LLM models discovered: ${#ggufs[@]} gguf$([ ${#ggufs[@]} -ne 1 ] && echo s)"
    for g in "${ggufs[@]}"; do echo "  $(basename "$g") ($(du -h "$g" | cut -f1))"; done
    echo

    build_llama_bench cpu
    $have_vulkan && build_llama_bench vulkan
    echo

    for g in "${ggufs[@]}"; do
        bold "── $(basename "$g") / CPU ──"
        "$LLAMA_BUILD/build-cpu/bin/llama-bench" -m "$g" -p 128 -n 64 -t "$(nproc)" -ngl 0 || true
        echo
        if $have_vulkan; then
            bold "── $(basename "$g") / Vulkan (full GPU offload) ──"
            "$LLAMA_BUILD/build-vulkan/bin/llama-bench" -m "$g" -p 128 -n 64 -ngl 99 || true
            echo
        fi
    done
}

# ── main ──────────────────────────────────────────────────────────────
whisper_bench
llm_bench

bold "Done."
dim "Tip: FONO_BENCH_NO_BUILD=1 ./tests/bench.sh re-runs without rebuilding."
