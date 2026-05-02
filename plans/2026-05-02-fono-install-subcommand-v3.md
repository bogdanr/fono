# Fono — `fono install` (system-wide; desktop or server)

## Objective

Give users who downloaded the prebuilt `fono` binary a one-command,
system-wide install with two clearly-separated modes:

```sh
sudo ./fono-vX.Y.Z-x86_64 install            # desktop mode
sudo ./fono-vX.Y.Z-x86_64 install --server   # headless server mode
```

Each mode installs exactly the artefacts its role needs and nothing
more. Symmetric `sudo fono uninstall` reads the install marker and
reverses precisely what was put down. The only other flag is
`--dry-run`.

## Why two modes (and why a flag rather than two subcommands)

Fono is one binary that runtime-gates the tray on
`DISPLAY` / `WAYLAND_DISPLAY` (`crates/fono/src/daemon.rs:232-247`,
status entry 2026-04-30 *one-binary-many-roles contract*). The two
roles differ only in **how the daemon is launched**:

| Role | Triggered by | What that needs on disk |
|---|---|---|
| **Desktop** (default) — auto on graphical login | The DE / WM at session start (XDG autostart) | `/etc/xdg/autostart/fono.desktop`, menu desktop entry, icon, completions |
| **Server** (`--server`) — auto at boot, headless | `systemd` at boot, dedicated `fono` system user | `/lib/systemd/system/fono.service` (enabled + started), `fono` system user, completions |

Splitting on a flag rather than collapsing into one install yields:

- **Desktop hosts get no dead system unit.** A disabled
  `fono.service` lying around on every workstation is
  configuration noise; admins running `systemctl list-unit-files`
  see something they didn't ask for. v2's "ship both, please don't
  enable the server one" contract is replaced by "ship only what
  this host needs".
- **Server hosts get no XDG autostart entry.** Headless servers
  often have no `/etc/xdg/autostart` consumer at all, but on
  hosts that do install one later (e.g. a server that grows a
  graphical maintenance session) the leftover entry would be
  surprising.
- **No `fono` service user on desktops.** The dedicated user is
  only created in server mode, since the system unit is the only
  consumer.
- **`--server` is the explicit opt-in.** Admins who want the
  server lane name it; nobody enables it accidentally.

A single subcommand with a flag (rather than `fono install-server` /
`fono install-desktop`) keeps the surface small and matches the
mental model "install Fono on this host, headless if `--server`".

## Implementation plan

### Phase 1 — Embed packaging assets

- [ ] Task 1.1. Move the canonical desktop entry, SVG icon, and the
  newly-authored system unit into a packaging-neutral
  `packaging/assets/` directory (`fono.desktop`, `fono.svg`,
  `fono.service`). Update the four distro recipes to read from
  there: `packaging/slackbuild/fono/fono.SlackBuild`,
  `packaging/debian/rules`, `packaging/aur/PKGBUILD`,
  `packaging/nix/flake.nix`. Delete the legacy per-user
  `packaging/systemd/fono.service` (replaced by the new
  system-scope unit; the per-user lane is dropped — see v2 plan
  rationale).
- [ ] Task 1.2. New module `crates/fono/src/install/assets.rs`
  exposes the three assets via `include_str!` / `include_bytes!`
  pointing at `packaging/assets/`. Path-relative `include_*!`
  fails the build at compile time on rename, so Phase 1 is
  self-checking.

### Phase 2 — CLI surface

- [ ] Task 2.1. Add to `crates/fono/src/cli.rs::Cmd`:
  - `Install { #[arg(long)] server: bool, #[arg(long)] dry_run: bool }`.
  - `Uninstall { #[arg(long)] dry_run: bool }`.

  No other flags. Doc comment on `Install` states that the command
  is system-wide and requires root; `--server` switches from the
  default desktop layout to the headless layout; `--dry-run`
  prints the planned actions without touching disk.
  `Uninstall` infers the mode from the install marker (Task 3.7);
  it does not need a `--server` flag.
- [ ] Task 2.2. Module
  `crates/fono/src/install/{mod.rs,actions.rs,desktop.rs,server.rs}`:
  - `mod.rs` — exports `run_install(server, dry_run)` and
    `run_uninstall(dry_run)`. Both first run the pre-flight in
    `actions.rs` (root check + `is_package_managed` refusal +
    marker presence/absence check), then dispatch to the
    appropriate per-mode helper.
  - `actions.rs` — `write_atomic(path, bytes, mode)` mirroring
    the temp-file-then-rename pattern in
    `crates/fono-update/src/lib.rs:442-537`; root-check helper;
    `is_package_managed` re-export; install-marker
    serialisation/parse.
  - `desktop.rs` — Phase 3 actions for desktop mode.
  - `server.rs` — Phase 4 actions for server mode.
- [ ] Task 2.3. Pre-flight checks (run unconditionally):
  - `geteuid() == 0` — fail with
    "this command must be run as root: `sudo fono install`".
  - `current_exe()` not under `/usr/bin/`, `/bin/`, `/usr/sbin/`
    (reuse `fono_update::is_package_managed`,
    `crates/fono-update/src/lib.rs:374-380`); on hit, refuse
    with "your distro's package manager already owns this
    binary".
  - On `install`: refuse if a marker already exists for a
    *different* mode ("desktop install detected; run `fono
    uninstall` first if you want to switch to server mode").
    Re-running the same mode is idempotent — overwrites in place.
  - On `uninstall`: refuse if no marker exists ("no install
    marker found at /usr/local/share/fono/install_marker.toml;
    nothing to uninstall").

### Phase 3 — Desktop mode actions

Default. No `--server`.

- [ ] Task 3.1. **Binary** — `current_exe()` →
  `/usr/local/bin/fono`, mode `0755`, atomic. Skip the copy if
  the running binary is already at the destination (idempotent
  re-install).
- [ ] Task 3.2. **Desktop entries.** `assets::DESKTOP` written to
  both `/usr/share/applications/fono.desktop` (menu) and
  `/etc/xdg/autostart/fono.desktop` (graphical autostart for
  GNOME, KDE, XFCE, LXQt, MATE, Cinnamon, Budgie, and i3/sway/
  Hyprland with `dex`). Both copies use `Exec=fono`; the
  autostart copy adds `X-GNOME-Autostart-enabled=true`.
  Best-effort `update-desktop-database -q
  /usr/share/applications` after both writes.
- [ ] Task 3.3. **Icon.** `assets::ICON_SVG` →
  `/usr/share/icons/hicolor/scalable/apps/fono.svg`. Best-effort
  `gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor`.
- [ ] Task 3.4. **Shell completions.** Generated by spawning
  `/usr/local/bin/fono completions <shell>` and writing to:
  - `/usr/share/bash-completion/completions/fono`
  - `/usr/share/zsh/site-functions/_fono`
  - `/usr/share/fish/vendor_completions.d/fono.fish`

  Per-shell: skip silently when the parent directory doesn't
  exist (the shell isn't installed system-wide).
- [ ] Task 3.5. **Install marker.** Write
  `/usr/local/share/fono/install_marker.toml` containing
  `mode = "desktop"`, version, ISO-8601 timestamp, and the
  literal list of files Tasks 3.1–3.4 created. `fono uninstall`
  reads this list and removes exactly those paths.
- [ ] Task 3.6. **Final summary.** Print to stdout: install
  succeeded, the daemon will start automatically on next
  graphical login, and (if `$DISPLAY`/`$WAYLAND_DISPLAY` is set
  in the invoking shell) `fono` can be started immediately
  without re-login.

### Phase 4 — Server mode actions (`--server`)

- [ ] Task 4.1. **Service user.** Create the `fono` system user
  and group when missing: `useradd --system --no-create-home
  --shell /usr/sbin/nologin --user-group fono`. Idempotent —
  if the user already exists, leave it alone and log a debug
  line. If `useradd` is unavailable (rare; minimal containers),
  bail with a clear error rather than silently producing a unit
  that will fail to start.
- [ ] Task 4.2. **Binary** — same as Task 3.1.
- [ ] Task 4.3. **System systemd unit.** `assets::SYSTEMD_SYSTEM_UNIT`
  → `/lib/systemd/system/fono.service`. Authored fresh in Task
  1.1's `packaging/assets/fono.service`:

  ```
  [Unit]
  Description=Fono voice dictation daemon (server / headless mode)
  Documentation=https://github.com/bogdanr/fono
  After=network-online.target sound.target
  Wants=network-online.target

  [Service]
  Type=simple
  User=fono
  Group=fono
  ExecStart=/usr/local/bin/fono daemon --no-tray
  Restart=on-failure
  RestartSec=3

  # Hardening — server lane never touches a graphical session
  # or the user's home dir.
  NoNewPrivileges=true
  ProtectSystem=strict
  ProtectHome=true
  PrivateTmp=true
  ProtectKernelTunables=true
  ProtectControlGroups=true
  RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
  StateDirectory=fono
  CacheDirectory=fono
  ConfigurationDirectory=fono

  [Install]
  WantedBy=multi-user.target
  ```

  After write: `systemctl daemon-reload`. Then `systemctl
  enable --now fono.service` — the admin asked for server mode
  by passing `--server`, so starting the unit is the expected
  behaviour. If `systemctl` is missing (Void / Artix / Alpine),
  log a clear "systemd not detected; the unit file is in place
  but you'll need to wire it into your init system manually"
  and continue.
- [ ] Task 4.4. **Shell completions** — same as Task 3.4. (Server
  admins ssh in and tab-complete just like everyone else.)
- [ ] Task 4.5. **Install marker** — same shape as Task 3.5 but
  with `mode = "server"` and the file list reflecting Tasks
  4.1–4.4. The marker also records that `useradd` ran (so
  uninstall knows whether to attempt user removal).
- [ ] Task 4.6. **Final summary.** Print: install succeeded, the
  unit is enabled and running, daemon status visible via
  `systemctl status fono.service`, configuration goes in
  `/etc/fono/config.toml` (the unit's `ConfigurationDirectory=fono`
  line creates `/etc/fono/` for us), Wyoming bind etc. documented
  in `docs/providers.md`.

### Phase 5 — Uninstall

- [ ] Task 5.1. Read the install marker. If `mode = "server"` and
  `systemctl is-active fono.service` is true, run `systemctl
  disable --now fono.service` first. After unit removal, run
  `systemctl daemon-reload` (best-effort). For desktop mode,
  refresh `update-desktop-database` and
  `gtk-update-icon-cache` after removing the relevant files.
- [ ] Task 5.2. Remove every file the marker lists, in reverse
  dependency order. Never glob; never remove anything not in
  the list — protects against removing a `fono` placed at the
  same path by other means (e.g. a later distro package).
- [ ] Task 5.3. Server mode only: remove the `fono` system user
  and group via `userdel fono` when no leftover state remains
  under `/var/lib/fono` / `/var/cache/fono` / `/etc/fono`. If
  state is present, preserve the user so an operator can
  re-install without losing it. Best-effort.
- [ ] Task 5.4. **User data is never touched.** The system
  installer has no business deleting per-user XDG dirs
  (`~/.config/fono`, `~/.local/share/fono`, `~/.cache/fono`,
  `~/.local/state/fono`) — they belong to the user, not to the
  install. The desktop-mode install summary states this
  explicitly.
- [ ] Task 5.5. Remove `/usr/local/share/fono/install_marker.toml`
  last. If the directory is then empty, `rmdir` it
  (best-effort).

### Phase 6 — `fono doctor` integration + tests + docs

- [ ] Task 6.1. `crates/fono/src/doctor.rs` reads the install
  marker and reports one of four states: "self-installed
  (desktop)", "self-installed (server)", "package-managed", or
  "ad-hoc on PATH". Helps users diagnose why an update or
  uninstall refused, and helps server admins confirm the role.
- [ ] Task 6.2. Unit tests in `crates/fono/src/install/`:
  - Layout helpers return the expected paths for each mode.
  - `--dry-run` for both modes lists every expected target
    path and writes nothing (use a tempdir-rooted layout
    helper).
  - `is_package_managed` refusal short-circuits before any
    writes.
  - Wrong-mode-marker refusal: install --server when a
    desktop marker already exists fails with the documented
    message; same for the inverse.
  - Marker round-trip parses what it serialises, including
    the file list.
  - Idempotent re-install of the same mode rewrites in place
    without errors.
- [ ] Task 6.3. Integration tests:
  - `crates/fono/tests/install_desktop_dry_run.rs` — drives
    `Cmd::Install { server: false, dry_run: true }` against a
    tempdir-rooted layout and asserts the report.
  - `crates/fono/tests/install_server_dry_run.rs` — same for
    `--server`.
- [ ] Task 6.4. Update `README.md` "Install" with the two
  one-liners and a one-paragraph explanation of when each is
  appropriate (desktop = workstation, laptop, anything with
  a logged-in user; server = LAN inference host, Wyoming /
  REST endpoint, anything you'd `ssh` into). Update
  `docs/dev/update-qa.md` with parallel install scenarios:
  desktop dry-run, server dry-run, root check, package-managed
  refusal, mode-switch refusal, idempotent re-install,
  uninstall on each mode, no-marker uninstall refusal.
- [ ] Task 6.5. `CHANGELOG.md [Unreleased]` — `Added: fono
  install (system-wide) with --server flag for headless
  deployments; fono uninstall reverses the install via the
  install marker`. Per AGENTS.md release contract, the next
  tag must move this into the versioned section and add a
  "One-command install" tile to `ROADMAP.md` "Recently
  shipped".
- [ ] Task 6.6. ADR `docs/decisions/0023-self-installer.md`:
  (a) why a self-installer at all (single-binary parity with
  `fono update`, distro-agnostic); (b) why system-wide only
  (rejects v1's user-vs-system split as flag bloat for
  ambiguous gain); (c) why `--server` flag rather than
  shipping both artefacts on every install (no dead unit on
  desktops, no dead autostart entry on servers, no dedicated
  service user where it isn't needed); (d) why `--server`
  enables the unit immediately (admin opted in by passing the
  flag — surprising-restraint is worse UX here than
  do-what-I-said); (e) why uninstall refuses without a marker
  (won't delete files we didn't write).

## Verification criteria

- `sudo fono install --dry-run` lists every desktop-mode target
  path (binary + 2 desktop entries + icon + 3 completions +
  marker) and writes nothing.
- `sudo fono install --server --dry-run` lists every server-mode
  target path (binary + system unit + 3 completions + marker;
  notes the `useradd fono` and `systemctl enable --now`
  side effects) and writes nothing.
- `fono install` without root exits non-zero with a clear
  message and writes nothing.
- `sudo fono install` against a `/usr/bin/fono` package-managed
  source binary refuses cleanly before any writes.
- After `sudo fono install` (desktop) on a clean host:
  - `command -v fono` resolves to `/usr/local/bin/fono` in a
    fresh shell.
  - The Fono entry appears in the application launcher.
  - On next graphical login the daemon comes up automatically;
    no `systemctl` step required.
  - `systemctl list-unit-files | grep fono` returns nothing —
    no server-lane noise on desktop hosts.
- After `sudo fono install --server` on a clean host:
  - `systemctl status fono.service` shows the unit `enabled`
    and `active (running)`.
  - The daemon runs as `fono`, no tray.
  - `getent passwd fono` returns the system user.
  - No `/etc/xdg/autostart/fono.desktop` exists — no desktop
    autostart on a server.
- `sudo fono install` after a previous `--server` install fails
  with the mode-switch refusal message; same for the inverse.
- `sudo fono uninstall` on either mode removes every file the
  marker lists, leaves user data intact, removes the `fono`
  user only on server mode and only when state is empty, and
  is idempotent on re-run (second run hits the no-marker
  refusal).
- `cargo test -p fono install::` green; `cargo clippy
  --workspace --all-targets -- -D warnings` clean;
  `tests/check.sh` matrix green; the `size-budget` CI gate
  unchanged (embedded SVG + two text snippets cost a few KB
  against the 20 MiB ceiling).
- All four distro packaging recipes still build against the
  new `packaging/assets/` location and produce identical
  artefacts.

## Potential risks and mitigations

1. **Mode confusion.** A desktop user runs `--server` by mistake
   and ends up with a headless service running as a different
   user. Mitigation: the desktop-mode summary is the default and
   the README's first install snippet; `--server` documentation
   makes it explicit; `fono doctor` reports the mode prominently
   so a misconfigured workstation surfaces fast.
2. **Embedded asset drift vs distro copies.**
   Mitigation: single source of truth at `packaging/assets/`;
   `include_str!` paths fail at compile time on rename; CI build
   matrix already exercises the canonical paths.
3. **No-systemd hosts.** `--server` mode needs `systemctl` to be
   useful. Mitigation: detect systemd presence; if absent, write
   the unit file but skip `daemon-reload`/`enable --now` with a
   clear info log naming the missing tool. Admin can wire a
   parallel init script if they want.
4. **`/usr/local/bin` not on `$PATH`.** Almost universal; minimal
   containers occasionally strip it. Mitigation: install-time
   check; if missing, print the literal one-line export to add to
   the global profile. No automatic edits to system files we
   didn't write.
5. **Desktop sessions ignoring `/etc/xdg/autostart`.** Rare
   (mostly bespoke window managers configured without `dex`).
   Mitigation: install summary tells the user to add `fono`
   to their WM's autostart config or run from a terminal. The
   runtime gate handles the headless edge gracefully — Fono
   doesn't crash, it just doesn't start.
6. **Live binary swap during desktop install.** A user's desktop
   `fono` daemon is running while `sudo fono install` overwrites
   `/usr/local/bin/fono`; the running process is inode-pinned to
   the old binary until the next graphical login. Mitigation:
   install summary suggests `pkill -u $USER fono` (or just log
   out and back in) to pick up the new binary. Same caveat as
   `fono update`.
7. **Live unit replace during server install.** `--server`
   running on a host that already has the service enabled.
   Mitigation: `systemctl daemon-reload` + `systemctl restart
   fono.service` after the unit write so the new binary path
   takes effect; idempotent re-install is the supported upgrade
   path.
8. **Service-user collision.** A different `fono` user already
   exists with non-system attributes. Mitigation: detect via
   `getent passwd fono`; if present, leave alone and log a
   warning; the install proceeds and the unit will use whatever
   `fono` user resolves at start time.

## Alternative approaches

1. **Two subcommands (`fono install-server` /
   `fono install-desktop`).** Larger surface; same outcome.
   Rejected — the flag is the natural axis (verb is "install",
   target is "this machine", role is the modifier).
2. **One install that ships both artefacts; admin chooses what
   to enable.** v2 plan; rejected by user feedback and on its
   merits (dead config on hosts that don't need it; no
   dedicated service user on desktops where it serves nothing).
3. **`--server` writes the unit but leaves it disabled.**
   Surprising restraint after the admin already named the role
   they want. Rejected — `enable --now` matches the explicit
   intent of passing the flag.
4. **Per-user systemd unit (the original
   `packaging/systemd/fono.service`).** Rejected: required a
   manual `systemctl --user enable` step everyone forgot,
   doesn't work without lingering for tray-less sessions, and
   is strictly worse than XDG autostart for the desktop role.
5. **`curl | sudo bash` install script alongside / instead of
   the subcommand.** Requires network at install time and is
   asymmetric with `fono update`. Rejected; the binary already
   in the user's hand is the source of truth.
