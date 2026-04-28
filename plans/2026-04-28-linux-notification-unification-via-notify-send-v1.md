# Universal Linux Desktop Notifications via `notify-send`

## Objective

Replace Fono's brittle `notify-rust` dependency with a single, reliable, universal notification path that shells out to `notify-send` (libnotify CLI). Eliminate the silent-failure mode currently affecting all forty notification call sites, reduce binary size and dependency surface, and document libnotify-bin as a soft dependency.

## Background

User report (2026-04-28): rate-limit desktop notifications never fire even though `notify-send` from the same shell works perfectly. Instrumentation revealed the underlying error: `notify-rust: I/O error: No such file or directory (os error 2)` — `zbus` cannot find the D-Bus session socket via its discovery logic, even when `DBUS_SESSION_BUS_ADDRESS` and a working notification daemon are present. This is a known asymmetry between `libnotify`'s C-side discovery (autolaunch fallback, alternative socket-path formats, abstract sockets) and `zbus`'s pure-Rust path.

The problem is **not** specific to rate-limit notifications — Fono has ~40 `notify_rust::Notification::new()` call sites across `crates/fono/src/daemon.rs` and `crates/fono/src/session.rs`, all using `let _ = …show()` which silently discards errors. Every one of them is currently broken on the user's machine and likely on a non-trivial fraction of users' machines. The user only noticed the rate-limit one because that's the only call site whose error path was instrumented (commit `c1ff8cc`).

The user requirement: a universal fix that works on any standard Linux distribution, keeps Fono simple and light, and is appropriate for an open-source project distributed across diverse environments.

## Implementation Plan

- [ ] **Task 1.** Add a new `crates/fono-core/src/notify.rs` module exposing a single function `pub fn send(summary: &str, body: &str, icon: &str, timeout_ms: u32)`. Implementation shells out to `notify-send` via `std::process::Command` with `--icon=<icon>` and `--expire-time=<ms>`, redirecting stdin/stdout/stderr to `/dev/null`. Logs at `debug!` on success, `warn!` on non-zero exit or missing binary with an actionable message naming the distro packages (`libnotify-bin` for Debian/Ubuntu, `libnotify` for Arch/Fedora/openSUSE/Alpine, etc.). Adds zero new Cargo dependencies.

- [ ] **Task 2.** Add `pub fn is_available() -> bool` that does a one-time probe (cached behind `OnceLock<bool>`) running `notify-send --version` to detect whether libnotify is installed. Used by Task 7 (wizard).

- [ ] **Task 3.** Replace every `notify_rust::Notification::new()…show()` call site with `fono_core::notify::send(...)`:
  - `crates/fono/src/daemon.rs` lines 68, 899, 984, 1037, 1046, 1055, 1099, 1108, 1117, 1152, 1161, 1172, 1204, 1213, 1224, 1269, 1279, 1291, 1310, 1324, 1335 (~21 sites).
  - `crates/fono/src/session.rs` lines 171, 1107, 1348 (~3 sites).
  - `crates/fono-stt/src/rate_limit_notify.rs:127-147` (1 site, inside `fire_notification`).

  Preserve the original semantic intent at each site (icon names, timeouts) by mapping each call's existing args to the helper. Where the existing call used a non-standard icon, keep it.

- [ ] **Task 4.** Drop the `notify` cargo feature from `crates/fono-stt/Cargo.toml` (lines 38-41) and the `[features = ["notify"]]` from `crates/fono/Cargo.toml:70`. Remove `notify-rust` from the workspace `[workspace.dependencies]` block in `Cargo.toml`. Verify no `cfg(feature = "notify")` blocks remain in the codebase.

- [ ] **Task 5.** Remove `notify_rust` imports across the touched files. Compile and verify no orphan references remain (`grep -r notify_rust .` should return zero hits in `crates/`).

- [ ] **Task 6.** Update `fono_core::notify::send` signature to take an optional `urgency: Urgency` parameter (`Low | Normal | Critical`) mapped to `notify-send --urgency=<low|normal|critical>`. Wire `Critical` to error-class notifications (STT failures, X11 grab denied, panic-recovery), `Normal` to informational ones (rate limited, live dictation active, update available), `Low` to ephemeral status pings. Default param is `Normal` so existing call sites can be ported without surveying urgency mapping immediately.

- [ ] **Task 7.** Wizard preflight: in `crates/fono/src/wizard.rs`, after the existing hardware/audio probes, call `fono_core::notify::is_available()`. If false, print:
  ```
  ⚠ notify-send not found in PATH.
    Fono uses it for desktop notifications (rate-limit warnings, errors).
    Install:
      Debian/Ubuntu:  sudo apt install libnotify-bin
      Fedora:         sudo dnf install libnotify
      Arch:           sudo pacman -S libnotify
      openSUSE:       sudo zypper install libnotify-tools
      Alpine:         sudo apk add libnotify
    Fono will still work, but notifications will be silenced.
  ```
  Non-fatal — wizard continues. This is the soft-dependency surfacing that the user's "universal fix" requirement implies.

- [ ] **Task 8.** Unit tests for the helper:
  - `notify::send` does not panic when `notify-send` is missing (test by setting `PATH=` to an empty dir for the spawned command).
  - `notify::is_available` is cached and idempotent.
  - Mock-via-`PATH-injection` test that confirms the args passed to a fake `notify-send` script match the documented interface (icon, expire-time, summary, body, urgency).

- [ ] **Task 9.** Update documentation:
  - `README.md` add a "Soft dependencies" subsection listing `libnotify-bin` / `libnotify` package names per distro and what's lost without it (desktop notifications only; clipboard / hotkey / STT / LLM all unaffected).
  - `docs/troubleshooting.md` new section "Desktop notifications don't appear" pointing at the libnotify install commands and the wizard preflight output.
  - `packaging/*.SlackBuild`, `packaging/*.spec`, etc. update `REQUIRES=` / `Requires:` to declare `libnotify` as a runtime soft-dep where the package format supports it.
  - `ROADMAP.md` add a "Shipped" entry under whichever release this lands in.
  - `CHANGELOG.md` `[Unreleased]` section: `### Changed` (notify-rust → notify-send), `### Removed` (`notify` cargo feature), and `### Fixed` (forty silent notification failures across daemon and session).

- [ ] **Task 10.** ADR `docs/decisions/0022-desktop-notifications-via-libnotify.md` documenting the decision: rejected options (notify-rust pure-Rust path, gdbus, dbus-send, xdg-desktop-portal), chosen option (`notify-send`), trade-off accepted (loses notify-rust's cross-platform reach in exchange for reliability on Linux; macOS/Windows ports will use platform-native APIs when shipped).

- [ ] **Task 11.** Verify the binary shrinks: `cargo build --release -p fono`, compare `ls -l target/release/fono` against the pre-change baseline. Expectation: ~25-50 KB smaller. Also confirm `cargo tree -p fono | wc -l` drops by ~25-30 lines (the zbus/zvariant subtree).

- [ ] **Task 12.** Smoke-test all twelve major notification triggers manually after the port:
  1. Daemon startup (some sites print "Fono ready" or first-run messages).
  2. F8/F9 first live-dictation press ("Live dictation active").
  3. Tray "Quit" click confirmation.
  4. Self-update available toast.
  5. Self-update downloaded / "restart pending" toast.
  6. STT error (force by pulling the network).
  7. LLM error (force by setting an invalid API key).
  8. Inject failure (force by killing wtype/ydotool).
  9. Clipboard fallback notice.
  10. Tray menu reload after config change.
  11. Rate-limit (force as user already does).
  12. X11 hotkey grab denied (force by binding F8 in DE shortcuts).

- [ ] **Task 13.** Build, format, lint, test: `cargo fmt --all`, `cargo build -p fono`, `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --lib`. All must pass clean.

- [ ] **Task 14.** Update `docs/status.md` session log entry summarising what changed and any follow-ups.

- [ ] **Task 15.** Stop after Task 14. Do NOT bump version, do NOT tag, do NOT push tags. The operator runs the smoke test plan from Task 12 first and decides whether to roll into v0.3.4 or a separate v0.3.5.

## Verification Criteria

- `grep -r 'notify_rust' crates/` returns zero hits.
- `grep -r 'notify-rust' Cargo.toml` returns zero hits.
- `cargo tree -p fono | grep -i zbus` returns zero hits.
- `cargo build --release -p fono` succeeds; binary is measurably smaller than the pre-change baseline.
- All twelve notification triggers from Task 12 produce a visible desktop popup on the user's machine (where `notify-send "test"` works).
- On a machine where `notify-send` is intentionally missing (e.g. `PATH=/usr/bin:/bin` with libnotify-bin uninstalled): Fono starts cleanly, emits one `WARN` per attempted notification with the install instructions, and does not crash.
- `cargo test --workspace --lib` passes with no new failures.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.

## Potential Risks and Mitigations

1. **Headless / minimal-setup user without libnotify.** Notifications go silent.
   Mitigation: WARN log instructs user to install the package; wizard preflight surfaces it explicitly; the actual functional paths (STT, LLM, inject) all continue to work. Notifications were never load-bearing for correctness.

2. **Spawn cost (one fork per notification) on a slow embedded machine.** Notifications fire roughly 1-5 times per minute under heavy use; ~3-5 ms per fork is negligible. No mitigation needed; flagged here for completeness.

3. **`notify-send` argument escaping.** Long bodies with quotes / backslashes / newlines could in theory misbehave.
   Mitigation: `Command::arg` already passes args as separate argv slots, no shell interpretation. Test case in Task 8 confirms a body containing `"`, `'`, `\`, `\n`, `$VAR` round-trips correctly.

4. **Future cross-platform port (macOS/Windows).** Removing notify-rust loses its built-in macOS/Windows backends.
   Mitigation: those ports are on the roadmap as separate work and will use platform-native APIs (`NSUserNotification` / `UserNotifications.framework` on macOS; `Windows.UI.Notifications.ToastNotificationManager` on Windows) anyway. The new `fono_core::notify` module becomes a `cfg(target_os = "linux")` block at port time, with `cfg(target_os = "macos")` and `cfg(target_os = "windows")` siblings.

5. **Asynchronous fire-and-forget vs synchronous.** `Command::status()` blocks for ~3-5 ms.
   Mitigation: most call sites are not in latency-critical paths (UI events, error handlers). For the rate-limit site inside the streaming loop, wrap in `tokio::task::spawn_blocking` to keep the event loop unblocked. Alternative: use `Command::spawn()` and discard the `Child` — fire-and-forget. Decision deferred to implementation; spawn() is simpler.

## Alternative Approaches

1. **Keep notify-rust + add notify-send fallback.** Try notify-send first, fall back to notify-rust on failure (and vice versa). Strictly more code than the chosen approach, with notify-rust dragging in 28 crates and a known failure mode that would still occur 50% of the time. Rejected: a fallback chain only helps if both paths work in different environments; notify-rust does not work in any environment where notify-send fails (both rely on the same D-Bus session bus).

2. **Use `gdbus call` instead of `notify-send`.** Slightly lower-level. Same external dependency category but less universally installed (glib-only). Rejected: no upside over notify-send and worse universality.

3. **Talk directly to `org.freedesktop.Notifications` via raw zbus, hand-rolling the address discovery.** Re-implements the broken logic. Rejected: the bug is *in* the discovery code; reimplementing it doesn't help.

4. **Use `xdg-desktop-portal-notification`.** The future of sandboxed-app notifications. Rejected for now: not universally installed on i3/sway/minimal setups; protocol is more complex; appropriate for Flatpak builds when those land.

5. **Drop notifications entirely; use only the tray submenu.** Aligns with "simple and light" but removes a useful surface for asynchronous events (rate limit, update available, errors that occur while user isn't looking at the tray). Rejected: notifications are the standard Linux UX for these events.

## Suggested Forge invocation

> "Implement `plans/2026-04-28-linux-notification-unification-via-notify-send-v1.md`. Stop after Task 14 — do not bump version, do not tag."
