# Fono architecture

## Module layout

```
fono (bin)
  ├─ cli          clap dispatcher + subcommands
  ├─ wizard       first-run interactive setup
  ├─ daemon       orchestrator: tray + hotkeys + IPC + pipeline
  ├─ doctor       diagnostic report
  └─ models       ensure configured models exist on disk

fono-core        paths (XDG), Config/Secrets (atomic TOML), SQLite+FTS5 history
fono-audio       cpal capture → broadcast frame stream, VAD, envelope follower,
                 silence watch, trim, playback worker
fono-hotkey      accelerator parser, FSM, global-hotkey listener thread,
                 xdg-desktop-portal GlobalShortcuts + gsettings fallbacks
fono-stt         async SpeechToText + StreamingStt traits + registry + backends
fono-polish      async TextCleanup trait + registry + backends
fono-tts         async TextToSpeech trait + Wyoming + OpenAI-compat + native
                 (Cartesia, Deepgram) backends
fono-assistant   streaming chat (Anthropic + OpenAI-compat + native) +
                 rolling history, sentence splitter for TTS hand-off
fono-inject      wtype / ydotool / xdotool / xtest-paste + clipboard-paste
                 fallback, focus detection
fono-tray        tray-icon lifecycle + menu, StatusNotifierItem
fono-overlay     pluggable overlay backends — `wlr-layer-shell` (sway,
                 Hyprland, KDE Plasma, COSMIC, niri, …), X11 override-redirect
                 (winit + softbuffer; native on Xorg, also covers GNOME via
                 Xwayland), and a `noop` fallback. Software renderer paints
                 bars / FFT / oscilloscope / heatmap / VU / transcript styles.
                 See `docs/wayland.md`.
fono-ipc         Unix-socket single-instance protocol (length-prefixed JSON)
fono-download    streaming HTTPS with SHA-256 verify + range resume
fono-http        shared HTTP instrumentation, body watchdog, upstream
                 request-id helpers for every cloud backend
fono-net         Wyoming-protocol server + mDNS LAN peer discovery
fono-net-codec   length-prefixed JSON framing shared by Wyoming and the
                 fono-internal IPC protocols
fono-update      self-update: GitHub Releases poll, archive download,
                 SHA-256 verify, in-place atomic replace + re-exec
```

The dictation pipeline goes **STT → polish → text injection**.
The voice-assistant pipeline (F8 hold-to-talk) diverges after STT:
**STT → assistant chat → SentenceSplitter → TTS → AudioPlayback**,
with no text injection. `fono::assistant` orchestrates the pump and
hosts the rolling conversation history; `fono-audio::playback` is
the cpal/paplay output worker the assistant flow uses.

## Runtime model

Single tokio runtime. Three ingress points feed a shared `HotkeyAction`
channel consumed by the daemon orchestrator:

1. **Global hotkey thread** (owned by `fono-hotkey::listener`): dedicated
   OS thread runs the `global-hotkey` event loop, forwards Pressed/
   Released into the FSM, FSM emits actions onto an `mpsc<HotkeyAction>`.
2. **Tray menu** (`fono-tray`): menu item activation pushes the same
   action variants.
3. **IPC socket** (`fono-ipc`): `fono toggle` etc. send JSON requests
   over `~/.local/state/fono/fono.sock`; the handler routes requests
   into the same channel.

Dispatching an action goes:

```
HotkeyAction::StartRecording
   → fono-audio opens a cpal stream into a ring buffer
   → on StopRecording: buffer handed to fono-stt backend
   → (if cfg.polish.enabled): raw text → fono-polish backend → cleaned text
   → fono-inject: type cleaned text at focused window
   → fono-core::history: persist (raw, cleaned, app_class, ...)
```

## State machine

`fono_hotkey::fsm::State` — `Idle`, `Recording(RecordingMode)`,
`LiveDictating(RecordingMode)`, `Processing`, plus the assistant trio
`AssistantRecording` / `AssistantThinking` / `AssistantSpeaking`.
`RecordingMode` is `Hold` or `Toggle`; hold mode transitions on
Pressed/Released, toggle transitions on each press. `Processing` returns
to `Idle` on `ProcessingDone` from the orchestrator when STT + optional
polish + inject completes. The orchestrator dispatches `LiveHold*` /
`LiveToggle*` action variants instead of the plain `Hold*` / `Toggle*`
ones when `[interactive].enabled = true`, routing capture through
`crates/fono/src/live.rs::LiveSession`.

## On-disk layout

| Kind                 | Path                                                       |
|----------------------|------------------------------------------------------------|
| Config               | `~/.config/fono/config.toml`                               |
| Secrets (mode 0600)  | `~/.config/fono/secrets.toml`                              |
| Whisper models       | `~/.cache/fono/models/whisper/ggml-<name>.bin`             |
| Polish models (GGUF)    | `~/.cache/fono/models/polish/<name>.gguf`                     |
| History DB           | `~/.local/share/fono/history.sqlite`                       |
| IPC socket + PID     | `~/.local/state/fono/fono.sock`, `fono.pid`                |

All paths honour `XDG_*_HOME` overrides.
