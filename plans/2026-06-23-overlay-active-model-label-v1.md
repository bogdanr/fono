# Overlay Active-Model Label

## Objective

Show the user which model is currently handling their request (STT, Polish/cleanup,
Assistant, TTS) as a *discreet*, right-aligned label on the existing overlay status
line — sharing the row with `RECORDING` / `PONDERING` / `ASSISTANT` / `THINKING` /
`SPEAKING` / `POLISHING`. The label must:

- render in a **dim gray** so it stays subordinate to the status word on the left,
- show a **user-friendly model name** (not raw internal ids like `large-v3-turbo`,
  `gpt-5.4-nano`, `qwen3.5-0.8b-q4_k_m`),
- reflect the model relevant to the *current pipeline stage* (the STT model while
  recording, the cleanup LLM while polishing, the assistant LLM while thinking, the
  TTS voice while speaking).

## Assessment / Source Findings

- The status word is drawn left-aligned on the top row by the renderer at
  `crates/fono-overlay/src/renderer.rs:1862-1913`, using `state_label()`
  (`crates/fono-overlay/src/renderer.rs:108-122`) and the dim text color
  `COLOR_TEXT_DIM = 0xCCAA_AAB2` (`crates/fono-overlay/src/renderer.rs:85`). This is the
  exact row the user is pointing at. Implication: a right-aligned string on the same
  `status_baseline` is the correct, minimal place to draw the model name.
- The renderer is a pure function of `RendererState` + `OverlayState`
  (`crates/fono-overlay/src/renderer.rs:1636-1674`). It already owns a font and a
  glyph-advance measurement path (`wrap_text` at
  `crates/fono-overlay/src/renderer.rs:1402-1443` uses `font.as_scaled(...).h_advance`).
  Implication: right-alignment width measurement can reuse the existing `ab_glyph`
  scaled-advance approach — no new dependency.
- State→pipeline-stage mapping is unambiguous from the `OverlayState` enum
  (`crates/fono-overlay/src/lib.rs:24-122`): Recording/Pondering/LiveDictating/Processing
  and the Transcribing phase of `Polishing` are STT; `Polishing{Cleanup}` is the cleanup
  LLM; AssistantThinking is the assistant LLM; AssistantSynthesising/AssistantSpeaking are
  TTS; AssistantRecording/AssistantPondering are STT; Hidden/Ignoring show nothing.
- Commands reach the overlay over a stable channel
  (`OverlayCmd` at `crates/fono-overlay/src/backend.rs:128-147`) with a matching
  `OverlayHandle` setter per command (`crates/fono-overlay/src/backend.rs:225-258`) and a
  `RendererState` field + `set_*` method per piece of state
  (`crates/fono-overlay/src/renderer.rs:1691-1763`). Implication: adding a model-labels
  payload follows an established, low-risk pattern (new enum variant + handle setter +
  renderer field + setter), exactly like `SetVolumeBar` / `SetWaveformStyle`.
- Friendly-name material already exists and should be reused rather than reinvented:
  - Cloud providers have `display_name` (e.g. "OpenAI", "ElevenLabs", "Deepgram") in
    `crates/fono-core/src/provider_catalog.rs:354-380` and the `CLOUD_PROVIDERS` table.
  - Local LLMs have `display_name` (e.g. "Gemma 4 E2B Instruct Q4_0") in
    `crates/fono-polish/src/registry.rs:10-46`.
  - Local STT already has a friendly mapper, `friendly_model_label`
    (`crates/fono/src/wizard.rs:1646-1653`), turning `large-v3-turbo` → "Turbo", etc. —
    but it currently lives in the wizard and is `fn`-private.
  - Local TTS uses named catalog voices (`crates/fono-tts/src/voices.rs`).
- Config carries everything needed to resolve names: backend enums + `local.model` +
  optional `cloud { provider, model }` per stage
  (`crates/fono-core/src/config.rs:410-520` for STT; analogous `Tts`, `Polish`,
  `Assistant` structs at 530, 659, 870). Implication: a single resolver over `&Config`
  can produce all four friendly labels.
- The overlay is driven from `crates/fono/src/session.rs`, `crates/fono/src/live.rs`,
  and `crates/fono-mcp-server/src/voice_io.rs` through the shared `OverlayHandle`.
  Implication: the new labels must be pushed once at overlay spawn (and re-pushed on
  any runtime config / model switch from the tray).

### Prioritised risks/challenges (highest first)

1. **Visual collision with the waveform / VU bar.** The waveform visualisations paint
   over the whole panel area *after* the status row (the status label is drawn first, viz
   on top — see `crates/fono-overlay/src/renderer.rs:1863,1927`). A right-aligned label
   could be partially overpainted and could overlap the right-edge VU bar. Highest
   priority because it directly affects whether the feature reads as "discreet" vs
   "broken".
2. **Friendly-name coverage.** Cloud `model` strings are arbitrary and there is no
   per-model display table for cloud STT/TTS; we must define a sensible fallback so the
   label is never an ugly raw id. Medium-high: affects the core value ("user-friendly").
3. **Keeping the label fresh on runtime switches.** The tray lets users swap STT/LLM at
   runtime; a stale label is worse than none. Medium.
4. **Crate-boundary cleanliness.** The overlay crate must not depend on the MCP server
   and should stay light (binary-size rule). The resolver belongs in `fono-core` /
   `fono`, not `fono-overlay`; the overlay only receives finished strings. Medium.

## Implementation Plan

- [ ] Task 1. **Define the model-label payload type.** Add a small, `Clone`, owned
  struct (e.g. `ModelLabels { stt, polish, assistant, tts: Option<String> }`) in
  `fono-overlay` (alongside `OverlayState` in `crates/fono-overlay/src/lib.rs`) so the
  renderer can hold finished strings without depending on `fono-core::config`. Rationale:
  keeps the overlay crate decoupled from config/resolution logic and avoids a new
  dependency edge.

- [ ] Task 2. **Add the transport path.** Add `OverlayCmd::SetModelLabels(ModelLabels)`
  to `crates/fono-overlay/src/backend.rs:128-147`, a matching
  `OverlayHandle::set_model_labels(...)` setter (mirroring `set_volume_bar` at
  `crates/fono-overlay/src/backend.rs:245-247`), and handle it in each backend's command
  loop the same way `SetVolumeBar` is handled. Add the equivalent no-op on the stub
  `Overlay` (`crates/fono-overlay/src/lib.rs:184-189`). Rationale: reuses the proven
  command/handle pattern with minimal surface change.

- [ ] Task 3. **Store labels in `RendererState`.** Add a `model_labels: ModelLabels`
  field to `RendererState` (`crates/fono-overlay/src/renderer.rs:1636-1674`) plus a
  `set_model_labels` setter (next to `set_volume_bar` at
  `crates/fono-overlay/src/renderer.rs:1752-1759`). Default to empty so nothing renders
  until labels arrive. Rationale: state-driven pure renderer stays pure.

- [ ] Task 4. **Map state → active label.** Add a helper
  `active_model_label(state, &ModelLabels) -> Option<&str>` in the renderer that returns:
  STT label for Recording/Pondering/LiveDictating/Processing/AssistantRecording/
  AssistantPondering and `Polishing{Transcribing}`; cleanup-LLM label for
  `Polishing{Cleanup}`; assistant label for AssistantThinking; TTS label for
  AssistantSynthesising/AssistantSpeaking; `None` for Hidden/Ignoring. Rationale: the
  label must track the stage the user is actually waiting on, matching the existing
  state→accent and state→word mappings.

- [ ] Task 5. **Add a dim-gray color + right-aligned draw.** Introduce a dimmer constant
  (e.g. `COLOR_MODEL_LABEL`, a desaturated gray dimmer than `COLOR_TEXT_DIM` —
  conceptually CSS DimGray-ish with reduced alpha) near
  `crates/fono-overlay/src/renderer.rs:84-85`. In `redraw`
  (`crates/fono-overlay/src/renderer.rs:1862-1913`), after the left status label is
  drawn, measure the active label width via the existing scaled `h_advance` approach and
  draw it with `draw_line` at `x = right_edge - text_width`, on the same
  `status_baseline`, in the new color. Rationale: satisfies the "right side of the status
  line" + "dim gray, doesn't stick out" requirements with existing primitives.

- [ ] Task 6. **Handle the collision/space budget.** Set the right edge to
  `w - PADDING_X*scale`, and when `state_has_vu_bar(state)` and the VU bar is on
  (`crates/fono-overlay/src/renderer.rs:130-139`, `1682-1684`) pull it left by the bar
  width so the label never sits under the VU bar. Truncate (ellipsis, reuse `wrap_text`'s
  tail logic) if the label would overrun the left status word. Optionally suppress the
  label for the non-text waveform styles where the viz would overpaint it, OR accept the
  same draw-first/paint-over behaviour the status word already has — pick the
  text/`Transcript`-style-always, waveform-style-best-effort approach. Rationale: keeps
  the label legible and genuinely discreet, addressing the top risk.

- [ ] Task 7. **Build the friendly-name resolver.** Add a function (in `fono-core` or
  `fono`, e.g. `model_labels_from_config(&Config) -> ModelLabels`) that resolves each
  stage to a short friendly string: local STT via a shared/promoted `friendly_model_label`
  (lift it out of `crates/fono/src/wizard.rs:1646-1653` into a reusable location); local
  LLM via `PolishRegistry`/assistant registry `display_name`
  (`crates/fono-polish/src/registry.rs`); cloud stages via `CLOUD_PROVIDERS[*].display_name`
  (`crates/fono-core/src/provider_catalog.rs`) optionally suffixed with a tidied model
  name; Wyoming via "LAN (Wyoming)" or the configured model hint; local TTS via the
  catalog voice name. Define a clear fallback (provider display name, else a
  title-cased/cleaned model id) so the label is never a raw ugly id. Rationale: reuses
  existing display tables; centralises the "user-friendly name" requirement.

- [ ] Task 8. **Wire pushes at spawn + on switch.** Push `set_model_labels(...)` right
  after the overlay handle is created in `crates/fono/src/session.rs` /
  `crates/fono/src/live.rs` / `crates/fono-mcp-server/src/voice_io.rs`, and re-push
  whenever the tray/runtime changes a backend or model (same call sites that today
  re-issue `set_waveform_style` / config reloads). Rationale: ensures the label is present
  on first show and never goes stale (risk 3).

- [ ] Task 9. **Tests.** Unit-test `active_model_label` for every `OverlayState` variant;
  unit-test the resolver for local/cloud/Wyoming combinations per stage including the
  ugly-id fallback; add a renderer test asserting the label is drawn right-aligned and
  uses the dim color (in the spirit of the existing renderer tests at
  `crates/fono-overlay/src/renderer.rs:2200`+). Rationale: locks in the mapping and the
  friendly-name contract.

- [ ] Task 10. **Docs + changelog.** Note the new overlay label in the relevant overlay
  docs and add a `CHANGELOG.md` entry under the current unreleased section. Rationale:
  per project release rules; keeps user-facing behaviour documented.

## Verification Criteria

- During plain dictation the overlay shows e.g. `RECORDING` on the left and a dim-gray
  friendly STT name (e.g. "Turbo", "OpenAI", "Deepgram") right-aligned on the same row.
- During cleanup the label switches to the cleanup LLM's friendly name; during assistant
  thinking to the assistant LLM; during speaking to the TTS voice/provider.
- The label color is visibly dimmer/less prominent than the left status word and never
  overlaps the VU bar; long names truncate with an ellipsis instead of overrunning.
- No raw internal id (e.g. `large-v3-turbo`, `qwen3.5-0.8b-q4_k_m`) is ever shown.
- Switching STT/LLM/TTS from the tray updates the label without a daemon restart.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  and `cargo test --workspace --tests --lib` all pass; no new crate dependency is added.

## Potential Risks and Mitigations

1. **Label overpainted by the waveform visualisation.**
   Mitigation: draw on the status baseline (top padding, above most of the viz body),
   right-align clear of the VU bar, and either always-draw for `Transcript`/text styles
   and best-effort for waveform styles, or accept the same draw-order semantics the status
   word already lives with. Tunable by eye like the other layout constants.
2. **Cloud model ids with no friendly mapping.**
   Mitigation: fall back to the provider `display_name` (always present in the catalog),
   optionally plus a lightly cleaned model token; never emit the raw id verbatim.
3. **Stale label after a runtime model switch.**
   Mitigation: re-push `set_model_labels` from the same code paths that already react to
   config changes / tray switches.
4. **Binary-size / dependency rule.**
   Mitigation: reuse `ab_glyph` advances and existing display tables; the resolver lives
   in `fono-core`/`fono`; the overlay receives only finished `String`s. No new crate.
5. **Crate-coupling regression.**
   Mitigation: keep `ModelLabels` as plain owned strings in `fono-overlay`; do not import
   `fono-core::config` or the MCP server into the overlay crate.

## Alternative Approaches

1. **Single "active model" string instead of four-field struct.** The orchestrator
   computes the one relevant label per state transition and pushes a single string.
   Trade-off: simpler renderer/state, but requires the orchestrator to know stage→model
   mapping and to push on every state change (more call-site churn, more chances to go
   stale). The four-field struct keeps mapping in the renderer (one source of truth) and
   needs pushing only on config change.
2. **Render the label as a second line under the status word instead of right-aligned on
   the same row.** Trade-off: zero collision risk and room for longer names, but grows the
   panel height and is less "discreet" than the user asked; contradicts the explicit
   "right side of the same line" request.
3. **Show all four models stacked (STT/Polish/Assistant/TTS) at once.** Trade-off: maximal
   transparency, but visually noisy and contrary to "discrete"; rejected in favour of the
   single stage-relevant label.
