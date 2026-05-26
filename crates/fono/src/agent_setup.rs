// SPDX-License-Identifier: GPL-3.0-only
//! `fono agent-setup <agent>` — one-command setup that wires a coding agent
//! to Fono's MCP server and injects the shared voice-mode system prompt.
//!
//! Three idempotent steps are executed in order:
//!
//! 1. **MCP server** — sets `[mcp] enabled = true` in
//!    `~/.config/fono/config.toml` (idempotent; no-op if already on).
//! 2. **Agent MCP JSON** — merges `"mcpServers": { "fono": { … } }` into the
//!    agent's own MCP config file (e.g. `~/.forge/mcp.json`). Other entries
//!    in the file are untouched.
//! 3. **Voice preset** — appends the shared voice-mode system prompt
//!    (`assets/agent-presets/voice.md`) to `AGENTS.md` / `CLAUDE.md` (or
//!    similar) in the project directory, guarded by a sentinel comment so
//!    re-running the command never double-injects. Agents that use `"none"` /
//!    `"manual"` preset injection get printed instructions instead.
//!
//! All steps respect `--dry-run` (print what would happen, write nothing) and
//! `--project-dir` (default: current directory).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fono_core::{Config, Paths};

use crate::agents::{self, AgentEntry};

/// Embedded voice-mode system prompt shipped with the binary.
pub const VOICE_PRESET: &str = include_str!("../../../assets/agent-presets/voice.md");

/// Sentinel injected into AGENTS.md / CLAUDE.md to guard against re-runs.
const PRESET_SENTINEL: &str = "<!-- fono-voice-preset -->";

/// Outcome of a single setup step — used by the printer and by tests.
#[derive(Debug, PartialEq, Eq)]
pub enum StepResult {
    /// Already configured; nothing written.
    AlreadyDone,
    /// Written (or would be written in dry-run).
    Written,
    /// No file injection for this agent; manual instructions printed.
    Manual,
    /// Step skipped because of an earlier error (not currently used).
    Skipped,
}

/// Run `fono agent-setup <agent_name>`.
///
/// * `agent_name`   — key from `agents.toml` (e.g. `"forge"`).
/// * `project_dir`  — directory for preset-file injection (AGENTS.md etc.).
/// * `dry_run`      — print actions but write nothing.
/// * `list_only`    — print all known agents and exit (ignores other args).
pub async fn run(
    agent_name: Option<&str>,
    project_dir: &Path,
    dry_run: bool,
    list_only: bool,
    paths: &Paths,
) -> Result<()> {
    if list_only {
        return print_agent_list(paths);
    }

    let name = agent_name.ok_or_else(|| {
        anyhow::anyhow!(
            "agent name required. Pass an agent name (e.g. `forge`) or use \
             `--list` to see all available agents."
        )
    })?;

    let entry = agents::find(name, paths)?;

    // Preflight: make sure the agent's launcher actually exists on
    // $PATH before we start writing config for it. Otherwise the user
    // gets a fully-wired Fono setup that talks to a binary they
    // haven't installed yet, which fails opaquely the next time they
    // run the agent. If the entry declares an `install_command`, we
    // offer to run it on their behalf.
    ensure_agent_installed(&entry, dry_run)?;

    if dry_run {
        eprintln!("(dry-run — nothing will be written)");
    }

    println!("Setting up Fono voice integration for {name}…\n");

    // ── Step 1: MCP server ────────────────────────────────────────────────────
    let r1 = step_enable_mcp(paths, dry_run).context("step 1: enable MCP server")?;
    print_step(1, 3, "MCP server    ", "fono use mcp-server on", paths.config_file(), r1);

    // ── Step 2: Agent MCP JSON ────────────────────────────────────────────────
    let mcp_path = expand_tilde(&entry.mcp_config_path);
    let r2 = step_merge_mcp_json(&mcp_path, dry_run).context("step 2: merge agent MCP JSON")?;
    print_step(2, 3, "Agent MCP JSON", &entry.mcp_config_path, &mcp_path, r2);

    // ── Step 3: Voice preset ──────────────────────────────────────────────────
    let r3 =
        step_inject_preset(&entry, project_dir, dry_run).context("step 3: inject voice preset")?;
    let preset_label = entry.preset_target_file().unwrap_or("(manual)");
    let preset_path = project_dir.join(preset_label);
    if r3 == StepResult::Manual {
        print_step(3, 3, "Voice preset  ", preset_label, &preset_path, StepResult::Manual);
        print_manual_preset_instructions(&entry, project_dir);
    } else {
        print_step(3, 3, "Voice preset  ", preset_label, &preset_path, r3);
    }

    println!();

    if dry_run {
        println!("Dry-run complete. Re-run without `--dry-run` to apply.");
        return Ok(());
    }

    // ── Preflight: the MCP entry we just wrote spawns `fono mcp serve`.
    // If `fono` is not on PATH, the agent will silently fail to load the
    // voice tools at runtime. Surface this loudly here rather than letting
    // the user discover it the hard way in a live voice session.
    if let Some(diag) = preflight_fono_on_path() {
        print_path_diagnostic(&diag);
        anyhow::bail!(
            "setup incomplete: `fono` is not reachable as written in the agent's mcp.json. \
             Fix the issue above and re-run `fono agent-setup {name}`."
        );
    }

    println!("Done. Start a voice session by launching `{name}` the way you normally do.");

    Ok(())
}

// ─── Step implementations ──────────────────────────────────────────────────────

/// Step 1: ensure `cfg.mcp.enabled = true`.
pub fn step_enable_mcp(paths: &Paths, dry_run: bool) -> Result<StepResult> {
    let path = paths.config_file();
    let mut cfg = Config::load(&path).unwrap_or_default();
    if cfg.mcp.enabled {
        return Ok(StepResult::AlreadyDone);
    }
    cfg.mcp.enabled = true;
    if !dry_run {
        cfg.save(&path).with_context(|| format!("saving config to {}", path.display()))?;
    }
    Ok(StepResult::Written)
}

/// Step 2: merge `{ "mcpServers": { "fono": { "command": "fono",
///   "args": ["mcp", "serve"] } } }` into the agent's MCP JSON file.
///
/// Creates the file (and parent directories) if it doesn't exist.
/// All other keys in the file are preserved.
pub fn step_merge_mcp_json(mcp_path: &Path, dry_run: bool) -> Result<StepResult> {
    // Load existing JSON or start fresh.
    let mut root: serde_json::Value = if mcp_path.exists() {
        let raw = std::fs::read_to_string(mcp_path)
            .with_context(|| format!("reading {}", mcp_path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("parsing JSON in {}", mcp_path.display()))?
    } else {
        serde_json::json!({})
    };

    // Ensure root is an object.
    if !root.is_object() {
        anyhow::bail!(
            "{} is not a JSON object — cannot safely merge the Fono MCP entry. \
             Please merge manually:\n{}",
            mcp_path.display(),
            fono_mcp_snippet()
        );
    }

    // Check if already configured (exact match).
    let desired = fono_mcp_entry();
    let current = root
        .get("mcpServers")
        .and_then(|m| m.get("fono"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    if current == desired {
        return Ok(StepResult::AlreadyDone);
    }

    // Merge: set `mcpServers.fono = desired`.
    root.as_object_mut()
        .expect("just checked")
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("`mcpServers` in {} is not an object", mcp_path.display()))?
        .insert("fono".to_string(), desired);

    if !dry_run {
        if let Some(parent) = mcp_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating directory {}", parent.display()))?;
        }
        let pretty = serde_json::to_string_pretty(&root)?;
        std::fs::write(mcp_path, pretty + "\n")
            .with_context(|| format!("writing {}", mcp_path.display()))?;
    }
    Ok(StepResult::Written)
}

/// Step 3: append the voice preset to the agent's preset file.
pub fn step_inject_preset(
    entry: &AgentEntry,
    project_dir: &Path,
    dry_run: bool,
) -> Result<StepResult> {
    let Some(file_name) = entry.preset_target_file() else {
        return Ok(StepResult::Manual);
    };

    let target = project_dir.join(file_name);

    // Check sentinel before reading (fast path).
    if target.exists() {
        let existing = std::fs::read_to_string(&target)
            .with_context(|| format!("reading {}", target.display()))?;
        if existing.contains(PRESET_SENTINEL) {
            return Ok(StepResult::AlreadyDone);
        }
    }

    if !dry_run {
        use std::io::Write;
        let block = format!("\n{PRESET_SENTINEL}\n## Voice mode (Fono)\n\n{VOICE_PRESET}\n<!-- /fono-voice-preset -->\n");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target)
            .with_context(|| format!("opening {} for append", target.display()))?;
        file.write_all(block.as_bytes())
            .with_context(|| format!("appending to {}", target.display()))?;
    }
    Ok(StepResult::Written)
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// The JSON value for `mcpServers.fono`.
fn fono_mcp_entry() -> serde_json::Value {
    serde_json::json!({
        "command": "fono",
        "args": ["mcp", "serve"]
    })
}

/// Human-readable snippet for manual instructions.
fn fono_mcp_snippet() -> String {
    r#"  {
    "mcpServers": {
      "fono": {
        "command": "fono",
        "args": ["mcp", "serve"]
      }
    }
  }"#
    .to_string()
}

/// Expand a leading `~` to `$HOME`.
pub fn expand_tilde(p: &str) -> PathBuf {
    p.strip_prefix("~/").map_or_else(
        || {
            if p == "~" {
                PathBuf::from(std::env::var("HOME").unwrap_or_default())
            } else {
                PathBuf::from(p)
            }
        },
        |rest| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(rest)
        },
    )
}

// ─── Agent install / presence check ───────────────────────────────────────────

/// Verify the agent's launcher (`entry.command[0]`) is on `$PATH`. If
/// it isn't, print a clear message and — when the agent registry
/// provides an `install_command` — prompt the user to run it.
///
/// Returns:
/// * `Ok(())` when the agent is already installed, OR was installed
///   successfully during this call.
/// * `Err(...)` when the agent is missing AND either has no install
///   command registered, the user declined, or the install command
///   ran but did not put the launcher on `$PATH`.
///
/// In `--dry-run` mode the missing-agent case prints what would be
/// done and returns `Ok(())` without invoking the installer.
pub fn ensure_agent_installed(entry: &AgentEntry, dry_run: bool) -> Result<()> {
    let Some(cmd) = entry.command.first() else {
        // No launcher declared at all — nothing meaningful to check.
        return Ok(());
    };
    let cmd = cmd.as_str();
    if cmd.is_empty() || find_command_on_path(cmd).is_some() {
        return Ok(());
    }

    eprintln!();
    eprintln!("Agent `{}` is not installed: `{cmd}` was not found on $PATH.", entry.name);
    if entry.install_command.is_empty() {
        anyhow::bail!(
            "No install command is registered for `{}`. Install it manually \
             (see docs/coding-agents.md), then re-run `fono agent-setup {}`.",
            entry.name,
            entry.name,
        );
    }
    eprintln!("Suggested install command:");
    eprintln!("  {}", entry.install_command);
    eprintln!();

    if dry_run {
        eprintln!("(dry-run — install command would be executed here)");
        return Ok(());
    }

    if !prompt_yes_no(&format!("Run the install command now for `{}`?", entry.name))? {
        anyhow::bail!(
            "setup aborted: install `{}` manually, then re-run `fono agent-setup {}`.",
            cmd,
            entry.name,
        );
    }

    run_install_command(&entry.install_command).with_context(|| {
        format!("install command for `{}` failed: {}", entry.name, entry.install_command)
    })?;

    // Re-check $PATH. Some installers drop their binaries into a
    // directory that only becomes effective in a fresh shell (e.g.
    // `~/.local/bin` added to the user's profile by the installer).
    // Be honest about that case rather than press on and confuse the
    // user later.
    if find_command_on_path(cmd).is_none() {
        anyhow::bail!(
            "install command finished, but `{cmd}` is still not on $PATH in the current \
             shell. Open a new terminal (or update your PATH) and re-run \
             `fono agent-setup {}`.",
            entry.name,
        );
    }
    eprintln!("Installed `{}` successfully.\n", entry.name);
    Ok(())
}

/// Locate `cmd` on `$PATH`, returning its canonical path on success.
/// Pure equivalent of `which`. Distinct from [`find_fono_on_path`] so
/// future changes to one cannot quietly break the other.
fn find_command_on_path(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    // `cfg!(windows)` instead of hard-coding so the same source
    // compiles cleanly cross-platform; agents.toml's commands are
    // declared without the `.exe` suffix.
    let with_ext = if cfg!(windows) { format!("{cmd}.exe") } else { cmd.to_string() };
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(&with_ext);
        if candidate.is_file() {
            return std::fs::canonicalize(&candidate).ok().or(Some(candidate));
        }
    }
    None
}

/// Print `question [y/N]:` to stderr and read a line from stdin.
/// Treats `y` / `yes` (case-insensitive) as yes; anything else as no.
/// EOF on stdin counts as no — keeps non-interactive callers from
/// accidentally auto-executing shell commands.
fn prompt_yes_no(question: &str) -> Result<bool> {
    use std::io::{BufRead, Write};
    eprint!("{question} [y/N]: ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    let stdin = std::io::stdin();
    let n = stdin.lock().read_line(&mut line).context("reading stdin")?;
    if n == 0 {
        return Ok(false);
    }
    let answer = line.trim().to_lowercase();
    Ok(matches!(answer.as_str(), "y" | "yes"))
}

/// Run `cmd` via the platform shell. Inherits stdin/stdout/stderr so
/// the installer can show progress and prompt for sudo etc. Returns
/// an error when the shell exits non-zero.
fn run_install_command(cmd: &str) -> Result<()> {
    let status = if cfg!(windows) {
        std::process::Command::new("cmd").arg("/C").arg(cmd).status()
    } else {
        std::process::Command::new("sh").arg("-c").arg(cmd).status()
    }
    .context("spawning shell for install command")?;
    if !status.success() {
        anyhow::bail!("install command exited with status {status}");
    }
    Ok(())
}

fn print_step(
    n: u8,
    total: u8,
    label: &str,
    display: impl std::fmt::Display,
    _path: impl AsRef<Path>,
    result: StepResult,
) {
    let mark = match result {
        StepResult::AlreadyDone => "✓ already configured",
        StepResult::Written => "✓ written",
        StepResult::Manual => "→ manual (see below)",
        StepResult::Skipped => "- skipped",
    };
    println!("  [{n}/{total}] {label:<15} {display:<32} {mark}");
}

fn print_manual_preset_instructions(entry: &AgentEntry, project_dir: &Path) {
    println!();
    println!("  Voice preset — manual step for {}:", entry.name);
    println!("  Add the following block to your agent's system prompt:");
    println!("  (file: {})", project_dir.display());
    println!();
    println!("  {PRESET_SENTINEL}");
    println!("  ## Voice mode (Fono)");
    println!();
    for line in VOICE_PRESET.lines() {
        println!("  {line}");
    }
    println!("  <!-- /fono-voice-preset -->");
}

// ─── PATH preflight ───────────────────────────────────────────────────────────

/// Diagnostic outcome of the post-setup PATH check.
///
/// `None` means everything is wired correctly: a `fono` executable is on
/// `$PATH` and it is the same binary that is currently running. The agent
/// will be able to spawn `fono mcp serve` at runtime.
#[derive(Debug, PartialEq, Eq)]
pub enum PathDiagnostic {
    /// No `fono` executable found anywhere on `$PATH`.
    NotOnPath { current_exe: Option<PathBuf> },
    /// A `fono` exists on `$PATH` but resolves to a different file than the
    /// currently running binary. The agent will use the PATH one, not this one.
    Mismatch { on_path: PathBuf, current_exe: PathBuf },
}

/// Returns `Some(diagnostic)` if the agent will not be able to spawn the
/// `fono` MCP server the way step 2 just wrote it, otherwise `None`.
pub fn preflight_fono_on_path() -> Option<PathDiagnostic> {
    let current = std::env::current_exe().ok().and_then(|p| std::fs::canonicalize(&p).ok());
    match find_fono_on_path() {
        None => Some(PathDiagnostic::NotOnPath { current_exe: current }),
        Some(on_path) => match current {
            Some(cur) if cur != on_path => {
                Some(PathDiagnostic::Mismatch { on_path, current_exe: cur })
            }
            _ => None,
        },
    }
}

/// Walk `$PATH` looking for an executable named `fono` (or `fono.exe` on
/// Windows). Returns the canonical path of the first match, if any.
fn find_fono_on_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exe_name = if cfg!(windows) { "fono.exe" } else { "fono" };
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(exe_name);
        if candidate.is_file() {
            return std::fs::canonicalize(&candidate).ok().or(Some(candidate));
        }
    }
    None
}

fn print_path_diagnostic(diag: &PathDiagnostic) {
    println!("WARNING — Fono is not on your $PATH the way the agent will look for it.");
    println!();
    match diag {
        PathDiagnostic::NotOnPath { current_exe } => {
            println!("  The agent's mcp.json now contains `\"command\": \"fono\"`, but no `fono`");
            println!("  executable was found on your $PATH. When the agent starts, the MCP");
            println!("  spawn will fail and the `fono.speak` / `fono.listen` tools will be");
            println!("  missing from the session — silently.");
            println!();
            println!("  Pick one of the following fixes, then re-run agent-setup:");
            println!();
            if let Some(exe) = current_exe {
                println!("    1) Symlink the current binary onto your PATH (quickest):");
                println!("         ln -sf {} ~/.local/bin/fono", exe.display());
                println!();
                println!("    2) Install via cargo (per-user, picks up future rebuilds):");
                println!("         cargo install --path crates/fono");
                println!();
                println!("    3) Edit the agent's mcp.json by hand and replace `\"fono\"` with");
                println!("       the absolute path: {}", exe.display());
            } else {
                println!("    - Install `fono` so that `which fono` resolves it, then re-run.");
            }
        }
        PathDiagnostic::Mismatch { on_path, current_exe } => {
            println!("  The `fono` on your PATH is NOT the binary you just ran.");
            println!("    on PATH:  {}", on_path.display());
            println!("    this run: {}", current_exe.display());
            println!();
            println!("  The agent will spawn the PATH copy at runtime. If that copy is older");
            println!("  than this one, MCP features added since then will be missing.");
            println!();
            println!("  Fix by replacing the PATH copy, e.g.:");
            println!("    ln -sf {} {}", current_exe.display(), on_path.display());
        }
    }
    println!();
}

fn print_agent_list(paths: &Paths) -> Result<()> {
    let agents = agents::load_all(paths)?;
    println!("{:<14} {:<28} {:<12} COMMAND", "AGENT", "MCP CONFIG", "PRESET");
    for a in &agents {
        let cmd = a.command.first().map(String::as_str).unwrap_or("-");
        println!(
            "{:<14} {:<28} {:<12} {}",
            a.name,
            if a.mcp_config_path.is_empty() { "-" } else { &a.mcp_config_path },
            if a.preset_injection.is_empty() { "none" } else { &a.preset_injection },
            cmd,
        );
    }
    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::agents::AgentEntry;

    fn fake_entry(injection: &str, mcp_path: &str) -> AgentEntry {
        AgentEntry {
            name: "test-agent".into(),
            command: vec!["test-agent".into()],
            args: vec![],
            mcp_config_path: mcp_path.into(),
            preset_injection: injection.into(),
            preset_file: String::new(),
            install_command: String::new(),
        }
    }

    // ── step_merge_mcp_json ───────────────────────────────────────────────────

    #[test]
    fn merge_creates_file_when_absent() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mcp.json");
        assert!(!p.exists());
        let r = step_merge_mcp_json(&p, false).unwrap();
        assert_eq!(r, StepResult::Written);
        assert!(p.exists());
        let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["fono"]["command"], "fono");
        assert_eq!(v["mcpServers"]["fono"]["args"][0], "mcp");
        assert_eq!(v["mcpServers"]["fono"]["args"][1], "serve");
    }

    #[test]
    fn merge_idempotent() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mcp.json");
        step_merge_mcp_json(&p, false).unwrap();
        let r = step_merge_mcp_json(&p, false).unwrap();
        assert_eq!(r, StepResult::AlreadyDone);
    }

    #[test]
    fn merge_preserves_other_servers() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mcp.json");
        fs::write(&p, r#"{"mcpServers":{"other":{"command":"other","args":[]}}}"#).unwrap();
        step_merge_mcp_json(&p, false).unwrap();
        let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        // Original entry preserved.
        assert_eq!(v["mcpServers"]["other"]["command"], "other");
        // Fono entry added.
        assert_eq!(v["mcpServers"]["fono"]["command"], "fono");
    }

    #[test]
    fn merge_dry_run_writes_nothing() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("sub").join("mcp.json");
        let r = step_merge_mcp_json(&p, true).unwrap();
        assert_eq!(r, StepResult::Written); // would write
        assert!(!p.exists()); // nothing actually written
    }

    // ── step_inject_preset ────────────────────────────────────────────────────

    #[test]
    fn inject_appends_to_new_file() {
        let dir = TempDir::new().unwrap();
        let entry = fake_entry("agents-md", "");
        let r = step_inject_preset(&entry, dir.path(), false).unwrap();
        assert_eq!(r, StepResult::Written);
        let content = fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(content.contains(PRESET_SENTINEL));
        assert!(content.contains("VOICE MODE"));
    }

    #[test]
    fn inject_appends_to_existing_file() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("AGENTS.md");
        fs::write(&target, "# My project\n\nSome existing content.\n").unwrap();
        let entry = fake_entry("agents-md", "");
        step_inject_preset(&entry, dir.path(), false).unwrap();
        let content = fs::read_to_string(&target).unwrap();
        assert!(content.starts_with("# My project"));
        assert!(content.contains(PRESET_SENTINEL));
    }

    #[test]
    fn inject_idempotent_no_double_sentinel() {
        let dir = TempDir::new().unwrap();
        let entry = fake_entry("agents-md", "");
        step_inject_preset(&entry, dir.path(), false).unwrap();
        let r2 = step_inject_preset(&entry, dir.path(), false).unwrap();
        assert_eq!(r2, StepResult::AlreadyDone);
        let content = fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert_eq!(content.matches(PRESET_SENTINEL).count(), 1);
    }

    #[test]
    fn inject_dry_run_no_file() {
        let dir = TempDir::new().unwrap();
        let entry = fake_entry("claude-md", "");
        let r = step_inject_preset(&entry, dir.path(), true).unwrap();
        assert_eq!(r, StepResult::Written);
        assert!(!dir.path().join("CLAUDE.md").exists());
    }

    #[test]
    fn inject_manual_returns_manual() {
        let dir = TempDir::new().unwrap();
        let entry = fake_entry("none", "");
        let r = step_inject_preset(&entry, dir.path(), false).unwrap();
        assert_eq!(r, StepResult::Manual);
    }

    #[test]
    fn inject_preset_file_override() {
        let dir = TempDir::new().unwrap();
        let mut entry = fake_entry("none", "");
        entry.preset_file = "SYSTEM.md".into(); // explicit override
        let r = step_inject_preset(&entry, dir.path(), false).unwrap();
        assert_eq!(r, StepResult::Written);
        assert!(dir.path().join("SYSTEM.md").exists());
    }

    // ── preflight_fono_on_path ────────────────────────────────────────────────

    #[test]
    fn preflight_reports_not_on_path_when_path_empty() {
        // Save and clobber PATH so the lookup is guaranteed to miss. Using a
        // single empty entry rather than unsetting PATH keeps other helpers
        // (e.g. `cargo`-spawned harnesses) sane.
        let saved = std::env::var_os("PATH");
        // SAFETY: tests in this crate are not run in parallel against shared env.
        unsafe { std::env::set_var("PATH", "/nonexistent-fono-preflight-dir") };
        let diag = preflight_fono_on_path();
        if let Some(prev) = saved {
            unsafe { std::env::set_var("PATH", prev) };
        } else {
            unsafe { std::env::remove_var("PATH") };
        }
        match diag {
            Some(PathDiagnostic::NotOnPath { .. }) => {}
            other => panic!("expected NotOnPath, got {other:?}"),
        }
    }

    // ── expand_tilde ──────────────────────────────────────────────────────────

    #[test]
    fn expand_tilde_replaces_home() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        let p = expand_tilde("~/.forge/mcp.json");
        assert_eq!(p, PathBuf::from(home).join(".forge/mcp.json"));
    }

    #[test]
    fn expand_tilde_no_op_for_absolute() {
        let p = expand_tilde("/absolute/path");
        assert_eq!(p, PathBuf::from("/absolute/path"));
    }

    // ── find_command_on_path / ensure_agent_installed ─────────────────────────

    #[test]
    fn find_command_on_path_misses_nonexistent_binary() {
        // PATH stays as-is; the bogus binary name guarantees a miss
        // without disturbing other tests that rely on $PATH.
        assert!(find_command_on_path("fono-definitely-not-a-real-binary-xyz").is_none());
    }

    #[test]
    fn ensure_agent_installed_ok_when_command_exists() {
        // `sh` is guaranteed on any Unix CI runner (cargo test) and
        // the only thing we need is "the lookup function finds
        // something". On Windows the test file is still compiled but
        // CI runs Linux-first, so guard the assertion.
        if cfg!(unix) {
            let entry = AgentEntry {
                name: "test".into(),
                command: vec!["sh".into()],
                args: vec![],
                mcp_config_path: String::new(),
                preset_injection: "none".into(),
                preset_file: String::new(),
                install_command: String::new(),
            };
            ensure_agent_installed(&entry, false).expect("sh must resolve on $PATH");
        }
    }

    #[test]
    fn ensure_agent_installed_fails_without_install_command() {
        let entry = AgentEntry {
            name: "test".into(),
            command: vec!["fono-definitely-not-a-real-binary-xyz".into()],
            args: vec![],
            mcp_config_path: String::new(),
            preset_injection: "none".into(),
            preset_file: String::new(),
            install_command: String::new(),
        };
        let err = ensure_agent_installed(&entry, false).expect_err("missing binary must error");
        let msg = format!("{err}");
        assert!(msg.contains("No install command"), "unexpected error: {msg}");
    }

    #[test]
    fn ensure_agent_installed_dry_run_does_not_execute() {
        let entry = AgentEntry {
            name: "test".into(),
            command: vec!["fono-definitely-not-a-real-binary-xyz".into()],
            args: vec![],
            mcp_config_path: String::new(),
            preset_injection: "none".into(),
            preset_file: String::new(),
            // Anything that would obviously fail if executed; in
            // dry-run we must return Ok without running it.
            install_command: "exit 1".into(),
        };
        ensure_agent_installed(&entry, true).expect("dry-run must short-circuit");
    }
}
