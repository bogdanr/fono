# Make live transcript preview the 5th overlay visualisation style

## Objective

Add `Transcript` to `WaveformStyle` so the overlay style picker has
five options (Bars, Oscilloscope, FFT, Heatmap, **Transcript**),
delete the `[interactive].enabled` master toggle outright, and route
both dictation and assistant through the streaming pipeline whenever
`overlay.style = "transcript"` is selected. No deprecation window,
no migration, no compatibility shims — the project has no shipped
users yet, so the goal is a clean single-axis design rather than a
gentle transition.

The rest of the `[interactive]` block (boundary heuristics,
`chunk_ms_*`, `hold_release_grace_ms`, `cleanup_on_finalize`, filler
vocab, etc.) stays as live-pipeline tuning for the Transcript style;
only the `enabled` field disappears.

## Initial Assessment

### Project structure summary

- `crates/fono-core/src/config.rs:704-728` — `WaveformStyle =
  Bars | Oscilloscope | Fft | Heatmap`, default `Fft`. Lives inside
  `Overlay { waveform, style, volume_bar }` at `:738-755`.
- `crates/fono-core/src/config.rs` — `Interactive` block (separate
  search) carries `enabled` + ~17 other tuning knobs. Only
  `enabled` collapses; the rest are renamed to nothing and stay put
  as the streaming-pipeline tuning surface.
- `crates/fono-overlay/src/real.rs:48-52` — `OverlayMode::{Text,
  Waveform(WaveformStyle)}` chosen at spawn; runtime flip via
  `enable_text_mode` / `enable_waveform_mode` (`:165-179`) and
  `SetWaveformStyle` no-ops while in Text mode (`:1546-1561`).
- `crates/fono-overlay/src/lib.rs:14-51` — `OverlayState` enum;
  `LiveDictating` is the state the streaming renderer uses today.
- `crates/fono-tray/src/lib.rs:184-190, 1317-1336` — four-entry
  `WAVEFORM_STYLES` array + tray submenu emitting
  `SetWaveformStyle(u8)`.
- `crates/fono/src/session.rs:411-453, 582-590` — overlay spawn
  + reload decision tree, gated on `interactive.enabled`.
- `crates/fono/src/session.rs:683` (waveform level ticker),
  `:1197, :1228, :1259, :1333, :1757` (assistant streaming branch
  and batch-vs-live polish gates), `:1944, :2586+` (FSM tests) —
  every `interactive.enabled` reader.
- `crates/fono/src/daemon.rs:33-54` — `translate_for_interactive`
  flips dictation hotkeys to their `Live*` variants when
  `interactive.enabled = true`.
- `crates/fono-stt/src/factory.rs:336-373` — `build_streaming_stt`
  gates the cloud branch on `interactive.enabled` (local whisper
  streaming is unconditional once the feature is compiled in).
- `crates/fono/src/wizard.rs` — first-run wizard offers the
  `interactive.enabled` choice; needs to switch to offering the
  style picker.

### Key findings

- The transcript panel and the four waveform visualisations are
  drawn by **different overlay modes** today; the user-facing
  picker only sees the four waveform modes. Adding `Transcript` to
  the same enum unifies the axis.
- Streaming preview's *real* prerequisite is a `StreamingStt`
  impl, not the `enabled` flag. Local whisper has one
  unconditionally; Groq's is gated by the same flag the plan
  removes (`crates/fono-stt/src/factory.rs:354`); other clouds
  have none. Removing the flag means the gate becomes "user
  picked Transcript".
- The `[interactive]` block has tuning knobs that are still
  useful when streaming runs (filler vocab, prosody hints, drain
  grace, cleanup_on_finalize). Those stay; only `enabled` is
  deleted.

### Risk ranking (highest first)

1. **winit one-event-loop limit.** The Text↔Waveform runtime
   flip already exists (`real.rs:1531-1545`); plan must reuse it
   rather than respawning the window. Low risk if we keep that
   path.
2. **Streaming backend gap for cloud-only deployments.**
   `Transcript` selected with OpenAI/Anthropic/Cerebras/OpenRouter
   has no streaming impl. Must produce a clean placeholder + batch
   fallback rather than a dead overlay.
3. **Test surface.** Six known config readers + the FSM
   translation tests (`daemon.rs:2580+`) hold `interactive.enabled`
   directly. Each needs updating to use `overlay.style` instead.
4. **Equivalence harness** — `A2-default` rows pin
   `[interactive]` settings (plan v7 R18.10/R18.23). With
   `enabled` gone the harness's pinned config must rewrite to
   `overlay.style = "transcript"` and re-baseline if any verdict
   moves.

## Implementation Plan

### Phase 1 — Schema

- [ ] Task 1.1. Add `WaveformStyle::Transcript` (serde lowercase
  `"transcript"`) to `crates/fono-core/src/config.rs:704-728`.
  Keep default at `Fft` so out-of-the-box behaviour is unchanged.
- [ ] Task 1.2. Delete `Interactive::enabled` from
  `crates/fono-core/src/config.rs`. Keep the rest of the block —
  it remains the streaming-pipeline tuning surface, applied
  whenever the Transcript style is active. Update the `Default`
  impl + serde defaults accordingly.
- [ ] Task 1.3. Add an inline helper `Config::live_preview()` (or
  free fn) returning `bool` for the single condition
  `overlay.style == WaveformStyle::Transcript`. Centralises the
  predicate so future surfaces (doctor, tray, wizard, daemon) all
  read the same source.
- [ ] Task 1.4. New ADR
  `docs/decisions/0026-overlay-transcript-style.md` documenting
  the unified single-axis design and recording that
  `[interactive].enabled` was removed (no users yet → no
  migration). Cross-reference and amend ADR 0009 to match.

### Phase 2 — Overlay renderer

- [ ] Task 2.1. Replace `OverlayMode` in
  `crates/fono-overlay/src/real.rs:48-52` with the bare
  `WaveformStyle`. The `Transcript` variant selects the existing
  text-render branch; the other four select today's waveform
  branches. `spawn_with_mode` takes a `WaveformStyle` parameter.
- [ ] Task 2.2. Delete `enable_text_mode` and
  `enable_waveform_mode` from `OverlayHandle`
  (`real.rs:165-179`). The single `set_waveform_style(style)` API
  is the only runtime control left. Update the two callers in
  `crates/fono/src/session.rs:582-590`.
- [ ] Task 2.3. Extend the `SetWaveformStyle` handler at
  `real.rs:1546-1561` to clear ring buffers when leaving
  Transcript and clear cached text when entering Transcript —
  exactly the cleanup the deleted `SetMode` branch did.
- [ ] Task 2.4. Keep the stub `Overlay::set_waveform_style` in
  `crates/fono-overlay/src/lib.rs:101` accepting the new variant
  so headless builds still compile.
- [ ] Task 2.5. Delete `RealOverlay::spawn` /
  `RealOverlay::spawn_waveform` distinction; expose a single
  `spawn(style: WaveformStyle)` constructor.

### Phase 3 — Orchestrator + factory wiring

- [ ] Task 3.1. `crates/fono/src/daemon.rs::translate_for_interactive`
  (`:33-54`) rename to `translate_for_live_preview` and gate the
  `Live*` rewrite on `cfg.overlay.style == Transcript`. Rename
  internals (e.g. `interactive_enabled` field in
  `crates/fono/src/daemon.rs:1944`) to `live_preview`. Update the
  three FSM-translation unit tests at `daemon.rs:2580+`.
- [ ] Task 3.2. `crates/fono-stt/src/factory.rs:336-373` —
  `build_streaming_stt` takes the resolved boolean
  `live_preview` rather than the deleted `interactive.enabled`.
  Update the two `#[cfg(streaming)]` tests at `factory.rs:493+` to
  drive the new signature.
- [ ] Task 3.3. `crates/fono/src/session.rs` — replace every
  reader of `cfg.interactive.enabled` with
  `cfg.live_preview()`:
  - overlay spawn (`:411-453`) and reload (`:582-590`)
  - waveform-level ticker gate (`:683`)
  - polish gates (`:1197, :1228, :1259`)
  - assistant streaming branch (`:1333`)
  - pipeline overlay reset (`:1757`)
  - `Self::new` plumbing (`:430, :584, :1334`)
- [ ] Task 3.4. `crates/fono/src/cli.rs:1663` — same rename pass
  for the `fono record --live` path's language plumbing.
- [ ] Task 3.5. Overlay spawn site collapses: with only one
  `WaveformStyle` axis there's a single
  `RealOverlay::spawn(cfg.overlay.style)` call, no branch.

### Phase 4 — Fallback for non-streaming STT

- [ ] Task 4.1. When `live_preview()` is true but
  `current_streaming_stt() == None`, the dictation hotkey routes
  to the streaming path; `on_start_live_dictation` already falls
  back to `on_start_recording` (`session.rs:1996-2007`). The
  overlay is spawned in Transcript mode regardless and shows a
  one-line placeholder (e.g. "Streaming preview unavailable for
  this backend — using batch mode") until the batch pipeline
  finishes, at which point the final transcript replaces it just
  before injection.
- [ ] Task 4.2. Same placeholder behaviour on the assistant
  branch at `session.rs:1320-1353` when streaming STT is missing.
- [ ] Task 4.3. `fono doctor` row: `live preview : on / off` with
  a one-line reason — the resolved style + the streaming
  capability of the active STT backend.

### Phase 5 — Tray + wizard + CLI

- [ ] Task 5.1. `crates/fono-tray/src/lib.rs:184-190` —
  `WAVEFORM_STYLES` gains `("transcript", "Transcript")`. The
  active-marker logic at `:1317-1336` extends to five entries.
  Human-label match adds `"Transcript" => "Transcript (live
  preview text)"`.
- [ ] Task 5.2. Drop the "Interactive mode" tray entry (if one
  exists in current builds) — picking the style is the user-facing
  control now.
- [ ] Task 5.3. `crates/fono/src/wizard.rs` — replace the
  "enable interactive (live) dictation?" yes/no step with a single
  "Overlay visualisation style" picker offering all five options.
  Recommend `Transcript` when a streaming-capable STT is
  selected; recommend `Fft` otherwise.
- [ ] Task 5.4. Any CLI `fono use …` subcommand or shell
  completion that mentioned `[interactive]` gets retargeted to
  `[overlay].style`.

### Phase 6 — Tests, harness, docs

- [ ] Task 6.1. Unit tests:
  - Overlay renderer: Transcript routes text through the text
    branch; switching style clears stale state in both directions.
  - Tray: five-entry round-trip + active marker.
  - Daemon: `translate_for_live_preview` fires when
    `style = Transcript` (mirrors the existing
    `translate_hold_toggle_to_live_when_enabled` test).
  - Config: `live_preview()` truth table.
- [ ] Task 6.2. Integration test in
  `crates/fono/tests/live_pipeline.rs` (or a sibling) drives a
  dictation press with `overlay.style = "transcript"` and asserts
  preview/finalize text reaches the overlay handle without any
  `interactive.enabled` knob anywhere.
- [ ] Task 6.3. Equivalence harness's `A2-default` row updates to
  pin `overlay.style = "transcript"` instead of
  `interactive.enabled = true`. Re-run; if per-fixture verdicts
  hold, the JSON only changes its echoed-config block. If any
  verdict moves, re-baseline with the new config and note the
  delta in the commit.
- [ ] Task 6.4. Docs:
  - `docs/interactive.md` rewritten around the style picker.
  - `docs/providers.md` notes which STT backends support
    Transcript (local whisper, Groq) vs which fall back.
  - CHANGELOG `[Unreleased]` entries under Added (Transcript
    style) and Removed (`[interactive].enabled`).
- [ ] Task 6.5. Search-and-destroy pass: after Phase 3+5 land,
  `rg 'interactive\.enabled|InteractiveEnabled'` across the
  workspace must return zero hits outside the new config struct's
  definition. Same for the deleted overlay-handle methods
  (`enable_text_mode`, `enable_waveform_mode`,
  `RealOverlay::spawn_waveform`).

## Verification Criteria

- Default first-run config produces today's overlay (Fft style)
  identically.
- Tray → Waveform style → Transcript switches the next dictation
  press to streaming preview live in the overlay, with no
  `[interactive]` edit required.
- Picking Transcript on a cloud STT without streaming
  (OpenAI/Anthropic/Cerebras/OpenRouter) shows the placeholder line
  during recording, runs the batch pipeline, and updates the
  overlay with the final transcript before injection.
- `rg 'interactive\.enabled'` across the repo returns zero hits
  (config struct's `Interactive` block no longer has the field).
- `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, and `cargo test --workspace
  --tests --lib` all clean.
- Equivalence harness A2 rows continue to PASS against their
  baseline (with the harness's pinned config updated to the new
  style toggle).
- `fono doctor` row reflects the four scenarios accurately.

## Potential Risks and Mitigations

1. **Stale state on style switch.**
   Switching from Transcript to FFT mid-session could leave stale
   text on screen, or vice versa with stale ring buffers.
   Mitigation: Task 2.3 explicitly extends the
   `SetWaveformStyle` handler to clear the relevant cache on
   each switch direction; covered by Task 6.1's unit tests.

2. **Cloud-only slim builds.**
   `Transcript` selected without the `streaming` feature compiled
   in produces a transcript-mode overlay that can never receive
   text. Mitigation: Task 4.1/4.2 placeholder + doctor row
   (Task 4.3) make the situation legible; the underlying batch
   pipeline still works.

3. **Equivalence harness verdict drift.**
   Pinning a different config knob may exercise a slightly
   different code path. Mitigation: Task 6.3 re-runs the harness
   and refuses to commit unless verdicts hold or the delta is
   explained inline.

4. **Hidden `interactive.enabled` reader.**
   A test, example, smoke script, or doctor surface might still
   reference the deleted field. Mitigation: Task 6.5 is the
   explicit grep gate, run after Phase 3+5.

## Alternative Approaches

1. **Keep `[interactive].enabled` and also offer the style.**
   Two switches, two ways to disagree. Rejected — the whole point
   is single-axis simplicity.

2. **Delete the entire `[interactive]` block and inline the
   tuning into `[overlay]`.**
   Tempting but the boundary heuristics, filler vocab, drain
   grace, prosody hints are streaming-pipeline tuning, not
   visualisation tuning. Keeping them under `[interactive]`
   (just without `enabled`) preserves a clean conceptual
   separation. Could be revisited in a later config-shape pass.

3. **Reuse `Recording { db }` and stream text into it instead of
   adding a fifth style.**
   Smaller enum, but loses the user-facing picker entry and the
   ability for advanced users to pick e.g. FFT during dictation
   when they don't care about live preview. Rejected.
