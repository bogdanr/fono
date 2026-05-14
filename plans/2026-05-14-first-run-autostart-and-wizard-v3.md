# First-run autostart + wizard unification

## Objective

After any supported install path — `curl https://fono.page/install | sh`,
`sudo fono install`, the deb / Arch / Slackware / Nix packages, or a
manual binary drop followed by `fono install` — Fono must, with at most
one keystroke of user input:

1. **Run the setup wizard** in the right user context.
2. **Start the daemon immediately**, no logout / re-login.
3. **Enable the daemon at boot or graphical-session start.**

### Install locations (FHS-aligned, **two paths only**)

| Source of the binary | Path | Owner of that path |
|---|---|---|
| Distro package (deb / AUR / Slackware) | `/usr/bin/fono` | package manager |
| Everything else (`fono install`, `curl \| sh`, manual `cp`) | `/usr/local/bin/fono` | sysadmin / installer |

No `~/.local/bin/fono`. No third path. The binary always lives
system-wide; only the **systemd unit** is per-lane:

- **Desktop lane** (graphical session present, non-root invocation)
  → user unit at `~/.config/systemd/user/fono.service`, enabled via
  `systemctl --user enable --now`. Daemon runs under the user's UID,
  with their `~/.config/fono` and DBus session, and comes back on
  every login. Binary stays in `/usr/local/bin/fono` (shared across
  all users on the box).
- **Server lane** (headless OR `--server` OR root without a graphical
  session) → system unit at `/lib/systemd/system/fono.service`, runs
  as the dedicated `fono` user, enabled via `systemctl enable --now`.
  Unchanged from today's `fono install --server`.

### Lane probe (one rule, no flag-juggling)

```
DISPLAY|WAYLAND_DISPLAY set  AND  --server not passed   →  Desktop
otherwise                                               →  Server
```

EUID does **not** pick the lane. `sudo` is required only to write
`/usr/local/bin/fono` and (on the server lane) `/lib/systemd/system/`;
the script and `fono install` re-exec via `sudo` for that single
step and drop back to the invoking user for the wizard.

Re-running an install is idempotent: matching marker + matching lane
+ existing `config.toml` short-circuits to a one-line "already
installed and configured" summary.

## Implementation Plan

### A — Lane selection + single binary path

- [ ] Task A1. Replace `Mode::{Desktop, Server}` in
  `crates/fono/src/install.rs:58-72` with `Lane::{Desktop, Server}`,
  picked by the probe above (re-using
  `fono_hotkey::is_graphical_session()` per `docs/status.md:179-183`).
  The `--server` flag stays as an explicit override. The marker
  field is serialised as `"desktop"` / `"server"`.

- [ ] Task A2. Keep `BIN_PATH = /usr/local/bin/fono` (`install.rs:42`)
  for **both** lanes. Drop the v2 idea of `~/.local/bin/fono` for
  desktop. Per-lane diverges only on:
  - unit location (`~/.config/systemd/user/fono.service` vs
    `/lib/systemd/system/fono.service`),
  - whether the system user `fono` is created (server only,
    unchanged from `install.rs:541-622`),
  - which `systemctl` invocation enables / starts the unit.

- [ ] Task A3. Both `fono install` lanes require root to write
  `/usr/local/bin/fono`. Behaviour when invoked without `sudo`:
  - **Desktop lane**: re-exec the command via `sudo` for the
    binary-and-marker write step, then drop privileges to the
    invoking user (`SUDO_UID` / `SUDO_GID`) for the user-unit drop,
    the wizard, and `systemctl --user enable --now`. A user who
    types `fono install` from a graphical terminal gets a sudo
    prompt once, no further questions.
  - **Server lane**: already requires root (today's contract); a
    `--server` flag from a non-root shell re-execs via `sudo`
    identically.
  The existing `require_root()` check at `install.rs:167-172` moves
  inside the binary-copy step rather than guarding the whole
  command, so the wizard can run unprivileged.

### B — Wizard hand-off

- [ ] Task B1. After binary + unit are in place, run the wizard
  inline against the invoking user's XDG home (TTY check via
  `io::IsTerminal`; honour `FONO_NONINTERACTIVE=1` for
  CI / Ansible / Docker). On the server lane the wizard still runs
  as the invoking user (not as the dedicated `fono` system user) so
  it can talk to that user's notification / tray bus; the resulting
  config is then copied to `/etc/fono/config.toml` by the install
  path so the system daemon reads it on first start. Privilege drop
  goes through `setuid(SUDO_UID)` / `setgid(SUDO_GID)` with `HOME`,
  `XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS` forwarded.

- [ ] Task B2. Promote the implicit-first-run silent default at
  `crates/fono/src/cli.rs:412-427` to additionally fire a one-shot
  `Critical`-urgency notification ("Fono needs setup — run
  `fono setup` from a terminal") through the existing
  `critical_notify` cascade cap. Covers users whose distro package
  landed the unit but who never ran the wizard.

- [ ] Task B3. `fono setup` calls a shared `enable_and_start`
  helper at the end that picks `systemctl --user enable --now` on
  the desktop lane and `systemctl enable --now` on the server lane,
  surfacing failures the same way `verify_service_running` does at
  `install.rs:282-327`. Re-running the wizard later still leaves
  the daemon enabled and running.

### C — One-liner install script

- [ ] Task C1. Move the canonical `https://fono.page/install`
  script into the repo at `packaging/install.sh` so it stops
  drifting silently. The release workflow uploads it alongside the
  binaries; the website hosts the published artefact. Step added to
  `docs/dev/release-checklist.md`.

- [ ] Task C2. Script flow, end to end:
  1. Detect arch / glibc, pick the CPU asset (variant swap-up is
     `fono update`'s job; script does not duplicate it).
  2. Download the asset + `.sha256` sidecar (shipped since
     `87221a2`, `docs/status.md:1418-1444`); verify before exec.
  3. `sudo install -m 0755 fono /usr/local/bin/fono` (re-exec via
     `sudo` if not already root). One path, no `BIN_DIR=` override
     to maintain — `/usr/local/bin/fono` is the only target.
  4. `exec /usr/local/bin/fono install` (the binary picks the lane
     and runs the wizard). All real logic lives in the binary; the
     shell script is a thin downloader.

- [ ] Task C3. Re-running the one-liner against an already-installed
  host delegates to `fono update` (which verifies the same sidecar
  + respects `fono_update::is_package_managed` on `/usr/bin/fono`),
  then prints a one-line `fono doctor` summary. No "marker exists"
  error.

### D — Distro packages

- [ ] Task D1. Distro packages keep `/usr/bin/fono` (FHS — package
  manager owns that path). `fono_update::is_package_managed` and
  `refuse_if_package_managed` (`install.rs:174-185`) already keep
  `fono install` and `fono update` from clobbering a
  distro-installed binary. No change needed there.

- [ ] Task D2. Convert the user-systemd unit shipped by deb /
  PKGBUILD / SlackBuild (`packaging/debian/rules:18-19`,
  `packaging/aur/PKGBUILD:46`,
  `packaging/slackbuild/fono/doinst.sh`) to be **preset-enabled**:
  drop `90-fono.preset` under `/usr/lib/systemd/user-preset/`
  containing `enable fono.service`. Every user's first login then
  brings the unit up without manual `systemctl --user enable`.

- [ ] Task D3. Distro packages also ship the system unit
  (`/lib/systemd/system/fono.service`) **disabled by default**, so
  an admin who wants the server lane runs
  `sudo systemctl enable --now fono.service` without needing
  `sudo fono install --server`. Both units shell out to
  `/usr/bin/fono` (the package-managed location).

- [ ] Task D4. Replace today's ad-hoc instructions at
  `packaging/slackbuild/fono/doinst.sh:12-25` with one line:
  "Open a terminal and run `fono` once to finish setup." The
  notification path from Task B2 + the preset from Task D2 carry
  the rest.

### E — Tests + doctor row + idempotency

- [ ] Task E1. Extend the install unit tests at
  `crates/fono/src/install.rs:780-854` to lock the lane probe and
  the privilege-drop boundary (the binary-copy step is the only
  privileged operation; everything else runs at `SUDO_UID`).

- [ ] Task E2. New integration test at
  `crates/fono/tests/install_round_trip.rs` mocking the
  `systemctl` call via an env-var stub — walks both lanes
  end-to-end against a `tempfile` HOME; asserts the binary lands at
  `/usr/local/bin/fono`, the right unit is written, marker shape is
  correct.

- [ ] Task E3. New `fono doctor` row reporting (a) active lane from
  the marker, (b) `is-enabled` + `is-active` for the lane's unit,
  (c) `which fono` showing whether the running binary is
  distro-managed (`/usr/bin/`) or self-installed (`/usr/local/bin/`),
  (d) whether `config.toml` exists. Extends the existing Install
  section from `docs/status.md:419`.

### F — Docs + changelog

- [ ] Task F1. Rewrite the `## First run` section of `README.md`
  (around `README.md:30`) to one paragraph: "After any install path,
  Fono runs the wizard and starts itself. That's it." Drop the
  `BIN_DIR=` example — only one path now.

- [ ] Task F2. New ADR
  `docs/decisions/0026-first-run-autostart-lanes.md` recording the
  two-path FHS split (`/usr/bin` vs `/usr/local/bin`), the two-lane
  unit decision, the privilege-drop boundary, and the
  script-defers-to-binary contract.

- [ ] Task F3. CHANGELOG `[Unreleased]` Added / Changed / Removed
  entries (note the **Removed**: `BIN_DIR=` env-var on the install
  script, since there's now only one canonical path); ROADMAP
  shipment row staged.

## Verification Criteria

- Fresh desktop user running `curl -fsSL https://fono.page/install
  | sh` from a terminal ends with: binary at `/usr/local/bin/fono`,
  wizard completed, `systemctl --user is-active fono.service` →
  `active`, `systemctl --user is-enabled fono.service` → `enabled`,
  `fono doctor` reporting `lane=desktop`, `binary=/usr/local/bin/fono
  (self-installed)`, `unit=active+enabled`, `config=present`.
- Same command on a headless box ends with: binary at
  `/usr/local/bin/fono`, system unit active, marker records
  `Server`, wizard skipped if no TTY.
- `sudo apt install ./fono_*.deb` ends with binary at `/usr/bin/fono`
  (package-managed), user-systemd preset enabling the unit on next
  login. `fono doctor` reports `binary=/usr/bin/fono
  (package-managed)`.
- A user who runs the one-liner against a host that already has the
  `.deb` installed gets a graceful "package-managed; update through
  your distro" message and no `/usr/local/bin/fono` written (existing
  `refuse_if_package_managed` contract preserved).
- Re-running any install path against a configured host is a no-op.
- `cargo test -p fono`, `cargo clippy --workspace --all-targets
  -- -D warnings`, and the new `install_round_trip.rs` all green.

## Potential Risks and Mitigations

1. **`systemd --user` missing on the desktop host.** Probe via
   `systemctl --user show-environment` exit status; fall back to
   the XDG `~/.config/autostart/fono.desktop` entry + spawn `fono`
   once in the foreground so the user gets a running daemon this
   session. Marker still records `Desktop`. The binary path stays
   `/usr/local/bin/fono` — fallback is unit-level only.

2. **Both `/usr/bin/fono` and `/usr/local/bin/fono` present on the
   same host** (distro install layered with a one-liner attempt).
   `$PATH` ordering normally picks `/usr/local/bin/fono` first,
   which is the user's intent. `fono doctor` calls this out
   explicitly in Task E3 so the situation is visible. The
   `refuse_if_package_managed` check prevents the second copy from
   landing in the first place when the install script is honest.

3. **`sudo fono install` running the wizard as the invoking user
   leaks env / breaks DBus.** Privilege drop via `setuid(SUDO_UID)`
   / `setgid(SUDO_GID)`; pass `HOME`, `XDG_RUNTIME_DIR`,
   `DBUS_SESSION_BUS_ADDRESS` through explicitly. The install path
   itself stays root.

4. **Wizard blocks `curl | sh` in CI / Ansible / Docker.** Honour
   `FONO_NONINTERACTIVE=1` + TTY check; skip the wizard silently;
   the daemon-startup notification (Task B2) fires on the user's
   next interactive login.

5. **Removing `BIN_DIR=` from the install script is a soft
   breaking change.** A handful of users may have scripted around
   `BIN_DIR=~/.local/bin curl … | sh`. Mitigation: print a
   one-line "BIN_DIR ignored; binary always installs to
   /usr/local/bin/fono" warning when the env-var is set, then
   proceed. Document in CHANGELOG Removed.

6. **Off-repo install script drift.** Canonical source moves into
   `packaging/install.sh` (Task C1); fono.page becomes a publish
   target, not a hand-edited file.

## Alternative Approaches

1. **Keep `~/.local/bin/fono` for non-root desktop installs.**
   Avoids the `sudo` prompt during the one-liner, at the cost of
   three install paths, `$PATH`-ordering surprises, and an extra
   `Layout` variant in `install.rs`. Rejected — the one-line sudo
   prompt during install is a known, well-understood Unix idiom;
   path fragmentation is not.

2. **Single path for *everything*, including distro packages
   (`/usr/local/bin/fono` from the deb / PKGBUILD).** Violates FHS
   (`/usr/local/` is reserved for sysadmin-installed software, not
   package-manager-owned files). Some distros (Debian) explicitly
   reject packages that write to `/usr/local`. Rejected.

3. **Always require `sudo fono install` separately after the
   one-liner.** Today's behaviour. Rejected — the user reported
   this exact split as the friction point.
