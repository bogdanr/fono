# AGENTS.md — Fono Agent Orientation

## What is Fono?

Fono is a GPL-3.0 Rust single-binary voice dictation tool for the desktop. It replaces
[Tambourine](https://github.com/kstonekuan/tambourine-voice) (Tauri + Python) and
[OpenWhispr](https://github.com/OpenWhispr/openwhispr) (Electron) with a lighter native
stack — no WebKit, no Node, no Python — while unioning their feature sets (global hotkey
push-to-talk, local + cloud STT, optional LLM cleanup, text injection, tray UI, history).
Target users: Linux desktop (i3 / sway / KDE / GNOME, X11 and Wayland), Windows, and macOS.

## Orientation: read in this order

1. `docs/plans/2026-04-24-fono-design-v1.md` — authoritative design and 10-phase
   implementation plan. This is the source of truth for *what to build and when*.
2. `docs/decisions/` — Architecture Decision Records (ADRs) explaining *why* key
   choices were made (language, name, license, default models).
3. `docs/status.md` — current phase, what's next, session log.
4. `CONTRIBUTING.md` — DCO sign-off requirement, formatting, and clippy rules.

## Current phase

**Phase 0 complete; Phase 1 next.** See `docs/status.md` for details.

## External references

- `/mnt/nvme0n1p5/Work/slackbuilds/earlyoom/` — NimbleX SlackBuild template to mirror
  when Phase 9 packaging lands.
- `/mnt/nvme0n1p5/Work/slackbuilds/tambourine-voice/` — earlier aborted attempt to
  package Tambourine on NimbleX; catalogued missing system deps (webkit2gtk-4.1,
  python3.13, uv, libxdo, libayatana-appindicator3). Useful reference for Phase 9
  dependency negotiation.
- <https://github.com/kstonekuan/tambourine-voice> and
  <https://github.com/OpenWhispr/openwhispr> — upstream projects whose feature union
  Fono is replicating.

## Hard rules for agent sessions

- All commits **MUST** be signed off (`git commit -s`) — DCO enforced by CI.
- Every Rust source file **MUST** start with `// SPDX-License-Identifier: GPL-3.0-only`
  on line 1.
- Do **NOT** add dependencies without updating `deny.toml` and verifying the licenses
  are compatible with GPL-3.0.
- Do **NOT** add Llama-family or Gemma models as defaults — their licenses are not
  OSI-approved. Opt-in only. (See `docs/decisions/0004-default-models.md`.)
- Do **NOT** attempt to install system packages on NimbleX. Document required deps in
  `docs/providers.md` or the SlackBuild `REQUIRES=` and let the user install them.
- Work **one phase at a time**; tick checkboxes in the design plan as you go; update
  `docs/status.md` at the end of every session.

## Next-step template

Typical user invocation for a new session:

> "Continue from Phase N per docs/plans/2026-04-24-fono-design-v1.md; update
> status.md when done."
