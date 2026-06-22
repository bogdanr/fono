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

## Relationship to PipeWire AEC (when revisited)

Wake-word has two modes with opposite acoustic needs, and only one of
them touches the PipeWire echo-cancel work from
`plans/2026-05-25-double-talk-barge-in-pipewire-aec-v1.md`:

- **Idle always-on listening** (the headline feature: Fono idle,
  nothing playing). There is no Fono playback to cancel, so AEC is a
  no-op and must **not** be loaded. The detector reads the default
  source directly. AEC also cannot help with the real idle challenge —
  rejecting TV / music / non-wake speech — because the AEC sink only
  carries Fono's *own* TTS; ambient audio never passes through it.
  Idle wake-word must therefore work on every platform off the default
  source and must **not** depend on AEC (which is Linux/PipeWire-only).
- **Wake / interrupt while the assistant is speaking** is exactly the
  "talk over the assistant" case AEC was built for. To detect a wake
  phrase over Fono's own TTS, the TTS must be cancelled from the mic —
  here wake-word **reuses** the AEC, consuming the same
  `fono_aec_source_<pid>`.

What is reusable is the **capture + detector seam**, not the
echo-canceller itself. Barge-in ("read a source → envelope/VAD → fire
`AssistantPressed`") and wake-word are the same shape with an energy
VAD vs a KWS model swapped in as the trigger. The intent is to land
that detector abstraction once (in the AEC slice) with interchangeable
triggers, treat AEC as an optional upgrade engaged only while Fono is
making noise, and account for the lifecycle mismatch (AEC is
per-utterance and short-lived; wake-word listening is always-on, so the
detector switches its input to the AEC source only while one exists and
back to the default source otherwise).

## Surviving artefacts

- `plans/2026-04-27-fono-interactive-v3.md:204`
- `plans/2026-04-27-fono-interactive-v4.md:224-227`
- `plans/2026-04-27-fono-interactive-v5.md:228-229`
