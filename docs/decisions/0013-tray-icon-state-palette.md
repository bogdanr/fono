# ADR 0013 — Tray icon state palette

## Status

Reconstructed (original lost in filter-branch rewrite; rationale
recovered from plan history at
`plans/2026-04-27-fono-interactive-v4.md:228` and
`plans/2026-04-27-fono-interactive-v5.md:219`, 2026-04-28).

(Rationale not recovered; this stub exists to fill the numbering gap
and link the relevant surviving artefacts.)

## Context

The tray icon is the primary always-visible surface. It must
communicate at a glance whether the daemon is idle, recording,
processing, paused (live mode), or in an error state. KDE Plasma,
GNOME, and sway each render tray icons differently; colour and shape
choices must hold up across all three.

## Decision (recovered intent)

Lock a small palette of mono-colour icon variants:

- **Idle** — neutral foreground colour (white on dark, black on light;
  driven by the indicator host's theme).
- **Recording** — accent colour (red).
- **Processing** — accent colour (amber).
- **Live mode** — accent colour (blue / teal).
- **Update available** — small badge overlay on the idle icon (per the
  self-update plan).

Icons live as SVG sources in `assets/`; runtime selects the matching
PNG variant for the active host. Animation (pulsing dot, etc.) is
deferred to F4 (real overlay window) where rasterisation is in our
hands.

## Consequences

- Tray-side state is unambiguous and matches the in-process overlay
  state machine (`OverlayState`).
- Icon swap is a cheap `set_icon` call; no menu rebuild needed.
- New states need a new variant + a new ADR amendment; the palette is
  not a free-for-all.

## Surviving artefacts

- `plans/2026-04-27-fono-interactive-v4.md:228`
- `plans/2026-04-27-fono-interactive-v5.md:219, 230`
