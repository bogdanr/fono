# ALSA plugin filter + enumeration cache

## Objective

Stop the tray "Microphone" submenu from listing ALSA plugin
pseudo-devices (`pulse`, `oss`, `speex`, `upmix`, `vdownmix`,
`default`, `surround51`, `iec958`, `hdmi`, …) as if they were
microphones, and silence the chronic `snd_pcm_dsnoop_open: unable
to open slave` errors that appear repeatedly because the tray
refresh loop re-enumerates cpal devices every ~2 s. Both symptoms
share one root cause — cpal's ALSA host enumerates **every** PCM
in `/etc/asound.conf` — and one file change in
`crates/fono-audio/src/devices.rs` addresses both.

## Implementation Plan

- [ ] Task 1. **Add an ALSA plugin blocklist filter to
  `crates/fono-audio/src/devices.rs`.**
  Introduce `pub fn is_alsa_plugin_name(name: &str) -> bool` with
  two tiers:
  1. *Speaker-channel mappings and digital outputs* (`front`,
     `rear`, `side`, `center_lfe`, `surround21/40/41/50/51/71`,
     `iec958`, `spdif`, `hdmi`) are blocked **even when** ALSA
     reports them with a `:CARD=…` binding — they are never
     microphones regardless of the config quirk that exposed
     them as inputs.
  2. *Plugin pseudo-devices* (`default`, `sysdefault`, `pulse`,
     `pipewire`, `jack`, `oss`, `speex`, `speexrate`, `upmix`,
     `vdownmix`, `samplerate`, `lavrate`, `null`, `dmix`,
     `dsnoop`, `modem`, `phoneline`) are blocked **only when**
     they have **no** `:CARD=…` binding. Card-bound variants
     like `sysdefault:CARD=USB` route to a real card and must
     survive — that's the entry the user wants to pick when the
     dock's silent endpoint becomes the OS default and they need
     to fall back to the laptop's built-in mic.

  Rationale: simple prefix-token check (split on `:`, trim,
  lowercase) keeps the filter cheap, deterministic, and easy to
  extend. macOS Core Audio and Windows MMDevice names never
  match any blocklist token, so the filter is a Linux-only
  no-op elsewhere — no `cfg(target_os)` gate needed.

- [ ] Task 2. **Apply the filter in `enumerate_input_devices_raw`
  (renamed from the body of the current `list_input_devices`).**
  Skip any device whose name returns true from
  `is_alsa_plugin_name` *before* deduplication. This collapses a
  noisy 20+ -entry submenu to ~1–4 real-hardware rows on a
  typical PipeWire system, and skips the dsnoop probe path that
  fires when those plugins are touched.

- [ ] Task 3. **Add a 10-second enumeration cache.**
  Introduce a module-private
  `static CACHE: OnceLock<Mutex<Option<(Instant, Vec<InputDevice>)>>>`
  and a `const ENUM_CACHE_TTL: Duration = Duration::from_secs(10)`.
  Public surface:
  - `pub fn list_input_devices()` — cached read; returns the
    cached vec when `elapsed < ENUM_CACHE_TTL`, otherwise
    re-enumerates and stores. This is what the tray refresh
    loop and `fono doctor` call.
  - `pub fn refresh_input_devices()` — bypasses the cache,
    re-enumerates, updates the cache, returns. Use from
    explicit user actions where freshness matters more than
    enumeration cost: `fono use input <name>` resolution
    (`crates/fono/src/cli.rs` `UseCmd::Input` arm) and the
    first-run wizard's microphone picker
    (`crates/fono/src/wizard.rs::pick_input_device_if_needed`).

  Rationale: tray polls every ~2 s, so a 10 s TTL means ~80 % of
  polls hit cache; freshly-plugged USB mics still appear within
  10 s without user action. Daemon-startup cost is unchanged
  (first call is uncached).

- [ ] Task 4. **Wire `refresh_input_devices()` into the two
  freshness-sensitive call sites.**
  - `crates/fono/src/cli.rs` — replace the
    `fono_audio::devices::list_input_devices()` call inside the
    `UseCmd::Input` arm (current lookup at ~`:972`) with
    `refresh_input_devices()` so the user gets up-to-the-moment
    enumeration after physically plugging in the device they
    want to switch to.
  - `crates/fono/src/wizard.rs::pick_input_device_if_needed` —
    same swap. The wizard runs once on first launch; freshness
    matters more than the cost of one enumeration.

  Leave the daemon's `MicrophonesProvider`, the recovery hook
  in `crates/fono/src/audio_recovery.rs`, and `fono doctor` on
  the cached `list_input_devices()` — they all benefit from the
  TTL.

- [ ] Task 5. **Tests.**
  Add to the existing `#[cfg(test)] mod tests` in
  `devices.rs`:
  - `blocks_plain_alsa_plugin_names` — every blocklist token
    returns `true` from `is_alsa_plugin_name`.
  - `blocks_speaker_channel_mappings_even_with_card_binding` —
    `front:CARD=PCH,DEV=0`, `surround51:CARD=PCH,DEV=0`,
    `iec958:CARD=PCH,DEV=0`, `hdmi:CARD=NVidia,DEV=0` all
    return `true`.
  - `keeps_card_bound_plugin_variants` — `sysdefault:CARD=PCH`,
    `sysdefault:CARD=USB`, `hw:CARD=PCH,DEV=0`,
    `plughw:CARD=USB,DEV=0` all return `false`.
  - `keeps_macos_and_windows_style_names` — `MacBook Pro
    Microphone`, `Microphone (Realtek …)`, `Logitech BRIO`,
    `USB Audio Device` all return `false`.
  - `refresh_returns_a_vec_without_panicking` — symmetry with
    the existing infallibility test on `list_input_devices`.

- [ ] Task 6. **Docs + changelog.**
  - Update the module doc-comment at the top of `devices.rs` to
    explain the two-tier filter and the cache TTL.
  - Add an entry under `## [Unreleased] / ### Fixed` in
    `CHANGELOG.md`: "Tray Microphone submenu and `fono doctor`
    no longer list ALSA plugin pseudo-devices (`pulse`, `oss`,
    `speex`, `default`, `surround*`, `iec958`, `hdmi`, …); only
    real-hardware-shaped entries (`hw:CARD=…`,
    `plughw:CARD=…`, `sysdefault:CARD=…`) are surfaced.
    Enumeration is now cached for 10 s, dramatically reducing
    repeated `snd_pcm_dsnoop_open` errors from cpal's ALSA
    backend during tray refreshes."
  - Append a session note to `docs/status.md` linking back to
    plan v1 of this fix and the empty-transcript recovery v2
    plan it patches.

- [ ] Task 7. **Verification.**
  Run `./tests/check.sh` (full matrix) and confirm no
  regressions. Manually start the daemon on the affected
  machine (laptop + dock with passive capture endpoint) and
  inspect:
  - tray "Microphone" submenu — should show only the
    `sysdefault:CARD=…` / `hw:CARD=…` / `plughw:CARD=…` rows
    matching real cards (typically the laptop's built-in mic
    and the dock's silent endpoint), not `pulse` / `oss` /
    `speex` etc.;
  - daemon stderr — `snd_pcm_dsnoop_open` lines should appear
    at most once every 10 s (cache miss boundary), not every
    2 s (tray poll period).

## Verification Criteria

- `is_alsa_plugin_name("pulse")`, `is_alsa_plugin_name("oss")`,
  `is_alsa_plugin_name("speex")`, `is_alsa_plugin_name("default")`,
  `is_alsa_plugin_name("upmix")`, `is_alsa_plugin_name("hdmi")`,
  `is_alsa_plugin_name("front:CARD=PCH,DEV=0")`,
  `is_alsa_plugin_name("surround51:CARD=PCH,DEV=0")`,
  `is_alsa_plugin_name("iec958:CARD=PCH,DEV=0")` all return
  `true`.
- `is_alsa_plugin_name("sysdefault:CARD=USB")`,
  `is_alsa_plugin_name("hw:CARD=PCH,DEV=0")`,
  `is_alsa_plugin_name("plughw:CARD=USB,DEV=0")`,
  `is_alsa_plugin_name("MacBook Pro Microphone")`,
  `is_alsa_plugin_name("USB Audio Device")` all return `false`.
- Two consecutive `list_input_devices()` calls within 10 s do
  not invoke cpal's `host.input_devices()` twice (verifiable by
  instrumenting the test or by observation: dsnoop noise drops
  to one burst per 10 s, not one per 2 s).
- `refresh_input_devices()` always runs the raw enumeration,
  regardless of cache freshness.
- `./tests/check.sh` (full matrix — fmt + build × default +
  interactive + clippy × 2 + tests × 2) green.
- On the affected dock: tray submenu lists only real
  microphones; user can switch to the laptop's built-in mic in
  one click and the next dictation succeeds.

## Potential Risks and Mitigations

1. **A future ALSA plugin name we haven't blocklisted slips
   through and ends up in the submenu.**
   Mitigation: the blocklist is permissive (filter only
   well-known tokens). New/unknown names are kept and surfaced;
   the worst case is a noisy entry, not a missing real
   microphone. The two-tier rule keeps card-bound entries
   regardless of prefix, so we'd never accidentally hide a real
   microphone.

2. **A user has a custom ALSA config where one of the
   blocklisted plain plugin names *is* their preferred capture
   path** (e.g. they explicitly want to dictate via a `pulse`
   bridge with no `:CARD=…` binding).
   Mitigation: the override path is unaffected. They can still
   set `[audio].input_device = "pulse"` in `config.toml` by
   hand and the runtime will honour it (cpal opens the named
   device verbatim; only the *enumeration* helper filters). A
   release note in CHANGELOG points this out.

3. **Cache returns stale data after a hot-plug: user plugs in
   their USB mic and the submenu doesn't show it for up to 10
   seconds.**
   Mitigation: 10 s is short enough to feel responsive. Two
   user-visible actions (`fono use input` and the wizard
   picker) bypass the cache via `refresh_input_devices()`. We
   could shorten the TTL but at the cost of more dsnoop noise;
   10 s is the sweet spot.

4. **Cache `Mutex` contention.**
   Mitigation: lock is held only for a Vec clone (~tens of
   strings); the slow path (cpal enumeration) runs *outside*
   the lock between the two `lock()` calls. No real
   contention concern even at 10 Hz polling.

5. **`OnceLock` warm-up race when the daemon and a CLI command
   both call `list_input_devices` concurrently.**
   Mitigation: `OnceLock::get_or_init` is documented
   thread-safe; first-call wins, the loser observes the
   already-initialised `Mutex` and proceeds normally.

6. **Existing tests in `audio_recovery::tests::body_*` rely on
   the recovery helper's candidate filtering** — independent of
   the plugin blocklist. The blocklist runs upstream in
   `list_input_devices`, so the helper sees fewer (cleaner)
   inputs, but its tests inject device lists directly via the
   `build_body` helper signature and don't depend on cpal at
   all.
   Mitigation: no change required; the existing tests stay
   green.

## Alternative Approaches

1. **Probe-open every device with `default_input_config()` and
   drop those that error out.** Catches plugins that fail at
   probe time even if their name doesn't match the blocklist.
   *Trade-off:* the probe call is what triggers the
   `snd_pcm_dsnoop_open` errors in the first place — running it
   on every entry would increase noise during enumeration, not
   reduce it. Rejected as making the symptom worse.

2. **Redirect ALSA's stderr around `host.input_devices()`.**
   Would silence the dsnoop noise without changing what's
   surfaced.
   *Trade-off:* invasive (FD plumbing), platform-specific, and
   doesn't address the wrong-items-in-submenu symptom. The
   blocklist solves both issues cleanly; rejected.

3. **Collapse ALSA entries to one row per `CARD=…` binding**
   (keep `plughw:CARD=PCH,DEV=0` and drop `hw:CARD=PCH,DEV=0`,
   `sysdefault:CARD=PCH`, `front:CARD=PCH,DEV=0` for the same
   card).
   *Trade-off:* friendlier UX (one row per physical mic) but
   loses meaningful distinctions — `plughw:` cooks the format,
   `hw:` is raw, `sysdefault:` follows ALSA's default routing
   for the card. Some users want the raw entry. Worth a
   follow-up plan after this fix lands and we see how the
   simple blocklist plays out in practice.

4. **Switch cpal to its JACK or PulseAudio host on Linux.**
   Eliminates ALSA enumeration entirely.
   *Trade-off:* new dependencies, breaks systems without those
   daemons, and shifts the same problem to a different surface
   (PulseAudio enumerates `auto_null`, monitor sources, etc.).
   Out of scope for this fix.
