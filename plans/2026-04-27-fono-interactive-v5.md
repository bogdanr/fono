# Fono — Interactive / Live Dictation + Context + Macros + Wake-Word (R-plan v5)

Date: 2026-04-27
Status: Proposed (supersedes v4)
Scope changes from v4:

1. **Default wake phrase is criterion-driven, not pre-chosen.** Final
   pick happens during Slice D model evaluation against four published
   criteria; first-run default candidate is `hey_jarvis` with a wizard
   phrase-picker. Custom-trained `hey_computer` is the fallback if no
   off-the-shelf model clears the bar.
2. **Wake-word models download on demand**, never embedded in the
   binary. Reuses the existing local-STT model-fetcher infrastructure
   (HTTP + SHA-256). Triggered on first enable and on phrase change.
3. **Macros (Slice C) and wake-word (Slice D) are fully independent
   features** — separate cargo features, separate config blocks,
   separate consent flows, separate ship trains. Each is functional
   without the other; the wake-word `command` action gracefully
   degrades to `dictate` when macros aren't installed.
4. **Tray icon-state palette promoted into Slice B** (was Slice D in
   v4). Streaming UX (LiveDictating, Processing) benefits immediately;
   later slices add their state variants into the same palette.

## Objective

Unchanged from v4 — live dictation that feels instant without cost or
quality regression, plus app-aware context, voice command macros, and
local always-on wake-word activation, all opt-in and independent.

## Locked architectural decisions

(All v1–v4 decisions carry over.)

16. **Wake-word default phrase is selected by criteria, not by name.**
    Selection runs at Slice D dev time against four published criteria
    (R15.2). Plan does not hard-code "computer" or "hey computer";
    final default is whichever qualifying phrase ranks highest, with
    `hey_jarvis` as the v5 placeholder until that evaluation runs.
17. **Wake-word models are on-disk artifacts, never embedded.** Same
    download/verify/cache pattern as local STT; `[wakeword]` is fully
    inert until models are present.
18. **Macros and wake-word are independent features.** No
    cross-feature compile or runtime dependency; cross-feature
    *integration* (wake → macro action) is opt-in and graceful.

## Implementation Plan

### R1 — R13 (carryover from v2; sequencing in v5 below)

[Unchanged.]

### R14 — Voice command system (carryover from v2; Slice C)

[Unchanged. Independence guarantee: builds + runs cleanly without
R15 enabled.]

### R15 — Wake-word activation engine (revised in v5)

#### R15a — Engine + audio plumbing

- [ ] R15.1. New crate `crates/fono-wakeword/`. Define `WakeWordEngine`
  trait → `Stream<Item = WakeEvent { phrase, confidence, t_audio }>`.
- [ ] R15.2. **Default backend: openWakeWord via `tract`**
  (`wakeword-onnx` cargo feature, on by default when `wakeword`
  meta-feature is enabled). **Phrase-selection criteria** (locked):
  - C1 — false-positive rate ≤ 5/hour against LibriSpeech test-clean
    ambient noise fixture.
  - C2 — activation recall ≥ 95% across 5 native-English speakers and
    3 non-native accents on held-out test set.
  - C3 — phrase is ≥ 2 syllables (single-word real-English phrases
    like "computer" alone have inherently higher FP rates).
  - C4 — Apache-2.0-compatible license; model hosted on a stable
    mirror (upstream releases or HuggingFace).

  **Default candidate ranking** if all clear the bar: `hey_jarvis`
  (placeholder default in v5), `ok_nabu`, custom-trained `hey_computer`,
  `hey_mycroft`, `alexa`. Final pick is recorded in ADR 0012 at Slice
  D landing.
- [ ] R15.3. **Alternative backend: rustpotter** (`wakeword-rustpotter`
  cargo feature, off by default) — size-constrained builds.
- [ ] R15.4. **Opt-in backend: Porcupine** (`wakeword-porcupine`,
  proprietary, off by default).
- [ ] R15.5. Audio source sharing — fan-out subscriber on the
  `fono-audio` frame stream; no second mic open.

#### R15a-bis — On-demand model fetcher (NEW)

- [ ] R15.46. **Model fetcher reuses local-STT infrastructure.** New
  module `crates/fono-wakeword/src/fetch.rs` calls into the existing
  `fono-core` HTTP+SHA-256 download path. Storage layout under
  `~/.cache/fono/wakeword-models/`:
  ```
  embeddings/melspec.onnx          (shared, ~5 MB)
  embeddings/speech_embedding.onnx (shared, ~5 MB)
  classifiers/<phrase>.onnx        (~100 KB each)
  ```
- [ ] R15.47. **Manifest** at `assets/wakeword/models.toml` lists every
  curated model with URL, SHA-256, size, license, and FP/recall
  benchmark numbers. Committed to the repo; verified at fetch time.
- [ ] R15.48. **Fetch triggers**:
  - First wake-word enable (wizard step or tray toggle).
  - Phrase change (only the new classifier; embeddings already cached).
  - Manual `Wake word ▸ Refresh models` tray entry.
  - Never automatic from the daemon without an explicit user action.
- [ ] R15.49. **Offline-first behavior** — if models aren't cached and
  no network, listener stays in `Suspended` state with tooltip
  "Wake-word models not yet downloaded — connect to network and click
  `Wake word ▸ Refresh models`". No silent failure; no daemon retry
  loop.
- [ ] R15.50. **Integrity** — SHA-256 verified before load; corrupt
  files trigger one re-download attempt then surface an error to
  tray + `fono doctor`.
- [ ] R15.51. **Slim-build compatibility** — when `wakeword` feature
  is compiled out, the fetcher and its assets contribute zero bytes
  to the binary; `models.toml` is `include_bytes!`'d only behind the
  feature flag.

#### R15b — Cost-control cascade (carryover from v4)

[R15.6–R15.9 unchanged.]

#### R15c — Battery & power-profile awareness (carryover)

[R15.10–R15.14 unchanged.]

#### R15d — Activation flow + actions (revised for independence)

- [ ] R15.15. `WakeAction` enum: `dictate`, `command`,
  `dictate_with_phrase`.
- [ ] R15.16. **Multi-phrase routing** — `[wakeword.phrases]` table
  maps phrase → action. v5 default seeded by wizard:
  `<top-ranked-phrase>` → `dictate`. If macros (Slice C) are also
  enabled at the time of the wizard, a second phrase is offered for
  `command`.
- [ ] R15.17–R15.20. (Latency target, pre-roll, self-trigger
  prevention, optional chime — unchanged.)
- [ ] R15.52. **Graceful degradation when macros aren't installed** —
  if `[wakeword.phrases.<name>.action = "command"]` but the macro
  feature is compiled out or `[macros].enabled = false`, the listener
  logs a warn-level message at startup, falls back to `dictate` for
  that phrase, and surfaces the misconfiguration in `fono doctor`.

#### R15e — False-positive handling (carryover)

[R15.21–R15.25 unchanged.]

#### R15f — Privacy + transparency (carryover)

[R15.26–R15.30 unchanged.]

#### R15g — Config + wizard + CLI (revised)

- [ ] R15.31. `[wakeword]` block (unchanged from v4).
- [ ] R15.32. **Wizard wake-word step** (revised):
  - Cost disclosure (CPU + battery range from doctor self-bench).
  - Privacy explainer + consent checkbox.
  - **Phrase picker** — lists every model in `models.toml` that
    passes the criteria at build time, with FP/recall numbers visible.
    Default-selected = top-ranked.
  - Triggers R15.46 model fetch on completion; shows download
    progress.
  - Optional `wakeword calibrate` pass.
  - Step is independent of the macro wizard step (Slice C); either
    can be skipped without affecting the other.
- [ ] R15.33. CLI: `fono wakeword status`, `fono wakeword test`,
  `fono wakeword calibrate`, `fono wakeword train <phrase>`
  (rustpotter backend only), `fono wakeword bench`,
  `fono wakeword refresh-models`, `fono wakeword set-phrase <name>`
  (refetches classifier if not cached).

#### R15h — Tray UX (revised)

- [ ] R15.34. **Top-level tray toggle** "Wake word: <state>" cycling
  `Off → On (AC only) → On (always) → Off`. Bullet color matches the
  Slice-B icon palette.
- [ ] R15.35. **`Wake word ▸` submenu** retains: engine selector,
  phrase picker (live update via `set-phrase` CLI path), calibrate,
  bench, refresh models, open audit log.
- [ ] R15.53. **Independence from macros** — wake-word menu entries
  are present only when the `wakeword` cargo feature is compiled in,
  regardless of macros feature state.

#### R15i — Observability + tests (carryover)

[R15.36–R15.39 unchanged.]

### R16 — Tray icon-state palette (PROMOTED to Slice B)

Moved out of v4's Slice D into Slice B; ships with v0.2.0. Later
slices register additional state variants (`CommandListening` from
Slice C, `Armed` from Slice D) into the existing palette without
re-architecting tray code.

- [ ] R16.1. Formal `IconState` enum in `crates/fono-tray/src/icon.rs`:
  `Idle`, `Recording`, `LiveDictating`, `Processing`, `Suspended`,
  `Error`. Slice C adds `CommandListening`; Slice D adds `Armed`.
- [ ] R16.2. **Slice B color palette** (locked):
  | State | Glyph | Color |
  |---|---|---|
  | Idle | mic-outline | Theme grey |
  | Recording | mic-filled | Red (#DC2626 / #EF4444) |
  | LiveDictating | mic-filled + waveform | Red |
  | Processing | mic-filled + spinner | Amber (#D97706 / #F59E0B) |
  | Suspended | mic-slash | Grey-X |
  | Error | mic-alert | Red-X |
- [ ] R16.3. **Slice C extension** (lands with macros):
  | State | Glyph | Color |
  |---|---|---|
  | CommandListening | mic-filled + bolt | Purple (#7C3AED / #A78BFA) |
- [ ] R16.4. **Slice D extension** (lands with wake-word):
  | State | Glyph | Color |
  |---|---|---|
  | Armed | mic-outline + dot badge | Blue (#3A82F6 / #5BA3FF), 0.5 Hz pulse |
- [ ] R16.5. SVG sources in `assets/tray/`; `crates/fono-tray/build.rs`
  rasterizes to 16/22/24/32/48 px PNG sets for SNI/AppIndicator hosts
  that don't render SVG.
- [ ] R16.6. **Color-blindness fallback** — `[tray].icon_set =
  "color" | "monochrome" | "shape_only"`.
- [ ] R16.7. ADR `0013-tray-icon-state-palette.md` locks the palette
  and the rule that future state additions follow the same
  convention.

### R17 — Docs + ADRs (revised numbering)

- [ ] R17.1. ADR `0009-interactive-live-dictation.md` (Slice A/B).
- [ ] R17.2. ADR `0010-app-context-and-privacy.md` (Slice B).
- [ ] R17.3. ADR `0011-voice-commands.md` (Slice C).
- [ ] R17.4. ADR `0012-wake-word-activation.md` (Slice D) — records
  the criterion-driven phrase choice and the final default.
- [ ] R17.5. ADR `0013-tray-icon-state-palette.md` (Slice B).
- [ ] R17.6. `docs/interactive.md`, `docs/context.md`, `docs/macros.md`,
  `docs/wakeword.md`. Each user guide is independent.

## Sequencing (deliverable slices, revised)

1. **Slice A** — Streaming + budget engine + overlay (local-first):
   R1, R2, R3, R5, R7 (partial), R10 (partial), R12. v0.2.0-alpha.
2. **Slice B** — Cloud streaming + app context + tray icon palette:
   R4, R8.3–R8.4, R9.5, R10.4, R11, R13, **R16.1+R16.2+R16.5+R16.6+R16.7**.
   v0.2.0.
3. **Slice C** — Voice command macros (independent):
   R9.6, R14, R16.3, R17.3. v0.3.0.
4. **Slice D** — Wake-word activation (independent):
   R15 (incl. on-demand fetcher R15.46–R15.51 and graceful macro
   degradation R15.52–R15.53), R16.4, R17.4. Ships v0.3.x or v0.4.0,
   independent of Slice C.
5. **Slice E** — Polish: R6 live-inject, R4.3 Deepgram/AssemblyAI,
   richer app context (URL via WebExtension, editor file via
   Neovim/VS Code plugins). post-v0.3.

**Independence guarantees** verified by build matrix:
- `cargo build --no-default-features --features tray` → builds clean.
- `cargo build --no-default-features --features tray,macros` → C only.
- `cargo build --no-default-features --features tray,wakeword` → D only.
- `cargo build --no-default-features --features tray,macros,wakeword` →
  both, with the integration path (wake → command) live.

## Verification Criteria

(All v4 criteria carry over.)

- **Wake-word default phrase** clears criteria C1–C4 at Slice D
  landing; choice + numbers recorded in ADR 0012.
- **Models fetcher** verifies SHA-256 against `assets/wakeword/models.toml`;
  corrupt-file regression test forces a single re-download then surfaces
  the error path.
- **Independence build matrix** above passes in CI; each feature
  combination produces a working binary.
- **Wake → macro graceful degradation** test: configure a phrase with
  `action = "command"`, build without macros feature, assert daemon
  starts cleanly, logs the warn, falls back to dictate, and `fono
  doctor` reports the misconfiguration.
- **Slice B tray icon palette** ships with v0.2.0 and renders correctly
  on KDE Plasma 5 LTS, KDE Plasma 6, GNOME-AppIndicator, sway+waybar,
  i3+i3bar.

## Potential Risks and Mitigations

(All v4 risks carry over; new ones below.)

25. **No off-the-shelf wake phrase clears criteria C1–C4 at Slice D
    eval time.** Mitigation: Slice D includes a custom-training fallback
    using upstream openWakeWord's training pipeline to produce a fono-
    branded `hey_computer` model; budget for this is built into the
    slice. If even that fails the criteria, Slice D ships with a more
    conservative default (`hey_jarvis` or `ok_nabu`) and a doc note.
26. **Model download fails behind corporate proxies / air-gapped
    machines.** Mitigation: `fono wakeword refresh-models --from
    <local-path>` accepts a pre-downloaded model bundle; manifest
    verification still runs.
27. **`[wakeword].action = "command"` configured before macros are
    installed.** Mitigation: graceful degradation (R15.52); wizard
    avoids offering `command` action when macros aren't enabled at
    wizard time; doctor surfaces the misconfiguration.
28. **Tray icon palette ships in Slice B but Slice C/D add states
    later — visual regression risk.** Mitigation: SVG sources for all
    eight states are committed in Slice B; later slices only register
    them into the live `IconState` enum. No tray code re-architecture
    per slice.

## Alternative Approaches

(All v4 alternatives carry over; new ones below.)

15. **Pre-pick "hey computer" as the hard default and train a custom
    model now.** Faster to ship a polished default, but pre-commits
    project to a phrase before its FP/recall is measured. Rejected in
    favor of the criterion-driven approach; custom-train is the
    documented fallback.
16. **Embed wake-word models in the binary.** Eliminates first-enable
    online dependency, but bloats the release artifact by ~10 MB and
    complicates phrase changes (every change requires a binary
    rebuild). Rejected — matches local-STT pattern.
17. **Bundle macros and wake-word in one v0.3 release.** Faster
    end-user message ("v0.3 = voice control") but couples ship trains
    and forces both to wait on the slower one. Rejected per user
    direction; independent slices.
