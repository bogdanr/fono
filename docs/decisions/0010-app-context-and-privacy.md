# ADR 0010 — App context and privacy

## Status

Reconstructed (original lost in filter-branch rewrite; rationale
recovered from plan history at
`plans/2026-04-27-fono-interactive-v2.md:144` and
`plans/2026-04-27-fono-interactive-v4.md:222`, 2026-04-28).

(Rationale not recovered; this stub exists to fill the numbering gap
and link the relevant surviving artefacts.)

## Context

The interactive / live-dictation slice considered surfacing app-aware
behaviour: per-app paste rules, per-app vocabulary boost, per-app
prompt overrides. The privacy concern: capturing the focused
window's title or class name to drive that behaviour means the daemon
maintains a record of which apps the user dictates into.

## Decision (recovered intent)

Capture only the focused window's **category** (e.g. "terminal",
"browser", "editor") rather than its title or PID. No keystroke
content beyond what the user dictates leaves the daemon. No telemetry.
Per-app rules are user-authored (`[[context_rules]]` blocks in
`config.toml`); the daemon never auto-learns rules from observed
dictations.

## Consequences

- The daemon can route dictations through different LLM prompts or
  paste shortcuts per category without keeping a per-app history.
- Future work (auto-translation per-app target, vocabulary boost per
  category) layers cleanly on top.

## Surviving artefacts

- `plans/2026-04-27-fono-interactive-v2.md:144`
- `plans/2026-04-27-fono-interactive-v4.md:222`
- `plans/2026-04-27-fono-interactive-v5.md:226`
