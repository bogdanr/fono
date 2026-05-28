# Fono documentation

Start here, then drop into whichever page covers what you're after.

## New to Fono

1. [install.md](install.md) — one-liner, manual install, server mode,
   updating, uninstalling.
2. [quickstart.md](quickstart.md) — your first dictation, your first
   assistant turn, the most common follow-up commands.

## Day-to-day reference

- [configuration.md](configuration.md) — every section in
  `config.toml`, hotkey rebinding, secrets, XDG paths.
- [providers.md](providers.md) — STT / polish / assistant / TTS
  matrices, per-provider quirks, the wizard.
- [interactive.md](interactive.md) — live (streaming) dictation: how
  the overlay paints while you speak, the tuning knobs that survived
  the 2026-05-22 simplification.
- [inject.md](inject.md) — text injection backends, clipboard safety
  net, per-compositor notes.
- [wayland.md](wayland.md) — overlay backends, hotkey registration
  (portal vs gsettings), per-compositor caveats.

## When something is wrong

- [troubleshooting.md](troubleshooting.md) — symptom-first recipes.
- `fono doctor` — diagnostic report covering config, paths, providers,
  audio device, injector, overlay backend, hotkey backend, tray host.

## Privacy and security

- [privacy.md](privacy.md) — what stays local, what leaves the
  machine, where data lives on disk.
- [../SECURITY.md](../SECURITY.md) — vulnerability disclosure.

## Under the hood

- [architecture.md](architecture.md) — workspace crate map, runtime
  model, the dictation FSM.
- [decisions/](decisions/) — Architecture Decision Records explaining
  *why* (language, default models, FSM design, overlay style,
  language stickiness, etc.).
- [plans/](plans/) — phased implementation plans driving the
  roadmap.
- [bench/](bench/) — performance baselines and sweep reports.

## Contributor docs

- [../AGENTS.md](../AGENTS.md) — orientation for agent sessions.
- [../CONTRIBUTING.md](../CONTRIBUTING.md) — DCO sign-off, formatting,
  clippy rules.
- [../ROADMAP.md](../ROADMAP.md) — in progress / planned / shipped.
- [dev/release-checklist.md](dev/release-checklist.md),
  [dev/update-qa.md](dev/update-qa.md) — release-time procedures.
- [status.md](status.md) — running session log.
