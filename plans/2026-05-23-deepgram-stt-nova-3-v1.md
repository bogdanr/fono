# Deepgram STT (Nova 3)

## Objective

Ship a working Deepgram speech-to-text backend for Fono with `nova-3`
as the default model. The catalogue, wizard, secrets, and TTS sides
already advertise Deepgram (since v0.8.0), but `fono-stt` never grew
an implementation — `factory::build_stt` falls through to the catch-all
"not yet implemented" arm for `SttBackend::Deepgram`. Selecting
Deepgram STT in `fono setup` (wizard option index `2`) therefore
configures the user toward a backend that fails at daemon startup.

Two delivery slices, ordered so each is independently shippable:

* **Slice 1 — Batch REST.** Implements `SpeechToText` against
  `POST https://api.deepgram.com/v1/listen?model=nova-3&...`. Closes
  the "selected but unimplemented" gap end-to-end (record, transcribe,
  doctor probe, language allow-list, language cache). Uses the
  existing `reqwest` + `fono-http` stack already pulled in by the
  `deepgram` cargo feature.
* **Slice 2 — Streaming WebSocket.** Implements `StreamingStt`
  against `wss://api.deepgram.com/v1/listen?...&interim_results=true`.
  This is the differentiator vs Groq's pseudo-stream: Deepgram has a
  first-class realtime endpoint and partial / final transcripts
  arrive as JSON frames at sub-300 ms cadence. Uses the
  `tokio-tungstenite` dep the `deepgram` feature already declares
  but doesn't currently consume.

Slice 2 is gated on Slice 1 landing cleanly; both should ship in the
same release window so docs don't have to caveat "batch only for a
release or two."

## Implementation Plan

### Slice 1 — Batch REST backend

- [ ] Task 1.1. **Add `crates/fono-stt/src/deepgram.rs`** modelled
  on `groq.rs`. Public type `DeepgramStt` with the same builder
  surface as `GroqStt`: `new(key)`, `with_model(key, model)`,
  `with_languages`, `with_prompts`, `with_cloud_rerun_on_mismatch`,
  `with_lang_cache`. Reuse `groq::warm_client` (move it to a shared
  `http_util` module if cross-module access is awkward, or
  re-export it `pub(crate)` from `groq.rs`). Crate-private const
  `BACKEND_KEY: &str = "deepgram"` for the language cache.
  *Rationale*: keeps the cross-backend ergonomics identical so the
  factory and the language-stickiness rerun lane drop in without
  special cases.

- [ ] Task 1.2. **Implement `SpeechToText::transcribe`** by POSTing
  the raw WAV body (the `encode_wav` helper already in `groq.rs` is
  the right encoder — re-export it `pub(crate)`). Request shape:
  - URL: `https://api.deepgram.com/v1/listen` with query params
    `model=<self.model>`, `smart_format=true`,
    `punctuate=true`, and conditional `language=<code>` /
    `detect_language=true` derived from `LanguageSelection`
    (mirrors the Groq lane: forced → `language=code`,
    auto → `detect_language=true`, allow-list → omit
    `language` and let post-validation rerun handle it).
  - Headers: `Authorization: Token <api_key>` (literal `Token`,
    not `Bearer` — same convention as the existing Deepgram TTS
    client at `crates/fono-tts/src/deepgram.rs:1-15`),
    `Content-Type: audio/wav`.
  - Body: raw WAV bytes (no multipart — Deepgram's listen
    endpoint accepts the audio as the request body).
  *Rationale*: Deepgram's REST API is bytes-in, JSON-out — simpler
  than Groq's multipart form. Sending WAV (instead of raw PCM +
  query-string sample rate) avoids encoding-mismatch bugs and keeps
  the encoder shared with Groq.

- [ ] Task 1.3. **Parse the response**. Define
  `DeepgramListenResponse` with the minimum shape needed:
  `results.channels[0].alternatives[0].transcript` (String) and
  `results.channels[0].detected_language` (Option<String>, present
  only when `detect_language=true`). Map to
  `Transcription { text, language, duration_ms: None }`. Drop
  `confidence` and word-level timings; if Slice 2's hallucination
  filter ever needs them they can be added then.
  *Rationale*: keep the parser surface minimal so upstream schema
  drift (Deepgram adds fields routinely) doesn't break us.

- [ ] Task 1.4. **HTTP error handling parity with Groq**:
  - 401/403 → bail with a "key rejected" message and route through
    `fono_core::critical_notify::Stage::Stt` so the existing
    cascade-cap notification fires.
  - 429 → call `crate::rate_limit_notify::mark_rate_limited()` +
    `notify_once("deepgram", …)` with a short remediation hint.
  - 5xx / network → context-rich `anyhow::Error`.
  *Rationale*: cloud STT backends share one observability surface
  (notifications, log targets); Deepgram joining it costs nothing
  but lets users self-diagnose without grepping logs.

- [ ] Task 1.5. **Implement `prewarm`** as a cheap authed
  `GET https://api.deepgram.com/v1/projects` request (mirrors
  Groq's `GET /v1/models` warm). Drain body, ignore the response —
  the point is the TCP+TLS handshake.

- [ ] Task 1.6. **Language allow-list rerun**. Apply the same
  post-validation pattern as `groq::transcribe`: if the detected
  language is outside the allow-list and
  `cloud_rerun_on_language_mismatch` is on, run one re-request per
  peer with `language=<peer>` forced. Deepgram doesn't expose
  per-segment `avg_logprob` in batch mode, so the tiebreak signal
  is `results.channels[0].alternatives[0].confidence` (0..1, higher
  is better). Pick the highest-confidence rerun and record it in
  the language cache.
  *Rationale*: keeps Deepgram on equal footing with Groq /
  OpenAI / OpenRouter / Cartesia for multilingual users; the
  confidence-based pick is a documented behaviour we can describe
  in `docs/providers.md`.

- [ ] Task 1.7. **Wire the factory.** Add a `build_deepgram` arm to
  `crates/fono-stt/src/factory.rs:98-110` matching the Groq /
  OpenAI / OpenRouter / Cartesia shape, gated by
  `#[cfg(feature = "deepgram")]` with the not-compiled-in fallback
  the other arms use. Drop `SttBackend::Deepgram` from the
  catch-all `_other` error.

- [ ] Task 1.8. **Bump the catalogue default from `nova-2` to
  `nova-3`** at `crates/fono-core/src/provider_catalog.rs:372`.
  Update the test in `crates/fono-stt/src/defaults.rs:36` to assert
  `nova-3`. Update the wizard literal at
  `crates/fono/src/wizard.rs:1705` (currently `"nova-2"`) and the
  matrix in `docs/providers.md:126` (`nova-2`, `nova-3` → keep
  `nova-3` as primary, list `nova-2` and `nova-3` as available).
  *Rationale*: the user explicitly asked for Nova 3 and it is now
  the production-default Deepgram model (lower latency + better
  WER on English than nova-2; multilingual support comparable).

- [ ] Task 1.9. **Unit tests** in `deepgram.rs`:
  - `wav_header_is_44_bytes` parity check on the shared encoder.
  - `build_url_*` set: forced language, auto-detect, allow-list,
    custom model override, `smart_format` always present.
  - `parse_response_extracts_text_and_language` against a tiny
    pinned JSON fixture.
  - `auth_header_uses_token_prefix_not_bearer` — pin the literal
    `Authorization: Token …` string. This is the historical
    footgun the TTS client already has.
  - `rate_limit_summarisation` if Deepgram's 429 body has structure
    worth surfacing; else a "body excerpted" path test.

- [ ] Task 1.10. **Integration coverage**. Extend
  `crates/fono/tests/provider_switching.rs` (or the closest
  existing fixture) to assert that `fono use stt deepgram` with
  `DEEPGRAM_API_KEY` set walks the factory without erroring. No
  live network calls — the test only exercises construction +
  one-shot `Arc<dyn SpeechToText>` shape.

- [ ] Task 1.11. **Doctor + wizard surface.** Verify
  `fono doctor` prints the active backend label on the existing
  STT row when Deepgram is configured. Wizard already lists
  Deepgram (index `2` of the secondary STT options at
  `wizard.rs:1693`); confirm the model literal it writes lands as
  `nova-3` after Task 1.8.

- [ ] Task 1.12. **Docs.** Update `docs/providers.md`:
  - STT table row: keep `Streaming = yes` (Slice 2 wires it; Slice
    1 alone leaves it `no` — adjust to match what ships in this
    PR).
  - Add a *Deepgram batch STT* subsection mirroring the Groq one:
    endpoint, auth header gotcha, language stickiness behaviour,
    model menu (`nova-3`, `nova-2`).
  Update `CHANGELOG.md` under `[Unreleased] ## Added`.

### Slice 2 — Streaming WebSocket

- [ ] Task 2.1. **Add `crates/fono-stt/src/deepgram_streaming.rs`**
  modelled on `groq_streaming.rs` shape but using a *real*
  WebSocket (no pseudo-stream re-POSTs). Implements
  `crate::streaming::StreamingStt` behind
  `#[cfg(all(feature = "deepgram", feature = "streaming"))]`.

- [ ] Task 2.2. **WebSocket connection lifecycle.** Open
  `wss://api.deepgram.com/v1/listen` with query params
  `model=nova-3`, `encoding=linear16`, `sample_rate=<frame_sr>`,
  `channels=1`, `interim_results=true`, `smart_format=true`,
  and `vad_events=true`. Auth via the `Authorization: Token …`
  header attached to the upgrade request. Send PCM s16le frames
  as binary WS messages straight from the capture pump. Send
  `{"type":"CloseStream"}` JSON on segment boundary / EOF to
  flush a final transcript before the socket closes.
  *Rationale*: matches Deepgram's documented client contract;
  16 kHz linear16 mono is what the capture pipeline already
  produces.

- [ ] Task 2.3. **Frame translation.** Map incoming `Results`
  messages to the existing `TranscriptUpdate` /
  `UpdateLane::{Preview, Finalize}` enum. `is_final: false` →
  `Preview`; `is_final: true` → `Finalize`. Drop empty-transcript
  frames silently. Wire `vad_events` (`SpeechStarted` /
  `UtteranceEnd`) into the same `StreamFrame::SegmentBoundary`
  signal that the local Whisper streaming path emits, so the
  overlay's "Pondering…" + auto-stop hook (slice 4 of the
  auto-stop plan) sees Deepgram-driven boundaries without
  special-casing the backend.

- [ ] Task 2.4. **Reconnect + backoff.** Network blip mid-session:
  reconnect once with the same query params, replay the
  *last* finalized text as a hidden `Preview` so the user-visible
  transcript stays continuous. Hard-fail and surface
  `Stage::Stt` notification on the second consecutive failure
  (don't wedge the FSM).

- [ ] Task 2.5. **Wire `build_streaming_stt`** in
  `factory.rs:392-430` with a new `SttBackend::Deepgram if
  cloud_streaming` arm that constructs `DeepgramStreaming`. Mirror
  the Groq cadence-injection pattern even though Deepgram's cadence
  is server-driven — pass it through for parity (and so a future
  rate-limited preview throttle has a knob).

- [ ] Task 2.6. **Unit tests** with no network:
  - `build_url_includes_all_required_query_params`.
  - `auth_header_pinning` (same `Token` gotcha as Slice 1).
  - `is_final_routes_to_finalize_lane` against pinned JSON.
  - `vad_event_emits_segment_boundary` against pinned JSON.

- [ ] Task 2.7. **Docs update.** Flip
  `docs/providers.md` STT table row `Streaming` to `yes` and add
  a *Deepgram streaming dictation* subsection that calls out: no
  per-utterance cost surprise (Deepgram bills by audio seconds, so
  it's *cheaper* than Groq pseudo-stream, not the other way
  round); `[overlay].style = "transcript"` enables it; partials
  arrive at ~150 ms cadence.

- [ ] Task 2.8. **Manual dogfood**. Live test on the maintainer's
  host with Transcript overlay + dictation + assistant. Confirm:
  partials paint smoothly, finalize arrives within ~300 ms of
  release, language switching mid-session works when
  `general.languages` has 2+ peers, network drop + reconnect
  recovers without losing audio.

## Verification Criteria

### Slice 1

- `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace --tests
  --lib` all green per the AGENTS.md pre-commit gate.
- `cargo test -p fono-stt deepgram::tests` runs the new fixtures.
- `fono use stt deepgram` followed by `fono record` produces a
  transcript on a host with `DEEPGRAM_API_KEY` set. Error path
  (missing key) prints the catalogue-aware remediation hint.
- `fono doctor` reports `STT : deepgram (nova-3)` (or whatever
  the configured model is).
- The catch-all "not yet implemented" arm in
  `factory.rs:105-109` no longer mentions Deepgram.
- `defaults.rs:36` test asserts `nova-3`.

### Slice 2

- All pre-commit gate steps remain green with `streaming`
  + `deepgram` features enabled together.
- `fono use stt deepgram` with `[overlay].style = "transcript"`
  routes through the streaming path (verified via the
  `streaming STT not yet supported for backend deepgram` warning
  *not* appearing in logs).
- Manual session: partials visible in the overlay; assistant flow
  reaches `Speaking` within the usual latency budget.

## Potential Risks and Mitigations

1. **Authorization-header footgun.** Deepgram uses
   `Authorization: Token <k>`, not `Bearer`. Easy to copy-paste from
   the Groq client and break silently (HTTP 401 with no client-side
   hint that the prefix is wrong).
   Mitigation: pin the literal in a unit test (Task 1.9) and mirror
   the comment block from the existing TTS client at
   `crates/fono-tts/src/deepgram.rs:1-15`.

2. **Nova-3 multilingual coverage differs from Nova-2.** Nova-3
   currently supports a smaller language matrix at full quality
   than Nova-2 in some regions. Users with `general.languages`
   containing a Nova-3-untrained code may see degraded results
   without a clear signal.
   Mitigation: leave `nova-2` documented as an available model
   override in `docs/providers.md` (Task 1.12) so users can pin
   `[stt.cloud].model = "nova-2"` on a Deepgram-untrained
   language. Add a short note in the wizard's model-selection
   prompt if and only if it grows multi-model awareness later;
   for v1 the docs path is enough.

3. **WebSocket reconnect storms.** A flaky LAN can produce a
   reconnect loop that hammers Deepgram and burns minutes.
   Mitigation: hard-cap at 1 reconnect per session (Task 2.4);
   second failure routes to `critical_notify` and falls the FSM
   back to `Idle`.

4. **Schema drift on the `results` envelope.** Deepgram adds
   fields routinely. A strict deserializer breaks on the next
   harmless addition.
   Mitigation: derive `Deserialize` with `#[serde(default)]` on
   every optional field and don't model the full schema —
   only the three or four fields we actually consume (Task 1.3,
   Task 2.3).

5. **Bench drift.** The release-cloud-equivalence CI gate (ADR
   0021) is Groq-only today. Adding Deepgram to the gate later is
   in scope of the test infrastructure roadmap, not this plan —
   but we should not silently regress the Groq baseline by sharing
   the `warm_client` constructor with Deepgram if their TLS
   profiles disagree.
   Mitigation: keep `warm_client` parameters identical; if Deepgram
   needs a different setting (e.g. lower `pool_max_idle_per_host`),
   add a builder argument rather than mutating the shared constant.

## Alternative Approaches

1. **WebSocket-first, no batch.** Skip Slice 1 and ship only the
   streaming client. *Trade-off*: cleaner end-state — one transport
   for both batch (`fono record`) and live (`overlay = transcript`).
   *Why rejected*: the existing `SpeechToText` trait is batch-shaped;
   wedging WS into it forces an awkward "open-send-close per
   utterance" lifecycle that pays handshake cost on every press
   and inflates Deepgram bills. Two backends with different
   transports is the honest design.

2. **Use the third-party `deepgram` crate** instead of hand-rolling
   the client. *Trade-off*: fewer LOC, official SDK shape.
   *Why rejected*: pulls a new dependency tree (the crate has its
   own `tokio-tungstenite` + `reqwest` versioning), adds GPL-3.0
   audit surface, and the API surface we need is small enough that
   the saved LOC is in the noise. Mirrors the decision recorded for
   Groq / OpenAI / Cartesia.

3. **Make `nova-3` the only supported model**, drop `nova-2`.
   *Trade-off*: smaller catalogue, fewer footguns.
   *Why rejected*: `nova-2` is still GA, has broader language
   coverage, and users with stable pipelines on `nova-2` shouldn't
   be silently broken by a config-defaults bump. Keep `nova-2` as
   an override, default to `nova-3`.
