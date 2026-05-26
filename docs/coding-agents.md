# Voice-driven coding agents


## Quick setup (one command)

```sh
fono agent-setup forge          # or claude-code, cursor, codex, gemini
```

This single command does three idempotent steps in sequence:

| Step | Action |
|---|---|
| 0/3 Agent check | Verifies the agent's launcher is on `$PATH`. If missing **and** the registry has an `install_command` for it (e.g. `curl -fsSL https://forgecode.dev/cli \| sh` for Forge, `curl -fsSL https://claude.ai/install.sh | bash` for Claude Code), prompts before running the installer. Bails with manual-install instructions when no install command is registered (e.g. Cursor — a GUI editor). Skipped in `--dry-run` (prints what would happen). |
| 1/3 MCP server | Sets `mcp.enabled = true` in `~/.config/fono/config.toml`. No-op if already on. |
| 2/3 Agent MCP JSON | Merges `"mcpServers": { "fono": { "command": "fono", "args": ["mcp", "serve"] } }` into the agent's JSON file (e.g. `~/.forge/.mcp.json`). Other entries are untouched. |
| 3/3 Voice preset | Appends the shared voice-mode system prompt to `AGENTS.md` / `CLAUDE.md` in the current directory. Guarded by a sentinel — running twice never double-injects. |

Running it again is always safe.

```
Setting up Fono voice integration for forge…

  [1/3] MCP server        fono use mcp-server on             ✓ already configured
  [2/3] Agent MCP JSON    ~/.forge/.mcp.json                 ✓ written
  [3/3] Voice preset      AGENTS.md                          ✓ written

Done. Start a voice session by launching `forge` the way you normally do.
```

**Flags:**

| Flag | Purpose |
|---|---|
| `--dry-run` | Preview all changes without writing anything. |
| `--project-dir <path>` | Override the directory for preset-file injection (default: `.`). |
| `--list` | Print all registered agents and exit (no agent name required). |

**List agents:**

```sh
fono agent-setup --list
```
```
AGENT          MCP CONFIG                     PRESET       COMMAND
forge          ~/.forge/.mcp.json             agents-md    forge
claude-code    ~/.claude.json                 claude-md    claude
cursor         ~/.cursor/mcp.json             none         cursor
codex          ~/.codex/mcp.json              none         codex
gemini         ~/.gemini/settings.json        none         gemini
```

---


MCP-capable coding agent can use it for speech input and audio output. The three tools
are:

| Tool | Purpose |
|---|---|
| `fono.speak { text }` | Synthesise `text` and block until audio finishes. |
| `fono.listen { prompt?, max_seconds? }` | Optionally speak `prompt`, then record until silence; returns transcript. |
| `fono.confirm { question, choices, timeout_seconds? }` | Speak question + choices, listen for a spoken A/B/C answer; returns matched choice or `timeout`. |

The integration is **agent-agnostic**: the same `fono mcp serve` process serves every
agent. There is no per-agent code in Fono. Adding a new agent means adding a config
snippet — no Fono changes needed.

---

## Dictate-in, pipe-speak-out

Before or alongside the MCP server, you can use `fono speak stream` as a
pipe to give any tool a voice. It reads from stdin, sanitises markdown, sentence-splits
the text, and speaks each sentence through your configured TTS backend.

```sh
# Speak any command's output
echo "Hello there. This is sentence two." | fono speak stream

# Stream Forge's response audio as it writes
forge | fono speak stream

# Claude Code, Gemini, or any other CLI tool
claude | fono speak stream
gemini | fono speak stream
```

**Requirements:**

- A TTS backend must be configured: `fono use tts openai` (or `wyoming`, `piper`,
  `groq`, `cartesia`, `deepgram`). Without a backend the command exits with a clear
  error message.
- The daemon does **not** need to be running for `fono speak stream` — it is
  completely standalone.

**What the markdown sanitiser does:**

The sanitiser strips agent output before it reaches your ears:

- Fenced code blocks (` ``` `…` ``` `) → `"(code block elided)"`.
- `**bold**` / `__bold__` / `*em*` / `_em_` → plain text.
- ATX headings (`## …`) → drop the `#` prefix.
- `[text](url)` Markdown links → keep only `text`.
- Inline `` `code` `` → keep only `code`.
- URLs longer than 30 characters → `"a link"`.

**Backpressure:** at most 5 sentences are queued for synthesis at a time. When the
queue is full, stdin stalls until the synthesiser drains a slot, so a fast-writing
agent can never run arbitrarily far ahead of the listener.

**Cancellation:** Ctrl-C flushes the queue and exits cleanly.

---

## MCP server setup

### Enable the server

```sh
fono use mcp-server on
```

This sets `[mcp.server] enabled = true` in your config. Until this flag is on,
`fono mcp serve` exits immediately with an error explaining the toggle — the flag is
the safety gate that prevents any local process from calling your microphone via MCP.

### Latency expectations

- **`fono.speak`** — TTS synthesis: ~200 ms (local Piper/Wyoming) or ~400 ms (cloud
  OpenAI/Cartesia). Audio plays while the synthesis streams, so first audio is heard
  sooner.
- **`fono.listen`** — depends on your silence-detection settings
  (`auto_stop_silence_ms` in your config). Typical turn: 2–10 seconds.
- **`fono.confirm`** — speak + listen; roughly `fono.speak` latency + 3–5 s for the
  response.

Recommended: bump your agent's MCP tool timeout to at least 120 seconds so a long
dictation doesn't time out mid-sentence.

### What you see and hear during an MCP voice turn

When the coding agent calls `fono.listen`, `fono.speak`, or `fono.confirm`,
Fono provides three concurrent feedback channels so it never feels like
your microphone has been hijacked silently:

- **Fono overlay panel.** During `fono.listen` the same on-screen panel
  used by F7 dictation paints `Recording`, then `Pondering` once you
  stop talking, then disappears when the silence-watch state machine
  commits. The panel is scoped to the actual microphone-open phase —
  it does **not** appear during prompt TTS — so it lights up only
  when Fono is genuinely listening to you. If the daemon is already
  running, the daemon's overlay is used; otherwise `fono mcp serve`
  spawns its own panel.
- **Tray icon turns amber** for the entire duration of the
  interaction (`fono.listen` + `fono.speak` + `fono.confirm`).
  Amber is the same colour Fono uses while STT or polish is running
  during dictation — palette reuse is intentional; the overlay
  carries the precise sub-state ("RECORDING" / "IGNORED" /
  "CONFIRMING"). The previous tray state is restored when the call
  ends. Nested spans — e.g. `fono.listen` speaking its prompt
  before recording — keep the icon steady (no flicker).
- **Audio cue.** `fono.speak` and the prompt prelude of
  `fono.listen` are heard through your speakers as normal.

### Silence defaults

- **`fono.listen` silence default: 10 s.** The MCP listen loop uses
  a deliberately generous silence floor so the user can pause for
  thought without being cut off mid-sentence. Override via the
  user's existing `[audio].auto_stop_silence_ms`; the MCP listen
  path honours it when set, otherwise falls back to 10 s.
- **Default `max_seconds`: 45 s** per `fono.listen` call. Coupled
  with the multi-utterance loop (see below) this gives a responsive
  turn-taking budget without stranding the user.

### Relevance filter

`fono.listen` accepts an optional `context` argument describing the
kind of answer expected (e.g. `"asking the user for their favourite
colour"` or the question text itself). When `[mcp].relevance_filter`
is on (default `"heuristic"`), each captured utterance is scored;
clear background speech (radio, TV, side conversation, prompt-TTS
echo) is dropped, the overlay flashes the `Ignoring` state for
~700 ms, and the loop re-arms for a fresh capture. The loop bails
out after `[mcp].relevance_max_rejections` rejections (default `2`)
and returns the most recent transcript so the agent is never
stranded.

Modes:

| Mode | Behaviour |
|---|---|
| `"off"` | Filter disabled — every transcript is returned. |
| `"heuristic"` (default) | Length, filler-only, and prompt-echo rules only. Cheap, deterministic. |
| `"llm"` | Heuristic gate first, then the configured polish backend as a one-shot classifier. Hardcoded 1.5 s timeout; on timeout / parse failure the filter fails open and accepts the utterance. |

The `rejected_count` field in the tool reply surfaces how many
utterances the filter dropped before the returned one was accepted.

### Voice-mode system prompt

The prompt in `assets/agent-presets/voice.md` (shipped with Fono) tells the agent to
keep responses short and audible. It is **identical for every agent** — the per-agent
sections below only explain how to load it. Content:

```
You are in VOICE MODE. The user is listening AND has the chat
window visible on screen. Treat the two channels differently.

Two channels, one turn:
- **Spoken channel (`fono.speak`)**: short, conversational, the way
  you'd actually talk. One to three sentences. No lists read aloud,
  no paths, no command names spelled out, no "firstly / secondly".
  Contractions are fine. If something is long or technical, say
  "details are on screen" and stop.
- **Written channel (the chat reply)**: the place for the full
  detail — file paths, command output summaries, next-step lists,
  diffs-by-reference. The user reads this when they want depth.

Rules:
- EVERY turn — including the very first reply of a session — MUST
  call `fono.speak`. No exceptions: greetings, acknowledgements,
  and "I'm here" responses all go through `fono.speak`. If you do
  not call `fono.speak`, the user hears nothing.
- The spoken text and the written text are NOT the same string.
  Speak the conversational summary; write the detailed version in
  the chat reply. Never paste the written reply verbatim into
  `fono.speak` — that produces stilted, read-aloud prose.
- Never speak code blocks, tables, file paths, or long identifiers.
  Refer to them as "the preset file" or "the AGENTS doc" out loud;
  put the exact path in the written reply.
- When you have multiple paths forward, offer them as A/B/C and
  call the `fono.confirm` tool with the choices array. Prefer
  `fono.confirm` over a free-form `fono.listen` whenever the
  decision is bounded — it's faster for the user, the spoken
  answer maps cleanly to one of the labels, and Fono flashes both
  the overlay and the tray so the user knows you're waiting on
  them. STOP after the call.
- When you DO need a free-form answer via `fono.listen`, ALWAYS
  pass a `context` argument describing the kind of answer you're
  expecting — e.g. the question text itself, or
  `"asking the user for their favourite colour"`. Fono uses this
  to filter out background speech (radio, TV, side conversation)
  so an unrelated voice in the room doesn't get fed back to you
  as the user's reply. Skipping `context` works but degrades the
  filter to the cheap heuristic-only path.
- End each spoken turn with a one-line cue that hands the turn
  back: a question, "your turn", or "ready when you are".

Brevity > caveats. Be willing to be wrong fast.

When the user wants more input from you (asks a follow-up, says
"keep going"), call `fono.listen` to capture their next
instruction.
```

---

## Forge

Add to `~/.forge/.mcp.json`:

```json
{
  "mcpServers": {
    "fono": {
      "command": "fono",
      "args": ["mcp", "serve"],
      "transport": "stdio"
    }
  }
}
```

**Load the voice preset.** Copy `assets/agent-presets/voice.md` into your project's
`AGENTS.md` (or append it to the existing file). Forge reads `AGENTS.md`
automatically on every session. `fono agent-setup forge` does this for you.

**Audio-device note:** `fono.listen` and `fono.speak` are serialised in v1 — they will
not run simultaneously. Full-duplex is planned for v2.

**Privacy note:** if you use a cloud Forge backend, your voice transcripts will be sent
to the backend provider along with the rest of your session. See the notice at the top
of this page.

---

## Claude Code

Add to `~/.claude.json` (Claude Code stores MCP servers in this single file at the
root of your home directory, alongside its other settings — `fono agent-setup`
merges in the `mcpServers.fono` entry without touching anything else):

```json
{
  "mcpServers": {
    "fono": {
      "command": "fono",
      "args": ["mcp", "serve"],
      "transport": "stdio"
    }
  }
}
```

**Load the voice preset:** copy `assets/agent-presets/voice.md` into your project's
`CLAUDE.md` (or append it). Claude Code reads `CLAUDE.md` at the start of each session.
`fono agent-setup claude-code` does this for you.

---

## Cursor

Go to **Settings → Features → MCP Servers → Add Server**. Set the command to
`fono mcp serve` with stdio transport.

Alternatively, add to `~/.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "fono": {
      "command": "fono",
      "args": ["mcp", "serve"]
    }
  }
}
```

**Load the voice preset:** paste the contents of `assets/agent-presets/voice.md` into
your Cursor system-prompt settings (Settings → General → Rules for AI).

---

## Codex CLI

> **Not yet supported by `fono agent-setup`.** Codex stores MCP servers in
> `~/.codex/config.toml` as `[mcp_servers.<name>]` TOML tables, not JSON. The
> agent registry still lists `~/.codex/mcp.json` for documentation, but the
> automated setup will not produce a working Codex config — paste the snippet
> manually for now. Tracked for a future release.

Add to `~/.codex/config.toml`:

```toml
[mcp_servers.fono]
command = "fono"
args = ["mcp", "serve"]
```

Load the voice preset with `--instructions` or via your `codex` project config. See
Codex CLI docs for the exact flag name (varies across versions).

---

## Gemini CLI

Add to `~/.gemini/settings.json` (Gemini CLI reads `mcpServers` from its main
settings file; `fono agent-setup` merges the entry in without touching the
other keys):

```json
{
  "mcpServers": {
    "fono": {
      "command": "fono",
      "args": ["mcp", "serve"]
    }
  }
}
```

Load the voice preset by passing it as a system instruction. See Gemini CLI docs for
the exact mechanism.

---

## Cline / Continue / Windsurf (VS Code extensions)

All three extensions support MCP servers. Configure via the extension's settings panel
(usually a JSON config or a GUI "Add MCP server" form). Point the command at
`fono mcp serve` with stdio transport.

Paste the contents of `assets/agent-presets/voice.md` into the extension's system
prompt field.

---

## Goose

Goose is MCP-native and provides a clean reference integration. Add a `fono` server
entry to Goose's MCP config (typically `~/.config/goose/mcp.yaml` or equivalent):

```yaml
servers:
  fono:
    command: fono mcp serve
    transport: stdio
```

Load the voice preset as a Goose extension or system instruction.

---

## Adding your own agent

Any MCP-capable CLI tool can be integrated by:

1. Adding the `fono mcp serve` entry to the tool's MCP config file.
2. Loading `assets/agent-presets/voice.md` as a system prompt (or prepending it to
   your prompt file).
3. Optionally adding an entry to `~/.config/fono/agents.toml` so
   `fono agent-setup my-agent` can configure it for you:

```toml
[[agent]]
name = "my-agent"
command = ["my-agent-cli"]
args    = []
mcp_config_path = "~/.config/my-agent/mcp.json"
preset_injection = "manual"   # cli-flag | config-file | agents-md | claude-md | manual
```

PRs adding first-party entries for new agents are welcome — see `CONTRIBUTING.md`.

---

## Quick reference

```sh
# One-shot setup for a known agent
fono agent-setup forge
fono agent-setup claude-code

# Check MCP server status
fono doctor | grep -A 6 "Coding agents"
```

After `fono agent-setup` finishes, launch your agent the way you normally would
(`forge`, `claude`, etc.). Adding a new agent is a `~/.config/fono/agents.toml`
edit, not a Fono code change.
