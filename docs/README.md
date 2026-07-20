# Fono documentation

Everything about installing, using, hosting, and hacking on Fono lives here.
Pick the section that matches what you're trying to do.

## Get started

- [Install Fono](install.md) — the one-liner, manual install, updating,
  uninstalling.
- [Do your first dictation](quickstart.md) — first dictation, first assistant
  turn, and the follow-up commands you'll reach for next.

## Use it

- [Change any setting](configuration.md) — every section of `config.toml`,
  hotkey rebinding, secrets, XDG paths.
- [Pick your STT, polish, and TTS providers](providers.md) — the full provider
  matrices, per-provider quirks, and the setup wizard.
- [Lock dictation to your own voice](speakers.md) — on-device speaker
  verification: enrolling, calibration, the `fono speaker` commands.
- [Watch words appear while you speak](interactive.md) — live streaming
  dictation and how the overlay paints during it.
- [Fix where text lands](inject.md) — text-injection backends, the clipboard
  safety net, per-compositor notes.
- [Sort out Wayland quirks](wayland.md) — overlay backends, hotkey registration
  (portal vs gsettings), per-compositor caveats.
- [Diagnose a problem](troubleshooting.md) — symptom-first recipes, plus
  `fono doctor` for a full diagnostic report.
- [See what stays local and what leaves the machine](privacy.md) — data flows
  and where files live on disk.
- [Report a vulnerability](../SECURITY.md) — disclosure process.

## Serve your network

- [Connect Home Assistant](home-assistant.md) — run the Docker container as a
  Wyoming STT/TTS server for your voice assistant.
- [Run a headless STT host](install.md#server-mode-wyoming-stt-tts-and-wake-word-host) — server
  mode from the install guide.
- [Serve your assistant as a local LLM API](configuration.md#serve-local-inference-over-http-openai--ollama-api)
  — an OpenAI- and Ollama-compatible HTTP endpoint for editors, Open WebUI, and
  Home Assistant.
- [Change settings in your browser](configuration.md#settings-in-the-browser) —
  the local web settings page, started with `fono config web`.

## Build on it

- [Talk to your coding agent by voice](coding-agents.md) — the MCP server,
  `fono agent-setup`, and per-agent configs for Claude Code, Cursor, Forge, and
  more.
- [Understand how Fono is put together](architecture.md) — workspace crate map,
  runtime model, the dictation FSM.
- [Keep the binary small](binary-size.md) — the size budget and how it's
  enforced.
- [Build on macOS](build-macos.md) and [build on Windows](build-windows.md) —
  platform-specific build notes.
- [Read why key choices were made](decisions/) — Architecture Decision Records
  (language, default models, FSM design, overlay style, and more).
- [Browse the implementation plans](plans/) — phased plans driving the roadmap.
- [Check performance baselines](bench/) — benchmark reports and sweep results.

Contributor notes: [get oriented for agent sessions](../AGENTS.md),
[read the contribution rules](../CONTRIBUTING.md) (DCO sign-off, formatting,
clippy), [see what's planned and shipped](../ROADMAP.md),
[follow the release checklist](dev/release-checklist.md),
[QA an update](dev/update-qa.md), and
[read the running session log](status.md).
