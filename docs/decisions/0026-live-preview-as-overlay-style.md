# ADR 0026 — Live preview is an overlay visualization style

- **Status:** Accepted
- **Date:** 2026-05-17
- **Supersedes:** [ADR 0009 — Interactive live dictation](0009-interactive-live-dictation.md) (architectural shape only; rationale for *having* a live mode is unchanged)
- **Plan:** [`plans/2026-05-17-live-transcript-as-overlay-style-v2.md`](../../plans/2026-05-17-live-transcript-as-overlay-style-v2.md)

## Context

Before this change Fono had two orthogonal user-facing toggles for
what the overlay does during a recording:

1. `[overlay].style ∈ { Bars, Oscilloscope, Fft, Heatmap }` — picked
   from the tray "Waveform style" submenu, controls how audio levels
   are visualized.
2. `[interactive].enabled : bool` — a config-file-only flag that
   replaced the waveform with streaming transcript text **and**
   rewired the dictation hotkey to a separate live pipeline.

This created two bugs by construction:

- The tray exposed the waveform picker but not the live-preview
  toggle, so a user who picked "Transcript-y behaviour" had to
  hand-edit `~/.config/fono/config.toml` and restart.
- The dictation hotkey only entered the streaming path when
  `[interactive].enabled = true`; without it the user saw post-record
  batch text and read that as "live transcription is broken". The
  assistant happened to feel live because it always shows post-record
  Thinking/TTS activity, but no equivalent existed for dictation.

The two toggles were conceptually the same question — *what should the
overlay show while I'm recording?* — split across two surfaces for
historical reasons.

## Decision

Collapse the two toggles into a single ordered picker on
`[overlay].style` with five entries:

```
Bars | Oscilloscope | Fft (default) | Heatmap | Transcript
```

- `Transcript` *is* live-preview mode. Picking it both swaps the
  overlay renderer to the text layout and routes dictation through the
  streaming pipeline (rewriting `HoldPressed` → `LiveHoldPressed` in
  the hotkey FSM dispatcher, and asking the STT factory to build a
  streaming client when the backend supports one).
- `Fft` stays the first-run default. Live preview is opt-in because it
  costs more — extra CPU for local backends and extra API tokens for
  any cloud STT that bills per-second of streamed audio.
- The `[interactive]` config block keeps its boundary-heuristic /
  drain-grace / cleanup knobs (those are streaming-pipeline tuning,
  not a UX toggle) but loses its `enabled` field. The new helper
  `Config::live_preview() -> bool` is the single source of truth and
  is defined as `overlay.style == Transcript`.
- The tray label for the new entry is `"Transcript (live preview —
  more CPU / tokens)"` so the cost is visible at the click site.

## Consequences

**Removed**

- `Interactive::enabled` field; one-shot config migration is **not**
  provided — Fono has no users yet.
- `OverlayMode` enum (collapsed into `WaveformStyle`).
- `RealOverlay::enable_text_mode` / `enable_waveform_mode` / the
  twin `spawn` / `spawn_waveform` constructors. The overlay now has a
  single `spawn(style)` entry point and one `set_style(style)` runtime
  switch.
- The wizard's "enable live mode?" prompt. The tray picker is the only
  control.

**Renamed**

- `translate_for_interactive` → `translate_for_live_preview`.
- Factory parameter `interactive_enabled: bool` → `live_preview: bool`.

**Fallback**

When `overlay.style = Transcript` but the active STT backend has no
streaming implementation, the daemon logs a single warn-level line at
startup and the runtime falls back to the batch path with a
placeholder line in the overlay. No silent dead-overlay state.

**Tests**

The equivalence harness's `A2-default` row stays pinned to the new
default (`style = Fft`, no live preview); a separate row exercises
`style = Transcript` end-to-end. The existing `[interactive]`
boundary-heuristic knobs the harness pins (R18.23) are unchanged.

## Rejected alternatives

1. **Keep both toggles, document the relationship.** Loses the bug-fix:
   the tray still wouldn't surface live mode, and new users would still
   wonder why dictation feels non-live.
2. **Fold the entire `[interactive]` block into `[overlay]`.** Out of
   scope. The remaining `Interactive` fields are streaming-pipeline
   tuning that applies to the orchestrator independent of where the UI
   picker lives. Possible future config-shape pass.
3. **Make `Transcript` the default.** Rejected per maintainer
   guidance: extra CPU/token cost should be opt-in, and `Fft` is the
   visually-rich passive option most first-run users actually want.
