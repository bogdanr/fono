# Troubleshooting

A symptom-first guide. For each problem, the first step is the diagnostic
command — paste its output into a bug report if the suggested fix doesn't
help.

The commands on this page are Linux-flavoured; on macOS or Windows the
same diagnostics apply but the platform notes live in
[build-macos.md](build-macos.md) and [build-windows.md](build-windows.md).

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

After `StopRecording` you should see a single `pipeline:` summary line:

```
INFO pipeline: 2.3s trim=4ms | en | stt groq 540ms 42 chars | polish groq 310ms [app] 42→45 chars | inject xtest-type 11ms
```

**Reading the fields:**

| Field | Meaning |
|---|---|
| `2.3s` | Capture duration (seconds ≥ 10 s, ms below) |
| `trim=4ms` | Time spent trimming silence from the audio |
| `en` | Language detected by the STT backend |
| `stt groq 540ms 42 chars` | STT backend, latency, and character count of the raw transcript |
| `polish groq 310ms [app] 42→45 chars` | Polish backend, latency, context enrichment tag, and char count before → after |
| `inject xtest-type 11ms` | Injection backend used and latency |

**Context enrichment tags** (the `[…]` after the polish latency):

| Tag | Meaning |
|---|---|
| `[-]` | Default prompt only — no app context |
| `[app]` | Active window class was detected and sent to polish |
| `[app+rule]` | Window class matched a `[[context_rules]]` entry |
| `[app+dict]` | Window class + personal dictionary |
| `[app+rule+dict]` | Window class + context rule + personal dictionary |

**Slow-stage highlighting:** latency values that exceed thresholds (STT > 2 s,
polish > 1.5 s, inject > 500 ms) appear in yellow when the log is shown in a
real terminal. When the log is redirected (journald, a file, a pipe), the
numbers are emitted plain — no colour escape, no marker character — so captured
logs stay trivially parseable. Honours `NO_COLOR`.

If `polish skipped` appears instead of a polish entry, the utterance was below
the `[polish].skip_if_words_lt` threshold (default: 3 words).

If the pipeline line appears but text didn't land, jump to "Pipeline ran but
nothing pasted".

### Step 4 — confirm STT and polish are reachable

```sh
fono doctor
```

Look for the Providers (STT) and Providers (Polish) sections. Each should
show `(active) reachable` next to your selected backend.

## Assistant (F8) turn diagnostics

Every F8 voice-assistant turn emits a single `assistant:` INFO line at the
end. Pair it with the `pipeline:` line above to reason about the two
distinct flows in one place.

```
INFO assistant: 4823ms | en | stt 580ms 14 chars in | llm 234ms ttfb / 2103ms 312 chars out [fono_screen 1284ms] | tts 420ms ttfa / 8 sent
```

**Reading the fields:**

| Field | Meaning |
|---|---|
| `4823ms` | Total turn time — STT start to last audio queued (seconds ≥ 10 s, ms below). Drain time is **not** included. |
| `en` | Language tag (`?` when neither STT nor config supplied one) |
| `stt 580ms 14 chars in` | Batch STT latency + user-text length in chars |
| `stt skipped (live) 14 chars in` | Live-streaming F8 path — the streaming STT already produced text; batch STT did not run |
| `llm 234ms ttfb / 2103ms 312 chars out` | Time-to-first-delta + full LLM stream time + reply char count. Includes any tool-roundtrip wait. |
| `[fono_screen 1284ms]` | **Optional.** Tool name + exec time. Multiple tools comma-separated. Omitted entirely on text-only turns. |
| `tts 420ms ttfa / 8 sent` | Time-to-first-audio queued for playback + number of sentences synthesised |
| `tts none` | No audio produced (cancelled before TTS, empty reply) |
| `| aborted` (tail) | Turn was cancelled mid-stream (Esc, toggle off, hotkey re-press) |

**Tool outcome tags** (the `[…]` segment, only present when tools were used):

| Tag | Meaning |
|---|---|
| `[fono_screen 1284ms]` | Tool ran successfully — exec time shown |
| `[fono_screen failed=cancelled]` | User pressed Escape in the OS-side region picker |
| `[fono_screen failed=private]` | Focused window is on the private-window allow-list (KeePassXC, Bitwarden, …) |
| `[fono_screen failed=no-tool]` | No grabber tool (scrot/maim/grim/import/spectacle/gnome-screenshot) found in `PATH` |
| `[fono_screen failed]` | Anything else (timeout, downscale failure, …) |

**Slow-stage highlighting:** latency values that exceed thresholds appear in
yellow when the log is shown in a real terminal. When stderr is redirected
(journald, a file, a pipe for parsing) the numbers are emitted plain — no ANSI
escape, no marker character — so captured logs stay trivially parseable.
Honours `NO_COLOR`.

| Stage | Yellow above |
|---|---|
| STT | 2 000 ms |
| LLM ttfb | 1 500 ms |
| LLM total | 5 000 ms |
| Tool exec | 1 000 ms |
| TTS ttfa | 1 500 ms |

For per-phase debug logging (the individual `STT:`, `first LLM delta`,
`first audio queued` lines that used to appear at INFO level) run with
`RUST_LOG=fono::assistant=debug`.

### Deep dive: per-turn performance traces

When the INFO summary isn't enough — "why was this turn slow?" — run the
daemon with `FONO_ASSISTANT_TRACE` pointing at a directory:

```sh
FONO_ASSISTANT_TRACE=/tmp/fono-traces fono
```

Every dictation turn, assistant turn, and daemon startup then writes a
Chrome Trace Event JSON file (`dictation-*.json`, `assistant-*.json`,
`startup-*.json`) into that directory. Open it in `chrome://tracing`,
[Perfetto](https://ui.perfetto.dev), or `about:tracing` to see the full
waterfall: audio capture, STT, polish, LLM prefill/decode (including
prompt-cache hits, misses, and restores), tool calls, TTS, and injection,
each on its own lane with timings. The final `turn.finish` event carries a
cache scoreboard (`cache_hits`, `cache_misses`, `cold_prefills`,
`bytes_restored`) — the headline number for prompt-cache diagnostics.

Pointing the variable at a path ending in `.json` writes a single trace to
that exact file instead. Setting it empty, to `0`, or to `false` disables
tracing.

> **Privacy note:** traces include the full prompt text — your dictated
> words, the transcript being cleaned, and the assistant conversation — so
> treat trace files like the history database. Set
> `FONO_ASSISTANT_TRACE_PROMPT=0` to omit prompt text from traces, and
> prefer a private directory over a world-readable `/tmp` path on shared
> machines. Tracing is off unless `FONO_ASSISTANT_TRACE` is set.

## Hotkey doesn't fire

### X11 (i3, KDE-X11, GNOME-X11, Xfce)

`global-hotkey` works out of the box. If it doesn't fire:

- Another app may already own the binding. Try a different hotkey via
  `fono setup` or by editing `[hotkeys]` in config.
- Check the keyboard layout — `F7`/`F8` should work everywhere; modifier
  combos like `Ctrl+Alt+Space` need literal Ctrl, Alt, and Space in your
  active layout.
- **htop overlap**: htop binds F7/F8 to nice +/-. The keystroke only
  reaches htop while it has focus, so it doesn't normally collide with
  Fono's global hotkey. If you live inside htop and want a free key,
  rebind `[hotkeys].dictation` / `[hotkeys].assistant` to e.g. `F11`,
  `Pause`, or `ScrollLock`.

If you see this in the log:

```
ERROR X11 hotkey grab denied (BadAccess on X_GrabKey): another
      application … already owns one of the keys you bound. Change
      `[hotkeys].dictation` or `[hotkeys].assistant` in
      ~/.config/fono/config.toml …
```

— it means another running process (window manager, browser
extension, screen-recorder, OBS, KDE shortcut, etc.) has grabbed the
key first. Pick a different hotkey in `[hotkeys]` and restart Fono.
Common conflict-free choices: `Pause`, `ScrollLock`, `Insert`, or
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
#   Trigger: F7
#   Action:  fono toggle

# GNOME — Settings → Keyboard → Custom Shortcuts → +
#   Name:    Fono toggle
#   Command: fono toggle
#   Set:     F7
```

## Polish responds with a question instead of cleaning my text

Symptom: instead of the cleaned transcript, the injected text reads
something like *"It seems like you're describing a situation, but the
details are incomplete. Could you provide the full text you're referring
to, so I can better understand and assist you?"*

This is **not provider-specific**. The failure mode shows up on every
cleanup backend Fono supports — Cerebras, Groq, OpenAI, OpenRouter,
Ollama, Anthropic, and the local llama.cpp path — because chat-trained
LLMs (regardless of where they run) sometimes treat a short raw
transcript as a conversational fragment addressed to them. Push-to-talk
captures (long-press of the dictation hotkey) hit this case more often
than a quick toggle press because they tend to capture shorter
utterances.

Fono detects and discards these replies as of v0.2.3. The fix is
identical for every backend: the user message is wrapped in `<<<` /
`>>>` delimiters, the system prompt forbids clarification questions,
and any reply that still looks like a meta-question is rejected so the
raw STT text is injected instead. If you still see clarification-shaped
output:

- Confirm you're on v0.2.3 or newer (`fono --version`).
- Check the daemon log for `polish returned a clarification reply instead
  of a cleaned transcript; falling back to raw text.` — that line means
  the detector fired and the fallback worked, on whichever backend was
  active.
- Raise `[polish].skip_if_words_lt` (default `3`) in `config.toml` to skip
  the polish step for longer utterances, on cloud or local backends alike.
- If a specific provider is producing borderline replies the heuristic
  doesn't catch, switch backends with `fono use polish <name>` — the fix
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

Your key is rejected by the provider. Since v0.7.2 the daemon also
fires a critical desktop notification on the first auth-class failure
of each dictation session, so you no longer need to tail journalctl to
notice — the same notification distinguishes STT-key vs polish-key
failures and tells you which provider rejected the key. A polish-key
failure still injects the raw STT transcript so the dictation is not
lost; an STT-key failure injects nothing and asks you to update the
key via the tray or `fono doctor`.

Verify the key works directly:

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
- Disable polish if you don't need it: `fono use polish none`.
- Check your network round-trip: `curl -w '%{time_total}\n' -o /dev/null -s https://api.groq.com`.

### Local models

- Run `fono hwprobe` to see your hardware tier.
- If the tier says `unsuitable` or `minimum`, switch to a smaller whisper
  model (`small` instead of `large-v3-turbo`, `tiny` instead of `small` —
  `base` was removed from the model ladder; see
  [providers.md](providers.md#speech-to-text) for the current rungs).
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
built-in tray, `snixembed` as a shim). Without one, the daemon logs a
single warning at startup and continues without a tray icon — dictation,
the overlay, and the IPC commands (`fono toggle`, `fono record`, …) all
keep working.

### XEmbed-only systems

`tray-icon` 0.19 doesn't speak XEmbed. Bind the hotkey via your
compositor's keyboard config; the tray icon will be skipped automatically
on hosts where no StatusNotifier watcher is registered.

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
   show the new `stt_backend` / `polish_backend`.

If it still uses the old backend, restart the daemon. The orchestrator's
hot-swap is exercised by the test suite (`crates/fono/tests/`), so a
real-world miss likely indicates a config-load error — check the daemon
log around the switch.

## Voice assistant (F8) doesn't trigger

The assistant pipeline is independent of dictation and needs an
assistant chat backend selected before F8 will do anything useful. A
TTS backend is optional — without one the assistant shows the reply as
an on-screen text panel instead of speaking it (see
[providers.md → Text-only mode](providers.md#text-to-speech-assistant-audio-replies)).

```sh
fono doctor
```

Look for the `assistant:` row; it should show `(active) reachable` or
`ready`. If it says `not configured`:

```sh
fono use assistant local          # or groq, anthropic, cerebras, openai, gemini
fono use tts openai               # optional — spoken replies instead of the text panel
```

If `fono doctor` reports the backends ready but pressing F8 still
produces nothing, tail the daemon log while you press the key. You
should see `INFO fsm event: AssistantStart` on press and
`AssistantStop` on the second press (toggle mode) or release (hold
mode). If neither appears, the F8 binding isn't reaching the
daemon — see "Hotkey doesn't fire" above (Wayland users must bind F8
in the compositor and run `fono assistant press` / `fono assistant
release`).

## Voice assistant produces no audio

The text reply was generated but you didn't hear it.

1. Confirm `paplay` is installed — it's the default playback path on
   the slim release build:

   ```sh
   which paplay && paplay --version
   ```

   On Debian/Ubuntu install `pulseaudio-utils`; on Arch
   `libpulse`; on Slackware/NimbleX `pulseaudio`.
2. If you selected `tts = openai`, confirm `OPENAI_API_KEY` is set and
   reachable: `fono keys check`.
3. If you selected `tts = wyoming`, confirm your Piper / Wyoming
   server is running and reachable on the configured `host:port`. The
   daemon logs `wyoming tts: connect <host>:<port>` on the first turn.
4. Lower the system mixer or the per-app volume? The daemon doesn't
   touch volume — `pactl list sink-inputs` will show the playback
   stream while a reply is speaking.

## Assistant overlay disappears before audio finishes

Known follow-up tracked for v0.7.1: the thinking / speaking overlay
returns to idle as soon as the TTS pump enqueues its last chunk, but
`paplay` keeps playing the buffered audio for up to a second after.
The reply is still complete and audible — only the visual cue is
early. There's a `TODO` marker in `fono::assistant::run_assistant_turn`
where the drain wait will land.

## "Edit config" tray entry opens Dolphin / a file manager

`xdg-open` defaults vary by distro. There is no in-app override today; edit
`~/.config/fono/config.toml` directly, or change the system default for
`text/plain` via `xdg-mime default <editor>.desktop text/plain`.

## Wake word doesn't trigger

Run `fono doctor` — it shows whether the wake word is enabled, which
detector backend would run, each phrase's target, and whether the model
file is cached. Common causes:

- `[wakeword].enabled` is `false` (the default — always-on listening is
  opt-in; see [configuration.md](configuration.md#wakeword--always-on-wake-word)).
- The phrase's model file isn't downloaded yet — `fono doctor` reports
  the cache state.
- You spoke during a recording or an assistant turn: the idle listener
  suspends while either is active and resumes when Fono goes idle.

## Speaker tag missing in history

Speaker verification is off by default, and even when enabled it only
tags dictations that match an **enrolled** voice. Enable
`[speaker].enabled`, enroll yourself (settings-page enrollment card, or
manage profiles with `fono speaker list` / `rename` / `test`), and check
`fono doctor` for the verification section — it warns when verification
is on but nobody is enrolled. Full guide: [speakers.md](speakers.md).

## Settings page won't open

The web settings listener (`[server.web]`) is off by default. `fono
config web` or the tray's **Settings…** entry starts it on demand and
opens the browser. It binds to loopback (`127.0.0.1:10808`) — it is not
reachable from other machines unless you widen `bind`, in which case
keep `auth = true` and create an inbound API key first. See
[configuration.md → Settings in the browser](configuration.md#settings-in-the-browser).

## The OpenAI/Ollama API isn't reachable

The LLM server (`[server.llm]`) is off by default. Enable it in config
or via the tray (*Servers → Local LLM server*); it listens on port
`11434` (Ollama's port) and binds to loopback unless you set `bind =
"0.0.0.0"`. Remote callers also need an inbound API key while `auth =
true` (the default). See
[configuration.md → Serve local inference](configuration.md#serve-local-inference-over-http-openai--ollama-api).

## Where to file a bug

GitHub issues, with:

1. Output of `fono doctor` (redact API keys).
2. Output of `fono test-inject "diag"` if injection-related.
3. Last 50 lines of the daemon log.
4. Distribution + desktop environment + display server (X11/Wayland).
