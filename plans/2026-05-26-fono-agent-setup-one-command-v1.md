# `fono agent-setup` — One-Command Agent Integration

## Objective

Replace the current three-step manual process (enable MCP server, edit `mcp.json`,
copy the voice preset) with a single command that does everything and reports each
action. After running it, the user can immediately start a voice session with
`fono agent-loop --agent <name>`.

```sh
# Before this plan: three manual steps, one docs page.
# After this plan: one command.
fono agent-setup forge
```

Expected output:

```
Setting up Fono voice integration for forge…

  [1/3] MCP server      fono use mcp-server on   ✓ already enabled
  [2/3] Agent MCP JSON  ~/.forge/mcp.json         ✓ written
  [3/3] Voice preset    AGENTS.md                 ✓ appended

Done. Start a voice session with:
  fono agent-loop --agent forge
```

The command is idempotent: running it twice is safe. Already-correct configuration
is detected and reported as `✓ already configured` without overwriting.

---

## Implementation Plan

- [x] Task 1. **`AgentSetup` CLI variant**
- [x] Task 2. **`crates/fono/src/agent_setup.rs` module**
  - [x] Task 2a. Step 1 — Enable MCP server
  - [x] Task 2b. Step 2 — Write/merge MCP JSON
  - [x] Task 2c. Step 3 — Inject voice preset
- [x] Task 3. **`agents.toml` schema extension** — `preset_file` field added to `AgentEntry`
- [x] Task 4. **Embed voice preset** — `include_str!("../../../assets/agent-presets/voice.md")`
- [x] Task 5. **`fono agent-setup --list`**
- [x] Task 6. **Refactor shared TOML loader** — `crates/fono/src/agents.rs`
- [x] Task 7. **Unit tests** — 12 tests, all green
- [x] Task 8. **Update `docs/coding-agents.md`** — "Quick setup" section added
- [x] Task 9. **`crates/fono/src/lib.rs`** — `pub mod agent_setup;` + `pub mod agents;`
- [x] Task 10. **Pre-commit gate** — fmt ✓ · clippy ✓ · tests ✓ (0 failures)

---

## Verification Criteria

- `fono agent-setup forge --dry-run` prints the three planned actions without
  creating or modifying any file.
- `fono agent-setup forge --yes` on a clean machine: enables the MCP server,
  creates `~/.forge/mcp.json` with the canonical snippet, appends the voice preset
  to `AGENTS.md`, and prints `Done`.
- Running `fono agent-setup forge --yes` a second time on the same machine prints
  `✓ already enabled`, `✓ already configured`, `✓ already present` for all three
  steps without modifying any file.
- `fono agent-setup forge --yes` on a machine where `~/.forge/mcp.json` already
  exists with other `mcpServers` entries: adds `fono` without removing the others.
- `fono agent-setup --list` prints the table of registered agents and exits 0.
- `cargo test -p fono agent_setup` — all unit tests green.
- All three pre-commit gates pass.

---

## Potential Risks and Mitigations

1. **JSON corruption from a malformed existing `mcp.json`.**
   Mitigation: parse with `serde_json::from_str`; on error, print the parse
   failure and the path, then offer `--force` to overwrite rather than silently
   corrupting. Never write if parse fails.

2. **`~` expansion and symlinks in `mcp_config_path`.**
   Mitigation: use `dirs::home_dir()` for `~` expansion only; do not follow
   symlinks. If the resolved path does not exist, create it (with parent
   directories via `fs::create_dir_all`).

3. **Race: another process writes `mcp.json` between read and write.**
   Mitigation: write to a `.tmp` sibling first, then `rename` atomically.
   Standard pattern in `fono-core`'s `Config::save`.

4. **`AGENTS.md` / `CLAUDE.md` encoding / line endings.**
   Mitigation: read as UTF-8 bytes, append with `\n` separator, write back.
   If read fails with a non-UTF-8 error, bail with a clear message rather than
   silently truncating.

5. **`preset_injection = "none"` agents (Cursor, Codex, Gemini) still require
   a manual step for the voice preset.**
   Mitigation: the command still succeeds for Steps 1 and 2; Step 3 prints
   explicit manual instructions rather than failing. The UX is `~ manual step
   required` (amber, not red) so the user knows the command did most of the work.

---

## Alternative Approaches

1. **Integrate into `fono agent-loop --setup`** — add a `--setup` flag to the
   existing `agent-loop` command so it performs setup before launching. Trade-off:
   mixes one-time setup with every-session launch; a user who only wants to set up
   (e.g. for a CI check) can't do so without triggering a launch.

2. **Wizard integration** — add "Set up a voice-driven coding agent?" as the
   final first-run wizard step. Trade-off: the wizard runs once; users who install
   a new agent later need a separate command anyway. Wizard can call `agent-setup`
   internally.

3. **Shell script bundled with docs** — ship a `scripts/setup-agent.sh` instead of
   a Rust command. Trade-off: not cross-platform, requires shell, doesn't benefit
   from the type-safe TOML registry.
