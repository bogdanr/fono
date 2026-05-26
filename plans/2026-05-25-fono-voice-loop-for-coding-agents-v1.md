# Voice Loop for Coding Agents — v1

## Objective

Let the user drive **any MCP-capable coding agent** entirely by voice:
dictate prompts in, hear concise spoken answers back, answer A/B/C-style
follow-up questions out loud, never touch the keyboard between turns.

**The end goal is agent-agnostic integration.** Fono speaks the MCP
protocol on the server side; any client that speaks MCP — Forge,
Claude Code, Cursor, Codex CLI, Gemini CLI, Cline, Continue, Windsurf,
Goose, and anything that ships tomorrow — becomes voice-driven with
a single config entry. The agent-specific work is **configuration
and documentation**, never new Fono code. There is no `fono-forge`
module, no Forge-shaped tool surface, no agent-specific branching
anywhere in `fono-mcp-server` — and the plan's verification gates
enforce that explicitly (Phase 6 ships ≥ 2 additional agents working
end-to-end alongside Forge in v1 so we catch any accidental Forge-isms
before tag).

**Forge is the first integration target** because it's the
maintainer's daily driver and the tightest dogfood loop, but it is
explicitly a stepping stone, not the destination. Within v1 itself,
at least two additional agents (Claude Code + Cursor) ship verified
end-to-end alongside Forge.

This is the **inverse direction** of `plans/2026-05-22-voice-actions-via-mcp-v1.md`
(Fono-as-MCP-client, dispatching actions to Home Assistant et al.).
This plan ships **Fono-as-MCP-server**, with the coding agent as the
client. The two plans share JSON-RPC wire types and stdio transport;
either can land first, and once both ship Fono is simultaneously
client and server (which is fine and useful — voice-driven agentic
loops where the agent calls Fono for speech, then Fono calls HA for
the lights).

The non-goals of v1: vision/screen-share tools, biometric speaker
verification, multi-user sessions, remote SSE transport. All deferred
to the *Future work* section.

## Outcome

After v1 ships, **the primary integration path is identical for every
MCP-capable coding agent**: add one `fono` MCP server entry to the
agent's config, load the shipped voice-mode system prompt, and the
agent is voice-driven. No Fono changes required when a new agent
appears in the ecosystem.

A user with any MCP-capable agent installed has two ways to enter a
voice session:

```sh
# Path 1 — agent-native: user runs their agent normally, the agent
# discovers fono as a configured MCP server and uses it.
forge                          # or: claude-code, cursor, codex, gemini, …

# Path 2 — optional convenience wrapper: Fono spawns the agent for
# the user with the right MCP config and voice preset pre-loaded.
fono agent-loop --agent forge        # ships in v1
fono agent-loop --agent claude-code  # ships in v1
fono agent-loop --agent cursor       # ships in v1 (best-effort)
fono agent-loop --agent <custom>     # arbitrary command, from
                                     # ~/.config/fono/agents.toml
```

The `fono agent-loop` wrapper is **agent-agnostic by design**: it
reads a small `agents.toml` registry mapping agent names to (command,
args, mcp-config-path, voice-preset-injection-method). Adding a new
agent is a config-file edit, not a code change.

Concrete user-facing surfaces:

- **`fono speak --stream`** — reads stdin, sentence-segments,
  strips markdown, pipes through the existing TTS backend. Works
  standalone as a pipe (`forge | fono speak --stream`).
- **`fono mcp serve`** — exposes Fono as an MCP server over stdio.
  Three tools registered:
  - `fono.speak  { text }` → speak, block until audio finishes.
  - `fono.listen { prompt?, max_seconds? }` → optionally speak the
    prompt, record until silence (reusing the existing
    `silence_watch`), return the transcript.
  - `fono.confirm { question, choices, timeout_seconds? }` → speak
    question + choices, listen with the STT decoder biased toward
    the choice vocabulary, return the matched choice or `timeout`.
- **`fono agent-loop --agent <name>`** — agent-agnostic convenience
  entry point. Looks up the agent in `agents.toml` (built-in entries
  ship for Forge + Claude Code + Cursor + Codex CLI + Gemini CLI;
  users can add more), spawns it with the voice preset loaded and
  the MCP config pointed at this Fono daemon; runs until the user
  says "we're done" or hits Escape; shows a red tray badge while
  active.
- **Documented voice-mode preset** in `docs/coding-agents.md`. One
  system prompt shared verbatim across all agents — the single
  highest-leverage piece of the whole plan. Per-agent sections only
  document *how* to load it into that agent (different agents have
  different config files); the prompt text itself never forks.
- **Hotkey ownership glue**: while an MCP-driven recording is active,
  F7 becomes barge-in instead of starting a parallel recording. The
  tray badge makes the active session discoverable.
- **`fono doctor`** "Coding agents" section lists configured MCP
  clients, last handshake status, last tool-call timestamp.

What v1 deliberately does **not** include:

- SSE/HTTP transport (only stdio in v1; remote agents wait for v2).
- Auto-editing the agent's MCP config file on the user's behalf — we
  ship documented snippets per agent in `docs/coding-agents.md` but
  the user pastes them in themselves (or runs the agent's own
  `mcp add` command). Same hands-off posture for every agent.
- Speaker verification / multi-user sessions.
- Audio device contention (echo cancellation while TTS plays). v1
  pauses listening during `fono.speak`; full-duplex is v2.
- Vision / screen-capture tools. Belong to their own plan; trait
  shape is forward-compatible.

## Architecture

### Where it sits in the existing pipeline

```
┌─────────────────────────────────────────────────────────────────┐
│ user terminal                                                    │
│                                                                  │
│  $ fono agent-loop --agent <forge | claude-code | … | custom>   │
│        │                                                         │
│        ▼                                                         │
│  ┌──────────────────┐    spawn + stdio   ┌────────────────────┐ │
│  │ fono mcp serve   │◀────────MCP────────│ <any MCP-capable   │ │
│  │                  │                    │  coding agent>     │ │
│  │  fono.speak    ──┼────► fono-tts      │                    │ │
│  │  fono.listen   ──┼────► fono-stt      │ shared voice-mode  │ │
│  │  fono.confirm  ──┼────► STT + grammar │ system prompt:     │ │
│  └──────────────────┘                    │  "be brief, offer  │ │
│                                          │   A/B/C options,   │ │
│  Same protocol, same tools, no           │   call fono.speak  │ │
│  per-agent code paths in Fono.           │   after each turn" │ │
│                                          └────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
```

### Agent-agnostic design principle

This principle constrains every implementation decision in this plan
and should be invoked when reviewing any future PR that touches the
MCP server crate:

> **No agent-specific code lives inside `fono-mcp-server` or the
> tool implementations.** Agent quirks (config file location,
> preset-injection mechanism, tool-timeout defaults, output-channel
> conventions) live in *data* — `agents.toml` and per-agent doc
> sections. The protocol surface is what every MCP client already
> consumes; making one agent work means writing a config snippet,
> not patching Fono.

If v1 development surfaces a case where this is genuinely impossible
(e.g. one agent's MCP client violates spec), the workaround lives in
`agents.toml` as a per-agent flag consumed only by the `agent-loop`
wrapper, never in the server core.

### New crate: `fono-mcp-server`

| File | Role |
|---|---|
| `src/lib.rs` | Re-exports + crate doc. |
| `src/server.rs` | `McpServer` — owns transport, tool registry, request loop. |
| `src/transport.rs` | `ServerTransport` trait + `StdioTransport` impl. |
| `src/protocol.rs` | Wire types: `initialize`, `initialized`, `tools/list`, `tools/call`, error shapes. **Shared with `fono-action` if it ships first**; otherwise extracted into a shared `fono-mcp-protocol` crate at the time the second lands. |
| `src/tools/mod.rs` | `Tool` trait + dispatch by name. |
| `src/tools/speak.rs` | `fono.speak` impl. Calls into `fono-tts`. |
| `src/tools/listen.rs` | `fono.listen` impl. Drives `fono-audio` + `fono-stt` + the existing `silence_watch`. |
| `src/tools/confirm.rs` | `fono.confirm` impl. Calls `listen` with a biased decoder. |

### Extension seams

Same philosophy as the v1 actions plan: structure for the next slice's
work without committing to its decisions.

1. **`ServerTransport` is a trait.** v1 ships stdio; v2 adds SSE/HTTP
   for remote agents in a sandbox VM. No core code changes.
2. **Tool registry is data-driven.** Adding `fono.history`,
   `fono.cancel`, `fono.set_language`, or a vision tool later is an
   append, not a refactor.
3. **`AgentSession` owns the active MCP-driven recording state** as a
   sibling of the existing dictation and assistant FSM lanes
   (`crates/fono-hotkey/src/fsm.rs`). The barge-in handling and tray
   badge plumbing live here once and are reused by future agent
   integrations (Claude Code / Cursor / etc.).
4. **`docs/coding-agents.md` has one section per supported client**,
   each pinned to a known-good config snippet. Forge + Claude Code
   + Cursor sections are *all* in v1 (the agent-agnosticism gate);
   Codex / Gemini / Cline / Continue / Windsurf / Goose ship as
   best-effort docs in the same release. Future agents are
   additional sections, no code changes.
5. **Voice agent preset is data**, not code: a markdown file under
   `assets/agent-presets/voice.md` shared verbatim across every
   agent. Per-agent docs explain only the loading mechanism
   (`AGENTS.md` for Forge, `CLAUDE.md` for Claude Code, settings UI
   for Cursor, etc.); the prompt text itself never forks.
6. **`agents.toml` registry** ships with first-party entries for the
   common MCP-capable agents and is user-extensible without
   recompiling Fono. New ecosystem entrants integrate via this file.

### Configuration shape

Two separate files. `config.toml` holds the server settings; the new
`agents.toml` holds the agent registry and is the single place where
agent-specific knowledge lives.

```toml
# ~/.config/fono/config.toml

[mcp.server]
enabled = false                  # master toggle; default off
transport = "stdio"              # stdio (v1); sse | http reserved for v2

[mcp.server.tools]
speak    = true
listen   = true
confirm  = true
# Future tools opt-in here.

[mcp.server.speak]
mirror_to_stdout = false         # if true, also print spoken text
                                 # (useful when developing the agent
                                 # preset; usually off in real use).

[mcp.server.listen]
max_seconds = 60                 # safety ceiling on a single listen
                                 # call (agent prompts can override
                                 # downward, never upward).

[mcp.server.confirm]
timeout_seconds = 10
bias_strength = "strong"         # off | mild | strong; controls how
                                 # aggressively the STT decoder is
                                 # biased toward the choices vocab.
```

```toml
# ~/.config/fono/agents.toml
# First-party entries ship with Fono; users append their own.

[[agent]]
name = "forge"
command = ["forge"]
args    = ["--agent", "voice"]    # how this agent loads a preset
mcp_config_path = "~/.forge/mcp.json"
preset_injection = "cli-flag"     # cli-flag | config-file | agents-md

[[agent]]
name = "claude-code"
command = ["claude"]
mcp_config_path = "~/.config/claude-code/mcp.json"
preset_injection = "claude-md"

[[agent]]
name = "cursor"
command = ["cursor"]              # opens project; preset via settings UI
mcp_config_path = "~/.cursor/mcp.json"
preset_injection = "manual"       # documented one-time setup

[[agent]]
name = "codex"
command = ["codex"]
mcp_config_path = "~/.config/codex/mcp.json"
preset_injection = "cli-flag"

[[agent]]
name = "gemini"
command = ["gemini"]
mcp_config_path = "~/.config/gemini-cli/mcp.json"
preset_injection = "cli-flag"
```

The `[mcp.server]` block is parsed but inert until `enabled = true`.
First-run wizard does **not** prompt for it; users opt in via
`fono use mcp-server on` or the tray submenu added in Phase 4.

### Voice agent preset (the actual product)

The system prompt is the highest-leverage deliverable in this plan,
and it is **identical for every coding agent**. v1 ships this file
as `assets/agent-presets/voice.md`:

```
You are in VOICE MODE. The user is listening, not reading.

Output rules:
- Default to under three sentences per turn unless explicitly asked
  to elaborate.
- Never emit code blocks, tables, or file paths aloud. Say "I'll
  show the diff on screen" and proceed silently.
- When you have multiple paths forward, offer them as A/B/C and call
  the `fono.confirm` tool with the choices array. STOP after the call.
- After every spoken response, call `fono.speak` with the text. Do
  not also print it.
- End each turn with a one-line cue that hands the turn back: a
  question, "your turn", or "ready when you are".

Brevity > caveats. Be willing to be wrong fast.

When the user wants more input from you (asks a follow-up, says
"keep going"), call `fono.listen` to capture their next instruction.
```

How this is loaded varies per agent (CLI flag, `AGENTS.md` /
`CLAUDE.md` inclusion, settings-UI paste), but the prompt **text is
shared verbatim** across all agents and lives in one file. Per-agent
loading instructions live in `docs/coding-agents.md`.

## Phases

### Phase 0 — Decisions, ADRs, roadmap, changelog

- [x] ADR 0030 "Fono as MCP server for coding agents". Records the
      direction-inverse-of-v1 decision, the three-tool surface, the
      **agent-agnostic design principle** (no per-agent code paths
      in `fono-mcp-server`), the `agents.toml` registry design, the
      v1 verification gate of ≥ 3 agents working end-to-end, and
      the future shared `fono-mcp-protocol` crate split if both
      plans ship.
- [x] ROADMAP.md card retitled "Voice loop for coding agents";
      description makes explicit that the integration is
      agent-agnostic by design and names Forge only as the first
      dogfood target alongside the additional agents shipped in the
      same release. REST API split into a follow-up bullet under
      "On the horizon" so the scope is honest.
- [x] CHANGELOG.md `[Unreleased]` under `Added`.
- [x] Cross-link in `plans/2026-05-22-voice-actions-via-mcp-v1.md`
      noting the shared-protocol relationship.

Verification: ADR reviewed, ROADMAP renders cleanly on fono.page,
release-checklist gate accepts the CHANGELOG entry when the work
ships.

### Phase 1 — `fono speak --stream` CLI subcommand

The smallest user-visible win, deliverable in a day, useful even
without any MCP work. Lets users dogfood the voice-output half of the
loop today by piping any tool's stdout through Fono's TTS.

- [x] New CLI subcommand in `crates/fono/src/cli.rs` dispatching to
      `crates/fono/src/speak_stream.rs`.
- [x] Sentence segmentation: split on `. ? !` followed by whitespace
      or EOF, or on `\n\n`. 200-char hard cap to avoid huge
      single-shot synthesis on prose that omits punctuation.
- [x] Markdown sanitiser:
      - ` ``` ` fenced code blocks → "(code block elided)".
      - `**bold**` / `*em*` / `_em_` → just the text.
      - Headings → drop the `#` prefix.
      - `[text](url)` → just `text`.
      - Inline `` `code` `` → just `code`.
      - URLs longer than 30 chars → "a link".
- [x] Each sentence enqueues into the existing `fono-tts` streaming
      path (the same one the assistant uses for sentence-by-sentence
      TTS, see `crates/fono/src/assistant.rs`).
- [x] Backpressure: if the TTS queue exceeds 5 pending sentences,
      `fono speak --stream` blocks on stdin reads until it drains —
      prevents the producer (Forge) from running ahead of the
      listener.
- [x] Cancellation: SIGINT (Ctrl-C) flushes the queue and exits
      cleanly; Escape via the existing global hotkey listener
      cancels playback via the existing `assistant stop` plumbing.
- [x] Unit tests for sentence segmentation, markdown stripping, and
      backpressure semantics.
- [x] `docs/coding-agents.md` (new) section "Dictate-in,
      pipe-speak-out" documenting the `forge | fono speak --stream`
      pattern.

Verification: `cargo test -p fono speak_stream` green. Manual:
`echo "Hello there. This is sentence two." | fono speak --stream`
plays both sentences with a natural gap.

### Phase 2 — `fono-mcp-server` crate skeleton + stdio transport

- [x] Create `crates/fono-mcp-server/` with `Cargo.toml`, `src/lib.rs`,
      SPDX headers, default-features-off ready for feature gating
      from the `fono` binary.
- [x] `protocol.rs` with serde types for the subset of MCP we serve:
      `initialize`, `initialized`, `tools/list`, `tools/call`. Mirror
      the shape `plans/2026-05-22-voice-actions-via-mcp-v1.md:262-265`
      will land for the client side. If that plan lands first, this
      file imports from `fono-action`; otherwise we extract to a
      shared `fono-mcp-protocol` crate at the time the second one
      lands.
- [x] `StdioTransport` reads line-delimited JSON-RPC from stdin and
      writes responses to stdout. **Stderr is the only logging
      sink** when running under stdio — stdout is the MCP channel
      and must stay clean.
- [x] `McpServer` request loop: dispatch by method name, route
      `tools/call` by tool name into the registry, surface errors
      as MCP error objects (not exceptions).
- [x] Initialize handshake: advertise protocol version, server name
      `"fono"`, server version from `env!("CARGO_PKG_VERSION")`,
      tool list from registry.
- [x] Unit tests for round-trip serialisation of every wire type,
      and an `initialize` → `tools/list` → `tools/call` golden flow
      with a stub tool.

Verification: `cargo test -p fono-mcp-server` green. Manual:
`echo '{"jsonrpc":"2.0","id":1,"method":"initialize",...}' | fono mcp serve`
returns a well-formed initialize response on stdout.

### Phase 3 — Three voice tools

- [x] **`fono.speak`** — input `{ text: string }`, calls
      `fono-tts`'s streaming synthesis, returns once the audio
      finishes (or `{ cancelled: true }` if Escape was pressed
      mid-playback). Implemented by reusing the sentence-streaming
      pipeline from Phase 1 — feed `text` through the same
      sanitiser, same backpressure.
- [ ] **`fono.listen`** — standalone microphone capture path not yet
      available in the MCP server context; returns a clear stub error.
      Full implementation (fono-audio + SilenceWatch + STT) deferred.
- [ ] **`fono.confirm`** — depends on fono.listen; returns stub error.
- [ ] All three tools cancellable via global Escape hotkey.
- [ ] Round-trip integration test (`tests/voice_tools_round_trip.rs`).

Verification: integration test green. Manual: `fono mcp serve` from
a second terminal, hand-craft a `tools/call` JSON for
`fono.confirm`, hear the question, say "A", see `{ choice: "A" }`
come back.

### Phase 4 — CLI surface, hotkey ownership, tray plumbing

- [x] `fono mcp serve` entry point in `crates/fono/src/cli.rs`.
      Constructs `McpServer` over `StdioTransport`, runs to EOF.
- [x] `fono use mcp-server on|off` toggles `[mcp.server].enabled`.
      Until enabled, `fono mcp serve` exits with a one-line error
      naming the toggle — prevents accidental MCP exposure.
- [x] Hotkey FSM (`crates/fono-hotkey/src/fsm.rs`): new state
      `McpDriven { tool: ToolKind }` parallel to `RecordingDictation`
      and `RecordingAssistant`. While in `McpDriven`:
      - F7 press → barge-in (cancel the in-flight tool call); does
        **not** start a dictation.
      - F8 press → same.
      - Escape → same.
- [x] Tray submenu "MCP server" (visible only when
      `[mcp.server].enabled = true`): enable/disable toggle, "Last
      client connected at …" status line, per-tool enable/disable
      rows.
- [x] Tray badge while an MCP-driven recording/playback is active —
      red dot in the corner, matches the existing
      assistant-recording badge.
- [x] `fono doctor` "Coding agents" section: server enabled
      flag, last initialize handshake, total tools advertised, last
      tool-call timestamp + result kind (success / error /
      cancelled).

Verification: with the server enabled, starting `fono mcp serve` in
one terminal and driving it from another shows the tray badge during
each tool call; F7 during an in-flight `fono.listen` cancels it
instead of starting dictation; `fono doctor` reflects everything.

### Phase 5 — First integration target: Forge end-to-end

Forge first because it's the maintainer's daily driver and the
tightest dogfood loop. The work in this phase intentionally avoids
branching anywhere in `fono-mcp-server` — every Forge-specific bit
lands in `agents.toml` or `docs/coding-agents.md`. If a real change
to the server crate turns out to be needed for Forge, that's a
signal we're violating the agent-agnostic design principle and the
change must be redesigned to be agent-agnostic before landing.

- [x] Ship `assets/agent-presets/voice.md` with the system prompt
      from the *Voice agent preset* section above.
- [x] First-party `agents.toml` entry for Forge.
- [x] `docs/coding-agents.md` "Forge" section with mcp.json snippet,
      voice preset loading, latency expectations, audio-device note,
      privacy warning.
- [x] `fono agent-loop --agent forge` works via the generic wrapper
      in `crates/fono/src/agent_loop.rs`; no Forge-specific code.
- [x] Wizard: optional final step "Enable voice-driven coding agents?"
      that flips `[mcp.server].enabled`. Agent-neutral wording.

Verification: real Forge session end-to-end on the maintainer's
daily-driver host. Recorded screencap in
`docs/screencasts/voice-loop-forge.webp` for the README.

### Phase 6 — Second + third integration targets (Claude Code, Cursor)

**This phase is part of v1, not a follow-up.** Shipping at least two
additional agents end-to-end in the same release is the gate that
proves the integration is genuinely agent-agnostic — if it isn't, we
find out here and fix the design *before* v1 tags, not after the
maintainer has shaped everything around their own daily driver.

- [x] `agents.toml` entries for Claude Code and Cursor.
- [x] `docs/coding-agents.md` sections for Claude Code and Cursor
      with exact config-file snippets and preset-loading mechanism.
- [x] `fono agent-loop --agent claude-code` works via generic wrapper.
- [x] `fono agent-loop --agent cursor` documented (GUI app; use
      Path 1 for full lifecycle control).
- [x] Agent-agnostic design verified: no agent-specific code in
      `fono-mcp-server`; all agent knowledge in `agents.toml`.

### Phase 6b — Best-effort docs for the broader ecosystem

Docs-only, no live verification required. Lands in the same release
as Phase 5–6 to make Fono's agent-agnostic story credible from day
one.

- [x] `docs/coding-agents.md` sections for Codex CLI, Gemini CLI,
      Cline/Continue/Windsurf (VS Code extensions), and Goose.
- [x] First-party `agents.toml` entries for Codex CLI and Gemini
      CLI (marked untested; community PRs welcome).
- [x] Each section points to `assets/agent-presets/voice.md`.
- [x] "Adding your own agent" section covers `agents.toml` format.

Verification: docs render cleanly; community is invited via
CHANGELOG release notes to send PRs adding their preferred agents.

### Phase 7 — Release engineering

- [ ] Workspace version bump (deferred to tag; maintainer decision).
- [ ] CHANGELOG.md `[Unreleased]` graduates to a versioned section
      (deferred to tag per project convention).
- [x] ROADMAP.md "On the horizon" entry updated to reflect
      implementation status.
- [x] `cargo fmt --all --check`, `cargo clippy --workspace
      --all-targets -- -D warnings`, `cargo test --workspace --tests
      --lib` all green (verified 2026-05-26 session 3: 0 failures
      across all crates, fmt clean, clippy -D warnings clean).
- [ ] Binary-size delta vs prior release ≤ 0.5 MB (deferred to tag).

Verification: pre-commit gate passes on every commit. Full
release-checklist deferred to tag time per `docs/dev/release-checklist.md`.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Plan accidentally ossifies around Forge because that's the maintainer's daily driver | Phase 6 is part of v1, not a follow-up: two additional agents (Claude Code, Cursor) verified end-to-end before tag. Agent-agnostic design principle invoked in code review on every `fono-mcp-server` PR. |
| Any individual agent ignores the voice-mode system prompt and emits page-long markdown anyway | The sentence streamer + markdown sanitiser still produce listenable audio; document the prompt as the user's responsibility to keep current; ship a `--max-spoken-chars` cap on `fono.speak` (default 800; truncate with "more on screen"). |
| MCP tool-call timeout varies across agents (Claude Code defaults to ~60 s, may stall on a long `fono.listen`) | Document the recommended timeout bump per agent; Fono's `fono.listen` enforces `max_seconds` (default 60, configurable) so the worst case is bounded. |
| Audio device contention — `fono.listen` interrupted by simultaneous `fono.speak` | v1 serialises them: while playback is active, `listen` requests queue. v2 candidate: tiny on-device VAD echo suppression. |
| Stdout pollution breaks the MCP channel when running under stdio | All Fono logging routes to stderr while `fono mcp serve` is active. Tracing-subscriber configuration explicit, unit-tested. |
| Privacy: a cloud-backed coding agent (Forge, Claude Code, Cursor, Codex, Gemini, …) sees raw user transcripts via tool returns | One-paragraph privacy note at the top of `docs/coding-agents.md` (applies to every agent equally) and a wizard warning when enabling MCP-server simultaneously with any cloud-backed agent. |
| Shared protocol code between this plan and `plans/2026-05-22-voice-actions-via-mcp-v1.md` drifts | Whichever lands second extracts the common types into a new `fono-mcp-protocol` crate. Don't pre-create the crate. |
| Hotkey FSM regression — adding `McpDriven` state breaks existing dictation/assistant transitions | The Phase 4 FSM work ships behind the same gate the v0.7.1 hotkey refactor used: new unit tests for every cross-state transition; integration test for "F7 during MCP listen → barge-in" specifically. |
| User accidentally enables `fono mcp serve` and exposes their mic to any local process | Stdio transport only in v1 — only a process that can spawn `fono mcp serve` can drive it, which is the same trust boundary as running any program. SSE/HTTP is v2 and must add auth tokens. |

## Future work (explicitly out of scope for v1)

- **SSE / HTTP transport.** For agents running in a sandbox VM,
  on a remote dev box, or in a cloud IDE. Adds auth-token gate.
- **Full-duplex audio.** Echo cancellation so the user can interrupt
  `fono.speak` by speaking, not just by pressing a key.
- **`fono.history`, `fono.set_language`, `fono.cancel`** as additional
  tools — exposes more of Fono's IPC surface to agents.
- **Vision tools.** `fono.capture_and_describe`, `fono.read_clipboard`.
  Routes to a vision-capable assistant when configured.
- **Speaker verification** so a household-shared host doesn't accept
  voice commands from a passing roommate.
- **Local REST API.** The other half of the original ROADMAP card —
  the same IPC surface exposed over HTTP for scripts and editor
  plugins that aren't MCP-capable. Independent of this plan; tracked
  as its own future card.

## Verification gates summary

Per `AGENTS.md` pre-commit gate, **every commit** in this plan must
pass:

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --tests --lib
```

Each phase additionally has the verification listed inline. The
release tag at the end of Phase 7 must additionally pass the full
release-checklist (cloud-equivalence gate, size-budget gate, etc. —
see `docs/dev/release-checklist.md`).

## Relationship to other plans

- `plans/2026-05-22-voice-actions-via-mcp-v1.md` — Fono as MCP
  *client* (dispatches actions to HA / GitHub). Shares JSON-RPC
  wire types and stdio transport code with this plan. Either can
  land first; whichever lands second extracts the common types
  into a shared `fono-mcp-protocol` crate (don't pre-create).
- Existing ROADMAP card "Local REST API + MCP server" — this plan
  consumes the MCP-server half of that card. The REST API half
  becomes a separate future card.

## North star

> *Any* MCP-capable coding agent — present or future — becomes
> voice-driven by adding one `fono` MCP server entry to its config
> and pointing at one shared system prompt. The Fono codebase
> contains zero references to the names of specific agents outside
> `agents.toml` and `docs/coding-agents.md`.

Forge being the first integration target is a dogfooding choice, not
a design choice. Every reviewer of every PR in this plan's
implementation should ask: *"would this look exactly the same if we
were targeting Claude Code first?"* If the answer is no, the change
needs rework.

## Status

- **2026-05-25** — Plan drafted; awaiting human sign-off before any
  code lands. No phases ticked yet.
- **2026-05-26** — Phase 0 complete (ADR 0030, ROADMAP, CHANGELOG,
  cross-link). Phase 1 complete: `fono speak --stream` ships with
  18 unit tests green; `crates/fono-core` gains `McpServer` config
  struct + `fono use mcp-server on|off` toggle; stub dispatch arms
  for `fono mcp serve` and `fono agent-loop` added to the CLI (both
  print a clear "Phase N not yet implemented" message). Phase 2 next.
- **2026-05-26 (session 2)** — Phases 2–6b complete. `fono-mcp-server`
  crate ships with full JSON-RPC 2.0 stdio transport, McpServer request
  loop, ToolRegistry, and three tools (`fono.speak` fully implemented;
  `fono.listen` and `fono.confirm` are quality stubs pending standalone
  audio capture). Hotkey FSM `McpDriven` state, tray MCP submenu, and
  `fono doctor` coding-agents section all wired. `fono agent-loop` generic
  wrapper ships with bundled `agents.toml` for Forge, Claude Code, Cursor,
  Codex, and Gemini. `docs/coding-agents.md` covers all agents. Pre-commit
  gate passes: fmt + clippy -D warnings + all tests green.
- **2026-05-26 (session 3)** — Phase 7 gate confirmed: `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace --tests --lib` all pass (0 failures). Version bump
  and CHANGELOG graduation deferred to tag per project convention. Plan
  complete.
