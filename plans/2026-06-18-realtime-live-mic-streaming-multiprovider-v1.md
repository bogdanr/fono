# Realtime Live Mic Streaming + Multi-Provider (OpenAI) — v1

## Objective

Deliver two independent improvements to the realtime / speech-to-speech assistant
path, each landing without disturbing the staged pipeline, local TTS, server mode,
or the currently-working buffered Gemini Live turn:

1. **Live mic streaming (Inc5b).** Stream captured mic PCM into the open realtime
   session *during* the F8 hold instead of buffering the whole utterance and
   uploading it after release. This removes upload time from the post-release
   critical path, so the model can start responding sooner.
2. **Multi-provider realtime (OpenAI Realtime).** Add a second `RealtimeAssistant`
   implementation (OpenAI Realtime API over WebSocket) behind the existing trait,
   selectable by model id, with no orchestrator changes.

Non-goal (explicitly out of scope): true open-mic full-duplex conversation. Push-to-talk
semantics are retained, which avoids acoustic-echo / AEC entirely.

## Key architectural facts (grounded)

- `RealtimeSession` is already a streaming interface: `audio_in:
  mpsc::Sender<Vec<f32>>` + `events: BoxStream<RealtimeEvent>`
  (`crates/fono-assistant/src/traits.rs:225-232`). Closing `audio_in` signals
  end-of-utterance.
- Buffering lives only in the orchestrator: `run_realtime_turn(pcm: Vec<f32>)`
  + `send_mic_to_session` resample-chunk-dump-drop
  (`crates/fono/src/assistant.rs:1332-1355`, `1441-1442`). The wire client
  (`gemini_live.rs`) and the trait need NO changes for live streaming.
- The capture-on-press + real-time forwarder machinery already exists and is
  battle-tested for interactive STT (`cap.start_with_forwarder(...)`,
  `crates/fono/src/session.rs:3371`; press handler mirrors
  `on_assistant_hold_press` / `on_start_live_dictation`).
- The catalogue already generalises providers: `RealtimeProtocol::{GeminiLive,
  OpenAiRealtime}` (`crates/fono-core/src/provider_catalog.rs:88-93`),
  `RealtimeProfile` carries `input_sample_rate` / `output_sample_rate`
  (`provider_catalog.rs:71-83`), `native_input_rate()` per backend.
- The orchestrator only ever talks to the `RealtimeAssistant` trait, so a new
  provider is additive.
- Capture target rate is 16 kHz (`[audio] sample_rate = 16000`); Gemini Live
  input is 16 kHz → **identity, no resampling**. OpenAI Realtime input is
  24 kHz → per-frame resample needed (the only resampling case).
- Dependencies: both live streaming and OpenAI Realtime are WebSocket + JSON.
  `tokio-tungstenite`, `serde_json`, `base64` are all already in the binary
  graph → net-zero, nothing to flag per the binary-size rule.

## Implementation Plan

### Part A — Live mic streaming (Inc5b)

- [ ] A1. **Make the session feed stream-based, not buffer-based.** Change
  `RealtimeTurnInputs` to carry a frame source (`mpsc::Receiver<Vec<f32>>` of
  mono f32 frames at capture rate) instead of `pcm: Vec<f32>`. Replace
  `send_mic_to_session`'s "resample whole clip + dump + drop" with a forwarding
  loop that pulls frames from the receiver, resamples each to
  `native_input_rate` (identity for Gemini), pushes into `audio_in`, and drops
  `audio_in` when the receiver closes. Rationale: localises the buffered→streamed
  change to the orchestrator; clients/trait untouched.

- [ ] A2. **Preserve the buffered path as a degenerate stream (test + fallback
  safety).** Provide a thin adapter that feeds a pre-recorded `Vec<f32>` through
  the same receiver (send all frames, then close). This keeps every existing
  offline test and the "record-then-send" behaviour working byte-for-byte, and
  gives a safe fallback if live capture-on-press is unavailable. Rationale: zero
  regression to the currently-shipping turn.

- [ ] A3. **Open the realtime session + start capture on F8 press.** In the
  session F8 realtime branch, mirror the interactive press path
  (`on_assistant_hold_press` / `start_with_forwarder`): spawn `open_session(ctx)`
  and start a capture session whose forwarder pushes frames into the A1 receiver.
  The reply-drain task (`drive_realtime_reply`) is spawned as today. Rationale:
  audio uploads concurrently with the hold instead of after it.

- [ ] A4. **Buffer-until-ready ring (the one genuine new piece).** Because
  `open_session` takes a few hundred ms (WS handshake + setup/`setupComplete`),
  hold frames captured before the session is ready in a small bounded
  `VecDeque`; once `audio_in` exists, flush the deque in order, then forward
  live. Cap the deque (e.g. ~2 s of audio) and drop-oldest on overflow with a
  `warn!`. Rationale: handles the short-utterance race that the current
  buffered design sidesteps.

- [ ] A5. **Close `audio_in` on F8 release (commit/end-of-utterance).** Release
  stops the capture forwarder and drops the A1 sender, which the A1 loop turns
  into a dropped `audio_in` → the client emits `audioStreamEnd` (Gemini) /
  buffer commit (OpenAI). Reply continues draining until `Done`. Rationale:
  preserves push-to-talk turn boundaries.

- [ ] A6. **Preserve push-to-talk; suppress mid-hold auto-response.** Where the
  protocol supports it, disable server-side automatic activity detection so the
  model does not start replying mid-hold (Gemini: rely on `audioStreamEnd`;
  OpenAI: `turn_detection: null` + explicit `commit`/`response.create` on
  release). Rationale: keeps the current feel and guarantees the mic is closed
  before reply audio plays → no acoustic echo.

- [ ] A7. **Graceful handling of earlier open-failure.** A WS open failure now
  surfaces at press rather than release. On failure, abort the turn cleanly with
  the existing critical-notify path (Auth/Network classes) and, if A2's buffered
  adapter is wired, optionally fall back to a buffered attempt. Rationale: the
  failure UX must not regress now that connect happens earlier.

- [ ] A8. **Tracing.** Replace the single `realtime.audio_sent` instant with
  streaming markers on the `capture` lane: `realtime.session_open`,
  `realtime.first_frame_sent`, `realtime.input_closed`, so a `/tmp/fono-traces`
  waterfall shows upload overlapping the hold. Rationale: makes the latency win
  visible and debuggable.

### Part B — OpenAI Realtime provider (multi-provider proof)

- [ ] B1. **New client `crates/fono-assistant/src/openai_realtime.rs`** behind
  the existing `realtime` feature, implementing `RealtimeAssistant`. Connect to
  `wss://api.openai.com/v1/realtime?model=...` with `Authorization: Bearer` +
  `OpenAI-Beta: realtime=v1`. Map the wire events to `RealtimeEvent`:
  `response.audio.delta` → `Audio`, `response.audio_transcript.delta` →
  `AssistantTextDelta`, `conversation.item.input_audio_transcription.completed`
  → `UserTextFinal`, `input_audio_buffer.speech_started` → `Interrupted`,
  `response.done` → `Done`. Mic frames → `input_audio_buffer.append` (base64
  PCM16); on `audio_in` close → `input_audio_buffer.commit` + `response.create`.
  `native_input_rate() = 24_000`. Mirror the Deepgram-streaming /
  `gemini_live.rs` idioms (manual upgrade request, split read/write, serde
  `#[serde(default)]`, offline serialization tests). Rationale: reuses the
  proven WebSocket pattern; no new deps.

- [ ] B2. **Add an OpenAI `RealtimeProfile` to the catalogue** on the `openai`
  entry: `protocol: RealtimeProtocol::OpenAiRealtime`, the realtime model id
  (e.g. the current `gpt-realtime` / `gpt-4o-realtime-preview` — verify against
  the live models list before defaulting), `input_sample_rate: 24_000`,
  `output_sample_rate: 24_000`, `ws_url`. Extend the catalogue realtime
  invariant tests to cover it. Rationale: selection stays data-driven by model
  id.

- [ ] B3. **Generalise factory dispatch.** In `build_assistant_handle`, when the
  selected provider's `RealtimeProfile.model` matches `[assistant.cloud].model`,
  dispatch on `profile.protocol` to build `GeminiLive` or `OpenAiRealtime`
  (key/auth resolved per provider). Keep the opt-in-by-model-id gate. Rationale:
  one selection seam for all realtime providers; orchestrator/session unchanged.

- [ ] B4. **Per-frame resampling for non-16k inputs.** Confirm the A1 forwarding
  loop resamples 16 kHz capture → 24 kHz for OpenAI. Prefer a small stateful
  resampler over independent per-frame `resample_linear` to avoid frame-boundary
  artifacts; Gemini stays identity. Rationale: correctness for the 24 kHz path.

- [ ] B5. **Live wire verification** of the OpenAI path against a real key
  (handshake, append/commit, audio deltas, transcripts, interrupt), mirroring
  the Gemini Live verification that caught the `mediaChunks` deprecation.

### Shared / hygiene

- [ ] S1. Update `docs/providers.md` (realtime section: both providers,
  push-to-talk semantics, the live-streaming latency note) and `docs/status.md`.
- [ ] S2. Wizard/doctor: surface OpenAI realtime in the same "low-latency voice
  mode" discoverability already added for Gemini; doctor shows the active
  realtime model + protocol.
- [ ] S3. Run the full pre-commit gate (fmt --check, clippy -D warnings,
  workspace tests) before each commit; signed-off, no co-author trailer, no push
  without explicit instruction.

## Verification Criteria

- A `/tmp/fono-traces` waterfall for an F8 realtime turn shows mic-frame upload
  overlapping the hold (A8 markers), and post-release time-to-first-audio drops
  versus the buffered turn on the same utterance.
- The buffered adapter (A2) keeps all existing `gemini_live` / orchestrator
  offline tests green with no behavioural change.
- A short (<1 s) hold still produces a correct turn (A4 ring covers the
  open-latency race) with no dropped leading audio.
- Reply audio never triggers a false barge-in (mic closed on release before
  playback; A6), confirmed on a live turn.
- OpenAI Realtime: a live turn produces reply audio + user/assistant
  transcripts + clean `Done`, selected purely by setting
  `[assistant.cloud].model` to the OpenAI realtime id, with zero changes to
  `session.rs` / `run_realtime_turn` beyond Part A.
- Staged path, local TTS, and MCP server mode behave identically (no diffs in
  their tests).
- `cargo tree -p fono -i tokio-tungstenite` confirms net-zero deps for both
  parts.

## Potential Risks and Mitigations

1. **Open-latency race on short utterances (frames captured before session
   ready).**
   Mitigation: A4 bounded buffer-until-ready ring, flush-then-stream, drop-oldest
   with warn on overflow.
2. **Acoustic echo / false barge-in if the mic is open during playback.**
   Mitigation: retain strict push-to-talk (A5/A6) — mic closes on release before
   reply plays; full-duplex/AEC explicitly out of scope.
3. **Server-side VAD starting a reply mid-hold (changes the PTT feel).**
   Mitigation: disable automatic activity detection per protocol (A6); commit on
   release.
4. **Earlier WS connect surfaces failures at press, changing failure UX.**
   Mitigation: A7 clean abort via existing critical-notify, optional buffered
   fallback through the A2 adapter.
5. **Per-frame resampling artifacts on the 24 kHz OpenAI path.**
   Mitigation: B4 stateful resampler; Gemini remains identity (no resample).
6. **Regressing the currently-shipping buffered Gemini turn.**
   Mitigation: A2 keeps buffered behaviour as a degenerate stream; land Part A
   behind the existing `realtime` feature and verify the buffered tests first.
7. **Unverified OpenAI wire shapes (the mediaChunks-class risk).**
   Mitigation: offline serialization tests + B5 live verification against a real
   key before recommending as default.

## Alternative Approaches

1. **OpenAI first, live streaming second (or vice-versa).** The two parts are
   independent (the trait is the seam). Doing OpenAI first proves the
   multi-provider abstraction on the existing buffered turn with the least new
   logic; doing live streaming first delivers the latency win to Gemini users
   immediately and OpenAI inherits it. Recommended order: **live streaming first**
   (bigger felt win, exercises the press-path machinery), then OpenAI.
2. **Keep buffering, only pre-connect the WebSocket at idle (`prewarm`).** Smaller
   change — removes connect latency without restructuring capture — but realtime
   sessions are stateful/one-per-turn and WS idle-timeouts make a held-open
   pre-connect fragile; it also doesn't remove upload time. Lower payoff.
3. **Full open-mic duplex conversation (no push-to-talk).** Maximum "realtime"
   feel, but requires acoustic echo cancellation and turn-management — a major
   project with cross-platform audio complications. Deliberately rejected for
   this plan.
