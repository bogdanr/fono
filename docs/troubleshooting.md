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
- Check the keyboard layout — `F9`/`F8` should work everywhere; modifier
  combos like `Ctrl+Alt+Space` need literal Ctrl, Alt, and Space in your
  active layout.

If you see this in the log:

```
ERROR X11 hotkey grab denied (BadAccess on X_GrabKey): another
      application … already owns one of the keys you bound. Change
      `[hotkeys].hold` or `[hotkeys].toggle` in ~/.config/fono/config.toml
      …
```

— it means another running process (window manager, browser
extension, screen-recorder, OBS, KDE shortcut, etc.) has grabbed the
key first. Pick a different hotkey in `[hotkeys]` and restart Fono.
Common conflict-free choices: `F11`, `Pause`, `ScrollLock`, or
`Mod4+space` (Super+Space). Note that prior to v0.3.4 this surfaced
as a raw `X Error of failed request: BadAccess … X_GrabKey` line on
stderr without a tracing prefix; if you upgrade and still see the raw
form, you're running an older daemon — `pkill fono` and start fresh.

### Wayland (sway, Hyprland, KDE-Wayland, GNOME-Wayland)

Most Wayland compositors don't deliver global keys to applications. Bind
your compositor's hotkey to `fono toggle` (IPC fallback):

```
# sway / Hyprland
bindsym $mod+space exec fono toggle

# KDE Plasma — System Settings → Shortcuts → Custom Shortcuts → New →
#   Trigger: F9
#   Action:  fono toggle

# GNOME — Settings → Keyboard → Custom Shortcuts → +
#   Name:    Fono toggle
#   Command: fono toggle
#   Set:     F9
```

## LLM responds with a question instead of cleaning my text

Symptom: instead of the cleaned transcript, the injected text reads
something like *"It seems like you're describing a situation, but the
details are incomplete. Could you provide the full text you're referring
to, so I can better understand and assist you?"*

This is **not provider-specific**. The failure mode shows up on every
cleanup backend Fono supports — Cerebras, Groq, OpenAI, OpenRouter,
Ollama, Anthropic, and the local llama.cpp path — because chat-trained
LLMs (regardless of where they run) sometimes treat a short raw
transcript as a conversational fragment addressed to them. F8
push-to-talk just hits it more often because it tends to capture shorter
utterances than F9 toggle.

Fono detects and discards these replies as of v0.2.3. The fix is
identical for every backend: the user message is wrapped in `<<<` /
`>>>` delimiters, the system prompt forbids clarification questions,
and any reply that still looks like a meta-question is rejected so the
raw STT text is injected instead. If you still see clarification-shaped
output:

- Confirm you're on v0.2.3 or newer (`fono --version`).
- Check the daemon log for `LLM returned a clarification reply instead
  of a cleaned transcript; falling back to raw text.` — that line means
  the detector fired and the fallback worked, on whichever backend was
  active.
- Raise `[llm].skip_if_words_lt` (default `3`) in `config.toml` to skip
  the LLM for longer utterances, on cloud or local backends alike.
- If a specific provider is producing borderline replies the heuristic
  doesn't catch, switch backends with `fono use llm <name>` — the fix
  applies to every option, but different chat fine-tunes have different
  refusal personalities and one may suit your dictation style better.

## Cloud STT keeps detecting the wrong language

Symptom: a Groq / OpenAI / etc. transcription comes back in a language
you don't speak (e.g. Russian when you only dictate English and
Romanian). Common with Groq's `whisper-large-v3-turbo` for non-native
English speakers — the model occasionally mis-classifies accented
English.

Fono mitigates this with an in-memory per-backend language cache. The
first correctly-detected utterance populates the cache; the next time
the provider returns a banned (out-of-allow-list) detection, Fono
re-issues the request once with `language=<cached>` and returns the
recovered transcript. Logs show `re-issuing with cached
language=<code>` on the rerun.

What to do if a single utterance still injects the wrong language:

- **Wait for the next utterance.** If your cache was empty (cold start
  with no OS-locale overlap), the first banned detection is accepted as
  the populating sample; the *second* utterance should self-heal.
- **Tray → Languages → Clear language memory.** Useful when the cache
  is stale (e.g. you switched topics from English to Romanian and the
  rerun forced English on the first Romanian clip).
- **Edit `config.toml`** to remove a language from `general.languages`
  permanently. Order doesn't matter; English is just a wizard
  suggestion and can be removed freely.
- **Disable the rerun** with `[stt.cloud].cloud_rerun_on_language_mismatch
  = false` if you'd rather get the (wrong) raw detection than pay one
  extra round-trip on misfires.

The cache resets on every daemon restart and is keyed only by backend
name, never by config-file order — two configs with the same allow-list
in different orders behave identically.

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
