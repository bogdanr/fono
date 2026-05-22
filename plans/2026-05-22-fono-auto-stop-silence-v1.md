# Auto-Stop on Silence — v1

## Objective

Make `audio.auto_stop_silence_ms` actually do something. Today the
config knob, the tray submenu, and the daemon plumbing all exist
(`crates/fono-core/src/config.rs:233`, `crates/fono-tray/src/lib.rs:196`,
`crates/fono/src/daemon.rs:973-978`), but no consumer reads the value
— the capture loop in `crates/fono/src/session.rs` never references
it. Users who set it to "1.5 s" get the same behaviour as "Off":
recording continues until the hotkey fires again.

Ship a noise-gate-style auto-stop watchdog with hysteresis, adaptive
ambient-noise tracking, and a calm, observable UX that doesn't punish
the user for thinking mid-dictation. Land it in four reviewable
slices so behaviour change is gated on observability work that lands
first.

## Background and shape of the design

Phrased in audio-engineering terms because the problem is literally a
noise gate: decide per-frame whether RMS is above an ambient-relative
threshold, with hysteresis between open and close, a hold time before
the visual transition, and a release time before the commit. None of
this is novel; we are borrowing standard DAW noise-gate conventions
(FabFilter Pro-G / ReaGate / Waves C1) wholesale and only the
*consumer* of the gate-shut signal — `on_stop_recording()` instead of
muting output — is fono-specific.

### State machine

```
Armed ──speech──► Speaking ──silence≥pondering_visual_ms──► Pondering
   ▲                  ▲                                         │
   │                  └────── speech (snap) ───────────────────┘
   │                                                            │
   └─ recording never auto-commits in Armed (preamble required) │
                                                                ▼
                                       silence≥auto_stop_silence_ms
                                                                │
                                                                ▼
                                                           Committed
                                                  (= synthetic on_stop_recording)
```

- `Armed` = recording is on but no qualifying speech yet. Watchdog
  disabled. Kills the "I pressed the dictation key and need 4 s to
  think before starting" failure mode.
- `Speaking` = ≥ `speech_confirm_arm_ms` of contiguous voiced frames
  observed since the last silence run. Resets the pondering timer.
- `Pondering` = silence run reached `pondering_visual_ms`. Watchdog
  armed; overlay label flips from "Recording" to "Pondering…" and the
  visual countdown starts 1 s later (see UX section).
- `Committed` fires when the silence run reaches
  `auto_stop_silence_ms` total. Any voiced frame in `Pondering` →
  back to `Speaking` instantly (snap, no animation).

### Noise-gate parameters (locked)

| Param | Default | Notes |
|---|---:|---|
| `auto_stop_silence_ms` | `0` | Off by default. Tray presets: 0 / 5000 / 10000. |
| `auto_stop_require_speech` | `true` | Armed state must observe speech before commit is possible. |
| `speech_confirm_arm_ms` | `100` | Voiced duration required for `Armed → Speaking`. Single value (no separate resume threshold). |
| `pondering_visual_ms` | `1000` | Silence duration before the "Pondering…" label and state pill flip. |
| `speech_gate_db` | `+11 dB` | Open threshold above `floor_rms`. |
| `silence_gate_db` | `+6 dB` | Close threshold above `floor_rms`. Hysteresis = open − close = 5 dB. |
| `floor_floor_dbfs` | `-25 dBFS` | If `floor_rms` exceeds this, auto-stop is disabled for the session and a one-shot tray notification fires. |
| `floor_ema_window_ms` | `3000` | Slow EMA over the quietest 20 % of recent frames. |
| `inst_rms_ema_window_ms` | `30` | Fast EMA for the moving dot on the meter. |
| `voiced_rms_ema_window_ms` | `500` | Mid-speed EMA for the green-fill marker. |

All dB values stored linearly internally (0.0–1.0); every log line
and overlay label renders dBFS via `20·log10(rms)`.

### UX (locked)

- **Pondering label** appears at `pondering_visual_ms = 1000 ms` of
  silence.
- For 1 s after that, the label is plain (gives the user a moment to
  resume without seeing the countdown start).
- At `pondering_visual_ms + 1000 ms = 2000 ms`, the **walking-letter
  highlight** begins: one glyph at a time across the 9 letters of
  "Pondering" (the `…` stays static), hue-shifted +45° from the
  base text colour with a small saturation bump. Cursor advances
  linearly across `auto_stop_silence_ms − 2000 ms`.
- Cadence at 5 s preset: 333 ms/letter. At 10 s preset: 889 ms/letter.
- On speech-resume, snap (single frame): all letters back to normal
  colour, label snaps to "Recording", state pill flips OPEN. Slow
  walk forward, instant restore.
- If `auto_stop_silence_ms ≤ pondering_visual_ms + 1000 ms` (only
  reachable by hand-editing `config.toml`), the walk window
  collapses; label stays plain "Pondering…" until commit. One-shot
  WARN at session start.
- **State pill** below the label: green "OPEN" in Speaking, red
  "SHUT" in Pondering, grey "ARMING" in Armed. Always present when
  auto-stop is enabled; suppressed when `auto_stop_silence_ms = 0`.

### Gate-meter widget (locked)

Repurpose the existing `overlay.volume_bar` from bool to enum:

```toml
[overlay]
volume_bar = "simple"   # off | simple | advanced
```

- **Off** — no bar.
- **Simple** — vertical bar, dBFS-log scale, white moving dot at
  `inst_rms`, green fill up to `voiced_rms`, grey fill up to
  `floor_rms`. No threshold ticks. **Default.**
- **Advanced** — adds orange tick at `speech_gate`, red tick at
  `silence_gate`, faint shaded band between them (the hysteresis
  zone), small dBFS axis labels (-60 / -40 / -20 / 0), and the red
  "noise too high" marker at `floor_floor_dbfs`. Config-file-only;
  tray menu surfaces only Off / Simple.

The bar is visible in any overlay state that has live audio —
`Recording { db }`, `LiveDictating`, the new `Pondering { db }` —
and hidden in `Hidden`, `Processing`, `Polishing`. Today's
`state_has_vu_bar` gate (`crates/fono-overlay/src/renderer.rs:938`)
is replaced by a `state_has_live_audio` predicate. This means a user
who edits the config to `"simple"` (or `"advanced"`) sees the bar
across *every* visualization style (waveform / oscilloscope / bars
/ live-transcript), not only the live-transcript overlay.

### Breaking change

`[overlay].volume_bar: bool → "off" | "simple" | "advanced"`. No
back-compat shim. Existing configs with `volume_bar = true` will
fail to deserialise with a clear `expected string` error. Listed
under **Breaking** in `CHANGELOG.md`. The single-bool form was only
in user configs since the 2026-04-29 waveform-overlay-v2 plan
landed; the userbase is small and the project owner has opted out
of the shim explicitly.

## Slices

Four slices, each independently mergeable, each unlocking the next.
The order matters: slices 1 and 2 build the observability before
slice 4's behaviour change is allowed to fire. If slice 2's
dogfooding turns up false-Pondering pathologies we tune in slices 1
or 2 before slice 4 ships.

---

### Slice 1 — Envelope follower + debug logging (no UI, no behaviour change)

Goal: land the measurement layer with zero downstream consumer.
Produces telemetry we'll use to tune slice 4's thresholds.

- [x] **1.1** New module `crates/fono-audio/src/envelope.rs`:
  - `pub struct EnvelopeFollower { inst_rms, floor_rms, voiced_rms, .. }`
  - `pub fn push_frame(&mut self, frame: &[f32])` — updates the
    three EMAs per the time constants in the parameters table.
  - `pub fn snapshot(&self) -> EnvelopeSnapshot` — returns linear
    RMS values *and* dBFS conversions, plus derived `speech_gate`
    and `silence_gate`.
  - Floor follower: rolling 3 s window, EMA over the *quietest 20 %*
    of frames (NOT a plain EMA — a plain EMA tracks voice as much
    as silence). Use a small sorted buffer + percentile.
  - Doctest covering: pure silence → floor near input level, voiced
    near zero; speech burst → voiced rises, floor unchanged.
  - SPDX header.

- [ ] **1.2** *Deferred to slice 2.* The follower needs no in-session
  consumer in slice 1 — the CLI subcommand (1.5) runs it standalone
  against the default input device. Wiring into
  `crates/fono/src/session.rs` lands in slice 2 alongside
  `SilenceWatch`, which is the first real consumer.

- [ ] ~~**1.3**~~ *Dropped.* No persistent `[audio.debug]` config
  section. The follower's outputs are surfaced exclusively through
  the standalone CLI subcommand (1.5). Slice 2's state-transition
  logs and slice 4's PCM dump will use ad-hoc CLI flags or tracing
  targets (`RUST_LOG=fono::silence_watch=info`) rather than config
  toggles — keeps the on-disk schema free of debug knobs.

- [ ] ~~**1.4**~~ *Dropped.* No 1 Hz in-session log line. Same
  reasoning as 1.3 — the data is available on demand via 1.5; a
  daemon-resident log line is either always-on noise or requires a
  persistent flag.

- [x] **1.5** ~~Hidden subcommand `fono debug levels`~~ — landed
  during dogfooding to tune envelope parameters, then **removed**
  after slice 3 shipped the live Advanced VU bar in the overlay.
  The on-screen meter reads the same dBFS values in real time
  without a separate CLI tool, so the subcommand became dead code.

- [x] **1.6** Unit tests for `EnvelopeFollower`: pure-silence floor,
  speech burst doesn't lift floor, sustained speech raises voiced
  EMA, EMA time constants behave per spec (within ±10 %).

- [x] **1.7** Pre-commit gate: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`. CHANGELOG `## Added` entry.

**Risk**: trivial. Pure observation, no decisions yet.

---

### Slice 2 — Pondering state machine (visual only, no commits)

Goal: ship the state machine end-to-end with the *only* effect being
the overlay's "Pondering…" label and state pill. No audio decision
is taken. This is the dogfooding instrument for slice 4.

- [x] **2.1** New module `crates/fono-audio/src/silence_watch.rs`:
  - `pub struct SilenceWatch { state, since_*, cfg }`
  - `pub fn observe(&mut self, snap: &EnvelopeSnapshot) -> Option<WatchTransition>`
  - `WatchTransition` enum: `EnteredSpeaking`, `EnteredPondering`,
    `EnteredArmed`, `Committed` (last variant emitted but
    unconsumed in this slice).
  - Hysteresis: open via `inst_rms ≥ speech_gate` and ≥
    `speech_confirm_arm_ms` of voiced; close via `inst_rms <
    silence_gate` continuously.
  - Pondering entry at `pondering_visual_ms` of close-state.
  - Commit deferred to slice 4 (return the variant but no consumer
    yet).

- [x] **2.2** Add `OverlayState::Pondering { db }` to
  `crates/fono-overlay/src/lib.rs` (and the renderer/backend match
  arms). State pill: green "OPEN" / red "SHUT" / grey "ARMING".
  When `auto_stop_silence_ms = 0`, the pill is suppressed entirely
  (no auto-stop means no gate to display).

- [x] **2.3** Walking-letter highlight on "Pondering":
  - 1 s of plain "Pondering…" after entry.
  - Then highlight letters[0], letters[1], …, letters[8] across
    `auto_stop_silence_ms − 2000 ms`.
  - Highlight = +45° hue shift in HSV from the base text colour,
    +15 % saturation, value unchanged.
  - On `EnteredSpeaking` → snap reset in a single frame.
  - If walk window ≤ 0 ms: no walk, one-shot WARN log.

- [x] **2.4** Hook `SilenceWatch::observe` into the capture thread.
  When `audio.debug.log_pondering = true`, emit one INFO per
  transition:
  ```
  fono::session silence_watch armed→speaking via 142 ms voiced @ -28 dBFS
  fono::session silence_watch speaking→pondering after 1024 ms close
  fono::session silence_watch pondering→speaking after 312 ms (resume)
  ```
  At end of session, emit a summary:
  ```
  session ended; pondering_enters=4 pondering_aborts=3 pondering_max_ms=8412
  ```

- [ ] ~~**2.5**~~ *Dropped.* Per the slice-1 rollback, there is no
  persistent `floor_rms` to compare against. Re-evaluate in slice 4
  if/when the floor estimator returns.

- [ ] **2.6** *Deferred.* Integration test in `crates/fono/tests/live_pipeline.rs`
  (which already has a synthetic-frame pump at `:147-156`):
  - Feed speech → 4 s silence → assert exactly one
    `EnteredPondering` transition and no `Committed`.
  - Feed speech → 800 ms silence → speech → assert no
    `EnteredPondering` (under the 1 s threshold).
  - Feed speech → 1.5 s silence → speech-impulse (single frame) →
    silence → assert `EnteredPondering` not `EnteredSpeaking`
    (`speech_confirm_arm_ms` rejects single frames).

- [x] **2.7** Pre-commit gate + CHANGELOG `## Added`.

**Risk**: low. UI change only; no audio decision. Worst case is a
visually wrong label.

---

### Slice 3 — Gate-meter widget (advanced flavour + per-state visibility)

Goal: the dBFS bar described in the design. Slice 2 already lit up
the *simple* flavour by virtue of the existing `volume_bar` bar
existing; slice 3 is the schema change, the advanced flavour, and
the state-gate refactor.

- [x] **3.1** Breaking schema change: `[overlay].volume_bar: bool →
  enum {Off, Simple, Advanced}`. Edit
  `crates/fono-core/src/config.rs:792` and the default at `:797`.
  Default is `Simple`. No back-compat deserialiser. Update fixture
  configs in `crates/fono-core/tests/*`.

- [x] **3.2** Renderer changes in
  `crates/fono-overlay/src/renderer.rs`:
  - Replace `state_has_vu_bar(self.state)` (`:938`, `:1248`) with
    `state_has_live_audio(self.state)` = `Recording | LiveDictating
    | Pondering`.
  - In `Simple` mode: bar as today (white dot at `inst_rms`, green
    fill to `voiced_rms`, grey fill to `floor_rms`).
  - In `Advanced` mode: add orange tick at `speech_gate`, red tick
    at `silence_gate`, shaded band between them, dBFS axis labels
    (-60 / -40 / -20 / 0), red marker line at `floor_floor_dbfs`.
  - Tooltip / debug-print path (when overlay is run in debug) shows
    numeric dBFS values so screenshots are usable in bug reports.

- [ ] ~~**3.3**~~ *Dropped for this slice.* The tray submenu for
  `volume_bar` was deferred to the slice-4 tray work (where the
  auto-stop presets land). Users who want `Advanced` edit
  `config.toml`; `Simple` is the shipping default.

- [x] **3.4** Per-state visibility test: small renderer unit tests
  assert `state_has_vu_bar` covers `Recording`, `Pondering`,
  `LiveDictating`, `AssistantRecording` and rejects `Hidden`,
  `Processing`, `Polishing`, `AssistantThinking`. Full image-
  snapshot tests skipped — existing pixel paths are stable and
  the dispatch is exercised end-to-end at runtime.

- [x] **3.5** CHANGELOG `## Breaking` entry naming the config key
  and giving the migration: `volume_bar = true` → `volume_bar =
  "simple"`; `volume_bar = false` → `volume_bar = "off"`.
  CHANGELOG `## Added`: advanced gate meter flavour.

- [x] **3.6** Pre-commit gate.

**Risk**: low-medium. Renderer paths are well-tested; the only
user-visible breakage is configs that have to be edited.

---

### Slice 4 — Auto-stop commit (the real behaviour change)

Goal: actually wire `auto_stop_silence_ms` into the recording loop.
Only land after slice 2 has been dogfooded long enough to have
confidence that `Pondering` triggers correctly in the wild.

- [x] **4.1** Tray preset rename + bump:
  ```rust
  // crates/fono-tray/src/lib.rs:196
  pub const AUTO_STOP_PRESETS_MS: &[(&str, u32)] = &[
      ("Off",  0),
      ("5 s",  5_000),
      ("10 s", 10_000),
  ];
  ```
  Drop the existing 0.8 s / 1.5 s / 3 s presets — they're chat-app
  values, wrong for prose dictation. Config default stays `0`.

- [x] **4.2** `SilenceWatch::push` returns `SilenceEvent::Committed` when
  the silence run in `Pondering` reaches `auto_stop_silence_ms`.
  Implemented as `SilenceWatchConfig::auto_stop_silence_ms:
  Option<u32>`; `None` keeps the existing visual-only behaviour.
  Five new unit tests in `silence_watch.rs` lock the semantics.

- [x] **4.3** Consume `Committed` in the silence-watch task
  (`crates/fono/src/session.rs:986-1000`) by sending
  `HotkeyAction::TogglePressed` through the orchestrator's existing
  `action_tx`. The daemon's central loop translates this to
  `LiveTogglePressed` when live preview is on. Auto-stop is
  observationally identical to a hotkey press — same FSM, same
  `on_stop_recording` call, same overlay transitions to Processing.

- [x] **4.4** Gating rules (all enforced):
  - Toggle mode only: the silence-watch task itself only spawns
    when `RecordingMode::Toggle`. Hold-to-talk and assistant-hold
    never spawn it.
  - `auto_stop_silence_ms > 0`: zero →
    `SilenceWatchConfig::auto_stop_silence_ms = None`, no commit.
  - Speech preamble: enforced by construction — `Committed` only
    fires from `Pondering`, which can only be entered from
    `Speaking`, which requires `speech_confirm_arm_ms`.
  - ~~Floor-too-high disable~~ — dropped, we have no floor
    estimator. The voice-relative threshold self-calibrates without
    one (slice-1 retrospective).

- [ ] ~~**4.5** PCM dump on commit~~ — **dropped**. Persistent
  `[audio.debug]` config was killed in slice 1. If we want
  post-mortem PCM dumps later they belong behind a CLI flag.

- [ ] ~~**4.6** Integration tests in live_pipeline.rs~~ —
  **deferred**. Unit-test matrix on `SilenceWatch` covers every
  commit semantics. The wiring is one `action_tx.send` call;
  scaffolding a full orchestrator + overlay-stub + capture-pump
  integration harness to assert one line of glue is
  over-investment. Re-evaluate if dogfooding surfaces a wiring bug.

- [x] **4.7** Tray submenu — verified the label format works with
  the new presets (slice 0 was dropped, submenu was never disabled
  in the first place). Three-entry submenu reads cleanly.

- [x] **4.8** Documentation:
  - `crates/fono-core/src/config.rs:230` doc-comment rewritten
    with the actual semantics (toggle-only, voice-relative
    threshold, speech preamble by construction).
  - `docs/providers.md` — **skipped**, no provider-specific
    auto-stop behaviour to document.

- [ ] **4.9** ROADMAP.md update + CHANGELOG release-tag
  entry — done on tag day, not now.

- [x] **4.10** Pre-commit gate (`cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`).

**Risk**: medium. First slice that actually changes behaviour.
Mitigated by `write_pcm` debug capture, by the speech-preamble
requirement, by the floor-too-high disable, and by the hold-to-
talk exemption.

## Out of scope

- **Silero ONNX VAD wiring.** The `SileroVad` placeholder at
  `crates/fono-audio/src/vad.rs:43-49` is unrelated. Our gate is
  energy-based, not learned. A future slice can swap the energy
  RMS for a Silero confidence score if the energy gate proves
  inadequate in real-world noise (open window, fan, cafe), but the
  state machine and UX layered on top stay identical.
- **Per-language pondering tuning.** Speech cadence varies (Mandarin
  pauses differ from English), but `auto_stop_silence_ms` is a
  user-set knob and 3 s / 5 s are large enough that per-language
  tuning is not interesting until we have telemetry that says
  otherwise.
- **Auto-stop on the assistant flow.** Assistant is hold-to-talk;
  the user owns the boundary. Slice 4 explicitly excludes it.
- **`audio.debug.write_pcm` retention policy.** First version dumps
  on every commit when enabled; we don't garbage-collect. Users
  enabling this flag are debugging and will clean up manually. A
  retention sweep can come later if anyone leaves the flag on for
  weeks.

## Verification

After slice 4 lands, the manual verification protocol is:

1. `fono setup` fresh on a clean profile.
2. Tray → Auto-stop after silence → "10 s".
3. Press the dictation key (F7 by default) to toggle. Stay silent
   for 30 s. Confirm: recording does *not* stop (preamble required).
4. Press the dictation key to cancel. Press it again. Say "hello
   world". Stay silent. Confirm: at ~1 s the label flips to
   "Pondering…"; at ~2 s the first letter takes a +45° hue shift;
   the cursor walks across "Pondering" over 8 s; at ~11 s recording
   auto-commits and the pipeline runs.
5. Press the dictation key. Say "hello world". 2 s silence. Say
   "again". Confirm:
   the label snaps back to "Recording" at the resume, the highlight
   resets, and the second utterance is captured fully.
6. Set `audio.debug.levels = true` and `volume_bar = "advanced"`.
   Repeat (5). Confirm the meter shows live `floor_rms` /
   `voiced_rms` / threshold ticks, and the DEBUG log line matches.
7. Cover the mic with a fan running. Confirm the floor-too-high
   notify fires within 2 s and auto-stop is disabled for the
   session (`Pondering` may flip on-and-off, but no `Committed`).

## File touch list (estimate)

- `crates/fono-audio/src/envelope.rs` (new, ~200 lines)
- `crates/fono-audio/src/silence_watch.rs` (new, ~250 lines)
- `crates/fono-audio/src/lib.rs` (re-exports)
- `crates/fono-core/src/config.rs` (schema + `[audio.debug]` section + `volume_bar` enum)
- `crates/fono-overlay/src/lib.rs` (`OverlayState::Pondering`, state pill API)
- `crates/fono-overlay/src/renderer.rs` (walking-letter highlight, advanced meter, state predicate)
- `crates/fono-tray/src/lib.rs` (`AUTO_STOP_PRESETS_MS`, `SetVolumeBarMode` action)
- `crates/fono/src/session.rs` (envelope thread-through, `SilenceWatch` integration, commit hookup)
- `crates/fono/src/daemon.rs` (`TrayAction::SetVolumeBarMode` handler)
- `crates/fono/src/cli.rs` (hidden `debug levels` subcommand)
- `crates/fono/tests/live_pipeline.rs` (integration tests)
- `CHANGELOG.md` (Breaking + Added entries)
- `ROADMAP.md` (move to Shipped on tag day)

## Status

- 2026-05-22 — design locked, plan opened.
- 2026-05-22 — slice 1 landed (envelope follower + `fono debug levels`
  CLI). No behaviour change in the daemon yet; pure measurement layer.
  See `docs/status.md` for the session log.
- 2026-05-22 — slice 2 landed (`SilenceWatch` state machine +
  visual `Pondering` overlay state with walking-letter highlight,
  toggle-mode only).
- 2026-05-22 — slice 3 landed (`volume_bar` bool → enum with
  Advanced annotations: green voiced reference + amber silence
  threshold ticks on the right-edge VU bar).
- 2026-05-22 — slice 4 landed (auto-stop commit wired:
  `SilenceEvent::Committed` → `HotkeyAction::TogglePressed` →
  `on_stop_recording`; tray presets `Off / 3 s / 5 s`).
