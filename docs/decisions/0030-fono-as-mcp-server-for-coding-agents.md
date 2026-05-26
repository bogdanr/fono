# ADR 0030 — Fono as MCP server for coding agents

- **Status:** Accepted
- **Date:** 2026-05-26
- **Plan:** [`plans/2026-05-25-fono-voice-loop-for-coding-agents-v1.md`](../../plans/2026-05-25-fono-voice-loop-for-coding-agents-v1.md)
- **Inverse of:** [`plans/2026-05-22-voice-actions-via-mcp-v1.md`](../../plans/2026-05-22-voice-actions-via-mcp-v1.md)
  (Fono-as-MCP-client dispatching to Home Assistant et al.)

## Context

Fono already speaks voice in ↔ text out. The natural extension for users who
code with AI agents is to close the loop in both directions: speak prompts *in*
to the agent and hear concise spoken answers *back*, all without touching the
keyboard between turns.

Three design questions drove the decision:

1. **Which side of MCP is Fono?** The voice-actions plan (`0029`) makes Fono an
   MCP *client*, calling external tool servers. For the coding-agent loop, the
   coding agent needs to call *Fono's* audio hardware — so Fono is the MCP
   *server*.

2. **How do we avoid agent-specific code?** Every MCP-capable coding agent
   (Forge, Claude Code, Cursor, Codex CLI, Gemini CLI, Cline, Continue,
   Windsurf, Goose, and future entrants) already speaks the same JSON-RPC
   wire protocol. The integration for any individual agent is therefore a
   *configuration snippet*, not a code change. Agent quirks live in `agents.toml`
   (data), never in `fono-mcp-server` (code).

3. **What is the minimal tool surface?** Three tools cover every voice-loop
   interaction pattern:
   - `fono.speak { text }` — speak text, block until audio finishes.
   - `fono.listen { prompt?, max_seconds? }` — optionally speak a prompt, record
     until silence, return the transcript.
   - `fono.confirm { question, choices, timeout_seconds? }` — speak a multiple-
     choice question, bias the STT decoder toward the choices vocabulary, return
     the matched choice.

## Decision

Ship a new `fono-mcp-server` crate that exposes Fono as a local MCP server over
stdio transport (v1 only; SSE/HTTP follows in v2 when remote-agent use-cases
are clear).

### Agent-agnostic design principle

> **No agent-specific code lives inside `fono-mcp-server` or the tool
> implementations.** Agent quirks (config file location, preset-injection
> mechanism, tool-timeout defaults, output-channel conventions) live in
> *data* — `agents.toml` and per-agent doc sections in
> `docs/coding-agents.md`. The protocol surface is what every MCP client
> already consumes; making one agent work means writing a config snippet,
> not patching Fono.

This principle is a hard constraint on every PR that touches
`fono-mcp-server`. If a code change would only be needed for one specific
agent, the design must be reworked so the fix is general, or the quirk must
be expressed as a flag in `agents.toml` consumed by the `agent-setup`
helper.

### Verification gate: ≥ 3 agents in v1

Phase 6 of the plan ships verified end-to-end against **Forge + Claude Code
+ Cursor** in the same release. This gate exists specifically to catch any
accidental Forge-isms before tag — if the integration works on three different
agents with different config formats and different preset-injection mechanisms,
it is genuinely agent-agnostic.

### Tool surface

The three tools are intentionally minimal:

| Tool | Input | Returns |
|---|---|---|
| `fono.speak` | `{ text: string }` | `{ ok: true }` or `{ cancelled: true }` |
| `fono.listen` | `{ prompt?: string, max_seconds?: number }` | `{ transcript: string }` |
| `fono.confirm` | `{ question: string, choices: string[], timeout_seconds?: number }` | `{ choice: string }` (or `"timeout"`) |

Future tools (`fono.history`, `fono.set_language`, `fono.cancel`, vision tools)
are additive: the tool registry is data-driven so each is an append, not a
refactor.

### Transport: stdio only in v1

Stdio is the correct starting point because:
- Every MCP client already supports it (it's the reference transport).
- Trust boundary is trivially clear: only a process that can spawn
  `fono mcp serve` can drive it — same trust as running any program.
- No auth tokens, no network config, no firewall rules.

SSE/HTTP (v2) adds remote-agent support with an auth-token gate. That
decision is deferred until there are users who need it.

### `agents.toml` registry

A new `~/.config/fono/agents.toml` file holds the agent registry. First-party
entries ship with Fono for the common MCP-capable agents (Forge, Claude Code,
Cursor, Codex CLI, Gemini CLI); users append their own entries without
recompiling. Each entry declares:

```toml
[[agent]]
name = "forge"
command = ["forge"]
mcp_config_path = "~/.forge/mcp.json"
preset_injection = "agents-md"   # cli-flag | config-file | agents-md | claude-md | manual
```

The `preset_injection` field describes *how* the shared voice-mode preset is
loaded into that agent. This is the only agent-specific knowledge Fono needs.

### Voice-mode system prompt: one file, shared verbatim

`assets/agent-presets/voice.md` is the single highest-leverage deliverable in
this plan. All agents load the same text; per-agent docs explain only *how*
to load it (different agents use different config files). The prompt text
never forks.

### Shared protocol code with `fono-action`

Both this plan and `plans/2026-05-22-voice-actions-via-mcp-v1.md` use JSON-RPC
over stdio. Whichever plan lands second extracts the common wire types into a
new `fono-mcp-protocol` crate. Pre-creating it now would be premature
abstraction.

## Consequences

**Positive:**
- Any new MCP-capable coding agent that appears in the ecosystem integrates via
  one `agents.toml` entry and one `docs/coding-agents.md` section — zero Fono
  code changes.
- The voice-mode system prompt is a single file; quality improvements benefit
  every agent at once.
- Stdio-only v1 is simple, auditable, and inherently sandboxed.

**Negative / trade-offs:**
- `fono mcp serve` must route all log output to stderr; stdout is the MCP
  channel. Misconfigured logging would silently corrupt the protocol stream.
  Mitigated by explicit tracing-subscriber configuration and a unit test for
  stdout cleanliness.
- Audio device contention: `fono.listen` and `fono.speak` cannot run
  simultaneously in v1 (listen queues behind speak). Full-duplex with echo
  cancellation is a v2 item.
- The agent must obey the voice-mode system prompt to produce short,
  voice-friendly responses. If an agent ignores the prompt and emits
  page-long markdown, the markdown sanitiser in `fono.speak` still produces
  listenable audio, but the user experience degrades. Documented as a known
  limitation; the user's responsibility to keep the prompt current.

## Relationship to other ADRs

- **ADR 0029** (voice actions, Fono-as-MCP-client) — inverse direction;
  shared wire types extracted into `fono-mcp-protocol` when the second plan
  lands.
- **ADR 0022** (binary size budget) — the new crate adds `serde_json` usage
  (already in the dependency tree) and minimal new code. Binary-size delta
  is gated at ≤ 0.5 MiB in Phase 7; if exceeded, the crate is feature-gated.
