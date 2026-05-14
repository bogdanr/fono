# First-run autostart + wizard unification

## Objective

After any supported install path — `curl https://fono.page/install | sh`,
`sudo fono install`, the deb / Arch / Slackware / Nix packages, or a
manual binary drop followed by `fono install` — Fono must, with at most
one keystroke of user input:

1. **Run the setup wizard** in the right user context.
2. **Start the daemon immediately**, no logout / re-login.
3. **Enable the daemon at boot or graphical-session start.**

Two lanes, picked by environment probe — no flag-juggling for the user:

- **Desktop** (graphical session present, ordinary user) → installs
  to the user's `~/.local/bin/fono` + a `systemd --user` unit,
  enabled and started immediately. The unit comes back automatically
  on every login.
- **Server** (no graphical session OR `--server` passed OR EUID 0
  without a session) → installs to `/usr/local/bin/fono` + the
  hardened `/lib/systemd/system/fono.service`, runs the wizard if a
  TTY is attached (else writes default config + a one-shot critical
  notification path), `systemctl enable --now`.

Lane detection: `DISPLAY` / `WAYLAND_DISPLAY` present + EUID ≠ 0 →
desktop. Anything else → server. The `--server` flag is an explicit
override only; not normally needed. Re-running an install is
idempotent: a present marker plus matching lane plus existing
`config.toml` short-circuits to a one-line "already installed"
summary.

## Implementation Plan

### A — Lane selection collapses to a single probe

- [ ] Task A1. Replace `Mode::{Desktop, Server}` in
  `crates/fono/src/install.rs:58-72` and the `--server` boolean in
  `run_install` (`install.rs:367-399`) with a `Lane::{Desktop, Server}`
  picked by one probe: `is_graphical_session()` (already in
  `fono-hotkey`, gated by `DISPLAY` / `WAYLAND_DISPLAY` per
  `docs/status.md:179-183`) **AND** `geteuid() != 0`. The `--server`
  flag stays as an explicit override for users who want the system
  unit on a graphical machine. Marker schema gains nothing new —
  the existing `Mode` field is renamed and serialised as `"desktop"`
  / `"server"`.

- [ ] Task A2. Allow `fono install` to run as an ordinary user on
  the desktop lane. Today `install.rs:167-172` aborts unless EUID 0.
  Split the path constants into a `Layout` struct picked per lane:
  - Desktop: `~/.local/bin/fono`,
    `~/.config/systemd/user/fono.service`,
    `~/.config/autostart/fono.desktop` (kept as a belt-and-braces
    fallback if the user's session lacks `systemd --user`),
    `~/.local/share/fono/install_marker.toml`.
  - Server: existing `/usr/local/bin/fono`,
    `/lib/systemd/system/fono.service`,
    `/usr/local/share/fono/install_marker.toml` (unchanged).
  Root + no `--server` on a graphical session refuses with a hint
  to run the command without `sudo` for the desktop lane.

### B — Wizard hand-off

- [ ] Task B1. After the binary and unit are in place, run the
  wizard inline (TTY check via `io::IsTerminal`; honour
  `FONO_NONINTERACTIVE=1` for CI / Ansible / Docker). The wizard
  writes to the *invoking user's* XDG home; on the server lane
  re-entered via `sudo`, drop privilege via `SUDO_UID`/`SUDO_GID`
  and pass `HOME`, `XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS`
  through to the wizard subprocess so it can talk to the user's
  notification / tray buses.

- [ ] Task B2. When the daemon comes up unconfigured under a
  non-interactive launch (today's silent `Config::default()` write
  at `crates/fono/src/cli.rs:412-427`), additionally fire a single
  `Critical`-urgency notification ("Fono needs setup — open a
  terminal and run `fono setup`") through the existing
  `critical_notify` cascade cap. The default config write stays.
  This covers users whose distro package landed the unit but who
  never ran the wizard yet.

- [ ] Task B3. `fono setup` itself becomes lane-aware: after the
  wizard's last screen, call a single `enable_and_start` helper
  that picks `systemctl --user enable --now fono.service` on the
  desktop lane or `systemctl enable --now fono.service` on the
  server lane, and surfaces failures using the same journal-tail
  output `verify_service_running` already emits at
  `install.rs:282-327`. A user who runs `fono setup` first (e.g.
  re-running it later) still ends with an enabled, running daemon.

### C — One-liner install script

- [ ] Task C1. Move the canonical `https://fono.page/install`
  script into the repo at `packaging/install.sh` so it stops
  drifting silently from binary behaviour. Publish step added to
  `docs/dev/release-checklist.md` so a release uploads the script
  alongside the binaries.

- [ ] Task C2. New script logic, end-to-end:
  1. Detect arch / glibc; pick the CPU asset (variant swap-up is
     `fono update`'s job — script does not duplicate it).
  2. Download the asset + its `.sha256` sidecar (shipped since
     `87221a2`, `docs/status.md:1418-1444`); verify before exec.
  3. Drop the binary at `~/.local/bin/fono` (desktop) or
     `/usr/local/bin/fono` (server, re-exec via `sudo` if needed).
     The script picks desktop vs server with the **same probe** as
     the binary (Task A1) so script and binary agree.
  4. `exec` the new binary as `fono install`. All real logic
     (wizard, unit installation, `enable --now`, idempotency) lives
     in one place — the Rust binary — never in shell.

- [ ] Task C3. Re-running the one-liner against an already-installed
  host delegates to `fono update` instead of `fono install`, and
  prints a one-line `fono doctor` summary at the end. No
  "marker exists" error.

### D — Distro packages

- [ ] Task D1. Convert the user-systemd unit shipped by deb /
  PKGBUILD / SlackBuild (`packaging/debian/rules:18-19`,
  `packaging/aur/PKGBUILD:46`,
  `packaging/slackbuild/fono/doinst.sh`) to be **preset-enabled**:
  drop `90-fono.preset` under `/usr/lib/systemd/user-preset/`
  containing `enable fono.service`. Every user's first login then
  brings the unit up — no manual `systemctl --user enable`. Admin
  override is a higher-priority preset file.

- [ ] Task D2. Also ship the system unit
  (`/lib/systemd/system/fono.service`) **disabled by default** in
  the same packages, so an admin who wants the server lane runs
  `sudo systemctl enable --now fono.service` without needing
  `sudo fono install --server`.

- [ ] Task D3. Replace today's ad-hoc instructions in
  `packaging/slackbuild/fono/doinst.sh:12-25` with a single line:
  "Open a terminal and run `fono` once to finish setup." The
  notification path from Task B2 + the preset from Task D1 carry
  the rest.

### E — Tests + doctor row + idempotency

- [ ] Task E1. Extend the install unit tests at
  `crates/fono/src/install.rs:780-854` to lock the lane probe and
  the two `Layout` variants. New integration test at
  `crates/fono/tests/install_round_trip.rs` (mocking the `systemctl`
  call via an env-var stub) walks both lanes end-to-end against a
  `tempfile` HOME.

- [ ] Task E2. New `fono doctor` row reporting (a) active lane
  (read from marker), (b) `is-enabled` + `is-active` for the
  lane's unit, (c) whether `config.toml` exists. Extends the
  existing Install section from `docs/status.md:419`.

### F — Docs + changelog

- [ ] Task F1. Rewrite the `## First run` section of `README.md`
  (around `README.md:30`) to a single narrative: "After any install
  path, Fono runs the wizard and starts itself. That's it." Drop
  the explicit `fono setup` instruction except as a re-run path.

- [ ] Task F2. New ADR
  `docs/decisions/0026-first-run-autostart-lanes.md` recording the
  two-lane probe, the privilege-drop pattern for `sudo fono install`,
  and the script-defers-to-binary contract.

- [ ] Task F3. CHANGELOG `[Unreleased]` Added / Changed / Removed
  entries per AGENTS.md release rules; ROADMAP shipment row staged
  for the cut.

## Verification Criteria

- Fresh desktop user (GNOME / KDE / sway / i3) running
  `curl -fsSL https://fono.page/install | sh` from a terminal ends
  with: wizard completed, `systemctl --user is-active fono.service`
  → `active`, `systemctl --user is-enabled fono.service` →
  `enabled`, `fono doctor` reporting `lane=desktop`,
  `unit=active+enabled`, `config=present`.
- Same command on a headless box ends with:
  `systemctl is-active fono.service` → `active`, marker records
  `Server`, no wizard if no TTY (config defaulted + notification
  arming on next login).
- Same command on a graphical box where `systemd --user` is
  unavailable falls back to the XDG autostart entry, spawns
  `fono` as a child of the install shell, marker still records
  `Desktop`.
- `sudo apt install ./fono_*.deb` (and the PKGBUILD / SlackBuild
  equivalents) end with the user-systemd preset taking effect on
  next login, plus the daemon-startup notification if the user
  hits an unconfigured first launch.
- Re-running any install path against an already-installed,
  already-configured host is a no-op with a single-line summary.
- `cargo test -p fono`, `cargo clippy --workspace --all-targets
  -- -D warnings`, and the new `install_round_trip.rs` all green.

## Potential Risks and Mitigations

1. **`systemd --user` missing on the host.** Probe via
   `systemctl --user show-environment` exit status; fall back to
   the XDG `~/.config/autostart/fono.desktop` entry (same file we
   already ship) + spawn `fono` once in the foreground so the
   user gets a running daemon for *this* session. Marker still
   says `Desktop`; uninstall reverses exactly what was written.

2. **`sudo fono install` running the wizard as the invoking user
   leaks env / breaks DBus.** Drop privilege only for the wizard
   subprocess via `setuid(SUDO_UID)` / `setgid(SUDO_GID)`; pass
   `HOME`, `XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS` through
   explicitly. The install path itself stays root.

3. **Wizard blocks `curl | sh` in CI / Ansible / Docker
   provisioning.** Honour `FONO_NONINTERACTIVE=1` + TTY check;
   skip the wizard silently, default config + Critical notification
   on next interactive login.

4. **Two daemons running at once** (one from `systemd --user`,
   one from the foreground spawn during install). The foreground
   spawn only happens on the XDG-fallback sub-case; on the
   systemd-user path we `systemctl --user start`. The daemon's
   existing IPC-socket single-instance lock blocks duplicates
   either way.

5. **Off-repo install script drift.** Canonical source moves into
   the repo at `packaging/install.sh` (Task C1); fono.page becomes
   a publish target.

## Alternative Approaches

1. **Keep `--server` mandatory on headless boxes.** Rejected: the
   point of the probe is to remove a foot-gun. An admin who runs
   `curl | sh` on a server should still end with a running daemon;
   forcing them to know the flag exists is exactly the gap the user
   is reporting.

2. **One lane only — always `systemd --user`.** Simpler but
   excludes server / headless installs and breaks the existing
   `fono install --server` lane already shipped in v0.5.0.

3. **Always XDG autostart for desktop, never `systemd --user`.**
   What today's `fono install` (desktop) does. The user explicitly
   reported the resulting "daemon starts on next login, not now"
   feel as the problem; switching to `systemd --user` is what fixes
   it without an extra config knob.

4. **Make the install script run the wizard via piped shell.**
   Rejected — the wizard reads arrow keys + masked input
   (`crates/fono/src/wizard.rs:1900-1925`) and needs a real TTY.
   The script must `exec` the binary in the foreground shell.
