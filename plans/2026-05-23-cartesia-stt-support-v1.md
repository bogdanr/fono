# Cartesia STT Support

## Objective

Light up Cartesia as a working cloud STT backend, matching the
provider's existing Cartesia TTS integration. Most surrounding
plumbing (config enum, env var, catalogue entry, wizard prompts,
tray submenu, doctor block, `critical_notify` classifier) is
already in place — `SttBackend::Cartesia` exists and Cartesia
already declares `SttDefaults { model: "sonic-transcribe" }` in
`crates/fono-core/src/provider_catalog.rs:397-414`. The only
reason `fono use stt cartesia` fails today is that the
`build_stt` factory falls through to the "not yet implemented"
arm at `crates/fono-stt/src/factory.rs:104-108`.

Phase 1 (this plan): ship a **batch HTTP client** that mirrors the
shape of `GroqStt` / `OpenAiStt` / Cartesia TTS, behind a new
`cartesia` cargo feature on `fono-stt`. WebSocket streaming is
deferred to a Phase 2 slice (mirror `groq_streaming.rs`).

Expected outcome:

- `cargo run -p fono -- use stt cartesia` succeeds and the next
  dictation press transcribes via `api.cartesia.ai`.
- Wizard secondary-STT picker (`crates/fono/src/wizard.rs:1143-1197`)
  routes to a working backend instead of a factory error.
- Tray "STT backend" submenu lists Cartesia whenever
  `CARTESIA_API_KEY` is present in `secrets.toml` (already true
  via `configured_stt_backends` at `crates/fono-core/src/providers.rs:336-356`)
  and selecting it now works.
- No new workspace dependencies; `deny.toml` untouched.

## Implementation Plan

- [ ] Task 1. **Verify Cartesia STT API surface against current docs.**
  Confirm the batch endpoint URL, multipart field names, model id
  (`sonic-transcribe` vs `ink-whisper`), language parameter
  format, auth header capitalisation (`X-Api-Key` vs `X-API-Key`
  — the TTS path uses `X-Api-Key` at
  `crates/fono-tts/src/cartesia.rs:258, 286`, the key validator
  uses `X-API-Key` at `crates/fono/src/wizard.rs:1853`; pick the
  documented spelling), and the response JSON shape. Update
  `provider_catalog.rs:404`'s `SttDefaults { model: ... }` only if
  the docs say something other than `"sonic-transcribe"`.
  Rationale: every downstream piece of the plan is parameterised
  on these strings, and we have already been bitten by drift
  between catalogue defaults and live API in the Cartesia TTS
  path.

- [ ] Task 2. **Add a `cartesia` feature flag to `fono-stt`.** In
  `crates/fono-stt/Cargo.toml` add
  `cartesia = ["dep:reqwest", "dep:fono-http"]` next to the
  existing `groq` / `openai` rows, matching the dormant
  `deepgram` flag already declared on line 25. No new deps;
  `reqwest` and `fono-http` are already optional workspace deps.
  Rationale: keeps cloud-cost-free builds (Wyoming + local) opt-out
  consistent with the existing per-provider gating pattern.

- [ ] Task 3. **Create `crates/fono-stt/src/cartesia.rs` modeled on
  `groq.rs`.** Module exports `pub struct CartesiaStt`,
  `pub(crate) const BACKEND_KEY: &str = "cartesia"`, and a builder
  surface (`with_model`, `with_languages`, `with_prompts`,
  `with_lang_cache`). HTTP path: `POST` to the documented batch
  endpoint with `multipart/form-data` carrying a WAV blob
  (reuse `crate::groq::encode_wav` — make it `pub(crate)` if it
  isn't already), plus `model`, optional `language` (omit on
  auto-detect — mirror `LanguageSelection::Auto` branch at
  `crates/fono-stt/src/groq.rs:379-384`), and optional
  `prompt` if Cartesia accepts one (drop the field otherwise).
  Use `crate::openai_compat::warm_client()` for the
  `reqwest::Client` to share the HTTP/2 keep-alive pool. Apply
  the `X-Api-Key` + `Cartesia-Version` headers exactly like the
  TTS client (`crates/fono-tts/src/cartesia.rs:258-260`). Error
  string format `cartesia STT returned {status}: {body}` so the
  existing `critical_notify` classifier
  (`crates/fono-core/src/critical_notify.rs:120-167`) picks up
  401/403/429 paths without modification.
  Rationale: structural symmetry with the existing cloud backends
  keeps the audit surface small and lets every cross-cutting
  feature (rate-limit notifications, language cache, doctor
  reporting) plug in for free.

- [ ] Task 4. **Decide explicitly what to do without Whisper
  segment scores.** Cartesia's Ink/Sonic models almost certainly
  do **not** return `avg_logprob` / `no_speech_prob` per segment.
  Two Whisper-specific behaviours in `groq.rs` therefore cannot
  transplant: the per-peer rerun chooser
  (`pick_best_peer` at `crates/fono-stt/src/groq.rs:469-495`)
  and the hallucination filter (`groq.rs:236-274`). For Phase 1:
  ignore `cfg.cloud_rerun_on_mismatch` for Cartesia (still accept
  it in `build_cartesia` for symmetry, but never invoke the rerun
  path), and skip the segment-confidence filter. Record the
  limitation in the module doc-comment and in
  `docs/providers.md` so users know Cartesia STT will not
  language-rerun.
  Rationale: shipping a degraded-but-correct Phase 1 is better
  than shipping a half-port of Whisper logic that silently
  no-ops.

- [ ] Task 5. **Wire `SttBackend::Cartesia` into the factory.** In
  `crates/fono-stt/src/factory.rs`, change the match arm at
  lines 98-110 to call a new feature-gated `build_cartesia`
  helper (mirroring `build_groq` at `factory.rs:226-253`). The
  helper resolves the API key with the existing
  `resolve_cloud(cfg, secrets, &SttBackend::Cartesia, "cartesia")`
  flow (defaults already produce
  `model="sonic-transcribe"`, `api_key_ref="CARTESIA_API_KEY"`).
  Re-export the module at `crates/fono-stt/src/lib.rs:17-28` as
  `#[cfg(feature = "cartesia")] pub mod cartesia;` and add the
  factory's `Cartesia =>` arm.
  Rationale: the factory is the only file the daemon's `build_stt`
  call site reads from; this is the gate that flips Cartesia from
  "rejected with anyhow!" to "instantiated".

- [ ] Task 6. **Turn the feature on in the binary.** In
  `crates/fono/Cargo.toml:68` extend the `fono-stt` features list
  to include `"cartesia"` so the default release binary ships
  with it (the surrounding comment at `Cargo.toml:69-76` confirms
  cloud backends should be default-on).
  Rationale: the catalogue and wizard already advertise Cartesia
  STT to users, so the default binary must back that promise.

- [ ] Task 7. **Add focused factory tests.** In the test module of
  `crates/fono-stt/src/factory.rs`, add
  `cartesia_with_key_succeeds` (mirrors the TTS factory test at
  `crates/fono-tts/src/factory.rs:334-341`) verifying that a
  populated `Secrets` returns `Ok` and the backend's `name()`
  reports `"cartesia"`. Add `cartesia_without_key_errors` to
  pin the missing-key path through `resolve_key`'s canonical
  message. Within the new `cartesia.rs` module, add a tiny
  unit-test pair that round-trips the JSON response deserialiser
  through `serde_json` with a captured-fixture body, covering
  both detected-language and missing-language cases.
  Rationale: protects the wire shape against future Cartesia
  schema drift; matches the test density of `groq.rs` /
  `openai.rs`.

- [ ] Task 8. **Provider switching / wizard integration tests.**
  `crates/fono/tests/provider_switching.rs` already references
  `SttBackend::Cartesia`; verify it now exercises the success
  path with a stub key in env. In
  `crates/fono/tests/wizard_primary_flow.rs` add a sibling case to
  the existing `cartesia_tts_only_path` (at lines 97-104) that
  drives the secondary-STT picker into Cartesia and asserts
  `cfg.stt.backend == SttBackend::Cartesia` and the api-key-ref
  recorded points at `CARTESIA_API_KEY`.
  Rationale: catches regressions where a future wizard refactor
  drops Cartesia from the secondary-STT list, which is the
  user-facing entry point.

- [ ] Task 9. **Documentation sweep.** Update
  `docs/providers.md` Cartesia section to list STT alongside TTS
  with the chosen model name; add a note that Cartesia STT does
  **not** support the language-mismatch rerun heuristic and so
  per-language hints in `[general].languages` go straight to the
  API as a hint rather than being verified against a peer model.
  Add a `CHANGELOG.md` `[Unreleased]` entry under `### Added`:
  "Cartesia STT (batch) — set `[stt].backend = "cartesia"` or use
  `fono use stt cartesia`". Update `ROADMAP.md` "In progress" if
  Cartesia STT was previously listed there.
  Rationale: docs/changelog are part of the release contract per
  `AGENTS.md`'s "every release" rule.

- [ ] Task 10. **Manual verification.** Run with a real
  `CARTESIA_API_KEY`:
  1. `fono keys add CARTESIA_API_KEY` (if not already stored).
  2. `fono use stt cartesia`.
  3. Quick-tap F7 and dictate "the quick brown fox".
  4. Confirm overlay transitions Recording → Processing → text
     injection works and the daemon log shows a single POST to
     `api.cartesia.ai`.
  5. Disable the network and confirm the failure path raises the
     critical notification with `provider="cartesia"`.
  6. With `[general].languages = ["en", "ro"]`, dictate in
     Romanian and confirm `language` flows through to the API
     (or auto-detect engages if Cartesia ignores the hint).
  Rationale: the wire-shape verification cannot be done from
  unit tests alone; this is the gate before tagging.

## Verification Criteria

- `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, and `cargo test --workspace
  --tests --lib` all green (per `AGENTS.md` pre-commit gate).
- `cargo run -p fono -- use stt cartesia` completes without
  error on a config where `CARTESIA_API_KEY` is present.
- A live dictation session through Cartesia produces a non-empty
  transcript on the standard English fixture.
- 401/403/429 errors surface as a single critical-urgency
  desktop notification with `provider="cartesia"` and no cascade
  storm (cascade cap already guards this — see
  `crates/fono-core/src/critical_notify.rs` `SESSION_HAS_FIRED`).
- `fono doctor` prints a Cartesia row under STT providers with
  the resolved model and a green check next to the key.
- Tray "STT backend" submenu shows Cartesia and selecting it
  reloads the orchestrator without restart.
- No new entries in `deny.toml`; `cargo tree -p fono-stt
  --features cartesia` shows zero new transitive crates beyond
  what `groq` already pulls.
- `provider_catalog.rs`'s `no_orphan_cloud_variants` test
  remains green.

## Potential Risks and Mitigations

1. **Cartesia STT response schema diverges from expectations.**
   Public docs may have drifted; the catalogue's
   `"sonic-transcribe"` model id may be stale (the TTS path has
   re-litigated similar model-id drift more than once).
   Mitigation: Task 1 verifies the API up front against current
   Cartesia docs *before* any code lands; deserializer is
   `#[serde(default)]` on every optional field so unknown
   additions don't fail parsing. Capture a real response body
   and pin it as a JSON fixture in the module's unit tests.

2. **No Whisper-style segment scores → broken assumptions
   downstream.** `cfg.cloud_rerun_on_mismatch = true` users will
   silently get no rerun behaviour on Cartesia. Mitigation:
   document the limitation in `docs/providers.md` and in the
   module's doc-comment; consider logging a one-time `info!`
   when `cloud_rerun_on_mismatch` is set but the backend is
   Cartesia, naming the limitation.

3. **Auth header capitalisation mismatch.** TTS uses `X-Api-Key`,
   wizard probe uses `X-API-Key`. If the STT endpoint is
   case-sensitive and disagrees with one of them, either the
   wizard validator will green-light a key the runtime rejects
   (or vice versa). Mitigation: Task 1 confirms the documented
   spelling; Task 10 cross-verifies that wizard validation and
   runtime transcription share the same outcome on the same key.

4. **Hallucination filter regression.** The Groq path filters
   "Thanks for watching!" and similar artefacts via segment
   scores. Cartesia output may pass through unfiltered. If
   Cartesia hallucinates on silence we'll inject garbage into
   the focused window. Mitigation: rely on the empty-transcript
   recovery path at `crates/fono/src/audio_recovery.rs` for
   true-silence cases; for Sonic-Transcribe-specific
   hallucinations, log a follow-up issue rather than blocking
   Phase 1.

5. **WebSocket streaming path not implemented.** Users in the
   tray-selected "Transcript (live preview)" mode who pick
   Cartesia STT will silently fall back to the batch path (per
   the existing `supports_streaming() -> false` default).
   Mitigation: document the fallback in `docs/providers.md` and
   open a follow-up plan for the streaming variant.

## Alternative Approaches

1. **Vendor a thin shared `fono-stt::cloud_common` module first.**
   Move `warm_client`, `summarise_429`, `RATE_LIMIT_HINT`, and
   `encode_wav` out of `groq.rs` so Cartesia (the third copy)
   doesn't continue the cut-and-paste pattern. Trade-off: extra
   diff churn now in exchange for cheaper future Deepgram /
   AssemblyAI ports. Defer unless this plan grows naturally into
   that refactor.

2. **Implement WebSocket streaming as part of Phase 1.** Mirrors
   `groq_streaming.rs` and slots into `build_streaming_stt`
   (`crates/fono-stt/src/factory.rs:362-400`). Trade-off:
   roughly doubles the LoC and the test surface, and pushes
   live-API verification of two wire formats into the same
   slice. Recommended Phase 2; do not combine.

3. **Reuse the dormant `deepgram` feature flag's WebSocket dep
   plumbing and ship Deepgram STT alongside Cartesia.** Both
   providers' WebSocket streaming is similarly shaped, and the
   `tokio-tungstenite` dep is already declared. Trade-off:
   widens the user's surprise budget for a single release;
   conflates two unrelated bug surfaces. Out of scope for this
   plan but recorded as a future grouping opportunity.
