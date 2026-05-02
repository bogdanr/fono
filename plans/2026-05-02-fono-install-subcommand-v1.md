# Fono — `fono install` self-installer subcommand

## Objective

Give users who download the prebuilt single-file `fono` binary a one-command
path to a fully integrated OS install — desktop launcher, icon, systemd user
unit, shell completions, `$PATH`-resolvable executable — without writing a
SlackBuild / `.deb` / AUR package or copying files by hand. The same command
should also work in reverse (`fono uninstall`) and complement, never fight,
the existing `fono update` self-update flow and the distro packages.

Target user story: download `fono-vX.Y.Z-x86_64` from a GitHub release,
`chmod +x`, run `./fono-vX.Y.Z-x86_64 install`, get a functioning Fono on
the next login (or immediately via the printed `systemctl --user enable
--now fono.service` hint).

## Scope and assumptions

- Linux only for v1 (matches `crates/fono-update/src/lib.rs:148-150`
  platform gate). macOS / Windows installers are ROADMAP "On the
  horizon" items and out of scope.
- Two install scopes:
  - **User scope (default)** — no privilege escalation. Targets
    `~/.local/bin`, `~/.local/share/applications`,
    `~/.local/share/icons/hicolor/scalable/apps`,
    `~/.config/systemd/user`, `~/.local/share/bash-completion/completions`,
    `~/.local/share/zsh/site-functions`,
    `~/.config/fish/completions`.
  - **System scope (`--system`)** — requires root (sudo). Targets
    `/usr/local/bin`, `/usr/local/share/applications`,
    `/usr/local/share/icons/hicolor/scalable/apps`,
    `/lib/systemd/user` (or `/etc/systemd/user`),
    `/usr/share/bash-completion/completions`,
    `/usr/share/zsh/site-functions`,
    `/usr/share/fish/vendor_completions.d`.
- **Distro-package coexistence.** Refuse to overwrite system
  package-managed paths (mirror `crates/fono-update/src/lib.rs:374-380`
  `is_package_managed`). System scope writes to `/usr/local/...` only;
  if the user's `$PATH` still resolves `fono` to a `/usr/bin/fono`
  shipped by `pacman` / `dpkg`, surface a clear warning.
- **Self-sufficiency.** The installer must work from a single binary
  with no source checkout present. Therefore the `.desktop` file, SVG
  icon, and `systemd` user unit currently in
  `packaging/slackbuild/fono/` must be embedded into the `fono` binary
  via `include_str!` / `include_bytes!` (single source of truth — the
  packaging tree references the same files at build time).
- **Completions.** Generated at install time by calling Fono's existing
  `fono completions <shell>` (`crates/fono/src/cli.rs:230-234`) — no
  new generator code, just routing the output to the right path.
- **Update interaction.** `fono update` continues to overwrite the
  installed binary in place. The install scope is recorded so
  `update --bin-dir` can fall back gracefully when `current_exe()`
  resolves to a non-canonical path (already supported by the existing
  `--bin-dir` flag, `crates/fono/src/cli.rs:267-274`).

## Implementation plan

### Phase 1 — Embed packaging assets in the binary

- [ ] Task 1.1. Move the canonical desktop entry, SVG icon, and systemd
  user unit out from under `packaging/slackbuild/fono/` into a
  packaging-neutral location (e.g. `packaging/assets/`) so the
  SlackBuild, Debian rules, AUR `PKGBUILD`, Nix flake, and the new
  installer all reference one copy. Update the four packaging
  recipes (`packaging/slackbuild/fono/fono.SlackBuild`,
  `packaging/debian/rules`, `packaging/aur/PKGBUILD`,
  `packaging/nix/flake.nix`) to read from the new path. Rationale:
  prevents the embedded copy from drifting from the distro copies.
- [ ] Task 1.2. Add a new module `crates/fono/src/install/assets.rs`
  exposing the desktop file, icon SVG, and systemd unit as
  `pub const DESKTOP: &str = include_str!(...)` /
  `pub const ICON_SVG: &[u8] = include_bytes!(...)` /
  `pub const SYSTEMD_USER_UNIT: &str = include_str!(...)`. The
  `include_*!` paths are workspace-relative to
  `packaging/assets/` from Task 1.1.
- [ ] Task 1.3. Decide whether to also embed a 256×256 PNG fallback
  for desktop environments without librsvg-driven SVG icon
  rendering. Default: rely on the SVG only (Fono's existing
  packaging ships SVG only); revisit if real-world feedback shows
  icon-loading gaps.

### Phase 2 — `Cmd::Install` and `Cmd::Uninstall` CLI surface

- [ ] Task 2.1. Add two new variants to
  `crates/fono/src/cli.rs::Cmd`:
  - `Install` with flags: `--system` (write to `/usr/local/...`,
    requires root), `--bin-dir <path>` (override binary destination),
    `--no-systemd` (skip writing the user unit), `--no-completions`
    (skip shell completions), `--no-desktop` (skip `.desktop` and
    icon), `--enable-service` (run `systemctl --user enable --now
    fono.service` after install), `--force` (overwrite existing
    installs and ignore the "already on `$PATH` from somewhere else"
    warning), `--dry-run` (print what would happen, write nothing).
  - `Uninstall` with flags: `--system`, `--keep-config` (default:
    keep `~/.config/fono` and `~/.local/share/fono`; explicit
    `--purge` removes them), `--dry-run`. Uninstall must never
    touch user data unless `--purge` is set; the help text states
    this prominently.
- [ ] Task 2.2. Create module
  `crates/fono/src/install/{mod.rs,layout.rs,actions.rs,detect.rs}`:
  - `layout.rs` — pure functions resolving target paths from a
    `Scope::{User, System}` enum. Output is a typed `InstallLayout`
    struct (one `PathBuf` per asset). User-scope layout uses
    `dirs::home_dir()` and the existing
    `crates/fono-core/src/paths.rs` XDG helpers; system-scope uses
    fixed `/usr/local/...` constants.
  - `detect.rs` — pre-flight checks: detect the running binary path
    via `std::env::current_exe()`, detect whether it's already a
    package-managed copy (reuse
    `fono_update::is_package_managed`), detect existing installs at
    each candidate location (so `--force` is meaningful), and
    detect whether `~/.local/bin` is on `$PATH` (warn if not, with
    a one-liner suggestion to add it to the user's shell rc).
  - `actions.rs` — atomic copy + chmod 0755 + parent-directory
    creation helpers, mirroring the temp-file-then-rename pattern
    in `crates/fono-update/src/lib.rs:442-537` so a failed install
    leaves the system unchanged. All file writes go through one
    `write_file_atomic(path, bytes, mode)` helper.
- [ ] Task 2.3. Wire dispatchers in `crates/fono/src/cli.rs::run`
  to call `install::run_install(opts)` / `install::run_uninstall(opts)`.
  Both return a structured `InstallReport` (list of created /
  skipped / already-present paths) which the dispatcher prints in
  human-friendly form, and emits as JSON when a future `--json`
  flag is added (out of scope for v1; reserve the field shape).

### Phase 3 — Install actions, in dependency order

- [ ] Task 3.1. **Binary copy.** `current_exe()` → `<bindir>/fono`.
  When the running binary already lives at the destination
  (e.g. user already extracted to `~/.local/bin/fono` and just
  re-ran install for the desktop entry), skip the copy and log
  "binary already in place at <path>". Set 0755. On `--system`,
  surface a clear error when `geteuid() != 0` rather than letting
  `EACCES` bubble up.
- [ ] Task 3.2. **Desktop entry.** Materialise
  `assets::DESKTOP` to `<applications>/fono.desktop`. The `Exec=`
  line stays `Exec=fono` because we just put `fono` on the user's
  `$PATH`. On user scope, run `update-desktop-database -q
  <applications>` if available; on system scope, run it against
  the system applications dir. Best-effort, mirroring
  `packaging/slackbuild/fono/doinst.sh:5-7`.
- [ ] Task 3.3. **Icon.** Materialise
  `assets::ICON_SVG` to
  `<icondir>/hicolor/scalable/apps/fono.svg`. Best-effort
  `gtk-update-icon-cache -q -t -f <icondir>/hicolor` afterwards.
- [ ] Task 3.4. **Systemd user unit.** Materialise
  `assets::SYSTEMD_USER_UNIT` to `<systemd-user-dir>/fono.service`.
  Patch the embedded `ExecStart=/usr/bin/fono` line at write time
  to point at the actual install location resolved in Task 3.1
  (`crates/fono/src/install/actions.rs` does the substitution
  before writing). Run `systemctl --user daemon-reload` after the
  write when on user scope and the bus is available
  (`DBUS_SESSION_BUS_ADDRESS` set or `XDG_RUNTIME_DIR/bus`
  exists). When `--enable-service` is set, additionally run
  `systemctl --user enable --now fono.service` and report the
  outcome.
- [ ] Task 3.5. **Shell completions.** Spawn `<bindir>/fono
  completions bash|zsh|fish` and write the output to the matching
  per-shell directory in the layout. Skip a shell silently when
  its target directory's parent is missing (e.g. user has no
  `~/.config/fish`). Log per-shell outcome.
- [ ] Task 3.6. **`$PATH` advisory.** After Task 3.1 on user
  scope, check whether `<bindir>` is in `$PATH`. If not, print a
  one-paragraph advisory naming the shell rc file
  (`~/.bashrc` / `~/.zshrc` / `~/.config/fish/config.fish`)
  detected from `$SHELL` and the literal one-line snippet to
  append. Never modify shell rc files automatically — that's a
  surprising side effect and doubles the uninstaller's surface.

### Phase 4 — Uninstall actions

- [ ] Task 4.1. Inverse of Phase 3, in reverse dependency order:
  disable + stop the user service (`systemctl --user disable
  --now fono.service`), remove the systemd unit, remove
  completions, remove icon, remove `.desktop` (with cache
  refresh), remove the binary at `<bindir>/fono` (only if its
  embedded version string matches what the running binary
  declares — guards against removing a distro-shipped binary with
  the same name; on mismatch, refuse and tell the user to remove
  via their package manager). Mirror the
  package-managed-path refusal from Task 3.1.
- [ ] Task 4.2. `--purge` removes
  `~/.config/fono`, `~/.local/share/fono`, `~/.cache/fono`,
  `~/.local/state/fono`. Default uninstall keeps these;
  print their sizes so the user can decide whether to follow up
  with `fono uninstall --purge`.

### Phase 5 — Update / `is_package_managed` interactions

- [ ] Task 5.1. Extend `crates/fono-update/src/lib.rs::is_package_managed`
  (or add a sibling helper) with `/usr/local/bin/` recognised as a
  *user-installed-via-`fono install`* path — already writable, already
  the default for self-update (`crates/fono-update/src/lib.rs:376-380`
  comment). No change needed here, but document the contract in
  the new install module so future contributors don't accidentally
  block it.
- [ ] Task 5.2. When `fono install --system` overwrites
  `/usr/local/bin/fono`, write a sentinel file
  `/usr/local/share/fono/INSTALL_MARKER` containing the install
  scope, version, and ISO-8601 timestamp. `fono uninstall
  --system` reads this marker before removing files (refuses if
  absent — protects against `fono` binaries placed at the same
  path by other means). Same pattern in user scope:
  `~/.local/share/fono/install_marker.toml`.
- [ ] Task 5.3. `fono doctor` learns to surface the install
  marker (or its absence) plus the resolved binary path, so users
  can see at a glance whether their `fono` is "self-installed",
  "package-managed", or "ad-hoc-on-PATH". Touch
  `crates/fono/src/doctor.rs` only.

### Phase 6 — Tests, docs, packaging hygiene

- [ ] Task 6.1. Unit tests in `crates/fono/src/install/`:
  layout resolution returns expected paths under both scopes
  given a fake `$HOME`; `--dry-run` produces a non-empty
  `InstallReport` with no filesystem writes (use `tempfile::tempdir`
  to chroot the user-scope layout); the desktop / unit
  substitution helper rewrites `Exec=` and `ExecStart=` correctly;
  `is_package_managed` refusal path bails before any writes; the
  install-marker round-trip parses what it serialises.
- [ ] Task 6.2. Smoke integration test
  `crates/fono/tests/install_user_scope_dry_run.rs`: invokes the
  `Cmd::Install` path with `--dry-run` against a tempdir-rooted
  layout and asserts the report names every expected target
  path.
- [ ] Task 6.3. Update `README.md` "Install" section with the
  new one-liner: `./fono-vX.Y.Z-x86_64 install` → desktop
  launcher, completions, optional service. Update
  `docs/dev/update-qa.md` to add an "install / uninstall"
  scenario alongside the ten existing update scenarios.
- [ ] Task 6.4. Update `CHANGELOG.md` `[Unreleased]` with `Added:
  fono install / fono uninstall self-installer subcommands`. Per
  AGENTS.md release contract, the next tag must move this entry
  into the released section and update `ROADMAP.md` "Up next" →
  "Recently shipped" (a new tile titled "One-command install"
  fits the style of the existing badges).
- [ ] Task 6.5. Add an ADR
  `docs/decisions/0023-self-installer.md` capturing: (a) why we
  ship a self-installer rather than relying purely on distro
  packages — single-binary ergonomics, parity with `fono update`,
  zero-toolchain installs on bespoke distros not yet packaged;
  (b) why user scope is the default — no `sudo` prompt during
  first-time setup; (c) why we refuse `/usr/bin/...` overwrites —
  package-manager hygiene, parity with `is_package_managed`;
  (d) why we embed assets — single-binary contract.

## Verification criteria

- `fono install --dry-run` on a fresh `$HOME` lists exactly the
  six expected target paths (binary, desktop, icon, systemd unit,
  three completions) and writes nothing to disk.
- `fono install` on a fresh `$HOME` produces a working desktop
  launcher visible after `update-desktop-database`, an icon
  rendered by KDE / GNOME / XFCE / i3-with-rofi, and
  `command -v fono` resolving to `~/.local/bin/fono` after the
  user re-sources their shell rc (or in a fresh shell).
- `systemctl --user start fono.service` succeeds when run after
  `fono install --enable-service` and the daemon's tray icon
  appears (on hosts with an SNI watcher).
- `fono install --system` from a non-root shell exits non-zero
  with a clear "this requires root; re-run with sudo" message
  before any filesystem writes.
- `fono install` against a host with `/usr/bin/fono`
  package-managed and `~/.local/bin` already on `$PATH` succeeds
  and the new user-scope install takes precedence at the next
  shell.
- `fono uninstall` on a self-installed host removes every file
  the installer wrote, leaves config / history / cache intact,
  and reports the on-disk size of those untouched directories.
- `fono uninstall --purge` additionally removes the four XDG
  data dirs.
- `cargo test -p fono install::` is green; `cargo clippy
  --workspace --all-targets -- -D warnings` is clean; the
  `tests/check.sh` matrix is green; the size budget gate
  (`size-budget` CI job) still passes — embedded SVG icon
  (~few KB) and three text snippets are well below the 20 MiB
  ceiling and the four-NEEDED allowlist is unchanged.
- The four distro packaging recipes (SlackBuild, Debian rules,
  AUR PKGBUILD, Nix flake) all build successfully against the
  new `packaging/assets/` location and produce identical output
  to before the move.

## Potential risks and mitigations

1. **Embedded asset drift vs distro copies.**
   Mitigation: single-source-of-truth move in Task 1.1; CI
   `tests/check.sh` already reads files relative to the workspace
   root, so a missing file fails fast at build time. Add a CI
   guard that greps for `include_str!("../../../packaging/assets/...")`
   to make the dependency explicit.
2. **`PATH` precedence surprises.** A package-managed
   `/usr/bin/fono` may shadow a user-scope `~/.local/bin/fono`,
   leaving the user confused about which version they're running.
   Mitigation: install-time advisory printed in Task 3.6 and
   reflected in `fono doctor` (Task 5.3).
3. **Systemd unit write on hosts without systemd.** Some Linux
   distros (Void with runit, Artix with OpenRC, Alpine with
   OpenRC) don't run systemd. Mitigation: Task 3.4 detects
   `systemctl --version` first; on absence, log "systemd not
   detected; skipping unit installation" and continue. The
   `--no-systemd` flag is the explicit opt-out.
4. **Overwriting a different `fono` binary.** A user might have
   built a local `~/.cargo/bin/fono` from a fork. Mitigation:
   install-marker file (Task 5.2) plus `--force` requirement to
   overwrite an existing install whose marker is missing or whose
   version doesn't match the running binary.
5. **Wayland desktop-cache invalidation lag.** Some compositors
   cache `.desktop` and icon lookups for the session. Mitigation:
   document in `docs/troubleshooting.md` that a fresh login is
   sometimes required; `update-desktop-database` and
   `gtk-update-icon-cache` are best-effort and the install
   succeeds regardless of their availability.
6. **Self-uninstall with running daemon.** Removing the binary
   while the daemon is running yields an inode-pinned process
   that survives until restart. Mitigation: `fono uninstall`
   first sends `Request::Shutdown` over IPC (or `pkill -TERM
   -u $USER fono` as fallback), waits up to 3 s, then
   removes — same pattern `fono update` already follows for
   in-place re-exec (`crates/fono-update/src/lib.rs:632-657`).
7. **Distro packagers double-installing.** A Debian / Slackware
   user who already has the package and then runs
   `fono install --system` ends up with two binaries on `$PATH`.
   Mitigation: install-time pre-flight detects
   `/usr/bin/fono` and refuses with a clear "you already have
   the distro package; use `apt remove fono` first or use user
   scope" message unless `--force` is set.

## Alternative approaches

1. **Ship a separate `fono-installer` binary.** Lower complexity
   in the main binary; doubles the release-asset count and
   confuses users who expect `fono` itself to handle the job.
   Rejected — `fono update` already lives in the same binary, so
   `fono install` is a natural extension.
2. **Distribute a shell `install.sh` (curl | sh) instead of a
   subcommand.** Common pattern (rustup, ollama, Tailscale).
   Pros: no embedded assets; no Rust changes. Cons: requires
   network access at install time, can't reuse the running
   binary, contradicts the single-binary value proposition,
   harder to test, leaves no symmetric uninstall. Rejected.
3. **Lean entirely on the existing distro packages.** Status
   quo. Works for users on supported distros (Slackware,
   Arch, Debian, NixOS); fails the user reporting this issue
   who downloaded the binary directly. Doesn't satisfy the
   stated objective.
4. **`cargo install fono` from crates.io.** Requires a Rust
   toolchain and a from-source build (slow, fragile across
   distros for whisper.cpp's C++ deps). The single-binary
   release artefact is precisely the artefact most users will
   already have downloaded. Out of scope and orthogonal.
5. **XDG `org.freedesktop.portal.Background` autostart instead
   of a systemd user unit.** Cleaner integration with
   sandboxed sessions; requires a portal implementation
   (xdg-desktop-portal) that not every minimal session ships.
   Defer to a v2 follow-up; v1 keeps the systemd unit Fono
   already publishes.
