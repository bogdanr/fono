# ADR 0011 — Voice commands

## Status

Reconstructed (original lost in filter-branch rewrite; rationale
recovered from plan history at
`plans/2026-04-27-fono-interactive-v2.md:146` and
`plans/2026-04-27-fono-interactive-v5.md:227`, 2026-04-28).

(Rationale not recovered; this stub exists to fill the numbering gap
and link the relevant surviving artefacts.)

## Context

Hands-free dictation tools historically choose between hotkey-first
activation (user presses a key, then speaks) and command-word
activation ("hey computer, ..."). The latter requires either a
constantly-listening wake-word engine or aggressive VAD plus a
command-grammar matcher.

## Decision (recovered intent)

Hotkey-first activation. Voice commands are accepted only inside an
already-active dictation session — i.e. the user has already pressed
the toggle/hold hotkey and the daemon is listening. Always-on wake-word
listening is explicitly out of scope for the v0.x line; see ADR 0012.
Command grammar (`/cancel`, `/edit-last`, etc.) is layered on top of
the dictation pipeline and runs after the LLM cleanup step rather than
as a separate listener.

## Consequences

- Zero idle-state CPU and zero "what are you listening to" privacy
  concerns when the daemon is not actively recording.
- Command parsing is best-effort prose detection; the LLM cleanup step
  has the final say on whether a phrase is text or a command.
- Wake-word activation can be added later as an opt-in feature without
  reworking the command pipeline.

## Surviving artefacts

- `plans/2026-04-27-fono-interactive-v2.md:146`
- `plans/2026-04-27-fono-interactive-v5.md:227`
- `plans/2026-04-27-fono-interactive-v4.md:223`
