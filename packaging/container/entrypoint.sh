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
    : "${FONO_ASSISTANT_ENABLED:=false}"

    stt_backend=$(toml_string "$FONO_STT_BACKEND")
    stt_model=$(toml_string "$FONO_STT_MODEL")
    stt_quantization=$(toml_string "$FONO_STT_QUANTIZATION")
    tts_backend=$(toml_string "$FONO_TTS_BACKEND")
    tts_voice=$(toml_string "$FONO_TTS_VOICE")
    tts_local_voice=$(toml_string "$FONO_TTS_LOCAL_VOICE")
    wyoming_bind=$(toml_string "$FONO_WYOMING_BIND")
    mdns_name=$(toml_string "$FONO_MDNS_NAME")

    languages_toml="["
    first_language=true
    old_ifs=$IFS
    IFS=,
    for language in $FONO_LANGUAGES; do
        IFS=$old_ifs
        language=$(printf '%s' "$language" | tr -d '[:space:]')
        if [ -n "$language" ]; then
            escaped_language=$(toml_string "$language")
            if [ "$first_language" = true ]; then
                first_language=false
            else
                languages_toml="$languages_toml, "
            fi
            languages_toml="$languages_toml\"$escaped_language\""
        fi
        IFS=,
    done
    IFS=$old_ifs
    languages_toml="$languages_toml]"

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
backend = "none"

[server.wyoming]
enabled = true
bind = "$wyoming_bind"
port = $FONO_WYOMING_PORT

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
