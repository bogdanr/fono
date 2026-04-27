# Fono — Interactive / Live Dictation + Context + Macros + Wake-Word (R-plan v3)

Date: 2026-04-27
Status: Proposed (supersedes v2)
Scope expansion from v2: local always-on wake-word activation as an
alternative to the dedicated command hotkey.

## Objective

Land a live-dictation experience that feels instant (≤ 300 ms first
feedback) without inflating cloud costs above the current batch baseline
or degrading committed local-inference quality, plus:

1. **App-aware context** (carryover from v2).
2. **Voice command macros** (carryover from v2).
3. **Local wake-word activation** ("computer", "fono", or user-trained)
   so users can trigger dictation or commands hands-free, with an
   explicit cost budget that protects battery life.

All three layers remain opt-in.

## Locked architectural decisions

(All v2 decisions carry over.)

8. **Wake-word engine runs entirely locally** — audio never leaves the
   machine until activation. No cloud KWS.
9. **Default engine: rustpotter** (Apache-2.0 / MIT pure-Rust).
   Alternative `openWakeWord`-via-`tract` engine selectable by cargo
   feature for users wanting the larger pretrained model library.
   Porcupine is opt-in only, like Llama/Gemma, due to non-OSS licensing.
10. **Battery-aware default** — wake-word listener defaults to
    `active_when = "ac_only"`. User explicitly opts into always-on.
11. **Cascade architecture** — coarse 8 kHz energy/VAD gate first,
    16 kHz neural confirm second. Full inference runs ~5–10% of the
    time in typical quiet rooms.
12. **Wake-word listener pauses during active dictation/commands** —
    eliminates self-trigger false positives.

## Implementation Plan

### R1 — R14 (carryover from v2)

[Reference v2 plan; no changes.]

### R15 — Wake-word activation engine (NEW)

#### R15a — Engine + audio plumbing

- [ ] R15.1. New crate `crates/fono-wakeword/`. Define `WakeWordEngine`
  trait → `Stream<Item = WakeEvent { phrase, confidence, t_audio }>`.
- [ ] R15.2. Backend: `rustpotter` impl (`wakeword-rustpotter` cargo
  feature, **on by default**). Ships with a built-in "computer" model
  (community-trained or fono-trained from synthesized + crowd-sourced
  samples) and accepts user-trained `.rpw` models.
- [ ] R15.3. Backend: `openwakeword` impl via `tract` ONNX runtime
  (`wakeword-onnx` cargo feature, off by default). Larger model zoo
  including pretrained "computer", "jarvis", "alexa", "mycroft", etc.
- [ ] R15.4. Backend: `porcupine` impl via `pv_porcupine` crate
  (`wakeword-porcupine` cargo feature, off by default, opt-in only —
  documented as proprietary/non-OSS analogous to Llama/Gemma).
- [ ] R15.5. **Audio source sharing** — wake-word listener consumes
  from the same `fono-audio` frame stream as dictation. No second mic
  open. Frame stream gains a fan-out subscriber API so the wake-word
  consumer doesn't starve the dictation consumer (or vice versa).

#### R15b — Cost-control cascade

- [ ] R15.6. **Stage 1 — energy gate** (always on when listener
  enabled). Computes RMS over 30 ms windows at native sample rate.
  If RMS < `[wakeword].noise_floor_db` (auto-calibrated at startup
  from the first 5 s of audio), skip stages 2–3 entirely.
  Target cost: < 0.1% of one core.
- [ ] R15.7. **Stage 2 — coarse VAD** (runs only when stage 1 passes).
  Lightweight VAD at 8 kHz (the existing `fono-audio` VAD downsampled).
  If no voice, skip stage 3. Target cost: < 0.3% of one core.
- [ ] R15.8. **Stage 3 — neural confirm** (runs only when stages 1+2
  pass). Full feature extraction + NN inference at 16 kHz. Target cost:
  1–3% of one core when active; ~5–10% duty cycle in typical use.
- [ ] R15.9. **Periodic mode** — `[wakeword].duty_cycle = "continuous"
  | "balanced" | "low_power"`. `balanced` (default on AC) runs stage 3
  at 30 Hz; `low_power` (default on battery) at 10 Hz; `continuous`
  pins to 60 Hz. User-overridable.

#### R15c — Battery & power-profile awareness

- [ ] R15.10. `BatteryMonitor` reads `/sys/class/power_supply/AC/online`
  and `/sys/class/power_supply/BAT*/capacity` on Linux; equivalents on
  macOS (`pmset -g batt`) and Windows (`GetSystemPowerStatus`). Polls at
  10 s.
- [ ] R15.11. `[wakeword].active_when` enum: `always`, `ac_only`
  (default), `ac_or_above_<N>%` (e.g., `ac_or_above_40`), `never`.
  Tray reflects active/idle state.
- [ ] R15.12. `[wakeword].battery_floor_pct` (default 20). Below this,
  listener auto-suspends regardless of `active_when`.
- [ ] R15.13. **Platform-profile awareness** — read
  `/sys/firmware/acpi/platform_profile` (Linux) or platform equivalents.
  On `low-power`, downgrade `duty_cycle` one step.
- [ ] R15.14. **Lid-close auto-suspend** — listen for systemd-logind
  `LidClosed` signal (or compositor equivalent); pause engine until lid
  reopens.

#### R15d — Activation flow + actions

- [ ] R15.15. `WakeAction` config enum: `dictate` (start a dictation
  session, hotkey-equivalent), `command` (start a command session, R14
  equivalent), `dictate_with_phrase` (treat the words *after* the wake
  word in the same utterance as dictation; requires continuous listen
  for ~5 s post-wake).
- [ ] R15.16. **Multi-phrase routing** — different wake words map to
  different actions: `[wakeword.phrases]` table maps phrase → action.
  Default: `"computer"` → `command`, `"dictate"` → `dictate`.
- [ ] R15.17. **Activation latency target**: ≤ 500 ms from word-end to
  audio capture start (typical rustpotter latency ~200–300 ms; budget
  includes buffer drain + state transition).
- [ ] R15.18. **Pre-roll capture** — keep last 1.5 s of audio in a ring
  so `dictate_with_phrase` can transcribe words spoken immediately after
  the wake word without missing onset.
- [ ] R15.19. **Self-trigger prevention** — wake-word listener pauses
  the moment the FSM enters `Recording` / `LiveDictating` /
  `CommandListening`; resumes on return to `Idle`.
- [ ] R15.20. **Confirmation cue** (optional, default off):
  `[wakeword].activation_chime` plays a short tone via the existing
  audio output path. Useful for users who want feedback before speaking
  the command.

#### R15e — False-positive handling

- [ ] R15.21. **Confidence threshold** — `[wakeword].threshold` (default
  engine-specific, ~0.5 for rustpotter). Tunable; higher = fewer false
  positives, more false negatives.
- [ ] R15.22. **Silence-prefix gate** — require ≥ 200 ms of relative
  silence (RMS < noise_floor + 6 dB) immediately preceding the wake
  word. Rejects activation triggered by phrases inside continuous
  speech.
- [ ] R15.23. **Activation timeout** — if wake word triggers but no
  speech follows within `[wakeword].follow_through_ms` (default 3000),
  silently cancel session, no notification.
- [ ] R15.24. **Recent-trigger debounce** — after an activation, ignore
  further wake events for 1.5 s.
- [ ] R15.25. **Per-user calibration** — `fono wakeword calibrate`
  records 5 utterances of the wake word from the user, computes a
  personalized confidence threshold, stores in
  `~/.config/fono/wakeword.cache`.

#### R15f — Privacy + transparency

- [ ] R15.26. **First-run consent step** in the wizard before enabling
  the listener. Explicit text: "Fono will continuously listen for the
  wake word. Audio is processed locally, never sent to the cloud, never
  written to disk. Microphone activity is shown in the tray icon."
  User must check a box and click Confirm; no auto-enable.
- [ ] R15.27. **Tray indicator** — separate icon state ("listening" =
  small mic dot in tray icon corner) distinct from "recording". Tooltip
  shows "Wake-word listener active — say 'computer'".
- [ ] R15.28. **Hardware mic mute respected** — listener pauses while
  the OS reports the mic muted; tray reflects suspended state.
- [ ] R15.29. **Audio strictly in-RAM** — wake-word ring buffer never
  touches disk. Debug-recording mode (`FONO_WAKEWORD_DEBUG_RECORD=1`)
  exists but is documented and prints a warn-level log on every start.
- [ ] R15.30. **Activation log** at `~/.local/share/fono/wakeword.log`
  records each activation: timestamp, matched phrase, confidence, action
  taken. No audio. User-rotatable; documented in `docs/wakeword.md`.

#### R15g — Config + wizard + CLI + tray

- [ ] R15.31. New `[wakeword]` config block: `enabled` (default
  `false`), `engine`, `phrases`, `threshold`, `noise_floor_db`,
  `duty_cycle`, `active_when`, `battery_floor_pct`, `follow_through_ms`,
  `activation_chime`, `consent_acknowledged_at` (RFC3339).
- [ ] R15.32. Wizard step: explains cost (CPU + battery range), privacy
  model, wake-phrase choice, default action mapping. Walks the user
  through consent + an optional `wakeword calibrate` pass. Skipped on
  unsupported tiers.
- [ ] R15.33. CLI: `fono wakeword status` (active/idle, last 10
  activations, current confidence threshold), `fono wakeword test`
  (records 3 s, shows the engine's per-frame confidence trace —
  debugging helper), `fono wakeword calibrate`, `fono wakeword train
  <phrase>` (rustpotter only — record 8 samples and produce a custom
  `.rpw` model).
- [ ] R15.34. Tray menu entry: "Wake word: On / Off / On (AC only)" —
  click to cycle states; mirrors `[wakeword].active_when`.

#### R15h — Observability + tests

- [ ] R15.35. Tracing spans: `wakeword.stage1_skip_rate`,
  `wakeword.stage2_skip_rate`, `wakeword.activation_latency`,
  `wakeword.false_positive_count` (decided by the
  `follow_through_timeout` heuristic — wakes that didn't lead to a
  successful action).
- [ ] R15.36. Power telemetry — `fono doctor` reports
  `~/measured wake-word CPU: <x>% / mic-on share: <y>% / estimated
  battery cost on this machine: ~<z>%`. The estimated cost is
  calibrated from a 60 s self-bench (`fono wakeword bench`).
- [ ] R15.37. Integration test with synthetic wake-word audio →
  asserts activation fires within latency budget, no false trigger from
  ambient-noise fixture, clean self-suspend during dictation.
- [ ] R15.38. Battery regression test (manual, documented in
  `docs/wakeword.md`): reference machine baseline idle hours vs
  wake-word-on idle hours; documented in `docs/bench/battery-*.json`.

#### R15i — Docs + ADR

- [ ] R15.39. ADR `0012-wake-word-activation.md` — engine choice
  (rustpotter default + tract alternative + Porcupine opt-in),
  cascade architecture rationale, battery budget guarantees,
  privacy posture.
- [ ] R15.40. `docs/wakeword.md` user guide: cost table per tier,
  privacy explainer, troubleshooting (false positives, false negatives,
  battery debugging), how to train a custom phrase, hardware mic mute
  interaction.

## Sequencing (deliverable slices)

1. **Slice A — Streaming + budget engine + overlay (local-first):**
   R1–R3, R5, R7, R10 (partial), R12. v0.2.0-alpha.
2. **Slice B — Cloud streaming + app context (privacy-aware):**
   R4, R8.3–8.4, R9.5, R10.4, R11, R13. v0.2.0.
3. **Slice C — Voice command macros:** R9.6, R14, ADR. v0.3.0.
4. **Slice D — Wake-word activation:** R15. v0.3.0 same train as
   macros (they share the consent UX and command-mode plumbing).
5. **Slice E — Polish:** R6 live-inject, R4.3 Deepgram/AssemblyAI,
   richer app context (URL via WebExtension, editor file via Neovim/
   VS Code plugins). post-v0.3.

## Verification Criteria

(All v2 criteria carry over.)

- **Wake-word CPU cost**: ≤ 3% sustained of one core on Recommended-
  tier reference machine, with stage-1 skip rate ≥ 80% in a quiet
  room (verified by R15.36 self-bench).
- **Wake-word activation latency**: ≤ 500 ms p95 from end-of-phrase to
  capture-start (R15.37 fake-stream test + reference-machine manual).
- **Wake-word false-positive rate**: ≤ 5 per hour against the LibriSpeech
  test-clean reference noise fixture (synthetic ambient).
- **Battery**: documented worst-case impact on the reference laptop
  ≤ 5% reduction in idle battery life (R15.38 manual). Worst case
  acceptable disclosure threshold; users on hostile audio platforms
  will see the disclosed-in-doctor measured number.
- **Privacy**: no wake-word audio frame is ever written to disk by
  default builds (audited by file-system tracing test); first-run
  consent gate is unbypassable.
- **Self-suspend**: wake-word listener verifiably stops consuming frames
  during active dictation (verified by counter test).

## Potential Risks and Mitigations

(All v2 risks carry over; new ones below.)

13. **Battery cost on hostile laptops** (older Intel chassis where
    audio prevents deep package idle, 10–15% impact).
    Mitigation: `active_when = "ac_only"` default; `fono doctor` reports
    measured per-machine cost; docs explain the failure mode and the
    mitigation knobs (`duty_cycle`, `battery_floor_pct`).
14. **False-positive activations from TV/music.**
    Mitigation: confidence threshold, silence-prefix gate, debounce,
    follow-through timeout, per-user calibration. Documented tuning.
15. **Self-trigger during dictation** ("computer" appears in user's own
    speech, mid-dictation).
    Mitigation: hard-stop the listener while FSM ≠ `Idle`. Verified by
    integration test.
16. **Privacy backlash from always-on microphone.**
    Mitigation: explicit consent gate, tray indicator, audit log, no-
    disk policy, hardware mute respect, `active_when = "never"` is a
    legitimate first-class option. ADR + docs spell out the model.
17. **Custom-trained models leak voice samples** — if the user runs
    `fono wakeword train`, those samples must not leave the machine.
    Mitigation: training is fully local; the resulting `.rpw` model
    stays in `~/.config/fono/`; audit log notes training events.
18. **Wake-word engine licensing drift** — rustpotter / openWakeWord
    upstreams change license.
    Mitigation: pin versions in `Cargo.toml`; `deny.toml` audited per
    project rules; review on each bump.
19. **Wayland / pipewire mic-route changes mid-session** — engine
    silently goes deaf when the user changes default input.
    Mitigation: subscribe to PipeWire/PulseAudio default-source-changed
    events; auto-reattach. `fono wakeword status` shows current source.

## Alternative Approaches

(All v2 alternatives carry over; new ones below.)

7. **Wake-word always on by default** — better discoverability, worse
   battery and privacy posture; rejected.
8. **Wake-word via the configured cloud STT** (continuous streaming
   with keyword filter) — destroys cost guarantees of v2's R12
   budget engine; rejected.
9. **Wake-word via the configured local STT** (`whisper-tiny`
   continuous) — possible, but ~10× CPU of a dedicated KWS engine and
   no obvious quality gain; rejected as default. Could appear behind
   `engine = "whisper-tiny"` for users who want it.
10. **Hardware DSP offload** (Apple Neural Engine, Pixel AOC, x86
    audio DSPs) — would slash cost dramatically but the cross-platform
    surface is too fragmented to depend on; revisit per-platform if
    field reports show worst-case battery hits in practice.
