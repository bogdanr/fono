# Configuration

Fono is configured through two TOML files in `~/.config/fono/`:

| File | Mode | Purpose |
|---|---|---|
| `config.toml` | `0644` | All non-secret settings |
| `secrets.toml` | `0600` | API keys (refuses to load if world-readable) |

Both files are written atomically, can be edited with any text editor,
and are reloaded on every `fono use` command (no daemon restart). Run
`fono setup` to recreate them from the wizard if you ever want a clean
slate.

## Hot-reload

The daemon listens on its IPC socket for a `Reload` message. Anything
that mutates the file (`fono use`, the tray menu, `fono setup`, or a
manual edit followed by `fono toggle` from any terminal) re-reads the
config and applies the change atomically. The orchestrator's
single-in-flight cap means an active pipeline finishes first, then the
new config takes effect on the next press.

## Section overview

The full schema lives in `crates/fono-core/src/config.rs` with
field-level rustdoc comments. The user-facing sections are:

| Section | Purpose | See |
|---|---|---|
| `[general]` | Languages, autostart, system-mute, clipboard safety net | this file |
| `[hotkeys]` | Dictation, cancel, assistant key bindings | below |
| `[audio]` | Sample rate, VAD, silence trimming, auto-stop | this file |
| `[stt]` | Speech-to-text backend selection + per-backend config | [providers.md](providers.md) |
| `[polish]` | Cleanup-pass backend selection + behaviour | [providers.md](providers.md) |
| `[assistant]` | Voice-assistant chat backend + capability flags | [providers.md](providers.md) |
| `[tts]` | Text-to-speech for the assistant | [providers.md](providers.md) |
| `[interactive]` | Streaming-pipeline tuning (live mode) | [interactive.md](interactive.md) |
| `[inject]` | Injection backend override and clipboard safety net | [inject.md](inject.md) |
| `[overlay]` | Waveform style; picking `transcript` enables live mode | [interactive.md](interactive.md) |
| `[history]` | History DB retention, FTS5 settings | this file |
| `[update]` | Auto-check toggle, release channel | [install.md](install.md) |
| `[server]` | Wyoming-protocol STT server (LAN host mode) | [install.md](install.md) |
| `[network]` | mDNS metadata overrides | — |
| `[mcp]` | MCP server limits + voice-tool relevance filter | this file |
| `[[context_rules]]` | Per-app prompt/behaviour overrides | this file |

## Common knobs by example

### Languages — bilingual setup (English + Romanian)

```toml
[general]
languages = ["en", "ro"]            # empty = unconstrained auto-detect
```

Codes are BCP-47 (alpha-2 forms most-commonly used). Empty list = full
Whisper auto-detect; one entry = constrained auto-detect (not a hard
force — see ADR 0016); two or more entries ban every language outside
the set. Order doesn't matter; the in-memory cache reflects what was
actually heard. See the *Multilingual STT and language stickiness*
section of [providers.md](providers.md).

### Auto-stop after N seconds of silence

```toml
[audio]
auto_stop_silence_ms = 3000         # 0 disables; tray presets: 0 / 3000 / 5000
```

Only fires in toggle mode. Hold-to-talk and assistant-hold always honour
the explicit release.

### Force a specific input device

```toml
[audio]
input_device = "alsa_input.pci-0000_00_1f.3.analog-stereo"
```

`fono doctor` lists the detected default and any candidates; `arecord
-l` (ALSA) or `pw-cli list-objects | grep node.name` (PipeWire) gives
the system view.

### Switch STT or polish

Edit by hand if you want, but `fono use` is shorter:

```sh
fono use stt groq           # writes [stt].backend = "groq"
fono use polish anthropic   # writes [polish].backend = "anthropic"
fono use polish none        # disable polish entirely
fono use show               # show the active selection
```

The minimal viable cloud block is two lines plus one key:

```toml
[stt]
backend = "groq"
[polish]
backend = "cerebras"
enabled = true
```

…with `GROQ_API_KEY` and `CEREBRAS_API_KEY` either in `secrets.toml` or
exported in the environment. The factories fall through to the canonical
env-var name when the optional `[stt.cloud]` / `[polish.cloud]`
sub-blocks are absent.

### Skip polish for short utterances

```toml
[polish]
skip_if_words_lt = 3        # default; one- and two-word captures bypass cleanup
```

Useful when the polish step is slower than the dictation it cleans
(typical for one-word commands and chat-bar dictation).

### Live mode (streaming preview)

Live mode is gated by the overlay style, not by a separate flag. Pick
`transcript` in the tray (*Preferences → Waveform style*) or by hand:

```toml
[overlay]
style = "transcript"        # bars | oscilloscope | fft | heatmap | transcript
```

`[interactive]` tunes the streaming pipeline once it's on; most users
never touch it. See [interactive.md](interactive.md).

### Per-app context rules

```toml
[[context_rules]]
match_class = "Slack"               # WM_CLASS / wl_compositor app-id
prompt_append = "Format as a chat message; keep it casual."

[[context_rules]]
match_class = "code"                # VSCode
polish = "anthropic"                # override polish backend just for this app
```

Rules are evaluated in order; the first match wins. The `match_class`
field matches the focused window's class id (`xprop WM_CLASS` on X11,
`hyprctl activewindow` on Hyprland, etc.).

### History retention

```toml
[history]
max_entries = 10_000        # rolling cap; older entries are pruned
```

`fono history clear` truncates the table without touching the file;
deleting `~/.local/share/fono/history.sqlite` wipes everything.

### MCP voice-tool relevance filter

When a coding agent calls `fono.listen` (directly or via
`fono.confirm`), the captured utterance is scored to filter out
background speech (radio, TV, side conversation, prompt-TTS echo):

```toml
[mcp]
# "off"       — disable the filter, every transcript is returned.
# "heuristic" — length / filler / echo rules only (cheap, default).
# "llm"       — heuristic first, then the configured polish backend
#               as a one-shot classifier (1.5 s hardcoded timeout;
#               fails open on timeout / parse failure).
relevance_filter = "heuristic"

# Maximum number of background utterances the loop will drop before
# returning the most recent one regardless. Prevents an infinite
# wait in pathological environments.
relevance_max_rejections = 2

# System-prompt override for `fono.summarize` (MCP tool) and
# `fono summarize` (CLI). Empty/omitted — use the built-in
# prompt: 1-2 spoken sentences saying who wants what; never read raw
# logs or long content aloud; mention attachments briefly by kind.
# Requires a configured `[assistant]` backend.
# A failed summarize request is retried once on the configured
# backend, then tried once on the first other backend with a usable
# API key (canonical env vars: CEREBRAS_API_KEY, GROQ_API_KEY, …) or
# local model. Cloud requests time out fast (10 s to first byte);
# the local backend keeps a long budget (60 s) for model load.
# summarize_prompt = ""
```

The 1.5 s LLM-classifier timeout is hardcoded in
`crates/fono-mcp-server/src/relevance.rs` and not user-configurable;
it's a per-iteration ceiling, not a budget. On timeout the filter
**fails open** (accepts the utterance) so a sluggish polish backend
can never strand a real answer.

Tray feedback during MCP voice interactions is **automatic** — no
config knob. The daemon's tray icon turns amber (the same colour as
the existing `Processing` state used for STT / polish) for the
duration of any `fono.listen`, `fono.speak`, or `fono.confirm` call,
then restores whatever it was showing before. See
[coding-agents.md](coding-agents.md#what-you-see-and-hear-during-an-mcp-voice-turn).

## Hotkeys

```toml
[hotkeys]
dictation = "F7"                    # short tap = toggle; long hold = PTT
cancel    = "Escape"
assistant = "F8"                    # empty disables the assistant hotkey
```

Accelerator syntax: `Mod+Key`. Modifiers are `Ctrl` / `Alt` / `Shift` /
`Super` (or `Meta`); keys are letter / digit / function-key / named
key names (`Space`, `Tab`, `Return`, `Pause`, `ScrollLock`, `Insert`,
`Delete`). Examples: `Ctrl+Alt+Space`, `Super+grave`, `Mod4+space`,
`F11`, `Pause`. The dictation key is a soft modal: a short tap (under
~1 s) toggles capture, a longer hold runs push-to-talk. The cancel key
stops a recording in flight or shuts up an in-progress assistant reply.
Leave `assistant` empty to disable the F8 hotkey.

## Inject and clipboard

Text injection has no per-key knobs in `config.toml` — the backend is
auto-detected at startup and can be overridden per-session with the
`FONO_INJECT_BACKEND` environment variable (`enigo`, `wtype`, `ydotool`,
`xdotool`, `xtest`, `none`). See [inject.md](inject.md) for the
priority table.

The `[general].also_copy_to_clipboard` flag (default `true`) is a
belt-and-suspenders that keeps the clipboard populated even on
compositors where key injection silently fails (KDE Wayland with
`wtype`). Disable only if you have a specific reason.

## Secrets

`~/.config/fono/secrets.toml` is a flat key-value file:

```toml
GROQ_API_KEY      = "gsk_..."
OPENAI_API_KEY    = "sk-..."
ANTHROPIC_API_KEY = "sk-ant-..."
```

Manage it via the CLI instead of editing by hand:

```sh
fono keys add GROQ_API_KEY          # paste at the prompt
fono keys list                      # masked listing
fono keys check                     # reachability probe per key
fono keys remove OPENAI_API_KEY
```

The file refuses to load if it's world- or group-readable. Fono never
logs key values; the `fono.http` tracing target records masked
request-id metadata only.

You can also reference an environment variable instead of pasting the
key, by setting `api_key_ref` in `[stt.cloud]` / `[polish.cloud]` /
`[assistant.cloud]` / `[tts.cloud]` to an env-var name (e.g.
`"GROQ_API_KEY"`); the daemon reads `$GROQ_API_KEY` at request time and
nothing touches disk. Useful for systemd `EnvironmentFile=` setups.

## On-disk paths (XDG)

| Kind | Path |
|---|---|
| Config | `$XDG_CONFIG_HOME/fono/config.toml` (default `~/.config/fono/config.toml`) |
| Secrets | `$XDG_CONFIG_HOME/fono/secrets.toml` |
| Whisper models | `$XDG_CACHE_HOME/fono/models/whisper/` |
| Polish models | `$XDG_CACHE_HOME/fono/models/polish/` |
| History DB | `$XDG_DATA_HOME/fono/history.sqlite` |
| IPC socket + PID | `$XDG_STATE_HOME/fono/` |

Server mode uses `/etc/fono/`, `/var/lib/fono/`, `/var/cache/fono/`,
and `/run/fono/` instead. See [install.md](install.md).

## Versioning and migration

The top-level `version = N` field is bumped whenever the schema gains a
breaking change. The daemon refuses to load a config from a future
version; it loads older configs and migrates fields on the fly. Removed
fields (the 2026-05-22 simplification, for example) are silently
ignored — your old config keeps working, the dropped keys just no
longer do anything.
