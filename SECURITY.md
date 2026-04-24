# Security Policy

## Reporting a vulnerability

If you believe you have found a security vulnerability in Fono, please report
it **privately** to <bogdan@nimblex.net> with the subject line
`[fono-security]`. Do not open a public GitHub issue for security problems.

We aim to acknowledge reports within 72 hours and to ship a fix (or a
documented mitigation) within 30 days for confirmed high-severity issues.
Credit will be given in the changelog and release notes unless you prefer
otherwise.

## Scope

In scope:

- Remote code execution, privilege escalation, or sandbox-escape bugs in the
  `fono` binary or any of its crates.
- Credential leakage — API keys stored in `secrets.toml`, environment
  variables, log files, or the history database.
- Integrity bypass of the model downloader (`fono-download`): accepting a
  tampered file that fails SHA256 verification, or silently downgrading
  revision pins.
- IPC socket (`fono.sock`) authorization flaws allowing cross-user access.

Out of scope (report as regular bugs, not security issues):

- Crashes or denial-of-service from malformed local config files the user
  themselves wrote.
- Feature gaps or UX issues with cloud providers that already require the
  user to supply their own API key.
- Licensing questions — please use regular issues for those.

## Non-goals

Fono does not promise protection against:

- A local attacker with read access to the user's `$HOME` (they can read
  `secrets.toml` regardless of file mode).
- Malicious compositor or desktop environment packages impersonating the
  global-hotkey portal.
- Supply-chain attacks on upstream model hosts — we pin SHA256 and revision
  hashes as a mitigation, but a user bypassing verification via
  `FONO_MODEL_MIRROR` assumes responsibility for their mirror.

## Signing

Release binaries are (planned to be) signed with minisign. The public key and
verification instructions will appear here once the first signed release
ships; see `docs/plans/2026-04-24-fono-design-v1.md` Task 9.1.
