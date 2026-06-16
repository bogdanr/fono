# Speechmatics STT + TTS Integration

## Objective

Add Speechmatics as a fully wired cloud provider for **both** speech-to-text
and text-to-speech, matching the architecture Fono already uses for Deepgram /
Cartesia / Groq. Speechmatics already exists as a half-baked stub
(`SttBackend::Speechmatics` enum variant + a catalogue placeholder) but is
*not* constructible — `build_stt` falls into the catch-all `other => Err(...)`
arm. There is no TTS variant at all. This plan first-classes both directions.

### Scope decisions (assumptions, since the request is "implement … for both")

- **STT transport = realtime WebSocket** (`wss://eu.rt.speechmatics.com/v2`),
  used in a *buffered one-shot* mode for `SpeechToText::transcribe` and,
  optionally, as a true `StreamingStt` for live preview. Rationale: Fono's
  dictation model records-then-transcribes a buffer; the Batch API is an async
  *job* API (POST job → poll status → fetch transcript) whose polling latency
  is wrong for push-to-talk, whereas the realtime socket returns finals as the
  buffer drains and reuses the `tokio-tungstenite` dependency Deepgram already
  brings in. The Batch job API is documented as an Alternative below.
- **TTS transport = REST** `POST https://preview.tts.speechmatics.com/generate/<voice>`
  with `?output_format=pcm_16000`, mirroring `fono-tts/src/deepgram.rs`
  almost exactly (raw int16 LE mono @ 16 kHz → f32). Default voice `sarah`.
- **No new third-party crates.** STT uses the existing
  `reqwest` + `tokio-tungstenite`; TTS uses the existing `reqwest`. So
  `deny.toml` needs **no** change (verify during implementation).
- **TTS is English-only and in preview.** The endpoint lives on a `preview.`
  subdomain and supports four English voices (`sarah`, `theo`, `megan`,
  `jack`). Per-language voice routing must treat Speechmatics TTS as
  English-only; this is documented as a known limitation, not a bug.
- **Auth = `Authorization: Bearer <key>`** for every Speechmatics surface
  (batch, realtime handshake header, and TTS) — unlike Deepgram's literal
  `Token` prefix. A unit test should pin `Bearer` so a copy-paste from the
  Deepgram client can't regress it.

## Reference touch-points (verified during research)

- STT trait: `crates/fono-stt/src/traits.rs:30-85`
- STT factory dispatch: `crates/fono-stt/src/factory.rs:91-114` (batch) and
  `crates/fono-stt/src/factory.rs:424-466` (streaming)
- STT module registry: `crates/fono-stt/src/lib.rs:17-34`
- STT feature flags: `crates/fono-stt/Cargo.toml:15-52`
- Closest STT analog (batch REST + rerun lane): `crates/fono-stt/src/deepgram.rs`
- TTS trait: `crates/fono-tts/src/traits.rs:17-46`
- TTS factory dispatch: `crates/fono-tts/src/factory.rs:42-58`
- TTS module registry: `crates/fono-tts/src/lib.rs:14-49`
- TTS feature flags: `crates/fono-tts/Cargo.toml:15-46`
- Closest TTS analog (REST + int16→f32): `crates/fono-tts/src/deepgram.rs`
- Enum + env + arrays: `crates/fono-core/src/config.rs:290-313`,
  `crates/fono-core/src/providers.rs:134-189` and `:317-328`
- Catalogue stub to replace: `crates/fono-core/src/provider_catalog.rs:438-452`
- Default model accessor: `crates/fono-stt/src/defaults.rs:24-26`
- Binary feature wiring: `crates/fono/Cargo.toml:82` (STT) and `:91` (TTS)
- CLI help strings: `crates/fono/src/cli.rs:400` and `:1405`
- Bench capability lists (already name speechmatics):
  `crates/fono-bench/src/capabilities.rs:51,189`

## Implementation Plan

### A. Shared config & provider metadata (`fono-core`)

- [ ] A1. In `crates/fono-core/src/config.rs`, add a `Speechmatics` variant to
  the `TtsBackend` enum (alongside the existing STT variant which is already
  present). Keep `#[serde(rename_all = "lowercase")]` so it serialises as
  `speechmatics`. Rationale: the TTS factory and `parse_tts_backend` switch on
  this enum; without the variant TTS can't be selected.
- [ ] A2. In `crates/fono-core/src/providers.rs`, extend the TTS helpers for the
  new variant: `tts_backend_str`, `parse_tts_backend`, `tts_key_env`
  (→ `SPEECHMATICS_API_KEY`), `tts_requires_key` (→ true), and add it to
  `all_tts_backends`, bumping the array length `[TtsBackend; 8]` → `[…; 9]`.
  STT helpers already cover Speechmatics (`stt_key_env` → `SPEECHMATICS_API_KEY`
  at `:101`, `parse_stt_backend` at `:47`, `all_stt_backends` at `:280`); no STT
  change needed here.
- [ ] A3. In `crates/fono-core/src/provider_catalog.rs`, replace the
  "STT-only stub" Speechmatics entry (`:438-452`) with a real entry: keep
  `stt: Some(SttDefaults { model: … })` (use the realtime default operating
  point/model identifier Speechmatics expects — see B-notes), and add
  `tts: Some(TtsDefaults { model: "", default_voice: "sarah", endpoint: …,
  runtime_probe: false })`. Add a `TtsEndpoint` variant for Speechmatics if the
  existing enum can't express the `preview.tts.speechmatics.com/generate/<voice>`
  shape; otherwise reuse a generic one. Update the tagline to drop "(planned)".
- [ ] A4. Update the catalogue/provider round-trip + "no orphans" unit tests in
  the same files so the new TTS variant and the de-stubbed entry pass. The
  `tts_roundtrip` test (`providers.rs:570`) iterates `all_tts_backends`, so it
  covers A2 automatically once the array grows.

### B. STT backend (`fono-stt`)

- [ ] B1. Add a `speechmatics` feature to `crates/fono-stt/Cargo.toml` pulling
  `dep:reqwest`, `dep:fono-http`, and `dep:tokio-tungstenite` (same set as the
  `deepgram` feature at `:25`). Rationale: realtime transport needs the
  WebSocket client; reqwest is used for the optional prewarm/REST probe.
- [ ] B2. Create `crates/fono-stt/src/speechmatics.rs` (SPDX header line 1)
  implementing `SpeechToText`:
  - Builder mirroring `DeepgramStt`: `with_model`, `with_languages`,
    `with_prompts` (captured for forward-compat; Speechmatics has a
    `additional_vocab`/custom-dictionary field the `context_hint` could later
    feed), `with_cloud_rerun_on_mismatch`, `with_lang_cache`, plus a
    configurable region endpoint (default `eu`).
  - `transcribe()`: open the realtime WS with the `Authorization: Bearer`
    header, send a `StartRecognition` message describing the audio
    (`audio_format`: raw `pcm_f32le` or `pcm_s16le` at `sample_rate`,
    `transcription_config` with the resolved language / operating point), stream
    the buffered PCM as binary `AddAudio` frames, send `EndOfStream`, collect
    `AddTranscript` finals until `EndOfTranscript`, concatenate, and return a
    `Transcription { text, language, duration_ms }`.
  - Honour `LanguageSelection` (Forced / AllowList / Auto) the same way Deepgram
    does, including the post-validate + cloud-rerun lane if the detected
    language is out of the allow-list (Speechmatics supports automatic language
    detection in its config). Reuse `BACKEND_KEY = "speechmatics"` for the
    `LanguageCache`.
  - `name()` → `"speechmatics"`; `is_local()` stays false; implement a cheap
    `prewarm()` (authed GET against the SaaS base or a no-op if none is cheap).
  - Unit tests: `Bearer` auth header (NOT `Token`), `StartRecognition` JSON
    shape, transcript-concatenation from a canned message sequence, empty-input
    → empty transcript, and a `default_model_matches_catalogue` drift guard.
- [ ] B3. Register the module in `crates/fono-stt/src/lib.rs`:
  `#[cfg(feature = "speechmatics")] pub mod speechmatics;` and re-export the
  type if other crates need it (follow the Deepgram pattern).
- [ ] B4. Wire `SttBackend::Speechmatics` into `build_stt`
  (`factory.rs:100-114`) with a feature-gated `build_speechmatics` helper +
  `#[cfg(not(feature))]` error stub, exactly mirroring `build_deepgram`
  (`:346-373`). Update the catch-all error message's "pick …" list to include
  `speechmatics`.
- [ ] B5. (Optional, gated on `streaming`) Implement `StreamingStt` for the same
  client in a `speechmatics_streaming.rs` (or inline) so live preview works,
  and add a `SttBackend::Speechmatics if cloud_streaming =>` arm in
  `build_streaming_stt` (`factory.rs:438-466`), mirroring the Deepgram streaming
  branch (`:447-450`, `:509-531`). The realtime socket already produces partials
  (`AddPartialTranscript`) which map naturally to `TranscriptUpdate` preview
  lane. If deferred, the existing catch-all `Ok(None)` already falls back to
  batch gracefully — no change required for a first cut.
- [ ] B6. Add factory tests mirroring the Deepgram ones
  (`factory.rs:648-672`): cloud-optional-with-env-key succeeds and reports
  `name() == "speechmatics"`, and missing-key yields the
  `SPEECHMATICS_API_KEY` + `fono keys add` remediation message.

### C. TTS backend (`fono-tts`)

- [ ] C1. Add a `speechmatics` feature to `crates/fono-tts/Cargo.toml` pulling
  `dep:reqwest` + `dep:fono-http` (same as `deepgram` at `:28`).
- [ ] C2. Create `crates/fono-tts/src/speechmatics.rs` (SPDX header line 1)
  implementing `TextToSpeech`, modelled on `fono-tts/src/deepgram.rs`:
  - Constructor `new(api_key, model_override)` reading the default voice
    (`sarah`) and base endpoint from the catalogue.
  - `synthesize()`: empty text → empty PCM early-return; otherwise
    `POST {base}/generate/{voice}?output_format=pcm_16000` with
    `Authorization: Bearer`, body `{"text": text}`, parse the raw int16 LE bytes
    into f32 via a `pcm_i16_le_to_f32` helper (copy the one in deepgram.rs:120),
    return `TtsAudio { pcm, sample_rate: 16000 }`.
  - Resolve `voice` from the `voice` hint / config / catalogue default, clamping
    to the four supported English voices; ignore `lang` (English-only) but log a
    debug note if a non-English lang hint arrives.
  - `name()` → `"speechmatics"`; `native_sample_rate()` → `16_000`;
    `prewarm()` no-op (no documented cheap GET on the preview host).
  - Unit tests: `Bearer` auth header, URL shape with the voice + output_format
    query, request body is bare `{"text":…}`, empty-text short-circuit, and
    native rate = 16000.
- [ ] C3. Register the module in `crates/fono-tts/src/lib.rs`:
  `#[cfg(feature = "speechmatics")] pub mod speechmatics;`.
- [ ] C4. Wire `TtsBackend::Speechmatics` into `build_tts`
  (`factory.rs:48-58`) with a feature-gated `build_speechmatics` + error stub.
  Extend the `resolve_cloud` / `resolve_key` feature-gate `cfg(any(...))` lists
  (`factory.rs:137-197`) to include `speechmatics`, and add the display-name arm
  to `resolve_key`'s match (`:184-191`).
- [ ] C5. Add factory tests mirroring `deepgram_with_key_succeeds` /
  `groq_missing_key_errors_clearly` (`factory.rs:409-427`) for Speechmatics.

### D. Binary wiring & surfacing

- [ ] D1. In `crates/fono/Cargo.toml`, add `"speechmatics"` to the `fono-stt`
  feature list (`:82`) and the `fono-tts` feature list (`:91`) so default
  release builds ship the provider. If `streaming` STT (B5) is implemented, no
  extra feature is needed — it is already gated by the existing `streaming`
  feature.
- [ ] D2. In `crates/fono/src/cli.rs`, confirm the STT help string already lists
  `speechmatics` (`:400`, `:1405` — it does) and add `speechmatics` to the TTS
  backend help/validation list wherever the TTS choices are enumerated. Verify
  `fono use stt speechmatics` and `fono use tts speechmatics` resolve through
  `parse_stt_backend` / `parse_tts_backend`.
- [ ] D3. Verify the setup wizard surfaces Speechmatics for both STT and TTS.
  The wizard is catalogue-driven via `configured_stt_backends` /
  `configured_tts_backends`; once A1–A3 land and `SPEECHMATICS_API_KEY` is in
  `secrets.toml`, both menus should include it automatically. Add an explicit
  wizard branch only if the wizard hard-codes provider lists rather than reading
  the catalogue (confirm during implementation).
- [ ] D4. Confirm `fono doctor` reachability enumeration picks up the new
  provider (it iterates `all_stt_backends` / `all_tts_backends`, so A2 covers
  it) and that bench `capabilities.rs` (which already names `speechmatics`)
  still compiles.

### E. Docs, licensing, and changelog

- [ ] E1. Update `docs/providers.md` with a Speechmatics section: required
  `SPEECHMATICS_API_KEY`, STT realtime regions (eu/us), the four English TTS
  voices, the English-only + preview-endpoint caveats, and `fono use`
  invocations.
- [ ] E2. Verify `deny.toml` needs no change (no new crates). If any new crate
  *is* introduced, add it to `deny.toml` and confirm GPL-3.0 compatibility per
  the AGENTS hard rule before committing.
- [ ] E3. Add a `CHANGELOG.md` entry and move/annotate the matching `ROADMAP.md`
  item **only at release time**, per the AGENTS release rules — not as part of
  the feature commit unless this ships in a tagged release.

## Verification Criteria

- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
  warnings`, and `cargo test --workspace --tests --lib` all pass (the AGENTS
  pre-commit gate).
- `fono use stt speechmatics` and `fono use tts speechmatics` both succeed and
  persist to config; `fono doctor` shows Speechmatics reachability for both
  directions when `SPEECHMATICS_API_KEY` is set.
- A live STT round-trip transcribes a recorded buffer via the realtime socket
  and returns non-empty text with a detected language.
- A live TTS round-trip on `sarah` returns 16 kHz mono PCM that plays back
  cleanly through `fono-audio::playback`.
- New unit tests pin the `Bearer` auth header for both clients, the
  `StartRecognition` / TTS request shapes, and catalogue-default drift guards.
- Missing-key paths produce the canonical `SPEECHMATICS_API_KEY` + `fono keys
  add` remediation message for both STT and TTS.

## Potential Risks and Mitigations

1. **Realtime WS protocol complexity (StartRecognition / AddAudio / framing).**
   Mitigation: implement and test against a canned message-sequence fixture
   first (pure parsing/serialisation unit tests, no network), exactly as the
   Deepgram tests parse canned JSON; only then exercise the live socket.
2. **TTS endpoint is a `preview.` host with no SLA and may change/break.**
   Mitigation: keep the base URL in the catalogue (single edit point), gate the
   whole backend behind the `speechmatics` feature, and document the preview
   caveat in `docs/providers.md` and the module doc-comment.
3. **TTS is English-only — per-language voice routing has no non-English
   fallback.** Mitigation: document the limitation; ignore the `lang` hint and
   always use an English voice; do not register Speechmatics as a per-language
   TTS option in any auto-routing layer.
4. **Auth-prefix footgun (Bearer vs Deepgram's Token).** Mitigation: a unit
   test pins the exact `Bearer ` header string for both new clients (the
   Deepgram code carries the inverse test as precedent).
5. **Batch latency if the realtime path is rejected in review.** Mitigation: the
   Alternative below documents the job-based Batch API as a drop-in for
   `transcribe()` if maintainers prefer plain REST over a WebSocket.
6. **Array-length and exhaustive-match churn.** Mitigation: bumping
   `[TtsBackend; 8]`→`9` and adding match arms will surface every consumer at
   compile time; rely on `-D warnings` + the round-trip tests to catch misses.

## Alternative Approaches

1. **Batch job API for STT instead of realtime WS.** `POST /v2/jobs/`
   (multipart: audio + `config` JSON) → poll `GET /v2/jobs/{id}` → fetch
   `GET /v2/jobs/{id}/transcript`. Pros: plain `reqwest`, no WebSocket, simpler
   error surface. Cons: multi-request polling latency is poor for push-to-talk
   dictation and there is no live-preview path. Best reserved as a fallback for
   very long buffers.
2. **Realtime WS as a true `StreamingStt` from day one (fold B5 into B2).**
   Pros: live preview + one-shot share a single code path. Cons: larger initial
   surface; the local-agreement/preview-lane plumbing adds risk to the first
   cut. Recommended only if live preview for Speechmatics is an explicit
   requirement now.
3. **TTS via the forthcoming official SDK.** The docs note SDKs are "not
   available yet"; a thin REST client (this plan) is the only viable option
   today and avoids a new dependency + license review.
