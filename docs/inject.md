# Text injection in Fono

Fono delivers transcribed text to your active window via a layered injection
stack. This document explains the precedence, override mechanisms, and
troubleshooting recipes for each scenario.

## How injection works

For every dictation Fono runs three steps in sequence:

1. **Clipboard copy** — the cleaned text is written to *both* X selections
   (CLIPBOARD via Ctrl+V and PRIMARY via middle-click) and the Wayland
   clipboard, using whichever of `wl-copy`, `xclip`, and `xsel` are
   installed. At least one is required for the safety net to work.
2. **Key injection** — Fono picks the first available backend (priority
   order below) and synthesizes a paste keystroke or types each character.
3. **Notification** (optional, default on) — a desktop toast confirms the
   dictation landed and shows the cleaned text.

If step 2 fails or no backend is available, the clipboard from step 1
remains populated; press the paste shortcut yourself to recover.

## Backend detection priority

Fono picks the first match from this list at startup:

| # | Backend | Detection | Notes |
|---|---|---|---|
| 1 | `FONO_INJECT_BACKEND` env var | manual | `xtest`, `wtype`, `ydotool`, `xdotool`, `enigo`, `none` |
| 2 | `wtype` | `which wtype` | Wayland virtual-keyboard protocol; required for sway/Hyprland |
| 3 | `ydotool` | `which ydotool` | Wayland uinput; needs ydotoold running |
| 4 | `xdotool` | `which xdotool` | X11 / XWayland subprocess injection |
| 5 | `enigo` | compile flag | Built-in libxdo binding; opt-in feature |
| 6 | `xtest-paste` | x11rb XTEST probe | **Pure-Rust X11 fallback** that synthesizes a paste shortcut against the clipboard. Default on Linux. |
| 7 | `clipboard-only` | wl-copy/xclip/xsel | Last resort — populates clipboard, user pastes manually |

Run `fono doctor` to see which backend was picked on your machine, and run
`fono test-inject "hello"` to send a test string without touching the
recording pipeline.

## Paste shortcut (xtest-paste backend only)

When Fono uses the built-in `xtest-paste` backend, it synthesizes a paste
keystroke. The default is **Shift+Insert** because it's the universal X11
paste binding — accepted by GTK, Qt, Xt, every common terminal, Vim/Emacs
in insert mode, Electron apps, and Java/Swing.

Override precedence:

```
FONO_PASTE_SHORTCUT env var  >  [inject].paste_shortcut config  >  default (Shift+Insert)
```

Recognised values: `shift-insert`, `ctrl-v`, `ctrl-shift-v`.

### Per-session override

```sh
FONO_PASTE_SHORTCUT=ctrl-v fono test-inject "test"
```

### Persistent override

```toml
# ~/.config/fono/config.toml
[inject]
paste_shortcut = "ctrl-shift-v"
```

Restart the daemon (or run `fono use show` to trigger a hot reload).

### Test-inject CLI flag

```sh
fono test-inject "verify" --shortcut shift-insert
fono test-inject "verify" --shortcut ctrl-v
fono test-inject "verify" --shortcut ctrl-shift-v
```

The flag overrides both env and config, just for that one invocation.

## Common scenarios

### Wayland (sway, Hyprland, wlroots compositors)

Install `wtype` for direct typing. Fono auto-detects it.

```sh
sudo pacman -S wtype          # Arch
sudo apt install wtype        # Debian/Ubuntu
sudo slackpkg install wtype   # NimbleX/Slackware (if available)
```

If `wtype` isn't packaged, fall back to `ydotool` (needs `ydotoold` running)
or accept the clipboard-only path.

### KDE Plasma Wayland (KWin)

KWin currently does not implement the wlroots `virtual-keyboard-v1`
protocol that `wtype` uses, so `wtype` either silently no-ops or
crashes. **Use the XTEST fallback** — Fono's default `xtest-paste` backend
works under KWin's XWayland session for X11 apps; for Wayland-native KDE
apps, the clipboard safety net plus a manual Ctrl+V is the workaround
until KWin gains virtual-keyboard support.

### X11 (i3, KDE-X11, GNOME-X11, Xfce, MATE, LXQt)

The built-in `xtest-paste` backend works without any extra system
packages. If you prefer direct character typing (no clipboard round-trip),
install `xdotool`:

```sh
sudo pacman -S xdotool
```

### Terminals (xterm, urxvt, alacritty, kitty, gnome-terminal, Konsole)

Default Shift+Insert works in every terminal. If you've remapped paste in
your terminal, set `FONO_PASTE_SHORTCUT` accordingly.

### Vim / Neovim / Emacs

Shift+Insert pastes into insert mode in Vim and into the buffer in Emacs.
You must already be in insert mode (Vim) when the dictation finishes.

### tmux / screen

If you use tmux's copy-mode bindings that capture Shift+Insert, set
`FONO_PASTE_SHORTCUT=ctrl-v` for the session. Better long-term: rebind
tmux's copy-mode key away from Shift+Insert.

## Troubleshooting

### "Nothing pasted into the focused window"

1. Run `fono test-inject "smoke-test" --no-clipboard` and check the log
   line for `inject backend: <name>`. If it says `none`, install one of
   the tools above.
2. If the backend ran successfully but the text didn't land, the focused
   app may not accept the chosen shortcut. Try
   `fono test-inject "smoke-test" --shortcut ctrl-v`.
3. If keystroke injection fails entirely (e.g. `wtype` on KDE Wayland),
   the clipboard fallback should have populated your clipboard. Press
   the paste shortcut yourself to recover.

### "Clipboard is empty"

Run `fono test-inject "x" --no-inject` and inspect the per-tool diagnostic.
Each row shows whether the tool is installed and whether it succeeded:

```
✓ wl-copy  [wayland  ] ok
✗ xclip    [-        ] not installed
✓ xsel     [clipboard] ok
✓ xsel     [primary  ] ok
readback: MATCHES (1 byte via wl-paste)
```

`readback: MATCHES` means the bytes are in the clipboard — if your
clipboard manager doesn't show them, the issue is on the manager's side,
not Fono's. Install `xclip` (most clipboard managers register `xclip`
writes more reliably than `xsel`).

### "First dictation works, second one doesn't"

Possibly a clipboard-manager race. Set `FONO_PASTE_DELAY_MS=50` (planned
in v0.1.x) or report the issue with the output of
`fono test-inject "x" --no-inject` for triage.

### "Text appears in the wrong window"

Fono synthesizes the paste keystroke against whichever window has X
keyboard focus when the pipeline finishes. If your compositor doesn't
keep focus during the brief recording-toast period, increase
`audio.silence_ms` in config so the recording ends with a deliberate
pause instead of mid-word, giving focus time to settle.

## Future work

- Streaming injection (type tokens as they arrive from the LLM) — see
  `docs/plans/2026-04-25-fono-latency-v1.md` Tasks L7/L8.
- Wayland-native global shortcut + virtual-keyboard via the xdg-desktop
  portal — blocked on `global-hotkey` upstream.
- Per-app paste shortcut rules — out of scope for v0.1; revisit if a real
  Shift+Insert failure case appears.
