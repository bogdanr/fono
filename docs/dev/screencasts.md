# Capturing screencasts

How to record the overlay screencasts used in the README and release notes.

`scripts/capture-overlay.sh` records the overlay window for the README and
release notes. It runs entirely against the live daemon — no test harness —
so the captures show the real hotkey → waveform → inject flow.

## Modes

- `--mode overlay` *(default)* — tight crop of the overlay (640 × ≤240
  logical px, bottom-centered) for the README hero shot.
- `--mode paste --target-app <wm_class>` — extends the crop down to
  include a target window (terminal, editor, browser) so the screencast
  shows the pasted text landing in a real surface. Falls back to
  `--below <px>` when geometry probes are restricted (most Wayland
  compositors), or `--region WxH+X+Y` for a fully manual crop.
- `--mode gallery [--styles bars,oscilloscope,fft,heatmap]` — records
  one take per waveform style, labels each clip, and stitches them
  into one master via `ffmpeg -f concat` (or `--layout grid` for a
  2×2 `xstack` mosaic). Style switching edits `[overlay].style` in
  `~/.config/fono/config.toml` and sends `SIGHUP` to the daemon.

## Output triplet

Each take emits MP4 (`-crf 23` faststart), animated GIF (palette
pipeline, auto-tiered down 480→420→360 px / 15→12→10 fps for overlay,
640→540→480 for paste / gallery until under the 5 MB soft budget; hard
fail above 9.5 MB to stay inside GitHub's 10 MB inline cap), and
animated WebP (q 70). MP4 is the master for PRs / release notes; GIF
or WebP go in `README.md`.

## Dependencies

The script aborts at startup with a single message listing every
missing tool. On NimbleX / Slackware install:
`ffmpeg`, `xorg-xrandr`, `xdotool`, `wmctrl` (X11) or `wlr-randr`,
`grim`, `wf-recorder` (Wayland). `gifsicle` is optional but produces
noticeably smaller GIFs when present. See `--help` for the full flag
set and per-distro package names.

## Recipes

```sh
# README hero (X11/i3, 6 s, daemon auto-spawned).
scripts/capture-overlay.sh --mode overlay --duration 6 --start-fono

# "Lands in a real app" demo — position Alacritty under the overlay first.
scripts/capture-overlay.sh --mode paste --target-app Alacritty --duration 8

# All four waveform styles, labelled and stitched into one clip.
scripts/capture-overlay.sh --mode gallery --duration 5 --layout concat
```

HiDPI displays need `--scale 1.25` (or whatever `GDK_SCALE` /
`Xft.dpi/96` is). Compositors that refuse sub-region capture (most
Wayland portals) record the full output and rely on the ffmpeg crop
pass — geometry is still resolved via `wlr-randr` or `swaymsg`.
