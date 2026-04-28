# ADR 0006 — XDG paths

## Status

Reconstructed (original lost in filter-branch rewrite; rationale
recovered from `docs/status.md` and plan history, 2026-04-28).

## Context

Fono is a daemon. It reads config, writes a SQLite history database,
caches model files, and persists self-update state. Each kind of data
has a different lifecycle (config edited by hand, history grows
unbounded, models are large blobs, runtime state is ephemeral).
Putting them all under `~/.fono/` would conflict with established
Linux desktop conventions and with the user's right to back up,
delete, or move each class of data independently.

## Decision

Honour the XDG Base Directory Specification:

- Config: `$XDG_CONFIG_HOME/fono/config.toml` (default
  `~/.config/fono/config.toml`); secrets in `secrets.toml` alongside,
  mode `0600`.
- Data: `$XDG_DATA_HOME/fono/` (default `~/.local/share/fono/`)
  for the SQLite history DB and any user-curated assets.
- Cache: `$XDG_CACHE_HOME/fono/` (default `~/.cache/fono/`) for
  downloaded model files.
- State: `$XDG_STATE_HOME/fono/` (default `~/.local/state/fono/`)
  for `update.json` and other ephemeral runtime markers.

Resolution lives in `crates/fono-core/src/paths.rs` (or equivalent),
shared by every other crate; nothing else hard-codes path strings.

## Consequences

- Standard backup tools (rsnapshot, restic, etc.) automatically
  capture config + history while skipping caches.
- Users on multi-user boxes get per-user installs out of the box.
- Models are downloaded once per user and survive a config-only
  delete.
- Distro packagers can drop default files into
  `/etc/xdg/fono/config.toml` and have them merge over user config.
