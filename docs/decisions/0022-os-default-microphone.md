# ADR 0022 — Delegate microphone selection to the OS

Status: Accepted (2026-04-29)

## Context

Up to v0.3.5 Fono kept an `[audio].input_device` config field that
named a specific cpal device. The tray submenu, the
`fono use input <name>` CLI, the first-run wizard, and the
`fono doctor` matrix all wrote to and read from this field; the
audio capture path opened that exact cpal name when set, and fell
through to `default_input_device()` when empty.

Two real-world issues against this design surfaced on the same day:

1. A user plugged into an external dock that advertised a passive
   capture endpoint with no microphone wired to it. The OS elected
   the dock as `@DEFAULT_SOURCE@`; cpal's
   `default_input_device()` followed the OS default; capture buffers
   were flat-lined zeros; STT returned empty. The user had no
   `[audio].input_device` set — and no reason to set one before
   things broke. The override mechanism didn't and couldn't help.
2. The tray "Microphone" submenu showed cpal's raw ALSA enumeration,
   which on a typical Linux desktop is full of plugin pseudo-devices
   (`pulse`, `oss`, `speex`, `default`, `surround51`, …). Several of
   them spew `snd_pcm_dsnoop_open: unable to open slave` to stderr
   when probed. The user's question was reasonable: *those aren't
   microphones, why am I seeing them?*

## Decision

Stop maintaining a microphone override. Microphone selection is
fully delegated to the OS layer:

- **Linux desktops with PulseAudio / PipeWire (Pulse compat):** the
  audio server's default-source is the source of truth. Fono's tray
  submenu enumerates `pactl list sources` and clicking a row runs
  `pactl set-default-source <name>`, mutating the system-wide
  default. The change is reflected in `pavucontrol`, GNOME / KDE
  settings, and every other Pulse client. cpal's
  `default_input_device()` then resolves to whatever Pulse reports
  as default at stream-open time.
- **macOS:** System Settings → Sound → Input owns the choice. cpal's
  Core Audio host respects it.
- **Windows:** Sound control panel / Settings → System → Sound owns
  the choice (with per-app routing since Windows 10 1803). cpal's
  MMDevice host respects it.
- **Pure-ALSA Linux (rare):** `~/.asoundrc`'s `pcm.default` is the
  knob. Out of Fono's scope.

The `[audio].input_device` field is removed from the schema with no
migration grace period (Fono has no released users yet); the tray
submenu is hidden on `AudioStack::Unknown` hosts (macOS, Windows,
pure-ALSA) where Fono can't sensibly mutate the OS default; and
`fono use input` and the wizard microphone picker are removed.

## Consequences

Positive:

- One place to set "which microphone" — the OS UI the user already
  knows. No Fono-specific knob to discover or maintain.
- Tray submenu shows real friendly names ("Built-in Audio Analog
  Stereo", "Logitech BRIO") sourced from Pulse's `Description:` —
  no ALSA plugin pseudo-device clutter, no `snd_pcm_dsnoop_open`
  errors.
- Hot-plug works natively. Pulse sees the dock arrive / depart and
  re-elects its default; Fono's next stream open lands on the new
  default with no extra plumbing.
- Per-app microphone routing is supported by the OS layer
  (`pavucontrol`'s Recording tab on Linux, per-app default-input on
  Windows ≥ 10 1803). Power users who want Fono on a different mic
  from system default have a working tool already.

Negative:

- Pure-ALSA Linux users (no Pulse, no PipeWire) keep the cpal
  enumeration which still surfaces some plugin entries. Acceptable
  trade-off: that combination is rare in 2026, and the recovery
  notification with its OS-settings hint is enough actionable
  signal.
- A user whose OS default is repeatedly wrong (e.g. headset
  reconnect storms on a buggy laptop) has no in-Fono workaround.
  Pulse's per-app routing in `pavucontrol` is the supported answer;
  if real reports accumulate, a future ADR can revisit per-app
  pinning at the Pulse-API level.

## Alternatives considered

1. **Keep the field as a hidden, deprecated escape hatch.**
   Rejected: zero benefit — the field served no use case the OS
   layer doesn't already cover, and Fono has no released users
   needing a migration path.
2. **Maintain a curated ALSA plugin blocklist + cache to clean up
   the cpal submenu** (plan
   `2026-04-29-alsa-plugin-filter-and-cache-v1.md`). Rejected:
   the blocklist is an ongoing maintenance burden, doesn't fix the
   "which microphone" delegation question, and adds complexity that
   PulseAudio's own enumeration sidesteps cleanly.
3. **Implement per-app microphone routing inside Fono** (a custom
   `pavucontrol`). Rejected: massive scope, duplicates a tool the
   user already has, and pulls Fono into territory orthogonal to
   speech-to-text.

## References

- `plans/2026-04-29-pulseaudio-first-microphone-enumeration-v1.md`
- `plans/2026-04-29-drop-input-device-config-knob-v1.md`
- `crates/fono-audio/src/pulse.rs` — the parse-and-delegate
  implementation.
- `crates/fono-audio/src/devices.rs` — backend dispatch.
