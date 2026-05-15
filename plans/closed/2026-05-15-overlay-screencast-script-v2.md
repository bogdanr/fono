# Overlay Screencast Capture Script

## Status: Completed

## Objective

Provide a repeatable `scripts/capture-overlay.sh` helper that records the
Fono overlay during dictation/assistant sessions and emits README-ready
artifacts (animated GIF or MP4) that:

1. Crops tightly to the overlay window (`640 × ≤240` logical px, bottom-
   center, ~48 px above the screen bottom — see
   `crates/fono-overlay/src/real.rs:16` and `:248-264`).
2. Optionally extends the crop downward to include a target window
   (terminal / browser address bar / chat composer) so the README can
   show **paste-into-real-app** behaviour, not just the overlay in
   isolation.
3. Can be invoked repeatedly per waveform style and **stitch the
   results into one combined clip** demonstrating Bars, Oscilloscope,
   FFT, and Heatmap visualisations (`crates/fono-tray/src/lib.rs:193-196`).
4. Stays under GitHub's per-asset upload ceiling so the README renders
   inline on github.com without LFS or an external host.
5. Works on the dominant Fono development surfaces (X11 + Wayland)
   without requiring proprietary tools.

## GitHub embedding constraints (decisions baked into the plan)

* **GIF / animated WebP / APNG in `<img>` or Markdown image** — must be
  ≤ **10 MB** to render inline (camo proxy hard-fails larger files).
  Target ≤ **5 MB** for safety + fast page load.
* **MP4 / WebM via drag-and-drop "video" upload** — GitHub re-hosts on
  `user-images.githubusercontent.com`, ceiling **100 MB** per file, but
  the embed only works in issues/PRs/Releases, **not** in `README.md`
  rendered from the repo. So MP4 is fine for release notes / PRs;
  README must use GIF / WebP (or a poster image linking to a release
  video).

Conclusion: the script's *primary* output is a small (≤ 5 MB) GIF for
`README.md`; it should also emit the source MP4 (high-quality master)
so we can reuse it for release notes and a WebP variant as the smaller
fallback.

## Capture modes (driven by user feedback)

The script exposes three top-level invocation modes via `--mode`:

| Mode | Flag | Frame composition | Use case |
|------|------|-------------------|----------|
| `overlay` | `--mode overlay` *(default)* | Tight crop: `640 × overlay-height`, bottom-centered. | "Hero" hotkey-press → waveform → transcript shot for the top of the README. |
| `paste`   | `--mode paste --target-app <selector> [--below <px>]` | Overlay **plus** a configurable strip of space below it down to / including a target window (e.g. a terminal or a browser). Width is the wider of the overlay and the target window; height extends from the top of the overlay to the bottom of the target window. | Demonstrate that Fono actually injects text into a real app — terminal, IDE, chat. |
| `gallery` | `--mode gallery [--styles bars,oscilloscope,fft,heatmap]` | Runs the capture once per style, switching Fono between each via `fono use waveform <style>`, then stitches the per-style clips into one labelled montage. | Single asset in the README that shows every visualisation option side-by-side or sequentially. |

`overlay` and `paste` produce one MP4 / GIF / WebP triplet; `gallery`
produces one stitched clip plus the individual per-style masters so we
can re-cut without re-recording.

## Implementation Plan

- [x] Task 1. **Add `scripts/capture-overlay.sh`** as the single
      entry point. Shebang `#!/usr/bin/env bash`, `set -euo pipefail`,
      SPDX header comment, usage block, and `--help` flag. The script
      dispatches on `--mode {overlay|paste|gallery}` then runs the
      shared pipeline: environment probe → countdown → record → crop →
      encode → size-check. Rationale: a one-file workflow matches the
      existing `scripts/` style (`bench-sweep.sh`) and keeps the README
      capture reproducible by maintainers.

- [x] Task 2. **Detect the session type** (`$XDG_SESSION_TYPE` /
      `$WAYLAND_DISPLAY`) and pick a recorder accordingly:
      * X11 → `ffmpeg -f x11grab` (universally available, no portal
        prompt, can grab a fixed geometry directly).
      * Wayland (GNOME/KDE/sway) → `wf-recorder` if present, else
        `gpu-screen-recorder`, else `ffmpeg -f pipewire` via the
        `xdg-desktop-portal-*` ScreenCast portal. Fail fast with a
        clear message naming the missing package.
      Rationale: Fono targets both display servers; a single recorder
      command cannot cover both.

- [x] Task 3. **Resolve the overlay geometry** rather than hard-coding
      monitor coordinates. Strategy:
      1. Read the primary display size via `xrandr --query` (X11) or
         `wlr-randr` / `swaymsg -t get_outputs` (wlroots) / fall back
         to the user-provided `--monitor WxH+X+Y` flag.
      2. Compute the **overlay** crop box: `W=640`, `H` configurable
         (`--height`, default 240 to cover the tallest waveform),
         origin centered horizontally, `bottom_inset = 48 + H`.
      3. Apply the active `--scale` factor (HiDPI) so the pixel box
         matches what `winit` actually draws.
      Rationale: hard-coding pixel positions makes the script useless
      on anyone else's monitor; deriving from xrandr/wlr-randr keeps
      it portable. Expose overrides for the edge cases.

- [x] Task 4. **`--mode paste` geometry**: extend the crop box
      downward to include a target window. Two selector strategies,
      tried in order:
      1. **`--target-app <wm_class>`** — locate the target window via
         `xdotool search --class <wm_class>` (X11) or
         `swaymsg -t get_tree` (sway). Read its geometry, then build
         a union rectangle: `x = min(overlay.x, target.x)`,
         `width = max(overlay.right, target.right) - x`,
         `y = overlay.y`, `height = target.bottom - overlay.y`.
      2. **`--below <px>`** — simple fallback that just extends the
         crop downward by N pixels regardless of what's there. Useful
         on Wayland where geometry probes are restricted.
      3. **`--region WxH+X+Y`** — full manual override.
      Document expectation that the user positions the target window
      directly under the overlay before running the script. Rationale:
      the README needs to *show* the paste actually happening; that
      means the recording must contain both the overlay and the
      injection target in the same frame.

- [x] Task 5. **`--mode gallery` orchestration**: loop over the
      `--styles` list (default: `bars,oscilloscope,fft,heatmap` to
      match `crates/fono-tray/src/lib.rs:193-196`). For each style:
      1. Call `fono use waveform <style>` (verify the subcommand
         exists; if not, edit `~/.config/fono/config.toml` directly
         under `[overlay] style = "..."` and `pkill -HUP fono` /
         restart per `CHANGELOG.md:767`).
      2. Wait `--style-settle` seconds (default 1.5) for the daemon
         to reload.
      3. Run the standard record → encode pass into
         `target/screencasts/gallery/<style>.mp4`.
      4. Optionally overlay a corner caption ("Bars" / "Oscilloscope"
         / "FFT" / "Heatmap") via `ffmpeg -vf drawtext` using a system
         font (`/usr/share/fonts/.../DejaVuSans-Bold.ttf`), gated
         behind `--label` (default on for gallery, off otherwise).
      After the loop, **stitch** the per-style clips with
      `ffmpeg -f concat -safe 0 -i clips.txt -c copy gallery.mp4`
      (re-encode if codecs differ), then emit the standard GIF /
      WebP variants from the concatenated master. Rationale:
      recording each style independently and stitching is far more
      reliable than trying to hot-switch styles mid-take.

- [x] Task 6. **Recording lifecycle (shared)**: print a 3-second
      countdown, auto-launch `fono` in a child process if
      `--start-fono` is given (otherwise assume the daemon is already
      running), then record for `--duration` seconds (default 8 for
      `overlay`/`paste`, 5 per style for `gallery`). Capture at
      **30 fps** to a lossless intermediate (`-c:v ffv1` in `.mkv` or
      `-c:v libx264 -crf 0`) under
      `target/screencasts/raw-<mode>-<timestamp>.mkv`. Rationale: a
      lossless master lets us re-encode to GIF / WebP / MP4 without
      compounding artefacts when we re-tune sizes later.

- [x] Task 7. **Post-process with ffmpeg** into three artefacts per
      capture in `target/screencasts/<mode>/`:
      * `<name>.mp4` — H.264 yuv420p, `-crf 23`, faststart. For PRs /
        release notes.
      * `<name>.gif` — two-pass palette pipeline (`palettegen` +
        `paletteuse=dither=bayer:bayer_scale=5`) at **15 fps** and
        `scale=480:-2:flags=lanczos` (overlay mode) or
        `scale=640:-2` (paste / gallery — wider canvas needs more
        horizontal room) to hit the size budget.
      * `<name>.webp` — animated WebP at 20 fps, `-loop 0 -q:v 70`. A
        smaller alternative if the GIF ever blows the budget.

- [x] Task 8. **Size budget enforcement**. After each encode, check
      output size; if `*.gif > 5 MB`, automatically re-run the GIF
      pass at progressively reduced width/fps tiers
      (`overlay`: `480→420→360 px`, `15→12→10 fps`;
      `paste`/`gallery`: `640→540→480 px`, `15→12→10 fps`) until under
      budget or the script gives up with a clear error pointing at the
      WebP fallback. Hard-fail at 9.5 MB to stay inside GitHub's
      10 MB cap. Rationale: keeps the README render-safe without
      manual fiddling after every capture session.

- [x] Task 9. **Dependency probe + helpful errors**. At startup,
      verify `ffmpeg`, `xdpyinfo`/`xrandr`/`xdotool` (X11) or
      `wlr-randr`/`grim`/`swaymsg` (Wayland), and `gifsicle`
      (optional final optimiser) are on `PATH`. Missing tools produce
      a single message naming the packages on Slackware/NimbleX, Arch,
      Debian. Rationale: aligns with the `AGENTS.md` rule about not
      silently installing system packages; matches the documentation
      tone in `docs/providers.md`.

- [x] Task 10. **CLI surface**. Document via `--help` and a short
      `## Capturing screencasts` block in `docs/troubleshooting.md`
      (existing dev-facing doc — *do not create a new markdown file*).
      Full flag set:
      `--mode {overlay|paste|gallery}`,
      `--duration`, `--height`, `--monitor`, `--scale`, `--fps`,
      `--start-fono`, `--keep-raw`, `--output-dir`,
      `--format {gif,mp4,webp,all}`,
      `--target-app <wm_class>` *(paste)*,
      `--below <px>` *(paste)*,
      `--region WxH+X+Y` *(paste, manual override)*,
      `--styles bars,oscilloscope,fft,heatmap` *(gallery)*,
      `--style-settle <s>` *(gallery)*,
      `--label` / `--no-label` *(gallery)*,
      `--layout {concat|grid}` *(gallery — sequential vs 2×2)*.
      Rationale: keeps invocation explicit and avoids surprising
      future maintainers.

- [x] Task 11. **Gallery layout option**. Beyond sequential
      concatenation, support a `--layout grid` that produces a 2×2
      mosaic via `ffmpeg`'s `xstack` filter so all four styles play
      simultaneously. Trade-off (documented in `--help`): the grid is
      visually denser but each tile is 320 × ~120 px, so individual
      waveforms are smaller. Default remains `concat` for clarity.

- [x] Task 12. **README integration step (informational only)**. The
      maintainer drops outputs under `assets/` (already present per
      the file tree) and references them. Suggested embed pattern:
      a hero `assets/overlay.gif` (mode=overlay) near the top of
      `README.md:5`, a `assets/overlay-paste.gif` (mode=paste) under
      the "Lands in any window" bullet at `README.md:20`, and
      `assets/overlay-gallery.gif` (mode=gallery) under a new
      "Visualisations" subsection. Confirm `assets/README` conventions
      before committing large binaries.

- [x] Task 13. **Smoke-test recipe** (manual):
      * `scripts/capture-overlay.sh --mode overlay --duration 6
        --start-fono` → produces ≤ 5 MB hero GIF.
      * `scripts/capture-overlay.sh --mode paste --target-app
        Alacritty --duration 8` → frame contains overlay + terminal
        with pasted text visible.
      * `scripts/capture-overlay.sh --mode gallery --duration 5
        --layout concat` → produces ~20 s stitched clip cycling
        through all four styles with labels.

## Verification Criteria

* `scripts/capture-overlay.sh --help` prints usage covering all three
  modes and exits 0.
* `--mode overlay` on X11 i3 produces `overlay.gif`, `overlay.mp4`,
  `overlay.webp` in `target/screencasts/overlay/`; GIF ≤ 5 MB.
* `--mode paste --target-app <wm_class>` produces a frame whose top
  edge is the overlay and bottom edge is the bottom of the target
  window, with no taskbar/wallpaper bleed outside that rectangle.
* `--mode gallery` produces one master MP4 plus per-style MP4s, all
  four labelled styles visible in the stitched output, total GIF
  size ≤ 5 MB or graceful WebP fallback with a clear message.
* Same three invocations succeed on a Wayland session using the
  portal / `wf-recorder` recorder path.
* `overlay.mp4` ≤ 20 MB, `overlay.webp` ≤ 2 MB.
* Embedding any of the produced GIFs in `README.md` renders inline on
  github.com (verified by pushing to a throwaway branch).
* Missing tools produce a single actionable error mentioning the
  package name per major distro.

## Potential Risks and Mitigations

1. **Wayland compositor refuses geometry-specific capture.**
   Most portals only expose full-screen or single-output capture, not
   sub-rectangles. Mitigation: capture the full output then crop with
   `ffmpeg -vf "crop=W:H:X:Y"` in the encode pass instead of asking
   the recorder to crop.
2. **HiDPI scaling mis-aligns the crop box.**
   `winit` reports logical sizes; X11 grabs physical pixels.
   Mitigation: read `Xft.dpi` / `GDK_SCALE` / `WAYLAND_SCALE` and
   multiply the 640 × H box accordingly; expose `--scale` to override.
3. **`paste` mode: target-window geometry probe fails on Wayland.**
   `xdotool` doesn't work; sway exposes geometry via IPC but
   GNOME/KDE don't. Mitigation: when `--target-app` lookup fails,
   fall back to `--below <px>` (or `--region`) with a one-line
   warning telling the user to pass it explicitly next time.
4. **`paste` mode: target window moves during recording.**
   The crop box is fixed at capture start. Mitigation: in the
   pre-record countdown, lock the target window's position
   (`wmctrl -r <wm_class> -e 0,X,Y,W,H` on X11) and document this in
   `--help`.
5. **Gallery mode: `fono use waveform <subcmd>` may not exist yet.**
   `crates/fono-tray/src/lib.rs:193-196` enumerates the styles but
   only as a tray menu; the CLI surface may be limited to editing
   `~/.config/fono/config.toml`. Mitigation: implement the style
   switch through config-file edit + daemon HUP first; promote to a
   real `fono use` subcommand later if that proves clumsy.
6. **GIF blows the 5 MB budget on long captures (esp. gallery
   concat).**
   Mitigation: Task 8's auto-tiering, plus document that gallery
   captures over 20 s should prefer the WebP fallback or use
   `--layout grid` (single 5 s clip is plenty for 2×2).
7. **Recorder leaks the desktop wallpaper / other windows.**
   The overlay is transparent outside its accent strip, so the
   underlying desktop shows through. Mitigation: instruct the
   maintainer (in the smoke-test recipe) to capture against a
   neutral solid-colour background or temporarily set the wallpaper
   to `#202020`. Optionally add `--background=hex` that pre-sets a
   feh/swaybg wallpaper for the duration of the recording.
8. **GitHub silently rewrites GIFs over 10 MB to a static frame.**
   Mitigation: hard-fail the script when the final GIF exceeds
   9.5 MB even after the auto-tier ladder; surface the WebP path
   explicitly.
9. **`gifsicle` not available on all distros.**
   Mitigation: treat it as optional; if missing, skip the final
   `gifsicle -O3` optimisation pass and warn (not fail).

## Alternative Approaches

1. **Use `peek` or `byzanz-record` directly, no script.**
   Trade-off: zero engineering cost, but the maintainer has to
   re-pick geometry every time, no `paste` / `gallery` orchestration,
   no size-budget enforcement. Acceptable as a quick fallback
   documented in the script's `--help`.
2. **Record an MP4 only and link to a GitHub-hosted upload from the
   README via a static poster image.**
   Trade-off: avoids the GIF size budget entirely and gives crisp
   60 fps playback, but the README no longer auto-animates on first
   view. Worth offering as `--format=mp4` for cases where the demo
   is too long for a sane GIF.
3. **Render the overlay headlessly into a series of PNG frames from
   within `fono-overlay` itself, then assemble offline.**
   Trade-off: pixel-perfect, reproducible, no compositor coupling,
   and no desktop bleed — but requires a non-trivial test harness in
   the crate and won't capture the real-world hotkey/injection flow
   that sells the tool. Defer; revisit if recording flake-rate
   becomes a problem.
4. **Skip `gallery` mode; show one waveform in the README and
   document the others in text.**
   Trade-off: simpler script, but loses the strongest visual
   marketing beat (Fono has four bespoke animations). Rejected.
5. **For `gallery`, instead of stitching post-hoc, build a special
   `fono` debug subcommand that auto-cycles styles every N
   seconds during a single recording.**
   Trade-off: removes ffmpeg concat complexity, but adds a binary
   feature that exists only for marketing. Worse engineering trade;
   keep the cycling in the script.
