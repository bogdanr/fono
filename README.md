<div align="center">

<picture>
  <source media="(prefers-color-scheme: light)" srcset="assets/logo-light.svg">
  <img src="assets/logo-dark.svg" alt="fono" width="400">
</picture>

### Dictate anywhere. Drive agents by voice.

Press a key and speak — Fono types it into whatever window has focus,<br>
answers as a voice assistant, or drives your coding agent. Local-first, one static binary.

<a href="https://github.com/bogdanr/fono/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/bogdanr/fono/ci.yml?branch=main&amp;style=flat-square&amp;label=ci&amp;labelColor=16140f" alt="CI status"></a>
<a href="https://github.com/bogdanr/fono/releases/latest"><img src="https://img.shields.io/github/v/release/bogdanr/fono?style=flat-square&amp;label=release&amp;labelColor=16140f&amp;color=d9342f" alt="Latest release"></a>
<a href="LICENSE"><img src="https://img.shields.io/badge/license-GPL--3.0-555049?style=flat-square&amp;labelColor=16140f" alt="License: GPL-3.0-only"></a>

<a href="https://fono.page">Website</a> ·
<a href="docs/README.md">Docs</a> ·
<a href="docs/install.md">Install</a> ·
<a href="docs/quickstart.md">Quickstart</a> ·
<a href="docs/providers.md">Providers</a> ·
<a href="docs/coding-agents.md">Coding agents</a> ·
<a href="ROADMAP.md">Roadmap</a> ·
<a href="CONTRIBUTING.md">Contributing</a>

<a href="assets/fono.webp"><img src="assets/fono.webp" alt="Fono demo — press a hotkey, speak, and the text lands in the focused window" width="720"></a>

</div>

## Install

```sh
curl -fsSL https://fono.page/install | sh
```

The script picks the right binary for your CPU (and the Vulkan build if you have a GPU), installs it on `$PATH`, starts the daemon, and opens the setup wizard in the same terminal. Everything runs locally unless you opt into a cloud provider.

Already installed? `fono update` keeps you current. Prefer packages? See [other ways to install](#other-ways-to-install).

## Sixty seconds

Two keys, one escape hatch. Each key auto-detects how you press it: a quick tap toggles recording, holding it turns into push-to-talk that stops on release.

| Key   | What it does |
|-------|--------------|
| `F7`  | **Dictate.** Speak — your words are typed into the focused window. Terminal, browser, editor, anything. |
| `F8`  | **Ask.** Talk to an LLM — the reply is read aloud, streamed sentence-by-sentence so audio starts before the model finishes thinking. |
| `Esc` | Cancel a recording, or interrupt an assistant reply. |

```
F7     mic ▸ speech-to-text ▸ optional LLM polish ▸ typed into the focused window (+ clipboard)
F8     mic ▸ speech-to-text ▸ LLM assistant       ▸ spoken reply, streamed into TTS
agent  voice ▸ MCP ▸ Claude Code / Cursor / Forge ▸ the agent talks back and listens for more
```

While you speak, a small overlay shows what the microphone hears — `bars`, `oscilloscope`, `fft`, or `heatmap`. Switch via the tray or `[overlay].style` in `~/.config/fono/config.toml`.

<p align="center">
  <a href="assets/styles.webp"><img src="assets/styles.webp" alt="Four overlay styles: bars, oscilloscope, FFT, heatmap" width="720"></a>
</p>

## What you get

- ⌨️ **Dictation that lands anywhere.** X11 or Wayland — your words are typed straight into whatever window has focus, with a clipboard mirror as a safety net. Per-compositor details in [docs/wayland.md](docs/wayland.md).
- 💬 **A voice assistant that talks back.** Ask on `F8` — replies stream sentence-by-sentence into TTS so you hear the answer before the model finishes thinking, and it can call tools to actually do things.
- 🤖 **Voice-driven coding agents** *(early preview)*. Claude Code, Cursor, Forge — any MCP-capable agent. One command wires it up; the agent then speaks or listens for follow-ups ([docs](docs/coding-agents.md)).
- 🔒 **Local-first, actually.** Whisper speech-to-text plus llama.cpp polish and assistant run on your machine on a shared model instance, no duplicated memory. Nothing leaves it unless you opt into a cloud provider.
- 🏎️ **Engineered for latency.** Pinned KV-cache snapshots and append-only prompts get the local assistant's first word out in ~⅓ s on a laptop CPU — 2–4× ahead of Ollama on identical weights. [How we did it](https://bogdan.nimblex.net/programming/2026/06/10/making-local-llm-fast.html).
- ⚡ **Zero-tuning model selection.** The first run probes your CPU and GPU, then picks the heaviest Whisper model that still beats real time — a decision matrix built from [900+ benchmark runs](https://fono.page/calibration), not guesses.
- ✨ **Optional polish pass.** A small LLM tidies the transcript before it's typed — punctuation, casing, filler words — locally via the bundled llama.cpp or through the cloud provider of your choice.
- 📡 **LAN-friendly.** Speaks the [Wyoming protocol](https://github.com/rhasspy/wyoming) as both client and server, so Fono can route through (or host for) Home Assistant or another Fono on your network — mDNS finds peers automatically.
- 📦 **One small static binary.** ~22 MB CPU or ~60 MB Vulkan, four glibc dependencies — no Electron, no Node, no Python, no WebKit. `fono update` probes your host and pulls the matching build automatically.

## Providers

Local by default; every stage can be swapped to a cloud provider independently. Full matrix with models and config keys: [docs/providers.md](docs/providers.md).

| Stage | Local (default) | Cloud |
|-------|-----------------|-------|
| Speech-to-text | Whisper (bundled) | Groq · OpenAI · Deepgram · Cartesia · AssemblyAI |
| Polish | llama.cpp (bundled)\* · Ollama | Cerebras · Groq · OpenAI · Anthropic · OpenRouter |
| Assistant | llama.cpp (bundled)\* · Ollama | OpenAI · Groq · Anthropic · Cerebras · OpenRouter |
| Text-to-speech | Kokoro (En) · Piper (International) | OpenAI · Groq · OpenRouter · Cartesia · Deepgram |

<sub>\* Polish and the assistant share a single llama.cpp instance — the local model is loaded once, not twice.</sub>

Switching is one command — no restart, the daemon hot-reloads:

```sh
fono use cloud groq           # one key covers STT + polish + assistant + TTS
fono use stt deepgram         # change a single stage
fono use tts cartesia
fono use local                # back to fully local

fono keys add GROQ_API_KEY    # keys live in ~/.config/fono/secrets.toml
fono keys check               # reachability probe per stored key
```

## Privacy

Local-first, by design. With the default setup, audio and text never leave your machine. Cloud providers are strictly opt-in, per stage.

## Other ways to install

- **Distro packages.** `.deb`, `.pkg.tar.zst`, and `.txz` files are built by CI and attached to each [release](https://github.com/bogdanr/fono/releases/latest). They are not regularly tested — file an issue if one misbehaves.
- **macOS (Apple Silicon, experimental).** Each release attaches a Metal-accelerated `fono-vX.Y.Z-aarch64-apple-darwin` binary. Download it, `chmod +x`, and run `fono install` — it assembles a `Fono.app`, sets up start-at-login, and walks you through the two one-time permission grants (microphone, Accessibility), which then survive updates. Ported and tested headless; not yet verified on a physical Mac with a display — details in [docs/build-macos.md](docs/build-macos.md).
- **Windows.** Planned, not shipping yet. See the [roadmap](ROADMAP.md).

## Documentation

- [Documentation index](docs/README.md) — the full map
- [Install](docs/install.md) — one-liner, manual install, server mode, updating
- [Quickstart](docs/quickstart.md) — first dictation, common follow-ups
- [Configuration](docs/configuration.md) — every key in `config.toml`
- [Provider matrix](docs/providers.md) — STT, polish, assistant, and TTS endpoints
- [Home Assistant](docs/home-assistant.md) — run the Docker container as a Wyoming STT/TTS server
- [Live dictation](docs/interactive.md) — streaming overlay, latency budget
- [Troubleshooting](docs/troubleshooting.md) — symptom-first recipes

## Status

Linux-first; used daily by the maintainer. Rough edges exist — issues and patches are welcome. See the [roadmap](ROADMAP.md) for what's next.

## Contributing

Pull requests welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for the workflow (DCO sign-off required).

## License

GPL-3.0-only. See [LICENSE](LICENSE).
