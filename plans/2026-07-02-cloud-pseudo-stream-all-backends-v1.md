# Cloud Pseudo-Stream for All STT Backends

## Objective

Give **every** cloud STT backend (and Wyoming) a streaming-capable path so that
`[overlay].style = "transcript"` (live transcript mode) works regardless of the
configured provider, instead of silently falling back to the batch pipeline with a
daemon-log warning. Today only three backends stream: `local` (native whisper-rs),
`deepgram` (native WebSocket), and `groq` (pseudo-stream per ADR 0020). Gemini,
OpenAI, OpenRouter, Cartesia, ElevenLabs, Speechmatics, and Wyoming all hit the
`other =>` fallback arm in `crates/fono-stt/src/factory.rs:546-561` and return
`Ok(None)`, which routes F7 to the batch path at
`crates/fono/src/session.rs:3903-3910`.

The fix: extract the Groq pseudo-stream pump into a **generic adapter over the
batch `SpeechToText` trait**, so any backend that can transcribe a WAV can also
pseudo-stream (re-decode the trailing audio window on a cadence, stabilise the
preview through `LocalAgreement`, finalize at VAD segment boundaries).

## Assessment / Source Findings

- The streaming factory (`build_streaming_stt`, `crates/fono-stt/src/factory.rs:519-561`)
  only has arms for `Local`, `Groq if cloud_streaming`, and `Deepgram if cloud_streaming`.
  Every other backend warns and returns `None`. The daemon stores that `None` in the
  `streaming_stt` slot (`crates/fono/src/session.rs:845-867`, and on reload at
  `crates/fono/src/session.rs:1027-1049`), and `on_start_live_dictation` falls back to
  batch when the slot is empty (`crates/fono/src/session.rs:3903-3910`).
- The Groq pseudo-stream (`crates/fono-stt/src/groq_streaming.rs`) is already ~90 %
  backend-agnostic. Its pump loop (`stream_transcribe`,
  `crates/fono-stt/src/groq_streaming.rs:273-560`) is a pure state machine over
  `StreamFrame`s: buffer PCM per segment, on cadence tick re-POST the trailing window
  (in-flight cap = 1, drop-on-overlap with a skip counter), run tokens through
  `LocalAgreement` for a stable prefix, emit `TranscriptUpdate::preview`; on
  `SegmentBoundary`/`Eof` run one authoritative decode and emit
  `TranscriptUpdate::finalize`. The only Groq-specific pieces are:
  1. the request closures (`GroqRequestFn` / `GroqVerboseFn`,
     `crates/fono-stt/src/groq_streaming.rs:57-75`) that hit `groq_post_wav[_verbose]`;
  2. the verbose-response extras — `avg_logprob`/`no_speech_prob` hallucination
     filtering and the per-peer language-mismatch rerun lane;
  3. Groq-flavoured 429 handling (`summarise_429_public`, provider name in
     `rate_limit_notify::notify_once`).
- Crucially, the batch trait is already the right common denominator:
  `SpeechToText::transcribe(&[f32], sample_rate, Option<&str>) -> Transcription`
  (`crates/fono-stt/src/traits.rs:30-56`). Every backend (Gemini, OpenAI, Cartesia,
  ElevenLabs, Speechmatics, OpenRouter, Wyoming) implements it and handles its own
  encoding/auth/prompting internally. A generic adapter can therefore wrap
  `Arc<dyn SpeechToText>` directly — no per-backend WAV closures, no per-backend
  encode step (`encode_wav` at `crates/fono-stt/src/groq.rs:586` stays a Groq detail).
  `Transcription.language` gives us the detected-language echo needed for the
  allow-list post-validation that the Groq path performs.
- Cadence and windows are constants tuned for Groq
  (`crates/fono-stt/src/groq_streaming.rs:35-51`): 700 ms interval, 0.6 s preview
  minimum, 28 s trailing window (Groq's 30 s per-request cap minus headroom). Groq's
  round-trip is ~150 ms; Gemini's `generateContent` and OpenAI's `transcriptions`
  endpoints are typically 0.5–2 s+, so the per-provider default cadence must be
  slower or previews will permanently overlap (in-flight cap = 1 would drop most
  ticks — functional but wasteful). The user override already exists:
  `interactive.preview_cadence()` (`PreviewCadence::Interval(ms)` /
  `DisabledFinalizeOnly`), plumbed at `crates/fono-stt/src/factory.rs:531` and
  honoured by `with_preview_cadence`.
- Rate limiting is already generic: `crate::rate_limit_notify` keeps a global
  throttle window + once-per-session notification keyed by provider name; the pump
  checks `is_throttled()` before each preview tick
  (`crates/fono-stt/src/groq_streaming.rs:321-323`).
- Equivalence/bench harness: `crates/fono-bench/src/equivalence.rs` and ADR 0021
  exercise the Groq pseudo-stream through injected request closures
  (`GroqStreaming::with_request_fn`). Whatever refactor happens must keep that
  public constructor (or migrate the harness in the same slice).
- Cost model: a pseudo-stream multiplies request count by roughly
  `segment_seconds / cadence` + 1 finalize. ADR 0020 measured ~25 % audio-seconds
  overhead at 700 ms for Groq. Slower default cadences for slower providers also
  keep their request counts sane. `DisabledFinalizeOnly` remains the free-tier
  escape hatch.
- Docs: the STT provider table has a "Live streaming" column
  (`docs/providers.md:135`) and a Groq pseudo-stream section
  (`docs/providers.md:165`) that must be updated when new providers gain streaming.
- Related fix that already landed (2026-07-02): the batch-fallback overlay bug —
  terminal `Hidden` transitions in `on_stop_recording` / `on_cancel` /
  `spawn_pipeline` are no longer gated on `!live_preview()`, so the fallback can't
  leave the panel stuck on screen. This plan removes most *reasons* for the
  fallback, but the fallback (and that fix) stay for missing-model/missing-key
  degraded states.

### Prioritised risks/challenges (highest first)

1. **Provider latency vs cadence.** A fixed 700 ms cadence against a 1.5 s
   round-trip provider means every other tick is dropped and previews lag by
   design. Needs per-provider defaults and possibly adaptive backoff
   (measure round-trip, stretch cadence).
2. **API cost / rate limits.** Pseudo-streaming multiplies billable audio-seconds
   and requests. Defaults must be conservative for providers with strict free
   tiers; the 429 throttle + `DisabledFinalizeOnly` path must work for all
   providers, not just Groq.
3. **Refactor blast radius on the tuned Groq path.** GroqStreaming is tested,
   benched (equivalence harness), and shipped. Extracting the pump must be
   behaviour-preserving; the harness constructors must keep working.
4. **Feature parity loss.** The generic path (via `transcribe()`) has no
   logprob-based hallucination filter and no verbose rerun lane. Acceptable for
   v1 (finalize text equals what the batch path would have produced anyway), but
   the design must leave a hook so providers with richer responses can opt in.
5. **Request-duration caps differ per provider.** 28 s trailing window is
   Groq-derived; other providers have different per-request limits (and Gemini
   inline audio has a payload-size cap). Needs a per-provider window constant.

## Implementation Plan

- [ ] Task 1. **Extract the generic pump.** New module
  `crates/fono-stt/src/pseudo_streaming.rs` (behind the existing `streaming`
  feature) containing `PseudoStreaming` — a `StreamingStt` implementation that
  wraps `inner: Arc<dyn SpeechToText>` and reuses the pump loop lifted from
  `crates/fono-stt/src/groq_streaming.rs:273-560`: per-segment PCM buffer,
  cadence-gated preview decode of the trailing window with in-flight cap = 1 and
  skip counter, `LocalAgreement` stable-prefix, allow-list post-validation from
  `Transcription.language`, finalize on `SegmentBoundary`/`Eof`, generic 429
  detection feeding `rate_limit_notify` with the wrapped backend's `name()`.
  Builder surface mirrors GroqStreaming: `with_languages`, `with_lang_cache`,
  `with_cloud_rerun_on_mismatch` (v1: finalize-lane re-`transcribe` with the
  cached peer code — no logprob scoring), `with_preview_cadence`,
  `with_trailing_window`, `with_preview_min`. Rationale: one tested pump, N
  providers.

- [ ] Task 2. **Per-provider tuning table.** Add a small const table (in
  `pseudo_streaming.rs` or `defaults.rs`) mapping provider name → default cadence
  and trailing-window seconds. Starting points: Gemini 1500 ms / 25 s (inline
  payload cap headroom), OpenAI 1000 ms / 28 s, OpenRouter 1500 ms / 28 s,
  Cartesia 1000 ms / 28 s, ElevenLabs 1000 ms / 28 s, Speechmatics 1500 ms / 28 s,
  Wyoming 1000 ms / 28 s (LAN, usually fast). `interactive.streaming_interval`
  (via `preview_cadence()`) continues to override. Tune by eye during
  verification; record final numbers in the ADR (Task 7).

- [ ] Task 3. **Wire the factory.** In `build_streaming_stt`
  (`crates/fono-stt/src/factory.rs:519-561`) replace the per-backend cloud arms
  with a uniform rule: when `live_preview` is on and the backend is a cloud/batch
  backend, build the batch backend via the existing `build_*` constructor and wrap
  it in `PseudoStreaming` with the Task-2 defaults. Keep `Local` (native) and
  `Deepgram` (native WS) on their dedicated paths; keep `Groq` on `GroqStreaming`
  for now (Task 6 decides its migration). Delete the now-unreachable warn arm for
  the covered backends; keep a warn for genuinely unsupported future backends.

- [ ] Task 4. **Tests.** Unit tests in `pseudo_streaming.rs` with a mock
  `SpeechToText` (scripted responses + call counter): preview cadence respected;
  in-flight cap drops overlapping ticks and increments the skip counter;
  `LocalAgreement` prefix stability across changing tails; finalize emitted on
  `SegmentBoundary` and `Eof` with lane ordering preview…finalize; allow-list
  suppression of banned-language previews; 429 path sets the throttle and stops
  preview traffic; `DisabledFinalizeOnly` sends zero preview requests. Factory
  tests mirroring the existing Groq ones
  (`crates/fono-stt/src/factory.rs:795-825`): each cloud backend yields
  `Some(...)` when `live_preview = true` + key present, `None` when
  `live_preview = false`.

- [ ] Task 5. **Daemon-side verification of the fallback story.** With every
  cloud backend now streaming-capable, the batch fallback in
  `on_start_live_dictation` remains only for degraded states (missing key /
  missing model / factory error). Confirm the 2026-07-02 overlay-hide fix keeps
  the panel behaviour correct in those states, and that the updated warn text
  (which names supported backends) is regenerated to say "all backends" or is
  demoted appropriately once this lands.

- [ ] Task 6. **(Stretch / follow-up) Migrate GroqStreaming onto the generic
  core.** Re-express `GroqStreaming` as `PseudoStreaming` + a provider hook
  carrying the verbose-response extras (logprob hallucination filter, per-peer
  rerun scoring), keeping `with_request_fn` / `with_request_and_verbose_fn` for
  the equivalence harness (ADR 0021). Only do this once the generic pump has
  soaked; it is not required for the user-facing win.

- [ ] Task 7. **Docs, ADR, changelog.** Update the "Live streaming" column for
  every provider in `docs/providers.md:135` and generalise the pseudo-stream
  section (`docs/providers.md:165`) from "Groq" to "cloud backends", including the
  cost-multiplier explanation and the `DisabledFinalizeOnly` escape hatch. Write a
  short ADR ("generic pseudo-stream for cloud STT") superseding the scope of ADR
  0020 (which stays as the origin story), recording the per-provider cadence
  table. Add the `CHANGELOG.md` entry under the unreleased section. Update
  `ROADMAP.md` if live-transcript-for-all-providers is listed.

## Verification Criteria

- With `[overlay].style = "transcript"` and each of gemini / openai / openrouter /
  cartesia / elevenlabs / speechmatics / wyoming configured (valid key), F7 shows
  the live transcript panel with growing preview text and a correct final commit;
  **no** "falling back to batch path" warning in the daemon log.
- `interactive.streaming_interval = 0` (finalize-only) sends exactly one request
  per VAD segment for every provider.
- A provider 429 stops preview traffic for the throttle window and surfaces one
  desktop notification naming that provider.
- Binary size: no new crates (`reqwest`, `tokio`, `futures` etc. are already in
  the graph); `./tests/check.sh --size-budget` stays green.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib` all pass.

## Potential Risks and Mitigations

1. **Slow providers make previews feel laggy.** Mitigation: per-provider cadence
   defaults (Task 2) + the existing in-flight cap (never queues); document that
   Deepgram/local/Groq remain the low-latency choices.
2. **Unexpected provider cost for users who flip on transcript style.** Mitigation:
   document the multiplier in `docs/providers.md`; conservative default cadences;
   `DisabledFinalizeOnly` documented as the free-tier mode; 429 throttle caps the
   damage.
3. **Groq regression during extraction.** Mitigation: Task 3 leaves GroqStreaming
   untouched; migration (Task 6) is a separate, soak-gated follow-up with the
   equivalence harness as the safety net.
4. **Per-request payload/duration caps (Gemini inline audio).** Mitigation:
   per-provider trailing-window constants; VAD segment boundaries already bound
   segment length in practice.
5. **Preview quality without logprob filtering.** Mitigation: `LocalAgreement`
   already suppresses most flicker; finalize text is identical to today's batch
   output, so the committed result cannot regress.

## Alternative Approaches

1. **Native streaming per provider** (Speechmatics RT WebSocket, OpenAI Realtime,
   Cartesia STT WS). Best latency and cost, but one bespoke client + auth + framing
   protocol per provider — weeks of work and new failure modes vs one generic
   adapter now. Keep as per-provider upgrades later (the factory arm structure
   makes swapping pseudo→native per backend trivial).
2. **Per-backend copies of GroqStreaming** (e.g. a `gemini_streaming.rs` clone).
   Fastest for one backend, but N copies of an 800-line tuned state machine is the
   maintenance worst case; rejected.
3. **Do nothing and improve the fallback UX** (show a persistent "batch mode"
   hint on the overlay). Cheapest, but leaves the headline feature (live
   transcript) unavailable for most cloud users; rejected.
