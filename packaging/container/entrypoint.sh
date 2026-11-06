#!/bin/sh
set -eu

: "${HOME:=/data}"
export HOME

: "${FONO_CONFIG_DIR:=$HOME/.config/fono}"
: "${FONO_DATA_DIR:=$HOME/.local/share/fono}"
: "${FONO_CACHE_DIR:=$HOME/.cache/fono}"
: "${FONO_STATE_DIR:=$HOME/.local/state/fono}"
: "${FONO_CONFIG:=$FONO_CONFIG_DIR/config.toml}"

mkdir -p "$FONO_CONFIG_DIR" "$FONO_DATA_DIR" "$FONO_CACHE_DIR" "$FONO_STATE_DIR"

toml_string() {
    printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

# Render a comma-separated list as a TOML string array: "a, b" -> ["a", "b"].
toml_list() {
    out="["
    first=true
    old_ifs=$IFS
    IFS=,
    for item in $1; do
        item=$(printf '%s' "$item" | tr -d '[:space:]')
        if [ -n "$item" ]; then
            if [ "$first" = true ]; then first=false; else out="$out, "; fi
            out="$out\"$(toml_string "$item")\""
        fi
    done
    IFS=$old_ifs
    printf '%s]' "$out"
}

if [ "${FONO_CONTAINER_WRITE_CONFIG:-missing}" = "always" ] || [ ! -f "$FONO_CONFIG" ]; then
    : "${FONO_LANGUAGES:=en}"
    : "${FONO_STT_BACKEND:=local}"
    : "${FONO_STT_MODEL:=small}"
    : "${FONO_STT_QUANTIZATION:=auto}"
    : "${FONO_STT_THREADS:=0}"
    : "${FONO_TTS_BACKEND:=local}"
    : "${FONO_TTS_VOICE:=}"
    : "${FONO_TTS_LOCAL_VOICE:=}"
    : "${FONO_WYOMING_BIND:=0.0.0.0}"
    : "${FONO_WYOMING_PORT:=10300}"
    : "${FONO_MDNS_NAME:=Fono}"
    : "${FONO_UPDATE_AUTO_CHECK:=false}"
    : "${FONO_POLISH_ENABLED:=false}"
    # Assistant backend: none, or a cloud provider (groq, openai, anthropic,
    # gemini, openrouter, ...) that reads its key from the environment.
    : "${FONO_ASSISTANT_BACKEND:=none}"
    if [ "$FONO_ASSISTANT_BACKEND" = "none" ]; then
        : "${FONO_ASSISTANT_ENABLED:=false}"
    else
        : "${FONO_ASSISTANT_ENABLED:=true}"
    fi
    # Wyoming services to offer: any of asr, tts, wake, handle. `handle`
    # (conversation agent) is only offered when the assistant is enabled.
    : "${FONO_SERVICES:=asr,tts,wake,handle}"
    # Ollama/OpenAI-compatible LLM server on 11434 and the web settings UI
    # on 10808. Both require an API key from remote callers.
    : "${FONO_LLM_ENABLED:=false}"
    : "${FONO_WEB_ENABLED:=false}"
    # A reverse proxy whose users have already signed in, such as Home
    # Assistant's ingress proxy. It reaches the web UI without an API key.
    : "${FONO_WEB_TRUSTED_PROXY:=}"

    stt_backend=$(toml_string "$FONO_STT_BACKEND")
    stt_model=$(toml_string "$FONO_STT_MODEL")
    stt_quantization=$(toml_string "$FONO_STT_QUANTIZATION")
    tts_backend=$(toml_string "$FONO_TTS_BACKEND")
    tts_voice=$(toml_string "$FONO_TTS_VOICE")
    tts_local_voice=$(toml_string "$FONO_TTS_LOCAL_VOICE")
    wyoming_bind=$(toml_string "$FONO_WYOMING_BIND")
    mdns_name=$(toml_string "$FONO_MDNS_NAME")
    assistant_backend=$(toml_string "$FONO_ASSISTANT_BACKEND")
    web_trusted_proxy=$(toml_string "$FONO_WEB_TRUSTED_PROXY")

    languages_toml=$(toml_list "$FONO_LANGUAGES")
    services_toml=$(toml_list "$FONO_SERVICES")

    tmp_config="$FONO_CONFIG.tmp.$$"
    cat > "$tmp_config" <<EOF_CONFIG
version = 1

[general]
languages = $languages_toml
startup_autostart = false
auto_mute_system = false
also_copy_to_clipboard = false
cloud_rerun_on_language_mismatch = true

[stt]
# local, or a cloud provider (groq, openai, deepgram, gemini, elevenlabs,
# cartesia, speechmatics, openrouter). Cloud backends read their API key
# from the matching environment variable (e.g. GROQ_API_KEY).
backend = "$stt_backend"

[stt.local]
model = "$stt_model"
quantization = "$stt_quantization"
threads = $FONO_STT_THREADS

[tts]
backend = "$tts_backend"
voice = "$tts_voice"

[tts.local]
voice = "$tts_local_voice"

[polish]
enabled = $FONO_POLISH_ENABLED
backend = "none"

[assistant]
enabled = $FONO_ASSISTANT_ENABLED
backend = "$assistant_backend"

[server.wyoming]
enabled = true
bind = "$wyoming_bind"
port = $FONO_WYOMING_PORT
services = $services_toml

[server.llm]
enabled = $FONO_LLM_ENABLED
bind = "0.0.0.0"

[server.web]
enabled = $FONO_WEB_ENABLED
bind = "0.0.0.0"
trusted_proxy = "$web_trusted_proxy"

[network]
instance_name = "$mdns_name"

[update]
auto_check = $FONO_UPDATE_AUTO_CHECK
channel = "stable"
EOF_CONFIG
    mv "$tmp_config" "$FONO_CONFIG"
fi

if [ "$#" -eq 0 ]; then
    set -- fono
elif [ "$1" = "fono" ]; then
    set -- "$@"
elif command -v "$1" >/dev/null 2>&1; then
    set -- "$@"
else
    set -- fono "$@"
fi

exec "$@"
