# MCP `fono.listen` — Overlay, Silence Parity, Relevance Filter, and Tray Feedback (v7 — final pre-implementation)

## Status: Completed (Slices 0–8 landed; Task 6.3 IPC overlay forwarding was a stretch goal, not pursued)

## Objective

Same four goals as v6 with the tray colour decided:

1. **Visibility** — Fono overlay shown only while the microphone is
   open; full `Recording → Pondering → Hidden` parity with F7
   dictation.
2. **Safety floor** — silence default 10 s; `max_seconds` default
   45 s.
3. **Relevance filter** — discard non-answer transcripts via
   heuristic + optional LLM gate. `Ignoring` overlay state for
   visual ack. Multi-utterance loop bounded by a rejection ceiling.
4. **Tray feedback** — daemon's tray icon reflects MCP activity by
   reusing the existing `TrayState::Processing` (amber) tint, no
   new tray-state variant.

### Diff vs v6

- **Tray colour decision: C — reuse amber** (the existing
  `TrayState::Processing` tint). Don't add `TrayState::Mcp = 5`;
  don't grow the palette. Map `Request::McpActivityStart` →
  `TrayState::Processing` directly.
- **Simplification** — Slice 7 shrinks: no new tray-state variant,
  no renderer arm, no ADR amendment. Just two IPC requests and the
  daemon-side dispatch.
- **Acknowledged trade-off** — amber today carries the "STT or polish
  is running" meaning (dictation post-release). When the MCP server
  is talking to the user, the icon will read the same way. This is
  acceptable given the user's explicit preference to keep the palette
  small; the overlay carries the fine-grained "listening" /
  "speaking" / "confirming" semantics for users who want detail.

Everything else from v6 carries forward unchanged.

## Background

(See v6 for the IPC / tray / MCP wiring tour. Unchanged.)

Key facts that drove the v7 decision:

- `crates/fono-tray/src/lib.rs:200-211` — five `TrayState` variants
  today: `Idle`, `Recording` (red), `Processing` (amber),
  `Paused`, `Assistant` (green).
- `docs/decisions/0013-tray-icon-state-palette.md` — the existing
  ADR explicitly limits the palette to keep the icon legible at
  16px / 22px in cluttered system trays. Reusing `Processing` is in
  keeping with that spirit.

## Design — Tray Feedback (Final)

### IPC additions (unchanged from v6)

Add to `crates/fono-ipc/src/lib.rs:14-61`:

```rust
McpActivityStart { phase: McpPhase },
McpActivityEnd,

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum McpPhase {
    Listening,
    Speaking,
    Confirming,
}
```

The `phase` field is kept in the wire format even though all three
phases map to the same tray tint in v7. Rationale:

- Logged at `info` on the daemon side for observability ("user X
  saw the tray amber for 12 s; the breakdown was 3 s listening +
  9 s confirming"). Useful for debugging missed-cue reports.
- Future-proofing — if the colour decision is ever revisited (e.g.
  if amber proves too overloaded in practice), the field is already
  there; the daemon switches the mapping in one place.

### Daemon-side handling

In `crates/fono/src/daemon.rs`:

- `McpActivityStart { phase: _ }` →
  - Increment `mcp_activity_depth: u32`.
  - On 0 → 1: snapshot the current tray state into
    `mcp_baseline_state` and set the tray to
    `TrayState::Processing`. Log the `phase` at `info`.
- `McpActivityEnd` →
  - Decrement counter.
  - On → 0: restore `mcp_baseline_state` unless another tray
    writer has taken over in the meantime (last-writer-wins on
    the tray channel; depth-counter on the MCP side).
- Reply `Response::Ok` in both arms.

### MCP-server-side emission

Same `McpActivityGuard` RAII pattern as v6:

- `listen_once` holds an
  `McpActivityGuard::new(McpPhase::Listening)`.
- `speak_text` holds an `McpActivityGuard::new(McpPhase::Speaking)`
  iff synthesised audio length ≥ 1 s.
- `confirm` tool holds an
  `McpActivityGuard::new(McpPhase::Confirming)` for the
  listen-and-match span.

Guards best-effort: `connect_any` failure → debug-log + no-op. MCP
listen continues even when daemon is unreachable.

### What v7 does NOT add

- ❌ `TrayState::Mcp` variant.
- ❌ New renderer arm in `fono-tray`.
- ❌ Renderer unit test for the new discriminant.
- ❌ ADR amendment to `docs/decisions/0013-tray-icon-state-palette.md`.

The `TrayState` enum stays at exactly the five variants it has
today.

## Implementation Plan

### Slice 0 — Configuration plumbing

- [x] Task 0.1. `fono-overlay` + `fono-polish` + `fono-ipc` as deps
  of `fono-mcp-server` in `crates/fono-mcp-server/Cargo.toml`.
- [x] Task 0.2. `MCP_LISTEN_DEFAULT_AUTO_STOP_MS = 10_000` at
  `crates/fono-mcp-server/src/voice_io.rs:41`.
- [x] Task 0.3. `DEFAULT_MAX_SECONDS = 45` at
  `crates/fono-mcp-server/src/tools/listen.rs:22` and
  `McpServer::listen_max_seconds = 45` at
  `crates/fono-core/src/config.rs:1031`.
- [x] Task 0.4. `McpServer.relevance_filter`
  (`"off" | "heuristic" | "llm"`, default `"heuristic"`) and
  `McpServer.relevance_max_rejections` (`u32`, default `2`)
  added to `crates/fono-core/src/config.rs`.
- [x] Task 0.5. Unit test at
  `crates/fono-mcp-server/src/voice_io.rs:397-402` asserts the
  10_000 ms default.

### Slice 1 — Overlay spawn scoped to listen phase

- [x] Task 1.1. Spawn `Option<OverlayHandle>` inside `listen_once`,
  unconditionally (Slice 6 adds the daemon-presence skip). Silently
  degrade on spawn error.
- [x] Task 1.2. `set_waveform_style` / `set_volume_bar` from
  `cfg.overlay`, then `Recording { db: 0 }`.
- [x] Task 1.3. Push live samples + gate metrics from the capture
  forwarder.
- [x] Task 1.4. RAII `OverlayGuard` whose `Drop` hides the panel.
- [x] Task 1.5. Comment in `tools/listen.rs` noting the overlay does
  **not** run during prompt-TTS, citing this plan.

### Slice 2 — Pondering visual + commit always-on

- [x] Task 2.1. Three-arm `SilenceEvent` match.
- [x] Task 2.2. `walk_progress` against `effective_silence_ms`.
- [x] Task 2.3. Unit smoke on synthetic transitions.

### Slice 3 — Multi-utterance listen loop scaffolding

- [x] Task 3.1. Refactor `listen_once` into an inner `capture_one`
  closure wrapped in an outer loop.
- [x] Task 3.2. Plumb `context: Option<String>` from the tool entry
  point; advertise in JSON Schema at `tools/listen.rs`.
- [x] Task 3.3. `RelevanceVerdict` enum and stub
  `evaluate_relevance` with heuristics only.
- [x] Task 3.4. Loop wiring: accept → break; reject → flash overlay,
  drop PCM, re-arm `SilenceWatch`, loop. Termination guards:
  rejection count + cumulative wall-clock > `max_seconds × 1.5`.
- [x] Task 3.5. `ListenOutcome.rejected_count` surfaced in the
  protocol body.

### Slice 4 — LLM relevance classifier

- [x] Task 4.1. New module
  `crates/fono-mcp-server/src/relevance.rs` with the classifier
  prompt template **and** the hardcoded timeout constant
  `const RELEVANCE_LLM_TIMEOUT_MS: u64 = 1_500;`.
- [x] Task 4.2. Lazy polish-backend construction inside
  `evaluate_relevance`; cache on `McpContext`.
- [x] Task 4.3. Wrap the LLM call in `tokio::time::timeout`; fail
  open on timeout / error / parse failure.
- [x] Task 4.4. Parse first whitespace-trimmed uppercase token →
  `ANSWER | BACKGROUND | UNSURE`. Map `BACKGROUND` → reject.
- [x] Task 4.5. Unit tests: heuristic rejections; LLM-stub
  classifications; timeout fail-open via a slow stub backend.
- [x] Task 4.6. Doc-comments on `relevance.rs`.

### Slice 5 — Overlay vocabulary for "ignored / waiting"

- [x] Task 5.1. Add `OverlayState::Ignoring { reason }` +
  `IgnoreReason` enum.
- [x] Task 5.2. Renderer dispatch: label `"IGNORED"`, neutral grey
  accent, VU bar hidden.
- [x] Task 5.3. Flash for 700 ms after each rejection, then revert.
- [x] Task 5.4. Renderer unit tests for the new state.

### Slice 6 — Daemon co-existence (auto-detect)

- [x] Task 6.1. Best-effort daemon-presence probe (IPC ping).
- [x] Task 6.2. Skip local overlay spawn when daemon is alive.
- [ ] Task 6.3. (Stretch) IPC overlay forwarding to the daemon.

### Slice 7 — Tray feedback over IPC (simplified)

- [x] Task 7.1. Extend `crates/fono-ipc/src/lib.rs`:
  - Add `McpPhase` enum (`Listening`, `Speaking`, `Confirming`).
  - Add `Request::McpActivityStart { phase: McpPhase }` and
    `Request::McpActivityEnd` variants.
  - Update doc-comments listing existing variants.
  - Extend any existing serde-roundtrip tests in `fono-ipc` to
    cover the new variants.
- [x] Task 7.2. **Skipped** — no `fono-tray` changes in v7.
- [x] Task 7.3. Daemon dispatch in `crates/fono/src/daemon.rs`:
  - Add `mcp_activity_depth: u32` and
    `mcp_baseline_state: TrayState` fields to the daemon state
    struct.
  - Match arm for `Request::McpActivityStart`: increment depth;
    on 0→1, snapshot baseline and call
    `tray.set_state(TrayState::Processing)`. Log `phase` at
    `info`.
  - Match arm for `Request::McpActivityEnd`: decrement depth;
    on →0, restore baseline (skip if another writer has changed
    the state in the interim).
  - Reply `Response::Ok` in both arms.
- [x] Task 7.4. `crates/fono-mcp-server/src/voice_io.rs` —
  introduce `struct McpActivityGuard`:
  - `new(phase: McpPhase) -> Self`: best-effort
    `tokio::spawn` of `fono_ipc::request_any` carrying
    `McpActivityStart { phase }`. Debug-log failures.
  - `Drop`: fire-and-forget `McpActivityEnd`.
- [x] Task 7.5. Wrap call sites with guards:
  - `listen_once` body → `McpPhase::Listening`.
  - `speak_text` → `McpPhase::Speaking` if audio ≥ 1 s.
  - `confirm` tool → `McpPhase::Confirming`.
- [x] Task 7.6. Unit tests:
  - `fono-ipc` round-trip for the two new variants.
  - `fono` daemon: feed `McpActivityStart` then `McpActivityEnd`
    via a mock socket; assert tray transitions `Idle →
    Processing → Idle`. Nested-start test:
    `Start, Start, End, End` should hold `Processing` throughout
    and only restore on the second `End`.
- [x] Task 7.7. Integration smoke (manual verification):
  daemon + MCP server running in parallel, trigger a
  `fono.listen` via an agent invocation, watch the tray flip
  amber and back. Document in
  `docs/coding-agents.md` so users know what to look for.

### Slice 8 — Documentation and changelog

- [x] Task 8.1. `docs/coding-agents.md`:
  - Overlay scope (recording phase only).
  - Silence defaults (10 s, 45 s).
  - Relevance filter usage + `context` argument.
  - **Tray feedback**: "the tray icon turns amber (the same colour
    used while STT or polish is running) for the duration of an
    MCP voice interaction. This is intentional palette reuse — the
    overlay carries the precise sub-state."
- [x] Task 8.2. `docs/configuration.md`: document
  `[mcp].relevance_filter` and
  `[mcp].relevance_max_rejections`. Note the hardcoded 1.5 s LLM
  timeout. Note tray feedback is automatic (no config).
- [x] Task 8.3. **Skipped in v7** — no ADR amendment needed since
  the palette is unchanged.
- [x] Task 8.4. CHANGELOG entries:
  - `## Added`: overlay during MCP listen.
  - `## Added`: relevance filter + `context` argument.
  - `## Added`: tray icon turns amber while the coding agent is
    interacting via voice (listening, speaking, or asking a
    question).
  - `## Changed`: silence default 10 s; `max_seconds` default
    45 s.
- [x] Task 8.5. `assets/agent-presets/voice.md`: teach the agent
  to pass `context` on every `fono.listen` call and to prefer
  `fono.confirm` for any A/B/C decision (it now flashes both
  overlay and tray for user attention).
- [x] Task 8.6. Pre-commit gate.

## Verification Criteria

(v6 list, adjusted for v7's amber-reuse decision.)

- (carried) overlay timing, silence default, heuristic rejections,
  LLM accept/reject mapping, fail-open timeout, `Ignoring` flash,
  rejection ceiling, daemon co-existence, headless fallback,
  lint/test gate.
- Tray icon turns amber within 200 ms of a `fono.listen` call
  starting and restores within 200 ms of it ending.
- Nested calls (`listen_once` calling `speak_text` for prompt) keep
  the tray amber throughout — no flicker between amber and the
  baseline.
- When the daemon isn't running, `fono.listen` works end-to-end and
  emits a single `debug`-level "ipc unreachable" log.
- After an MCP-active span ends, the tray restores whatever state
  the daemon had before — not unconditionally `Idle`.
- (NEW for v7) If dictation's `Processing` phase overlaps with an
  MCP activity span, the tray stays amber across both, and the
  baseline snapshot on the MCP side is whatever dictation left
  behind when MCP ended — the depth-counter handles the overlap
  without flicker.

## Potential Risks and Mitigations

(v6 list, plus one new entry specific to v7.)

(unchanged 1–9.)

10. Tray flicker on short prompts → emit `McpPhase::Speaking` only
    if audio ≥ 1 s.
11. Nested start/end balance → depth counter + RAII guards.
12. Tool-spam denial of service → negligible cost per request; add
    debounce only if measured.
13. **(NEW v7) Amber overload — user can't distinguish "STT is
    running on my F7 dictation" from "MCP is asking me a
    question."** Mitigation: the overlay carries the precise
    state. Documented in `docs/coding-agents.md` (Task 8.1). If
    this proves a real UX problem in practice, the colour mapping
    is a one-line change in the daemon dispatch; the IPC layer
    already carries the `phase` field that would drive a more
    specific tint.

## Alternative Approaches

(v6 list, with the colour decision now resolved.)

(unchanged 1–7.)
8. Reuse `TrayState::Assistant` — rejected per v6.
9. Drive tray directly from MCP — rejected per v6.
10. **(NEW v7) Add `TrayState::Mcp` with a dedicated colour** —
    considered and rejected by user in v6/v7: keep the palette
    small. Easy to revisit by a one-line change if the amber
    overload becomes a real problem.

## Status

- 2026-05-26 — v1 opened.
- 2026-05-26 — v2: overlay scoped; 5 s safety floor.
- 2026-05-26 — v3: 10 s safety floor; relevance filter;
  multi-utterance loop; `Ignoring` overlay state.
- 2026-05-26 — v4: drop `drive_overlay` knob.
- 2026-05-26 — v5: drop `relevance_llm_timeout_ms`; hardcoded
  `const`.
- 2026-05-26 — v6: keep `relevance_max_rejections` as config;
  add tray feedback over IPC; colour was an open question.
- 2026-05-26 — v7 (final pre-implementation): tray reuses
  `TrayState::Processing` (amber); no new tray-state variant; no
  ADR amendment. Plan is ready to hand off to an implementation
  agent.
- 2026-05-26 — v7 fully implemented; Slices 0–8 landed.
