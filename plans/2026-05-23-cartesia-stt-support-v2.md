# Cartesia STT Support

> v2 supersedes v1. Changes: corrected default model to **`ink-whisper`**
> (the batch endpoint only accepts the `ink-whisper` family;
> `ink-2` is WebSocket-only and is deferred to Phase 2);
> corrected batch endpoint to `POST https://api.cartesia.ai/stt`
> (not `/stt/transcribe`); recorded the actual response schema
> (`text`, `language`, `duration`, `words[]`); recorded that
> `language` is an ISO-639-1 query parameter; pinned the
> auth header to `X-Api-Key` for consistency with the existing
> Cartesia TTS client.

## Objective

Light up Cartesia as a working cloud STT backend, matching the
provider's existing Cartesia TTS integration. Most surrounding
plumbing (config enum, env var, catalogue entry, wizard prompts,
tray submenu, doctor block, `critical_notify` classifier) is
already in place — `SttBackend::Cartesia` exists and Cartesia
already declares `SttDefaults { model: "sonic-transcribe" }` in
`crates/fono-core/src/provider_catalog.rs:404`. The only reason
`fono use stt cartesia` fails today is that the `build_stt`
factory falls through to the "not yet implemented" arm at
`crates/fono-stt/src/factory.rs:104-108`.

**Phase 1 (this plan):** ship a **batch HTTP client** against
`POST https://api.cartesia.ai/stt` with model **`ink-whisper`**.
This is what the Cartesia batch endpoint actually accepts —
`ink-2` requires the WebSocket / Realtime path and is the
explicit recommendation for "user turn-detection" voice-agent
flows, not for fire-and-forget dictation.

**Phase 2 (deferred):** WebSocket streaming via
`wss://api.cartesia.ai/stt/turns/websocket` with model
`ink-2`. Turn-based events (`turn.start` /
`turn.update` / `turn.end`) map onto Fono's existing
`StreamingStt` trait deltas. Separate plan when Phase 1 has
soaked.

Expected outcome:

- `fono use stt cartesia` succeeds and the next dictation
  press transcribes via `api.cartesia.ai`.
- Wizard secondary-STT picker
  (`crates/fono/src/wizard.rs:1143-1197`) routes to a working
  backend instead of a factory error.
- Tray "STT backend" submenu lists Cartesia whenever
  `CARTESIA_API_KEY` is present in `secrets.toml` (already
  true via `configured_stt_backends` at
  `crates/fono-core/src/providers.rs:336-356`) and selecting
  it now works.
- No new workspace dependencies; `deny.toml` untouched.

## Pinned API facts (verified against `docs.cartesia.ai`
`2026-03-01`)

| Aspect | Value |
|---|---|
| Endpoint | `POST https://api.cartesia.ai/stt` |
| Model (Phase 1 default) | **`ink-whisper`** |
| Model family allowed | `ink-whisper` family only |
| Auth header | `X-Api-Key: $CARTESIA_API_KEY` (matches TTS client) |
| Version header | `Cartesia-Version: 2026-03-01` |
| Body | `multipart/form-data` with `file` field (audio bytes) and `model` field |
| Language hint | ISO-639-1 query parameter (`?language=ro`), enum-validated server-side; defaults to `en` when omitted |
| Audio formats accepted | flac, m4a, mp3, mp4, mpeg, mpga, oga, ogg, wav, webm |
| Response | `{ text: string, language: string|null, duration: number|null, words?: [{word, start, end}] }` |
| Word timestamps | Opt-in via `timestamp_granularities[]=word` (not needed Phase 1) |
| Pricing | 1 credit per 2 seconds of audio |
| Phase 2 streaming endpoint | `WSS https://api.cartesia.ai/stt/turns/websocket`, model `ink-2`, turn-based events |

## Implementation Plan

- [ ] Task 1. **Correct the catalogue's stale model id.** Change
  `crates/fono-core/src/provider_catalog.rs:404` from
  `SttDefaults { model: "sonic-transcribe" }` to
  `SttDefaults { model: "ink-whisper" }`. `sonic-transcribe`
  does not exist on the current Cartesia STT models page; the
  batch endpoint explicitly requires the `ink-whisper` family
  and `ink-2` is WebSocket-only. The `no_orphan_cloud_variants`
  test (`provider_catalog.rs:619-661`) continues to pass — only
  the string literal changes.
  Rationale: every downstream piece of the plan reads the model
  default through `crates/fono-stt/src/defaults.rs:24-26`; one
  edit at the source of truth fixes wizard, tray, doctor, and
  factory in a single stroke.

- [ ] Task 2. **Add a `cartesia` feature flag to `fono-stt`.**
  In `crates/fono-stt/Cargo.toml` add
  `cartesia = ["dep:reqwest", "dep:fono-http"]` next to the
  existing `groq` / `openai` rows. No new deps; `reqwest` and
  `fono-http` are already optional workspace deps.
  Rationale: keeps cloud-cost-free builds (Wyoming + local)
  opt-out consistent with the existing per-provider gating
  pattern.

- [ ] Task 3. **Create `crates/fono-stt/src/cartesia.rs`
  modeled on `groq.rs`.** Module exports:
  - `pub struct CartesiaStt` with the standard fields
    (`api_key`, `model`, `client`, `languages`,
    `cloud_rerun_on_mismatch` (ignored — see Task 4),
    `lang_cache`, `prompts`).
  - `pub(crate) const BACKEND_KEY: &str = "cartesia"`.
  - Builder methods symmetric with `GroqStt` (`with_model`,
    `with_languages`, `with_prompts`, `with_lang_cache`,
    `with_cloud_rerun_on_mismatch`).

  Wire shape:
  - `POST https://api.cartesia.ai/stt`.
  - Headers `X-Api-Key: ${api_key}` + `Cartesia-Version: 2026-03-01`.
    Pin the version constant in a module-level `const` mirroring
    `crates/fono-tts/src/cartesia.rs:57-63` so the TTS and STT
    versions move together.
  - `multipart/form-data` body: `file` (WAV bytes from
    `crate::groq::encode_wav` — promote to `pub(crate)` if it
    is not already), `model` (e.g. `"ink-whisper"`).
  - When `LanguageSelection::Forced(lang)` is set, append
    `?language={lang}` as a **query parameter** (per docs;
    not a form field). When the selection is `Auto`, omit the
    query parameter (server defaults to `en` — document this
    in the module doc-comment).
  - Use `crate::openai_compat::warm_client()` for the
    `reqwest::Client` to share the HTTP/2 keep-alive pool.

  Response deserialization (`serde`):
  ```text
  struct Resp {
      text: String,
      #[serde(default)] language: Option<String>,
      #[serde(default)] duration: Option<f64>,
      // `words` deliberately ignored Phase 1.
  }
  ```
  Convert to `Transcription { text, language, duration_ms:
  duration.map(|s| (s * 1000.0) as u64) }`.

  Error string format `cartesia STT returned {status}: {body}`
  so the existing `critical_notify` classifier
  (`crates/fono-core/src/critical_notify.rs:120-167`) picks up
  401/403/429 paths without modification.

  Rationale: structural symmetry with the existing cloud
  backends keeps the audit surface small and lets every
  cross-cutting feature (rate-limit notifications, language
  cache, doctor reporting) plug in for free.

- [ ] Task 4. **Decide explicitly what to do without
  Whisper-style segment scores.** Cartesia's `/stt` response
  schema is `{ text, language, duration, words[] }` — no
  `avg_logprob`, no `no_speech_prob`. Two Whisper-specific
  behaviours in `groq.rs` therefore cannot transplant: the
  per-peer rerun chooser (`pick_best_peer` at
  `crates/fono-stt/src/groq.rs:469-495`) and the hallucination
  filter (`groq.rs:236-274`). For Phase 1:
  - **Ignore `cfg.cloud_rerun_on_mismatch` for Cartesia** —
    still accept it in `build_cartesia` for symmetry, but
    log a single `info!` at construction time when it is
    `true` ("cartesia STT does not support multi-language
    rerun; flag ignored") and never invoke the rerun path.
  - **Skip the segment-confidence filter.** Document the
    limitation in the module doc-comment.
  - **Forward the cached/forced language** via the
    `?language=` query param so users who explicitly set
    `[general].languages = ["ro"]` still get accurate
    transcription on the first call.

  Rationale: shipping a degraded-but-correct Phase 1 is better
  than shipping a half-port of Whisper logic that silently
  no-ops; the warn-once log keeps the limitation visible.

- [ ] Task 5. **Wire `SttBackend::Cartesia` into the factory.**
  In `crates/fono-stt/src/factory.rs`:
  - Add `#[cfg(feature = "cartesia")] pub mod cartesia;` to
    `crates/fono-stt/src/lib.rs:17-28`.
  - Add a new feature-gated `build_cartesia` helper mirroring
    `build_groq` (`factory.rs:226-253`); resolve key + model
    through the existing `resolve_cloud(cfg, secrets,
    &SttBackend::Cartesia, "cartesia")` flow — defaults
    now produce `model="ink-whisper"`,
    `api_key_ref="CARTESIA_API_KEY"`.
  - Change the match arm at `factory.rs:98-110` to call
    `build_cartesia` instead of falling through to the
    "not yet implemented" error.

  Rationale: the factory is the only file the daemon's
  `build_stt` call site reads from; this is the gate that
  flips Cartesia from "rejected with anyhow!" to "instantiated".

- [ ] Task 6. **Turn the feature on in the binary.** In
  `crates/fono/Cargo.toml:68` extend the `fono-stt` features
  list to include `"cartesia"` so the default release binary
  ships with it (the surrounding comment at `Cargo.toml:69-76`
  confirms cloud backends should be default-on).
  Rationale: the catalogue and wizard already advertise
  Cartesia STT to users, so the default binary must back that
  promise.

- [ ] Task 7. **Add focused factory tests.** In the test module
  of `crates/fono-stt/src/factory.rs`, add
  `cartesia_with_key_succeeds` (mirrors the TTS factory test
  at `crates/fono-tts/src/factory.rs:334-341`) verifying that
  a populated `Secrets` returns `Ok` and the backend's `name()`
  reports `"cartesia"`. Add `cartesia_without_key_errors` to
  pin the missing-key path through `resolve_key`'s canonical
  message. Within the new `cartesia.rs` module, add a tiny
  unit-test pair that round-trips a captured-fixture response
  body through `serde_json` for both detected-language and
  missing-language cases. Include a fixture where
  `duration: null` to lock in the `Option<f64>` handling.
  Rationale: protects the wire shape against future Cartesia
  schema drift; matches the test density of `groq.rs` /
  `openai.rs`.

- [ ] Task 8. **Provider switching / wizard integration tests.**
  `crates/fono/tests/provider_switching.rs` already references
  `SttBackend::Cartesia`; verify it now exercises the success
  path with a stub key in env. In
  `crates/fono/tests/wizard_primary_flow.rs` add a sibling case
  to the existing `cartesia_tts_only_path` (at lines 97-104)
  that drives the secondary-STT picker into Cartesia and
  asserts `cfg.stt.backend == SttBackend::Cartesia`,
  `cfg.stt.cloud.unwrap().model == "ink-whisper"`, and the
  api-key-ref recorded points at `CARTESIA_API_KEY`.
  Rationale: catches regressions where a future wizard
  refactor drops Cartesia from the secondary-STT list, which
  is the user-facing entry point.

- [ ] Task 9. **Documentation sweep.** Update
  `docs/providers.md` Cartesia section to list STT alongside
  TTS with model `ink-whisper`; add a note that:
  1. Cartesia STT does **not** support the language-mismatch
     rerun heuristic — per-language hints flow straight to the
     API as a `?language=` query param.
  2. Cartesia STT does **not** support live preview
     (Transcript waveform style) in Phase 1; users who pick
     that overlay style with Cartesia selected will fall back
     to batch transcription. Streaming arrives in Phase 2 via
     the `ink-2` realtime endpoint.

  Add a `CHANGELOG.md` `[Unreleased]` entry under `### Added`:
  > Cartesia STT (batch) — set `[stt].backend = "cartesia"` or
  > use `fono use stt cartesia`. Defaults to the `ink-whisper`
  > model.

  Update `ROADMAP.md`: move any pre-existing "Cartesia STT"
  bullet from Planned/In-progress into the active slice; add a
  new "Cartesia STT (streaming, ink-2)" entry under Planned.
  Rationale: docs/changelog are part of the release contract
  per `AGENTS.md`'s "every release" rule.

- [ ] Task 10. **Audit Cartesia TTS auth header capitalisation.**
  The TTS client uses `X-Api-Key` (`crates/fono-tts/src/cartesia.rs:258,
  286`); the wizard key validator uses `X-API-Key`
  (`crates/fono/src/wizard.rs:1853`); Cartesia's docs use both
  spellings across pages. HTTP header names are case-insensitive
  per [RFC 7230 §3.2](https://www.rfc-editor.org/rfc/rfc7230#section-3.2)
  so neither is broken, but the new STT client should match the
  TTS spelling (`X-Api-Key`) for grep-ability, and the wizard's
  one outlier should be flipped to match. One-line change in
  `crates/fono/src/wizard.rs:1853`.
  Rationale: cosmetic but cheap; reduces the cognitive load of
  the next person who searches the workspace for the header
  name.

- [ ] Task 11. **Manual verification.** Run with a real
  `CARTESIA_API_KEY`:
  1. `fono keys add CARTESIA_API_KEY` (if not already stored).
  2. `fono use stt cartesia`.
  3. Quick-tap F7 and dictate "the quick brown fox".
  4. Confirm overlay transitions Recording → Processing → text
     injection works and the daemon log shows a single POST to
     `api.cartesia.ai/stt`.
  5. Disable the network and confirm the failure path raises
     the critical notification with `provider="cartesia"`.
  6. With `[general].languages = ["en", "ro"]`, dictate in
     Romanian and confirm `language=ro` flows through to the
     API and the response's `language` field matches.
  7. With `[overlay].waveform_style = "transcript"` selected
     in the tray, confirm Cartesia falls back to batch (no
     streaming) and a one-line `info!` log notes the fallback.
  Rationale: the wire-shape verification cannot be done from
  unit tests alone; this is the gate before tagging.

## Verification Criteria

- `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, and `cargo test --workspace
  --tests --lib` all green (per `AGENTS.md` pre-commit gate).
- `cargo run -p fono -- use stt cartesia` completes without
  error on a config where `CARTESIA_API_KEY` is present.
- A live dictation session through Cartesia produces a
  non-empty transcript on the standard English fixture using
  `ink-whisper`.
- 401/403/429 errors surface as a single critical-urgency
  desktop notification with `provider="cartesia"` and no
  cascade storm (cascade cap already guards this — see
  `crates/fono-core/src/critical_notify.rs` `SESSION_HAS_FIRED`).
- `fono doctor` prints a Cartesia row under STT providers
  with model `ink-whisper` and a green check next to the key.
- Tray "STT backend" submenu shows Cartesia and selecting it
  reloads the orchestrator without restart.
- No new entries in `deny.toml`; `cargo tree -p fono-stt
  --features cartesia` shows zero new transitive crates beyond
  what `groq` already pulls.
- `provider_catalog.rs`'s `no_orphan_cloud_variants` test
  remains green.

## Potential Risks and Mitigations

1. **Cartesia STT response schema diverges from expectations.**
   The fixture-pinned `{ text, language, duration, words[] }`
   shape may grow new optional fields. Mitigation: the
   deserializer is `#[serde(default)]` on every optional
   field; unknown additions don't fail parsing. Capture a real
   response body during Task 11 and pin it as a JSON fixture
   in the module's unit tests.

2. **`language` as query param vs form field.** The docs
   layout strongly suggests query parameter; if the live API
   rejects that and expects a multipart field, Phase 1 will
   silently always transcribe as `en`. Mitigation: Task 11
   step 6 explicitly tests Romanian. If the query-param form
   is rejected, swap to multipart and update Task 3's
   doc-comment.

3. **No Whisper-style segment scores → broken assumptions
   downstream.** `cfg.cloud_rerun_on_mismatch = true` users
   will silently get no rerun behaviour on Cartesia.
   Mitigation: Task 4 logs a one-time `info!` at construction
   time naming the limitation; `docs/providers.md` documents it.

4. **Hallucination filter regression.** The Groq path filters
   "Thanks for watching!" and similar artefacts via segment
   scores. Cartesia output may pass through unfiltered.
   Mitigation: rely on the empty-transcript recovery path at
   `crates/fono/src/audio_recovery.rs` for true-silence cases;
   for Sonic-/Ink-Whisper-specific hallucinations, log a
   follow-up issue rather than blocking Phase 1.

5. **WebSocket streaming path not implemented.** Users in the
   tray-selected "Transcript (live preview)" mode who pick
   Cartesia STT will silently fall back to the batch path (per
   the existing `supports_streaming() -> false` default).
   Mitigation: Task 9 documents the fallback in
   `docs/providers.md`; Task 11 step 7 verifies the fallback
   actually happens cleanly; a follow-up Phase 2 plan picks up
   the `ink-2` realtime endpoint.

6. **`ink-whisper` family is positioned as "older models" in
   the docs.** Cartesia could deprecate it once the realtime
   `ink-2` path matures. Mitigation: the model is parameterised
   through the catalogue, so when Cartesia ships a successor
   batch model the change is one literal in `provider_catalog.rs`.
   Phase 2 (WebSocket + `ink-2`) is the long-term
   future-proofing.

## Alternative Approaches

1. **Skip Phase 1 batch, go directly to Phase 2 WebSocket +
   `ink-2`.** Trade-off: gets us onto Cartesia's strategic
   model immediately but roughly triples LoC, doubles the
   wire-format audit surface, and conflates two unrelated bug
   surfaces in a single release. Not recommended.

2. **Vendor a thin shared `fono-stt::cloud_common` module
   first.** Move `warm_client`, `summarise_429`,
   `RATE_LIMIT_HINT`, and `encode_wav` out of `groq.rs` so
   Cartesia (the third copy) doesn't continue the
   cut-and-paste pattern. Trade-off: extra diff churn now in
   exchange for cheaper future Deepgram / AssemblyAI ports.
   Defer unless this plan grows naturally into that refactor.

3. **Reuse the dormant `deepgram` feature flag's WebSocket dep
   plumbing and ship Deepgram STT alongside Cartesia.** Both
   providers' WebSocket streaming is similarly shaped, and the
   `tokio-tungstenite` dep is already declared. Trade-off:
   widens the user's surprise budget for a single release;
   conflates two unrelated bug surfaces. Out of scope for this
   plan but recorded as a future grouping opportunity.
