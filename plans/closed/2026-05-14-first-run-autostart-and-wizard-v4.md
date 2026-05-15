# First-run autostart + wizard unification

## Status: Superseded

## Objective

After any supported install path — `curl https://fono.page/install | sh`,
`sudo fono install`, the deb / Arch / Slackware / Nix packages, or a
manual binary drop followed by `fono install` — Fono must, with at most
one keystroke of user input:

1. **Run the setup wizard** in the right user context when possible.
2. **Start the daemon immediately** *regardless of whether the wizard
   ran*; default config is good enough to come up with local Whisper
   and no LLM cleanup.
3. **Enable the daemon at boot or graphical-session start.**
4. **Make the wizard discoverable from the tray** so a user who
   dismissed the install-time prompt (or whose distro install never
   ran one) can finish setup with a single click.

### Install locations (FHS-aligned, **two paths only**)

| Source of the binary | Path | Owner of that path |
|---|---|---|
| Distro package (deb / AUR / Slackware) | `/usr/bin/fono` | package manager |
| Everything else (`fono install`, `curl \| sh`, manual `cp`) | `/usr/local/bin/fono` | sysadmin / installer |

No `~/.local/bin/fono`. No third path. The binary always lives
system-wide; only the **systemd unit** is per-lane.

### Lane probe (one rule, no flag-juggling)

```
DISPLAY|WAYLAND_DISPLAY set  AND  --server not passed   →  Desktop
otherwise                                               →  Server
```

### Missing-config behaviour (the new bit)

Both lanes follow the same rule: **the daemon always starts**, even
without a wizard run. They differ only in how the "you should
finish setup" signal reaches a human.

| Lane | Behaviour when `config.toml` is absent or `setup_completed=false` |
|---|---|
| Desktop | Daemon starts on `Config::default()`. Fires one `Critical`-urgency notification with a "Run setup" action button. Tray shows a prominent **⚠ Setup needed — click to finish** entry at the top of the menu. Clicking either spawns a terminal running `fono setup` (`x-terminal-emulator` → `gnome-terminal` → `konsole` → `kitty` → `xterm` fallback chain). When the wizard finishes, it sets `setup_completed=true`, calls `Request::Reload` over IPC, and the tray entry / notification arming both go away. |
| Server | Daemon starts on `Config::default()`. Logs one `WARN`-level line on every boot: `fono: running on default config; finish setup with \`sudo -u $SUDO_USER fono setup\` or edit /etc/fono/config.toml`. No notification (no user to see it). `fono doctor` prints the same hint, prefixed `[setup needed]`. |

A new sentinel `[general].setup_completed: bool` (default `false`)
distinguishes "default config because the user never ran the wizard"
from "default config because the user explicitly wants defaults".
The wizard writes `true` on success. The daemon checks the flag at
startup and on every `Reload`.

## Implementation Plan

### A — Lane selection + single binary path

- [ ] Task A1. Replace `Mode::{Desktop, Server}` in
  `crates/fono/src/install.rs:58-72` with `Lane::{Desktop, Server}`,
  picked by the probe above (re-using
  `fono_hotkey::is_graphical_session()` per `docs/status.md:179-183`).
  The `--server` flag stays as an explicit override. Marker field
  serialised as `"desktop"` / `"server"`.

- [ ] Task A2. Keep `BIN_PATH = /usr/local/bin/fono` (`install.rs:42`)
  for **both** lanes. Per-lane diverges only on:
  - unit location (`~/.config/systemd/user/fono.service` vs
    `/lib/systemd/system/fono.service`),
  - whether the system user `fono` is created (server only),
  - which `systemctl` invocation enables / starts the unit.

- [ ] Task A3. Both lanes require root to write
  `/usr/local/bin/fono`. Non-root invocations of `fono install`
  re-exec via `sudo` for the binary-and-marker step, then drop
  privileges (`setuid(SUDO_UID)` / `setgid(SUDO_GID)`) for the
  user-unit drop, the wizard, and `systemctl --user enable --now`.
  `require_root()` (`install.rs:167-172`) moves inside the
  binary-copy step.

### B — Missing-config daemon behaviour

- [ ] Task B1. Add `[general].setup_completed: bool` to
  `crates/fono-core/src/config.rs` (default `false`). The wizard at
  `crates/fono/src/wizard.rs:51` sets it to `true` right before
  saving. Loading a pre-existing `config.toml` (from before this
  change) without the field treats the absence as `true`
  via a `#[serde(default = "default_true")]` migration helper
  driven by the schema-version bump (the existing migration block
  per `docs/status.md:1012-1016`). This keeps every already-configured
  user out of the "setup needed" state.

- [ ] Task B2. **Desktop autostart now always succeeds.** When the
  daemon comes up on default config:
  1. Write `Config::default()` to disk if `config.toml` is absent
     (existing behaviour at `crates/fono/src/cli.rs:412-427`).
  2. Fire one `Critical`-urgency notification via the existing
     `critical_notify` cascade cap with body "Fono needs setup —
     pick STT / LLM backends and hotkeys" and a `notify-rust`
     action button labelled "Run setup". Action handler opens a
     terminal running `fono setup` via
     `crate::install::open_terminal_with("fono setup")` (new
     helper, see Task B5).
  3. Proceed with daemon startup. Local Whisper still works on
     defaults; the user is dictating within seconds even if they
     ignore the notification.

- [ ] Task B3. **Server missing-config UX.** Daemon startup logs
  one `WARN`-level line per boot: the message above. `fono doctor`
  surfaces the same hint at the top of its output with a
  `[setup needed]` prefix (extending the doctor row in plan task
  E3). No notification is fired on the server lane (no
  `DISPLAY`/`WAYLAND_DISPLAY`; the existing graphical-session probe
  already gates notifications today). Crucially the daemon still
  starts and answers IPC requests so `fono doctor` and `fono use
  stt …` work over the system socket from any admin login.

- [ ] Task B4. **Tray "⚠ Setup needed" entry.** When the daemon's
  loaded config has `setup_completed == false`, the tray (already
  consuming provider snapshots every 2 s per
  `docs/status.md:842-848`) prepends a top-of-menu entry titled
  "⚠ Setup needed — click to finish". `TrayAction::OpenSetup` fires
  the same `open_terminal_with("fono setup")` helper as the
  notification action. Once the wizard finishes and `Reload` lands,
  the tray refresh cycle drops the entry on its next poll. No
  separator change, no menu reflow when not needed.

- [ ] Task B5. New `crate::install::open_terminal_with(cmd: &str)`
  helper picks an emulator via the fallback chain
  `x-terminal-emulator` → `gnome-terminal --` → `konsole -e` →
  `kitty` → `xterm -e`, the first one that resolves on `$PATH` wins.
  If none resolve, fall back to a *second* notification reading
  "Run `fono setup` in any terminal to finish configuration". Used
  by both the notification action and the tray click; one code path,
  one fallback.

- [ ] Task B6. After `fono setup` completes (whether invoked from
  the tray, the notification action, or `fono setup` direct from
  the CLI), the wizard at `crates/fono/src/wizard.rs:127` already
  prints "Run `fono` to start the daemon". Replace that with a
  call into the new `enable_and_start` helper from Task B7, AND a
  best-effort `Request::Reload` over IPC so a *running* daemon
  picks up the new config without restart. Both paths covered.

### C — Wizard hand-off at install time

- [ ] Task C1. After binary + unit are in place, `fono install`
  runs the wizard inline against the invoking user's XDG home
  (TTY check via `io::IsTerminal`; honour `FONO_NONINTERACTIVE=1`).
  On the server lane re-entered via `sudo`, drop privileges as in
  Task A3. On the desktop lane the wizard runs unprivileged from
  the start. The wizard sets `setup_completed = true`.

- [ ] Task C2. `fono setup` end-state automation: shared
  `enable_and_start` helper picks `systemctl --user enable --now`
  on the desktop lane and `systemctl enable --now` on the server
  lane, surfacing failures via the existing
  `verify_service_running` (`install.rs:282-327`). Runs from both
  `fono install` and post-wizard `fono setup`.

- [ ] Task C3. `fono setup` invoked when a daemon is already
  running issues `Request::Reload` over IPC at exit so the new
  config takes effect immediately. Falls back to the
  `enable_and_start` path when no daemon is running.

### D — One-liner install script

- [ ] Task D1. Move canonical `https://fono.page/install` script
  into `packaging/install.sh`. Release workflow uploads alongside
  binaries; website hosts the published artefact. Publish step
  added to `docs/dev/release-checklist.md`.

- [ ] Task D2. Script flow:
  1. Detect arch / glibc; pick the CPU asset (variant swap-up is
     `fono update`'s job).
  2. Download asset + `.sha256` sidecar
     (`docs/status.md:1418-1444`); verify before exec.
  3. `sudo install -m 0755 fono /usr/local/bin/fono` (re-exec via
     `sudo` if not already root).
  4. `exec /usr/local/bin/fono install`. Binary picks the lane,
     runs wizard, enables unit.

- [ ] Task D3. Re-runs delegate to `fono update`; one-line
  `fono doctor` summary on completion. `refuse_if_package_managed`
  (`install.rs:174-185`) preserves the contract for distro hosts.

### E — Distro packages

- [ ] Task E1. Distro packages keep `/usr/bin/fono` (FHS).
  `fono_update::is_package_managed` already protects it.

- [ ] Task E2. User-systemd unit shipped by deb / PKGBUILD /
  SlackBuild becomes **preset-enabled**: drop `90-fono.preset`
  under `/usr/lib/systemd/user-preset/` containing `enable
  fono.service`. Every user's first login brings the unit up; the
  daemon hits Task B2 (default config + notification + tray
  entry) on that first boot. The user resolves the "setup needed"
  state from the tray, not from the post-install message.

- [ ] Task E3. System unit (`/lib/systemd/system/fono.service`)
  shipped **disabled by default** so admins can `sudo systemctl
  enable --now` for the server lane. Both units shell out to
  `/usr/bin/fono`.

- [ ] Task E4. Replace ad-hoc instructions at
  `packaging/slackbuild/fono/doinst.sh:12-25` with one line:
  "Open a terminal and run `fono` once to finish setup, or click
  the tray icon when the daemon comes up on next login."

### F — Tests + doctor row

- [ ] Task F1. Extend install unit tests at
  `crates/fono/src/install.rs:780-854` to lock the lane probe and
  the privilege-drop boundary.

- [ ] Task F2. New integration test at
  `crates/fono/tests/install_round_trip.rs` mocking `systemctl`
  via env-var stub — walks both lanes against a `tempfile` HOME.

- [ ] Task F3. New unit tests in `crates/fono-core/src/config.rs`
  locking the `setup_completed` defaults: fresh `Config::default()`
  → `false`; loading an old config without the field → `true`
  (migration); wizard-written config → `true`.

- [ ] Task F4. New tray test asserting the "⚠ Setup needed"
  entry is present when `setup_completed=false` and absent when
  `true`, plumbed through the existing provider-snapshot poll
  pattern.

- [ ] Task F5. `fono doctor` gets new rows: (a) active lane from
  the marker, (b) `is-enabled` + `is-active` for the lane's unit,
  (c) `setup_completed` flag, (d) which binary path
  (`/usr/bin/fono` vs `/usr/local/bin/fono`), (e) terminal-emulator
  fallback chain resolution (which one will the tray entry open?).

### G — Docs + changelog

- [ ] Task G1. Rewrite `## First run` in `README.md` (around
  `README.md:30`) to one paragraph: "After any install path, Fono
  runs and starts itself. If a wizard didn't run during install
  (autostart, distro package, server boot), the tray shows a
  ⚠ Setup needed entry and a desktop notification offers a 'Run
  setup' button. Either way the daemon is alive — local Whisper
  works out of the box."

- [ ] Task G2. New ADR
  `docs/decisions/0026-first-run-autostart-lanes.md` recording:
  two-path FHS split, two-lane unit decision, privilege-drop
  boundary, daemon-always-starts contract, setup_completed
  sentinel, tray-and-notification setup discovery surface.

- [ ] Task G3. CHANGELOG `[Unreleased]` Added / Changed / Removed.

## Verification Criteria

- Fresh desktop user runs `curl -fsSL https://fono.page/install
  | sh` from a terminal → wizard completes inline, daemon active
  under `systemd --user`, `setup_completed=true`. No tray
  "Setup needed" entry. No notification arming.
- Fresh desktop user installs the `.deb` and logs in → daemon
  starts under `systemd --user` via preset, comes up on default
  config, fires one Critical notification with "Run setup"
  action, tray shows "⚠ Setup needed". Clicking either spawns a
  terminal running `fono setup`. After wizard exits, both signals
  disappear within one tray poll cycle (≤ 2 s).
- Fresh headless box runs `curl … | sh` over SSH without TTY →
  binary + system unit installed, daemon active on defaults,
  `journalctl -u fono` shows the WARN line, `fono doctor` shows
  `[setup needed]`.
- Admin SSHes to that headless box, runs `sudo -u admin fono
  setup`, completes wizard → `Request::Reload` fires; running
  daemon picks up new config without restart; `setup_completed`
  flips to `true`; WARN line gone on next start.
- `fono doctor` on a desktop host shows: lane, unit state,
  `setup_completed`, binary path, resolved terminal emulator.
- Re-running any install path is a no-op with a one-line summary.
- `cargo test -p fono -p fono-core -p fono-tray`,
  `cargo clippy --workspace --all-targets -- -D warnings`, and the
  new `install_round_trip.rs` all green.

## Potential Risks and Mitigations

1. **No terminal emulator on `$PATH`.** Fallback chain ends with
   a second notification ("Run `fono setup` in any terminal").
   `fono doctor` reports `terminal: none found` so the user knows
   why the tray entry's click goes to a notification rather than
   a new window.

2. **`systemd --user` missing on the desktop host.** Probe via
   `systemctl --user show-environment` exit status; fall back to
   `~/.config/autostart/fono.desktop` + foreground `fono` spawn
   for the current session. Marker still records `Desktop`. The
   tray + notification setup-discovery surface is unaffected.

3. **`Critical` notification fires repeatedly.** Already covered
   by the `critical_notify` cascade cap
   (`docs/status.md:51-83`) — at most one Critical per session;
   `setup_completed=true` after wizard completion silences future
   arming permanently.

4. **Tray "Setup needed" entry stuck after wizard completion.**
   The wizard's `Request::Reload` (Task B6) forces the tray to
   re-evaluate `setup_completed` on the next poll; worst case the
   entry lingers for one ~2 s tray refresh cycle. Acceptable.

5. **Old configs without `setup_completed`.** Serde-default to
   `true` on migration (Task B1) so existing users are never
   shown the "Setup needed" surface incorrectly.

6. **`sudo fono install` running the wizard as the invoking user
   leaks env / breaks DBus.** Drop privilege via `setuid(SUDO_UID)`
   / `setgid(SUDO_GID)`; pass `HOME`, `XDG_RUNTIME_DIR`,
   `DBUS_SESSION_BUS_ADDRESS` through. The install path stays root.

7. **Server admin never sees the WARN line.** Mitigated by the
   `fono doctor [setup needed]` row (Task B3 + F5). An admin
   running any `fono` CLI command sees the notice; the only way
   to miss it is to never run `fono` from a shell, in which case
   the user has bigger ops problems.

8. **Off-repo install script drift.** Canonical source moves into
   `packaging/install.sh` (Task D1); fono.page becomes a publish
   target.

## Alternative Approaches

1. **Refuse to start the daemon without a wizard run.** Today's
   server-mode behaviour pre-`docs/status.md:184-186`. Already
   rejected once for crash-looping on missing config; rejecting it
   again here keeps the user dictating-on-defaults out of the box.

2. **In-tray wizard (no terminal spawn).** Implementing the wizard
   as a native dialog (gtk / iced / egui) tied to the tray is
   ~3–5× the work of the terminal-spawn path, and would force a
   GUI-toolkit dependency back into the default ship binary that
   the v0.3.7 ALSA / GTK removal (`docs/status.md:772-794`,
   `:824-855`) deliberately took out. Rejected for v1; revisit
   only if the terminal-spawn UX measurably underperforms.

3. **`setup_completed` derived from `config == Config::default()`
   equality.** Avoids a new field, but breaks for a user whose
   genuine preferred config happens to equal the defaults. Sentinel
   is clearer and round-trips through serde.

4. **Notification action button only, no tray entry.** Notifications
   on Linux desktops vanish quickly (5–10 s); a tray entry stays
   put until clicked. Both surfaces together cover the matrix of
   "user was looking at the screen" vs "user opened the laptop an
   hour after autostart".
