# ADR 0031 — Visual Context Capture

## Status

Accepted

## Context

Coding agents and the F8 voice assistant benefit from being able to see
what is on the user's screen. Two distinct use-cases drive the design:

1. **Agent use-case (MCP tool `fono.screen`):** A coding agent (Forge,
   Claude Code, Cursor, etc.) needs a screenshot to understand an error
   dialog, a UI state, or a terminal output that is not captured in the
   conversation text.

2. **Assistant use-case (LLM tool `fono_screen`):** During an F8 voice
   turn the user says "look at this" or "what do you see?" and the model
   autonomously decides to call the screen-capture function.

### Two-mode design

Both use-cases share one underlying capture pipeline with two modes:

- **Automatic** — grabs the focused window instantly, no UI, no user
  interaction. Intended for "look at the current error" patterns where
  the agent/assistant is confident the focused window is what the user
  means.
- **Interactive** — opens the OS-native region picker (e.g. `slurp` on
  Wayland, `scrot -s` on X11) so the user can frame a specific area.
  Intended for "let me show you this part" patterns.

### No full-screen capture by default

Full-screen capture would leak unrelated content (other terminal windows,
browser history, notification content). Automatic mode limits capture to
the focused window; interactive mode gives the user explicit control over
the captured region. Full-screen capture is not offered.

### No configuration block

No `[screen_capture]` config block is added. Tool-ladder probing happens
at runtime via PATH inspection, which is fast and sidesteps the "which
tool do I have?" onboarding friction. `fono doctor` surfaces the result
so users know which rung is active without editing config.

### LLM tool-calling for the assistant path

The F8 assistant path adds `fono_screen` as an LLM function-calling
tool. The model decides autonomously whether to capture the screen based
on the user's natural language. This avoids adding a mandatory screen-
capture step to every assistant turn and keeps latency low when no visual
context is needed.

### 250 KB binary ceiling

Screenshots are downscaled to at most 1600×1600 pixels (and capped at
2 MiB bytes) before encoding, which keeps base64 payloads under ~250 KB
in typical desktop resolutions. This is within the context-window limits
of all supported models.

### Probe-ladder ordering principle (lightest first)

The tool ladder orders candidates from lightest to heaviest system
impact:

1. **Portal** (XDG Desktop Portal) — zero external binary, native
   Wayland, sandboxed. Deferred to a future PR pending `ashpd` 0.10
   stabilisation.
2. **scrot** / **grim+slurp** — single lightweight binary, minimal
   dependencies.
3. **maim** / **import** (ImageMagick) — heavier, but widely available.
4. **spectacle** / **gnome-screenshot** — full GUI applications, slowest
   to launch.

### XWayland import gate

`import` (ImageMagick) uses X11 APIs and can only capture XWayland
windows when `DISPLAY` is set. On a pure Wayland session without
`DISPLAY`, `import` is excluded from the ladder to avoid silent
capture failures.

### Probe-ladder summary

- **Wayland auto:** portal → import (Xwayland, if DISPLAY set) → spectacle → gnome-screenshot
- **Wayland interactive:** grim+slurp → portal → import (Xwayland) → spectacle → gnome-screenshot
- **X11 auto:** scrot → maim → import → gnome-screenshot
- **X11 interactive:** scrot -s → maim -s → import → gnome-screenshot

## Decision

- Implement a `fono-core::screen_capture` module with a `GrabberProbe`
  type that detects available tools and exposes a `capture(mode,
  focused_wm_class)` method.
- Add a `fono.screen` MCP tool in `fono-mcp-server`.
- Add a `fono_screen` LLM function-calling tool in `fono-assistant`
  behind the `prefer_vision` config flag.
- Add a privacy gate in `GrabberProbe::capture`: if the focused window
  class matches a known private application (KeePassXC, Bitwarden,
  1Password, GNOME Keyring, Seahorse), return `CaptureError::PrivateWindow`
  without attempting capture.
- Surface tool-ladder status in `fono doctor`.

## Consequences

- No new *required* runtime dependencies. All screen-capture tools are
  optional; the agent degrades gracefully when none are available.
- Privacy gate is enforced at the Fono layer, not the OS layer, so it
  applies regardless of which capture backend is active.
- Full XDG Portal support (sandboxed Flatpak / Snap environments)
  requires a follow-up PR with `ashpd` integration once the API
  stabilises.
