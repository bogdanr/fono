# Fono Self-Update (background check + tray button + CLI flag)

## Status

~85% landed in commit `3e2c742` (2026-04-22). Remaining work tracked as
Wave 2 Task 8 of `plans/2026-04-28-doc-reconciliation-v1.md`.

## Objective

Add a self-update capability to the Fono daemon so it:

1. Periodically checks GitHub Releases in a background thread to determine whether a newer version than the running binary is available.
2. Surfaces an "Update available" item / button in the system tray icon that, when clicked, downloads and atomically replaces the running binary, then prompts (or performs) a daemon restart.
3. Exposes the same logic via a CLI subcommand/flag (`fono update`, with `--check`, `--yes`, `--channel`) for headless use, scripts, and parity with the existing `install` script flow.

The feature must be safe (signature/size verification, atomic replace, rollback on failure), respectful (no telemetry beyond an unauthenticated GitHub API call, opt-out config knob), and consistent with Fono's "single static Rust binary" identity.

## Scope and Assumptions

This plan targets the Fono application source on the `main` branch at `bogdanr/fono` (Rust). The current working tree is the `site` branch (`fono.page`), so file paths below are conceptual and must be reconciled with the actual module layout during implementation. Documented assumptions:

- The binary is a single static Rust executable distributed via GitHub Releases under the asset naming convention `fono-<tag>-<arch>` (confirmed by `install:41-42`).
- Releases are published at `https://api.github.com/repos/bogdanr/fono/releases/latest` and `tag_name` is a SemVer-ish string like `v0.2.0` (confirmed by `install:33-35` and `index.html:760-767`).
- The daemon already owns a tray icon and an async/threaded runtime (the README references "tray + hotkeys" at `index.html:632`). If a tray library is not yet in use, the same plan applies but the tray task becomes a prerequisite.
- `CARGO_PKG_VERSION` is the source of truth for the running version.
- Distro-packaged installs (`.pkg.tar.zst`, `.deb`, `.txz` per `index.html:621-623`) should **not** self-replace the binary — package managers own those files. The updater must detect package-managed installs and degrade to "notify only".

## Implementation Plan

### Phase 1 — Foundations

- [x] Task 1. Add a `version` module exposing `current() -> semver::Version` parsed from `CARGO_PKG_VERSION`, plus helpers `is_newer(remote, local) -> bool` and a `Channel` enum (`Stable`, `Prerelease`) for future-proofing. Rationale: centralises all version comparisons and avoids string compares scattered through the codebase.

- [x] Task 2. Add a `release` module that calls `GET https://api.github.com/repos/bogdanr/fono/releases/latest` (and optionally `/releases` for prereleases), parses `tag_name`, `name`, `body`, `published_at`, and the matching asset URL for the current `target_triple`/`arch`. Use `reqwest` (blocking or async to match the existing runtime) with a 10s timeout, `User-Agent: fono/<version>`, and graceful handling of HTTP 403 rate-limit responses. Rationale: mirrors the resolution logic already proven in `install:33-42`, but in Rust.

- [x] Task 3. Define an `UpdateInfo` struct (`current`, `latest`, `asset_url`, `asset_name`, `size`, `sha256_url_opt`, `notes`) and an `UpdateStatus` enum (`UpToDate`, `Available(UpdateInfo)`, `CheckFailed(String)`). Rationale: a single typed payload flows through the background task, the tray, and the CLI without ad-hoc tuples.

### Phase 2 — Background checker thread

- [x] Task 4. Spawn a dedicated background task at daemon startup (`tokio::spawn` or `std::thread` depending on the runtime in use) that runs an initial check after a short jittered delay (e.g. 30–120s) and then on a configurable interval (default 24h). Rationale: avoids slowing daemon startup and avoids thundering-herd on GitHub for users who restart often.

- [x] Task 5. Persist last-check timestamp and last-known `UpdateStatus` to the existing config/state directory (likely `$XDG_STATE_HOME/fono/update.json` or alongside the existing config file). Skip the network call if the cache is fresh. Rationale: keeps API usage low and lets the tray render the badge instantly on next start.

- [x] Task 6. Expose the latest `UpdateStatus` to the rest of the app via a thread-safe channel/`watch` (e.g. `tokio::sync::watch::Sender<UpdateStatus>` or `Arc<RwLock<...>>`). Rationale: tray menu and CLI subcommand both need read access without coupling to the checker's internals.

- [x] Task 7. Add config knobs in the existing config file: `update.auto_check` (bool, default `true`), `update.interval_hours` (u32, default `24`), `update.channel` (`stable`/`prerelease`, default `stable`), and respect `FONO_NO_UPDATE_CHECK=1` env var. Rationale: gives privacy-conscious users and CI environments an off switch consistent with the "no telemetry" promise on `index.html:563`.

### Phase 3 — Tray integration

- [x] Task 8. Extend the tray menu builder to react to `UpdateStatus`: when `Available`, prepend a highlighted item "Update to <tag>" plus a submenu entry "Release notes…" that opens the GitHub release URL in the default browser. When `UpToDate`, show a disabled "Up to date (vX.Y.Z)" item. Rationale: a single visible affordance is the simplest UX; submenu keeps the main menu uncluttered.

- [x] Task 9. Optionally swap the tray icon to a "badge" variant (small dot overlay) when an update is available, and revert when applied or dismissed. Rationale: discoverability without nagging notifications. If overlay variants are not yet in the asset set, skip and rely on the menu item.

- [x] Task 10. Wire the "Update to <tag>" click to invoke the same `apply_update` routine used by the CLI (Phase 4), running it on a worker thread so the tray event loop is never blocked. Show progress and final status via a desktop notification (already used elsewhere by Fono if `notify-rust` is wired; otherwise log to the daemon log and update the tray label to "Updating…" / "Restart required"). Rationale: one code path for tray and CLI eliminates drift.

### Phase 4 — Updater core (download + verify + atomic replace)

- [x] Task 11. Implement `apply_update(info: &UpdateInfo, opts: ApplyOpts)` that: (a) determines the running executable path via `std::env::current_exe()`, (b) detects package-manager-owned binaries (path starts with `/usr/bin`, `/usr/lib`, or the file is owned by a package per `dpkg -S` / `pacman -Qo` heuristics) and refuses with an actionable message, (c) downloads to a sibling temp file in the same directory as the running binary. Rationale: same-directory temp guarantees `rename(2)` is atomic on the same filesystem.

- [ ] Task 12. Verify the download: enforce HTTPS, check `Content-Length` matches the GitHub asset size, compute SHA-256, and compare against a `.sha256` companion asset if the release publishes one (recommended follow-up to also publish checksums). Set permissions to `0755`. Rationale: protects against truncated downloads and tampered mirrors; mirrors the `install -m755` behaviour at `install:51-54`.

- [x] Task 13. Atomically swap: `rename(new, current)` (Linux allows replacing a running executable's inode; the running process continues with the old inode until it re-execs). On non-writable destinations (e.g. `/usr/local/bin` for non-root users), surface a clear error suggesting `sudo fono update` or pointing to `BIN_DIR` semantics from the install script. Rationale: parity with the install script's permission handling at `install:50-60`.

- [x] Task 14. Restart strategy: by default, prompt the user (tray notification "Update applied — restart Fono?") and on confirmation call `execv(current_exe, original_argv)` to hot-swap into the new binary while preserving the daemon's PID. CLI variant accepts `--restart`/`--no-restart`. Rationale: zero-downtime upgrade; users who prefer manual restarts retain control.

- [x] Task 15. Rollback on failure (partial — `.bak` sidecar landed; smoke `--self-check` deferred, see Open follow-ups): keep the previous binary at `<exe>.bak` until the new process has run a smoke check (e.g. `--self-check` flag that exits 0 if it can parse config and bind no resources). If the smoke check fails, restore the `.bak` and notify. Rationale: safety net against bricking the daemon on a bad release.

### Phase 5 — CLI surface

- [ ] Task 16. Add a top-level subcommand `fono update` with flags: `--check` (only print status, exit 0/1 on up-to-date/available), `--yes`/`-y` (skip confirmation prompt), `--channel <stable|prerelease>`, `--no-restart`, `--bin-dir <path>` (override target install dir to mirror the install script's `BIN_DIR`), and `--dry-run` (resolve and verify but do not write). Rationale: covers interactive, scripted, and CI use cases.

- [x] Task 17. Add `fono version` (or extend existing `--version`) to print both running and last-known latest versions plus the cached check timestamp. Rationale: trivial diagnostics for bug reports.

- [x] Task 18. Document the new commands in `--help`, and add a short section to the website (`index.html` switch-callout near `index.html:629-635`) showing `fono update` alongside the existing first-run commands. Rationale: discoverability for users who landed via the install one-liner.

### Phase 6 — Quality, packaging, and release plumbing

- [x] Task 19. Detect package-managed installs and route them to a "notify only" mode: tray item becomes "Update available — use your package manager" with a tooltip mentioning the matching command from the install matrix at `index.html:621-624`. Rationale: prevents self-update from fighting `pacman`/`apt`.

- [ ] Task 20. Update the GitHub Actions release workflow to publish `fono-<tag>-<arch>.sha256` alongside each binary so Task 12's verification has authoritative checksums. Rationale: upgrades trust from "TLS to github.com" to "publisher-signed digest". (If signing keys exist or are added later, extend to `.minisig` / cosign.)

- [ ] Task 21. Add unit tests for `version::is_newer`, `release` JSON parsing (with fixture payloads for `latest` and `prerelease`), and the package-manager detection heuristic. Add an integration test that points the updater at a local HTTP server serving a fixture release and asserts the temp-file → rename → exec flow on a throwaway binary. Rationale: self-update is the kind of feature that breaks silently; tests are the only defence.

- [ ] Task 22. Add a manual QA checklist to the repo (e.g. `docs/dev/update-qa.md`) covering: bare-binary install, `/usr/local/bin` non-root, distro-packaged install, offline mode, rate-limited GitHub response, interrupted download, prerelease channel, and rollback. Rationale: makes future updater changes auditable.

## Verification Criteria

- Running daemon detects a newly published release within `update.interval_hours` and surfaces it in the tray without restart.
- `fono update --check` exits `0` when up to date and `1` when an update is available, and prints a one-line machine-parseable status (`up-to-date <ver>` / `available <cur>-><new>`).
- `fono update -y` on a writable install path completes a download, replaces the binary atomically, and the new version reports correctly via `fono --version` after restart.
- `fono update` on a package-managed install refuses to overwrite and prints the correct distro command from the install matrix.
- With `update.auto_check=false` or `FONO_NO_UPDATE_CHECK=1`, no network request to `api.github.com` occurs (verified by network-trace test).
- A corrupted/truncated download is detected (size or SHA-256 mismatch) and the original binary remains intact.
- A failed smoke check on the new binary triggers automatic rollback and a clear notification.
- Tray icon badge appears when an update is available and clears after the update is applied or explicitly dismissed.

## Potential Risks and Mitigations

1. **Replacing a running executable on Linux.**
   Mitigation: rely on inode semantics — `rename` over the running file is safe because the kernel keeps the old inode open for the running process; new invocations get the new inode. Use `execv` for in-place restart only after the file is fully written and fsynced.

2. **GitHub API rate limiting (60/h unauthenticated).**
   Mitigation: cache the last response, jitter the interval, honour `X-RateLimit-Reset`, and fall back to the HTML `releases/latest` redirect (which `install` already implicitly relies on) if the API is unavailable.

3. **Fighting distro package managers.**
   Mitigation: detect package-owned binaries (Task 19) and switch to notify-only mode rather than overwriting files the package manager tracks.

4. **Supply-chain risk on auto-replace.**
   Mitigation: enforce HTTPS, verify SHA-256 against a published `.sha256` asset (Task 20), and gate `apply_update` behind explicit user action by default — auto-check yes, auto-apply no.

5. **Permission errors on `/usr/local/bin`.**
   Mitigation: detect `EACCES` early, print the exact command to retry under `sudo`, and mirror the `BIN_DIR` override pattern from `install:6` and `install:50-60`.

6. **Privacy expectation regression** (the site states "Telemetry: None. Ever." at `index.html:563`).
   Mitigation: document the update check as an unauthenticated GitHub API call (no identifiers, no payload), provide an off switch (`FONO_NO_UPDATE_CHECK`, config flag), and call this out explicitly in the release notes for the version that ships the feature.

7. **Tray library limitations** (some Linux tray implementations don't support icon overlays or dynamic menu rebuilds).
   Mitigation: design the tray integration so the menu-item path is the primary surface and the badge overlay is a progressive enhancement gated on capability detection.

## Open follow-ups (carried into Wave 2 Task 8)

- Task 12 — per-asset `.sha256` sidecar verification (downloads enforce
  HTTPS + size today; SHA-256 comparison against a published `.sha256`
  asset is not yet wired).
- Task 15 (smoke half) — `--self-check` exit-0 smoke flag for the new
  binary before clearing the `.bak` sidecar.
- Task 16 — `fono update --bin-dir <path>` flag mirroring the install
  script's `BIN_DIR` semantics.
- Task 20 — release workflow emits `fono-<tag>-<arch>.sha256` per asset.
- Task 21 — unit + integration tests for the updater (`version::is_newer`,
  release JSON parsing fixtures, package-manager detection, end-to-end
  fixture-server flow).
- Task 22 — manual QA checklist `docs/dev/update-qa.md`.

Evidence for the landed work:

- `crates/fono-update/src/lib.rs:31-107` (`UpdateInfo`),
  `:38-59` (`Channel`), `:118-127` (`is_newer`), `:267`
  (`FONO_NO_UPDATE_CHECK`), `:381-477` (`apply_update`),
  `:455-463` (`.bak` sidecar), `:507-529` (`restart_in_place`).
- `crates/fono/src/daemon.rs:145-185` (background checker spawn +
  `update.json` persistence), `:476`, `:514`, `:1195-1213` (tray hook).
- `crates/fono-core/src/config.rs:47, 70` (`[update]` config block).
- `crates/fono-tray/src/lib.rs:78, 487-494` (tray menu wiring).

## Alternative Approaches

1. **Notify-only (no self-replace).** Background check + tray button that opens the GitHub releases page or copies the install one-liner. Trade-off: simplest, zero supply-chain surface, but loses the "automatic" UX the user asked for. Could be the v1 ship while Tasks 11–15 land in v2.

2. **Delegate to `cargo binstall` / `self_update` crate.** Use the `self_update` crate (designed for exactly this) to handle download/replace/restart. Trade-off: large dependency, less control over verification and package-manager detection, but slashes implementation effort by ~60%. Reasonable middle ground if maintenance bandwidth is tight.

3. **Re-run the existing `install` script.** Have `fono update` shell out to `curl -L https://fono.page/install | sh` after confirming. Trade-off: zero new code paths and guaranteed parity with the documented install flow, but loses atomic in-process restart, requires `curl` and `sh` at runtime, and is awkward to integrate with the tray.

4. **Use a separate updater helper binary.** Ship `fono-updater` that the daemon execs to perform replacement, then re-execs `fono`. Trade-off: cleaner separation and easier privilege escalation handling (`pkexec fono-updater`), but doubles the artefact count and complicates the "single static binary" story from `index.html:598`.
