<div align="center">

# Fono

A lightweight dictation tool for Linux. Press a key, speak, and the text lands at your cursor.

[![CI](https://github.com/bogdanr/fono/actions/workflows/ci.yml/badge.svg)](https://github.com/bogdanr/fono/actions/workflows/ci.yml)
[![License: GPL-3.0-only](https://img.shields.io/badge/License-GPL--3.0--only-blue.svg)](LICENSE)
[![Latest release](https://img.shields.io/github/v/release/bogdanr/fono)](https://github.com/bogdanr/fono/releases/latest)
[![Homepage](https://img.shields.io/badge/home-fono.page-2ea44f)](https://fono.page)

</div>

<p align="center">
  <a href="assets/fono.webp"><img src="assets/fono.webp" alt="Fono dictation demo: press a hotkey, speak, the text appears at the cursor" width="720" loading="lazy"></a>
</p>

## Install one-liner

```sh
curl -fsSL https://fono.page/install | sh
```

The script picks the right binary for your CPU (and switches to the Vulkan-GPU build if your machine has one), runs `sudo fono install` to place it on `$PATH`, starts the daemon, and opens the `fono setup` wizard in the same terminal.

## Different styles

<p align="center">
  <a href="assets/styles.webp"><img src="assets/styles.webp" alt="Four overlay visualisation styles: bars, oscilloscope, FFT, heatmap" width="720" loading="lazy"></a>
</p>

While you're speaking, a small overlay shows what the microphone is hearing. Four styles ship: `bars`, `oscilloscope`, `fft`, `heatmap`. Switch via the tray (*Preferences → Waveform style*) or set `[overlay].style` in `~/.config/fono/config.toml`.

## What Fono does

- **Dictation, push-to-talk or toggle.** Tap `F7` to toggle recording; hold `F7` for push-to-talk. The same key works either way — the press duration decides.
- **Lands in any X11 window.** Fono pastes with `Shift+Insert` after copying the text to the clipboard. Wayland works once you bind `fono toggle` in your compositor's keyboard settings (KDE, GNOME, sway); portal-based auto-binding is on the roadmap.
- **Local or cloud speech-to-text.** Whisper runs on your machine by default. Or switch to Groq, OpenAI, or Deepgram with one command (`fono use stt …`).
- **Optional cleanup pass.** A small LLM can tidy up the transcript before it's injected — locally with `llama.cpp`, or via Cerebras / Groq / OpenAI / OpenRouter / Anthropic / Ollama.
- **Voice assistant on `F8`** *(cloud-only for now)*. Talk to OpenAI, Anthropic, Groq, Cerebras, or OpenRouter; the reply is streamed sentence-by-sentence into TTS so audio starts before the model has finished thinking.
- **Visualisation overlay during recording.** Bars, oscilloscope, FFT, or heatmap. Live-dictation mode adds a small VU bar.
- **Optional GPU acceleration.** `fono update` probes your host for Vulkan and pulls the matching CPU or Vulkan build automatically.
- **LAN-friendly.** Speaks the [Wyoming protocol](https://github.com/rhasspy/wyoming) as both client and server, so Fono can route through (or host for) a Home Assistant satellite or another Fono on the network. mDNS finds peers automatically.
- **One static binary, around 20 MB.** No Electron, no Node, no Python, no WebKit. Four glibc dependencies.

## First run

`sudo fono install` - installs the files and starts the setup wizzard

Default hotkeys are `F7` (dictation) and `F8` (voice assistant). Both keys auto-detect how you press them: a quick tap toggles recording on (tap again to stop); holding for more than a second turns the key into push-to-talk and recording ends on release. `Escape` cancels a recording or shuts up an assistant reply.

The setup wizard hot-reloads the running daemon when it finishes, so you don't need to restart anything. Reconfigure with `fono setup`.

## Switching providers

`fono setup` asks for a primary cloud provider. With OpenAI or Groq, a single API key covers STT, cleanup, the assistant, and TTS. Narrower providers (Anthropic, Cerebras, OpenRouter) cover what they offer; the wizard only prompts for follow-on keys if you opt in to capabilities they don't cover.

```sh
fono use cloud groq           # paired preset (Groq STT + Groq LLM)
fono use stt openai           # change just STT
fono use tts cartesia         # swap TTS backend
fono use local                # back to whisper-local + skip LLM
```

Keys live in `~/.config/fono/secrets.toml`:

```sh
fono keys add GROQ_API_KEY    # paste at the prompt
fono keys check               # reachability probe per stored key
```

TTS works with OpenAI, Groq, OpenRouter (Kokoro), Cartesia, Deepgram, and any Wyoming server you have on the LAN.

## Other ways to install

- **Distro packages.** `.deb`, `.pkg.tar.zst`, and `.txz` files are built by CI and attached to each [release](https://github.com/bogdanr/fono/releases/latest), but they are not regularly tested — they may work, please file an issue if they don't.
- **macOS and Windows.** Planned, not shipping.

## Privacy

Local-first. Nothing leaves your machine unless you pick a cloud provider.

## Documentation

- [Roadmap](ROADMAP.md) — in progress, planned, and shipped.
- [Provider matrix](docs/providers.md) — STT, LLM, and TTS endpoints, env vars, default models.
- [Live (streaming) dictation](docs/interactive.md) — overlay, latency budget, configuration.
- [Text injection](docs/inject.md) — Shift+Insert, per-app overrides.
- [Wayland notes](docs/wayland.md) — compositor binding.
- [Troubleshooting](docs/troubleshooting.md) — symptom-first recipes.
- Homepage: [fono.page](https://fono.page).

## Status

Linux-first; used daily by the maintainer. Rough edges exist — issues and patches are welcome. See [`ROADMAP`](ROADMAP.md) for what's next.

## Contributing

Pull requests welcome. See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the workflow (DCO sign-off required).

## License

GPL-3.0-only. See [LICENSE](LICENSE).
