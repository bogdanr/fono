# Fono on Wayland

## How the overlay works on Wayland

Fono picks one of three overlay backends at runtime based on which
protocols (and which display servers) are reachable from the daemon's
session:

| Backend | Compositor / situation | Why |
|---|---|---|
| `wlr-layer-shell` | sway, hyprland, river, KDE Plasma 5.27+, COSMIC, Wayfire, niri, labwc | Native panel protocol — bottom-centre anchored, on-top, no focus / click theft, no taskbar entry, ARGB transparency. |
| `x11-override-redirect` (via **Xwayland**) | GNOME / Mutter, and any other Wayland session where layer-shell isn't advertised but Xwayland is running. Also the native path on Xorg sessions. | Mutter has refused to implement layer-shell. Override-redirect Xwayland windows bypass the window manager entirely — client controls position, the surface stacks above normal windows, and it's excluded from Alt+Tab and the taskbar. Same UX as native X11. Transparency via XRender ARGB visuals; fractional HiDPI scaling renders cleanly. |
| `noop` | headless / no display server / Wayland session with neither layer-shell nor Xwayland | Silent terminal sink so the daemon never aborts on a missing display. |

The two graphical backends use the same ARGB8888 framebuffer, so
transparency, rounded corners, and the volume-bar / FFT / oscilloscope
visualisations look identical wherever the overlay shows up. Both
refuse keyboard focus and the wlr backend passes pointer clicks
through via an empty `wl_region`.

The previous overlay path went through `winit + softbuffer` on Wayland.
That stack hard-coded `XRGB8888` (no alpha) and used `xdg_toplevel`
without any positioning hooks, which on GNOME produced an opaque
charcoal rectangle in the top-left corner that stole focus. The
pluggable backend rewrite fixes that; winit's Wayland features have
been dropped entirely (winit is now compiled X11-only on Linux).

### Clicks on the overlay (X11 / Xwayland path)

The X11 override-redirect backend does not yet set an input shape, so
pointer events that land *on the overlay rectangle* are consumed by
the overlay (and ignored) rather than passed through to the window
underneath. The overlay is small (≈ 640 × 80 px) and only visible
during dictation, so this is rarely noticeable; click-passthrough via
`XFixesSetWindowShapeRegion` is a planned follow-up. The wlr layer-
shell backend passes clicks through correctly via an empty
`wl_region`.

### `FONO_OVERLAY_BACKEND` escape hatch

Force a specific backend for diagnostics:

```sh
FONO_OVERLAY_BACKEND=wlr   fono dictate     # force wlr-layer-shell
FONO_OVERLAY_BACKEND=x11   fono dictate     # force the X11 path (Xwayland)
FONO_OVERLAY_BACKEND=noop  fono dictate     # disable the overlay entirely
```

Unknown values fall through to automatic selection with a warning in
the log. The `noop` backend is also the terminal fallback when no
graphics environment is detected (e.g. `fono` running as a headless
inference service, or a Wayland session without layer-shell or
Xwayland); the daemon never aborts on a missing display server.

### Verifying the chosen backend

`fono doctor` reports the selected backend and its capabilities:

```
Overlay     : wlr-layer-shell (transparency=yes positioning=client focus-passthrough=yes click-passthrough=yes) — Wayland + layer-shell preferred (falls through to Xwayland on GNOME / Mutter)
```

On a Wayland session with neither layer-shell nor Xwayland, the
backend will be `noop` and `fono doctor` will print a hint to install
your distro's `xwayland` package.

### Troubleshooting

1. **`fono doctor` reports backend `noop` on a Wayland session.**
   Either the daemon couldn't connect to the Wayland socket
   (check `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, and that the daemon
   runs inside your graphical session, not a stale terminal
   multiplexer), **or** your session has neither `zwlr_layer_shell_v1`
   nor Xwayland — install your distro's `xwayland` package (e.g.
   `sudo apt install xwayland`) to enable the overlay.
2. **Overlay is invisible / pure black.** You're probably on a build
   without ARGB8888 support in the compositor. Try
   `FONO_OVERLAY_BACKEND=noop` so dictation still works, and file a
   compositor bug.

## Global hotkeys

Wayland compositors don't expose X11's `XGrabKey` global-hotkey API. Fono
falls back to the `global-hotkey` crate's best-effort path; when that
doesn't work (some bare wlroots setups) you can always use the **CLI
fallback**, binding your compositor's own key handler to `fono toggle`.

### Per-compositor notes

#### sway / hyprland / river (wlroots-based)

Global hotkeys generally work. If they don't, bind the compositor shortcut:

```
# sway (~/.config/sway/config)
bindsym Ctrl+Alt+space exec fono toggle

# hyprland (~/.config/hypr/hyprland.conf)
bind = CTRL ALT, space, exec, fono toggle
```

Text injection requires `wtype` (preferred) or `ydotool`:

```
sudo apt install wtype     # Debian/Ubuntu
doas pkg_add  wtype        # OpenBSD
# NimbleX / Slackware:  slapt-get --install wtype  (builds from SBo)
```

#### GNOME (Mutter) / KDE (KWin)

Neither compositor implements the `org.freedesktop.portal.GlobalShortcuts`
portal as of 2026-04. Bind a compositor-level shortcut to `fono toggle`:

* **GNOME:** Settings → Keyboard → View and Customize Shortcuts → Custom
  Shortcut → `F9` → `fono toggle`.
* **KDE:** System Settings → Shortcuts → Custom Shortcuts → Edit → New →
  Global Shortcut → Command/URL → `fono toggle`.

Injection uses `ydotool` on Mutter/KWin; ensure the daemon runs with a
user in the `input` group:

```
sudo gpasswd -a "$USER" input
systemctl --user enable --now ydotool.service
```

## Tray

KDE, GNOME (with the AppIndicator extension), sway + waybar with the
`tray` module, and hyprland with `waybar` all host a StatusNotifierItem
and Fono's tray icon appears automatically. Bare i3 without `polybar`/
`waybar` has no tray host; Fono logs one warning and runs without the
tray icon — dictation and the overlay are unaffected.

## Verification

`fono doctor` prints `session_type`, compositor (when detectable), the
injector it chose, the selected overlay backend, and whether a tray
host answered on D-Bus.
