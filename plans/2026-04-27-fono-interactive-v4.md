# Fono — Interactive / Live Dictation + Context + Macros + Wake-Word (R-plan v4)

Date: 2026-04-27
Status: Proposed (supersedes v3)
Scope changes from v3:

1. **Default wake-word engine flipped to openWakeWord-via-`tract`.**
   rustpotter demoted to opt-in alternative behind a cargo feature.
   Rationale: openWakeWord has materially better accuracy, a vastly
   richer pretrained model zoo, and is field-tested at scale through
   Home Assistant Voice / Rhasspy / Wyoming. Both engines are
   Apache-2.0; both run via pure-Rust runtimes (`tract` for ONNX);
   the size and memory delta is small (~10 MB) and is a worthwhile
   trade for accuracy and community-tuning maturity.
2. **Tray UX promoted from a buried submenu entry to a first-class
   top-level toggle**, with a formal icon-state palette that visually
   distinguishes Idle / Armed / Recording / Command / Processing /
   Suspended / Error.

## Objective

Unchanged from v3 — live dictation that feels instant without cost or
quality regression, plus app-aware context, voice command macros, and
local always-on wake-word activation, all opt-in.

## Locked architectural decisions

(All v1–v3 decisions carry over.)

13. **Default wake-word engine: openWakeWord** via `tract` (pure-Rust
    ONNX). Rustpotter is a feature-flagged alternative for size-
    constrained builds. Porcupine remains opt-in only (proprietary).
14. **Tray icon-state model** is the single source of UX truth for
    daemon state — Idle / Armed / Recording / LiveDictating /
    CommandListening / Processing / Suspended / Error each map to a
    distinct icon + color. Defined in `crates/fono-tray/src/icon.rs`,
    consumed by every state-change event from the orchestrator.
15. **Wake-word toggle is a top-level tray entry**, not a submenu item.

## Implementation Plan

### R1 — R14 (carryover from v2)

[Reference v2 plan; no changes.]

### R15 — Wake-word activation engine (revised from v3)

#### R15a — Engine + audio plumbing

- [ ] R15.1. New crate `crates/fono-wakeword/`. Define `WakeWordEngine`
  trait → `Stream<Item = WakeEvent { phrase, confidence, t_audio }>`.
- [ ] R15.2. **Default backend: openWakeWord via `tract`**
  (`wakeword-onnx` cargo feature, **on by default** when `wakeword`
  meta-feature is enabled). Loads the Google pretrained `melspec` +
  `speech_embedding` ONNX models once at startup; per-phrase classifier
  heads are tiny (~100 KB) and loaded on demand. Ships built-in
  classifier heads for `"computer"`, `"hey_jarvis"`, `"alexa"`,
  `"hey_mycroft"`, `"ok_nabu"`. User-trained classifier heads from the
  upstream openWakeWord training pipeline can be dropped into
  `~/.config/fono/wakeword-models/` and selected by phrase name.
- [ ] R15.3. **Alternative backend: rustpotter** (`wakeword-rustpotter`
  cargo feature, **off by default**). Smaller resident footprint
  (~5–10 MB vs ~15–25 MB for openWakeWord); lower accuracy. Documented
  as the choice for size-constrained builds (embedded targets, slim
  packages). Ships built-in `.rpw` for `"computer"`; supports
  `fono wakeword train` for custom phrases.
- [ ] R15.4. **Opt-in backend: Porcupine** (`wakeword-porcupine`
  cargo feature, **off by default**, proprietary — same opt-in posture
  as Llama/Gemma per project rules).
- [ ] R15.5. **Audio source sharing** — wake-word listener consumes
  from the same `fono-audio` frame stream as dictation via a fan-out
  subscriber API. No second mic open.

#### R15b — Cost-control cascade

- [ ] R15.6. Stage 1 — energy gate (RMS over 30 ms windows; auto-
  calibrated noise floor). Skips stages 2–3. Target < 0.1% of one core.
- [ ] R15.7. Stage 2 — coarse VAD at 8 kHz reusing the `fono-audio`
  VAD. Skips stage 3 if no voice. Target < 0.3% of one core.
- [ ] R15.8. Stage 3 — neural confirm at 16 kHz. For openWakeWord this
  is `melspec` → `speech_embedding` → per-phrase classifier; for
  rustpotter this is the engine's native pipeline. Target 1–3% of one
  core when active; ~5–10% duty cycle in typical quiet rooms.
- [ ] R15.9. Periodic mode — `[wakeword].duty_cycle = "continuous"
  | "balanced" | "low_power"`. `balanced` (default on AC) at 30 Hz;
  `low_power` (default on battery) at 10 Hz; `continuous` at 60 Hz.

#### R15c — Battery & power-profile awareness

- [ ] R15.10. `BatteryMonitor` (Linux `/sys/class/power_supply`,
  macOS `pmset`, Windows `GetSystemPowerStatus`). 10 s poll.
- [ ] R15.11. `[wakeword].active_when` enum: `always`, `ac_only`
  (default), `ac_or_above_<N>%`, `never`.
- [ ] R15.12. `[wakeword].battery_floor_pct` (default 20). Below this,
  listener auto-suspends regardless of `active_when`.
- [ ] R15.13. Platform-profile awareness — read
  `/sys/firmware/acpi/platform_profile` (Linux) / equivalents.
  On `low-power`, downgrade `duty_cycle` one step automatically.
- [ ] R15.14. Lid-close auto-suspend via systemd-logind `LidClosed`.

#### R15d — Activation flow + actions

- [ ] R15.15. `WakeAction`: `dictate`, `command`,
  `dictate_with_phrase`.
- [ ] R15.16. Multi-phrase routing — `[wakeword.phrases]` table maps
  phrase → action. Default: `"computer"` → `command`,
  `"hey_jarvis"` → `dictate`.
- [ ] R15.17. Activation latency target ≤ 500 ms p95 from word-end to
  capture-start.
- [ ] R15.18. Pre-roll capture — last 1.5 s of audio kept in a ring so
  `dictate_with_phrase` doesn't clip the post-wake onset.
- [ ] R15.19. Self-trigger prevention — listener pauses while FSM is
  in any non-Idle state.
- [ ] R15.20. Optional activation chime (`[wakeword].activation_chime`,
  default off).

#### R15e — False-positive handling

- [ ] R15.21. Confidence threshold per phrase
  (`[wakeword.phrases.<name>.threshold`, default 0.5 for openWakeWord).
- [ ] R15.22. Silence-prefix gate — require ≥ 200 ms relative silence
  preceding the wake.
- [ ] R15.23. Activation timeout (`follow_through_ms`, default 3000) —
  no speech follows wake → silent cancel.
- [ ] R15.24. Recent-trigger debounce — 1.5 s suppression after an
  activation.
- [ ] R15.25. Per-user calibration via `fono wakeword calibrate`.

#### R15f — Privacy + transparency

- [ ] R15.26. First-run consent step in the wizard, unbypassable.
  Listener is OFF until user explicitly opts in.
- [ ] R15.27. Tray indicator — Armed icon visually distinct from
  Recording (see R15j). Tooltip "Wake-word listener active —
  say 'computer'".
- [ ] R15.28. Hardware mic mute respected; tray reflects suspended.
- [ ] R15.29. Audio strictly in-RAM; debug-record opt-in only with
  loud warn log on every start.
- [ ] R15.30. Activation log at `~/.local/share/fono/wakeword.log`
  (timestamp + phrase + confidence + action; no audio).

#### R15g — Config + wizard + CLI

- [ ] R15.31. `[wakeword]` block: `enabled` (default false), `engine`,
  `phrases` (table), `noise_floor_db`, `duty_cycle`, `active_when`,
  `battery_floor_pct`, `follow_through_ms`, `activation_chime`,
  `consent_acknowledged_at` (RFC3339).
- [ ] R15.32. Wizard wake-word step: cost disclosure (CPU + battery),
  privacy explainer, phrase choice, action mapping, consent checkbox,
  optional `wakeword calibrate` pass.
- [ ] R15.33. CLI: `fono wakeword status`, `fono wakeword test`,
  `fono wakeword calibrate`, `fono wakeword train <phrase>` (rustpotter
  backend only — openWakeWord training is offline-pipeline). Also:
  `fono wakeword bench` to measure per-machine CPU/battery cost.

#### R15h — Tray UX (NEW — first-class toggle + icon states)

- [ ] R15.34. **Top-level tray toggle entry** "Wake word: <state>"
  cycling `Off → On (AC only) → On (always) → Off` on left-click. The
  bullet/dot glyph next to the entry is colored to match the icon
  palette (R15.41) so the menu and tray icon stay visually coherent.
- [ ] R15.35. **Right-click "Wake word ▸" submenu** retains the
  power-user controls: engine selection, phrase list, calibrate,
  open audit log, run bench. Top-level toggle is the 80% path; the
  submenu is the 20% configuration path.

#### R15i — Observability + tests

- [ ] R15.36. Tracing spans: `wakeword.stage1_skip_rate`,
  `wakeword.stage2_skip_rate`, `wakeword.activation_latency`,
  `wakeword.false_positive_count`, `wakeword.duty_cycle_actual`.
- [ ] R15.37. `fono doctor` reports measured per-machine wake-word
  CPU%, mic-on share, estimated battery cost (calibrated from a 60 s
  `fono wakeword bench` self-bench).
- [ ] R15.38. Integration test with synthetic wake-word audio →
  asserts activation within latency budget; ambient-noise fixture
  test → asserts no false trigger; FSM-busy fixture → asserts clean
  self-suspend.
- [ ] R15.39. Manual battery regression test on the reference laptop
  (idle hours baseline vs wake-word-on); documented in
  `docs/bench/battery-*.json`.

#### R15j — Tray icon-state palette (NEW)

- [ ] R15.40. Formalize tray state enum in
  `crates/fono-tray/src/icon.rs::IconState` — `Idle`, `Armed`,
  `Recording`, `LiveDictating`, `CommandListening`, `Processing`,
  `Suspended`, `Error`. Orchestrator emits state transitions; tray
  consumes via the existing IPC channel. Replaces the current ad-hoc
  Recording/Processing handling.
- [ ] R15.41. **Icon palette** (theme-aware variants for light/dark/
  high-contrast — three SVG sets in `assets/tray/`):
  | State | Glyph | Color | Notes |
  |---|---|---|---|
  | Idle | mic-outline | Theme-default (dim grey) | Default |
  | Armed | mic-outline + dot badge | **Blue** (#3A82F6 light / #5BA3FF dark) | Wake-word listening |
  | Recording | mic-filled | **Red** (#DC2626 / #EF4444) | Active capture |
  | LiveDictating | mic-filled + waveform | Red | Streaming preview live |
  | CommandListening | mic-filled + bolt | **Purple** (#7C3AED / #A78BFA) | Capturing for macro engine |
  | Processing | mic-filled + spinner | **Amber** (#D97706 / #F59E0B) | STT/LLM in flight |
  | Suspended | mic-slash | Grey-with-X | Wake-word off by policy |
  | Error | mic-alert | Red-with-X | Last action failed; click for log |
- [ ] R15.42. **Subtle pulse on Armed** — 0.5 Hz alpha breathe (1.0 →
  0.6 → 1.0 over 2 s) so the user always knows the mic is open during
  wake-word listening. No pulse on Recording (already unambiguous).
- [ ] R15.43. **Tooltip** carries the human-readable state plus
  context-specific detail (active wake phrase; estimated session cost;
  next AC-policy transition; last error message).
- [ ] R15.44. **Asset pipeline** — SVG sources live in `assets/tray/`;
  build script (`crates/fono-tray/build.rs`) rasterizes to PNG sets at
  16/22/24/32/48 px for SNI/AppIndicator hosts that don't render SVG
  directly (KDE Plasma 5 LTS, older GNOME indicator extensions).
- [ ] R15.45. **Color-blindness fallback** — `[tray].icon_set =
  "color" | "monochrome" | "shape_only"`. The shape_only set
  distinguishes states by glyph (waveform / bolt / spinner / slash / X)
  alone, no color reliance.

### R16 — Docs + ADRs (revised)

- [ ] R16.1. ADR `0009-interactive-live-dictation.md` (carryover from
  v2/v3).
- [ ] R16.2. ADR `0010-app-context-and-privacy.md` (carryover).
- [ ] R16.3. ADR `0011-voice-commands.md` (carryover).
- [ ] R16.4. ADR `0012-wake-word-activation.md` — **revised** to record
  the openWakeWord-default decision, the comparison matrix, the
  tract-ONNX runtime choice, the cascade architecture, and the
  battery-budget guarantees.
- [ ] R16.5. ADR `0013-tray-icon-state-palette.md` — formalizes the
  icon enum, color choices, accessibility fallbacks, and asset
  pipeline. Locks the palette so future state additions follow the
  same conventions.
- [ ] R16.6. `docs/wakeword.md` user guide — engine choice, cost on
  this machine, privacy, troubleshooting, custom-phrase training.
- [ ] R16.7. README "Wake word" section + asciinema demo.

## Sequencing (deliverable slices)

Unchanged from v3:

1. **Slice A** — Streaming + budget engine + overlay (local-first):
   R1–R3, R5, R7, R10 (partial), R12. v0.2.0-alpha.
2. **Slice B** — Cloud streaming + app context: R4, R8.3–R8.4, R9.5,
   R10.4, R11, R13. v0.2.0.
3. **Slice C** — Voice command macros: R9.6, R14, ADR. v0.3.0.
4. **Slice D** — Wake-word + tray icon palette: R15, R16.4, R16.5.
   Bundled with C in v0.3.0 (shared consent UX, command-mode FSM,
   icon palette).
5. **Slice E** — Polish: R6, R4.3, richer app context. post-v0.3.

## Verification Criteria

(All v3 criteria carry over.)

- **Wake-word default engine** is openWakeWord-via-tract; built-in
  classifier heads for `"computer"`, `"hey_jarvis"`, `"alexa"`,
  `"hey_mycroft"`, `"ok_nabu"` ship in the release binary asset bundle.
- **Wake-word CPU cost** ≤ 3% sustained of one core on Recommended-
  tier reference machine with stage-1 skip rate ≥ 80% in a quiet room.
- **Activation latency** ≤ 500 ms p95 word-end → capture-start.
- **False-positive rate** ≤ 5/hour against the LibriSpeech test-clean
  ambient fixture.
- **Tray top-level toggle** is reachable in ≤ 1 click from the tray
  icon; icon-state changes are visually distinguishable in <100 ms of
  state transition.
- **Color-blindness fallback** — `shape_only` icon set passes manual
  inspection (each state distinguishable without color).
- **Tray asset pipeline** produces correctly-sized PNGs for SNI hosts
  that reject SVG (KDE Plasma 5, older GNOME indicators); verified by
  packaging smoke test on NimbleX.

## Potential Risks and Mitigations

(All v3 risks carry over; new ones below.)

20. **openWakeWord embedding extractor adds ~10 MB to release binary
    asset bundle** (the `melspec` and `speech_embedding` ONNX models).
    Mitigation: ship as separate downloadable assets fetched on first
    enable (mirrors how local STT models are handled today); slim
    cloud-only builds skip the wake-word feature entirely; size budget
    documented in `docs/wakeword.md`.
22. **`tract` ONNX cold-start latency** (~50–100 ms first inference).
    Mitigation: warm the engine during daemon startup if
    `[wakeword].enabled = true`, behind the existing prewarm hook.
23. **Tray icon palette differs across SNI/AppIndicator hosts** —
    Plasma renders SVG, older GNOME indicator extensions need PNG sets,
    macOS template-image conventions are different again.
    Mitigation: build-script rasterization (R15.44); per-host
    integration smoke tests in CI; documented host caveats in
    `docs/troubleshooting.md`.
24. **User confusion between Armed-pulse and Recording** if the pulse
    is too aggressive.
    Mitigation: subtle 0.5 Hz alpha breathe with 0.6 minimum opacity;
    Recording uses solid-fill with no pulse; documented in the icon
    legend section of `docs/wakeword.md`.

## Alternative Approaches

(All v3 alternatives carry over; new ones below.)

11. **Keep rustpotter as default, openWakeWord as opt-in** (the v3
    posture). Smaller binary, simpler integration, but materially worse
    accuracy and a tiny pretrained-model library. Rejected — accuracy
    is the dominant UX metric for wake-word and openWakeWord wins it
    decisively.
12. **Drop rustpotter entirely** in favor of openWakeWord-only.
    Tempting (less code surface), but rustpotter's smaller footprint
    is a real win for embedded / minimum-tier targets. Kept as
    feature-flagged alternative.
13. **Single tray icon with text label changes** instead of a state-
    palette. Simpler, but text in tray icons is unreliable across
    Wayland compositors and ignored entirely by SNI on KDE. Rejected.
14. **Animated icon for every state** (not just Armed). Looks "alive"
    but increases CPU draw cost on the tray side and is distracting.
    Rejected — only Armed pulses; everything else is static.
