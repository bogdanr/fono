# Silent input-device auto-recovery (dock with capture-but-no-mic)

## Objective

When Fono opens the OS-default input device and the resulting capture is
silent — typical failure mode: an external dock advertises an audio
"capture" endpoint (HDMI capture, line-in, S/PDIF input) with no
microphone wired to it, and PulseAudio/PipeWire (or cpal's host) elects
that endpoint as the default source — the user must currently dive into
`pavucontrol`, `wpctl`, or `[audio].input_device` in
`config.toml` to fix it. That violates the "Fono should just work"
contract.

Goal: detect the silent-capture condition automatically, transparently
fall back to a working microphone, and remember the choice so the next
dictation is silent-free without any user action. Keep an explicit user
override sacred (never overwrite `[audio].input_device` when the user
has set it). Surface the switch in a single non-blocking toast plus
existing log lines so the user understands what happened.

## Scope and non-goals

- **In scope.** Linux (PipeWire / Pulse / ALSA via cpal), macOS,
  Windows. The dock reproducer is Linux but the silent-default failure
  mode also occurs on Windows when an HDMI sink registers a
  capture endpoint and on macOS when an aggregate device is left
  selected after unplug.
- **In scope.** Both batch (`record` / hold / toggle) and live
  (`record --live`, `[interactive].enabled = true`) capture paths.
  Both call `AudioCapture::start{,_with_forwarder}` at
  `crates/fono-audio/src/capture.rs:117,238`.
- **Not in scope.** Hotplug-on-the-fly stream swap mid-recording
  (cpal does not expose hotplug events; the value isn't worth the
  invasiveness for v0.x). Defer.
- **Not in scope.** Replacing cpal with a native PipeWire client.
  Defer to a separate ADR.

## Initial assessment

### Where the problem lives

`crates/fono-audio/src/capture.rs:118-130` and `:242-254` resolve the
device strictly via `cpal::default_host().default_input_device()` when
`config.audio.input_device` is empty. cpal's "default" mirrors the
system default, which on Linux means whatever PipeWire/Pulse points
`@DEFAULT_SOURCE@` at — and that is exactly the dock's silent capture
endpoint when a dock is plugged in.

Downstream, `crates/fono/src/session.rs:612-616` only checks
`samples.is_empty()` and `elapsed < MIN_RECORDING`. A silent capture
is *not* empty (it has plenty of zero-ish samples), so the recording
sails through trim → STT, where local Whisper hallucinates ("Thank
you." / "you" / "."), or cloud STT returns empty/garbage. Either way
the user sees a failed dictation with no actionable feedback.

### Existing infrastructure to lean on

- `crates/fono-audio/src/trim.rs:82` already has an `rms()` helper
  we can re-export.
- `crates/fono-audio/src/mute.rs` already shells out to PulseAudio /
  PipeWire CLIs (`pactl`, `wpctl`) — we can use the same pattern to
  query the *current* default source rather than relying on cpal's
  cached host default.
- `crates/fono-tray/src/lib.rs` already has the STT/LLM submenu
  pattern (Languages submenu, dynamic refresh, `TrayAction::*`); a
  Microphone submenu is a near-copy.
- `crates/fono/src/wizard.rs` already runs an in-wizard latency
  probe and reads `LANG`; adding a microphone-presence probe slots
  cleanly next to the existing flows.
- `notify-rust` toasts are already wired through `fono` for STT
  warnings; we re-use the same call pattern.

### Risk priority (highest → lowest)

1. **False positives that downgrade a real silent recording.** The
   user spoke quietly, or the room was almost-silent, and we
   incorrectly conclude "the device is broken". Mitigation: combine
   *peak amplitude* AND *signal energy over time* AND *stream-level
   underruns* — never decide "silent" from a single short capture
   below an energy threshold; require *no signal at all* across the
   entire recording (peak < ~1e-4) before fallback fires.
2. **Stomping a deliberate user choice.** User explicitly picked the
   dock for a reason (e.g. line-in capture from external gear).
   Mitigation: never auto-override when `config.audio.input_device`
   is set; persist the auto-pick to a *separate* `auto_input_device`
   field that the user override always wins over.
3. **Wrong fallback on multi-mic systems.** Laptop with internal mic
   + USB headset + dock: picking "first non-silent" might pick the
   internal mic when the user wanted the headset. Mitigation: rank
   candidates by ALSA/Pulse "priority" hints when available, prefer
   devices whose name matches "headset"/"mic"/"USB" patterns over
   ones matching "HDMI"/"Monitor"/"Loopback"/"capture".
4. **PipeWire / Pulse CLI absent.** Slim install. Mitigation: fall
   back to pure cpal enumeration; behaviour degrades to "scan all
   inputs, probe each, pick the one with signal".

## Implementation Plan

### Phase 1 — Detect: instrument captures with a silence verdict

- [ ] Task 1.1. In `crates/fono-audio/src/capture.rs`, extend
      `RecordingBuffer` with running `peak: f32` and `energy_sum: f64`
      counters updated inside `push_slice`. Add a
      `pub fn signal_verdict(&self, sample_rate: u32) -> SignalVerdict`
      that returns `Silent` when `peak < SILENT_PEAK_THRESHOLD` (≈
      `1e-4`, well below any real microphone noise floor) AND
      duration ≥ `MIN_RECORDING`, else `Voiced`. Rationale: keeps the
      audio crate the single owner of audio-quality heuristics; lets
      both the daemon and the wizard call the same verdict.

- [ ] Task 1.2. Mirror the counters in
      `start_with_forwarder` so the live path also produces a
      verdict. The forwarder closure can update an
      `Arc<AtomicU64>`-backed peak so we don't pay a mutex per
      callback. Rationale: live dictation suffers the same dock
      failure; do not hard-code the fix in only the batch path.

- [ ] Task 1.3. Unit-test `signal_verdict` against three fixtures:
      pure zeros (Silent), 1 % white noise (Voiced — covers a quiet
      room with a working mic), and dock-style flat-line with
      occasional 16-bit denormal noise (still Silent). Rationale:
      pin the false-positive boundary explicitly so future tweaks
      don't drift.

### Phase 2 — Resolve: build a device-ranking helper

- [ ] Task 2.1. Add `crates/fono-audio/src/device.rs` exposing:
      - `pub fn enumerate_inputs() -> Vec<InputDevice>` returning a
        struct with `name`, `is_default`, `kind: DeviceKind`
        (`Microphone` / `Capture` / `Loopback` / `Unknown`), and a
        `priority_hint: i32`.
      - `pub fn os_default_source() -> Option<String>` shelling
        out to `pactl get-default-source` / `wpctl status`
        (PipeWire) on Linux, `AVCaptureDevice.systemDefault` on
        macOS, `MMDeviceEnumerator::GetDefaultAudioEndpoint` on
        Windows. Falls through to None on slim systems.
      - `pub fn rank_candidates(current: &str) -> Vec<String>`
        returning device names ordered most-likely-mic first:
        explicitly-named "mic"/"headset"/"USB" patterns, then
        anything whose `kind == Microphone`, then anything `Capture`
        that isn't the current silent device, demoting `Loopback`
        and `Monitor` last.
      Rationale: keeps the OS-specific noise out of `capture.rs` and
      out of `daemon.rs`; gives the future tray submenu a single
      source of truth.

- [ ] Task 2.2. In `AudioCapture::start{,_with_forwarder}`, replace
      the bare `host.default_input_device()` arm with a four-tier
      resolver:
      1. `config.audio.input_device` (user override) — sacred.
      2. `config.audio.auto_input_device` (last successful auto-pick)
         — used when the device still exists.
      3. `device::os_default_source()` matched against
         `host.input_devices()` — catches dock plug/unplug because
         the OS already updates `@DEFAULT_SOURCE@`.
      4. `host.default_input_device()` — current behaviour.
      Rationale: tiers (2) and (3) eliminate the dock failure mode
      *before* a single sample is captured in the common case
      (PipeWire correctly switches the default away from a freshly
      plugged dock that has no source signal — `wpctl` already knows
      this); the silent-capture detector below catches the rest.

- [ ] Task 2.3. Add a `pub async fn probe_signal(name: &str) ->
      ProbeResult` that opens a 250 ms capture against the named
      device, computes peak amplitude, and returns `{name, peak,
      sample_rate, error}`. Rationale: needed by Phase 3's auto-pick
      and Phase 5's wizard / tray refresh.

### Phase 3 — Recover: silent-capture auto-fallback

- [ ] Task 3.1. In `crates/fono/src/session.rs::on_stop_recording`
      (around `:603-616`), after draining samples, call
      `RecordingBuffer::signal_verdict`. On `Silent`:
      1. Log a `warn!` line naming the device that produced silence
         and the captured peak amplitude.
      2. If `config.audio.input_device` is set (user override),
         emit a toast — *"Microphone '%s' captured no audio. Check
         that it isn't muted, then run `fono doctor` or open the
         tray Microphone submenu to switch."* — and skip STT.
         Do **not** auto-override a user choice.
      3. If unset, run `device::rank_candidates(current)` →
         `device::probe_signal()` for each in turn (200 ms each,
         hard-cap total wall time at 1500 ms). Pick the first
         candidate with `peak > SIGNAL_THRESHOLD`. On hit: persist
         it to `config.audio.auto_input_device`, emit a toast —
         *"Switched microphone from '%old' (no signal) to
         '%new'. Press your hotkey to retry."* — and ask the user
         to redo the dictation. Do not silently re-record: the
         current capture's audio is gone, and re-recording a
         different stream than the one the user spoke into would be
         confusing.
      4. Always send `HotkeyAction::ProcessingDone` so the FSM
         returns to Idle.
      Rationale: zero-friction recovery for the dominant case
      (single-user laptop with one working mic), explicit
      diagnostic + tray hint when the choice is ambiguous, never
      stomp a deliberate user override.

- [ ] Task 3.2. Mirror the verdict check in the live path
      (`crates/fono/src/live.rs` / `daemon.rs` live dispatch)
      after the live session ends. Same toast + persistence logic.
      Rationale: hold-to-talk over a dock fails identically in
      live mode; the fix must be uniform.

- [ ] Task 3.3. Add `pub auto_input_device: String` to
      `Audio` in `crates/fono-core/src/config.rs:189-201`,
      `#[serde(default, skip_serializing_if = "String::is_empty")]`
      so absence stays out of `config.toml`. Rationale: keeps the
      auto-pick distinct from the user override at every read site
      and at every config-rewrite site (`fono use`, wizard, etc.),
      so a future `fono use input <name>` cannot accidentally
      collide with auto-recovery.

### Phase 4 — Pre-flight: catch the dock at daemon start

- [ ] Task 4.1. In `daemon.rs` startup (after the existing
      `hardware_acceleration_summary` log line), spawn a one-shot
      `device::probe_signal` against the resolver-tier-2/3/4
      result. If silent and no user override → run the same
      `rank_candidates` walk and persist to `auto_input_device`
      *before* the first dictation hotkey ever fires. Hard cap
      total wall time at 800 ms; on timeout, log a `warn!` and
      defer to Phase 3's post-capture path. Rationale: with a
      working mic plugged in, the user's first dictation succeeds
      with no toast and no retry — closest to "just works".
      Without one, Phase 3 still saves the next attempt.

- [ ] Task 4.2. Skip the pre-flight when `config.audio.input_device`
      is set (user explicitly chose). Rationale: respects override.

- [ ] Task 4.3. On Linux, subscribe to PipeWire/Pulse default-source
      change events via a polled `pactl get-default-source` every
      2 s in a low-cost background task **only when the last
      capture was Silent** (don't burn CPU / spawn subprocesses
      when things work). Repeat the resolver on change. Rationale:
      catches mid-session dock plug/unplug without a daemon
      restart.

### Phase 5 — Surface: tray submenu + wizard probe + doctor row

- [ ] Task 5.1. Add a "Microphone" submenu to
      `crates/fono-tray/src/lib.rs` mirroring the STT/LLM submenu
      pattern: `Auto` (ticked when `input_device` is empty), then
      one entry per `device::enumerate_inputs()`, with the
      currently-active device ticked. New variants
      `TrayAction::SetInputDevice(String)` and
      `TrayAction::SetInputDeviceAuto`. Rationale: gives the user
      a one-click recourse identical to existing provider switching
      UX, and surfaces the device list for users on multi-mic
      systems who want to override Fono's auto-pick.

- [ ] Task 5.2. In `crates/fono/src/daemon.rs` dispatch table,
      handle the new `TrayAction::*` variants by calling a new
      `set_input_device(&str)` / `clear_input_device_override()`
      helper that atomically rewrites `[audio].input_device` and
      sends `Request::Reload` (same path as STT/LLM swaps).
      Rationale: zero-restart device switching matches the
      provider-switching guarantee.

- [ ] Task 5.3. In `crates/fono/src/wizard.rs`, after the
      hardware-tier section, run `device::probe_signal` against
      the resolved default. If silent, present a checklist of
      ranked candidates with the first non-silent one pre-selected
      and write the choice to `[audio].input_device`. Rationale:
      catches the dock-already-plugged-in case at install time
      so the very first dictation works.

- [ ] Task 5.4. In `crates/fono/src/doctor.rs`, add an "Audio
      input" row showing: configured device (or "auto"), resolved
      device, last-known peak amplitude, and a hint
      (`source = wpctl`/`pactl`/`cpal-default`) so support reports
      include why a given device was picked. Rationale: the
      existing doctor command is the one-stop diagnostic; without
      this row the silent-capture failure is invisible to it.

- [ ] Task 5.5. Add a "No audio captured" section to
      `docs/troubleshooting.md` describing: the auto-fallback
      behaviour, how to override (`fono use input <name>` or tray
      submenu), and how to clear the auto-pick
      (`fono use input auto`). Rationale: discovery; users hitting
      edge cases need a documented escape hatch.

### Phase 6 — Validation

- [ ] Task 6.1. New unit tests in `device.rs`: ranker prefers a
      "Headset" device over an "HDMI Capture", a "USB Microphone"
      over a "Monitor of …", and never returns the input it was
      told to avoid as the first candidate. Rationale: pins the
      heuristic against future regressions.

- [ ] Task 6.2. New integration test driving
      `RecordingBuffer::signal_verdict` with synthetic inputs
      from `tests/fixtures` (1 s silence; 1 s 1 % white noise;
      1 s tone; dock-flatline with floating-point denormals).
      Rationale: locks the false-positive boundary.

- [ ] Task 6.3. Manual reproduction checklist in
      `docs/dev/audio-fallback-qa.md` (six-line file, **not** a
      new design doc): plug dock → start daemon → hotkey →
      observe toast + `auto_input_device` persisted → hotkey
      again → success without re-toast. Rationale: once-per-
      release manual gate; cpal device hotplug isn't reachable
      from CI.

- [ ] Task 6.4. Run `tests/check.sh` full matrix; ensure slim
      cloud-only and `interactive`-feature builds both compile
      with the new `device` module. Rationale: standing
      requirement from `AGENTS.md`.

## Verification Criteria

- With a dock plugged in (silent capture endpoint as system
  default), running `fono` → press hotkey → speak: a single
  toast announces the auto-switch, `[audio].auto_input_device`
  is written to config, and the **next** hotkey press produces
  a working dictation with no further toasts.
- With **only** a dock plugged in (no working mic anywhere on
  the system), the failure mode is a single explicit toast
  pointing the user at the tray Microphone submenu — no
  hallucinated transcripts, no `pavucontrol` excursion.
- With `[audio].input_device = "<dock-name>"` set explicitly, the
  daemon never auto-overrides; the toast says "Microphone X
  captured no audio …" and STT is skipped. `auto_input_device`
  remains empty.
- `fono doctor` "Audio input" row reports the active device,
  resolution source, and last peak amplitude.
- Tray "Microphone" submenu lists all enumerated input devices
  with the active one ticked; clicking another swaps without a
  daemon restart and persists to `[audio].input_device`.
- `tests/check.sh` passes on full / slim / `--features
  interactive` matrices.
- All new Rust files start with the
  `// SPDX-License-Identifier: GPL-3.0-only` header per
  `AGENTS.md`.

## Potential Risks and Mitigations

1. **Silent-capture detector firing on legitimately quiet
   recordings (whispered speech in a quiet room).**
   Mitigation: peak-amplitude threshold set near the noise floor
   of any real microphone (≈ `1e-4`); paired with a
   minimum-duration gate so transient short captures don't
   trigger a costly device walk; whispered speech still produces
   peaks > `1e-3` from preamp noise alone.
2. **`pactl` / `wpctl` absent on minimal Linux installs (slim
   NimbleX SlackBuild).** Mitigation: `device::os_default_source`
   degrades cleanly to `None`; resolver tier 4 (`cpal::default_input_device`)
   takes over; the post-capture detector still saves the user.
3. **Probe-driven device walk costs 1–2 s on multi-device hosts.**
   Mitigation: total wall-time hard cap, parallel probes via
   `tokio::task::spawn_blocking` with a `JoinSet`, abort the
   walk as soon as one candidate exceeds `SIGNAL_THRESHOLD` so
   typical hosts pay <300 ms.
4. **macOS / Windows differ from Linux on default-source
   detection.** Mitigation: ship the cross-platform behaviour as
   resolver tier 4 (cpal default) + post-capture detector first;
   tier-3 OS-default plumbing is platform-feature-gated and can
   land later without changing observable behaviour.
5. **User had previously persisted a working
   `auto_input_device` and that device is now unplugged.**
   Mitigation: resolver tier 2 falls through silently to tier 3
   when the persisted name no longer exists in
   `host.input_devices()`. Re-probe runs and rewrites
   `auto_input_device` on the next captured silence.
6. **Tray submenu over-promises on systems where a device exists
   but cpal can't open it (already in exclusive use by another
   app).** Mitigation: probe failures decay to `Capture / error`
   in the submenu label so the user sees why a click won't help.

## Alternative Approaches

1. **Static "always pick first non-monitor input" without a
   silence detector.** Trade-off: simple, but stomps the user's
   OS choice in cases where the OS *was* right (e.g. user actually
   wanted the dock line-in). Rejected.
2. **Native PipeWire client replacing cpal on Linux.** Trade-off:
   gives proper hotplug, role-based source selection, and lower
   latency, but is a multi-week refactor and a new system-dep
   chain. Defer; the proposed plan composes with a future PW
   migration.
3. **Ship a 1 kHz "speak now" tone at capture start and require
   the user to enable a "test the mic" mode in the wizard.**
   Trade-off: friction the design explicitly aims to eliminate.
   Rejected; the wizard probe (Task 5.3) is silent and one-shot.
4. **Treat the silent capture as a successful empty transcript
   (status quo).** Trade-off: zero engineering cost, breaks
   "Fono just works" the moment a dock is plugged in. Rejected.
