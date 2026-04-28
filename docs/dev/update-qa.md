# `fono update` QA Checklist

Manual smoke-test matrix for the self-update path
(`crates/fono-update/src/lib.rs`). Run before every release that
touches the `fono-update` crate, the `fono update` CLI surface
(`crates/fono/src/cli.rs:255-266`), or the release workflow's
sidecar emission step (`.github/workflows/release.yml:334-343`).

Wave 2 Thread B closes the self-update plan
(`plans/2026-04-27-fono-self-update-v1.md`) by adding per-asset
`.sha256` sidecar verification, a `--bin-dir` override, and this
ten-scenario matrix.

## Setup

A clean Linux x86_64 host with a previous Fono release installed in
`~/.local/bin/fono` is the easiest substrate. The package-managed
refusal (`crates/fono-update/src/lib.rs:374-380`) means scenarios that
exercise `/usr/bin/fono` need a chroot or a VM — never a host you care
about.

For sidecar mismatch tests, an HTTPS reverse proxy that rewrites the
`.sha256` body is the cleanest approach. `mitmproxy` with a small
`addons` script works.

## Scenarios

1. **Up-to-date.** `FONO_NO_UPDATE_CHECK=0 fono update --check` against
   a host running the latest release. Expect: `Up to date (vX.Y.Z).`
   No download, no temp file in the install dir.

2. **Available, dry-run.** `fono update --dry-run --yes` from a host
   one minor behind latest. Expect: download proceeds, sidecar fetched,
   digest matches, SHA-256 line printed, no rename, running binary
   untouched. Watch `~/.local/bin/` — no `.bak`, no leftover
   `.fono-update-*` temp.

3. **Available, real install, restart.** `fono update --yes` from one
   minor behind. Expect: `.bak` written, new binary installed,
   `execv()` re-exec keeps PID. `fono --version` after the call shows
   the new version.

4. **Available, real install, `--no-restart`.** Same as (3) plus
   `--no-restart`. Expect: same install, but the call exits 0 without
   re-exec; the next manual `fono` invocation runs the new build.

5. **Sidecar mismatch.** Proxy the asset through a server that returns
   a wrong `<asset>.sha256` body. Expect: `apply_update` bails with
   `sha256 mismatch for ...; refusing to apply`. The temp file is
   dropped, the running binary is intact, no `.bak` was created
   (the rename hasn't started yet — see
   `crates/fono-update/src/lib.rs:483-499`).

6. **Sidecar absent (legacy back-compat).** Point the metadata at a
   v0.2.x release whose assets predate the sidecar emission step.
   Expect: warn-and-proceed —
   `tracing::warn!("no .sha256 sidecar published")`
   (`crates/fono-update/src/lib.rs:494-498`) — and the install
   completes. This is the back-compat path for v0.1.x / v0.2.x.

7. **Package-managed refusal.** Run `fono update` against a binary
   located at `/usr/bin/fono`. Expect: `is owned by the system package
   manager; update via your distro's package manager` — exit non-zero,
   no download attempt.

8. **`--bin-dir` override.** Install a copy at `/opt/fono-test/fono`,
   then run `fono update --bin-dir /opt/fono-test --yes`. Expect: the
   override is canonicalised, the writability probe passes, the new
   binary lands at `/opt/fono-test/fono`, and the package-managed check
   still rejects `--bin-dir /usr/bin` even when the override is
   explicit.

9. **Read-only install dir.** `chmod a-w ~/.local/bin && fono update
   --yes`. Expect: the up-front writability probe trips
   (`crates/fono-update/src/lib.rs:430-438`) with `cannot write to ...;
   try sudo fono update`. No download.

10. **Network failure mid-stream.** Use `tc` or `iptables` to drop
    bytes after 50% of the asset has streamed. Expect:
    `stream chunk` context bubbles up, the temp file is dropped,
    no rename, running binary intact.

## Sign-off

A release engineer ticks off each scenario, records the host, the date,
and any deviations. The checklist itself never changes between
releases — if a scenario reveals a bug, fix the code, not the doc.
