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
fono-audio       cpal capture → ring buffer, resampler, VAD stub, auto-mute
fono-hotkey      accelerator parser, FSM, global-hotkey listener thread
fono-stt         async SpeechToText trait + registry + backends
fono-llm         async TextCleanup   trait + registry + backends
fono-inject      enigo typing + clipboard-paste fallback, focus detection
fono-tray        tray-icon lifecycle + menu
fono-overlay     winit/softbuffer recording indicator (deferred)
fono-ipc         Unix-socket single-instance protocol (length-prefixed JSON)
fono-download    streaming HTTPS with SHA-256 verify + range resume
```

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
   → (if cfg.llm.enabled): raw text → fono-llm backend → cleaned text
   → fono-inject: type cleaned text at focused window
   → fono-core::history: persist (raw, cleaned, app_class, ...)
```

## State machine

`fono_hotkey::fsm::State` — `Idle`, `Recording(Hold | Toggle)`, `Processing`.
Hold mode transitions on Pressed/Released; toggle transitions on each
press. `Processing` only returns to `Idle` on `ProcessingDone`, which the
daemon emits when the pipeline finishes (or, today, via a 150 ms shim
until the STT wiring lands).

## On-disk layout

| Kind                 | Path                                                       |
|----------------------|------------------------------------------------------------|
| Config               | `~/.config/fono/config.toml`                               |
| Secrets (mode 0600)  | `~/.config/fono/secrets.toml`                              |
| Whisper models       | `~/.cache/fono/models/whisper/ggml-<name>.bin`             |
| LLM models (GGUF)    | `~/.cache/fono/models/llm/<name>.gguf`                     |
| History DB           | `~/.local/share/fono/history.sqlite`                       |
| IPC socket + PID     | `~/.local/state/fono/fono.sock`, `fono.pid`                |

All paths honour `XDG_*_HOME` overrides.

## Deferred (pre-v0.1)

* Real audio → STT → LLM → inject pipeline (Phases 4–6 integration).
* `whisper-rs` + `llama-cpp-2` local engines (stubs in place, feature-gated).
* Silero-VAD ONNX end-of-speech detection.
* `winit` overlay window.
