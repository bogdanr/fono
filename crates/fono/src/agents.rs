// SPDX-License-Identifier: GPL-3.0-only
//! Agent registry — shared TOML loader for `agent-setup`.
//!
//! Agent-specific knowledge lives entirely in data (`agents.toml`) — never
//! in Rust code. Adding a new agent means adding an `[[agent]]` entry to
//! `~/.config/fono/agents.toml` or to the bundled `assets/agents.toml`;
//! no Fono source changes are required.

use anyhow::{anyhow, Context, Result};
use fono_core::Paths;
use serde::Deserialize;

/// Bundled first-party agent registry shipped with the binary.
/// Users can override or extend by placing their own `agents.toml`
/// at `~/.config/fono/agents.toml`.
pub const BUNDLED_AGENTS_TOML: &str = include_str!("../../../assets/agents.toml");

/// A single `[[agent]]` entry from the registry.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentEntry {
    /// Unique key; used with `fono agent-setup <agent>`.
    pub name: String,
    /// The command to run (argv\[0\]) — documentation only. Fono does
    /// not spawn the agent for you; after `fono agent-setup` finishes
    /// the user launches the agent the normal way.
    pub command: Vec<String>,
    /// Extra CLI args (documentation only; not invoked by Fono).
    #[serde(default)]
    pub args: Vec<String>,
    /// Path to the agent's MCP config file. `fono agent-setup` writes
    /// the `mcpServers.fono` snippet here.
    /// Supports `~` for `$HOME`.
    #[serde(default)]
    pub mcp_config_path: String,
    /// How the voice preset reaches the agent:
    /// `"agents-md"` — append to `AGENTS.md` in the project directory.
    /// `"claude-md"` — append to `CLAUDE.md` in the project directory.
    /// `"cli-flag"`  — passed as a CLI argument (see `args`); no file write.
    /// `"none"` / `"manual"` — print manual instructions.
    #[serde(default)]
    pub preset_injection: String,
    /// Override the file that receives the voice preset during
    /// `fono agent-setup`.  Defaults to `"AGENTS.md"` for `"agents-md"`,
    /// `"CLAUDE.md"` for `"claude-md"`, and empty for everything else.
    /// Advanced users can set this to any relative path without touching
    /// `agent_setup.rs`.
    #[serde(default)]
    pub preset_file: String,
    /// Optional shell command to install the agent when it isn't yet on
    /// `$PATH`. Executed via `sh -c "<install_command>"` (or
    /// `cmd /C` on Windows) after the user confirms at the prompt.
    /// Leave empty when no canonical install path exists (e.g. GUI-only
    /// editors) — `fono agent-setup` will then just print a "not
    /// installed" message and bail.
    #[serde(default)]
    pub install_command: String,
}

impl AgentEntry {
    /// Return the file (relative to the project directory) that should
    /// receive the injected voice preset, or `None` when no file injection
    /// is needed for this entry's `preset_injection` mechanism.
    pub fn preset_target_file(&self) -> Option<&str> {
        if !self.preset_file.is_empty() {
            return Some(&self.preset_file);
        }
        match self.preset_injection.as_str() {
            "agents-md" => Some("AGENTS.md"),
            "claude-md" => Some("CLAUDE.md"),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct AgentsFile {
    #[serde(rename = "agent")]
    pub agents: Vec<AgentEntry>,
}

/// Load all agent entries visible to the user: user file first (takes
/// precedence for duplicate names), then bundled fallback.
pub fn load_all(paths: &Paths) -> Result<Vec<AgentEntry>> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();

    // User override file.
    let user_path = paths.config_dir.join("agents.toml");
    if user_path.exists() {
        let raw = std::fs::read_to_string(&user_path)
            .with_context(|| format!("reading {}", user_path.display()))?;
        let file: AgentsFile =
            toml::from_str(&raw).with_context(|| format!("parsing {}", user_path.display()))?;
        for e in file.agents {
            seen.insert(e.name.clone());
            out.push(e);
        }
    }

    // Bundled fallback — skip names already provided by the user file.
    let file: AgentsFile =
        toml::from_str(BUNDLED_AGENTS_TOML).context("parsing bundled assets/agents.toml")?;
    for e in file.agents {
        if !seen.contains(&e.name) {
            out.push(e);
        }
    }
    Ok(out)
}

/// Match an entry by its registry name **or** by the launcher binary
/// in `command[0]` (case-insensitive). Lets users type the friendly
/// `claude` for the `claude-code` entry without inventing aliases.
fn entry_matches(entry: &AgentEntry, query: &str) -> bool {
    if entry.name.eq_ignore_ascii_case(query) {
        return true;
    }
    entry.command.first().map(|c| c.eq_ignore_ascii_case(query)).unwrap_or(false)
}

/// Find a single agent by name or launcher.  User file takes precedence
/// over bundled registry.
pub fn find(name: &str, paths: &Paths) -> Result<AgentEntry> {
    // User override.
    let user_path = paths.config_dir.join("agents.toml");
    if user_path.exists() {
        let raw = std::fs::read_to_string(&user_path)
            .with_context(|| format!("reading {}", user_path.display()))?;
        let file: AgentsFile =
            toml::from_str(&raw).with_context(|| format!("parsing {}", user_path.display()))?;
        if let Some(e) = file.agents.into_iter().find(|a| entry_matches(a, name)) {
            return Ok(e);
        }
    }

    // Bundled fallback.
    let file: AgentsFile =
        toml::from_str(BUNDLED_AGENTS_TOML).context("parsing bundled assets/agents.toml")?;
    file.agents.into_iter().find(|a| entry_matches(a, name)).ok_or_else(|| {
        anyhow!(
            "agent `{name}` not found.\n\
             Add an `[[agent]]` entry to ~/.config/fono/agents.toml or run \
             `fono agent-setup --list` to see available agents."
        )
    })
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_toml_parses() {
        let f: AgentsFile = toml::from_str(BUNDLED_AGENTS_TOML).expect("must parse");
        assert!(!f.agents.is_empty());
    }

    #[test]
    fn bundled_has_required_entries() {
        let f: AgentsFile = toml::from_str(BUNDLED_AGENTS_TOML).unwrap();
        let names: Vec<_> = f.agents.iter().map(|a| a.name.as_str()).collect();
        for required in ["forge", "claude-code", "cursor"] {
            assert!(names.contains(&required), "missing bundled entry: {required}");
        }
    }

    #[test]
    fn preset_target_file_derived_correctly() {
        let mut e = AgentEntry {
            name: "test".into(),
            command: vec!["test".into()],
            args: vec![],
            mcp_config_path: String::new(),
            preset_injection: "agents-md".into(),
            preset_file: String::new(),
            install_command: String::new(),
        };
        assert_eq!(e.preset_target_file(), Some("AGENTS.md"));
        e.preset_injection = "claude-md".into();
        assert_eq!(e.preset_target_file(), Some("CLAUDE.md"));
        e.preset_injection = "none".into();
        assert_eq!(e.preset_target_file(), None);
        // Explicit override wins.
        e.preset_file = "SYSTEM.md".into();
        assert_eq!(e.preset_target_file(), Some("SYSTEM.md"));
    }

    #[test]
    fn entry_matches_name_and_launcher() {
        let e = AgentEntry {
            name: "claude-code".into(),
            command: vec!["claude".into()],
            args: vec![],
            mcp_config_path: String::new(),
            preset_injection: "claude-md".into(),
            preset_file: String::new(),
            install_command: String::new(),
        };
        assert!(entry_matches(&e, "claude-code"));
        assert!(entry_matches(&e, "claude"));
        assert!(entry_matches(&e, "CLAUDE")); // case-insensitive
        assert!(!entry_matches(&e, "forge"));
    }
}
