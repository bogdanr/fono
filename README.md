# Fono — Lightweight Native Voice Dictation

Fono is a GPL-3.0, single-binary voice-dictation tool written in Rust.
Press a hotkey, speak, and cleaned text is typed at your cursor. Works
on Linux (X11 + Wayland), Windows, and macOS. One statically-linked
`fono` binary replaces the heavy Tambourine (Tauri + Python) and
OpenWhispr (Electron) stacks.

> **Status:** v0.1.0-rc. Pipeline (audio → STT → LLM → inject) is fully
> wired; default release ships local whisper.cpp out of the box. First
> run probes hardware and recommends local vs cloud automatically.
> Follow `docs/status.md` for the live milestone log.

## Install

### Linux (musl static binary)

```sh
curl -fLO https://github.com/bogdanr/fono/releases/latest/download/fono-v0.1.0-x86_64-unknown-linux-musl.tar.gz
tar -xzf fono-v0.1.0-x86_64-unknown-linux-musl.tar.gz
sudo install -m755 fono-v0.1.0-x86_64-unknown-linux-musl/fono /usr/local/bin/fono
fono   # first run starts the setup wizard
```

### NimbleX / Slackware

```sh
cp -r packaging/slackbuild/fono /tmp/fono
cd /tmp/fono && ./fono.SlackBuild
installpkg /tmp/fono-0.1.0-x86_64-1_NimbleX.txz
```

Set `FROM_SOURCE=1` to build from source instead of downloading the
pre-built binary. See [`packaging/slackbuild/fono/README`](packaging/slackbuild/fono/README).

### Arch Linux (AUR)

```sh
yay -S fono                  # or: paru -S fono
```

PKGBUILD lives at [`packaging/aur/PKGBUILD`](packaging/aur/PKGBUILD).

### Nix flake

```sh
nix run github:NimbleX/fono
nix profile install github:NimbleX/fono
```

### Debian / Ubuntu

```sh
cd packaging/debian && dpkg-buildpackage -us -uc
sudo dpkg -i ../fono_0.1.0-1_amd64.deb
```

### From source (any OS)

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --profile release-slim --target x86_64-unknown-linux-musl -p fono
./target/x86_64-unknown-linux-musl/release-slim/fono
```

## Quick start

```sh
fono setup          # pick local vs cloud, paste API keys, download models
fono                # start daemon (tray + hotkeys)
# or, as a systemd user service:
systemctl --user enable --now fono.service
```

Default hotkeys:

| Binding              | Action                                    |
|----------------------|-------------------------------------------|
| `Ctrl+Alt+Space`     | Toggle recording                          |
| `Ctrl+Alt+Grave`     | Push-to-talk (hold)                       |
| `Ctrl+Alt+Period`    | Re-type last transcription                |
| `Escape`             | Cancel current recording                  |

Change them in `~/.config/fono/config.toml` (`[hotkeys]` section).

## Providers

Fono supports both local and cloud STT + LLM backends. See
[`docs/providers.md`](docs/providers.md) for the full matrix. Defaults:

* **Local (recommended on capable hardware):** whisper `small` (~466 MB).
  Private, offline, ~1 s latency on 8-core x86_64. The default release
  binary bundles whisper.cpp so this works out of the box — no rebuild,
  no extra system packages. Local LLM cleanup (Qwen / SmolLM) ships in
  v0.2; for v0.1 the local path is whisper-only with optional cloud LLM
  cleanup. Run `fono hwprobe` to see what your machine can sustain.
* **Cloud:** Groq whisper-large-v3-turbo + Cerebras llama-3.3-70b. Sub-1
  s end-to-end, generous free tiers.

API keys go in `~/.config/fono/secrets.toml` (mode 0600) or via
`$ENV_VAR` references in config.

### Switching providers

Hot-swap STT, LLM, or both — no daemon restart. Multiple keys coexist:

```sh
fono use stt groq             # flip STT only
fono use llm cerebras         # flip LLM only
fono use cloud cerebras       # paired preset (STT=Groq + LLM=Cerebras)
fono use local                # back to whisper-local + skip LLM
fono use show                 # print active selection

fono keys add GROQ_API_KEY    # prompts via password input
fono keys list                # masked listing
fono keys check               # reachability probe per stored key

fono record --stt openai --llm anthropic   # one-shot per-call override
```

### Build flavours

| `cargo build …`                                                | Includes                        | First-run download |
|----------------------------------------------------------------|---------------------------------|--------------------|
| (default)                                                      | local STT + tray + cloud Groq   | ~466 MB (whisper)  |
| `--no-default-features --features tray`                        | cloud-only (no whisper.cpp C++) | none               |
| `--no-default-features --features tray,cloud-all`              | cloud-only, all providers       | none               |
| `--features llama-local`                                       | + local LLM (v0.2 — preview)    | + ~1 GB (Qwen)     |

## CLI

```
fono                          # alias for: fono daemon
fono daemon [--no-tray]
fono toggle                   # IPC: toggle recording on running daemon
fono paste-last               # IPC: re-type last cleaned transcription
fono setup                    # re-run the wizard
fono doctor                   # diagnostic report (HW tier + providers + injector)
fono hwprobe [--json]         # probe CPU/RAM/disk, print recommended local tier
fono use {stt,llm,cloud,local,show}        # hot-swap providers (no restart)
fono keys {list,add,remove,check}          # multi-provider API key vault
fono record [--stt X] [--llm Y] [--no-inject]
fono transcribe <wav> [--stt X] [--llm Y]
fono test-inject "<text>" [--shortcut shift-insert|ctrl-v|ctrl-shift-v]
fono config {path,show,edit}
fono history {list,search,clear} [--json] [--limit N]
fono models  {list,install,remove,verify} [name]
fono completions {bash,zsh,fish,powershell,elvish}
fono --debug | -v | -vv       # log verbosity
fono --quiet | -q             # warn-only
```

## Tray menu

Right-click the tray icon for live provider switching and one-click history paste:

```
Fono — idle
─────────────────
Toggle recording  (Ctrl+Alt+Space)
Pause hotkeys
─────────────────
Recent transcriptions  ▸    1. The meeting is at three…
                            2. (older items, click to re-paste)
─────────────────
STT: groq           ▸    Local · Groq* · OpenAI · Deepgram · …
LLM: cerebras       ▸    None · Cerebras* · Groq · OpenAI · Anthropic · …
─────────────────
Open history folder…
Edit config
─────────────────
Quit
```

`*` marks the active backend; clicking another item rewrites the config and hot-reloads
the orchestrator without restarting the daemon.

## Text injection

Fono pastes via **Shift+Insert** by default — the universal X11 paste binding that
works in every terminal (xterm/urxvt/alacritty/kitty/foot/konsole/gnome-terminal/…),
every browser, and every GTK / Qt / Electron text field. Each successful dictation
also writes the cleaned text to **both** the X CLIPBOARD *and* PRIMARY selections so
clipboard managers (clipit, parcellite, Klipper) can pick it up regardless of which
selection they watch.

If a specific app rejects Shift+Insert (rare):

```toml
# ~/.config/fono/config.toml
[inject]
paste_shortcut = "ctrl-v"            # or "ctrl-shift-v"
```

…or one-shot via env: `FONO_PASTE_SHORTCUT=ctrl-v fono record`.

Smoke-test injection without speaking a word:

```sh
fono test-inject "ana are mere"      # focus a text field within 5 s
```

## Wayland

See [`docs/wayland.md`](docs/wayland.md). Short version: sway /
hyprland / river work out of the box; GNOME / KDE need a compositor-
level shortcut bound to `fono toggle` because no compositor implements
the `GlobalShortcuts` XDG portal yet.

## On-disk layout

| Kind                 | Path                                        |
|----------------------|---------------------------------------------|
| Config               | `~/.config/fono/config.toml`                |
| Secrets (0600)       | `~/.config/fono/secrets.toml`               |
| Whisper models       | `~/.cache/fono/models/whisper/ggml-*.bin`   |
| LLM models           | `~/.cache/fono/models/llm/`                 |
| History DB           | `~/.local/share/fono/history.sqlite`        |
| IPC socket, PID      | `~/.local/state/fono/fono.sock`, `fono.pid` |

All XDG `_HOME` overrides are honoured.

## Workspace layout

```
crates/
├── fono            # bin: entry point, CLI, first-run wizard, daemon
├── fono-core       # config, secrets, XDG paths, SQLite history
├── fono-audio      # cpal capture, VAD, resampling
├── fono-stt        # SpeechToText trait + local + cloud backends
├── fono-llm        # TextCleanup   trait + local + cloud backends
├── fono-hotkey     # accelerator parser + hold/toggle FSM + listener
├── fono-inject     # enigo typing + Wayland fallback (wtype/ydotool)
├── fono-tray       # tray-icon wrapper, menu
├── fono-overlay    # winit/softbuffer recording indicator (deferred)
├── fono-ipc        # Unix-socket IPC between daemon and CLI
└── fono-download   # streaming HTTPS downloader with SHA-256 verify
```

## Privacy

Local by default. Nothing leaves your machine unless you pick a cloud
provider. No telemetry ever. See [`docs/privacy.md`](docs/privacy.md).

## Contributing

Please read [`CONTRIBUTING.md`](CONTRIBUTING.md). All commits **must**
carry a `Signed-off-by:` trailer (`git commit -s`) per the Developer
Certificate of Origin; CI rejects PRs that are missing it. Every Rust
source file **must** start with `// SPDX-License-Identifier: GPL-3.0-only`.

## License

Fono is distributed under the **GNU General Public License, version 3
only**. See [LICENSE](LICENSE) for the full text.
