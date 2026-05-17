#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
#
# capture-overlay.sh — record the Fono overlay for README screencasts.
#
# Produces tight, GitHub-embed-friendly captures of the overlay window
# in three modes:
#
#   overlay   Tight crop of just the overlay (640 × ≤240 logical px).
#   paste     Overlay + a target app window below, to demonstrate
#             text injection into a real surface (terminal / editor).
#   gallery   Records each `--styles` value (default
#             bars,oscilloscope,fft,heatmap — see
#             crates/fono-tray/src/lib.rs:193-196) in turn, labels
#             them, then stitches the clips together.
#
# Outputs land under target/screencasts/<mode>/ as a triplet:
#   <name>.mp4   H.264 yuv420p, crf 23, faststart (PRs / release notes).
#   <name>.gif   palette-optimised, ≤ 5 MB target for inline README.
#   <name>.webp  animated, q 70 — fallback when GIF blows the budget.
#
# See `docs/troubleshooting.md` → "Capturing screencasts" for end-to-end
# recipes and dependency packages per distro.

set -euo pipefail

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------

MODE="overlay"
DURATION=""                 # auto-default per mode below
HEIGHT=240                  # overlay logical height (clamped 80..240 in fono)
MONITOR=""                  # WxH+X+Y override
SCALE="1"                   # HiDPI multiplier
FPS=30                      # capture fps (raw master)
START_FONO=0
KEEP_RAW=0
OUTPUT_DIR=""               # defaults to target/screencasts
FORMAT="all"                # gif | mp4 | webp | all
TARGET_APP=""               # WM class for paste mode
BELOW=""                    # px below overlay to include in paste crop
REGION=""                   # full manual override WxH+X+Y for paste
STYLES="bars,oscilloscope,fft,heatmap"
STYLE_SETTLE="1.5"
LABEL=""                    # tri-state: empty → mode default
LAYOUT="concat"             # concat | grid (gallery only)
BACKGROUND=""               # optional #rrggbb wallpaper override
DETECT_WINDOW=1             # auto-locate the Fono window at capture time
DETECT_PAD=4                # px of breathing room around the detected box
CONVERT=""                  # if set, skip capture and convert this MP4 to GIF+WebP

SCRIPT_NAME="$(basename "$0")"

# ---------------------------------------------------------------------------
# Help text
# ---------------------------------------------------------------------------

usage() {
    cat <<'EOF'
Usage: capture-overlay.sh [OPTIONS]

Record the Fono overlay for README screencasts. Emits MP4 + GIF + WebP.

Modes (--mode):
  overlay   Tight crop of just the overlay window. (default)
  paste     Overlay plus a target app window below to demonstrate
            text injection. Requires one of --target-app, --below,
            or --region.
  gallery   Record each waveform style listed in --styles, label
            them, and stitch the clips into one montage.

Shared options:
  --mode {overlay|paste|gallery}   Selects capture pipeline.
  --duration <s>                   Seconds per take. Defaults: 8
                                   (overlay/paste), 5 (per gallery style).
  --height <px>                    Overlay logical height (default 240,
                                   clamped 80..240 by fono itself).
  --monitor WxH+X+Y                Override monitor geometry when
                                   xrandr/wlr-randr cannot resolve it.
  --scale <factor>                 HiDPI scale multiplier (default 1).
                                   Matches GDK_SCALE / Xft.dpi/96.
  --fps <n>                        Raw recorder fps (default 30).
  --start-fono                     Spawn `fono` for the duration of
                                   the capture and stop it after.
  --keep-raw                       Keep the lossless raw .mkv master.
  --output-dir <path>              Override target/screencasts/.
  --format {gif|mp4|webp|all}      Which artefacts to emit (default all).
  --background <#rrggbb>           Pre-set a solid wallpaper for the
                                   take (feh on X11, swaybg on Wayland)
                                   to avoid desktop bleed-through.
  --no-detect-window               Skip live X11/Wayland lookup of the
                                   Fono window. Use the hard-coded
                                   logical geometry from --height etc.
                                   instead. Default: detection on.
  --detect-pad <px>                Padding added around the detected
                                   window on each side (default 4).
                                   Useful for catching shadow / AA
                                   pixels that fall outside the inner
                                   frame.
  --convert <input.mp4>            Skip capture entirely; convert an
                                   existing MP4 to GIF + WebP (and
                                   re-encode MP4 if --format includes
                                   it) using the same encode pipeline
                                   as a normal capture. No session,
                                   monitor, or fono dependencies are
                                   required. Outputs land next to the
                                   input file (or in --output-dir).
  -h, --help                       Print this message.

paste-mode options:
  --target-app <wm_class>          Resolve target window geometry via
                                   xdotool (X11) or swaymsg (sway).
  --below <px>                     Fallback: extend crop N px below
                                   the overlay regardless of contents.
  --region WxH+X+Y                 Full manual crop override.

gallery-mode options:
  --styles a,b,c                   Comma-separated style list (default
                                   bars,oscilloscope,fft,heatmap).
  --style-settle <s>               Sleep after switching style before
                                   recording (default 1.5).
  --label / --no-label             Toggle drawtext corner caption.
                                   Default: on for gallery, off elsewhere.
  --layout {concat|grid}           concat = sequential clip, grid = 2x2
                                   xstack mosaic (default concat).

Dependencies (probed at startup):
  Required:   ffmpeg
  X11 path:   xrandr
  Wayland:    wlr-randr (or swaymsg), grim, wf-recorder
  Optional:   gifsicle (final GIF optimisation pass)
              xdotool + wmctrl (only for `--mode paste --target-app`
                                under X11 — probed lazily)

  Slackware / NimbleX:  sbopkg / slackbuild for each above.
  Arch:                 pacman -S ffmpeg xorg-xrandr \
                                  wlr-randr grim wf-recorder gifsicle
  Debian / Ubuntu:      apt install ffmpeg x11-xserver-utils \
                                    wlr-randr grim wf-recorder \
                                    gifsicle

Examples:
  capture-overlay.sh --mode overlay --duration 6 --start-fono
  capture-overlay.sh --mode paste --target-app Alacritty --duration 8
  capture-overlay.sh --mode gallery --duration 5 --layout concat
EOF
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

warn() {
    printf 'warn: %s\n' "$*" >&2
}

info() {
    printf '%s\n' "$*" >&2
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

while [ $# -gt 0 ]; do
    case "$1" in
        --mode)           MODE="${2:-}"; shift 2 ;;
        --duration)       DURATION="${2:-}"; shift 2 ;;
        --height)         HEIGHT="${2:-}"; shift 2 ;;
        --monitor)        MONITOR="${2:-}"; shift 2 ;;
        --scale)          SCALE="${2:-}"; shift 2 ;;
        --fps)            FPS="${2:-}"; shift 2 ;;
        --start-fono)     START_FONO=1; shift ;;
        --keep-raw)       KEEP_RAW=1; shift ;;
        --output-dir)     OUTPUT_DIR="${2:-}"; shift 2 ;;
        --format)         FORMAT="${2:-}"; shift 2 ;;
        --target-app)     TARGET_APP="${2:-}"; shift 2 ;;
        --below)          BELOW="${2:-}"; shift 2 ;;
        --region)         REGION="${2:-}"; shift 2 ;;
        --styles)         STYLES="${2:-}"; shift 2 ;;
        --style-settle)   STYLE_SETTLE="${2:-}"; shift 2 ;;
        --label)          LABEL=1; shift ;;
        --no-label)       LABEL=0; shift ;;
        --layout)         LAYOUT="${2:-}"; shift 2 ;;
        --background)     BACKGROUND="${2:-}"; shift 2 ;;
        --no-detect-window) DETECT_WINDOW=0; shift ;;
        --detect-window)  DETECT_WINDOW=1; shift ;;
        --detect-pad)     DETECT_PAD="${2:-}"; shift 2 ;;
        --convert)        CONVERT="${2:-}"; shift 2 ;;
        -h|--help)        usage; exit 0 ;;
        --) shift; break ;;
        *) die "unknown argument: $1 (try --help)" ;;
    esac
done

case "$MODE" in
    overlay|paste|gallery) ;;
    *) die "--mode must be overlay|paste|gallery (got '$MODE')" ;;
esac

case "$FORMAT" in
    gif|mp4|webp|all) ;;
    *) die "--format must be gif|mp4|webp|all (got '$FORMAT')" ;;
esac

case "$LAYOUT" in
    concat|grid) ;;
    *) die "--layout must be concat|grid (got '$LAYOUT')" ;;
esac

# Mode-specific label default.
if [ -z "$LABEL" ]; then
    if [ "$MODE" = "gallery" ]; then
        LABEL=1
    else
        LABEL=0
    fi
fi

# Mode-specific duration default.
if [ -z "$DURATION" ]; then
    if [ "$MODE" = "gallery" ]; then
        DURATION=5
    else
        DURATION=8
    fi
fi

# ---------------------------------------------------------------------------
# Repo root + output layout
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

if [ -z "$OUTPUT_DIR" ]; then
    OUTPUT_DIR="$REPO_ROOT/target/screencasts"
fi
if [ -z "$CONVERT" ]; then
    mkdir -p "$OUTPUT_DIR/$MODE"
    RAW_DIR="$OUTPUT_DIR/raw"
    mkdir -p "$RAW_DIR"
fi

TIMESTAMP="$(date +%Y%m%d-%H%M%S)"

# ---------------------------------------------------------------------------
# Session detection
# ---------------------------------------------------------------------------

SESSION_TYPE="${XDG_SESSION_TYPE:-}"
if [ -n "$CONVERT" ]; then
    # Convert mode: no graphical session needed; only ffmpeg is required.
    SESSION_TYPE="convert"
fi
# XDG_SESSION_TYPE can be 'tty', 'unspecified', or simply wrong when the
# script is launched from a TTY-attached shell or over SSH while a
# graphical session is in fact running. Trust WAYLAND_DISPLAY / DISPLAY
# whenever the env var isn't already x11/wayland.
case "$SESSION_TYPE" in
    x11|wayland|convert) ;;
    *)
        if [ -n "${WAYLAND_DISPLAY:-}" ]; then
            SESSION_TYPE="wayland"
        elif [ -n "${DISPLAY:-}" ]; then
            SESSION_TYPE="x11"
        fi
        ;;
esac
case "$SESSION_TYPE" in
    x11|wayland|convert) ;;
    *) die "could not detect session type (XDG_SESSION_TYPE='${XDG_SESSION_TYPE:-}', WAYLAND_DISPLAY='${WAYLAND_DISPLAY:-}', DISPLAY='${DISPLAY:-}'); set XDG_SESSION_TYPE=x11 or =wayland, or run under a graphical session" ;;
esac

# ---------------------------------------------------------------------------
# Dependency probe
# ---------------------------------------------------------------------------

have() { command -v "$1" >/dev/null 2>&1; }

WAYLAND_RECORDER=""
probe_deps() {
    local missing=""
    have ffmpeg || missing="$missing ffmpeg"

    if [ "$SESSION_TYPE" = "convert" ]; then
        have ffprobe || missing="$missing ffprobe"
        if [ -n "$missing" ]; then
            die "missing dependencies for --convert:$missing (install ffmpeg)"
        fi
        have gifsicle || warn "gifsicle not found - skipping final GIF optimisation pass"
        return 0
    fi

    if [ "$SESSION_TYPE" = "x11" ]; then
        have xrandr  || missing="$missing xrandr"
    else
        # Wayland: pick a recorder.
        if have wf-recorder; then
            WAYLAND_RECORDER="wf-recorder"
        elif have gpu-screen-recorder; then
            WAYLAND_RECORDER="gpu-screen-recorder"
        else
            missing="$missing wf-recorder(-or-gpu-screen-recorder)"
        fi
        if ! have wlr-randr && ! have swaymsg; then
            missing="$missing wlr-randr(-or-swaymsg)"
        fi
        have grim || missing="$missing grim"
    fi

    if [ -n "$missing" ]; then
        cat >&2 <<EOF
error: missing dependencies:$missing

Install via your package manager:
  Slackware / NimbleX : sbopkg / slackbuild each name above.
  Arch                : pacman -S ffmpeg xorg-xrandr \\
                                  wlr-randr grim wf-recorder gifsicle
  Debian / Ubuntu     : apt install ffmpeg x11-xserver-utils \\
                                    wlr-randr grim wf-recorder \\
                                    gifsicle
EOF
        exit 1
    fi

    have gifsicle || warn "gifsicle not found — skipping final GIF optimisation pass"
}

probe_deps

# ---------------------------------------------------------------------------
# H.264 encoder selection
# ---------------------------------------------------------------------------
#
# Distro ffmpeg builds vary in which H.264 encoder they ship: libx264 is
# the preferred one but is patent-encumbered, so trimmed builds (e.g.
# NimbleX) drop it. Pick the best software encoder available and fall
# back to mpeg4-in-mp4 as a last resort. Hardware encoders (h264_vaapi,
# h264_nvenc, h264_vulkan) are skipped intentionally because they need
# extra hwupload filter inserts that would conflict with our crop /
# drawtext chains.
H264_CODEC_ARGS=""
H264_ENCODER=""
detect_h264_encoder() {
    local encs
    encs="$(ffmpeg -hide_banner -encoders 2>/dev/null)"
    if printf '%s\n' "$encs" | grep -qE '^[[:space:]]*V[^ ]*[[:space:]]+libx264[[:space:]]'; then
        H264_ENCODER="libx264"
        H264_CODEC_ARGS="-c:v libx264 -preset slow -crf 23"
    elif printf '%s\n' "$encs" | grep -qE '^[[:space:]]*V[^ ]*[[:space:]]+libopenh264[[:space:]]'; then
        H264_ENCODER="libopenh264"
        H264_CODEC_ARGS="-c:v libopenh264 -b:v 2M"
        warn "ffmpeg lacks libx264; using libopenh264 (acceptable quality)"
    elif printf '%s\n' "$encs" | grep -qE '^[[:space:]]*V[^ ]*[[:space:]]+mpeg4[[:space:]]'; then
        H264_ENCODER="mpeg4"
        H264_CODEC_ARGS="-c:v mpeg4 -qscale:v 3"
        warn "ffmpeg has no libx264 or libopenh264; falling back to mpeg4 (lower quality, less GitHub-friendly). Consider building an ffmpeg with x264 support."
    else
        die "ffmpeg has no usable video encoder (libx264, libopenh264, mpeg4 all missing). Rebuild ffmpeg with at least one of these."
    fi
    info "h.264 encoder: $H264_ENCODER"
}

if [ "$FORMAT" = "mp4" ] || { [ "$FORMAT" = "all" ] && [ "$SESSION_TYPE" != "convert" ]; }; then
    detect_h264_encoder
fi

# ---------------------------------------------------------------------------
# Monitor geometry resolution
# ---------------------------------------------------------------------------

# Sets MON_W MON_H MON_X MON_Y.
resolve_monitor() {
    if [ -n "$MONITOR" ]; then
        # Parse WxH+X+Y.
        if ! printf '%s' "$MONITOR" | grep -Eq '^[0-9]+x[0-9]+\+[0-9]+\+[0-9]+$'; then
            die "--monitor must be WxH+X+Y, got '$MONITOR'"
        fi
        MON_W="${MONITOR%%x*}"
        local rest="${MONITOR#*x}"
        MON_H="${rest%%+*}"
        rest="${rest#*+}"
        MON_X="${rest%%+*}"
        MON_Y="${rest#*+}"
        return
    fi

    if [ "$SESSION_TYPE" = "x11" ]; then
        # `xrandr --query` line for primary: "DP-1 connected primary 2560x1440+0+0 ..."
        local line
        line="$(xrandr --query 2>/dev/null | awk '/ connected primary /{print; exit}')"
        if [ -z "$line" ]; then
            line="$(xrandr --query 2>/dev/null | awk '/ connected /{print; exit}')"
        fi
        [ -n "$line" ] || die "xrandr returned no connected outputs; pass --monitor"
        local geom
        geom="$(printf '%s\n' "$line" | grep -oE '[0-9]+x[0-9]+\+[0-9]+\+[0-9]+' | head -n1)"
        [ -n "$geom" ] || die "could not parse xrandr geometry; pass --monitor"
        MON_W="${geom%%x*}"
        local rest="${geom#*x}"
        MON_H="${rest%%+*}"
        rest="${rest#*+}"
        MON_X="${rest%%+*}"
        MON_Y="${rest#*+}"
    else
        # Wayland: prefer wlr-randr, fall back to swaymsg.
        if have wlr-randr; then
            # wlr-randr: "HDMI-A-1 \"...\"\n  2560x1440 px ..." plus
            # "  Position: 0,0". Parse the first connected output's
            # current mode (line ending in " (current)" or "*") plus
            # its position.
            local out
            out="$(wlr-randr 2>/dev/null || true)"
            local mode pos
            mode="$(printf '%s\n' "$out" | awk '/current/ {print $1; exit}')"
            pos="$(printf '%s\n' "$out" | awk '/Position:/ {print $2; exit}')"
            if [ -z "$mode" ] || [ -z "$pos" ]; then
                die "could not parse wlr-randr output; pass --monitor WxH+X+Y"
            fi
            MON_W="${mode%%x*}"
            MON_H="${mode#*x}"
            MON_X="${pos%%,*}"
            MON_Y="${pos#*,}"
        elif have swaymsg; then
            # swaymsg -t get_outputs (JSON). Avoid jq dep: grep first
            # focused output's rect.
            local json
            json="$(swaymsg -t get_outputs 2>/dev/null || true)"
            [ -n "$json" ] || die "swaymsg returned no outputs; pass --monitor"
            # Extract first rect: "rect":{"x":0,"y":0,"width":2560,"height":1440}
            local rect
            rect="$(printf '%s' "$json" | grep -oE '"rect":\{[^}]+\}' | head -n1)"
            [ -n "$rect" ] || die "could not parse swaymsg rect; pass --monitor"
            MON_X="$(printf '%s' "$rect" | grep -oE '"x":[0-9]+' | head -n1 | cut -d: -f2)"
            MON_Y="$(printf '%s' "$rect" | grep -oE '"y":[0-9]+' | head -n1 | cut -d: -f2)"
            MON_W="$(printf '%s' "$rect" | grep -oE '"width":[0-9]+' | head -n1 | cut -d: -f2)"
            MON_H="$(printf '%s' "$rect" | grep -oE '"height":[0-9]+' | head -n1 | cut -d: -f2)"
        else
            die "no wlr-randr/swaymsg available; pass --monitor WxH+X+Y"
        fi
    fi
}

if [ -z "$CONVERT" ]; then
resolve_monitor
info "monitor: ${MON_W}x${MON_H}+${MON_X}+${MON_Y}  session=$SESSION_TYPE"

# ---------------------------------------------------------------------------
# Overlay crop geometry
# ---------------------------------------------------------------------------

# fono draws the overlay 640 logical px wide, height clamped 80..240
# (crates/fono-overlay/src/real.rs:248-264), bottom-centered with a 48 px
# inset from the bottom edge (real.rs:259, :16).
OV_LOGICAL_W=640
OV_LOGICAL_H="$HEIGHT"
OV_LOGICAL_BOTTOM_INSET=48

# Apply HiDPI scale. SCALE may be fractional ("1.25"); use awk for math.
scale_int() {
    awk -v v="$1" -v s="$SCALE" 'BEGIN { printf "%d", (v*s)+0.5 }'
}

OV_W="$(scale_int "$OV_LOGICAL_W")"
OV_H="$(scale_int "$OV_LOGICAL_H")"
OV_BOTTOM="$(scale_int "$OV_LOGICAL_BOTTOM_INSET")"

# x = monitor.x + (monitor.w - overlay.w) / 2
OV_X=$(( MON_X + (MON_W - OV_W) / 2 ))
# y = monitor.y + monitor.h - bottom_inset - overlay.h
OV_Y=$(( MON_Y + MON_H - OV_BOTTOM - OV_H ))

# ---------------------------------------------------------------------------
# Live overlay window detection
# ---------------------------------------------------------------------------
#
# The hard-coded math above derives the overlay box from logical
# constants in `crates/fono-overlay/src/real.rs` (WIN_WIDTH, the
# WIN_*_HEIGHT family, BOTTOM_OFFSET). Those constants drift
# whenever the overlay code changes, and they cannot capture
# runtime sizing (e.g. waveform vs. text mode picks different
# heights, user config may override width). So if the Fono window
# is actually mapped right now, query the compositor for its true
# geometry and use that instead.
detect_overlay_window() {
    [ "$DETECT_WINDOW" -eq 1 ] || return 1

    local dx="" dy="" dw="" dh=""

    if [ "$SESSION_TYPE" = "x11" ]; then
        if ! have xwininfo; then
            return 1
        fi
        local out
        out="$(xwininfo -name Fono 2>/dev/null || true)"
        if [ -z "$out" ]; then
            return 1
        fi
        dx="$(printf '%s\n' "$out" | awk '/Absolute upper-left X/ {print $NF; exit}')"
        dy="$(printf '%s\n' "$out" | awk '/Absolute upper-left Y/ {print $NF; exit}')"
        dw="$(printf '%s\n' "$out" | awk '/^[[:space:]]+Width:/ {print $NF; exit}')"
        dh="$(printf '%s\n' "$out" | awk '/^[[:space:]]+Height:/ {print $NF; exit}')"
    else
        # Wayland (sway only — wlr-randr doesn't enumerate windows).
        if ! have swaymsg; then
            return 1
        fi
        local json
        json="$(swaymsg -t get_tree 2>/dev/null || true)"
        [ -n "$json" ] || return 1
        # Find first node whose app_id or window class is "fono" and
        # whose name contains "Fono". Avoid jq dep.
        local node
        node="$(printf '%s' "$json" \
            | tr -d '\n' \
            | grep -oE '\{[^{}]*"(app_id|class)":"fono"[^{}]*\}' \
            | head -n1 || true)"
        if [ -z "$node" ]; then
            return 1
        fi
        local rect
        rect="$(printf '%s' "$node" | grep -oE '"rect":\{[^}]+\}' | head -n1)"
        [ -n "$rect" ] || return 1
        dx="$(printf '%s' "$rect" | grep -oE '"x":[0-9]+' | head -n1 | cut -d: -f2)"
        dy="$(printf '%s' "$rect" | grep -oE '"y":[0-9]+' | head -n1 | cut -d: -f2)"
        dw="$(printf '%s' "$rect" | grep -oE '"width":[0-9]+' | head -n1 | cut -d: -f2)"
        dh="$(printf '%s' "$rect" | grep -oE '"height":[0-9]+' | head -n1 | cut -d: -f2)"
    fi

    if [ -z "$dx" ] || [ -z "$dy" ] || [ -z "$dw" ] || [ -z "$dh" ]; then
        return 1
    fi

    # Apply --detect-pad on each side, clamped to the monitor box so
    # ffmpeg's crop filter doesn't sample off-screen pixels.
    local pad="$DETECT_PAD"
    local nx=$(( dx - pad ))
    local ny=$(( dy - pad ))
    local nw=$(( dw + 2 * pad ))
    local nh=$(( dh + 2 * pad ))
    if [ "$nx" -lt "$MON_X" ]; then nw=$(( nw - (MON_X - nx) )); nx="$MON_X"; fi
    if [ "$ny" -lt "$MON_Y" ]; then nh=$(( nh - (MON_Y - ny) )); ny="$MON_Y"; fi
    local right_max=$(( MON_X + MON_W ))
    local bot_max=$(( MON_Y + MON_H ))
    if [ $(( nx + nw )) -gt "$right_max" ]; then nw=$(( right_max - nx )); fi
    if [ $(( ny + nh )) -gt "$bot_max" ]; then nh=$(( bot_max - ny )); fi

    OV_X="$nx"; OV_Y="$ny"; OV_W="$nw"; OV_H="$nh"
    info "detected Fono window: ${dw}x${dh}+${dx}+${dy} (+${pad}px pad -> ${OV_W}x${OV_H}+${OV_X}+${OV_Y})"
    return 0
}

if ! detect_overlay_window; then
    if [ "$DETECT_WINDOW" -eq 1 ]; then
        warn "could not locate Fono window (is fono running? on Wayland, only sway is supported) — falling back to logical geometry"
    fi
fi

# Ensure even dimensions (yuv420p needs even W/H).
even() { local v="$1"; if [ $(( v % 2 )) -ne 0 ]; then echo $(( v - 1 )); else echo "$v"; fi; }

# ---------------------------------------------------------------------------
# paste-mode crop union
# ---------------------------------------------------------------------------

# Inputs: CROP_W CROP_H CROP_X CROP_Y. Default = overlay box.
CROP_W="$OV_W"
CROP_H="$OV_H"
CROP_X="$OV_X"
CROP_Y="$OV_Y"

resolve_paste_crop() {
    if [ -n "$REGION" ]; then
        if ! printf '%s' "$REGION" | grep -Eq '^[0-9]+x[0-9]+\+[0-9]+\+[0-9]+$'; then
            die "--region must be WxH+X+Y, got '$REGION'"
        fi
        CROP_W="${REGION%%x*}"
        local rest="${REGION#*x}"
        CROP_H="${rest%%+*}"
        rest="${rest#*+}"
        CROP_X="${rest%%+*}"
        CROP_Y="${rest#*+}"
        return
    fi

    if [ -n "$TARGET_APP" ]; then
        local tx="" ty="" tw="" th=""
        if [ "$SESSION_TYPE" = "x11" ]; then
            if ! have xdotool || ! have wmctrl; then
                die "--mode paste --target-app on X11 needs xdotool + wmctrl (install via your package manager), or pass --region / --below instead"
            fi
            local wid
            wid="$(xdotool search --limit 1 --class "$TARGET_APP" 2>/dev/null || true)"
            if [ -n "$wid" ]; then
                # Pin position so the crop box stays valid.
                wmctrl -ir "$wid" -e "0,$OV_X,$(( OV_Y + OV_H + 8 )),-1,-1" >/dev/null 2>&1 || true
                # Re-query geometry after the move.
                local geom
                geom="$(xdotool getwindowgeometry --shell "$wid" 2>/dev/null || true)"
                tx="$(printf '%s\n' "$geom" | awk -F= '/^X=/{print $2}')"
                ty="$(printf '%s\n' "$geom" | awk -F= '/^Y=/{print $2}')"
                tw="$(printf '%s\n' "$geom" | awk -F= '/^WIDTH=/{print $2}')"
                th="$(printf '%s\n' "$geom" | awk -F= '/^HEIGHT=/{print $2}')"
            fi
        elif have swaymsg; then
            # Look up first node whose app_id or window_properties.class matches.
            local node
            node="$(swaymsg -t get_tree 2>/dev/null \
                | tr -d '\n' \
                | grep -oE "\\{[^{}]*\"(app_id|class)\":\"$TARGET_APP\"[^{}]*\\}" \
                | head -n1 || true)"
            if [ -n "$node" ]; then
                local rect
                rect="$(printf '%s' "$node" | grep -oE '"rect":\{[^}]+\}' | head -n1)"
                if [ -n "$rect" ]; then
                    tx="$(printf '%s' "$rect" | grep -oE '"x":[0-9]+' | head -n1 | cut -d: -f2)"
                    ty="$(printf '%s' "$rect" | grep -oE '"y":[0-9]+' | head -n1 | cut -d: -f2)"
                    tw="$(printf '%s' "$rect" | grep -oE '"width":[0-9]+' | head -n1 | cut -d: -f2)"
                    th="$(printf '%s' "$rect" | grep -oE '"height":[0-9]+' | head -n1 | cut -d: -f2)"
                fi
            fi
        fi

        if [ -n "$tx" ] && [ -n "$ty" ] && [ -n "$tw" ] && [ -n "$th" ]; then
            local right_ov=$(( OV_X + OV_W ))
            local right_tg=$(( tx + tw ))
            local bot_tg=$(( ty + th ))
            CROP_X=$(( OV_X < tx ? OV_X : tx ))
            local right=$(( right_ov > right_tg ? right_ov : right_tg ))
            CROP_W=$(( right - CROP_X ))
            CROP_Y="$OV_Y"
            CROP_H=$(( bot_tg - OV_Y ))
            return
        fi
        warn "could not resolve --target-app '$TARGET_APP'; falling back to --below if set"
    fi

    if [ -n "$BELOW" ]; then
        CROP_H=$(( OV_H + BELOW ))
        return
    fi

    die "paste mode needs one of --target-app, --below <px>, or --region WxH+X+Y"
}

if [ "$MODE" = "paste" ]; then
    resolve_paste_crop
fi

CROP_W="$(even "$CROP_W")"
CROP_H="$(even "$CROP_H")"

info "crop: ${CROP_W}x${CROP_H}+${CROP_X}+${CROP_Y}"
fi  # end: capture-only geometry setup

# ---------------------------------------------------------------------------
# Optional background colour wallpaper (X11=feh, Wayland=swaybg)
# ---------------------------------------------------------------------------

BG_PID=""
set_background() {
    [ -n "$BACKGROUND" ] || return 0
    if ! printf '%s' "$BACKGROUND" | grep -Eq '^#?[0-9a-fA-F]{6}$'; then
        warn "--background must be #rrggbb; got '$BACKGROUND' — skipping"
        return 0
    fi
    local hex="${BACKGROUND#\#}"
    if [ "$SESSION_TYPE" = "x11" ] && have feh; then
        local tmp
        tmp="$(mktemp --suffix=.png)"
        ffmpeg -y -f lavfi -i "color=c=0x${hex}:s=${MON_W}x${MON_H}:d=1" \
            -frames:v 1 "$tmp" >/dev/null 2>&1 || { warn "ffmpeg solid-bg gen failed"; return 0; }
        feh --bg-fill "$tmp" >/dev/null 2>&1 || warn "feh --bg-fill failed"
    elif [ "$SESSION_TYPE" = "wayland" ] && have swaybg; then
        swaybg -c "#${hex}" >/dev/null 2>&1 &
        BG_PID=$!
    else
        warn "--background set but no feh/swaybg available — skipping"
    fi
}

clear_background() {
    if [ -n "$BG_PID" ]; then
        kill "$BG_PID" >/dev/null 2>&1 || true
        BG_PID=""
    fi
}

# ---------------------------------------------------------------------------
# fono daemon lifecycle (optional)
# ---------------------------------------------------------------------------

FONO_PID=""
start_fono_if_requested() {
    [ "$START_FONO" -eq 1 ] || return 0
    have fono || die "--start-fono passed but `fono` not on PATH"
    info "spawning fono daemon..."
    fono >/dev/null 2>&1 &
    FONO_PID=$!
    sleep 1.5
}

stop_fono_if_started() {
    if [ -n "$FONO_PID" ]; then
        kill "$FONO_PID" >/dev/null 2>&1 || true
        wait "$FONO_PID" 2>/dev/null || true
        FONO_PID=""
    fi
}

cleanup() {
    stop_fono_if_started
    clear_background
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
# Recording primitive
# ---------------------------------------------------------------------------

countdown() {
    local n=3
    while [ "$n" -gt 0 ]; do
        info "  capture in $n..."
        sleep 1
        n=$(( n - 1 ))
    done
}

# Record full monitor losslessly to $1, for $DURATION seconds.
# Cropping is deferred to the encode pass per risk #1 in the plan.
record_full_monitor() {
    local raw="$1"
    if [ "$SESSION_TYPE" = "x11" ]; then
        ffmpeg -hide_banner -loglevel error -y \
            -f x11grab -framerate "$FPS" \
            -video_size "${MON_W}x${MON_H}" \
            -i "${DISPLAY:-:0}+${MON_X},${MON_Y}" \
            -t "$DURATION" \
            -c:v ffv1 -level 3 -coder 1 -context 1 -g 1 \
            "$raw"
    else
        case "$WAYLAND_RECORDER" in
            wf-recorder)
                wf-recorder -f "$raw" -c ffv1 -r "$FPS" \
                    -g "${MON_W}x${MON_H}+${MON_X},${MON_Y}" &
                local pid=$!
                sleep "$DURATION"
                kill -INT "$pid" 2>/dev/null || true
                wait "$pid" 2>/dev/null || true
                ;;
            gpu-screen-recorder)
                gpu-screen-recorder -w screen -f "$FPS" -o "$raw" &
                local pid=$!
                sleep "$DURATION"
                kill -INT "$pid" 2>/dev/null || true
                wait "$pid" 2>/dev/null || true
                ;;
            *)
                die "no Wayland recorder selected (internal error)"
                ;;
        esac
    fi
}

# ---------------------------------------------------------------------------
# Encode passes
# ---------------------------------------------------------------------------

DEJAVU_FONT=""
for f in \
    /usr/share/fonts/TTF/DejaVuSans-Bold.ttf \
    /usr/share/fonts/dejavu/DejaVuSans-Bold.ttf \
    /usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf \
    /usr/share/fonts/dejavu-sans-fonts/DejaVuSans-Bold.ttf; do
    if [ -r "$f" ]; then DEJAVU_FONT="$f"; break; fi
done

# Build a crop+optional-drawtext filter chain for a given output width.
# Args: out_w label_text(optional)
build_vf() {
    local out_w="$1"
    local label="${2:-}"
    local chain
    chain="crop=${CROP_W}:${CROP_H}:${CROP_X}:${CROP_Y}"
    if [ "$out_w" -gt 0 ]; then
        chain="${chain},scale=${out_w}:-2:flags=lanczos"
    fi
    if [ -n "$label" ] && [ -n "$DEJAVU_FONT" ]; then
        # Escape ':' and '\' in the label for ffmpeg.
        local esc
        esc="$(printf '%s' "$label" | sed -e 's/\\/\\\\/g' -e 's/:/\\:/g' -e "s/'/\\\\'/g")"
        chain="${chain},drawtext=fontfile=${DEJAVU_FONT}:text='${esc}':fontsize=22:fontcolor=white:box=1:boxcolor=black@0.5:boxborderw=8:x=16:y=16"
    fi
    printf '%s' "$chain"
}

encode_mp4() {
    local raw="$1" out="$2" label="${3:-}"
    local vf
    vf="$(build_vf 0 "$label")"
    # shellcheck disable=SC2086 # intentional word-splitting of $H264_CODEC_ARGS
    ffmpeg -hide_banner -loglevel error -y -i "$raw" \
        -vf "${vf},format=yuv420p" \
        $H264_CODEC_ARGS \
        -movflags +faststart \
        "$out"
}

encode_webp() {
    local raw="$1" out="$2" label="${3:-}" out_w="${4:-640}"
    local vf
    vf="$(build_vf "$out_w" "$label")"
    ffmpeg -hide_banner -loglevel error -y -i "$raw" \
        -vf "${vf},fps=20" \
        -c:v libwebp -loop 0 -q:v 70 -preset picture \
        "$out"
}

# Two-pass GIF using palettegen + paletteuse. Args: raw out label width fps.
encode_gif_once() {
    local raw="$1" out="$2" label="$3" out_w="$4" gif_fps="$5"
    local palette
    palette="$(mktemp --suffix=.png)"
    local vf
    vf="$(build_vf "$out_w" "$label")"
    ffmpeg -hide_banner -loglevel error -y -i "$raw" \
        -vf "${vf},fps=${gif_fps},palettegen=stats_mode=diff" \
        "$palette"
    ffmpeg -hide_banner -loglevel error -y -i "$raw" -i "$palette" \
        -lavfi "${vf},fps=${gif_fps} [v]; [v][1:v] paletteuse=dither=bayer:bayer_scale=5" \
        "$out"
    rm -f "$palette"
    if have gifsicle; then
        gifsicle -O3 --batch "$out" >/dev/null 2>&1 || true
    fi
}

filesize_mb() {
    local bytes
    bytes="$(stat -c%s "$1" 2>/dev/null || stat -f%z "$1" 2>/dev/null || echo 0)"
    awk -v b="$bytes" 'BEGIN { printf "%.2f", b/1048576 }'
}

filesize_bytes() {
    stat -c%s "$1" 2>/dev/null || stat -f%z "$1" 2>/dev/null || echo 0
}

# Size-budget auto-tier (5 MB soft, 9.5 MB hard). Tiers depend on mode.
encode_gif_with_budget() {
    local raw="$1" out="$2" label="$3"
    local widths fpses
    if [ "$MODE" = "overlay" ]; then
        widths="480 420 360"
    else
        widths="640 540 480"
    fi
    fpses="15 12 10"

    local soft=$(( 5 * 1024 * 1024 ))
    local hard
    hard="$(awk 'BEGIN { printf "%d", 9.5 * 1048576 }')"

    local best="" best_bytes=0
    for w in $widths; do
        for f in $fpses; do
            info "  gif tier: width=${w} fps=${f}"
            encode_gif_once "$raw" "$out" "$label" "$w" "$f"
            local b
            b="$(filesize_bytes "$out")"
            best="$w@${f}"
            best_bytes="$b"
            if [ "$b" -le "$soft" ]; then
                info "  gif within 5 MB budget at ${best} ($(filesize_mb "$out") MB)"
                return 0
            fi
        done
    done

    if [ "$best_bytes" -gt "$hard" ]; then
        die "gif still > 9.5 MB after all tiers ($(filesize_mb "$out") MB); use --format webp or shorten --duration"
    fi
    warn "gif over 5 MB soft budget ($(filesize_mb "$out") MB at ${best}); within 9.5 MB hard cap — consider .webp"
}

# ---------------------------------------------------------------------------
# Single-take pipeline (used by overlay/paste, and once per style in gallery)
# ---------------------------------------------------------------------------

# Args: name label
do_take() {
    local name="$1"
    local label="$2"

    local raw="$RAW_DIR/raw-${name}-${TIMESTAMP}.mkv"
    info "recording $DURATION s -> $raw"
    record_full_monitor "$raw"

    local out_base="$OUTPUT_DIR/$MODE/$name"
    case "$FORMAT" in
        mp4|all)
            info "encoding mp4..."
            encode_mp4 "$raw" "${out_base}.mp4" "$label"
            info "  $(filesize_mb "${out_base}.mp4") MB -> ${out_base}.mp4"
            ;;
    esac
    case "$FORMAT" in
        webp|all)
            info "encoding webp..."
            local webp_w=640
            [ "$MODE" = "overlay" ] && webp_w=480
            encode_webp "$raw" "${out_base}.webp" "$label" "$webp_w"
            info "  $(filesize_mb "${out_base}.webp") MB -> ${out_base}.webp"
            ;;
    esac
    case "$FORMAT" in
        gif|all)
            info "encoding gif..."
            encode_gif_with_budget "$raw" "${out_base}.gif" "$label"
            info "  $(filesize_mb "${out_base}.gif") MB -> ${out_base}.gif"
            ;;
    esac

    if [ "$KEEP_RAW" -ne 1 ]; then
        rm -f "$raw"
    else
        info "kept raw master: $raw"
    fi
}

# ---------------------------------------------------------------------------
# Gallery orchestration
# ---------------------------------------------------------------------------

CONFIG_FILE="${FONO_CONFIG:-$HOME/.config/fono/config.toml}"

# Swap [overlay].style in-place and SIGHUP fono. Creates the section if missing.
set_waveform_style() {
    local style="$1"
    if [ ! -f "$CONFIG_FILE" ]; then
        warn "no $CONFIG_FILE; skipping style switch for '$style'"
        return 0
    fi
    # If [overlay] section exists, replace style=... within it; else append.
    if grep -q '^\[overlay\]' "$CONFIG_FILE"; then
        # Use awk to edit only inside [overlay] section.
        local tmp
        tmp="$(mktemp)"
        awk -v sty="$style" '
            BEGIN { in_overlay = 0; replaced = 0 }
            /^\[overlay\]/ { in_overlay = 1; print; next }
            /^\[/ && in_overlay {
                if (!replaced) { print "style = \"" sty "\""; replaced = 1 }
                in_overlay = 0
            }
            in_overlay && /^[[:space:]]*style[[:space:]]*=/ {
                print "style = \"" sty "\""
                replaced = 1
                next
            }
            { print }
            END {
                if (in_overlay && !replaced) { print "style = \"" sty "\"" }
            }
        ' "$CONFIG_FILE" > "$tmp"
        mv "$tmp" "$CONFIG_FILE"
    else
        printf '\n[overlay]\nstyle = "%s"\n' "$style" >> "$CONFIG_FILE"
    fi
    pkill -HUP -x fono >/dev/null 2>&1 || true
    sleep "$STYLE_SETTLE"
}

caption_for_style() {
    case "$1" in
        bars)         echo "Bars" ;;
        oscilloscope) echo "Oscilloscope" ;;
        fft)          echo "FFT" ;;
        heatmap)      echo "Heatmap" ;;
        *)            echo "$1" ;;
    esac
}

run_gallery() {
    local IFS=','
    # shellcheck disable=SC2206
    local list=($STYLES)
    unset IFS

    [ "${#list[@]}" -gt 0 ] || die "--styles must list at least one style"

    local clip_paths=()
    for style in "${list[@]}"; do
        info "=== style: $style ==="
        set_waveform_style "$style"
        countdown
        local raw="$RAW_DIR/raw-gallery-${style}-${TIMESTAMP}.mkv"
        record_full_monitor "$raw"

        # Per-style mp4 master (used both as stitching input and as a
        # standalone per-style artefact in target/screencasts/gallery/).
        local clip="$OUTPUT_DIR/gallery/${style}.mp4"
        local label=""
        [ "$LABEL" -eq 1 ] && label="$(caption_for_style "$style")"
        encode_mp4 "$raw" "$clip" "$label"
        info "  per-style clip: $(filesize_mb "$clip") MB -> $clip"
        clip_paths+=("$clip")

        if [ "$KEEP_RAW" -ne 1 ]; then rm -f "$raw"; fi
    done

    # Stitch.
    local master="$OUTPUT_DIR/gallery/gallery.mp4"
    if [ "$LAYOUT" = "concat" ]; then
        local list_file
        list_file="$(mktemp --suffix=.txt)"
        for c in "${clip_paths[@]}"; do
            printf "file '%s'\n" "$c" >> "$list_file"
        done
        # Re-encode (codecs may differ if a clip got re-tried).
        # shellcheck disable=SC2086
        ffmpeg -hide_banner -loglevel error -y \
            -f concat -safe 0 -i "$list_file" \
            $H264_CODEC_ARGS -pix_fmt yuv420p \
            -movflags +faststart \
            "$master"
        rm -f "$list_file"
    else
        # 2x2 xstack grid. Requires exactly 4 inputs; pad/truncate.
        local n="${#clip_paths[@]}"
        if [ "$n" -ne 4 ]; then
            warn "--layout grid expects 4 styles; got $n. Falling back to concat."
            local list_file
            list_file="$(mktemp --suffix=.txt)"
            for c in "${clip_paths[@]}"; do
                printf "file '%s'\n" "$c" >> "$list_file"
            done
            # shellcheck disable=SC2086
            ffmpeg -hide_banner -loglevel error -y \
                -f concat -safe 0 -i "$list_file" \
                $H264_CODEC_ARGS -pix_fmt yuv420p \
                -movflags +faststart \
                "$master"
            rm -f "$list_file"
        else
            ffmpeg -hide_banner -loglevel error -y \
                -i "${clip_paths[0]}" \
                -i "${clip_paths[1]}" \
                -i "${clip_paths[2]}" \
                -i "${clip_paths[3]}" \
                -filter_complex \
                "[0:v]scale=320:-2[a];[1:v]scale=320:-2[b];[2:v]scale=320:-2[c];[3:v]scale=320:-2[d];[a][b][c][d]xstack=inputs=4:layout=0_0|w0_0|0_h0|w0_h0,format=yuv420p[v]" \
                -map "[v]" $H264_CODEC_ARGS \
                -movflags +faststart \
                "$master"
        fi
    fi
    info "stitched master: $(filesize_mb "$master") MB -> $master"

    # Emit derived GIF / WebP from the stitched master, treating it as
    # a "raw" input. CROP_* are already overlay-only; for the stitched
    # master we want the full frame.
    local saved_w="$CROP_W" saved_h="$CROP_H" saved_x="$CROP_X" saved_y="$CROP_Y"
    # Detect master's actual dimensions to crop=full.
    local probe
    probe="$(ffprobe -v error -select_streams v:0 \
        -show_entries stream=width,height -of csv=p=0 "$master" 2>/dev/null || echo)"
    if [ -n "$probe" ]; then
        CROP_W="${probe%,*}"
        CROP_H="${probe#*,}"
        CROP_X=0
        CROP_Y=0
    fi

    case "$FORMAT" in
        webp|all)
            encode_webp "$master" "$OUTPUT_DIR/gallery/gallery.webp" "" 640
            info "  $(filesize_mb "$OUTPUT_DIR/gallery/gallery.webp") MB -> $OUTPUT_DIR/gallery/gallery.webp"
            ;;
    esac
    case "$FORMAT" in
        gif|all)
            encode_gif_with_budget "$master" "$OUTPUT_DIR/gallery/gallery.gif" ""
            info "  $(filesize_mb "$OUTPUT_DIR/gallery/gallery.gif") MB -> $OUTPUT_DIR/gallery/gallery.gif"
            ;;
    esac

    CROP_W="$saved_w"; CROP_H="$saved_h"; CROP_X="$saved_x"; CROP_Y="$saved_y"
}

# ---------------------------------------------------------------------------
# Dispatch
# ---------------------------------------------------------------------------

if [ -n "$CONVERT" ]; then
    [ -r "$CONVERT" ] || die "--convert: cannot read input file: $CONVERT"

    # Probe input dimensions and use them as a no-op crop so build_vf
    # passes the frames straight through to scale/drawtext.
    in_dims="$(ffprobe -v error -select_streams v:0 \
        -show_entries stream=width,height \
        -of csv=p=0:s=x "$CONVERT" 2>/dev/null)"
    in_w="${in_dims%x*}"
    in_h="${in_dims#*x}"
    case "$in_w" in ''|*[!0-9]*) die "--convert: ffprobe could not read video stream from $CONVERT" ;; esac
    case "$in_h" in ''|*[!0-9]*) die "--convert: ffprobe could not read video stream from $CONVERT" ;; esac
    CROP_W="$in_w"; CROP_H="$in_h"; CROP_X=0; CROP_Y=0
    info "convert: $CONVERT (${in_w}x${in_h})"

    # Output basename: same dir as input unless --output-dir was set.
    in_base="$(basename "$CONVERT")"
    in_stem="${in_base%.*}"
    if [ -n "${OUTPUT_DIR_OVERRIDDEN:-}" ] || [ "$OUTPUT_DIR" != "$REPO_ROOT/target/screencasts" ]; then
        out_dir="$OUTPUT_DIR"
    else
        out_dir="$(cd "$(dirname "$CONVERT")" && pwd)"
    fi
    mkdir -p "$out_dir"
    out_base="$out_dir/$in_stem"

    label=""
    [ "$LABEL" = "1" ] && label="$in_stem"

    case "$FORMAT" in
        mp4)
            # Only re-encode mp4 when explicitly requested (--format mp4).
            # 'all' deliberately skips it: the user already has the source.
            if [ "${out_base}.mp4" = "$(cd "$(dirname "$CONVERT")" && pwd)/$in_base" ]; then
                die "--convert --format mp4 would overwrite the input ($CONVERT); pass --output-dir <dir> to write elsewhere"
            fi
            info "encoding mp4..."
            encode_mp4 "$CONVERT" "${out_base}.mp4" "$label"
            info "  $(filesize_mb "${out_base}.mp4") MB -> ${out_base}.mp4"
            ;;
    esac
    case "$FORMAT" in
        webp|all)
            info "encoding webp..."
            webp_w=640
            encode_webp "$CONVERT" "${out_base}.webp" "$label" "$webp_w"
            info "  $(filesize_mb "${out_base}.webp") MB -> ${out_base}.webp"
            ;;
    esac
    case "$FORMAT" in
        gif|all)
            info "encoding gif..."
            encode_gif_with_budget "$CONVERT" "${out_base}.gif" "$label"
            info "  $(filesize_mb "${out_base}.gif") MB -> ${out_base}.gif"
            ;;
    esac

    info "done. artefacts: ${out_base}.{webp,gif}"
    exit 0
fi

set_background
start_fono_if_requested

case "$MODE" in
    overlay)
        countdown
        do_take "overlay" ""
        ;;
    paste)
        countdown
        do_take "overlay-paste" ""
        ;;
    gallery)
        mkdir -p "$OUTPUT_DIR/gallery"
        run_gallery
        ;;
esac

info "done. artefacts under: $OUTPUT_DIR/$MODE/"
