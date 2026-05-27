# MCP `fono.listen` — Overlay, Silence Parity, Relevance Filter, and Tray Feedback (v6)

## Objective

Same three goals as v5, plus a fourth — **make the tray icon reflect
MCP activity** so the user can tell at a glance when the coding-agent
voice loop is actively talking to them, even when they're not looking
at the chat window:

1. **Visibility** — Fono overlay shown only while the microphone is
   open; full `Recording → Pondering → Hidden` parity with F7
   dictation.
2. **Safety floor** — silence default 10 s; `max_seconds` default
   45 s.
3. **Relevance filter** — discard non-answer transcripts (radio, TV,
   side conversation, prompt-TTS echo) via heuristic + optional LLM
   gate. `Ignoring` overlay state for visual ack. Multi-utterance
   loop bounded by a rejection ceiling.
4. **(NEW) Tray feedback** — daemon's tray icon tints to a dedicated
   `Mcp*` state while the MCP server is listening to or speaking at
   the user. Same channel (Fono IPC) the daemon already uses to
   accept commands from the CLI.

### Diff vs v5

- Keep `relevance_max_rejections` as config (per user decision). The
  hardcoded 1.5 s LLM timeout stays a `const`.
- **Add Slice 7** — tray-state propagation from the MCP server to
  the daemon over IPC.
- **Extend** `fono-ipc` `Request` enum with two new variants
  carrying MCP lifecycle events.
- **Extend** `fono-tray` `TrayState` enum with dedicated MCP states.

Everything else from v5 carries forward unchanged.

## Background — IPC and Tray Wiring Today

Fono already has every piece we need:

- **Daemon IPC** lives in `crates/fono-ipc/src/lib.rs`:
  length-prefixed bincode over Unix sockets at
  `$XDG_STATE_HOME/fono/fono.sock` (or
  `/var/lib/fono/fono.sock` for the system-service install). The
  `Request` enum at `crates/fono-ipc/src/lib.rs:14-61` already
  carries `Toggle`, `HoldPress`, `Cancel`, `AssistantStop`, etc.
  Adding two more variants is a small, well-understood change.
- **Tray state** lives in `crates/fono-tray/src/lib.rs:200-211`
  (`TrayState::{Idle, Recording, Processing, Paused, Assistant}`).
  Each variant maps to an icon tint inside the tray binary. Adding
  one or two more variants follows the existing pattern.
- The MCP server is a child process spawned by the coding agent,
  runs in the same user session as the daemon, and has access to the
  same XDG paths. It can connect to the daemon's socket via
  `fono_ipc::connect_any(&[...])` exactly the way the CLI already
  does (see `connect_any` at
  `crates/fono-ipc/src/lib.rs:161-188`).

So this slice is **all integration, no new primitives**.

## Design — Tray Feedback for MCP

### Two new IPC requests

Add to `crates/fono-ipc/src/lib.rs:14-61`:

```rust
/// MCP server is entering an interactive phase that the user
/// should be visually aware of. Daemon transitions its tray
/// state accordingly. The `phase` discriminates the icon tint.
McpActivityStart { phase: McpPhase },

/// MCP server has finished the interactive phase. Daemon
/// returns the tray to whatever its previous baseline state was
/// (typically `Idle` unless another flow is also active).
McpActivityEnd,
```

…where `McpPhase` is a small enum at the IPC layer:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum McpPhase {
    /// `fono.listen` — microphone is open, waiting for / receiving
    /// the user's spoken reply.
    Listening,
    /// `fono.speak` or the prompt prelude of `fono.listen` — TTS is
    /// playing audio at the user. Optional; only emitted when the
    /// TTS phase is long enough to be worth telegraphing (≥ 1 s
    /// scheduled playback).
    Speaking,
    /// `fono.confirm` — A/B/C question awaiting the user's spoken
    /// choice. Visually distinct so the user knows their attention
    /// is required.
    Confirming,
}
```

These are pure data carriers — no FSM transitions on the IPC layer.
The daemon decides how to interpret them; the MCP server doesn't need
to know about tray internals.

### New TrayState variants

Extend `crates/fono-tray/src/lib.rs:200-211` with **one** new
variant (not three — the user just needs "MCP is talking to me",
not three sub-flavours):

```rust
/// Coding-agent voice loop is active — the `fono mcp` child
/// process is listening, speaking, or asking the user a
/// question. Painted in a dedicated colour so it's
/// distinguishable from dictation (red) and the voice
/// assistant (green) at a glance. Sub-phases (listening /
/// speaking / confirming) collapse into one tray state because
/// the icon doesn't need that level of detail; the overlay
/// carries the fine-grained state.
Mcp = 5,
```

The discriminant `= 5` continues the existing `#[repr(u8)]` numbering.
Renderer in the tray crate gets one new arm in its tint table.

Colour choice — three sensible options (open question, see below).
The plan does **not** commit to one until the user picks; the renderer
PR is a one-line change either way.

### Daemon-side handling

In `crates/fono/src/daemon.rs` (where the IPC `Request` dispatch
lives — find the match arm that handles e.g. `Request::Toggle`):

- `McpActivityStart { phase: _ }` → set tray to `TrayState::Mcp`,
  remember the previous baseline so `McpActivityEnd` can restore it.
  Initial v6 keeps the phase information unused at the tray layer
  but logged at `info` for observability; future iterations could
  introduce sub-icons if user feedback warrants.
- `McpActivityEnd` → restore baseline; if another flow (dictation,
  assistant) is mid-flight, that flow's state wins. Treat overlapping
  starts as last-writer-wins, end matches by stack depth — start a
  counter to handle nested calls (`fono.listen` calling `fono.speak`
  for the prompt → two starts before two ends).

### MCP-server-side emission

In `crates/fono-mcp-server/src/voice_io.rs`:

- `speak_text`: wrap with `mcp_activity_guard(McpPhase::Speaking)` if
  the synthesised audio length ≥ 1 s (cheap to check after
  `tts.synthesize` returns). Skip for short prompts to avoid icon
  flicker.
- `listen_once`: wrap the capture-and-loop block with
  `mcp_activity_guard(McpPhase::Listening)`.

`mcp_activity_guard` is a thin RAII helper analogous to the
`OverlayGuard` from Slice 1: constructs by sending
`McpActivityStart { phase }`; `Drop` impl sends `McpActivityEnd`.
Best-effort — if the daemon isn't running or the socket isn't
reachable, log at `debug` and continue. Tray feedback is a
nice-to-have, not a requirement; MCP listen must keep working
headless or daemon-less.

In `crates/fono-mcp-server/src/tools/confirm.rs` (existing tool —
need to verify the file path; refactor if it's already
`fono.confirm`-shaped): wrap with
`mcp_activity_guard(McpPhase::Confirming)`.

### Why a dedicated tray state rather than reuse?

Three reasons the MCP path deserves its own variant rather than
reusing `Recording` or `Assistant`:

- The user explicitly asked for a way to know the MCP is
  interacting with them. Reusing `Recording` means F7 dictation
  and MCP listen look identical.
- The voice assistant (`TrayState::Assistant`, green) is a
  user-initiated flow (F8 keypress). MCP listen is
  agent-initiated, with potentially different consent
  expectations.
- The tray-state palette is small (5 variants today, 6 with this
  change); one more variant is well within the budget the
  designers had in mind (see
  `docs/decisions/0013-tray-icon-state-palette.md`).

### Headless / daemon-less fallback

The MCP server already needs to work when the daemon isn't running
(coding-agent might launch it standalone). The IPC emission is
purely additive: if `connect_any` fails, the guard logs and
becomes a no-op. The voice loop continues unimpeded.

## Open Question for the User

What colour should `TrayState::Mcp` use? The three sensible options:

- **A — Purple / violet** (e.g. `#A855F7`). Visually distinct from
  red (dictation), green (assistant), amber (processing). Reads as
  "AI / coding agent" in most colour conventions.
- **B — Blue / cyan** (e.g. `#0EA5E9`). Calm, "system-level"
  vibe; matches the `AssistantSpeaking` overlay tint already in use
  for TTS playback, so users who already know that signal will read
  it as "something machine-driven is happening."
- **C — Reuse one of the existing tints** (probably amber, the
  `Processing` colour). Argument: don't grow the palette unless
  necessary; "MCP is talking to me" is conceptually close to
  "processing." Argument against: amber today means "STT/LLM is
  running"; overloading it weakens the existing signal.

This plan stays open on the choice. Slice 7 will land whichever the
user picks; the colour token will live in `fono-tray`'s renderer
constants alongside the existing ones.

## Implementation Plan

### Slice 0 — Configuration plumbing

(Unchanged from v5.)

- [ ] Task 0.1. `fono-overlay` + `fono-polish` as deps of
  `fono-mcp-server`. Plus `fono-ipc` (new in v6) for the tray
  activity messages.
- [ ] Task 0.2. `MCP_LISTEN_DEFAULT_AUTO_STOP_MS = 10_000`.
- [ ] Task 0.3. `DEFAULT_MAX_SECONDS = 45` and
  `listen_max_seconds = 45`.
- [ ] Task 0.4. Add `McpServer.relevance_filter` (config) and
  `McpServer.relevance_max_rejections` (config; **kept** per
  user decision). Do **not** add `relevance_llm_timeout_ms`.
- [ ] Task 0.5. Update the unit tests at
  `crates/fono-mcp-server/src/voice_io.rs:384-394`.

### Slice 1 — Overlay spawn scoped to listen phase

(Unchanged from v5.)

### Slice 2 — Pondering visual + commit always-on

(Unchanged from v5.)

### Slice 3 — Multi-utterance listen loop scaffolding

(Unchanged from v5.)

### Slice 4 — LLM relevance classifier

(Unchanged from v5; timeout stays a `const` in `relevance.rs`.)

### Slice 5 — Overlay vocabulary for "ignored / waiting"

(Unchanged from v5.)

### Slice 6 — Daemon co-existence (auto-detect)

(Unchanged from v5. Note: the new IPC-based daemon-presence probe
in Slice 7 supersedes the pid-lock probe sketched in v5 — IPC
ping is more reliable.)

### Slice 7 — Tray feedback over IPC

- [ ] Task 7.1. Extend `crates/fono-ipc/src/lib.rs`:
  - Add the `McpPhase` enum.
  - Add `Request::McpActivityStart { phase }` and
    `Request::McpActivityEnd` variants. Update the doc-comments
    listing existing variants.
  - Update existing serde-roundtrip tests in `fono-ipc` to cover
    the new variants.
- [ ] Task 7.2. Extend `crates/fono-tray/src/lib.rs:200-211`:
  - Add `TrayState::Mcp = 5` with the doc-comment from Design.
  - Update the tint dispatch in the renderer (search for
    `TrayState::Assistant` to find the existing arms).
  - Update `Tray::state()` round-trip at
    `crates/fono-tray/src/lib.rs:317-323` to handle the new
    discriminant.
- [ ] Task 7.3. Daemon dispatch in `crates/fono/src/daemon.rs`:
  - Match arm for `Request::McpActivityStart`: increment an
    `mcp_activity_depth: u32` field on the daemon state; if it
    transitions from 0 → 1, snapshot the current tray state into
    `mcp_baseline_state: TrayState` and set tray to
    `TrayState::Mcp`. Log `phase` at `info`.
  - Match arm for `Request::McpActivityEnd`: decrement the counter;
    if it returns to 0, restore tray to `mcp_baseline_state`
    (unless another flow has set a new state in the interim — the
    daemon's existing tray-state writers win).
  - Reply `Response::Ok` in both cases; this is fire-and-forget
    from the MCP side but a real ack helps debugging.
- [ ] Task 7.4. `crates/fono-mcp-server/src/voice_io.rs` —
  introduce `struct McpActivityGuard` with:
  - Constructor: best-effort `tokio::spawn` of a
    `fono_ipc::request_any` carrying
    `McpActivityStart { phase }`. Failures debug-log; do not
    propagate.
  - `Drop` impl: fire-and-forget `McpActivityEnd`. Best-effort.
- [ ] Task 7.5. Wrap call sites:
  - `listen_once` body: hold an
    `McpActivityGuard::new(McpPhase::Listening)` for the
    duration. Drop on every exit path (RAII).
  - `speak_text`: only if synthesised audio ≥ 1 s, hold an
    `McpActivityGuard::new(McpPhase::Speaking)`.
  - `confirm` tool: hold an
    `McpActivityGuard::new(McpPhase::Confirming)` for the
    listen-and-match span.
- [ ] Task 7.6. Unit tests:
  - `fono-ipc` round-trip for the two new variants.
  - `fono-tray` discriminant round-trip for `Mcp`.
  - `fono` daemon: feed `McpActivityStart` + `McpActivityEnd`
    via a mock socket and assert the tray state transitions
    `Idle → Mcp → Idle`. Nest two starts and assert the depth
    counter handles them correctly.
- [ ] Task 7.7. Integration smoke (deferred to verification, not
  automated): start `fono` (the daemon) + `fono mcp serve`; trigger a
  `fono.listen`; visually confirm the tray icon flips to the
  chosen `Mcp` tint and back.

### Slice 8 — Documentation and changelog

(Renumbered from v5's Slice 7.)

- [ ] Task 8.1. `docs/coding-agents.md`: overlay scope, silence
  defaults, relevance filter, **tray feedback** ("the tray icon
  flips to a dedicated colour while the coding agent is asking
  you a question or speaking; restores afterwards").
- [ ] Task 8.2. `docs/configuration.md`: document
  `[mcp].relevance_filter` and `[mcp].relevance_max_rejections`.
  Mention the hardcoded 1.5 s LLM timeout and the tray feedback
  (no config knob for the latter; behaviour is automatic when the
  daemon is running).
- [ ] Task 8.3. Update
  `docs/decisions/0013-tray-icon-state-palette.md` to reflect the
  new `Mcp` state and its chosen colour. ADR amendment, not a new
  ADR.
- [ ] Task 8.4. CHANGELOG entries:
  - `## Added`: overlay during MCP listen.
  - `## Added`: relevance filter + `context` argument.
  - `## Added`: tray icon reflects MCP activity (listening /
    speaking / confirming).
  - `## Changed`: silence default 10 s; `max_seconds` default
    45 s.
- [ ] Task 8.5. `assets/agent-presets/voice.md`: teach the agent
  to pass `context` on every `fono.listen`. Mention that
  `fono.confirm` is the right tool for any A/B/C decision the
  agent has to make and that it will flash the tray + overlay so
  the user knows their attention is needed.
- [ ] Task 8.6. Pre-commit gate.

## Verification Criteria

(v5 list + four new entries for the tray feature.)

- (carried over) overlay timing, silence default, heuristic
  rejections, LLM accept/reject mapping, fail-open timeout,
  `Ignoring` flash, rejection ceiling, daemon co-existence,
  headless fallback, lint/test gate.
- **NEW** Tray icon flips to the `Mcp` tint within 200 ms of a
  `fono.listen` call starting and back within 200 ms of it
  ending.
- **NEW** Nested calls (e.g. `fono.listen` with a prompt that
  triggers `Speaking`) leave the tray in `Mcp` throughout, not
  flickering between states.
- **NEW** When the daemon isn't running, `fono.listen` still
  works end-to-end; only the tray-update side-effect is missing
  and a single `debug`-level "ipc not reachable" log is emitted.
- **NEW** After an MCP-active span ends, the tray restores the
  state the daemon had before (e.g. if dictation was somehow
  mid-flight, `Recording` is restored — not overwritten to
  `Idle`).

## Potential Risks and Mitigations

(v5 list + three new.)

(unchanged) misclassification, latency, cost, privacy, missing
context, overlay misread, echo false positives, daemon-probe
flakiness, slow user backend.

10. **(NEW) Tray icon flickers on short prompt-TTS phases.**
    Mitigation: only emit `McpPhase::Speaking` when synthesised
    audio is ≥ 1 s; short prompts stay silent at the tray layer.

11. **(NEW) Nested `McpActivityStart` from `fono.listen` calling
    `speak_text` for the prompt produces unbalanced
    starts/ends.** Mitigation: depth counter on the daemon side
    (Task 7.3) handles arbitrary nesting; RAII guards on the MCP
    side ensure each start has a matching end even on early
    return / panic.

12. **(NEW) MCP server can spam the daemon with activity
    requests if a misbehaving agent invokes tools in a tight
    loop.** Mitigation: each request is a single bincode frame
    over Unix socket — cost is negligible. If we ever see it
    matter, add a 50 ms debounce in the guard's constructor.

## Alternative Approaches

(v5 list + two new.)

(unchanged 1–7).

8. **(NEW) Reuse `TrayState::Assistant` for MCP instead of a
   dedicated state.** Rejected per Design rationale — the
   user-initiated assistant flow and agent-initiated MCP flow
   carry different consent and visibility semantics.

9. **(NEW) Drive the tray directly from the MCP server without
   IPC** (e.g. spawn a second tray icon). Rejected: two trays in
   the system tray would be ugly and confusing; the daemon
   already owns the tray, IPC is the right pattern.

## Status

- 2026-05-26 — v1 opened.
- 2026-05-26 — v2: overlay scoped; 5 s safety floor.
- 2026-05-26 — v3: 10 s safety floor; relevance filter; multi-
  utterance loop; `Ignoring` overlay state.
- 2026-05-26 — v4: drop `drive_overlay` knob.
- 2026-05-26 — v5: drop `relevance_llm_timeout_ms`; hardcoded
  `const`.
- 2026-05-26 — v6 (this revision): keep `relevance_max_rejections`
  as config (per user decision); add **tray feedback over IPC**
  as a new slice — MCP server emits `McpActivityStart/End` to the
  daemon, which flips the tray to a dedicated `TrayState::Mcp`
  for the duration. Colour for the new state is an open question
  routed via `fono.confirm`.
