# Auto-detect headless hosts in `fono install`

## Objective

Make `sudo fono install` (and the `curl … | sh` one-liner that wraps it)
pick **server mode** automatically on headless machines, without
requiring the operator to remember `--server`. Today the subcommand
defaults to desktop mode regardless of session state — that's the right
default on a workstation, but on a server it writes a menu desktop
entry, an XDG autostart file, an icon under
`/usr/share/icons/hicolor/scalable/apps/`, and skips the systemd unit
the operator actually wanted. The pieces needed to fix this already
exist in the codebase (`Session::detect()`, the `Headless` autostart
outcome, the `MODE=server` heuristic in `packaging/install.sh`); they
just aren't wired to the install-mode decision yet.

Desired UX:

- `sudo fono install` on a headless server (no graphical session, no DM
  active, `systemctl get-default = multi-user.target`) ⇒ server install,
  with a one-line "auto-detected headless host" banner and a hint about
  `--desktop` for overrides.
- `sudo fono install` on a workstation (active graphical login, DM
  running, or DISPLAY/WAYLAND_DISPLAY observable for the invoking user)
  ⇒ desktop install (today's behaviour).
- `sudo fono install --server` and `sudo fono install --desktop` remain
  unconditional overrides for both directions.
- The `curl … | sh` installer (`packaging/install.sh:65-84`) keeps its
  pre-sudo heuristic as an explicit hint (it already picks the right
  mode for the curl-pipe path), but no longer *needs* it to be correct
  — the binary is the source of truth.

Assumptions:

- Linux-only feature; Windows/macOS install paths are out of scope (the
  subcommand already `bail!`s on non-Unix at `crates/fono/src/install.rs:170`).
- systemd is the target init system for server-mode hosts; the
  detection signals lean on `systemctl` / `loginctl`, which are already
  consulted elsewhere in `install.rs` (see
  `crates/fono/src/install.rs:207-215`, `461-474`).
- Under `sudo` the invoking user's `DISPLAY` / `WAYLAND_DISPLAY` may or
  may not be inherited (depends on `sudo -E` vs default sudoers). The
  detector must therefore not rely on the current process's env alone;
  it must consult `loginctl`, filesystem session sockets, and DM unit
  state too.
- "Headless" is the conservative default: when detection is ambiguous,
  fall back to **desktop mode** (today's behaviour) and print a hint
  about `--server` — this avoids surprising a workstation user with an
  unexpected systemd unit + `fono` system user.

## Implementation Plan

- [x] Task 1. Add a `pub(crate) fn detect_headless() -> (bool, &'static str)`
  helper next to `Session::detect()` in `crates/fono/src/install.rs`,
  returning `true` only when we're confident the host is headless, plus
  a short reason string used in the install banner. Per design review
  (2026-05-22 feedback), there is no third `Ambiguous` state — anything
  that isn't a confident headless verdict falls through to today's
  silent desktop default, which is the conservative choice.
  Signals to combine, in priority order:
  1. **Active graphical session via `loginctl list-sessions
     --no-legend`** — if any row reports `Type=x11`/`Type=wayland` with
     `State=active`/`State=online` and `Class=user`, classify
     `Desktop`. This is the most reliable signal on systemd hosts and
     survives the `sudo` env scrub.
  2. **Display-manager unit active** — `systemctl is-active`
     for `gdm.service`, `sddm.service`, `lightdm.service`,
     `lxdm.service`, `xdm.service`, `greetd.service`, `ly.service`;
     any one returning `active` classifies `Desktop`.
  3. **Filesystem session sockets** — presence of `/tmp/.X11-unix/X*`
     or `/run/user/*/wayland-*` indicates an active graphical session
     even when `loginctl` is unavailable (non-systemd, or sandboxed
     test). Either ⇒ `Desktop`.
  4. **`systemctl get-default`** — `multi-user.target` is a strong
     server hint; `graphical.target` is a desktop hint. Treat as a
     tiebreaker rather than the sole signal (a workstation can be
     temporarily booted into multi-user.target for recovery).
  5. **Invoking user's env (best-effort)** — when `SUDO_USER` is set
     and `sudo -E` preserved `DISPLAY`/`WAYLAND_DISPLAY`, those vars
     classify `Desktop`. Pure additional positive signal.
  6. **Fallback** — when none of the desktop signals fire *and*
     `systemctl get-default = multi-user.target` (or systemd is
     absent), return `(true, "<reason>")`. In every other case
     (including "no desktop signals but `get-default` is graphical /
     unknown") return `(false, _)` and let the caller fall through to
     today's silent desktop default.

  Each `try_run`-style probe in this helper must be silent on failure
  (mirroring the existing `try_run` pattern at
  `crates/fono/src/install.rs:189-197`) and complete in well under a
  second on every host — no `sleep`-style waits.

- [x] Task 2. Introduce a `--desktop` flag on the `Install` subcommand
  in `crates/fono/src/cli.rs:274-281`, mutually exclusive with
  `--server` (clap's `conflicts_with`). Update the subcommand's help
  block (`crates/fono/src/cli.rs:266-273`) and the doc comment on
  `pub fn run_install` (`crates/fono/src/install.rs:315-350`) to
  describe the new three-state behaviour: explicit `--server`,
  explicit `--desktop`, or auto-detect. Plumb the new flag through
  the dispatch site (`crates/fono/src/cli.rs:525`) by changing
  `run_install`'s signature from `(server: bool, dry_run: bool)` to
  `(mode: InstallModeArg, dry_run: bool)` where `InstallModeArg` is a
  new local enum `{ Server, Desktop, Auto }`. Auto is the value used
  when neither flag is given.

- [x] Task 3. In `run_install` (`crates/fono/src/install.rs:315`),
  resolve `InstallModeArg::Auto` to a concrete `Mode` by calling
  `detect_headless()`:
  - `(true, reason)` ⇒ `Mode::Server`, print a one-line banner
    `→ auto-detected headless host (<reason>); installing in server mode (pass --desktop to override)`.
  - `(false, _)` ⇒ `Mode::Desktop` (today's silent default — no
    banner, since the existing UX already works for that case).

  The resolved `Mode` is then fed into the existing mode-switch
  refusal block (`crates/fono/src/install.rs:334-343`) and the
  existing `run_install_server` / `run_install_desktop` branches
  unchanged.

- [x] Task 4. Apply the same resolution in the `--dry-run` path at
  `crates/fono/src/install.rs:316-323` so `fono install --dry-run` on a
  headless host previews the server plan it will actually execute. The
  printed header line should include the chosen mode and the
  auto-detection reason when applicable so operators can verify the
  classification before committing.

- [x] Task 5. Update `packaging/install.sh`:
  - Keep the existing pre-sudo `MODE` heuristic
    (`packaging/install.sh:65-84`) — it's a useful hint for the
    curl-pipe path where the binary hasn't been downloaded yet — but
    pass it through to the binary as an explicit `--server` /
    `--desktop` flag rather than relying on the binary's auto-detect.
    The shell script has more context than the binary will (it runs in
    the *invoking user's* unmodified env), so when it's confident
    about `MODE=desktop` because `DISPLAY` is set, that's worth
    honouring.
  - When `FONO_MODE` is unset *and* the shell's heuristic is itself
    ambiguous (a future refinement, optional), call `fono install` with
    no mode flag and let the binary's `detect_headless()` decide. For
    the initial landing, keeping the shell's behaviour exactly as-is
    and adding the auto-detect inside the binary is sufficient — both
    layers reach the same answer on every supported host.
  - Update the inline doc comment at `packaging/install.sh:12-14`
    (`"…or '--server' (headless mode)"`) to mention the auto-detect.

- [x] Task 6. Tests in `crates/fono/src/install.rs`'s `#[cfg(test)]`
  module:
  - Refactor `detect_headless()` to accept injectable probes
    (`fn detect_headless_with(env, run_cmd, path_exists) -> HeadlessVerdict`)
    mirroring `Session::detect_with()`'s pattern at
    `crates/fono/src/install.rs:935`. The public `detect_headless()`
    becomes a thin wrapper that wires the real probes.
  - Add cases covering each branch:
    - Active loginctl wayland session ⇒ `Desktop`.
    - GDM active, no loginctl rows ⇒ `Desktop`.
    - `/tmp/.X11-unix/X0` present, nothing else ⇒ `Desktop`.
    - Nothing graphical + `multi-user.target` default ⇒ `true`.
    - Nothing graphical + `graphical.target` default ⇒ `false`.
    - `SUDO_USER` set with `WAYLAND_DISPLAY` inherited ⇒ `false`.
  - Add a dry-run test asserting the banner mentions the chosen mode
    and the reason string when `Auto` resolves to `Headless`.

- [x] Task 7. Update `docs/decisions/0023-self-installer.md` to record
  the auto-detect refinement (a short addendum block under the
  existing `--server` rationale at lines 79-91), and tick a checkbox
  in the currently-active phase plan referenced from
  `docs/status.md`. Add a `## [Unreleased]` bullet to `CHANGELOG.md`
  describing the new `--desktop` flag and the auto-detect default.

## Verification Criteria

- On a freshly-provisioned headless host (no X server, no DM unit,
  `systemctl get-default = multi-user.target`), `sudo fono install`
  installs the systemd unit at `/lib/systemd/system/fono.service`,
  creates the `fono` system user, and prints the
  `→ auto-detected headless host …` banner; no files appear under
  `/usr/share/applications/`, `/etc/xdg/autostart/`, or
  `/usr/share/icons/hicolor/`.
- On a workstation with an active graphical session, `sudo fono
  install` performs today's desktop install unchanged (no banner, no
  systemd unit written).
- `sudo fono install --desktop` on a headless host forces the desktop
  install (existing mode-switch refusal still applies when the other
  mode's artefacts already exist on disk).
- `sudo fono install --server` and `--desktop` cannot be combined —
  clap rejects the invocation with a usage error.
- `fono install --dry-run` (no flag) on a headless host prints a plan
  containing `SYSTEMD_UNIT` / `useradd` / `systemctl enable` lines,
  and on a workstation prints the desktop plan.
- The new `detect_headless_with()` unit tests all pass under
  `cargo test --workspace --tests --lib`.
- `cargo fmt --all -- --check` and `cargo clippy --workspace
  --all-targets -- -D warnings` both exit 0.
- `packaging/install.sh` continues to install successfully both for the
  headless and desktop cases, including when `FONO_MODE` is forced to
  either value.

## Potential Risks and Mitigations

1. **False-positive headless verdict on a workstation booted to
   multi-user.target for recovery.**
   Mitigation: classify *any* DM unit active, *any* `loginctl` session
   with `Type=x11/wayland`, and *any* X11/Wayland socket on disk as
   `Desktop` before consulting `get-default`. The `get-default`
   fallback only fires when literally no graphical signal exists,
   which is the correct verdict for a recovery-booted box anyway
   (there's no display to install a tray on).

2. **False-positive desktop verdict on a headless host that happens to
   have `gdm.service` installed but masked / inactive.**
   Mitigation: gate every DM probe through `systemctl is-active`
   (returning literal `active`), not `is-enabled`. Masked or inactive
   units read as `inactive`/`failed` and are ignored. Probes return
   silently on any non-`active` value; we never invoke `start`.

3. **Detection probes adding install-time latency.**
   Mitigation: every probe is a single short `systemctl` /
   `loginctl` / `stat` call with `Stdio::null()` redirection
   (matching `try_run` at `crates/fono/src/install.rs:189-197`). Total
   added wall-time is bounded by the slowest of these calls on a cold
   systemd, typically under 50 ms; well under the 2 s
   `verify_service_running` already sleeps for at
   `crates/fono/src/install.rs:233`.

4. **`packaging/install.sh` and the binary disagreeing about mode on
   the same host.**
   Mitigation: the shell script passes its decision explicitly via
   `--server` / `--desktop`, so the binary's auto-detect only runs
   when the operator invoked `fono install` directly (i.e. there is
   no shell-level decision to disagree with). When both layers run,
   the explicit flag wins and silences the auto-detect banner.

5. **Server-mode auto-install on a single-user laptop that happens to
   be SSH'd into from another box (no local graphical session at
   install time).**
   Mitigation: this is exactly the case `loginctl list-sessions`
   handles correctly — the laptop's local seat is still listed even
   when the operator is on SSH, so the verdict comes back `Desktop`.
   If the laptop is genuinely *not* logged in graphically at install
   time (cold boot, install from a TTY), the operator can always pass
   `--desktop` to override.

6. **CI / sandbox runs without systemd at all.**
   Mitigation: `systemctl_available()` already exists at
    Mitigation: `systemctl_available()` already exists at
  `crates/fono/src/install.rs:207-215`; without systemctl the detector
  has no graphical signal *and* no `get-default` to consult, so the
  fallback path treats it as `(true, "no systemd, no graphical session")`.
  CI sandboxes that need the desktop default for their assertions can
  set `FONO_INSTALL_NO_START=1` or pass `--desktop` explicitly; the
  unit tests use the injectable `detect_headless_with` seam and never
  exercise the real probes.

## Alternative Approaches

1. **Single `--mode {auto,desktop,server}` flag instead of
   `--server`/`--desktop` pair.** Cleaner CLI surface and avoids the
   `conflicts_with` rule, but breaks every existing tutorial,
   SlackBuild recipe, packaging README, and the `curl … | sh` script
   that already passes `--server`. The pair-of-booleans approach keeps
   `--server` byte-for-byte compatible.

2. **Auto-detect only inside `packaging/install.sh`, leave the binary
   unchanged.** Less code touched, but doesn't help operators who run
   `sudo fono install` directly (the most common case for users who
   built fono from source, downloaded a tarball, or pinned a specific
   version). The user's bug report comes from exactly this path.

3. **Always install both desktop and server artefacts, gated by a
   `WantedBy=` / `Hidden=true`.** Considered and rejected in the
   original ADR (`docs/decisions/0023-self-installer.md:79-91`) for
   good reason — it litters `systemctl list-unit-files` and creates a
   permanent "please don't enable this" footgun. The auto-detect
   default preserves the one-mode-per-install invariant while
   removing the manual-flag requirement.

4. **Prompt interactively when ambiguous.** Could be layered on top of
   the conservative-desktop fallback, but the install path is already
   running a setup wizard (`crates/fono/src/install.rs:837-882`) and
   adding a *second* TTY-gated prompt before that doubles the
   surface. The banner + `--server` hint is a less intrusive nudge,
   and operators who care can always re-run with an explicit flag.
