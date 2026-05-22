# Text injection in Fono

Fono delivers transcribed text to your active window via a layered injection
stack. This document explains the precedence, override mechanisms, and
troubleshooting recipes for each scenario.

## How injection works

For every dictation Fono runs three steps in sequence:

1. **Clipboard copy** — the cleaned text is written to the CLIPBOARD and
   PRIMARY X selections and the Wayland clipboard, using whichever of
   `wl-copy`, `xclip`, and `xsel` are installed. At least one is required
   for the safety net to work.
2. **Key injection** — Fono picks the first available backend (priority
   order below) and types the text into the focused window.
3. **Notification** (optional, default on) — a desktop toast confirms the
   dictation landed and shows the cleaned text.

If step 2 fails or no backend is available, the clipboard from step 1
remains populated; press your usual paste shortcut to recover.

## Backend detection priority

Fono picks the first match from this list at startup:

| # | Backend | Detection | Notes |
|---|---|---|---|
| 1 | `FONO_INJECT_BACKEND` env var | manual | `enigo`, `wtype`, `ydotool`, `xdotool`, `xtest`, `none` |
| 2 | `enigo` | compile flag | Built-in libxdo binding; opt-in `enigo-backend` feature, X11 only |
| 3 | `wtype` | `which wtype` | Wayland virtual-keyboard protocol; required for sway/Hyprland |
| 4 | `ydotool` | `which ydotool` | Wayland uinput; needs ydotoold running |
| 5 | `xdotool` | `which xdotool` | X11 / XWayland subprocess injection |
| 6 | `xtest-type` | x11rb XTEST probe | **Pure-Rust X11 fallback** that types each character via XTEST. Default on Linux when nothing above is installed. |
| 7 | `clipboard-only` | wl-copy/xclip/xsel | Last resort — populates clipboard, user pastes manually |

Run `fono doctor` to see which backend was picked on your machine, and run
`fono test-inject "hello"` to send a test string without touching the
recording pipeline.

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
crashes. **Use the XTEST fallback** — Fono's built-in `xtest-type` backend
works under KWin's XWayland session for X11 apps; for Wayland-native KDE
apps, the clipboard safety net plus a manual Ctrl+V is the workaround
until KWin gains virtual-keyboard support.

### X11 (i3, KDE-X11, GNOME-X11, Xfce, MATE, LXQt)

The built-in `xtest-type` backend works without any extra system
packages. `xdotool` is detected first if present, but `xtest-type` covers
the same ground without the subprocess overhead.

### Terminals, Vim/Emacs, tmux

Because Fono types characters directly rather than synthesizing a paste
shortcut, terminal copy-paste rebindings and Vim's normal-vs-insert mode
don't interfere — as long as the focused window accepts keyboard input,
the text lands. In Vim, you still need to be in insert mode when the
dictation finishes for the characters to enter the buffer.

## Troubleshooting

### "Nothing pasted into the focused window"

1. Run `fono test-inject "smoke-test" --no-clipboard` and check the log
   line for `inject backend: <name>`. If it says `none`, install one of
   the tools above.
2. If the backend ran successfully but the text didn't land, the focused
   app may not accept synthesized key events (some Electron-on-Wayland
   builds are picky). Try forcing a different backend with
   `FONO_INJECT_BACKEND=wtype fono test-inject "smoke-test"`.
3. If keystroke injection fails entirely (e.g. `wtype` on KDE Wayland),
   the clipboard fallback should have populated your clipboard. Press
   your usual paste shortcut to recover.

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

Possibly a clipboard-manager race. Run
`fono test-inject "x" --no-inject` to confirm the clipboard tools succeed
on a fresh invocation, and file an issue with the diagnostic output if the
problem reproduces.

### "Text appears in the wrong window"

Fono types into whichever window has keyboard focus when the pipeline
finishes. If your compositor doesn't keep focus during the brief
recording-toast period, increase `audio.auto_stop_silence_ms` in config
so the recording ends with a deliberate pause instead of mid-word,
giving focus time to settle.
