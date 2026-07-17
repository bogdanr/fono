# Assistant Without TTS (text-only reply overlay)

Closes GitHub issue #15 ("feat: Assistant without TTS").

## Objective

Let the voice assistant work when no TTS backend is available (e.g. an
Anthropic cloud user who has STT + LLM but no TTS). Instead of refusing the
turn, run the STT → LLM pipeline as usual and present the reply as **text in
the overlay** (streamed live as tokens arrive), holding it on screen long
enough to read or until the user presses Escape.

## Background / Current State

- The staged assistant turn hard-requires **both** an assistant and a TTS
  backend before it will run: the guard at `crates/fono/src/session.rs:3135`
  (`let (Some(assistant), Some(tts)) = (assistant, tts) else { … }`) sends a
  "assistant backend missing" desktop notification
  (`crates/fono/src/session.rs:3143-3162`), hides the overlay, emits
  `ProcessingDone`, and returns.
- `TtsBackend::None` is the **default** (`crates/fono-core/src/config.rs:562-563`).
  A user with a working STT + assistant but no `[tts]` block therefore always
  falls into the "backend missing" path — the assistant is unusable for them.
  This is precisely the issue's scenario.
- `run_assistant_turn` takes a non-optional `tts: Arc<dyn TextToSpeech>`
  (`crates/fono/src/assistant.rs:134`) and drives every LLM delta through a
  `SentenceSplitter` → `synth_and_enqueue` → `AudioPlayback` pipeline
  (`crates/fono/src/assistant.rs:603-813`). It already accumulates the complete
  reply text in `full_reply` (`assistant.rs:605, 764`).
- The overlay can already display arbitrary text: `OverlayHandle::update_text`
  (`crates/fono-overlay/src/lib.rs:262`) forwards to every backend renderer
  (`renderer.rs:1707`, wired in the wayland/x11/windows/macos backends). It is
  used today for the `LiveDictating` transcript
  (`crates/fono/src/session.rs:4291,4414`) and live-mode captions
  (`crates/fono/src/live.rs:336-343`).
- Overlay assistant states: `AssistantThinking` → `AssistantSynthesising` →
  `AssistantSpeaking` (`crates/fono-overlay/src/lib.rs`, palette/labels at
  `renderer.rs:97,117`). The pump flips them at first-delta and first-audio
  (`assistant.rs:738-740, 1042-1051`).
- FSM: `AssistantThinking --AssistantSpeakingStarted--> AssistantSpeaking`
  (`crates/fono-hotkey/src/fsm.rs:254-256`); Escape (`CancelPressed`) from
  either state returns to `Idle` (`fsm.rs:258-267`); the pump's timer-driven
  `ProcessingDone` also returns to `Idle` (`fsm.rs:288-291`). A text-reading
  phase can reuse `AssistantSpeaking` semantics verbatim: Escape already
  dismisses it, and a dwell timer already ends it.
- Playback drain / `ProcessingDone` emission and overlay hide happen in the
  pump-completion closure spawned by `spawn_assistant_pump`
  (`session.rs:3223`); the drain loop lives at `assistant.rs:930-987`.

## Design Decisions

- **TTS becomes optional, not required.** Change the guard to require only an
  `assistant` (STT is separately required for the mic path; `pre_transcribed`
  covers the streaming path). Keep the "backend missing" notification **only**
  when the *assistant* itself is absent. A `None` TTS is a valid, supported
  configuration — text-only mode — not an error.
- **Reuse `full_reply`, stream it to the overlay.** The pump already builds the
  reply string incrementally. In text-only mode, push the growing text to the
  overlay via `update_text` on each delta so the user sees a live "typing"
  reply, then hold the final text.
- **Reuse the `AssistantSpeaking` FSM state** for the reading phase so Escape
  dismissal and `ProcessingDone` teardown work with zero FSM changes. Fire
  `AssistantSpeakingStarted` at the first delta (there is no audio to gate on).
- **New overlay visual state `AssistantReading`** (distinct label/palette, e.g.
  "REPLY") so the text panel is styled for reading rather than reusing the
  waveform "SPEAKING" scene. It renders the reply text via the existing text
  panel path. This keeps the audible SPEAKING scene unchanged.
- **Read-dwell = a deliberately slow auto estimate, no config knob.** A fixed
  millisecond setting is false precision — real reading time tracks response
  *complexity*, not just word count — and it adds config surface for no real
  gain. Instead hold the overlay for a generous, slow reading-time estimate and
  let Escape be the escape hatch: dwelling slightly too long costs the user one
  keypress, while cutting a reply off mid-read is genuinely bad. Use a slow
  ~130 wpm (vs the ~200–250 wpm of a fluent reader) with a floor (~3 s) so even
  a one-word reply is legible, and a safety cap (~60 s) purely so the overlay
  can never wedge on screen forever. No `[assistant]` knob.
- **Long replies auto-scroll at reading pace.** The text panel today does not
  scroll, so a long reply would be clipped. Drive a continuous auto-scroll of
  the reading panel paced against the *same* slow reading-speed estimate, so a
  long answer scrolls through top-to-bottom over its dwell window and finishes
  scrolling right as the dwell elapses. Short replies that fit need no scroll.
  Escape dismisses at any point.
- **No new dependencies.** Everything reuses existing overlay/FSM/config
  machinery — net-zero on binary size.
- **Realtime (Gemini Live) path is out of scope.** It is inherently
  speech-to-speech and only engages when a realtime backend is selected
  (`session.rs:3076-3131`); text-only applies to the staged path only.

## Implementation Steps

- [ ] **A. Make TTS optional in the turn inputs.**
  - Change `AssistantTurnInputs.tts` to `Option<Arc<dyn TextToSpeech>>`
    (`crates/fono/src/assistant.rs:134`).
  - Relax the guard at `crates/fono/src/session.rs:3135` to require only
    `assistant`; pass `tts: self.current_tts()` through. Keep the missing-backend
    notification path for the assistant-absent case only. Update the warning
    text at `session.rs:3142-3150` accordingly.

- [ ] **B. Text-only branch in `run_assistant_turn`.**
  - When `tts.is_none()`: skip the lazy `AudioPlayback` init
    (`assistant.rs:575-601`) and the `SentenceSplitter`/`synth_and_enqueue`
    path; keep the LLM stream loop.
  - On the first delta, fire `AssistantSpeakingStarted` and set overlay state
    `AssistantReading` (instead of `AssistantSynthesising`/`AssistantSpeaking`).
  - On each delta, `overlay.update_text(full_reply.clone())` so the reply types
    out live.
  - Tool-event deltas continue to be recorded and skipped (they carry no prose)
    — reuse the existing `tool_event` handling (`assistant.rs:746-763`).
  - After the stream ends, hold: `tokio::select!` between the read-dwell sleep
    and `notify.notified()` (Escape / barge-in). Then fall through to the
    existing history push + `ProcessingDone` teardown.
  - Keep metrics honest: `tts_ttfa_ms = None`, `sentences = 0`, `reply_chars`
    populated; the `assistant:` summary line already tolerates missing audio.

- [ ] **C. Overlay `AssistantReading` state.**
  - Add `OverlayState::AssistantReading` (`crates/fono-overlay/src/lib.rs`).
  - Renderer: palette + label (e.g. "REPLY") at `renderer.rs:97,117`; route it
    through the existing text-panel rendering used by `LiveDictating` rather
    than the waveform/cortex scene. Confirm the text panel wraps multi-line
    replies (extend wrapping in `renderer.rs` if needed).
  - Add continuous auto-scroll for replies taller than the panel: track a
    scroll offset advanced each frame so the full reply pans top-to-bottom over
    the dwell window (see step D). Short replies that fit stay static.
  - Map it in `cortex.rs:406` phase match (treat as non-animated / static) so
    the Glass Cortex engine doesn't try to animate a speaking scene.

- [ ] **D. Read-dwell + auto-scroll helper (no config).**
  - Add a small pure helper `read_dwell(reply_chars) -> Duration` in
    `assistant.rs`: slow ~130 wpm, floor ~3 s, safety cap ~60 s — unit-testable.
    No `[assistant]` config knob; Escape is the user override.
  - Pace the overlay auto-scroll against the same estimate so a long reply
    finishes scrolling as the dwell elapses (the renderer derives its per-frame
    scroll step from the dwell duration + content height).

- [ ] **E. Doctor + docs.**
  - `fono doctor`: when assistant is present but TTS backend is `None`, report
    an informational line ("Assistant will reply as on-screen text; no TTS
    configured") rather than an error (`crates/fono/src/doctor.rs`).
  - `docs/providers.md`: note that the assistant works TTS-less (text overlay),
    and how to enable spoken replies.

- [ ] **F. Tests.**
  - Unit: `read_dwell` bounds (floor, cap, slow-wpm scaling).
  - Unit: auto-scroll step derivation — content shorter than the panel yields
    no scroll; taller content scrolls exactly to the bottom by dwell end.
  - Unit/integration: `run_assistant_turn` with `tts: None` returns `Ok(false)`
    for audio, pushes the reply to history, and issues `update_text` calls (use
    the noop overlay / a test double).
  - FSM: confirm `AssistantSpeakingStarted` → `AssistantSpeaking` and
    `CancelPressed` → `Idle` still hold (existing tests at `fsm.rs:427,448,497`
    already cover this — no change expected).
  - Guard: a config with assistant present + `tts.backend = None` no longer
    hits the "backend missing" branch.

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace --tests --lib`
- `./tests/check.sh --size-budget` (binary must stay within the `cpu` budget —
  no new deps, so expected net-zero).
- Manual: configure an Anthropic assistant + working STT + `tts.backend = none`,
  hold the assistant hotkey, speak, and confirm the reply renders as text in the
  overlay, types out live, holds for the dwell window, and dismisses on Escape.

## Out of Scope

- Realtime / speech-to-speech (Gemini Live) path — inherently audio.
- TTS/STT/LLM provider fallback (tracked separately as issue #12).
- Selectable agent voice (#13) and visible assistant model (#14).
