# First-run autostart + wizard unification

## Objective

After any supported install path — `curl https://fono.page/install | sh`,
`sudo fono install`, the deb/Arch/Slackware/Nix packages, or a manual
binary drop followed by `fono install` — Fono must, with one keystroke
of user input or fewer:

1. **Run the setup wizard** in the appropriate user context, against
   that user's `~/.config/fono`.
2. **Start the daemon immediately** without requiring a logout / re-login.
3. **Enable the daemon at boot or graphical-session start** through the
   right mechanism for the install lane:
   - **Graphical desktop install** → `systemd --user` unit, enabled,
     so the daemon comes back on every login (X11, Wayland, or a TTY
     session under `loginctl enable-linger`). XDG autostart kept as a
     belt-and-braces fallback for non-systemd sessions.
   - **Headless / root install** → `systemd --system` unit, enabled
     and started immediately (already works today; only the wizard
     hand-off needs the headless skip).
4. **Detect the environment automatically** (graphical vs headless;
   TTY vs non-interactive; `systemd --user` availability; running as
   root vs ordinary user) and pick the right lane without asking the
   user to choose.

Re-running any install path against an already-configured host is
idempotent: never overwrite a non-empty `config.toml`, never re-enable
an already-enabled unit, never re-run the wizard.

## Implementation Plan

### A — New `[Install Mode]` taxonomy + invocation routing

- [ ] Task A1. Replace today's binary `Mode::{Desktop, Server}` enum in
  `crates/fono/src/install.rs:58-72` with a richer `Lane` taxonomy
  derived from environment probing rather than a CLI flag alone:
  `UserSystemd` (default for non-root installs when `systemd --user`
  is available), `UserAutostart` (fallback for non-systemd graphical
  sessions), and `SystemSystemd` (current `--server`). The flag
  `--server` keeps its meaning; root with no flag still implies
  `SystemSystemd`; non-root drops into `UserSystemd` /
  `UserAutostart`. Rationale: today the desktop lane silently picks
  XDG autostart and waits for re-login — the user must currently
  `journalctl --user -u fono` or run `fono` by hand to verify the
  daemon. A `systemd --user` lane is symmetrical with the server
  lane, supports immediate start, and is exactly what every modern
  distro ships.

- [ ] Task A2. Add a graphical-session probe that re-uses
  `fono_hotkey::is_graphical_session()` (the same helper that gates
  the global-hotkey listener at `docs/status.md:179-183`) so install
  and `fono install` agree on what counts as "headless". On a
  headless box even without `--server`, `fono install` invoked as
  root should pick `SystemSystemd` and warn that the wizard cannot
  run unattended; non-root on a headless box should refuse with a
  hint to run with sudo.

- [ ] Task A3. Make `fono install` (no flag) work as an ordinary user
  for the user-lane. Today `install.rs:167-172` aborts unless EUID 0
  because every path is system-wide. Split the binary-copy and unit
  installation between root-required paths (system `BIN_PATH`,
  `SYSTEMD_UNIT`) and user-owned paths (`~/.local/bin/fono`,
  `~/.config/systemd/user/fono.service`,
  `~/.config/autostart/fono.desktop`). Drop privilege only when
  required, and surface a single `--system` flag that forces the old
  behaviour for advanced users.

### B — Wizard hand-off across all entry points

- [ ] Task B1. After a successful install (any lane), invoke
  `fono setup` interactively iff:
  - stdin/stdout are a TTY (`io::IsTerminal`), AND
  - `~/.config/fono/config.toml` does not already exist for the target
    user, AND
  - the install was invoked from a foreground shell (heuristic:
    `SHLVL>=1` and no `SYSTEMD_EXEC_PID`).
  The wizard runs *as the invoking user* — when `fono install` was
  re-entered through `sudo`, drop privileges via `SUDO_USER` /
  `SUDO_UID` before exec'ing the wizard so it writes to the user's
  XDG home, not root's. Rationale: today the wizard is launched only
  by implicit first-`fono` invocation, which the user typically never
  experiences because XDG autostart fires it under systemd before any
  TTY is attached.

- [ ] Task B2. Promote the implicit first-run wizard path in
  `crates/fono/src/cli.rs:396-428` to an explicit "wizard-needed"
  notification when the daemon comes up unconfigured under a
  non-interactive launch (current behaviour writes a default config
  silently). Replace the silent default with a `Critical`-urgency
  desktop notification ("Fono needs setup — open a terminal and run
  `fono setup`"), and arm it through the same `critical_notify`
  cascade cap so it fires at most once per session. The default
  `Config::default()` write happens after the notification.

- [ ] Task B3. Make `fono setup` end-state automation finish the job:
  after the wizard completes, run a single `enable_and_start` helper
  that picks the right `systemctl` call for the active lane
  (`--user enable --now fono.service` vs `--system enable --now
  fono.service`) and surfaces failures the same way
  `verify_service_running` does today (`install.rs:282-327`). This
  means a user who hits `fono setup` first (without running `fono
  install`) still ends with an enabled, running daemon.

### C — One-liner install script (off-repo, but in-scope strategically)

- [ ] Task C1. Land the new install script content in the repo at
  `packaging/install.sh` (or `packaging/install/install.sh`) as the
  canonical source for `https://fono.page/install`. The website's
  copy becomes a synced artefact published from this file by the
  release workflow. Without a repo-tracked source the script today
  drifts silently. Document the publish step in
  `docs/dev/release-checklist.md`.

- [ ] Task C2. Have the new script: (a) probe `uname` + `ldd
  --version` to pick the CPU vs GPU asset (already handled inside
  `fono update`; the script should *not* re-implement variant
  detection — instead it downloads the CPU asset and immediately
  defers to `fono update` for the variant-aware swap); (b) verify
  the per-asset `.sha256` sidecar (shipped since `87221a2`,
  `docs/status.md:1418-1444`); (c) drop the binary at `${BIN_DIR:-
  /usr/local/bin}` if writable, falling back to `~/.local/bin`
  otherwise (and warning if that's not on `PATH`); (d) exec
  `fono install` so all post-install behaviour (lane detection,
  wizard, systemd enable) lives in *one* code path inside the
  binary — never duplicated in shell.

- [ ] Task C3. The script gracefully handles re-runs (already-installed
  binaries) by delegating to `fono update` instead, and prints a
  one-line `fono doctor` health summary at the end. Rationale: a
  user running the one-liner twice should not be punished with a
  "marker exists" error from `install.rs:383-392`; the binary
  already knows how to upgrade itself.

### D — Distro packages

- [ ] Task D1. Convert the user-systemd unit shipped by the deb
  (`packaging/debian/rules:18-19`), PKGBUILD
  (`packaging/aur/PKGBUILD:46`), and SlackBuild
  (`packaging/slackbuild/fono/doinst.sh`) to be *preset-enabled* by
  default. Drop a `90-fono.preset` file under
  `/usr/lib/systemd/user-preset/` containing `enable fono.service`;
  this is the standard mechanism for a system-installed user unit
  to come up on every user's first login without per-user manual
  `systemctl --user enable`. Document that the user can opt out by
  dropping a higher-priority preset masking it.

- [ ] Task D2. Update each package's post-install scriptlet to add a
  one-line notice — "Fono needs setup; run `fono` once in a
  terminal to start the wizard, or it will prompt you on next
  graphical login via a notification." — instead of today's
  ad-hoc instructions in `packaging/slackbuild/fono/doinst.sh:12-25`.
  The daemon's notification (Task B2) handles the rest.

- [ ] Task D3. Decide whether the deb/Arch packages should also ship
  the *system* unit (`/lib/systemd/system/fono.service`) **disabled
  by default**, so that an admin who wants a headless server lane
  can `sudo systemctl enable --now fono.service` without first
  running `sudo fono install --server`. Trade-off: bigger package
  scope, but consistent with how packages currently ship optional
  units (e.g. `redis-server.service`). Default recommendation: yes,
  ship both, disabled by default for the system unit.

### E — Idempotency, telemetry-free reporting, and tests

- [ ] Task E1. Lock idempotency in unit tests next to the existing
  ones at `crates/fono/src/install.rs:780-854`: re-running install
  with an existing marker + matching mode is a no-op; with a
  matching mode but a missing config the wizard hand-off still
  fires; with a different mode the existing refusal stays.

- [ ] Task E2. Add a `fono doctor` row reporting (a) which install
  lane is active (read from the marker), (b) whether the relevant
  unit (`--user` or `--system`) is `enabled` and `active`, (c)
  whether the user's `config.toml` exists. Extends the existing
  `Install section` from `docs/status.md:419` so users have a
  single place to see whether the autostart promise was kept.

- [ ] Task E3. Integration test in
  `crates/fono/tests/install_round_trip.rs` (new) that drives the
  three lanes against a `tempfile` HOME / state dir and asserts:
  binary copied to the lane-appropriate path, unit file present,
  marker written, wizard skipped under non-TTY, wizard invoked
  under TTY. The `enable --now` call is mocked (env var stub)
  because CI runners aren't supposed to spawn long-lived daemons.

### F — Docs + changelog

- [ ] Task F1. Rewrite the `## First run` section of `README.md`
  (currently anchored after the install table around
  `README.md:30`) to a single narrative: "After any install path,
  Fono runs the wizard in your terminal and starts itself. That's
  it." Drop the explicit `fono setup` mention from the docs as a
  required first step — keep it only as the re-run path.

- [ ] Task F2. New ADR
  `docs/decisions/0026-first-run-autostart-lanes.md` recording the
  three-lane decision (UserSystemd / UserAutostart / SystemSystemd),
  why `systemd --user` won over the previous XDG-only desktop lane,
  and how the script defers all real logic to the binary.

- [ ] Task F3. CHANGELOG `[Unreleased]` Added/Changed/Removed entries
  per the AGENTS.md release rules; ROADMAP shipment row prepared for
  the cut.

## Verification Criteria

- A fresh user on a GNOME / KDE / sway / i3 desktop running
  `curl -fsSL https://fono.page/install | sh` from a terminal ends
  with: wizard completed, `systemctl --user is-active fono.service`
  → `active`, `systemctl --user is-enabled fono.service` →
  `enabled`, and `fono doctor` reporting "lane: user-systemd,
  unit: active+enabled, config: present".
- The same command on a headless box as root ends with:
  `systemctl is-active fono.service` → `active`, install marker
  records `SystemSystemd`, no wizard ran, a `Critical` desktop
  notification path is *not* taken (no display server).
- The same command on a graphical box where `systemd --user` is
  unavailable (rare; e.g. `runit`-based distros) falls back to the
  `UserAutostart` lane, drops `~/.config/autostart/fono.desktop`,
  spawns `fono` immediately as a child of the install shell, and
  records `UserAutostart` in the marker.
- `sudo apt install ./fono_*.deb` (or the PKGBUILD / SlackBuild
  equivalent) ends with the user-systemd preset taking effect on
  the next login, plus a notification prompting the wizard if the
  user logged in before running the wizard manually.
- Re-running any of the four install paths against an
  already-installed, already-configured host is a no-op and prints
  a single-line "already installed and configured" summary.
- `crates/fono/src/install.rs` unit tests + the new
  `install_round_trip.rs` integration test all green;
  `cargo clippy --workspace --all-targets -- -D warnings` clean.

## Potential Risks and Mitigations

1. **`systemd --user` is unavailable or disabled on the user's
   distro.** Detection via `systemctl --user show-environment` exit
   status; fall back to `UserAutostart` (existing XDG path) and
   record the lane in the marker so uninstall reverses exactly what
   was put down.

2. **`sudo fono install` running the wizard as the invoking user
   leaks env vars or violates DBus session ownership.** Drop
   privilege with `setuid(SUDO_UID)` + `setgid(SUDO_GID)` only for
   the wizard subprocess, never for the install path itself; pass
   `XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS`, and `HOME`
   explicitly so the wizard's later attempts to talk to
   notification / tray buses work.

3. **Wizard hand-off blocks the install script in non-interactive
   contexts (CI, Ansible, packer, Docker provisioning).** Honour
   `FONO_NONINTERACTIVE=1` and the existing TTY check; skip the
   wizard silently when set, falling back to `Config::default()`
   plus the Critical notification on next login.

4. **Distro user-presets conflict with already-installed user
   configurations.** Use a high `90-` prefix so an admin can mask
   it with `10-disable-fono.preset`; document the override path in
   the README + the package post-install message.

5. **Two daemons end up running** (one from `systemd --user`, one
   spawned by the install script's foreground `fono`). The
   install script's foreground spawn is only the *Autostart-lane*
   fallback; on the systemd-user lane it never spawns directly,
   only `systemctl --user start`. The daemon's single-instance lock
   (existing IPC socket binding) keeps a duplicate from succeeding
   anyway.

6. **Off-repo install script drift.** Task C1 moves the canonical
   copy into the repo and publishes via CI to fono.page; the
   website-only copy becomes a build artefact, not a manual edit.

## Alternative Approaches

1. **Leave XDG autostart as the desktop lane and only add the
   wizard hand-off + autostart-on-install.** Smaller delta but
   keeps the "daemon only starts on next login" wart and means
   `fono doctor` still cannot report "unit active+enabled" because
   there's no unit. Rejected on UX grounds — the user explicitly
   asked for autostart that works *now*, not on next login.

2. **Always use `systemd --user`, no XDG fallback.** Simpler
   matrix, but loses the (uncommon but real) non-systemd-user
   distros. The fallback is cheap to keep.

3. **Make the install script run setup directly via dialoguer over
   a piped shell.** Rejected — the wizard needs a real TTY for arrow
   keys and masked input (`crates/fono/src/wizard.rs:1900-1925`);
   running it from a `curl | sh` subshell breaks stdin. The script
   must `exec` the binary in the foreground terminal, not pipe to
   it.

4. **Ship the wizard as a one-shot systemd unit
   (`fono-setup.service`) instead of a foreground subprocess.**
   Cleaner from the systemd perspective, but a user-systemd
   unit running at first login can't grab the user's terminal —
   it would still have to send a notification and wait for the
   user to come back. Equivalent to today's "implicit first-run
   wizard" behaviour, which is what we're trying to improve.
