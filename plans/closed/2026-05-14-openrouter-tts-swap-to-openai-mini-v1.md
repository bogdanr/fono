# OpenRouter TTS: swap default from Kokoro to OpenAI GPT‑4o Mini TTS

## Status: Completed

## Objective

Replace the OpenRouter TTS default from `hexgrad/kokoro-82m` to
`openai/gpt-4o-mini-tts-2025-12-15` so multilingual users get
native-sounding speech out of the box, while keeping Kokoro as a
tracked future option for *both* local and cloud paths with shared
language/voice routing.

## Implementation Plan

### Phase 1 — Catalogue swap (core change)

- [x] Task 1. **Update the OpenRouter TTS catalogue entry** at
  `crates/fono-core/src/provider_catalog.rs:374-384` to set
  `model = "openai/gpt-4o-mini-tts-2025-12-15"`,
  `default_voice = "coral"` (warm, balanced, multilingual-friendly per
  OpenAI's voice docs), `endpoint = TtsEndpoint::OpenAiCompat` unchanged
  (`response_format = "pcm"`, 24 kHz). Rationale: keeps the existing
  `OpenAiCompatTtsClient` wire shape intact; the swap is purely a
  string change.

- [x] Task 2. **Mirror the default in `fono_tts::defaults::default_cloud_model`**
  for `"openrouter"` so wizard-generated configs match the catalogue.
  Rationale: the catalogue and the defaults function must stay in
  lock-step or the assistant pipeline picks one model and the wizard
  prints another.

- [x] Task 3. **Refresh the OpenRouter tagline and badges** in the
  catalogue entry so the wizard advertises "OpenRouter (OpenAI Mini TTS)
  — natural multilingual voices" instead of the current Kokoro phrasing.
  Rationale: the wizard's primary-pick copy is the single biggest
  influence on what users expect.

### Phase 2 — Wizard + tests

- [x] Task 4. **Update wizard test pins** for the OpenRouter primary
  row in `crates/fono/src/wizard.rs` (the assertions that hash the
  primary-capability table) to reflect the new model id and voice.
  Rationale: those tests were tightened in the previous session and
  will fail on any catalogue model rename.

- [x] Task 5. **Add unit coverage in `openai_compat.rs`** asserting that
  `openrouter_client(api_key, None, None)` resolves to
  `default_model = "openai/gpt-4o-mini-tts-2025-12-15"` and
  `default_voice = "coral"`. Mirrors the existing assertions at
  `crates/fono-tts/src/openai_compat.rs:497-504`.

- [x] Task 6. **Verify the wizard's voice override path** still flows
  through unchanged: when a user supplies a custom voice in the
  customize branch, the catalogue default voice is overridden, the
  language step from the previous plan is now *optional* (multilingual
  out of the box), and the per-language voice map is dropped from the
  OpenRouter row. Rationale: OpenAI Mini TTS does language switching
  natively, so the Kokoro-specific complexity disappears.

### Phase 3 — Documentation + CHANGELOG

- [x] Task 7. **Update `docs/providers.md` OpenRouter subsection**
  documenting the new default model, the OpenAI voice catalogue
  (`alloy`, `echo`, `fable`, `onyx`, `nova`, `shimmer`, `sage`, `coral`,
  `ash`, `verse`), per-character pricing, and a one-line note that
  Kokoro is deferred to a future local/cloud-symmetric backend.

- [x] Task 8. **Add a `## Changed` bullet to `CHANGELOG.md` under the
  upcoming release** noting the swap, per the AGENTS.md hard rule that
  every release ships a changelog entry before tagging.

### Phase 4 — File the Kokoro-parity follow-up (deferred work)

- [x] Task 9. **Create `plans/2026-05-14-kokoro-local-and-cloud-parity-v1.md`**
  scoping the future Kokoro work with explicit symmetry requirements:
  - A new `fono-tts` local backend (ONNX-runtime-based Kokoro inference,
    `misaki` G2P bindings, or a pure-Rust phonemizer if available),
    Apache-licensed end-to-end so it can be the offline default per
    `docs/decisions/0004-default-models.md`.
  - A shared `KokoroVoiceRouter` (lang → voice helper from the prior
    plan `plans/2026-05-14-openrouter-kokoro-multilingual-voice-routing-v1.md`)
    that is consumed by **both** the new local backend and the existing
    OpenRouter passthrough, so picking Kokoro local vs cloud gives the
    same audio output for the same `(text, lang, voice)` triple.
  - Wizard UX that presents voice + language as one unified setting,
    independent of whether the backend is local or cloud.
  - Model download flow (catalogue + checksum + cache dir under
    `~/.cache/fono/models/kokoro-82m-v1.0/`).
  - Latency target: local first-token < 200 ms on a 4-core x86 CPU.
- [x] Task 10. **Cross-link the prior multilingual-voice-routing plan**
  (`plans/2026-05-14-openrouter-kokoro-multilingual-voice-routing-v1.md`)
  from the new Kokoro-parity plan and mark its catalogue-fix portions
  as superseded by this swap, while keeping the voice-router design
  notes as reusable artifacts.

### Phase 5 — Optional follow-up: opt-in Gemini 3.1 Flash TTS

- [x] Task 11. **File a smaller follow-up plan** for adding
  `google/gemini-3.1-flash-tts-preview` as an *opt-in* OpenRouter voice
  for users who want 70+ language coverage or the inline audio tags
  (`[whispers]`, `[laughs]`, etc.). Surfaces only in the wizard's
  customize step; never the default. Rationale: it's a Preview model
  with 20× the per-turn cost; it would be irresponsible to ship as
  default but a real loss not to expose for power users.

## Verification Criteria

- Synthesising "Bonjour, comment allez-vous?" via the wizard-default
  OpenRouter TTS produces natural French audio without an English
  accent, with no per-call language argument needed.
- Synthesising the same text in Romanian, Spanish, German, and Mandarin
  produces audibly native pronunciation from a single voice choice
  (no voice swap required between languages).
- The OpenRouter primary-pick row in the wizard still shows TTS as
  `✓` and the user's first dictation round-trips through STT (Whisper
  Turbo) → LLM → TTS (Mini TTS) end-to-end with a single
  `OPENROUTER_API_KEY`.
- `cargo build -p fono`, `cargo test -p fono-tts -p fono`, and
  `cargo clippy --workspace --no-deps` are clean with the updated
  catalogue, with no new warnings.
- `CHANGELOG.md` carries a `## Changed` bullet under the upcoming
  release section before tag time, per the AGENTS.md rule.
- The new Kokoro-parity follow-up plan exists in `plans/` and
  cross-links the prior multilingual-voice-routing plan.

## Potential Risks and Mitigations

1. **OpenAI Mini TTS preview-stage features could change.**
   The model id is dated (`2025-12-15`), but OpenRouter could
   re-route or deprecate the snapshot.
   Mitigation: pin the dated snapshot id rather than a rolling alias;
   add a runtime-probe field to the catalogue entry if/when OR exposes
   a stable health endpoint; track upstream changes via the existing
   provider-rev process.

2. **Per-character pricing changes mid-release.**
   $0.60 / M characters today. A future price hike could surprise users.
   Mitigation: surface live pricing in the wizard prompt by hitting
   `GET /api/v1/models/{id}/endpoints` at first-run (already used for
   validation); cache for the session. Out of scope for the swap itself
   but worth keeping on the radar.

3. **Voice IDs are OpenAI-specific.**
   `coral`, `verse`, `ash` only exist on OpenAI's TTS family. If a
   user later swaps `[tts.cloud] provider` to a different OpenRouter
   model, the saved voice may be invalid.
   Mitigation: when the wizard rewrites `[tts.cloud] model`, also
   reset `voice` to the catalogue default for the new model unless
   the user explicitly opts to keep the override (already the wizard
   pattern for similar settings).

4. **Kokoro users who liked the previous default lose it.**
   Mitigation: document the override path in `docs/providers.md` and
   the CHANGELOG so existing users can pin
   `[tts.cloud] model = "hexgrad/kokoro-82m"` if they prefer. The
   Phase-4 follow-up plan delivers proper local+cloud Kokoro parity
   later.

5. **Wizard tests will break if the primary-row hash isn't updated
   carefully.**
   Mitigation: regenerate the expected strings from a local run
   before committing; the existing 28 wizard tests have well-scoped
   pins so the diff should be minimal.

## Alternative Approaches

1. **Swap to `google/gemini-3.1-flash-tts-preview` instead.**
   Best language coverage (70+) and adds inline audio-tag steering,
   but it's a Preview snapshot, priced at $1/M input + $20/M output
   tokens (≈20× more per turn for typical Fono workloads), routed via
   Vertex (higher latency), and our client cannot expose the headline
   advanced features without new wire-format work. Better as an
   opt-in (Task 11) than as the new default.

2. **Keep Kokoro and ship only the language→voice routing from the
   previous plan.**
   Solves the English-accent bug at zero per-turn cost, but Kokoro's
   non-English voices are graded `C` or worse, French has only one
   voice, and the user explicitly asked to swap. Defer to the
   Phase‑4 local+cloud Kokoro parity work instead.

3. **Introduce both OpenAI Mini TTS and Gemini Flash TTS at once, with
   a wizard choice.**
   Larger blast radius, more wizard surface to test, and the user
   asked which is *better* — defaulting to one and documenting the
   other as opt-in is the smaller, clearer change.

4. **Wait for a hypothetical multilingual Kokoro v2 / fine-tune.**
   No public roadmap exists; the user is hitting the problem now;
   not actionable.
