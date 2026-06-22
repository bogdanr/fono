# Realtime: Delete Dead Prewarm + Add Full-Duplex Live Mode (v4)

## Why v4

v4 supersedes v3 (`plans/2026-06-22-realtime-live-conversation-mode-v3.md`). v3 framed
push-to-talk (PTT) as new work; it is not. **PTT already behaves correctly today** — see
"Current PTT behaviour" below. v4 therefore narrows the plan to what is genuinely new:

1. **Delete the dead realtime prewarm** (pure deletion).
2. **Add a full-duplex "live mode"** (tap-to-enter conversation) — the one real feature.
3. **Preserve / verify PTT** — no rebuild.

Everything stays provider-agnostic (Gemini Live today, OpenAI Realtime next) and must not
disturb the standard staged models.

## Current PTT behaviour (already correct — DO NOT rebuild)

Grounded in code; the only task here is to keep it working and add regression coverage:

- The realtime branch runs in `on_assistant_hold_release`, i.e. **after** F8 release
  (`crates/fono/src/session.rs:2780-2842`).
- During the hold, audio is captured to a buffer
  (`crates/fono/src/session.rs:2561-2666`); on release, capture is stopped and drained
  (`session.stop_and_drain()`, `:2764`) — **the mic is closed before any reply plays.**
- The utterance is handed over as a buffered stream
  (`buffered_frame_stream(&pcm, …)`, `crates/fono/src/session.rs:2822-2826`); the socket
  is not opened until after release.
- `run_realtime_turn` opens the session, uploads, and **waits for the full reply** —
  `drive_realtime_reply` loops to `Done` before returning
  (`crates/fono/src/assistant.rs:1529`, `:1567-1650`), then the one-shot session closes.
- Cancellable via Escape (`notify` path, `crates/fono/src/session.rs:2797-2801`,
  `crates/fono/src/assistant.rs:1598-1605`).

Net: hold → record → release closes mic → wait for complete reply → close. No AEC, no
barge-in, cheapest possible. This is exactly the desired PTT contract, so v4 only
**verifies and documents** it.

## Objective

- Remove the useless startup prewarm and its complexity.
- Add a **full-duplex, phone-grade live conversation mode** on **tap** of F8, provider-
  agnostic, that **connects only on demand** and **cannot quietly burn money** when idle.
- Leave PTT (hold) and all standard staged models untouched.

Hard constraints: provider-agnostic (the `RealtimeAssistant` trait + catalogue are the
seam, `crates/fono-assistant/src/traits.rs:196-260`,
`crates/fono-core/src/provider_catalog.rs:66-93`); never break the staged path (all new
code behind `#[cfg(feature = "realtime")]` + the `current_realtime().is_some()` gate,
`crates/fono/src/session.rs:2788`); connect only when needed (no startup/idle prewarm).

## F8 semantics after v4

The hotkey FSM already splits tap vs hold (`crates/fono-hotkey/src/listener.rs:304-346`):

- **Hold (PTT)** → unchanged: record while held, release commits, wait for the full reply,
  close. No AEC, no barge-in.
- **Tap** → **new**: enter/leave full-duplex live mode (open mic, server VAD, acoustic
  barge-in, continuous voice, AEC, idle/cost controls).

## Implementation Plan

### Part A — Remove the dead prewarm (pure deletion)

- [x] A1. Delete `spawn_realtime_warmup` + its call site
  (`crates/fono/src/session.rs:2072-2106`, `:1950-1953`). Rationale: warms transient
  caches that die within minutes; useless complexity the maintainer flagged.
- [x] A2. Drop `prewarm` from the realtime path (`GeminiLive::prewarm`,
  `crates/fono-assistant/src/gemini_live.rs:408-426`, and any realtime trait surface).
  Future providers must not copy it.
- [x] A3. Soften the changelog claim. The prewarm shipped in the already-released
  0.11.0 section, so rather than rewrite shipped history, a new `## [Unreleased]`
  section documents the removal honestly (`CHANGELOG.md:8-16`).
- [x] A4. Confirmed no test depended on realtime prewarm; builder tests stay green.
  Full gate passed: `cargo fmt --check`, `clippy --all-targets --features realtime
  -D warnings`, and `cargo test --workspace --lib --tests --features realtime` (170
  fono-tts + 13 fono-update + all others). Default-feature build also verified.

### Part B — Preserve & verify PTT (no rebuild)

- [x] B1. Added `assistant::tests::realtime_reply_drains_to_done_then_stops`
  (`crates/fono/src/assistant.rs:1825-1864`) pinning the PTT contract: the reply pump
  accumulates user + assistant transcripts, drains to `Done`, and stops exactly there
  (a post-`Done` event is left unconsumed). Uses the device-free drain path so it runs
  headless in CI.
- [x] B2. Added `gemini_live::tests::setup_json_does_not_enable_server_vad`
  (`crates/fono-assistant/src/gemini_live.rs:570-584`) asserting the PTT setup payload
  carries no `realtimeInputConfig` and no `automaticActivityDetection` — PTT relies
  solely on `audioStreamEnd` to commit, so the model never replies mid-utterance.
- [ ] B3. (Optional, latency only — defer to the multiprovider plan.) Streaming mic during
  the hold instead of buffering-then-uploading is a post-release-latency optimization
  covered by `plans/2026-06-18-realtime-live-mic-streaming-multiprovider-v1.md` Part A. Not
  required for v4; noted so it isn't conflated with the "wait for reply" semantics.

### Part C — Full-duplex live mode (the one real feature)

- [x] C1. **Tap = enter/exit live mode.** Branch the F8 action on the FSM tap/hold signal:
  hold → existing PTT path; tap → new live-mode entry. Reuse the single
  `current_realtime()` gate (`crates/fono/src/session.rs:2788`) as the dispatch point.
- [x] C2. **Persistent session held in `AssistantSessionState`.** Open the realtime session
  lazily on first tap and keep it across many turns (not the per-turn one-shot the PTT path
  uses). Open-time config (Gemini setup/`setupComplete`; OpenAI `session.update`) is paid
  once. Rationale: this is what makes live mode interactive rather than turn-based.
- [x] C3. **Full-duplex provider config.** Enable server VAD + barge-in per protocol:
  Gemini `automaticActivityDetection`; OpenAI `turn_detection: server_vad`/`semantic_vad`
  + `create_response` + `interrupt_response`. The model owns turn boundaries.
  *(Done for Gemini: `RealtimeMode` seam on `open_session`; `build_setup_json` full-duplex
  flag emits `realtimeInputConfig` + `START_OF_ACTIVITY_INTERRUPTS`. OpenAI lands with F1.
  Verified live via the Slice-0 smoke harness `examples/smoke_realtime_live.rs`.)*
- [x] C4. **Continuous open-mic capture** for the live-session lifetime (reuse
  `start_with_forwarder`, `…multiprovider-v1.md:30-33`). Distinct from PTT's
  hold-and-drain capture.
- [x] C5. **Continuous reply pump.** Generalise `drive_realtime_reply`
  (`crates/fono/src/assistant.rs:1567-1650`) into a long-lived loop: play `Audio`, push
  transcripts to history per `Done`, honour `Interrupted` (acoustic barge-in,
  `:1630-1640`), exit on tap-off / Escape / idle / cap / provider-close.
- [ ] C6. **Input resampling per `RealtimeProfile.input_sample_rate`** (identity Gemini,
  16→24 kHz OpenAI; stateful resampler, cf. `crates/fono/src/assistant.rs:1367`). Shared
  with PTT.
- [x] C7. **Live overlay indicator with floor-ownership colours** (DECIDED 2026-06-22).
  Because the mute-while-speaking baseline (E2) guarantees exactly one party holds the
  floor at any instant, the overlay colour is an unambiguous "who's speaking" signal
  driven by the same gate that mutes the mic. Reuse the existing staged-assistant
  palette — no new renderer colours:
  - **User's turn (mic live)** → green `AssistantRecording` (`0xFF22C55E`,
    `crates/fono-overlay/src/renderer.rs:93-95`).
  - **Waiting on model first audio (request in flight, mic muted)** → amber
    `AssistantThinking`/`AssistantSynthesising` (`0xFFF59E0B`, `renderer.rs:96`).
  - **Model speaking (playback active, mic muted)** → sky-blue `AssistantSpeaking`
    (`0xFF38BDF8`, `renderer.rs:97`).
  Loop: green → amber → blue → green. Add elapsed time + provider + model to the live
  panel (build on `crates/fono/src/session.rs:117-132`) so the running meter is always
  visible. Live mode is a new *orchestration* of these existing states, not new renderer
  work.

### Part D — Live-mode cost controls

- [x] D1. **Lazy connect on demand only** — first tap opens the session; never at startup
  or idle.
- [x] D2. **Idle auto-close + notify** on local silence. *(Revised 2026-06-22: the
  original provider-VAD `idle_timeout_secs` timer never fired — on a no-AEC host
  the open mic keeps tripping the provider VAD, so `UserTextFinal` recurred and
  reset the timer indefinitely. Replaced with the dictation path's local
  silence-watch (`auto_stop_silence_ms`, default 3 s): after a completed reply,
  continuous **local** user silence closes the session with `LiveExit::Idle`,
  gated off while the model holds the floor. This also supplies the missing
  Pondering animation. `idle_timeout_secs` config removed.)*
- [x] D3. **Hard `max_session_secs` backstop**, set at/below each provider's own session
  ceiling.
- [x] D4. **No silent auto-reconnect** on provider-side close (idle/cap/resumption): drop to
  idle + notify; require a fresh tap.
- [ ] D5. **Optional client-side VAD silence gating** (`vad_gate`, default off) to withhold
  pure-silence frames from upload; bounded pre-roll so onsets/barge-in aren't clipped.
- [x] D6. **Explicit exit** (second tap / Escape) wired to the same teardown as D2/D3.
- [x] D7. **Model-driven end-of-conversation (Part B).** Full-duplex setup declares a single
  `end_conversation` function tool and the system instruction tells the model to call it
  when the user signals they are done ("goodbye", "that's all"). The reader maps the
  `toolCall` to `RealtimeEvent::EndConversation`; the pump closes with `LiveExit::EndedByModel`.
  Complements D2's local-silence close (semantic intent vs. trailing silence).

### Part E — Live-mode echo cancellation (AEC) — live mode ONLY

> **DECISION (2026-06-22, maintainer sign-off).** Ship live mode now with the
> **mute-while-speaking baseline (E2)** as the default, host-agnostic live mode.
> **AEC + talk-over barge-in (E1/E3) move to the ROADMAP** as the capability
> upgrade (the existing "Talk over the assistant" roadmap item already tracks the
> PipeWire AEC work). Rationale: E2 is proven complete and works on every host;
> AEC is Linux/PipeWire-only, needs a runtime module, and only adds barge-in.
> Shipping the proven path and treating barge-in as an upgrade keeps AEC off the
> critical path. The overlay floor-ownership colours (C7) are the UX that makes
> the no-barge-in turn structure legible.

> **Empirical finding (2026-06-22, Slice-0 smoke harness, live Gemini).**
> Full-duplex with a continuously open mic and **no** AEC is unusable: the mic
> picks up the model's own reply audio, the input transcriber turns that echo
> into bogus "user" text (observed: chopped multi-language fragments —
> Romanian/German/Korean — none of which the user spoke), and server VAD barges
> in on essentially every reply. This reproduced **on headphones** and could not
> be tuned out via VAD sensitivity (`START_SENSITIVITY_LOW` etc.) — lowering mic
> gain only delayed the self-interrupt. Gating the mic while the model speaks
> (`smoke_realtime_live --mute-while-speaking`) eliminated it completely: 0
> barge-ins, clean transcripts, full replies, many turns. **Conclusion:** AEC (or
> mic-gating) is a *gating dependency* for live mode, not a finishing touch.
> Barge-in specifically requires AEC; without it the only usable live mode is the
> mute-while-speaking fallback (E2), which trades barge-in for clean operation.

- [ ] E1. Engage AEC for live mode via the existing PipeWire plan
  (`plans/2026-05-25-double-talk-barge-in-pipewire-aec-v1.md`): load `module-echo-cancel`
  for the session, route playback at the AEC sink, capture from the AEC source. **Not used
  by PTT** (mic closed during reply → nothing to cancel). Enables talk-over barge-in (E3).
- [x] E2. **Mute-while-speaking fallback (no-AEC hosts) — VALIDATED + SHIPPED as baseline.** When AEC is
  unavailable (no PipeWire `module-echo-cancel`; macOS/Windows; PulseAudio-less), gate the
  mic forwarder while the model is producing reply audio so it cannot hear itself. This
  yields hands-free multi-turn conversation **without barge-in** (the user speaks only on
  the model's turn). Proven in the Slice-0 harness. Make it the automatic fallback when
  `echo_cancel` can't engage, surfaced to the user (barge-in disabled), not a hard error.
- [ ] E3. Confirm live-mode barge-in stops playback and provider generation on both
  providers (Gemini `interrupted`; OpenAI `speech_started` + truncate/cancel). **Requires
  E1 (AEC) — barge-in is meaningless under the E2 fallback.**

### Part F — Selection, config, docs

- [ ] F1. Data-driven factory dispatch on `RealtimeProtocol` (reuse
  `…multiprovider-v1.md:131-135`); orchestrator/session untouched. Both PTT and live mode
  run through the same trait, so a new provider is additive.
- [x] F2. Add `[assistant.realtime]` config (`live_mode`, `max_session_secs`) under
  `[assistant]` (`crates/fono-core/src/config.rs`) with `#[serde(default)]`. PTT needs none
  of these; they govern live mode only. *(Revised 2026-06-22: `idle_timeout_secs` dropped —
  idle close now derives from the existing `auto_stop_silence_ms`; `vad_gate`/`echo_cancel`
  deferred with the AEC roadmap work.)*
- [ ] F3. Docs: realtime section in `docs/providers.md` covering **both F8 gestures**
  (hold = PTT, unchanged; tap = live mode, full-duplex + cost + AEC), the cost model, and
  doctor/wizard surfacing provider + protocol + model. Update `docs/status.md`.

### Cross-cutting guardrail

- [ ] G1. Prove the staged path is untouched: build/test with `--no-default-features`
  (realtime off) → staged + server tests pass, no realtime symbols linked. With realtime
  on, both PTT and live mode run through one orchestrator with provider specifics confined
  to client modules + factory.

## Verification Criteria

- **PTT unchanged:** B1 regression test passes — hold/record/release plays the complete
  reply before the one-shot session closes, mic provably closed during playback, Escape
  cancels. No AEC engaged.
- **Prewarm gone:** no realtime socket opens at daemon startup for any provider; A-series
  deletion compiles clean and all tests pass.
- **Live mode:** a tap opens a persistent full-duplex session; the user converses turn
  after turn without pressing anything; talking over the model interrupts it; idle/cap/exit
  all close it; on PipeWire + speakers there is no feedback loop.
- Both PTT and live mode work selecting either a Gemini Live or OpenAI Realtime model id,
  with no provider branches outside the client modules + factory.
- Realtime off: staged/assistant/server tests pass unchanged; no realtime symbols linked.
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib` pass; `cargo tree -p fono -i tokio-tungstenite`
  net-zero.

## Potential Risks and Mitigations

1. **Refactoring the F8 dispatch regresses the working PTT path.**
   Mitigation: B1 pins the PTT contract first; C1 adds the tap branch beside PTT rather
   than rewriting it.
2. **Live-mode feedback loop without AEC.** Mitigation: E1 PipeWire AEC; E2 fallback;
   scoped to live mode only.
3. **Runaway live-mode cost.** Mitigation: D1 on-demand connect, D2 idle close, D3 hard
   cap, D4 no silent reconnect, D5 optional gating, C7 visible meter.
4. **Provider wire divergence leaking into the orchestrator.** Mitigation: trait-only
   orchestrator; G1 verifies no provider branches outside the factory.
5. **OpenAI 24 kHz resampling artifacts.** Mitigation: C6 stateful resampler; Gemini
   identity.
6. **Provider-imposed live-session caps cutting conversations short.** Mitigation: D3 cap
   at/below ceilings; D4 provider-close → notify + idle.
7. **Breaking the standard staged models.** Mitigation: cfg-gating + feature-off tests
   (G1); land incrementally.

## Alternative Approaches

1. **Ship Part A + B only (delete prewarm, verify PTT) and stop.** Smallest possible
   change; defers the full-duplex feature. Reasonable if live mode's AEC work is not yet
   wanted. Recommended as the first landing increment.
2. **Single full-duplex mode (drop PTT).** Forces AEC + open-mic + idle cost on every
   realtime user. Rejected: PTT is cheaper, AEC-free, already working, and matches
   standard-model muscle memory.
3. **OpenAI live mode before/after Gemini.** Independent via the trait; Gemini already
   has a client, so do Gemini live mode first, OpenAI inherits it. Order is flexible.
4. **Persistent keepalive session for instant first turn.** Bills continuously; rejected.
5. **Bundle a software AEC.** Grows the binary; PipeWire module is net-zero. Rejected.
6. **Build daemon wiring (Slice 1) before AEC (Part E).** Original order. **Revised after
   the Slice-0 finding:** AEC / mic-gating is a gating dependency, so the recommended order
   is now (a) land the E2 mute-while-speaking fallback as the baseline live mode — it is
   validated and host-agnostic — then (b) wire it into the daemon (Slice 1: tap/hold split,
   persistent session, overlay, exit) on top of that baseline, then (c) add E1 AEC + E3
   barge-in as the capability upgrade where PipeWire is available. This guarantees every
   slice past Slice 0 is demonstrably usable rather than self-interrupting.
