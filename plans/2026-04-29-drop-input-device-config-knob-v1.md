# Drop `[audio].input_device` as a user-facing knob

## Objective

Stop surfacing `[audio].input_device` anywhere in Fono's UX —
tray submenu, CLI, wizard, `fono doctor` highlight, recovery
hint — and stop writing to it from any code path. The field
stays defined in the config schema as an undocumented, deprecated
escape hatch so existing user configs continue to load and so
the rare "I want Fono on a different mic from system default
without using `pavucontrol`" case is still recoverable by
hand-editing TOML.

This supersedes plan
`plans/2026-04-29-pulseaudio-first-microphone-enumeration-v1.md`.
That plan still describes the Pulse-first enumeration and
selection mechanism (its Tasks 1, 3, 4-on-Pulse, 6, 8, 9, 10);
this plan replaces its Tasks 2, 4-on-cpal, and 5 with "remove,
don't refactor" and adds the deprecation handling.

The motivating insight: the dock-silent-capture bug that started
this thread happened *because* cpal's "default" device tracked
an OS-elected silent endpoint. An explicit override didn't help
the user — they had no reason to set one before things broke.
Delegating microphone selection to the OS layer (PulseAudio
default-source on Linux desktops, System Settings on macOS,
Sound control panel on Windows) is more honest, requires fewer
moving parts, and means tools the user already knows
(`pavucontrol`, GNOME / KDE settings, etc.) are the canonical
place to set per-app or system-wide microphone choice.

## Scope: what stays vs. what goes

### Stays (deprecated escape hatch)

- `[audio].input_device: String` field in
  `crates/fono-core/src/config.rs`. Default `""`. Documented in
  the field comment as **deprecated**, kept only so existing
  user configs and a power-user TOML edit continue to work. No
  validation, no migration prompt — serde silently parses.
- The two cpal-resolving branches in
  `crates/fono-audio/src/capture.rs:118-129` and `:242-253`.
  Unchanged in behaviour: empty → `default_input_device()`,
  non-empty → cpal name lookup. They survive solely so a
  hand-edited TOML override still works.
- The capture-config plumbing in `cli.rs:623`, `cli.rs:1477`,
  `session.rs:277`, `session.rs:781` — copies the field value
  through. No code change needed; it carries an empty string in
  the common case.

### Goes (every UI surface)

- The tray "Microphone" submenu's *cpal* branch. The submenu
  itself stays, but only on Pulse / PipeWire systems where it
  delegates to `pactl set-default-source`. On non-Pulse hosts
  the submenu is hidden entirely.
- `fono use input <name>` CLI arm. Replaced (or repurposed,
  see Task 2) for the Pulse path; removed for cpal.
- Wizard microphone picker — removed wholesale.
- Doctor matrix override-aware highlight ("configured device
  not currently visible") — removed; the matrix becomes pure
  informational.
- Recovery hook's `fono use input "<name>"` hint — replaced
  with OS-aware advice (tray submenu on Pulse, "configure your
  system's default microphone" elsewhere with a pointer to
  `pavucontrol` / System Settings / Sound control panel).
- Daemon's `switch_input_device_via_tray` /
  `clear_input_device_via_tray` config-rewrite logic —
  replaced with `pactl` calls; the cpal branch is removed.

## Implementation Plan

- [ ] Task 1. **Mark the field deprecated in
  `crates/fono-core/src/config.rs:190`** with a doc comment:
  ```text
  /// **Deprecated.** Fono follows the OS default microphone
  /// (PulseAudio default-source on Linux, System Settings on
  /// macOS, Sound control panel on Windows). Use the tray
  /// "Microphone" submenu, `pavucontrol`, or your OS audio
  /// settings to change which device Fono captures from. This
  /// field is honoured for backward compatibility but is not
  /// surfaced in any UI; do not set it in new configs.
  ```
  No serde attribute change. No struct change. Default stays
  `""`. Existing configs with a value continue to work; the
  cpal branch in `capture.rs` is unchanged.

- [ ] Task 2. **Remove `fono use input` from the CLI**
  (`crates/fono/src/cli.rs:289-297` `UseCmd::Input` variant
  and its handler at `:961-998`). Print a one-line note
  pointing the user at the tray submenu or
  `pavucontrol` / System Settings if the subcommand is
  invoked from a Fono ≤ 0.3.5 muscle-memory. Implementation:
  keep the `Input` enum variant briefly, replace the handler
  with a friendly redirect printed to stderr, exit code 0.
  Remove entirely on the next minor release.

  Rationale: the CLI is the wrong surface for "change which
  microphone the OS uses." Tray submenu and OS UIs do this
  better, and removing the CLI eliminates the
  `[audio].input_device` write site that was responsible for
  half the override-related complexity.

- [ ] Task 3. **Remove the wizard microphone picker.**
  Delete `pick_input_device_if_needed` at
  `crates/fono/src/wizard.rs:1224-1254` and its call site at
  `:88-94`. The wizard already trusts the OS for every other
  audio decision; making the microphone the one exception was
  a v2 Phase 3 mistake. New users get the OS default; if they
  want a different mic they use the tray submenu after
  install.

- [ ] Task 4. **Simplify `fono doctor`'s "Audio inputs:"
  matrix** at `crates/fono/src/doctor.rs:194-241`. Drop the
  `configured` lookup; drop the "configured device not
  currently visible" warning; drop the "Auto" pseudo-row; drop
  the `fono use input` advice line at the bottom. The matrix
  becomes a flat list of every device with one row marked
  active (sourced from Pulse default-source on Pulse hosts,
  cpal default elsewhere). The active marker now reflects
  what *the system* says is default, not what the config
  override says.

- [ ] Task 5. **Reword the recovery hook body** at
  `crates/fono/src/audio_recovery.rs::build_body`. Drop the
  `fono use input "<name>"` hint. New body shape, OS-aware:
  - **0 alternatives**: unchanged ("check that the device
    isn't muted or unplugged").
  - **1 alternative**: "Switch to '<name>' via the tray icon's
    Microphone submenu, or use `pavucontrol` / your OS sound
    settings."
  - **2+ alternatives**: "Choose a different microphone via
    the tray icon's Microphone submenu, or open `pavucontrol`
    / your OS sound settings. Available: …"
  Update the four existing tests in
  `audio_recovery::tests::body_*` to match the new strings.
  Remove the two callers' habit of passing the
  `input_device` value to `notify_empty_capture` — since
  there's no longer an override surfaced to the user, the
  `current_device` parameter becomes "what cpal /
  Pulse-default is currently capturing from", which the
  helper can resolve itself by reading Pulse's
  `get-default-source` (Linux) or cpal's `default_input_device()`
  (elsewhere). One fewer parameter threaded through
  `session.rs`.

- [ ] Task 6. **Replace the daemon's switch helpers** at
  `crates/fono/src/daemon.rs:1148-1224`
  (`switch_input_device_via_tray` /
  `clear_input_device_via_tray`). Both become Pulse-only:
  - `switch_input_device_via_tray` calls
    `fono_audio::pulse::set_default_pulse_source(&pa_name)`,
    sends `Request::Reload` (so cpal's stream is
    re-established on the new default), and toasts a critical
    error if either fails. No config write.
  - `clear_input_device_via_tray` is removed entirely. There's
    no override to clear; "Auto" in the submenu is
    informational, marking whichever PA source is currently
    default.
  Adjust the tray's `TrayAction::ClearInputDevice` so it's
  either removed from the enum or stubbed to a no-op (depending
  on whether removing it from the public `TrayAction` enum is
  allowed under our compatibility story — internal type, so
  removal is fine).

- [ ] Task 7. **Hide the tray "Microphone" submenu on
  non-Pulse hosts.** In the daemon's tray construction at
  `crates/fono/src/daemon.rs` (around `:315-343`), gate the
  `MicrophonesProvider` registration on
  `fono_audio::mute::detect()` returning `PulseAudio` or
  `PipeWire`. On `Unknown`, register a no-op provider that
  yields `(vec![], 0)` and have the tray builder collapse to
  no submenu when the device list is empty (or add a tray-side
  flag that suppresses the submenu when the provider's first
  call returns empty).

  Rationale: on macOS / Windows / pure-ALSA the tray
  microphone submenu would have nothing useful to do — the OS
  already owns the default-mic UI, and clicking a row in
  Fono's tray could only confuse by suggesting Fono can
  override it. Better to hide than to surface a dead surface.

- [ ] Task 8. **CHANGELOG + status.md.**
  - `## [Unreleased] / ### Changed`: "Microphone selection is
    now fully delegated to the operating system — Fono follows
    the PulseAudio / PipeWire default-source on Linux, the
    macOS Sound input device, and the Windows recording
    default. The tray "Microphone" submenu (Linux only) calls
    `pactl set-default-source` so the change applies system-
    wide and is reflected in `pavucontrol` / GNOME / KDE
    settings."
  - `## [Unreleased] / ### Removed`: "`fono use input <name>`
    CLI subcommand. Use the tray "Microphone" submenu,
    `pavucontrol`, or your OS audio settings instead. The
    first-run wizard no longer asks for a microphone."
  - `## [Unreleased] / ### Deprecated`: "`[audio].input_device`
    config field. Existing values are still honoured (so
    upgrading is non-breaking), but the field is no longer
    surfaced anywhere in the UI and will be removed in the
    next major release. Use the tray submenu or your OS
    settings to change which microphone Fono captures from."
  - `docs/status.md` 2026-04-29 entry appended explaining
    the OS-delegation pivot and pointing to this plan.
  - `docs/decisions/` — add a short ADR
    `00NN-os-default-microphone.md` recording the decision to
    delegate microphone selection to the OS layer rather than
    re-implement it in Fono's config. Cite the dock-silent-
    capture bug as the motivating example.

- [ ] Task 9. **Verification.**
  - Existing user with `[audio].input_device = "USB Headset"`
    in their config: daemon starts, capture opens that
    device, no warning, no migration prompt. Backward compat
    intact.
  - New user on a Pulse host: tray submenu shows real source
    descriptions, clicking changes the system default,
    `pavucontrol` reflects the change, no
    `[audio].input_device` is ever written.
  - New user on a non-Pulse host (macOS, Windows, pure-ALSA):
    no tray "Microphone" submenu. `fono doctor` shows the
    device matrix as informational. Recovery hook (if it ever
    fires) points at OS settings, not at `fono use input`.
  - `fono use input <anything>` prints the deprecation
    redirect and exits 0.
  - `./tests/check.sh` (full matrix) green.

## Verification Criteria

- No code path *writes* to `cfg.audio.input_device` after this
  plan lands. (Quick proof: `rg 'audio\.input_device\s*=|input_device\s*='`
  inside `crates/` returns only the schema default at
  `config.rs:206` and reads.)
- `crates/fono-audio/src/capture.rs` still respects a
  non-empty `input_device` field for backward compat.
- `fono use input <name>` either prints a redirect (interim)
  or fails with `unknown subcommand` (final).
- `fono doctor` matrix renders without referencing the
  config-set device or the `fono use input` CLI.
- Recovery hook body matches the new four-string set; tests
  pass.
- The tray "Microphone" submenu does not appear in builds
  running on `AudioStack::Unknown` hosts.

## Potential Risks and Mitigations

1. **A user has scripts that call `fono use input` from
   a setup automation.**
   Mitigation: Task 2 keeps the subcommand briefly with a
   redirect message and exit code 0, so scripts don't hard-
   fail. Only the next minor release removes it. CHANGELOG
   `### Deprecated` entry telegraphs the removal a release
   in advance.

2. **A user *deliberately* set `[audio].input_device` because
   their OS default keeps being wrong** (e.g. headset
   reconnect storms on a buggy laptop where Pulse repeatedly
   re-elects the wrong default).
   Mitigation: the field still works exactly as before — they
   just don't see it in any UI. The deprecation notice
   acknowledges this case and points at `pavucontrol`'s
   per-app routing as the supported alternative.

3. **`pactl set-default-source` succeeds but cpal's stream
   doesn't pick up the new default until the next stream open.**
   Mitigation: the daemon's `Request::Reload` handler already
   tears down and rebuilds the audio stack, so the new
   default is picked up on next dictation. No additional
   plumbing needed beyond the existing Reload path.

4. **`pactl` failure during a tray click leaves the user with
   no audible feedback.**
   Mitigation: the existing `switch_input_device_via_tray`
   pattern already toasts a critical notification on the
   reload error path; extend it to also toast on
   `set-default-source` failure with the underlying message.

5. **macOS / Windows users discover their OS default is wrong
   and there's no in-Fono way to fix it.**
   Mitigation: this is correct delegation — fixing the OS
   default in System Settings / Sound control panel is the
   right place. `fono doctor` includes a one-line pointer to
   each OS's settings UI so the user knows where to go.

6. **Removing `fono use input` is a breaking CLI change
   between 0.3.x and 0.4.x.**
   Mitigation: SemVer-disclose in CHANGELOG `### Removed`
   when the redirect-only stub graduates to a hard removal.
   Internal release notes mention it; external release blog
   highlights it as a simplification, not a regression.

## Alternative Approaches

1. **Remove the field entirely with a config schema bump.**
   Cleanest end-state, breaks any user who manually set the
   field. Rejected: zero benefit over keeping it as a hidden
   escape hatch, since the cpal branch in `capture.rs` is
   five lines and a clear cost-of-keeping. Revisit at the
   next major version when other breaking changes accumulate.

2. **Keep the field surfaced everywhere but mark it
   "advanced" in the wizard / doctor.**
   Rejected: the dock bug came from a user who had every
   reason *not* to set the override and still got a silent
   capture. Surfacing it doesn't help; it just adds another
   thing to ignore. The OS-layer UIs are the right home for
   this question.

3. **Remove only the wizard picker and CLI; keep the tray
   submenu writing `[audio].input_device` on cpal hosts.**
   Half-measure. Rejected: the hybrid leaves the cpal-name
   override path as a live UI surface, which means we still
   ship the dsnoop-error noise and the cpal-name complexity.
   Pulse-only on Linux is the cleaner cut.

4. **Add a per-app-routing UI to Fono itself (custom
   `pavucontrol`).**
   Rejected: massive scope, duplicates a tool the user
   already has. Out of scope for a "Fono should just work"
   simplification.
