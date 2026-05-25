// SPDX-License-Identifier: GPL-3.0-only
//! Terminal deep-enrichment via `/proc` walking (Phase C).
//!
//! [`terminal_context`] reads the process tree rooted at the terminal-emulator
//! PID to determine the shell's current working directory and any coding agent
//! running as a shell child.  All reads are best-effort; any I/O error or
//! missing file silently falls back to the default.
//!
//! The implementation is gated on Linux because it relies on the `/proc`
//! filesystem.  On other platforms [`terminal_context`] returns the default
//! `TerminalContext` immediately.

use crate::classifier::{CodingAgentKind, ProjectKind, TerminalContext};

// ── Capability gate (C.4) ─────────────────────────────────────────────────────

/// Returns `true` if `/proc`-based terminal enrichment is available.
///
/// Performs a single `read_link` on the calling process's own `/proc/self/cwd`
/// symlink; this is readable by the process owner on all Linux kernels ≥ 2.6.
/// Returns `false` on non-Linux platforms and on any permission error.
pub fn proc_enrichment_available() -> bool {
    std::fs::read_link(format!("/proc/{}/cwd", std::process::id())).is_ok()
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Derive terminal context by walking `/proc` from `terminal_pid`.
///
/// 1. Finds the shell child of the terminal emulator.
/// 2. Determines the project type from the shell's CWD.
/// 3. Detects any coding agent running as a grandchild.
///
/// Every I/O failure is silently ignored; the function never panics and
/// never returns an error.
#[cfg(target_os = "linux")]
pub fn terminal_context(terminal_pid: u32) -> TerminalContext {
    let Some(shell_pid) = find_shell_child(terminal_pid) else {
        return TerminalContext { project: ProjectKind::Shell, agent: None };
    };

    let project = project_from_cwd(shell_pid);
    let agent = detect_coding_agent(shell_pid);

    TerminalContext { project, agent }
}

/// On non-Linux platforms return the default immediately.
#[cfg(not(target_os = "linux"))]
pub fn terminal_context(_terminal_pid: u32) -> TerminalContext {
    TerminalContext { project: ProjectKind::Shell, agent: None }
}

// ── Step 1: find shell child ──────────────────────────────────────────────────

const KNOWN_SHELLS: &[&str] = &["bash", "zsh", "fish", "sh", "dash", "ksh", "tcsh", "nush"];

/// Read `/proc/{pid}/task/{pid}/children` and return the parsed PIDs.
#[cfg(target_os = "linux")]
fn read_children_file(pid: u32) -> Vec<u32> {
    std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|s| s.parse::<u32>().ok())
        .collect()
}

/// Fallback: scan `/proc` for processes whose `ppid` equals `parent_pid`.
///
/// Used only when `/proc/{pid}/task/{pid}/children` is absent (kernels < 3.5
/// or non-standard configurations).
#[cfg(target_os = "linux")]
fn scan_children_fallback(parent_pid: u32) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else { return Vec::new() };
    let mut children = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let Ok(pid) = name_str.parse::<u32>() else { continue };
        let stat_path = format!("/proc/{pid}/stat");
        let Ok(stat) = std::fs::read_to_string(stat_path) else { continue };
        // `/proc/{pid}/stat` format: `pid (comm) state ppid …`
        // The `comm` field may contain spaces and is wrapped in parens.
        // Skip past the last `)` to reach the remaining numeric fields.
        let after_comm = match stat.rfind(')') {
            Some(pos) => &stat[pos + 1..],
            None => continue,
        };
        let mut fields = after_comm.split_whitespace();
        let _state = fields.next();
        let ppid_str = fields.next().unwrap_or("");
        if ppid_str.parse::<u32>().ok() == Some(parent_pid) {
            children.push(pid);
        }
    }
    children
}

/// Return child PIDs of `pid`: prefer the `children` file; fall back to scan.
#[cfg(target_os = "linux")]
fn get_children(pid: u32) -> Vec<u32> {
    let from_file = read_children_file(pid);
    if from_file.is_empty() {
        scan_children_fallback(pid)
    } else {
        from_file
    }
}

/// Read `/proc/{pid}/comm` and return the trimmed process name.
#[cfg(target_os = "linux")]
fn read_comm(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_owned())
}

/// Find the first shell child of `terminal_pid`.
#[cfg(target_os = "linux")]
fn find_shell_child(terminal_pid: u32) -> Option<u32> {
    for child_pid in get_children(terminal_pid) {
        if let Some(comm) = read_comm(child_pid) {
            if KNOWN_SHELLS.contains(&comm.as_str()) {
                return Some(child_pid);
            }
        }
    }
    None
}

// ── Step 2: project type from CWD ────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn project_from_cwd(shell_pid: u32) -> ProjectKind {
    let Ok(cwd) = std::fs::read_link(format!("/proc/{shell_pid}/cwd")) else {
        return ProjectKind::Shell;
    };

    // K8s: KUBECONFIG env var present and non-empty takes highest precedence.
    if has_kubeconfig_in_environ(shell_pid) {
        return ProjectKind::K8s;
    }

    // File-marker checks — first match wins.
    let markers: &[(&[&str], ProjectKind)] = &[
        (&["docker-compose.yml", "docker-compose.yaml", "Dockerfile"], ProjectKind::Docker),
        (&["Cargo.toml"], ProjectKind::Rust),
        (&["pyproject.toml", "setup.py", "setup.cfg"], ProjectKind::Python),
        (&["package.json"], ProjectKind::Node),
        (&["go.mod"], ProjectKind::Go),
        (&[".git"], ProjectKind::Git),
    ];

    for (files, kind) in markers {
        if files.iter().any(|f| cwd.join(f).exists()) {
            return *kind;
        }
    }

    ProjectKind::Shell
}

/// Return `true` if `KUBECONFIG` is set to a non-empty value in the process
/// environment exposed via `/proc/{pid}/environ`.
#[cfg(target_os = "linux")]
fn has_kubeconfig_in_environ(pid: u32) -> bool {
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/environ")) else { return false };
    raw.split(|&b| b == 0)
        .filter_map(|entry| std::str::from_utf8(entry).ok())
        .any(|entry| entry.starts_with("KUBECONFIG=") && entry.len() > "KUBECONFIG=".len())
}

// ── Step 3: coding-agent detection ───────────────────────────────────────────

const INTERPRETER_COMMS: &[&str] = &["node", "python3", "python", "deno"];

/// Read `/proc/{pid}/cmdline`; returns argv split on NUL bytes.
#[cfg(target_os = "linux")]
fn read_cmdline(pid: u32) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .unwrap_or_default()
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// Scan the children of `shell_pid` for a known coding agent.
///
/// Returns the first detected [`CodingAgentKind`], or `None` if no agent is
/// found.  All reads fail silently.
#[cfg(target_os = "linux")]
fn detect_coding_agent(shell_pid: u32) -> Option<CodingAgentKind> {
    for grandchild_pid in get_children(shell_pid) {
        let Some(comm) = read_comm(grandchild_pid) else { continue };

        // 3a: direct binary-name match (covers all native binaries).
        let direct = match comm.as_str() {
            "forge" => Some(CodingAgentKind::Forge),
            "claude" => Some(CodingAgentKind::ClaudeCode),
            "codex" => Some(CodingAgentKind::Codex),
            "aider" => Some(CodingAgentKind::Aider),
            "goose" => Some(CodingAgentKind::Goose),
            "gemini" => Some(CodingAgentKind::GeminiCli),
            "amp" => Some(CodingAgentKind::Amp),
            "cursor" => Some(CodingAgentKind::Cursor),
            "qchat" => Some(CodingAgentKind::AmazonQ),
            _ => None,
        };
        if direct.is_some() {
            return direct;
        }

        // 3b: interpreter — inspect cmdline for agent-specific fragments.
        if INTERPRETER_COMMS.contains(&comm.as_str()) {
            let cmdline = read_cmdline(grandchild_pid);
            let cmdline_lower = cmdline.join(" ").to_ascii_lowercase();
            if cmdline_lower.contains("claude") {
                return Some(CodingAgentKind::ClaudeCode);
            }
            if cmdline_lower.contains("codex") {
                return Some(CodingAgentKind::Codex);
            }
            if cmdline_lower.contains("aider") {
                return Some(CodingAgentKind::Aider);
            }
            if cmdline_lower.contains("@google/gemini") || cmdline_lower.contains("/gemini/") {
                return Some(CodingAgentKind::GeminiCli);
            }
            continue;
        }

        // 3c: `gh` — only Copilot subcommand qualifies.
        if comm == "gh" {
            let cmdline = read_cmdline(grandchild_pid);
            if cmdline.iter().any(|arg| arg.contains("copilot")) {
                return Some(CodingAgentKind::GithubCopilot);
            }
            continue;
        }

        // 3d: `q` — cmdline disambiguates Amazon Q from other single-char binaries.
        if comm == "q" {
            let cmdline = read_cmdline(grandchild_pid);
            let full = cmdline.join(" ");
            if full.contains("amazon-q") || full.contains("aws/q") || cmdline.len() <= 1 {
                return Some(CodingAgentKind::AmazonQ);
            }
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn proc_enrichment_available_returns_true_on_linux() {
        // We are running on Linux inside a test, so /proc/self/cwd must exist.
        assert!(proc_enrichment_available());
    }

    #[test]
    fn self_terminal_context_does_not_panic() {
        // Calling terminal_context on our own PID should not panic, even if
        // no shell child is found — it must return Shell gracefully.
        let ctx = terminal_context(std::process::id());
        // We cannot assert the project kind (depends on test runner), but the
        // call itself must succeed and detected_agent is expected to be None.
        let _ = ctx;
    }

    #[test]
    fn known_shells_are_checked() {
        assert!(KNOWN_SHELLS.contains(&"bash"));
        assert!(KNOWN_SHELLS.contains(&"zsh"));
        assert!(KNOWN_SHELLS.contains(&"fish"));
        assert!(KNOWN_SHELLS.contains(&"nush"));
    }
}
