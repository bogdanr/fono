# PulseAudio-first microphone enumeration

## Objective

Replace cpal's noisy ALSA enumeration as the primary microphone
listing surface with a `pactl`-based query whenever PulseAudio or
PipeWire (with the Pulse compat layer) is available — i.e. on
essentially every modern Linux desktop. The existing
`AudioStack::detect()` at `crates/fono-audio/src/mute.rs:17-33`
already classifies the host; we extend the same shell-out pattern
to ask Pulse "what input sources do you actually have?" and use
its answer instead of fighting cpal's ALSA host.

This single-tool delegation supersedes the more invasive
blocklist + cache plan
(`plans/2026-04-29-alsa-plugin-filter-and-cache-v1.md`) — keeping
that one only as a pure-ALSA fallback for the rare systems
without Pulse / PipeWire.

## Why this is simpler than the blocklist approach

- One source of truth: `pactl list sources` returns only what the
  user actually has, with friendly `Description:` labels — no
  curated blocklist of plugin pseudo-device names to maintain.
- Selection delegates to the OS: clicking a row runs
  `pactl set-default-source <name>`, which changes the system
  default for *every* application, not just Fono. Matches what
  `pavucontrol` / GNOME / KDE settings already do.
- `[audio].input_device` stays empty (Auto) on Pulse systems —
  cpal's `default_input_device()` then resolves to Pulse's
  default source automatically, and the user's choice survives
  reboots because Pulse persists its default-source.
- macOS / Windows are unaffected: `AudioStack::detect()` returns
  `Unknown`, the cpal path stays in place, and Core Audio /
  MMDevice device names have always been clean there.
- The `snd_pcm_dsnoop_open` errors disappear as a side effect:
  we never call cpal's ALSA enumeration on Pulse systems.

## Implementation Plan

- [ ] Task 1. **Create `crates/fono-audio/src/pulse.rs`** with
  thin shell-out helpers, mirroring the style of `mute.rs`:
  - `fn list_pulse_sources() -> Vec<InputDevice>`. Parses
    `pactl list sources short` for `<id>\t<name>\t<driver>\t…`
    rows, drops any name ending in `.monitor` (these are
    loopback monitors of sinks, never microphones), and uses
    the long-form `pactl list sources` output to map
    `name → description` so the submenu shows friendly labels
    like "Built-in Audio Analog Stereo" instead of the
    `alsa_input.usb-…` mangled identifier.
  - `fn pulse_default_source_name() -> Option<String>`. Wraps
    `pactl get-default-source` for the `is_default` flag.
  - `fn set_default_pulse_source(name: &str) -> anyhow::Result<()>`.
    Wraps `pactl set-default-source <name>`. On PipeWire the
    Pulse-compat layer also accepts this command; no separate
    `wpctl` branch needed.

  All three return `None` / empty / a clear `anyhow::Error` if
  `pactl` isn't on `PATH` or the spawn fails — never panic.

- [ ] Task 2. **Define a small `InputDevice` extension** in
  `devices.rs` so consumers can tell whether a row was sourced
  from Pulse (and therefore selectable via
  `set_default_pulse_source`) versus cpal (selectable via
  `[audio].input_device` rewrite):

  ```rust
  pub enum InputBackend {
      Pulse {
          /// PA source name, e.g. "alsa_input.usb-…". Pass this
          /// to `set_default_pulse_source`.
          pa_name: String,
      },
      Cpal {
          /// cpal device name, written verbatim into
          /// `[audio].input_device`.
          cpal_name: String,
      },
  }
  pub struct InputDevice {
      pub display_name: String,  // friendly
      pub is_default: bool,
      pub backend: InputBackend,
  }
  ```

  Migrate the existing `name: String` field readers (recovery
  hook, daemon's `MicrophonesProvider`, doctor matrix, wizard,
  `fono use input` CLI) to use `display_name`. The two existing
  call sites that hand a string to cpal
  (`crates/fono-audio/src/capture.rs:118-130` and `:242-254`)
  are unchanged — they still consume `[audio].input_device`
  verbatim.

- [ ] Task 3. **Rewire `list_input_devices` in
  `crates/fono-audio/src/devices.rs`** with a single
  detect-then-dispatch:
  ```text
  match fono_audio::mute::detect() {
      PulseAudio | PipeWire => list_pulse_sources(),
      Unknown               => enumerate_cpal_inputs(),
  }
  ```
  Drop the ALSA plugin blocklist entirely — it's only relevant
  when cpal is the enumeration source, which on modern Linux
  it no longer is. Keep `enumerate_cpal_inputs` as the
  pre-existing logic for the `Unknown` branch (pure-ALSA
  systems, macOS, Windows).

  Caching stays optional. `pactl` invocation is ~5–15 ms; the
  tray polls every ~2 s, so even uncached the cost is
  negligible. Drop the 10 s cache from the previous plan to
  keep the code minimal — hot-plug detection becomes
  effectively instantaneous.

- [ ] Task 4. **Update the daemon's tray dispatch** at
  `crates/fono/src/daemon.rs` so `TrayAction::SetInputDevice`
  branches on the backend:
  - `InputBackend::Pulse { pa_name }`: call
    `fono_audio::pulse::set_default_pulse_source(&pa_name)`,
    then `Request::Reload`. Do **not** rewrite
    `[audio].input_device` — it stays empty (Auto), and cpal's
    next stream open lands on the new Pulse default.
  - `InputBackend::Cpal { cpal_name }`: existing behaviour —
    write `cpal_name` into `[audio].input_device`, save, reload.

  `TrayAction::ClearInputDevice` semantics differ by backend:
  on Pulse it's a no-op visible to the user (the tray "Auto"
  row is just informational — Pulse always has *some* default
  source); on cpal systems it clears the override as today.

- [ ] Task 5. **Update `fono use input <name>`** in
  `crates/fono/src/cli.rs` to accept both display names and PA
  source names: case-insensitive match against
  `display_name` first (so users can paste from `fono doctor`),
  PA `name` second (for users copying from `pactl list short`).
  Resolution to a backend dispatches to the same pulse vs cpal
  branch as the tray. `auto` continues to clear
  `[audio].input_device`.

- [ ] Task 6. **Surface the backend in `fono doctor`**:
  the existing "Audio inputs:" matrix (added in plan v2 Phase 3
  at `crates/fono/src/doctor.rs:194-241`) gains a one-line
  header naming the active stack —
  `Detected: PipeWire (via pactl)` or
  `Detected: PulseAudio (via pactl)` or
  `Detected: cpal (no Pulse/PipeWire)`. The per-row format
  stays the same (`* Display Name`); the `*` flag now reflects
  Pulse's default source on Pulse systems and the
  `[audio].input_device` match on cpal systems.

- [ ] Task 7. **Recovery hook awareness** at
  `crates/fono/src/audio_recovery.rs`. The body composer
  already hints "fono use input \"<name>\"" / "open the tray
  Microphone submenu". On Pulse systems the same advice still
  works (since `fono use input` resolves PA names too), but
  the prose can also mention `pavucontrol` /
  `wpctl set-default <id>` as a system-level recourse. Single
  string change in `build_body`; no logic change.

- [ ] Task 8. **Tests.**
  - Unit-test the `pactl` parser in `pulse.rs` against a small
    fixtures table (5–6 representative `pactl list sources
    short` outputs covering: zero non-monitor sources, one mic,
    one mic + one monitor, two mics with one default, names
    with embedded whitespace/dots/hyphens, malformed line
    skipped).
  - Unit-test the description join: feed sample
    `pactl list sources` long-form output and assert the
    expected `display_name` for each parsed `name`.
  - Integration test: gated `#[cfg_attr(not(target_os = "linux"),
    ignore)]`; when `pactl` is on PATH at test time, assert
    `list_input_devices()` returns `≥0` sources without
    panicking and that none of them have `.monitor` in the PA
    name.
  - Keep all existing `audio_recovery::tests::body_*` tests
    untouched — they inject device lists directly to
    `build_body` and don't depend on the enumeration source.

- [ ] Task 9. **Docs + changelog.**
  - Module doc-comment in `pulse.rs` explaining the
    parse-and-delegate model and why we don't shell out to
    `wpctl` even on PipeWire (the Pulse compat layer accepts
    the same commands universally; one tool, one parser).
  - Update `crates/fono-audio/src/devices.rs` top-level doc to
    point to `pulse.rs` for the Linux desktop path and clarify
    that the cpal branch is only the macOS / Windows /
    pure-ALSA fallback.
  - `CHANGELOG.md` `## [Unreleased]` → `### Changed` entry:
    "On Linux desktops with PulseAudio or PipeWire (the Pulse
    compat layer), Fono now lists microphones via
    `pactl list sources` instead of cpal's ALSA enumeration.
    Submenu rows show the source's friendly description (e.g.
    "Built-in Audio Analog Stereo") instead of cpal's raw ALSA
    PCM names; clicking a row runs
    `pactl set-default-source` so the change applies
    system-wide and is remembered by Pulse across reboots.
    Eliminates the chronic `snd_pcm_dsnoop_open: unable to
    open slave` errors and the plugin pseudo-device clutter
    (`pulse`, `oss`, `speex`, `default`, `surround51`, …) that
    previously appeared in the submenu. Pure-ALSA, macOS and
    Windows hosts are unaffected."
  - `docs/status.md` 2026-04-29 entry appended with the
    PulseAudio-first delegation summary.

- [ ] Task 10. **Verification.**
  - `./tests/check.sh` (full matrix) green.
  - On the affected machine (laptop + dock with passive
    capture endpoint):
    - tray submenu shows real friendly source names
      ("Built-in Audio Analog Stereo", "Logitech USB
      Headset Mono", maybe "USB Composite Device Digital
      Stereo (IEC958)" for the dock if PA exposes it as a
      source — never `pulse`, `oss`, `speex`, `default`);
    - clicking a different row immediately changes the
      default source — verifiable by running `pactl
      get-default-source` after the click;
    - `pavucontrol` reflects the same default source change;
    - first dictation after the swap captures real audio
      (recovery hook silent because the `STT returned empty
      text` path no longer fires);
    - daemon stderr is clean — no `snd_pcm_dsnoop_open`
      lines.

## Verification Criteria

- `pulse::list_pulse_sources()` parses representative `pactl`
  output correctly (parser unit tests cover the six fixtures).
- `list_input_devices()` returns Pulse-sourced rows when
  `AudioStack::detect()` returns `PipeWire` or `PulseAudio`,
  and cpal-sourced rows otherwise.
- `TrayAction::SetInputDevice` on a `Pulse`-backed row mutates
  Pulse's default source via `pactl set-default-source` and
  leaves `[audio].input_device` untouched.
- `TrayAction::SetInputDevice` on a `Cpal`-backed row writes
  `[audio].input_device` (current behaviour, regression-free).
- `fono use input <pa_name>`, `fono use input "<description>"`
  and `fono use input <cpal_name>` all resolve correctly on
  their respective backends; `fono use input auto` clears the
  cpal override.
- `fono doctor` shows the active stack and friendly device
  names with one row marked active.
- The `snd_pcm_dsnoop_open` error is no longer printed during
  daemon startup or tray refresh on Pulse / PipeWire hosts.
- `pavucontrol`'s "Default Source" reflects whatever the user
  picked from Fono's tray submenu.

## Potential Risks and Mitigations

1. **`pactl` not on PATH but Pulse is running** (e.g. minimal
   distro images with Pulse but no client tools).
   Mitigation: the helper returns `None` on spawn failure;
   `list_input_devices` falls through to the cpal branch.
   `fono doctor` surfaces "Detected: cpal (no Pulse/PipeWire)"
   so the user knows why and can `apt install pulseaudio-utils`
   if they want the better experience.

2. **Pulse / PipeWire restart between enumeration and
   selection.** A user picks a row, Pulse restarts, the source
   name is gone before `set-default-source` runs.
   Mitigation: `pactl set-default-source` returns non-zero;
   the helper surfaces the error via `anyhow`; the daemon's
   `switch_input_device_via_tray` already toasts a critical
   notification on `o.reload().await` failure — extend it to
   toast on the `set-default-source` failure too with a "try
   again" hint.

3. **PA source names with embedded whitespace** — the short
   format is tab-separated, but some user-defined names can
   contain tabs in adversarial setups.
   Mitigation: parser splits on `\t` (not whitespace) and
   keeps the second field verbatim; the long-form
   `Description:` line is read with a single `: ` delimiter.
   Tested with the "names with embedded whitespace" fixture.

4. **User has set `[audio].input_device = "pulse"` (or
   `"hw:CARD=…"`) by hand** in the config and now the tray
   shows Pulse-source rows.
   Mitigation: `[audio].input_device` is a hard override —
   `crates/fono-audio/src/capture.rs:118-130` opens it
   verbatim regardless of what the tray surfaces. The "Auto"
   row in the submenu and `fono use input auto` are how the
   user releases the override; nothing about the new
   enumeration path forces it.

5. **PipeWire-native users prefer `wpctl`** to `pactl`.
   Mitigation: PipeWire ships with the Pulse compat layer
   enabled by default; `pactl` works on every PipeWire
   install, and one parser is half the maintenance of two.
   We can add a `wpctl` branch later if a real user reports a
   case where `pactl` doesn't work on their PipeWire system.

6. **`pactl set-default-source` is privileged on some hardened
   distros.**
   Mitigation: it isn't on any of the major desktops (Ubuntu,
   Fedora, Debian, Arch); the user-session bus owns the audio
   stack. If a user does hit a permission error, the toast
   surfacing the exit code points them at the cause, and they
   can edit `[audio].input_device` by hand as the cpal escape
   hatch.

## Alternative Approaches

1. **Use `wpctl` on PipeWire and `pactl` on PulseAudio.** Two
   parsers, two command surfaces, cleaner separation. Rejected
   because PipeWire's Pulse compat layer is universal in
   practice and the PA tooling is the smaller surface.

2. **Read PulseAudio's D-Bus interface directly.** Most
   accurate, no `pactl` shell-out. Rejected because the
   `dbus`/`zbus` dependency surface is large for a single
   feature, and `pactl` is already a dependency we lean on
   for `mute`. Keeps the binary slim.

3. **Keep the cpal blocklist + cache plan and *also* add the
   Pulse path.** Belt-and-braces. Rejected: the blocklist
   only matters when cpal's ALSA enumeration is what we show
   the user, and on Pulse / PipeWire systems it isn't. Adding
   both is dead code most of the time and a maintenance
   liability when ALSA plugins evolve.

4. **Show pactl sources but still write `[audio].input_device`
   to the cpal name** by mapping PA source → `hw:CARD=…`
   through a lookup. Rejected: the mapping is unstable
   (PA renames sources on suspend/resume), and it loses the
   "system-wide default" property that makes Pulse's own
   default-source mechanism the right home for this state.
