# Fono — `fono install` (simple, system-wide)

## Objective

Give users who downloaded the prebuilt `fono` binary a one-command path to a
fully integrated **system-wide** install:

```sh
sudo ./fono-vX.Y.Z-x86_64 install
```

…produces a working `fono` on `$PATH`, a desktop launcher, an icon, autostart
under any graphical session, and a system-wide systemd unit for headless /
server use. Symmetric `sudo fono uninstall` reverses it. The only flag is
`--dry-run`. Everything else is full-blown by default.

## How Fono starts on the two roles

Fono is a single binary that the daemon already runtime-gates on
`DISPLAY` / `WAYLAND_DISPLAY` (`crates/fono/src/daemon.rs:232-247` —
tray spawn skipped on headless hosts; status entry 2026-04-30 *"Default
Linux audio…"* and the broader one-binary-many-roles contract). So the
**same** `fono` binary covers both roles; only the *launch trigger*
differs:

| Role | Triggered by | Mechanism installed by `fono install` |
|---|---|---|
| Desktop user — auto on login to graphical session | The user's DE / WM at session start | `/etc/xdg/autostart/fono.desktop` with `Exec=fono` |
| Headless server — auto on boot, no graphical session | systemd at boot | `/lib/systemd/system/fono.service` running as a dedicated `fono` system user, **disabled by default**; admin enables with `sudo systemctl enable --now fono.service` |

Why both, and why this split:

- **`/etc/xdg/autostart/fono.desktop`** is honoured by every XDG-compliant
  desktop (GNOME, KDE, XFCE, LXQt, MATE, Cinnamon, Budgie) and by tiling
  WMs that run `dex` / `dex-autostart` (i3, sway, Hyprland with the
  user's standard autostart helper). No `systemctl --user enable` step.
  When `DISPLAY` / `WAYLAND_DISPLAY` is set, Fono lights the tray + global
  hotkey + overlay; when it isn't, the autostart entry simply isn't
  triggered. This is the path 99 % of desktop users will take.
- **`/lib/systemd/system/fono.service`** is the headless / server lane
  (Wyoming server, future REST/MCP, LAN inference role from the ROADMAP
  *Network inference* tile). Daemon-launched by systemd as the `fono`
  service user with no DBus session and no `DISPLAY`, so the tray /
  hotkey / overlay all stay off automatically — the same binary serves
  network requests only. Disabled at install time so a desktop user
  doesn't end up with two competing daemons; the admin opts in
  explicitly.
- **Conflict avoidance.** The XDG autostart entry only fires inside a
  graphical session for a logged-in user; the system unit only fires
  at boot under the dedicated `fono` user. They cannot both be running
  for the same user simultaneously: a desktop user runs the
  per-session autostart copy, a server has no graphical session at all.
  Documented in the install summary the binary prints at the end of
  `fono install`.

The previous **per-user systemd unit** approach
(`packaging/systemd/fono.service`,
`packaging/slackbuild/fono/fono.service` — `WantedBy=default.target`,
`%h/...` paths) is **dropped** by this plan. It required
`systemctl --user enable fono.service` after install — a manual step
that everyone forgot, and which depended on lingering being enabled.
The XDG autostart route is zero-config for desktop users, and the
system unit is simpler for server users (admins already know
`systemctl enable`).

## Implementation plan

### Phase 1 — Embed packaging assets in the binary

- [ ] Task 1.1. Move the desktop entry, SVG icon, and the new
  system-wide systemd unit into a packaging-neutral
  `packaging/assets/` directory: `fono.desktop`, `fono.svg`, and a
  newly-authored `fono.service` (system unit; details in Task 3.3).
  Update the existing distro recipes to read from this new
  location: `packaging/slackbuild/fono/fono.SlackBuild`,
  `packaging/debian/rules`, `packaging/aur/PKGBUILD`,
  `packaging/nix/flake.nix`, plus the legacy
  `packaging/systemd/fono.service` (deleted). Rationale: single
  source of truth — embedded copy and distro-packaged copy can't
  drift.
- [ ] Task 1.2. New module `crates/fono/src/install/assets.rs`
  exposes the three asset blobs as `pub const` via
  `include_str!("../../../../packaging/assets/fono.desktop")` /
  `include_bytes!(... fono.svg)` /
  `include_str!(... fono.service)`. Path-relative `include_*!`
  fails the build if Task 1.1 missed any rename, which is
  exactly the safety net we want.

### Phase 2 — CLI surface (minimal)

- [ ] Task 2.1. Add two variants to
  `crates/fono/src/cli.rs::Cmd`:
  - `Install { #[arg(long)] dry_run: bool }`.
  - `Uninstall { #[arg(long)] dry_run: bool }`.

  No other flags. The doc comment on `Install` states that the
  command is system-wide, requires root, and that `--dry-run`
  prints the planned actions without writing anything.
- [ ] Task 2.2. New module
  `crates/fono/src/install/{mod.rs,actions.rs}` exporting
  `run_install(dry_run: bool) -> Result<()>` and
  `run_uninstall(dry_run: bool) -> Result<()>`. Both use one
  `write_atomic(path, bytes, mode)` helper that mirrors the
  temp-file-then-rename pattern in
  `crates/fono-update/src/lib.rs:442-537`, so a partial failure
  leaves the system unchanged.
- [ ] Task 2.3. Pre-flight checks at the top of
  `run_install` / `run_uninstall`:
  - `geteuid() == 0` — fail clearly with
    "this command must be run as root: `sudo fono install`".
  - The running binary is **not** package-managed at the source
    side: refuse if `current_exe()` resolves under `/usr/bin/`,
    `/bin/`, `/usr/sbin/` (reuse
    `fono_update::is_package_managed`,
    `crates/fono-update/src/lib.rs:374-380`), with the message
    "your distro's package manager already owns this binary;
    update through it instead".

### Phase 3 — Fixed install layout

System-wide paths only. No scope flag, no overrides.

- [ ] Task 3.1. **Binary** — `current_exe()` →
  `/usr/local/bin/fono`, mode `0755`, atomic. `/usr/local/bin`
  is the FHS-standard "locally installed executables" location
  and is also where `fono update` already self-replaces by
  default (`crates/fono-update/src/lib.rs:376-380` comment).
- [ ] Task 3.2. **Desktop entry** —
  `assets::DESKTOP` → `/usr/share/applications/fono.desktop`
  (visible in app launchers / menus) **and**
  `/etc/xdg/autostart/fono.desktop` (so it autostarts under any
  XDG-compliant desktop session). The autostart copy adds
  `X-GNOME-Autostart-enabled=true` and is otherwise byte-identical
  to the menu copy — both reference `Exec=fono`, which Task 3.1
  has just put on `$PATH`. After both writes, run
  `update-desktop-database -q /usr/share/applications` (best-effort).
- [ ] Task 3.3. **Icon** — `assets::ICON_SVG` →
  `/usr/share/icons/hicolor/scalable/apps/fono.svg`. Best-effort
  `gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor`.
- [ ] Task 3.4. **System systemd unit (server lane)** —
  `assets::SYSTEMD_SYSTEM_UNIT` →
  `/lib/systemd/system/fono.service`. The unit, authored fresh
  in Task 1.1's `packaging/assets/fono.service`, is:

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

  # Hardening — this lane never needs a graphical session, audio-out,
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

  After the write, run `systemctl daemon-reload` (best-effort —
  skipped silently when systemd is not present, e.g. Void / Artix
  / Alpine).

  The unit is **not** enabled or started by `fono install`. The
  install summary tells server admins to run
  `sudo systemctl enable --now fono.service` themselves once
  they've populated `/etc/fono/config.toml` and any required
  secrets. Desktop users ignore it; their daemon comes up via
  the XDG autostart in Task 3.2.
- [ ] Task 3.5. **Service user** — create the dedicated `fono`
  system user/group when missing (`useradd --system --no-create-home
  --shell /usr/sbin/nologin --user-group fono`, with a
  `getpwnam`-equivalent guard so re-runs are idempotent). The
  user is created even on desktop-only installs because
  uninstalling on a server later should remove the same user
  the install created. Best-effort: if `useradd` is missing
  (uncommon but possible on minimal containers), log a warning
  and continue — the system unit will fail at `systemctl start`
  time with a clear "no such user" error which is acceptable
  for that edge case.
- [ ] Task 3.6. **Shell completions** — generate by spawning the
  freshly-installed `/usr/local/bin/fono completions <shell>`
  (existing subcommand, `crates/fono/src/cli.rs:230-234`) and
  write to:
  - `/usr/share/bash-completion/completions/fono`
  - `/usr/share/zsh/site-functions/_fono`
  - `/usr/share/fish/vendor_completions.d/fono.fish`

  Per shell: skip silently when the parent directory doesn't
  exist (the shell isn't installed system-wide).
- [ ] Task 3.7. **Install marker** — write a small TOML
  `/usr/local/share/fono/install_marker.toml` with version,
  ISO-8601 timestamp, and the literal list of files Task 3.1–3.6
  created. `fono uninstall` reads this marker and removes
  exactly those files, never globs. Refuses to run if the marker
  is missing — protects against removing a `fono` placed at the
  same path by other means.

### Phase 4 — Uninstall

- [ ] Task 4.1. `fono uninstall` reads the install marker from
  Task 3.7 and removes every file it lists, in reverse order:
  completions → systemd unit (with `systemctl daemon-reload`
  after) → icon (with cache refresh) → desktop entries (both
  copies, with `update-desktop-database`) → binary → marker.
  If `systemctl is-active fono.service` is true, run
  `systemctl disable --now fono.service` first.
- [ ] Task 4.2. Remove the `fono` system user/group only when
  uninstall finds no leftover state under `/var/lib/fono` /
  `/var/cache/fono` / `/etc/fono` (best-effort; preserve the
  user when state is present so an operator can re-install
  without losing it).
- [ ] Task 4.3. **User data is never touched.** Per-user XDG
  dirs (`~/.config/fono`, `~/.local/share/fono`,
  `~/.cache/fono`, `~/.local/state/fono`) belong to the user,
  not to the system installer, and a system-wide uninstall has
  no business deleting them. The install summary at the end of
  `fono install` documents this contract explicitly.

### Phase 5 — `fono doctor` integration + tests + docs

- [ ] Task 5.1. `crates/fono/src/doctor.rs` learns to read the
  install marker and report one of three states: "self-installed
  via `fono install`" (marker present), "package-managed"
  (`current_exe()` matches `is_package_managed`), or "ad-hoc"
  (neither). Helps users diagnose why an update or uninstall
  refused.
- [ ] Task 5.2. Unit tests in `crates/fono/src/install/`:
  marker round-trip serialisation, `--dry-run` reports every
  expected target path and writes nothing (use a tempdir-rooted
  layout helper that mirrors the production constants), the
  `is_package_managed` refusal short-circuits before any writes,
  the systemd-unit `ExecStart=` substitution rewrites correctly.
- [ ] Task 5.3. Update `README.md` "Install" section: the new
  one-liner is the recommended path for users who skip distro
  packaging, with a one-paragraph explanation of the desktop
  vs. server autostart behaviour. Update
  `docs/dev/update-qa.md` with a parallel
  `install` / `uninstall` scenario list (run as root, run as
  non-root expecting the clear error, dry-run, package-managed
  refusal, missing marker refusal, idempotent re-install).
- [ ] Task 5.4. `CHANGELOG.md [Unreleased]` — `Added: fono
  install / fono uninstall (system-wide self-installer; XDG
  autostart for desktop sessions, opt-in system service for
  servers)`. The next release tag must move this entry into the
  versioned section per the AGENTS.md release contract and add
  a "One-command install" tile to `ROADMAP.md` "Recently
  shipped".
- [ ] Task 5.5. ADR
  `docs/decisions/0023-self-installer.md` capturing: (a) why a
  self-installer at all (single-binary parity with `fono
  update`, distro-package-agnostic); (b) why system-wide only
  (avoids the user-vs-system-vs-bindir flag matrix that bloated
  v1's draft; matches the user feedback); (c) why XDG autostart
  for desktop instead of a per-user systemd unit (zero
  follow-up step; works on every XDG-compliant DE and on
  i3/sway with `dex`); (d) why the system unit ships disabled
  (server admins opt in once; desktop users never trip over it).

## Verification criteria

- `sudo fono install --dry-run` on a clean host lists every
  expected target path (binary + 2 desktop entries + icon +
  system unit + 3 completions + marker) and writes nothing.
- `fono install` (no `sudo`) exits non-zero with a clear
  "must be run as root" message and writes nothing.
- `sudo fono install` against a `/usr/bin/fono` package-managed
  source binary refuses cleanly before any writes.
- `sudo fono install` on a clean host produces:
  - `command -v fono` → `/usr/local/bin/fono` in a fresh shell.
  - The Fono entry in the application launcher (after
    `update-desktop-database`).
  - On next graphical login, the daemon comes up automatically;
    the tray icon appears (on hosts with an SNI watcher) without
    the user touching `systemctl`.
  - `systemctl status fono.service` shows the unit as installed
    but `inactive (dead)` and `disabled` — proving the server
    lane is opt-in.
  - On a headless host, after `sudo systemctl enable --now
    fono.service`, the daemon runs as user `fono`, no tray,
    Wyoming listener available when configured.
- `sudo fono uninstall` on a self-installed host removes every
  file the marker lists, leaves user data intact, and is
  idempotent on re-run.
- `sudo fono uninstall` on a host without an install marker
  refuses with a clear "no install marker found" message.
- `cargo test -p fono install::` green; `cargo clippy
  --workspace --all-targets -- -D warnings` clean;
  `tests/check.sh` matrix green; the `size-budget` CI gate
  unchanged (embedded SVG + two text snippets cost a few KB
  against the 20 MiB ceiling).
- All four distro packaging recipes still build against the
  new `packaging/assets/` location and produce identical
  artefacts to before the move.

## Potential risks and mitigations

1. **Embedded asset drift vs distro copies.**
   Mitigation: single source of truth at `packaging/assets/`;
   `include_str!` paths fail the build at compile time on
   rename — fast feedback. CI build matrix already exercises
   the canonical paths.
2. **Two daemons running.** A user installs system-wide on a
   workstation, the XDG autostart fires, and an admin also
   enables `fono.service`. They run as different users so
   the IPC sockets, history, and tray contexts don't actually
   collide; but it is wasteful. Mitigation: install summary
   explicitly states "do not enable `fono.service` on a
   workstation — autostart already handles it".
3. **No-systemd hosts (Void / Artix / Alpine).** No
   `systemctl daemon-reload` available. Mitigation: detect
   `systemctl --version` and skip the reload step with a
   single info log; install otherwise succeeds. The system
   unit file is still written so an admin who later switches
   init systems doesn't have to reinstall.
4. **Desktop sessions that ignore `/etc/xdg/autostart`.**
   Rare (mostly bespoke window managers configured without
   `dex`). Mitigation: the install summary tells the user to
   either add `fono` to their WM's autostart config or run
   `fono` from a terminal. The runtime gate already handles
   the headless edge gracefully.
5. **`/usr/local/bin` not on `$PATH`.** Almost universally
   present on Linux distros, but minimal containers sometimes
   strip it. Mitigation: install-time check; if missing, print
   the literal one-line export to add to the global profile.
   No automatic edits.
6. **Live binary swap.** `sudo fono install` while a desktop
   user's `fono` daemon is running leaves the running process
   inode-pinned to the old binary until the next session.
   Mitigation: install summary suggests
   `pkill -u $USER fono` (or just logging out and back in) to
   pick up the new binary. Same caveat as `fono update` and
   well understood by users of self-updating tools.
7. **Service user collision.** Distro packagers might create
   a `fono` user with different attributes. Mitigation:
   `useradd` with `--system` is idempotent on re-run; if a
   non-system `fono` user already exists, leave it alone and
   log a warning.

## Alternative approaches

1. **Per-user systemd unit (the v1 plan, the original
   `packaging/systemd/fono.service`).** Rejected by user
   feedback and on its own merits: requires
   `systemctl --user enable` as a separate manual step,
   doesn't work without lingering for headless tray-less
   sessions, and is strictly worse than XDG autostart for the
   desktop role.
2. **XDG autostart + system unit, both at user scope.**
   System unit at user scope contradicts the "headless server"
   role — a server has no logged-in user — and just gets us
   back to the per-user-unit pitfalls.
3. **No system unit, autostart only.** Forces server operators
   to write their own unit file. Friction without payoff —
   the unit is small, well-understood, and disabled by default
   so it costs desktop users nothing.
4. **`curl | sudo bash` install script in addition to / instead
   of the subcommand.** Requires network at install time and
   is asymmetric with `fono update`. Rejected; the binary
   already in the user's hand is the source of truth.
