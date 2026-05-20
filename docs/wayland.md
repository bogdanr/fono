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

Wayland compositors don't expose X11's `XGrabKey` API, so Fono uses
a three-tier resolver picked automatically based on what the session
advertises (set `FONO_HOTKEY_BACKEND=portal|x11|disabled` to
override for diagnostics):

1. **`xdg-desktop-portal.GlobalShortcuts`** — preferred on every
   Wayland session that ships it. One consent dialog at first launch
   binds both the dictation and assistant keys for the lifetime of
   the install; subsequent launches reuse the cached approval
   silently. Works out-of-the-box on:
   * KDE Plasma 5.27+ / 6.x (`xdg-desktop-portal-kde`)
   * Hyprland (`xdg-desktop-portal-hyprland`)
   * sway / wlroots with `xdg-desktop-portal-wlr`
   * GNOME 47+ (`xdg-desktop-portal-gnome` 47 added GlobalShortcuts)

2. **gsettings custom-keybindings** — automatic fallback for
   **GNOME 46** (the default on Ubuntu 24.04, whose
   `xdg-desktop-portal-gnome` 46 doesn't yet expose GlobalShortcuts).
   Fono writes the `dictation` and `assistant` bindings into
   `org.gnome.settings-daemon.plugins.media-keys.custom-keybindings`
   pointing at `fono toggle` and `fono assistant`; the CLI then
   routes the action through IPC to the running daemon. Press /
   release semantics are lost on this path (no long-press
   push-to-talk), but the toggle behaviour works.

3. **X11 / Xwayland listener** — the X11-only `global-hotkey` crate
   path, used when neither portal nor gsettings is available
   (typically bare wlroots setups without `xdg-desktop-portal-wlr`,
   or sessions where Xwayland is reachable but the Wayland portal
   isn't).

`fono doctor` reports which backend was selected and why. If the
portal binding dialog never appears or the keys don't fire, you can
always **fall back to a manual compositor binding** as a last resort:

```
# sway (~/.config/sway/config)
bindsym Ctrl+Alt+space exec fono toggle

# hyprland (~/.config/hypr/hyprland.conf)
bind = CTRL ALT, space, exec, fono toggle
```

For **KDE**: System Settings → Shortcuts → Custom Shortcuts → Edit →
New → Global Shortcut → Command/URL → `fono toggle`. For **GNOME**:
Settings → Keyboard → View and Customize Shortcuts → Custom Shortcut.

### Text injection helpers (per compositor)

sway / hyprland / river require `wtype` (preferred) or `ydotool`:

```
sudo apt install wtype     # Debian/Ubuntu
doas pkg_add  wtype        # OpenBSD
# NimbleX / Slackware:  slapt-get --install wtype  (builds from SBo)
```

KDE Plasma Wayland and GNOME use `ydotool`; ensure your user is in
the `input` group and the user-level daemon is running:

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
