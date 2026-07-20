<div align="center">

<picture>
  <source media="(prefers-color-scheme: light)" srcset="assets/logo-light.svg">
  <img src="assets/logo-dark.svg" alt="fono" width="400">
</picture>

### Talk to your computer.

Press a key and speak, and Fono types into any app, answers as a voice assistant,<br>
drives your coding agent or smart home. It's a complete voice-AI stack<br>
(speech-to-text, natural voices, a local LLM, wake word, speaker ID)<br>
in one small binary. Everything runs locally, and every stage can switch<br>
to a cloud provider if that fits you better.

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

No setup marathon, no account, no stack of services to babysit. One small file: install it, press a key, talk. And it's not just for you; anything on your network can use it too: Home Assistant, Open WebUI, your editor. (If you're an engineer: think *SQLite of voice AI*. The entire stack, self-contained, in one binary.)

## Install

```sh
curl -fsSL https://fono.page/install | sh
```

The script picks the right binary for your CPU (and the Vulkan build if you have a GPU), starts the daemon, and opens the setup wizard in the same terminal. Everything runs locally unless you opt into a cloud provider.

Already installed? `fono update` keeps you current. Prefer packages? See [other ways to install](#other-ways-to-install).

## Sixty seconds

Two keys, one escape hatch. Each key auto-detects how you press it: a quick tap toggles recording, holding it turns into push-to-talk that stops on release.

| Key   | What it does |
|-------|--------------|
| `F7`  | **Dictate.** Your words are typed into the focused window. Terminal, browser, editor, anything. |
| `F8`  | **Ask.** Talk to an AI and get the reply read aloud, streamed sentence-by-sentence so audio starts before the model finishes thinking. |
| `Esc` | Cancel a recording, or interrupt an assistant reply. |

```
F7     mic ▸ speech-to-text ▸ optional LLM polish ▸ typed into the focused window (+ clipboard)
F8     mic ▸ speech-to-text ▸ LLM assistant       ▸ spoken reply, streamed into TTS
AI   voice ▸ MCP ▸ Claude Code / Cursor / Forge   ▸ the agent talks back and listens for more
```

Prefer no keys at all? Fono can idle and listen for a spoken wake phrase instead. Detection runs locally, and it's off until you enable it.

While you speak, a small overlay shows what the microphone hears — `bars`, `oscilloscope`, `fft`, or `heatmap`. Switch via the tray or `[overlay].style` in `~/.config/fono/config.toml`.

<p align="center">
  <a href="assets/styles.webp"><img src="assets/styles.webp" alt="Four overlay styles: bars, oscilloscope, FFT, heatmap" width="720"></a>
</p>

## What you get

- 🎙️ **It does everything voice.** Dictate into any window, ask a question and hear the answer, or drive your tools by voice. It bundles speech-to-text, natural text-to-speech, a local LLM, wake word, and speaker ID, all in one box. Dictation lands straight into the focused window on X11 or Wayland ([details](docs/wayland.md)) with a clipboard mirror as a safety net; the assistant streams its reply sentence-by-sentence and can call tools, including MCP-capable coding agents like Claude Code, Cursor, and Forge *(early preview, [docs](docs/coding-agents.md))*.
- 🧭 **Easy from minute one.** One command installs Fono and walks you through setup in the same terminal. The wizard measures your hardware and picks the right models for it, so there's no model shopping and no config file to learn. Switching providers is one command, and settings open as a searchable page in your browser, served locally by the daemon.
- 📦 **In one small binary.** ~22 MB on CPU or ~60 MB with cross-vendor GPU acceleration (Vulkan: NVIDIA / AMD / Intel), four glibc dependencies, and no Electron, no Node, no Python, no WebKit. `fono update` probes your host and pulls the matching build automatically.
- 🔒 **Local-first, actually.** With the default setup nothing leaves your machine. Whisper speech-to-text and a shared llama.cpp instance for polish and the assistant all run locally, with no duplicated memory, and every stage stays cloud-capable independently: swap just speech-to-text, or just text-to-speech, to any of a dozen providers with one command.
- 🏎️ **Fast, with receipts.** Pinned KV-cache snapshots and append-only prompts get the local assistant's first spoken word out in ~⅓ s on a laptop CPU, 2–4× ahead of Ollama on identical weights ([how we did it](https://bogdan.nimblex.net/programming/2026/06/10/making-local-llm-fast.html)). The model picker is backed by [900+ benchmark runs](https://fono.page/calibration), not guesses, and the installer auto-picks the GPU build so acceleration costs you nothing.
- 📡 **It serves, not just consumes.** Speaks the [Wyoming protocol](https://github.com/rhasspy/wyoming) as both client and server plus an OpenAI/Ollama-compatible API, so one Fono can be the voice backend for Home Assistant, another Fono, or your whole LAN, and mDNS finds peers automatically.
- 🔓 **Open source, GPL-3.0.** No telemetry, no account, no strings.
- ✨ **Nice touches everywhere.** Hands-free realtime conversation where you just keep talking. On-device voice ID that tags who's speaking ([docs](docs/speakers.md)). An assistant that can look at your screen when you point at something. Dictation that adapts to the focused app: shell vocabulary in terminals, identifier casing in editors. A searchable dictation history, and per-app voices so different programs answer in different, stable voices.

## Providers

Local by default; every stage can be swapped to a cloud provider independently.
Full matrix with models and config keys: [docs/providers.md](docs/providers.md).

| Stage | Local (default) | Cloud |
|-------|-----------------|-------|
| Speech-to-text | Whisper (bundled) | Groq · OpenAI · Gemini · Deepgram · Cartesia · AssemblyAI · Speechmatics · ElevenLabs |
| Polish | llama.cpp (bundled)\* · Ollama | Cerebras · Groq · OpenAI · Anthropic · Gemini · OpenRouter |
| Assistant | llama.cpp (bundled)\* · Ollama | OpenAI · Groq · Anthropic · Cerebras · Gemini · OpenRouter |
| Realtime assistant | — | Gemini Live (hands-free, back-and-forth conversation) |
| Text-to-speech | Kokoro (En) · Piper (International) · Supertonic (31 languages, opt-in) | OpenAI · Groq · Gemini · OpenRouter · Cartesia · Deepgram · ElevenLabs |

<sub>\* Polish and the assistant share a single llama.cpp instance — the local model is loaded once, not twice.</sub>

Switching is one command or web config save. No restart necessary, the daemon hot-reloads:

```sh
fono use cloud groq           # one key covers STT + polish + assistant + TTS
fono use stt deepgram         # change a single stage
fono use tts cartesia
fono use local                # back to fully local

fono keys add GROQ_API_KEY    # keys live in ~/.config/fono/secrets.toml
fono keys check               # reachability probe per stored key
```

## One box for the whole house

Fono doesn't need a desktop. `sudo fono install --server` sets up a hardened systemd service on a headless box, and that one machine becomes the voice backend for everything else: Home Assistant discovers it over mDNS as a Wyoming speech-to-text, text-to-speech, and wake-word provider; editors, Open WebUI, `llm`, and LangChain talk to its OpenAI- and Ollama-compatible API on port 11434; and other Fono desktops on the LAN route their dictation through it. Audio stays on your network. There's a Docker container too — see [docs/home-assistant.md](docs/home-assistant.md).

## Settings in your browser

Pick **Settings…** in the tray and every option opens as a searchable page in your browser. Saves apply instantly, no restart. It's a local page served by the daemon itself — loopback-only, off until you open it, and API keys are write-only: the page can set them but never read them back. Curious? [Click around the demo](https://fono.page/setup-demo/).

## Privacy

Local-first, by design. With the default setup, audio and text never leave your machine. Cloud providers are strictly opt-in, per stage. The full data-flow map — what leaves, when, and to whom — is in [docs/privacy.md](docs/privacy.md).

## Other ways to install

- **Distro packages.** `.deb`, `.pkg.tar.zst`, and `.txz` files are built by CI and attached to each [release](https://github.com/bogdanr/fono/releases/latest). They are not regularly tested — file an issue if one misbehaves.
- **macOS (Apple Silicon, experimental).** Each release attaches a Metal-accelerated `fono-vX.Y.Z-aarch64-apple-darwin` binary. Download it, `chmod +x`, and run `fono install` — it sets up start-at-login and walks you through the one-time permission grants. It's only been tested on a headless remote Mac so far, not eyeballed on a real display yet — if you try it, an issue report (good or bad) is genuinely useful. Details in [docs/build-macos.md](docs/build-macos.md).
- **Windows (experimental).** Each release attaches a `fono-vX.Y.Z-x86_64.exe`. Download it and run `fono install` — it copies the app into your user folder and starts it at login, no administrator prompt. One download uses your GPU when a driver is present and falls back to the processor otherwise. This is an early port, built and exercised remotely rather than daily-driven, so expect rough edges — if you try it, an issue report (good or bad) is genuinely useful. Details in [docs/build-windows.md](docs/build-windows.md).

## Documentation

- [Documentation index](docs/README.md) — the full map
- [Install](docs/install.md) — one-liner, manual install, server mode, updating
- [Quickstart](docs/quickstart.md) — first dictation, common follow-ups
- [Configuration](docs/configuration.md) — every key in `config.toml`
- [Provider matrix](docs/providers.md) — STT, polish, assistant, and TTS endpoints
- [Coding agents](docs/coding-agents.md) — voice loop for Claude Code, Cursor, Forge, and other MCP agents
- [Privacy](docs/privacy.md) — exactly what leaves your machine, and when
- [Speaker verification](docs/speakers.md) — on-device "who is speaking" tagging
- [Home Assistant](docs/home-assistant.md) — run the Docker container as a Wyoming STT/TTS server
- [Live dictation](docs/interactive.md) — streaming overlay, latency budget
- [Troubleshooting](docs/troubleshooting.md) — symptom-first recipes

## Status

Linux-first; used daily by the maintainer. macOS support is new and has not yet run on a real display — see [Other ways to install](#other-ways-to-install). Rough edges exist — issues and patches are welcome. See the [roadmap](ROADMAP.md) for what's next.

## Contributing

Pull requests welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for the workflow (DCO sign-off required).

## License

GPL-3.0-only. See [LICENSE](LICENSE).
