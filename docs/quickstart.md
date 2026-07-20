# Quickstart

This page walks through your first dictation, your first assistant turn,
and the most common follow-up commands. It assumes Fono is already
installed and the daemon is running. If not, see
[install.md](install.md) first.

## Your first dictation

1. Open any text field — a terminal, an editor, a chat window, anything
   that takes keyboard input.
2. Press **F7**, speak a sentence, and either:
   - tap **F7** again to stop (toggle mode), or
   - if you held the key for more than ~1 s, release it (push-to-talk).
3. The transcript appears at your cursor a moment later.

There's no separate "record" button. The press duration decides between
toggle and PTT, so you can use either one without changing config.

A small overlay paints near the bottom of the screen while you speak,
showing what the microphone is hearing (bars, oscilloscope, FFT, or
heatmap — switch via the tray *Preferences → Waveform style*). The
overlay disappears as soon as capture ends.

![The four waveform overlay styles: bars, oscilloscope, FFT, and heatmap](../assets/styles.webp)

## Your first assistant turn

1. Press and hold **F8**, ask a short question, release.
2. Fono streams the reply sentence-by-sentence into TTS so audio starts
   playing before the model is done thinking.
3. Press **Escape** at any time to shut up the reply.

The assistant runs on the bundled local model out of the box — the
setup wizard's local path enables it with no key and no cloud account.
If F8 does nothing, the assistant is probably disabled or has no
backend selected. Run `fono use assistant <backend>` (any of `local`,
`openai`, `anthropic`, `groq`, `cerebras`, `gemini`, `openrouter`);
cloud backends also need a key:

```sh
fono keys add OPENAI_API_KEY      # paste your key at the prompt
```

See [providers.md](providers.md) for the full setup and per-provider
capability matrix.

## Cancelling

**Escape** cancels a recording in flight (no transcript injected) or
shuts up an assistant reply. Toggle dictations can also be cancelled by
pressing F7 a second time before it stops; the daemon discards the
buffer instead of running STT.

## Switching to cloud STT

The local Whisper default works without an internet connection and
without any keys. To switch to a cloud provider:

```sh
fono use cloud groq             # paired preset (Groq STT + Groq LLM)
fono use cloud cerebras         # paired preset (Groq STT + Cerebras LLM)
fono use stt openai             # change STT only
fono use polish anthropic       # change polish only
fono use show                   # print the active selection + key refs
fono use local                  # back to whisper-local + skip polish
```

Each `fono use` writes the change atomically and hot-reloads the
daemon — no restart, no lost state. API keys live in
`~/.config/fono/secrets.toml` (mode 0600); add them with
`fono keys add <NAME>`.

## Useful one-shot commands

```sh
fono record                    # one capture from the mic, transcribe, inject
fono record --no-inject        # ... but print to stdout instead of typing
fono record --live             # use the streaming pipeline once (see interactive.md)
fono transcribe sample.wav     # transcribe a WAV without touching the mic
fono history                   # browse recent dictations
fono history --last            # full STT/LLM detail for the latest entry
fono test-inject "hello"       # verify the inject + clipboard pipeline
fono doctor                    # diagnostic report
fono hwprobe                   # recommend a local-model tier
```

`fono --help` lists everything; `fono <subcommand> --help` documents
flags.

## What didn't work

- **Hotkey didn't fire** — see
  [troubleshooting.md → Hotkey doesn't fire](troubleshooting.md#hotkey-doesnt-fire).
  On Wayland, Fono auto-registers via the xdg-desktop-portal
  GlobalShortcuts interface; on GNOME 46 it falls back to gsettings
  custom-keybindings. See [wayland.md](wayland.md) for the per-compositor
  story.
- **Nothing pasted** — the clipboard safety net always populates the
  clipboard even if key injection fails. Press your normal paste shortcut
  to recover; if even that's empty, run
  `fono test-inject "diag" --no-inject` for a per-tool diagnostic. See
  [inject.md](inject.md).
- **Wrong language detected** — see
  [troubleshooting.md → Cloud STT keeps detecting the wrong language](troubleshooting.md#cloud-stt-keeps-detecting-the-wrong-language).
- **STT or LLM failed** — see
  [troubleshooting.md → STT failed](troubleshooting.md#stt-failed) and
  `fono keys check` for reachability.

## Where data lives

| Kind | Path |
|---|---|
| Config | `~/.config/fono/config.toml` |
| Secrets (mode 0600) | `~/.config/fono/secrets.toml` |
| Whisper models | `~/.cache/fono/models/whisper/` |
| Polish models | `~/.cache/fono/models/polish/` |
| History DB | `~/.local/share/fono/history.sqlite` |
| IPC socket + PID | `~/.local/state/fono/` |

All paths honour `XDG_*_HOME` overrides. Nothing leaves the machine
unless you've chosen a cloud provider; see [privacy.md](privacy.md).

## Where next

- [configuration.md](configuration.md) — every key in `config.toml`,
  hotkey rebinding, history retention.
- [providers.md](providers.md) — STT / polish / assistant / TTS
  matrices.
- [interactive.md](interactive.md) — live (streaming) dictation.
- [troubleshooting.md](troubleshooting.md) — symptom-first recipes.
