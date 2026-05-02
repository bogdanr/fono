# ADR 0023 — Self-installer subcommand (`fono install` / `fono uninstall`)

## Status

Accepted 2026-05-02.

## Context

Fono ships as a single static-ish binary. Users who download a
release asset (`fono-vX.Y.Z-x86_64`) historically had no path to a
fully-integrated install short of building a distro package or
hand-copying files into `/usr/local/bin`, `/usr/share/applications`,
the hicolor icon tree, and (for headless hosts) a systemd unit.
Tambourine and OpenWhispr — the two projects Fono replaces — both
relied on heavy package frontends (Tauri installer, Electron AppImage)
to bridge that gap. We want the same drop-in experience without
adopting either stack.

We also have two distinct deployment targets:

1. **Desktop workstations** — fono runs in the user's graphical
   session, needs tray/hotkey/overlay, and should auto-start on
   graphical login.
2. **Headless servers** — fono runs as a long-lived daemon under a
   dedicated system user with no `DISPLAY`/`WAYLAND_DISPLAY`, no
   tray, no overlay, surfaces only its IPC socket and config dir.

These two roles are mutually exclusive on a given host in practice
and want different artefacts written to different paths.

## Decision

Add `fono install` and `fono uninstall` subcommands to the binary
itself, with a single mode-selecting flag (`--server`) and a single
preview flag (`--dry-run`). No `--system` / user-scope split, no
per-component opt-out flags, no `--force`, no `--purge`.

**Desktop mode (`sudo fono install`, default):**

- `/usr/local/bin/fono`
- `/usr/share/applications/fono.desktop` (menu)
- `/etc/xdg/autostart/fono.desktop` (graphical autostart)
- `/usr/share/icons/hicolor/scalable/apps/fono.svg`
- `/usr/share/bash-completion/completions/fono`
- `/usr/share/zsh/site-functions/_fono`
- `/usr/share/fish/vendor_completions.d/fono.fish`
- `/usr/local/share/fono/install_marker.toml` (`mode = "desktop"`)

**Server mode (`sudo fono install --server`):**

- `/usr/local/bin/fono`
- `/lib/systemd/system/fono.service` (hardened, enabled and started
  immediately)
- `fono` system user (created via `useradd --system`)
- shell completions (same three paths as desktop)
- `/usr/local/share/fono/install_marker.toml` (`mode = "server"`)

`fono uninstall` reads the marker and removes exactly the files the
marker recorded — never globs, never deletes user config or history.
Re-running `install` against an existing marker of the *same* mode
is idempotent (in-place upgrade); re-running with a different mode
is rejected with "run `fono uninstall` first".

Implementation lives in `crates/fono/src/install.rs`. Packaging
assets (`fono.desktop`, `fono.svg`, `fono.service`) live in
`packaging/assets/` as a single source of truth and are embedded
into the binary via `include_str!`/`include_bytes!`.

## Trade-offs

**Why system-only, no user-scope install.** A user-scope install
(`~/.local/bin`, `~/.config/systemd/user`, …) sounds friendlier but
runs into `$PATH`-precedence surprises against package-managed
copies, can't write a system unit for headless servers, and doubled
the test matrix. `sudo` is a one-time cost; the resulting layout
matches what every distro packager would produce by hand and what
`fono doctor` already knew how to inspect.

**Why a single `--server` flag instead of two subcommands
(`install-server` / `install-desktop`) or always-both.** Earlier
plan revisions considered shipping both artefacts unconditionally
(disabled systemd unit on desktops, unused autostart entry on
servers) with a written warning. That created a perpetual footgun —
admins enabling the "please don't enable this" unit by accident —
and littered `systemctl list-unit-files` with units no one asked
for. Splitting on `--server` keeps the CLI surface small (one verb,
one flag) while making each install do exactly one job.

**Why `--server` enables-and-starts the unit immediately.** The
admin already opted in by passing the flag; surprising restraint
(write-but-don't-enable) was a worse UX than the alternative.

**Why no `--purge` / no per-component `--no-*` flags.** Uninstall is
non-destructive by definition (config and history are XDG dirs the
installer never wrote). Users who want those gone can `rm` them
explicitly. Per-component opt-out flags expand the test matrix
without buying anything — every supported install path is one of
two named layouts.

**Why embed assets instead of reading from
`/usr/share/fono/...`.** Release-asset users running
`./fono-vX.Y.Z-x86_64 install` from `$HOME/Downloads` have no
installed asset tree yet. Embedding via `include_str!` /
`include_bytes!` keeps the binary self-sufficient. Desktop file +
SVG + systemd unit total under 8 KiB — no impact on the 20 MiB
budget (ADR 0022).

**Why marker-driven uninstall instead of pattern-matching.**
Globbing `/usr/share/applications/fono*.desktop` would surprise
users who hand-edited the file or installed a package alongside.
The marker is an explicit contract: "install put these exact paths
down; uninstall removes these exact paths".

## Rejected alternatives

- **AppImage / Flatpak / Snap.** All three add a heavy runtime layer
  (FUSE, sandbox, Bubblewrap) that defeats the "single static
  binary" property and complicates IPC + global hotkey + text
  injection. They also don't help the headless-server lane.
- **Self-elevating installer (`fono install` calls `sudo` itself).**
  Existing convention across `kubectl`, `cargo`, `rustup`, etc., is
  to require the caller to invoke `sudo` explicitly. We surface a
  clear permission-denied error pointing to `sudo` rather than
  prompting interactively.
- **Always-both install (v2 of the plan).** Rejected for the
  footgun and `list-unit-files` reasons above.

## Consequences

- Release-asset users get a one-command path to a fully-integrated
  install matching the layout distro packagers ship.
- `fono doctor` (`crates/fono/src/doctor.rs`) gains an Install
  section reporting one of: self-installed (desktop),
  self-installed (server), package-managed, ad-hoc on `$PATH`,
  not-on-`$PATH`.
- Distro packaging recipes (SlackBuild, Debian, AUR, Nix) and the
  embedded copy share `packaging/assets/` as the canonical source
  for the desktop file, icon, and (server-mode) systemd unit. The
  legacy per-user `packaging/systemd/fono.service` is left in place
  for now; recipes will migrate to the new system-mode unit at
  their next packaging revision.
- Phase 9 (distro packaging) becomes lighter: every recipe can
  defer to the embedded installer for paths it doesn't need to
  manage itself.

## References

- Plan: `plans/2026-05-02-fono-install-subcommand-v3.md`
- Implementation: `crates/fono/src/install.rs`
- Embedded assets: `packaging/assets/`
- ADR 0022 — binary size budget (asset embedding fits within budget)
- ADR 0019 — platform scope (Linux first; install paths reflect FHS)
