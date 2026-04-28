# ADR 0012 — Wake-word activation

## Status

Reconstructed (original lost in filter-branch rewrite; rationale
recovered from plan history at
`plans/2026-04-27-fono-interactive-v3.md:204` and
`plans/2026-04-27-fono-interactive-v5.md:228-229`, 2026-04-28).

(Rationale not recovered; this stub exists to fill the numbering gap
and link the relevant surviving artefacts.)

## Context

Always-on wake-word activation ("hey fono, ...") would let users
trigger dictation without a hotkey. Implementations need either an
on-device wake-word model (Picovoice, openWakeWord, etc.) or a
cloud-streaming pass that pays per-second for idle audio.

## Decision (recovered intent)

Out of scope for v0.x. Documented as a Slice D / v1.0 candidate.
Hotkey-first activation (ADR 0011) is the only supported entry point
for the v0.x line.

If revisited, the engine choice will be made in a future ADR; current
plans favour an on-device approach to preserve the "no idle-state
network traffic" promise.

## Consequences

- No always-listening privacy footprint in the v0.x release.
- No new dependency on a wake-word engine until the feature is
  reactivated.
- The interactive-mode plan keeps the door open via reserved config
  keys (`eou_adaptive`, `resume_grace_ms` in `[interactive]`) but
  these are inert until the wake-word work lands.

## Surviving artefacts

- `plans/2026-04-27-fono-interactive-v3.md:204`
- `plans/2026-04-27-fono-interactive-v4.md:224-227`
- `plans/2026-04-27-fono-interactive-v5.md:228-229`
