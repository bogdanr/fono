# Assistant Pondering Parity — v1

## Objective

Bring the existing dictation **"Pondering…"** UX to the **voice‑assistant** flow
so the user gets consistent feedback when they pause mid‑utterance, regardless
of whether they pressed the dictation key (F7) or the assistant key (F8). Today
the silence‑watch state machine, the `Pondering` overlay label, the
walking‑letter highlight, and (in toggle mode) the auto‑stop commit are wired
only into `on_start_recording` / `RecordingMode::Toggle`. Both the batch and
the streaming assistant capture paths explicitly opt out
(`crates/fono/src/session.rs:1429-1433`, `:1683-1687`,
`crates/fono/src/session.rs:1613-1623` — no silence task constructed at all).

The goal is **behavioural and visual parity**, with the same calibrated
thresholds and the same UX vocabulary (red SHUT pill, walking‑letter accent,
state‑pill suppression when `auto_stop_silence_ms = 0`). The plan is gated so
visual parity ships before commit semantics, mirroring the slice order in
`plans/2026-05-22-fono-auto-stop-silence-v1.md`.

A second, related defect is in scope: **Pondering currently fires while the
dictation key is held down** (F7 push‑to‑talk), which it never should — the
user owns the boundary by keypress. This is not a visual nit; it interacts
with slice 2's auto‑stop commit (an auto‑commit while the key is held would
be a real surprise). The root cause is documented in the new
"Why pondering leaks into hold mode today" section below; the fix lives in
a new Slice 0 that lands **before** the assistant parity work.

### Why pondering leaks into hold mode today

The hotkey listener at `crates/fono-hotkey/src/listener.rs:289-329`
never emits `HotkeyAction::HoldPressed`. The mapping is:

- **Press** → emit `TogglePressed` (or `AssistantPressed`) immediately, and
  stash the press timestamp.
- **Release** → if elapsed ≥ `LONG_PRESS_THRESHOLD`, emit a *second*
  `TogglePressed` to synthesise the toggle‑off; otherwise emit nothing
  (short press leaves recording latched).

The hold‑vs‑toggle distinction is therefore decided **retroactively at
release time**, by press duration. By then the FSM has already entered
`State::Recording(RecordingMode::Toggle)` (via the press arm at
`crates/fono-hotkey/src/fsm.rs:125-128`) and the orchestrator has already
been told `StartRecording(RecordingMode::Toggle)`. The Toggle‑gated spawn
site at `crates/fono/src/session.rs:1429` therefore fires for every
keyboard press, including long holds. `RecordingMode::Hold` is effectively
dead code on the keyboard path; only IPC / programmatic callers could
feed `HoldPressed` directly.

Fixing this at the FSM layer is risky (every consumer of
`RecordingMode::Hold` would need re‑validation) and over‑broad — the
listener's eager‑press behaviour is intentional, so the user sees
recording start immediately rather than waiting on `LONG_PRESS_THRESHOLD`.
Instead we suppress pondering at the consumer: the silence‑watch task
checks a **key‑held** flag and skips both the `Pondering` visual flip
and any commit publish while the flag is set.

## Background

### Where Pondering lives today

- State machine: `crates/fono-audio/src/silence_watch.rs` (envelope follower +
  hysteresis + `SilenceEvent::{EnteredPondering, ResumedFromPondering,
  Committed}`).
- Capture‑side driver: `spawn_silence_watch_task`
  (`crates/fono/src/session.rs:911-1048`). Hard‑codes
  `OverlayState::Recording { db: 0 }` on resume and
  `OverlayState::Pondering { … }` on entry, and emits
  `HotkeyAction::TogglePressed` on `Committed`.
- Overlay states: `OverlayState::Pondering { db, walk_progress }`
  (`crates/fono-overlay/src/lib.rs:40-43`). The renderer paints the label
  `"PONDERING"` with the dictation accent (red) and a walking‑letter
  highlight (`crates/fono-overlay/src/renderer.rs:92-128`, `:1283-1296`,
  `pondering_highlight_idx` at `:1039-1044`).
- Spawn site: only the toggle branch of `on_start_recording`
  (`crates/fono/src/session.rs:1429-1433`).

### Where the assistant breaks parity

- **Batch path** (`on_assistant_hold_press`,
  `crates/fono/src/session.rs:1627-1694`) builds a plain `CaptureSession` with
  `silence_task: None` and the comment "Assistant push‑to‑talk owns its own
  boundary".
- **Streaming path** (same fn, `:1602-1624`) builds a `LiveCaptureSession`
  via `build_live_capture_pipeline(.., AssistantRecording { db: 0 })`. The
  `LiveCaptureSession` struct (`:153…`) has no `silence_task` field at all.
- Overlay vocabulary is single‑purpose: `OverlayState::Pondering` paints with
  the red dictation accent, so re‑using it from the assistant flow would
  break the green "assistant" colour contract that the user relies on to tell
  the two pipelines apart at a glance.

### Hold vs toggle in the assistant FSM

The assistant key supports both modes (`docs/decisions/...`, fsm at
`crates/fono-hotkey/src/fsm.rs:163-176`): short‑press → toggle (a second
`AssistantPressed` stops), long‑press → hold (`AssistantReleased` stops). The
auto‑stop **commit** therefore makes sense only in the toggle case, exactly
mirroring the dictation rule. In hold mode the user owns the boundary by
releasing the key; the watchdog stays purely visual.

## Design

### Overlay vocabulary: dedicated `AssistantPondering` state

Add `OverlayState::AssistantPondering { db, walk_progress }` rather than
re‑using `Pondering`. Rationale: the assistant flow consistently uses a green
palette (`crates/fono-overlay/src/renderer.rs:93`), and merging the two states
would either paint the dictation pondering in green (wrong) or paint the
assistant pondering in red (worse — the user would think the FSM dropped them
back into dictation). Two distinct states keep the colour contract honest and
the `state_label` / `accent_color` matches local.

The walking‑letter highlight, label string ("PONDERING"), and `walk_progress`
semantics are identical to the dictation case — same
`pondering_highlight_idx` helper, same +45° hue shift on the green base,
same 0…10 000 fixed‑point range.

### Silence‑watch parameterisation

Refactor `spawn_silence_watch_task` to take three knobs instead of hard‑coding
dictation behaviour:

1. `recording_state: OverlayState` — what to flip back to on
   `ResumedFromPondering` (today: `Recording { db: 0 }`; assistant:
   `AssistantRecording { db: 0 }`).
2. `pondering_state_fn: fn(walk_progress: u16) -> OverlayState` — produces the
   pondering overlay state at the current walk progress (today:
   `Pondering { … }`; assistant: `AssistantPondering { … }`).
3. `commit_action: HotkeyAction` — what to publish on `SilenceEvent::Committed`
   (today: `TogglePressed`; assistant‑toggle: `AssistantPressed`; assistant‑hold:
   commit disabled, so caller passes `None` for the whole commit path).

Keep the existing thin wrapper `spawn_silence_watch_task` for the dictation
toggle call site by delegating to the parameterised core.

### Where to spawn from the assistant flow

- **Batch assistant path** (`on_assistant_hold_press`,
  `crates/fono/src/session.rs:1670-1689`): add a sibling `silence_task` field
  population that calls the new parameterised spawn helper with
  `AssistantRecording` / `AssistantPondering` and `commit_action =
  AssistantPressed` (gated by the same toggle‑mode predicate the dictation
  toggle uses). For hold mode, commit is disabled but the visual watch still
  runs.
- **Streaming assistant path**: add `silence_task: Option<AbortHandle>` to
  `LiveCaptureSession` (`crates/fono/src/session.rs:153…`). Spawn the
  watchdog inside `build_live_capture_pipeline` when the active state is
  `AssistantRecording` (or unconditionally for any caller — the
  `LiveDictating` call site already gets the dictation toggle watchdog from
  `on_start_recording`, so guard the spawn on a `silence_watch_kind` param to
  avoid double‑spawning). Abort the handle in every `LiveCaptureSession`
  teardown branch — `on_assistant_hold_release`, `on_assistant_stop`,
  `on_cancel`, and the live‑dictation analogues.

### How the assistant FSM consumes the auto‑stop commit

The dictation path sends `HotkeyAction::TogglePressed`, which the FSM routes
to `LiveTogglePressed` when live‑preview is on
(`crates/fono/src/session.rs:990-1003`). The assistant analogue is
`HotkeyAction::AssistantPressed`: the FSM at
`crates/fono-hotkey/src/fsm.rs:174-177` accepts a second `AssistantPressed`
while in `AssistantRecording` and emits `StopAssistant`, which is exactly
what we want. No new FSM transitions are needed.

Gating rules to enforce in code:

1. **Toggle mode only**: only spawn the commit‑enabled watch when the
   assistant press was a short press (toggle). For long‑press (hold),
   `commit_action = None` and the watch is visual‑only.
2. `auto_stop_silence_ms > 0`: same as dictation; zero means visual only.
3. Speech preamble: enforced by construction in `SilenceWatch` (already true
   for dictation).

### Walking‑letter visual default

When `auto_stop_silence_ms = 0`, the dictation watcher still drives a 5 s
default visual walk (`crates/fono/src/session.rs:937`) so the user *sees*
what auto‑stop would feel like. Mirror this for the assistant — same 5 s
default — so a user who has auto‑stop turned off still gets the pondering
UX during assistant pauses.

### Config story

No new config knobs. The same `audio.auto_stop_silence_ms` governs both flows,
which is correct: the value is "how long of a pause should be interpreted as
'I'm done'", and that contract is identical for dictation and assistant.

Doc update only: rewrite the doc‑comment at
`crates/fono-core/src/config.rs:230` (which was rewritten in slice 4 of the
auto‑stop plan) to drop the "dictation toggle only" qualifier and explain
that the assistant toggle path is also covered.

## Implementation Plan

### Slice 0 — Suppress Pondering while the dictation/assistant key is held

Goal: fix the existing dictation regression (Pondering firing during F7
hold) before extending the watch to the assistant flow. Without this,
slice 1's assistant watch would inherit the same bug and slice 2's
auto‑stop commit would fire under the user's finger.

- [ ] Task 0.1. Add a shared key‑held signal alongside the existing press
  timestamps in `crates/fono-hotkey/src/listener.rs`. Two
  `Arc<AtomicBool>` flags exported from the listener: `dictation_held`
  and `assistant_held`. The listener sets them to `true` on `Pressed`
  and back to `false` on `Released` (and on `CancelPressed`, mirroring
  the existing press‑timestamp clearing at `:323-324`). Surface the
  flags via an addition to the listener's spawn API so the orchestrator
  can plumb them into the session.

- [ ] Task 0.2. Store both flags on the `Session` struct
  (`crates/fono/src/session.rs:330-339` neighbourhood) so the
  silence‑watch task can clone them at spawn time. Default to a pair
  of `Arc::new(AtomicBool::new(false))` for the test/IPC paths that
  don't go through the keyboard listener (preserves current
  behaviour for non‑keyboard callers — IPC `HoldPressed` is the only
  caller that ever actually fed Hold mode anyway).

- [ ] Task 0.3. Teach `spawn_silence_watch_task` to consult the relevant
  key‑held flag inside its 20 ms loop. When the flag is `true`:
  - Skip `SilenceEvent::EnteredPondering` (do not flip the overlay to
    `Pondering`; keep `Recording`).
  - Skip `SilenceEvent::Committed` (do not publish the auto‑stop action).
  - Continue feeding the envelope follower and gate metrics so the VU
    bar stays live — only the *pondering decision* is gated, not the
    measurement.
  - When the flag flips back to `false` (key released, still in toggle
    semantics because the FSM never saw `HoldPressed`), the watch
    resumes normal behaviour from the *current* silence run; a release
    that lands mid‑silence will start counting from that release
    moment, which is the intuitive contract.

- [ ] Task 0.4. Mirror the same gate inside the parameterised core that
  slice 1 builds. The cleanest signature is to make the held‑flag part
  of the spawn config (one `Arc<AtomicBool>` per task), so the
  assistant call sites pass `assistant_held` and the dictation call
  site keeps passing `dictation_held`.

- [ ] Task 0.5. Unit test for the gate logic. Drive `SilenceWatch` with
  a synthetic envelope sequence that would normally produce
  `EnteredPondering`, with the held flag set; assert no overlay flip
  and no commit. Repeat with the flag clear; assert the events fire.
  This is a small wrapper around the existing `SilenceWatch` tests,
  not a full pipeline test.

- [ ] Task 0.6. Verification protocol entry: hold F7 for 10 s in silence
  (after a short qualifying utterance to satisfy the speech preamble),
  confirm the overlay stays in `RECORDING` the whole time and the
  pipeline does not auto‑commit.

- [ ] Task 0.7. Pre‑commit gate. CHANGELOG `## Fixed` entry: "Pondering
  no longer flips on while the dictation key is held down (push‑to‑talk
  semantics restored)."

**Risk**: low. The gate is additive and read‑only at the silence‑watch
side; the listener change is a thin extension of existing
press‑timestamp tracking.

### Slice 1 — Visual parity (no commit, dictation unchanged)

- [ ] Task 1.1. Add `OverlayState::AssistantPondering { db: i8, walk_progress: u16 }`
  to `crates/fono-overlay/src/lib.rs`. Doc‑comment cross‑references the
  existing `Pondering` variant and notes the colour contract (green accent
  kept).

- [ ] Task 1.2. Extend the renderer dispatch tables in
  `crates/fono-overlay/src/renderer.rs`:
  - `accent_color`: `AssistantPondering` → same green as `AssistantRecording`
    (`0xFF22_C55E`).
  - `state_label`: `AssistantPondering` → `"PONDERING"`.
  - `state_has_vu_bar` / live‑audio predicate: include `AssistantPondering`.
  - Walking‑letter highlight match at `:1283` and any sibling `matches!`
    sites: extend to include `AssistantPondering` so the same
    `draw_line_with_highlight` path fires.

- [ ] Task 1.3. Refactor `spawn_silence_watch_task`
  (`crates/fono/src/session.rs:911-1048`) into a parameterised core that
  accepts `recording_state`, a `pondering_state_fn(walk_progress) ->
  OverlayState`, and an `Option<HotkeyAction>` for the commit publish. Keep
  the existing public signature as a thin dictation‑toggle wrapper so the
  call site at `:1429-1433` is unchanged in behaviour. Cover the new
  pondering‑state constructor and resume‑state inputs with a small
  unit‑level smoke (constructor returns `AssistantPondering` when called
  with the assistant `pondering_state_fn`).

- [ ] Task 1.4. Spawn the watch in the **batch** assistant path
  (`on_assistant_hold_press`, `crates/fono/src/session.rs:1670-1689`):
  populate `silence_task` with the parameterised spawn using
  `AssistantRecording` / `AssistantPondering` and `commit_action = None`.
  Strip the "Assistant push‑to‑talk owns its own boundary — no silence
  watchdog" comment and replace with a one‑line cite of this plan.

- [ ] Task 1.5. Spawn the watch in the **streaming** assistant path: add
  `silence_task: Option<tokio::task::AbortHandle>` to `LiveCaptureSession`
  (`crates/fono/src/session.rs:153…`); construct it in
  `build_live_capture_pipeline` (`:2139…`) when `active_state ==
  AssistantRecording`. Teardown sites that must abort the handle:
  `on_assistant_hold_release` (`:1701…`), `on_assistant_stop` (`:1920…`),
  `on_cancel` (`:1519…`) for the assistant branch.

- [ ] Task 1.6. Renderer unit tests: per‑state‑visibility coverage extended
  so `state_has_vu_bar` accepts `AssistantPondering`; `state_label` returns
  `"PONDERING"`; `accent_color` returns the green accent. Mirror the slice‑3
  test style at `crates/fono-overlay/src/renderer.rs` test module.

- [ ] Task 1.7. Pre‑commit gate (`cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`). CHANGELOG `## Added` entry:
  "Assistant flow now shows the same Pondering UX as dictation when the
  user pauses mid‑utterance (visual only; auto‑stop commit follows in a
  separate slice)."

**Risk**: low. Renderer change is additive; capture‑side change spawns a
read‑only watchdog and does not touch the audio decision path.

### Slice 2 — Auto‑stop commit for assistant toggle

- [ ] Task 2.1. Determine the press kind (toggle vs hold) at
  `on_assistant_hold_press` so the spawn helper knows whether to pass
  `Some(HotkeyAction::AssistantPressed)` or `None`. The cleanest source is
  the FSM state transition that brought us in — the hotkey FSM at
  `crates/fono-hotkey/src/fsm.rs:163-176` already distinguishes by the
  triggering action. Pipe a `RecordingMode`‑style enum through the
  press handler (mirrors `crates/fono/src/session.rs:1429`'s
  `matches!(mode, RecordingMode::Toggle)` gate). Document the assumption
  if the press kind cannot be cleanly retrieved without FSM plumbing.

- [ ] Task 2.2. Wire `commit_action = Some(HotkeyAction::AssistantPressed)`
  through the parameterised spawn for the toggle branch only. Confirm by
  tracing that the daemon's central loop translates `AssistantPressed`
  while in `AssistantRecording` into `StopAssistant`, identical to the
  user pressing the assistant key a second time.

- [ ] Task 2.3. Five unit tests in `silence_watch.rs` already cover the
  commit semantics generically; add one assistant‑specific integration‑style
  test (or harness extension) that asserts: with `auto_stop_silence_ms =
  3 000`, after speech + 3.5 s silence, exactly one `AssistantPressed`
  reaches the orchestrator's `action_tx` and the assistant session
  transitions to `AssistantThinking`.

- [ ] Task 2.4. Rewrite the doc‑comment at
  `crates/fono-core/src/config.rs:230` to remove the "toggle dictation
  only" qualifier and add "and assistant toggle".

- [ ] Task 2.5. Tray submenu copy review (`crates/fono-tray/src/lib.rs:196`):
  the "Auto‑stop after silence" label currently implies dictation only.
  Re‑label as "Auto‑stop after pause" or similar, neutral between
  dictation and assistant. Cosmetic; copy change only.

- [ ] Task 2.6. Pre‑commit gate. CHANGELOG `## Added`: "Auto‑stop now
  applies to the assistant toggle path in addition to dictation toggle."

**Risk**: medium. First behaviour change. Mitigated by toggle‑only gating,
identical FSM contract (`AssistantPressed` is already the toggle‑off event),
and the speech‑preamble requirement carried over from
`SilenceWatch`'s construction.

### Slice 3 — Documentation and roadmap

- [ ] Task 3.1. `docs/status.md` session log entry summarising the parity
  shift.
- [ ] Task 3.2. `ROADMAP.md` — promote this feature to **Shipped** on tag
  day per the project guideline (not at slice‑land time).
- [ ] Task 3.3. CHANGELOG release section assembled at tag time.

## Verification Criteria

- Pressing the assistant key (F8) and pausing for ≥ 1 s causes the overlay
  label to flip from `ASSISTANT` to `PONDERING` while keeping the green
  assistant accent (visual only, slice 1).
- Resuming speech within `auto_stop_silence_ms` snaps the label back to
  `ASSISTANT` with no walking‑letter residue.
- With `auto_stop_silence_ms = 0`, the walking‑letter highlight still
  animates over the 5 s default visual window (matches dictation default).
- With `auto_stop_silence_ms = 5_000` and a short assistant press
  (toggle), 5+ s of silence after qualifying speech auto‑commits the turn
  and the FSM transitions `AssistantRecording → AssistantThinking` exactly
  as if the user had pressed F8 a second time (slice 2).
- With `auto_stop_silence_ms = 5_000` and a long assistant press (hold),
  the same silence shows the Pondering UX but does **not** commit; the
  user must release F8 to end the turn.
- Pressing ESC during `AssistantPondering` cancels cleanly: the silence
  task is aborted, the streaming session is torn down, and the overlay
  hides — no orphaned tokio tasks.
- Dictation behaviour is byte‑for‑byte unchanged (regression check against
  the existing `silence_watch.rs` unit tests and the
  `2026-05-22-fono-auto-stop-silence-v1` verification protocol).
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D
  warnings`, and `cargo test --workspace --tests --lib` all pass.

## Potential Risks and Mitigations

1. **Colour confusion if `Pondering` and `AssistantPondering` are
   accidentally merged in a renderer dispatch table.**
   Mitigation: dedicated `state_label`, `accent_color`, and `matches!`
   coverage; renderer unit tests assert the assistant variant returns
   green, not red.

2. **Auto‑stop firing during an assistant hold press would cut the user
   off mid‑sentence.**
   Mitigation: the spawn helper takes `commit_action: Option<HotkeyAction>`;
   the hold branch passes `None`. Speech‑preamble requirement carried over
   from `SilenceWatch` adds a second safety net.

3. **Streaming assistant capture's `LiveCaptureSession` teardown could
   leak the new silence task on the ESC path.**
   Mitigation: every teardown branch in `on_assistant_hold_release`,
   `on_assistant_stop`, and `on_cancel` aborts the handle. Unit‑level
   smoke is hard to write for this directly; gate on a manual ESC test in
   the verification protocol.

4. **Press‑kind detection (toggle vs hold) may require FSM plumbing that
   widens the slice 2 surface area.**
   Mitigation: if the cleanest detection point isn't available without an
   FSM refactor, defer slice 2 and keep visual parity (slice 1) as the
   shipped milestone. The user explicitly framed the request as "shifts
   the user behavior … consistent" — visual parity alone delivers that.

5. **CHANGELOG/ROADMAP forgotten at tag time** (recurring risk per
   `AGENTS.md`).
   Mitigation: explicit slice‑3 tasks; not closed until both files are
   updated.

## Alternative Approaches

1. **Re‑use `OverlayState::Pondering` directly.** Reject — would force a
   colour decision that breaks the green/red assistant‑vs‑dictation
   contract the user has internalised.

2. **Add a single `OverlayState::Pondering { flavour: Flavour::Dictation
   | Flavour::Assistant }` instead of two variants.** Equivalent in
   substance but worse in cost: every renderer `matches!` already
   enumerates the variants by name; threading a `Flavour` discriminator
   means every match arm grows a nested condition. Two variants stay
   honest to the existing pattern.

3. **Auto‑stop commit only, no visual parity.** Inverted order — would
   ship behaviour change without the user feedback that lets them
   understand why the turn ended. Rejected because the user explicitly
   asks for consistency, which is a UX request, not a behaviour request.

4. **Land auto‑stop commit for both hold and toggle.** Rejected — hold
   means the user owns the boundary by keypress contract. Auto‑stopping
   under their finger would be a surprise.

## Status

- 2026-05-22 — plan opened; no implementation work started.
