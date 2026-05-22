# Server-mode install: auto-enable Wyoming listener

## Objective

Make `sudo fono install --server` produce a daemon that **is actually
listening on the LAN** the moment the installer returns. Today the
installer drops a binary + systemd unit + `fono` service user and
runs `systemctl enable --now fono.service`, but the daemon comes up
with `[server.wyoming].enabled = false` (the in-code default in
`crates/fono-core/src/config.rs:958-967`) and `bind = 127.0.0.1`. The
short-circuit at `crates/fono/src/daemon.rs:1698` then skips the
listener and the mDNS advertiser entirely, so the service "runs" but
port 10300 has nothing on it and tray browsers on the LAN see no
peer.

After this slice: a fresh `sudo fono install --server` writes a
minimal `/etc/fono/config.toml` (only when none exists) enabling the
Wyoming server on `0.0.0.0:10300`, prints the bind address +
security caveat in the install summary, and verifies the port is
accepting connections before declaring success.

Scope is deliberately narrow:

- One target user lane (`fono install --server`).
- One protocol family (Wyoming STT). `[server.fono]` lands later in
  Slice 6 of the network plan and is out of scope.
- No changes to distro packaging (`packaging/{slackbuild,debian,aur}/`):
  those ship *user* units only and never install the system server,
  so there is no config to seed. Surfacing a system lane through
  distro packages is a separate follow-up.
- No silent edits to pre-existing `/etc/fono/config.toml`. Operator
  state is sacred.
- No uninstall cleanup of the seeded config file (Task 3 in the
  original sketch — skipped per feedback). Leaving the config on
  uninstall is consistent with how state under `/var/lib/fono`,
  `/var/cache/fono` is preserved today, and avoids the "did we
  create this exact byte sequence?" detection complexity.

## Implementation Plan

- [ ] Task 1. **Embed a seed config asset.** Add a new
      `packaging/assets/server-config.toml` containing the minimum
      `[server.wyoming]` block enabling the Wyoming listener on
      `0.0.0.0:10300`, with a leading comment block explaining what
      it does and the security implication of binding to all
      interfaces. Wire it into `crates/fono/src/install.rs:30-39`'s
      `assets` module alongside `DESKTOP` / `ICON_SVG` /
      `SYSTEMD_SYSTEM_UNIT` via `include_str!`.
      *Rationale:* keep the seed bytes under version control next
      to every other packaging asset; consolidate the "what does the
      seeded config look like" answer in one human-readable file
      instead of an inline Rust literal. The fono service user must
      be able to read the file, so a real file (later chowned
      `root:fono`, `0640`) is the right shape.

- [ ] Task 2. **Define canonical seed path.** Add
      `const SYSTEM_CONFIG_DIR: &str = "/etc/fono";` and
      `const SYSTEM_CONFIG_FILE: &str = "/etc/fono/config.toml";`
      near the existing path constants at
      `crates/fono/src/install.rs:45-52`. These paths already exist
      at runtime: the unit's `ConfigurationDirectory=fono` line at
      `packaging/assets/fono.service:40` creates `/etc/fono/` with
      `root:fono 0750` and the daemon's `XDG_CONFIG_HOME=/etc`
      override (line 20 of the same file) makes it the daemon's
      config root.
      *Rationale:* mirrors the existing `BIN_PATH` / `SYSTEMD_UNIT`
      constants and keeps the install module the single source of
      truth for filesystem layout.

- [ ] Task 3. **Add `write_atomic_owned` helper.** Extend the
      existing `write_atomic` at `crates/fono/src/install.rs:144-166`
      (or add a sibling) that additionally takes an `(uid, gid)` and
      `chown`s the persisted file. Resolve `fono:fono` via `getent
      passwd fono` / `getent group fono` rather than hard-coding
      numeric IDs (system UIDs vary per distro).
      *Rationale:* the seeded config holds future bearer-token refs;
      it should not be world-readable. The fono daemon, running as
      the `fono` user, must read it. `0640` + `root:fono` ownership
      is the minimum-privilege shape.

- [ ] Task 4. **Seed `/etc/fono/config.toml` in `run_install_server`.**
      Before the `systemctl enable --now` call at
      `crates/fono/src/install.rs:1492`, add a step that:
      1. Calls `std::fs::create_dir_all(SYSTEM_CONFIG_DIR)` (the
         directory is normally pre-created by `ConfigurationDirectory=`
         on first boot, but the unit hasn't started yet at this
         point); `chown` it `root:fono` mode `0750`.
      2. If `Path::new(SYSTEM_CONFIG_FILE).exists()` returns `true`,
         skip the write and print
         `  · /etc/fono/config.toml already present — leaving it alone`.
         Set a `seeded = false` flag for the summary.
      3. Otherwise, `write_atomic_owned(SYSTEM_CONFIG_FILE, SEED,
         0o640, fono_uid, fono_gid)` where `SEED` is the embedded
         asset from Task 1. Print
         `  · /etc/fono/config.toml (seeded: Wyoming server on 0.0.0.0:10300)`.
         Set `seeded = true`.
      *Rationale:* idempotent re-runs of `fono install --server` must
      not stomp operator edits. A fresh install gets a working,
      LAN-reachable Wyoming server out of the box.

- [ ] Task 5. **Update `build_install_plan` for `--dry-run`.** In
      `crates/fono/src/install.rs:670-692`, add a step
      `write {SYSTEM_CONFIG_FILE} (only if absent)` to the server
      branch so `sudo fono install --server --dry-run` accurately
      previews the new behaviour. Update the existing
      `build_server_plan_lists_all_targets` test
      (`crates/fono/src/install.rs:1707-1720`) to assert the new
      target string is present.

- [ ] Task 6. **Post-install port probe in `verify_service_running`.**
      Today `crates/fono/src/install.rs:226-266` only consults
      `systemctl is-active`, which returns `active` even when the
      Wyoming listener never bound. Extend it (or add a sibling
      `verify_wyoming_listener(addr: &str)`) that after the
      `is-active` check does a one-shot `TcpStream::connect_timeout`
      to `127.0.0.1:10300` with a 2 s budget. Print one of:
      - `  · Wyoming server listening on 0.0.0.0:10300 (TCP probe OK)`
      - `  · Wyoming server NOT listening on 127.0.0.1:10300
        (probe timed out / refused — check journalctl -u fono.service)`
      *Rationale:* surfaces silent server-side failures (missing STT
      backend, bind permission denied) at install time instead of
      the user discovering it via a cryptic client-side error like
      the one that started this thread.

- [ ] Task 7. **Surface degraded-mode warning.** The daemon already
      emits a `WARN` at `crates/fono/src/daemon.rs:2413-2418` when
      `[server.wyoming].enabled = true` but no STT backend is
      configured. Mirror that knowledge into the installer: after
      seeding the config, check `fono_core::config::Config::default()`
      (the shape the daemon will load with no `[stt]` block) for the
      `Local` STT backend and probe whether any whisper model is
      installed under `/var/lib/fono/models/` (or the path returned
      by `fono_core::paths::Paths::resolve()` for the server lane).
      If none, print:
      ```
      Note: no Whisper model is installed under /var/lib/fono/models/.
      The Wyoming server will accept connections but return errors
      until a model is downloaded. Install one with:
        sudo -u fono fono models install small
      ```
      Non-fatal — server still installs, port still listens, just
      no inference.

- [ ] Task 8. **Update install summary.** Replace the trailing block
      at `crates/fono/src/install.rs:1510-1524` to reflect the new
      reality:
      - When `seeded == true`: print the bind address
        (`0.0.0.0:10300`), the security caveat ("Wyoming v1 has no
        in-band authentication; binding to 0.0.0.0 exposes inference
        to anyone on this LAN. Edit /etc/fono/config.toml and
        restart `fono.service` to change the bind address or restrict
        with iptables / nftables / your firewall"), and a pointer to
        `docs/providers.md`.
      - When `seeded == false` (config already present): print a
        hint that the existing `/etc/fono/config.toml` is in effect
        and the operator may need to set `[server.wyoming].enabled =
        true` manually if they want LAN serving.
      *Rationale:* the user must understand they just opened a port,
      and must have an actionable next step if the seed was skipped.

- [ ] Task 9. **Tests** (`crates/fono/src/install.rs` `mod tests`):
      - `build_server_plan_mentions_seed_config_target` —
        `build_install_plan(true)` step list contains
        `/etc/fono/config.toml`.
      - `embedded_server_config_seed_is_valid_toml` — the embedded
        seed parses via `toml::from_str::<fono_core::config::Config>`
        and yields `cfg.server.wyoming.enabled == true`,
        `bind == "0.0.0.0"`, `port == 10300`.
      - `embedded_server_config_seed_has_security_note` — the seed
        contains the substring `0.0.0.0` and a `#` comment line
        mentioning auth, so the operator sees the warning in the file
        they edit.
      *Rationale:* mechanical end-to-end coverage that the asset and
      the install plan stay in sync; the toml-round-trip catches
      future schema drift the moment it lands.

- [ ] Task 10. **Docs**: append a short note to `docs/providers.md`
      (Wyoming section) explaining that `fono install --server`
      now writes a default `/etc/fono/config.toml` with the listener
      enabled on `0.0.0.0:10300`, and the operator can edit `bind`
      / `auth_token_ref` / add `iptables` rules to restrict
      exposure. Reference the file path so users searching for
      "where does the server config live" land here.

- [ ] Task 11. **Changelog + roadmap**. Add a bullet under the next
      `## [Unreleased]` (or `## [X.Y.Z]` if you're cutting a release
      with this) in `CHANGELOG.md`: "server install now writes a
      default `/etc/fono/config.toml` enabling the Wyoming listener
      on 0.0.0.0:10300, so `sudo fono install --server` is
      LAN-reachable out of the box." If this is part of a tagged
      release, also move the corresponding `ROADMAP.md` line per
      `AGENTS.md` "Hard rules" — otherwise leave the roadmap alone.

- [ ] Task 12. **Pre-commit gate** (per `AGENTS.md`):
      1. `cargo fmt --all -- --check`
      2. `cargo clippy --workspace --all-targets -- -D warnings`
      3. `cargo test --workspace --tests --lib`

      All three must exit 0 before pushing. Commit with `-s` (DCO).

## Verification Criteria

- On a clean Linux host:
  `sudo fono install --server` exits 0, prints
  `Wyoming server listening on 0.0.0.0:10300 (TCP probe OK)` in its
  summary, and `ss -tln | grep 10300` shows `LISTEN 0.0.0.0:10300`.
- On the same host re-running `sudo fono install --server` is
  idempotent: the existing `/etc/fono/config.toml` is preserved
  byte-for-byte (`sha256sum` before/after matches), and the summary
  prints `already present — leaving it alone`.
- From a second machine on the same LAN, `curl -v
  telnet://<server-ip>:10300` succeeds (or `nc -zv <ip> 10300`
  reports `succeeded`), and `fono` running on that second machine
  shows the peer in the tray submenu with a routable v4 address
  (regression coverage for the link-local IPv6 bug fixed in the
  prior turn).
- `sudo fono install --server --dry-run` lists
  `write /etc/fono/config.toml (only if absent)` in its plan.
- `cargo test --workspace --tests --lib` passes, including the
  three new tests in `crates/fono/src/install.rs`.
- `fono doctor` continues to report `self-installed via
  \`fono install\` (server mode, /usr/local/bin/fono)`.

## Potential Risks and Mitigations

1. **Auto-binding to 0.0.0.0 exposes inference to the LAN.**
   Mitigation: the install summary prints the security caveat
   verbatim; the seeded config file leads with a `#` comment block
   restating it; `docs/providers.md` gains a paragraph on auth /
   firewalling. The operator chose `--server` and the default is
   the operator's most likely intent (otherwise they would have
   used `--desktop`), so opting in is the right tradeoff. Users
   wanting loopback-only can edit one line.

2. **A future feature might want to add fields to the seeded config,
   creating drift between the embedded template and
   `fono_core::config::Config::default()`.**
   Mitigation: Task 9 includes a parse-and-assert round-trip; the
   moment a field is added that the seed doesn't carry, the test
   still passes (extra fields default), but a *removed/renamed*
   field surfaces immediately. Keep the seed minimal — only the
   `[server.wyoming]` block — so the surface is small.

3. **`/etc/fono/` may not exist when we try to write into it on
   pre-systemd hosts** (the `ConfigurationDirectory=` directive only
   runs at service start). Mitigation: Task 4 explicitly
   `create_dir_all`s before the write and `chown`s the directory.

4. **The TCP probe in Task 6 may race the systemd unit's start.**
   The existing `thread::sleep(2s)` at
   `crates/fono/src/install.rs:233` already gives systemd enough
   time on every machine we test on; reuse the same delay before
   the new probe. If the probe fails despite `is-active = active`,
   that's exactly the failure mode we want to surface — print the
   refused/timed-out line and let the journal output that follows
   explain why.

5. **Some hosts already have `/etc/fono/config.toml` from a prior
   manual setup with `[server.wyoming].enabled = false`.**
   Mitigation: Task 4's "skip if exists" branch keeps that config
   intact and Task 8 prints the "you may need to set enabled = true
   manually" hint. We never silently flip a flag the operator set
   to false.

6. **The fono user/group might not exist when we resolve their
   uid/gid in Task 3.** Mitigation: `ensure_service_user()` at
   `crates/fono/src/install.rs:1527` runs *before* the config seed
   step (it's already the first action in `run_install_server`).
   Resolve the uid/gid right after that call; fail loudly if
   `getent` can't find it.

## Alternative Approaches

1. **Detect server mode at daemon startup and auto-flip `enabled`
   when running under the system unit.** Pros: zero install-time
   logic, the config file stays optional, no `/etc/fono/config.toml`
   to leak across uninstalls. Cons: couples runtime behaviour to
   filesystem layout sniffing (`$XDG_CONFIG_HOME=/etc`? UID == fono
   uid?); operators can't introspect the bound address from
   `config.toml`; the "what is my server doing" question gains a
   third place to look. Rejected: explicit beats implicit for a
   port-binding decision.

2. **Add a `--listen` / `--bind <addr>` flag to `fono install
   --server` and require the operator to opt in.** Pros:
   conservative; never auto-exposes a port. Cons: defeats the whole
   point of `--server` mode — the operator already told us they
   want a server. Adds friction to the most common case. Rejected
   on UX grounds, but worth keeping the flag as an *override* in a
   later slice (e.g. `--bind 127.0.0.1` for "install but
   loopback-only").

3. **Parse the existing `/etc/fono/config.toml` (if any) and append
   a `[server.wyoming]` block if missing.** Pros: even users with
   pre-existing partial configs get the listener. Cons: TOML
   round-tripping through serde reorders / restyles the file and
   destroys operator comments; partial appends are fragile. Mixing
   our writes into a file we don't own end-to-end violates the
   "operator state is sacred" principle. Rejected: only seed when
   absent.

4. **Ship the seeded `config.toml` as a packaging artefact instead
   of embedding it in the binary.** Pros: distro packages could
   include it directly. Cons: distro packages don't install the
   system service today (they ship user units only), so the seeded
   config would have no consumer. Embedding keeps the
   self-installer self-contained — the same binary delivered via
   `curl | install` works identically to one installed from a
   downloaded archive. Revisit if/when distro packages grow a
   system-server lane.
