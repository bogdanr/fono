# Empty-transcript microphone recovery (dock with capture-but-no-mic)

## Objective

When a recording produces an empty transcript despite the user clearly
having held the hotkey for a meaningful duration — typical cause: an
external dock advertises a silent "capture" endpoint that the OS picks
as the default source — Fono should tell the user **what happened**
and **how to fix it in one click**, instead of dropping a quiet
`WARN STT returned empty text — nothing to inject` log line that the
user never sees.

Goal: zero-friction recovery, leaning on the *empty-STT-result* signal
that the pipeline already produces. No new audio-quality heuristic, no
silent-capture detector, no resolver tier rewrite — three thin layers
on top of the existing warn sites:

1. Notify the user when an empty-transcript happened on a long-enough
   recording (≥ 5 s) so it can't be a hotkey misfire.
2. If more than one input device exists, the notification offers a
   one-click switch to the most likely alternative (and persists it).
3. Tray "Microphone" submenu + wizard probe + `fono doctor` row so
   the user can manually pick / verify the input device.

User overrides remain sacred: when `[audio].input_device` is set
explicitly, the notification still fires but the auto-switch offer is
replaced with "open the tray Microphone submenu to switch".

## Initial assessment

### Existing signals to lean on

The pipeline already classifies a successful-but-silent capture in two
identical-shape places:

- `crates/fono/src/session.rs:1240-1244` — `if raw.is_empty()` after
  STT returns OK, emits `warn!("STT returned empty text — nothing to
  inject")` and returns `PipelineOutcome::EmptyOrTooShort`.
- `crates/fono/src/session.rs:698-700` — `PipelineOutcome::EmptyOrTooShort`
  arm in the post-pipeline match, currently logs a `warn!` and exits.

Both paths already carry the `capture_ms` we need for the 5-second
gate. The notify-rust toast plumbing is already wired (used after
inject for the "Fono — text copied to clipboard" toast pattern in
`session.rs`), and the tray submenu/refresh pattern is established by
the STT/LLM/Languages submenus at `crates/fono-tray/src/lib.rs`.

`crates/fono-audio/src/capture.rs` already has both `start` (batch)
and `start_with_forwarder` (live) paths that resolve devices via
`host.input_devices()` — listing alternatives is a single call.

### What we deliberately do **not** build (vs the v1 plan)

- No `RecordingBuffer::signal_verdict` peak/energy counters. The
  empty-transcript signal already covers the dock case at zero
  added complexity in the audio crate.
- No four-tier resolver / `pactl` shelling for OS-default-source
  detection. Out of scope; the user-action approach handles dock
  swaps correctly without it.
- No daemon-startup pre-flight probe. The user is going to press
  the hotkey within seconds of starting Fono anyway; the first
  empty-STT result triggers the same recovery path the pre-flight
  would, with no startup cost.
- No `auto_input_device` separate config field. Auto-switch writes
  directly to `[audio].input_device` (after notifying the user), so
  there is exactly one knob to read / clear.

### Risk priority

1. **Notification noise on legitimate empty captures.** User pressed
   the hotkey by accident, or the recording was genuinely all
   silence/background. Mitigation: 5 000 ms minimum-duration gate
   (per the user's spec); empty captures shorter than that fall
   through silently as today.
2. **Auto-switch picking the wrong alternative on multi-mic hosts.**
   Mitigation: the notification *offers* the switch (action button)
   rather than performing it unprompted; user confirms with one
   click. The default action is "open the Microphone submenu" when
   ranking is ambiguous (3+ candidates).
3. **`notify-rust` action buttons not supported on every desktop.**
   Notification-spec action buttons work on GNOME / KDE / dunst /
   Mako; on macOS and on Windows they're either limited or absent.
   Mitigation: when actions aren't available (or a click times out
   without selection), the notification text always names the tray
   Microphone submenu as the recourse, so it degrades gracefully.

## Implementation Plan

### Phase 1 — Notify on empty transcripts past the 5 s gate

- [ ] Task 1.1. In `crates/fono-core/src/config.rs`, add
      `notify_on_empty_capture_ms: u64` (default `5000`) under
      `[general]`. `0` disables the feature; values < 1000 are
      clamped at startup with a one-shot `warn!`. Rationale:
      single user-tunable knob; default matches the spec.

- [ ] Task 1.2. In `crates/fono/src/session.rs`, after the
      existing `warn!("STT returned empty text — nothing to inject")`
      at `:1240-1244`, branch on
      `capture_ms >= config.general.notify_on_empty_capture_ms`.
      On true, call a new
      `handle_empty_capture(&config, capture_ms, &available_inputs)`
      helper (defined in a new `crates/fono/src/audio_recovery.rs`
      module) before returning the existing `EmptyOrTooShort`
      outcome. Rationale: keeps `run_pipeline` linear; isolates the
      new code so it's testable on its own.

- [ ] Task 1.3. Mirror the call at the *outer* match arm
      `PipelineOutcome::EmptyOrTooShort` in
      `crates/fono/src/session.rs:698-700` and at the live path
      empty-transcript site in `crates/fono/src/session.rs:1008-1009`.
      Single call site each — emits exactly one notification per
      capture (use a per-pipeline `AtomicBool` if needed to dedupe
      the inner+outer overlap). Rationale: covers batch *and* live
      dictation with one helper.

- [ ] Task 1.4. `handle_empty_capture` enumerates devices via
      `cpal::default_host().input_devices()` and dispatches:
      - **Zero alternative inputs** → notification text:
        *"Fono — no audio captured (5.2 s recording, 0 chars). The
        configured microphone produced no signal. Open the tray
        Microphone submenu to verify."*
      - **One alternative input** → notification text plus an
        action button *"Switch to <name>"* that, when clicked,
        atomically rewrites `[audio].input_device`, sends
        `Request::Reload`, and emits a confirmation toast.
      - **Two or more alternatives** → notification text plus
        action button *"Choose microphone…"* that opens the tray
        Microphone submenu (or, if the tray isn't running, prints
        the candidate list to the daemon log and points at
        `fono use input <name>`).
      Rationale: matches the user's spec (auto-switch only when
      unambiguous; otherwise hand off to the manual surface).

- [ ] Task 1.5. When `config.audio.input_device` is non-empty
      (user override active), the notification's action becomes
      *"Open Microphone submenu"* unconditionally — never auto-
      rewrite a deliberate user override. Notification text adds
      one line: *"Currently set to '<name>' in config."*
      Rationale: respects explicit user choice (Risk 2 in the
      v1 plan).

### Phase 2 — Tray "Microphone" submenu

- [ ] Task 2.1. In `crates/fono-tray/src/lib.rs`, add a
      Microphone submenu mirroring the STT/LLM/Languages pattern:
      - First entry "Auto (system default)" — ticked when
        `config.audio.input_device.is_empty()`.
      - One entry per `host.input_devices()` — ticked when the
        name matches the active config.
      - "Refresh device list" trailing entry that re-enumerates
        on click (cpal does not push hotplug events, so an
        explicit refresh is more honest than a polling task).
      New `TrayAction::SetInputDevice(String)` and
      `TrayAction::ClearInputDevice` variants. Rationale: gives
      the user a one-click recourse identical to existing
      provider switching UX, and is the manual surface that the
      Phase 1 notification points at.

- [ ] Task 2.2. In `crates/fono/src/daemon.rs` tray dispatch
      table, handle the new variants by calling helpers that
      atomically rewrite `[audio].input_device` (or clear it) and
      send `Request::Reload`. Same path used for STT/LLM swaps.
      Rationale: zero-restart device switching matches the
      existing provider-switching guarantee.

- [ ] Task 2.3. Refresh the Microphone submenu on the same ~2 s
      tick that already drives the Recent transcriptions submenu
      so newly plugged devices appear without a click. Cap at one
      cpal enumeration per tick (cheap; ALSA enumeration is sub-
      ms on typical systems). Rationale: keeps the manual recovery
      UX honest when the user *does* plug a device in mid-session.

### Phase 3 — Wizard probe + doctor row + CLI

- [ ] Task 3.1. In `crates/fono/src/wizard.rs`, after the
      hardware-tier section, list available input devices and
      offer them as a checklist with the OS default pre-selected.
      Skipping persists nothing (current behaviour). Rationale:
      user picking a working device at install time means the
      Phase 1 notification path never has to fire on first run.

- [ ] Task 3.2. In `crates/fono/src/doctor.rs`, add an "Audio
      input" row showing: configured device or "(auto: <name>)",
      total device count, and a flag if no input devices exist.
      Rationale: surfaces the failure mode in the diagnostic
      surface that exists for exactly this purpose.

- [ ] Task 3.3. Add `fono use input <name|auto>` to the
      `fono use` subtree in `crates/fono/src/cli.rs`. Rationale:
      same affordance as `fono use stt`, completes the symmetry,
      and gives terminal-only users a fix path without the tray.

- [ ] Task 3.4. New "No audio captured" section in
      `docs/troubleshooting.md` describing the notification, the
      tray submenu, the `fono use input` CLI, and how to clear
      the override (`fono use input auto`). Rationale: discovery
      for users who hit the failure mode before learning about
      the tray.

### Phase 4 — Verification

- [ ] Task 4.1. Unit tests in `audio_recovery.rs` covering the
      three branches of `handle_empty_capture` (zero / one /
      many alternatives) with an injected device-list closure
      and an injected notify closure. Assert the notification
      action wiring without spawning a real notification daemon.
      Rationale: pins the user-spec behaviour; test must not
      require a graphical session.

- [ ] Task 4.2. Integration test extension in
      `crates/fono/tests/pipeline.rs`: drive the existing
      `pipeline_skips_history_when_stt_returns_empty` fixture
      with a 6-second `capture_ms` and assert the recovery hook
      was called exactly once. Rationale: locks the 5 000 ms
      gate against future regression.

- [ ] Task 4.3. Run `tests/check.sh` full + slim +
      `--features interactive` matrices. Rationale: standing
      `AGENTS.md` requirement.

- [ ] Task 4.4. Manual reproduction line in
      `docs/dev/audio-fallback-qa.md` (3 lines): plug dock →
      hotkey for 6 s → observe notification → click switch →
      next dictation succeeds. Rationale: cpal hotplug isn't
      reachable from CI; once-per-release manual gate.

## Verification Criteria

- A 6-second hold-to-talk against a dock that produces silence
  fires exactly one desktop notification stating duration and
  "no audio captured". A 2-second accidental press does **not**
  fire a notification.
- On a multi-input host with one obvious alternative, clicking
  the notification's action button rewrites
  `[audio].input_device`, the next dictation works, and no
  daemon restart is required.
- With `[audio].input_device` set explicitly, the notification
  still fires but the action only opens the tray Microphone
  submenu — config is never auto-overwritten.
- Tray Microphone submenu lists every cpal input device with the
  active one ticked; "Auto" is ticked when override is empty;
  switching either entry persists and reloads atomically.
- `fono doctor` "Audio input" row shows the active configured /
  resolved device.
- `fono use input <name>` and `fono use input auto` accept and
  persist the override (or clear it) and trigger Reload.
- All new Rust files start with the GPL-3.0-only SPDX header.
- `tests/check.sh` matrix is green.

## Potential Risks and Mitigations

1. **Notification fatigue when user records 5 s of pure silence
   on purpose** (e.g. testing the hotkey without speaking).
   Mitigation: the 5 s gate is tunable
   (`general.notify_on_empty_capture_ms`); set to `0` to
   disable. Documented in troubleshooting.
2. **`notify-rust` action buttons absent on macOS / Windows.**
   Mitigation: notification text always names the tray
   submenu as the fallback recourse; the action button is
   optional UX, not the only path.
3. **`cpal::input_devices()` reordering between enumerations.**
   Mitigation: match by name, not by index, throughout
   (`SetInputDevice(String)`).
4. **Auto-switch picks a "Monitor of …" loopback as the only
   alternative.** Mitigation: filter device names containing
   `Monitor`, `Loopback`, `HDMI`, `S/PDIF` from the auto-switch
   single-candidate path; they still appear in the manual tray
   submenu (Task 2.1), just not in the auto-offer.
5. **Live-mode and batch-mode firing two notifications for the
   same capture** when the inner + outer empty arms both run.
   Mitigation: dedupe via a `AtomicBool` carried on the pipeline
   metrics struct; first hit wins, second hit is a no-op.

## Alternative Approaches

1. **Detect silence by RMS in the audio crate** (the v1 plan
   approach). Trade-off: solves dock case before STT runs but
   adds peak/energy counters, false-positive tuning, and a
   parallel signal path. Rejected — the empty-STT signal already
   exists and matches the user's spec.
2. **Auto-switch unconditionally when one alternative exists,
   no notification.** Trade-off: zero-click recovery but
   confusing when the user *wanted* the dock for line-in
   capture. Rejected — the notification + one-click action is
   the user's stated preference.
3. **Pre-flight at daemon start.** Trade-off: catches the case
   one dictation earlier but burns 200–800 ms at every start
   for a failure mode most users will never hit. Deferred; not
   needed once Phase 1 is live.
