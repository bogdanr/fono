<div align="center">

# Fono

**Press a hotkey, speak, see your words on screen.**

A lightweight, native voice-dictation tool for Linux. Windows and macOS are on the roadmap.
One static Rust binary — no Electron, no Python, no WebKit.

[![CI](https://github.com/bogdanr/fono/actions/workflows/ci.yml/badge.svg)](https://github.com/bogdanr/fono/actions/workflows/ci.yml)
[![License: GPL-3.0-only](https://img.shields.io/badge/License-GPL--3.0--only-blue.svg)](LICENSE)
[![Latest release](https://img.shields.io/github/v/release/bogdanr/fono)](https://github.com/bogdanr/fono/releases/latest)

</div>

---

* **Local by default.** Whisper runs on your machine; nothing leaves it.
* **Or bring a key.** Groq, OpenAI, Cerebras, Anthropic, Deepgram — switch with one command, no restart.
* **Lands in any window.** Terminal, browser, IDE, chat — Shift+Insert paste works everywhere on X11.

## Install

| Distro                  | Command                                                                                              |
|-------------------------|------------------------------------------------------------------------------------------------------|
| **Arch / Manjaro**      | `sudo pacman -U fono-0.2.0-1-x86_64.pkg.tar.zst` *(from [Releases](https://github.com/bogdanr/fono/releases/latest))* |
| **Debian / Ubuntu**     | `sudo apt install ./fono_0.2.0_amd64.deb` *(from [Releases](https://github.com/bogdanr/fono/releases/latest))* |
| **Slackware / NimbleX** | `installpkg fono-0.2.0-x86_64-1.txz` *(from [Releases](https://github.com/bogdanr/fono/releases/latest))* |
| **NixOS / Nix flake**   | `nix profile install github:bogdanr/fono`                                                            |
| **Any Linux (one-liner)** | `curl -fsSL https://github.com/bogdanr/fono/releases/latest/download/fono-v0.2.0-x86_64 \| sudo install -m755 /dev/stdin /usr/local/bin/fono` |
| **macOS / Windows**     | Planned after the Linux-first releases |

## First run

```sh
fono setup    # picks local vs cloud based on your hardware, installs models
fono          # starts the daemon (tray + hotkeys)
```

Default hotkeys: **`F9`** to toggle recording, **`F8`** to push-to-talk (hold).
Speak. Text appears at your cursor.

## Switching providers

Hot-swap STT, LLM, or both — no daemon restart:

```sh
fono use cloud groq           # paired preset (Groq STT + Groq LLM)
fono use stt openai           # change just STT
fono use local                # back to whisper-local + skip LLM
```

API keys live in `~/.config/fono/secrets.toml`:

```sh
fono keys add GROQ_API_KEY    # paste at the prompt
fono keys check               # reachability probe per stored key
```

## Privacy

Local-first. Nothing leaves your machine unless you pick a cloud provider.
No telemetry, ever. See [`docs/privacy.md`](docs/privacy.md).

## Documentation

* [Provider matrix](docs/providers.md) — STT + LLM endpoints, env vars, default models.
* [Text injection guide](docs/inject.md) — Shift+Insert, override per-app.
* [Wayland notes](docs/wayland.md) — KDE/GNOME compositor binding.
* [Troubleshooting](docs/troubleshooting.md) — symptom-first recipes.

## Contributing

Pull requests welcome. See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the workflow.

## License

GPL-3.0-only. See [LICENSE](LICENSE).
