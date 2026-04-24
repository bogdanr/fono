# Fono — Project Status

Last updated: 2026-04-24

## Current milestone

**v0.1 scaffolding** — Phase 0 complete.

## Phase progress

| Phase | Description | Status |
|-------|-------------|--------|
| 0     | Repo bootstrap + workspace + CI skeleton | ✅ Complete (commit 0ecdf27) |
| 1     | fono-core: config, secrets, XDG paths, SQLite schema | ⏳ Next |
| 2     | fono-audio: cpal capture + Silero VAD + auto-mute | ⏳ Pending |
| 3     | fono-hotkey: global-hotkey + hold/toggle FSM | ⏳ Pending |
| 4     | fono-stt: trait + WhisperLocal + one cloud backend | ⏳ Pending |
| 5     | fono-llm: trait + LlamaLocal + one cloud backend | ⏳ Pending |
| 6     | fono-inject: enigo wrapper + focus detection | ⏳ Pending |
| 7     | fono-tray + fono-overlay | ⏳ Pending |
| 8     | First-run wizard + CLI | ⏳ Pending |
| 9     | Packaging: GitHub release + NimbleX SlackBuild | ⏳ Pending |
| 10    | Docs + v0.1.0 tag | ⏳ Pending |

## Next session

**Phase 1** — implement `fono-core` per Tasks 1.1–1.4 of the design plan:

- XDG path resolver honouring `XDG_*_HOME` overrides.
- `Config` struct with serde defaults + atomic load/save + version migration stub.
- `Secrets` struct at `~/.config/fono/secrets.toml` mode 0600.
- SQLite schema for `history.sqlite` with FTS5 + retention cleanup.

## Session log

- **2026-04-24 (Phase 0)**: Bootstrap complete. 10 crate stubs, CI (Linux/Mac/Win +
  DCO + cargo-deny), release workflow (cross-compile matrix),
  `docs/plans/2026-04-24-fono-design-v1.md`, `docs/decisions/` ADRs 0001–0004,
  `AGENTS.md`, this status file. Committed as `chore: initial scaffold for Fono
  v0.1` + `docs: add agent orientation and decision log`.
