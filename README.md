# Fono — Lightweight Native Voice Dictation

Fono is a GPL-3.0, single-binary voice-dictation tool written in Rust.
Press a hotkey, speak, and cleaned text is typed at your cursor. Works
on Linux (X11 + Wayland), Windows, and macOS. One statically-linked
`fono` binary replaces the heavy Tambourine (Tauri + Python) and
OpenWhispr (Electron) stacks.

> **Status:** v0.1 scaffold. All ten crates + CLI + tray + hotkeys +
> model auto-download are working. Real audio → STT → LLM → inject
> pipeline wiring is the next milestone — follow `docs/status.md`.

## Install

### Linux (musl static binary)

```sh
curl -fLO https://github.com/NimbleX/fono/releases/latest/download/fono-v0.1.0-x86_64-unknown-linux-musl.tar.gz
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

* **Local (recommended):** whisper `small` (~466 MB) + Qwen2.5-1.5B
  (~1.0 GB). Private, offline, ~2 s latency on a 4-core x86_64.
* **Cloud:** Groq whisper-large-v3 + Cerebras llama-3.3-70b. Sub-1 s
  latency, generous free tiers.

API keys go in `~/.config/fono/secrets.toml` (mode 0600) or via
`$ENV_VAR` references in config.

## CLI

```
fono                          # alias for: fono daemon
fono daemon [--no-tray]
fono toggle                   # IPC: toggle recording on running daemon
fono paste-last               # IPC: re-type last cleaned transcription
fono setup                    # re-run the wizard
fono doctor                   # diagnostic report
fono config {path,show,edit}
fono history {list,search,clear} [--json] [--limit N]
fono models  {list,install,remove,verify} [name]
fono completions {bash,zsh,fish,powershell,elvish}
fono --debug | -v | -vv       # log verbosity
fono --quiet | -q             # warn-only
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
