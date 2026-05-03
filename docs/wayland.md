# Fono on Wayland

Wayland compositors don't expose X11's `XGrabKey` global-hotkey API. Fono
falls back to the `global-hotkey` crate's best-effort path; when that
doesn't work (some bare wlroots setups) you can always use the **CLI
fallback**, binding your compositor's own key handler to `fono toggle`.

## Per-compositor notes

### sway / hyprland / river (wlroots-based)

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

### GNOME (Mutter) / KDE (KWin)

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
injector it chose, and whether a tray host answered on D-Bus.
