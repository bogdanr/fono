# Troubleshooting

A symptom-first guide. For each problem, the first step is the diagnostic
command — paste its output into a bug report if the suggested fix doesn't
help.

## Dictation produces nothing

### Step 1 — confirm the daemon is running

```sh
pgrep -fa /fono$
```

If empty, start it: `fono` (foreground) or `fono &` (background).

### Step 2 — confirm the hotkey reached the daemon

Tail the daemon log; you should see `INFO fsm event: StartRecording(...)`
when you press the hotkey. If not, see "Hotkey doesn't fire" below.

### Step 3 — confirm the pipeline ran

After `StopRecording` you should see lines like:

```
INFO recording stopped: 2300 ms / 36800 samples
INFO stt: groq 540ms → 42 chars
INFO llm: groq 310ms → 45 chars
INFO inject backend: typed via xtest-paste in 11ms
INFO clipboard: also wrote via xsel [primary]
INFO pipeline ok: capture=2300ms trim=4ms ...
```

If the pipeline ran but text didn't land, jump to "Pipeline ran but
nothing pasted".

### Step 4 — confirm STT and LLM are reachable

```sh
fono doctor
```

Look for the Providers (STT) and Providers (LLM) sections. Each should
show `(active) reachable` next to your selected backend.

## Hotkey doesn't fire

### X11 (i3, KDE-X11, GNOME-X11, Xfce)

`global-hotkey` works out of the box. If it doesn't fire:

- Another app may already own the binding. Try a different hotkey via
  `fono setup` or by editing `[hotkeys]` in config.
- Check the keyboard layout — `Ctrl+Alt+Space` requires literal Ctrl,
  Alt, and Space in your active layout.

### Wayland (sway, Hyprland, KDE-Wayland, GNOME-Wayland)

Most Wayland compositors don't deliver global keys to applications. Bind
your compositor's hotkey to `fono toggle` (IPC fallback):

```
# sway / Hyprland
bindsym $mod+space exec fono toggle

# KDE Plasma — System Settings → Shortcuts → Custom Shortcuts → New →
#   Trigger: Ctrl+Alt+Space
#   Action:  fono toggle

# GNOME — Settings → Keyboard → Custom Shortcuts → +
#   Name:    Fono toggle
#   Command: fono toggle
#   Set:     Ctrl+Alt+Space
```

## Pipeline ran but nothing pasted

The clipboard safety net should always populate the clipboard even when
key injection fails. Press your normal paste shortcut (Ctrl+V or
Shift+Insert) into a text field. If text appears, the problem is purely
key injection — see `docs/inject.md` for backend selection.

If even Ctrl+V/Shift+Insert produces nothing, run:

```sh
fono test-inject "diag" --no-inject
```

Inspect the per-tool table. If every clipboard tool shows `✗ not
installed`, install one:

```sh
sudo pacman -S xsel              # Arch
sudo apt install xsel            # Debian/Ubuntu
sudo slackpkg install xsel       # NimbleX/Slackware
```

For Wayland sessions, prefer `wl-clipboard` (provides `wl-copy`).

## STT failed

### "no API key found"

```sh
fono keys list
```

Add the missing key:

```sh
fono keys add GROQ_API_KEY      # paste your key when prompted
```

The key is stored in `~/.config/fono/secrets.toml` with mode `0600`.

### "401 Unauthorized" or "403 Forbidden"

Your key is rejected by the provider. Verify it works directly:

```sh
curl -H "Authorization: Bearer $(grep GROQ ~/.config/fono/secrets.toml | cut -d'"' -f2)" \
     https://api.groq.com/openai/v1/models | head -20
```

If the curl call also fails, regenerate the key on the provider's
dashboard and re-add it via `fono keys add`.

### "timeout" / "no response"

Your network can't reach the provider. Try a different provider:

```sh
fono use cloud openai            # if Groq is blocked
```

Or switch to local-only:

```sh
fono use local
```

## First dictation is slow (>3 s)

### Cloud providers

- Switch to Groq (typically the fastest): `fono use cloud groq`.
- Disable LLM cleanup if you don't need it: `fono use llm none`.
- Check your network round-trip: `curl -w '%{time_total}\n' -o /dev/null -s https://api.groq.com`.

### Local models

- Run `fono hwprobe` to see your hardware tier.
- If the tier says `unsuitable` or `minimum`, switch to a smaller whisper
  model (`base` instead of `small`, `tiny` instead of `base`).
- The first dictation after daemon start pays a model-load cost
  (~200–600 ms). Subsequent dictations should be faster — `fono history`
  shows actual `stt_ms` per row.
- Increase whisper threads to your physical core count:

  ```toml
  [stt.local]
  threads = 8                    # match physical cores, not hyperthreads
  ```

## "First dictation works, then nothing"

The orchestrator hard-caps the pipeline at one in flight. If a previous
pipeline crashed or stuck, the next press will be ignored with a warning.
Restart the daemon:

```sh
pkill -f /fono$
fono &
```

Report the issue with the last 50 lines of the daemon log.

## Tray icon doesn't appear

### KDE / sway / Hyprland with waybar / dwm

These compositors require a StatusNotifier-compatible tray host. Confirm
yours supports it (`sni-qt`, `waybar` with `tray` module, KDE Plasma's
built-in tray). Without one, run with `--no-tray`:

```sh
fono --no-tray
```

### XEmbed-only systems

`tray-icon` 0.19 doesn't speak XEmbed. Bind the hotkey via your
compositor's keyboard config and run with `--no-tray`.

## Recording is empty / "no audio captured"

```sh
fono doctor              # check the Audio section for input device
```

If `device=""`, your default ALSA/PipeWire input isn't detected. List
candidates:

```sh
arecord -l               # ALSA
pw-cli list-objects | grep node.name | head -20    # PipeWire
```

Set the device explicitly in config:

```toml
[audio]
input_device = "alsa_input.pci-0000_00_1f.3.analog-stereo"
```

## Provider switch doesn't take effect

Today's IPC `Reload` is fully implemented. If a switch via `fono use`
doesn't seem to apply:

1. Confirm the daemon is running: `pgrep -fa /fono$`.
2. Confirm `fono use show` reports the new selection.
3. Trigger one dictation; the next `fono history --limit 1` row should
   show the new `stt_backend` / `llm_backend`.

If it still uses the old backend, restart the daemon. The orchestrator's
hot-swap is exercised by the test suite (`crates/fono/tests/`), so a
real-world miss likely indicates a config-load error — check the daemon
log around the switch.

## "Edit config" tray entry opens Dolphin / a file manager

`xdg-open` defaults vary by distro. Override in config:

```toml
[general]
config_editor = "nvim"      # or "code", "kate", "gedit"
```

(Planned for v0.1.x — meanwhile, edit `~/.config/fono/config.toml` directly.)

## Where to file a bug

GitHub issues, with:

1. Output of `fono doctor` (redact API keys).
2. Output of `fono test-inject "diag"` if injection-related.
3. Last 50 lines of the daemon log.
4. Distribution + desktop environment + display server (X11/Wayland).
