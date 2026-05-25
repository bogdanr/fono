// SPDX-License-Identifier: GPL-3.0-only
//! Built-in context classifier for hover-context injection.
//!
//! Classifies a focused window (by X11/Wayland class name and title) into a
//! [`ContextProfile`] that carries Whisper and LLM prompt enrichments.
//! The classifier is zero-config: it works out-of-the-box with a static
//! built-in rule table and requires no file I/O or heap allocation.

use std::borrow::Cow;

// ── Types ────────────────────────────────────────────────────────────────────

/// Identifies a known coding-agent process running inside a terminal emulator.
///
/// Detection happens in Phase C (terminal deep enrichment via `/proc`).
/// In Phase A the field is always `None`.
///
/// The enum is `#[non_exhaustive]` so new agents can be added without
/// breaking existing match arms elsewhere in the codebase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CodingAgentKind {
    Forge,
    ClaudeCode,
    Codex,
    Aider,
    Goose,
    GeminiCli,
    Amp,
    GithubCopilot,
    AmazonQ,
    Cursor,
}

/// Identifies the type of project in the shell's current working directory.
///
/// Populated during Phase C terminal deep enrichment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectKind {
    Shell,
    Git,
    Rust,
    Python,
    Node,
    Go,
    K8s,
    Docker,
}

/// Runtime context enrichment profile produced by [`ContextClassifier::classify`].
///
/// Never serialised; never stored in config. This is a purely ephemeral,
/// per-session value captured at hotkey-press time.
#[derive(Debug, Clone)]
pub struct ContextProfile {
    /// Human-readable profile name used in debug logs (e.g. `"Terminal"`,
    /// `"Browser"`, `"Private"`). Always a static string.
    pub name: &'static str,
    /// Short vocabulary hint injected into the Whisper `initial_prompt`.
    /// `None` means no enrichment (base language prompt only).
    ///
    /// `Cow` allows static strings from the built-in rule table to be used
    /// without allocation, while still permitting owned strings when Phase C
    /// terminal enrichment appends project-specific tokens at runtime.
    pub whisper_hint: Option<Cow<'static, str>>,
    /// Additional instruction appended to the LLM polish system prompt.
    /// Only set for contexts where we are confident about the right framing
    /// (currently terminal only). `None` means no enrichment.
    pub llm_suffix: Option<&'static str>,
    /// When `true`, skip writing the transcription to the SQLite history DB
    /// and skip the `redact_secrets` pass (Phase G).
    pub suppress_history: bool,
    /// Coding agent detected in the terminal (Phase C). Always `None` in Phase A.
    pub detected_agent: Option<CodingAgentKind>,
    /// `true` when the profile was produced by the `Terminal` built-in rule.
    /// Used in Phase E to gate the `/proc` deep-enrichment path.
    pub is_terminal: bool,
    /// `true` when the profile was produced by the `CodeEditor` built-in rule.
    /// Used in Phase F to trigger file-extension refinement from the window title.
    pub is_code_editor: bool,
}

/// Terminal context derived from `/proc` walking (Phase C).
///
/// Bundled here alongside [`CodingAgentKind`] so the types are co-located.
#[derive(Debug, Clone)]
pub struct TerminalContext {
    pub project: ProjectKind,
    pub agent: Option<CodingAgentKind>,
}

// ── Built-in rule table ───────────────────────────────────────────────────────

/// A single entry in the built-in classifier rule table.
pub struct BuiltinRule {
    /// Window class names matched case-insensitively (exact match).
    pub classes: &'static [&'static str],
    /// Optional window-title substrings.  A non-empty slice means *at least
    /// one* fragment must appear (case-insensitive) for the rule to fire.
    /// An empty slice means the rule fires on class match alone.
    pub title_fragments: &'static [&'static str],
    /// Factory that returns the profile for this rule.
    pub profile: fn() -> ContextProfile,
}

fn terminal_profile() -> ContextProfile {
    ContextProfile {
        name: "Terminal",
        whisper_hint: Some(Cow::Borrowed(
            "ls -la, grep -r, chmod 755, git commit, sudo apt install, \
             cd /etc, rm -rf, | grep, > /dev/null, ./script.sh, ssh user@host",
        )),
        llm_suffix: Some(
            "The user is dictating shell commands. Output as shell syntax. \
             Use lowercase. Preserve flags verbatim (e.g. -rf, -la, --verbose). \
             Convert spoken paths to filesystem notation \
             (\"home dot config\" → ~/.config, \"dot slash\" → ./, \
             \"pipe\" → |, \"redirect\" → >, \"dev null\" → /dev/null). \
             No prose punctuation. Do not capitalise the first word.",
        ),
        suppress_history: false,
        detected_agent: None,
        is_terminal: true,
        is_code_editor: false,
    }
}
fn code_editor_profile() -> ContextProfile {
    ContextProfile {
        name: "CodeEditor",
        whisper_hint: Some(Cow::Borrowed(
            "function, struct, impl, async, await, const, return, println!, \
             cargo build, git diff",
        )),
        // LLM suffix omitted: code editors are used for too many things
        // (prose comments, READMEs, commit messages) — we are not confident
        // enough in a single framing to apply it unconditionally.
        llm_suffix: None,
        suppress_history: false,
        detected_agent: None,
        is_terminal: false,
        is_code_editor: true,
    }
}

fn text_editor_profile() -> ContextProfile {
    ContextProfile {
        name: "TextEditor",
        whisper_hint: None::<Cow<'static, str>>,
        llm_suffix: None,
        suppress_history: false,
        detected_agent: None,
        is_terminal: false,
        is_code_editor: false,
    }
}

fn browser_profile() -> ContextProfile {
    ContextProfile {
        name: "Browser",
        whisper_hint: None::<Cow<'static, str>>,
        // No LLM suffix: browsers are used for email, chat, documents —
        // we cannot assume the user's intent from the window class alone.
        llm_suffix: None,
        suppress_history: false,
        detected_agent: None,
        is_terminal: false,
        is_code_editor: false,
    }
}

fn email_profile() -> ContextProfile {
    ContextProfile {
        name: "Email",
        whisper_hint: None::<Cow<'static, str>>,
        // No LLM suffix: dedicated email clients are rare and the right
        // formality level varies too much by use-case to inject blindly.
        llm_suffix: None,
        suppress_history: false,
        detected_agent: None,
        is_terminal: false,
        is_code_editor: false,
    }
}

fn chat_profile() -> ContextProfile {
    ContextProfile {
        name: "Chat",
        whisper_hint: None::<Cow<'static, str>>,
        // No LLM suffix: the same chat app is used for formal and informal
        // messages — we cannot confidently pick a register.
        llm_suffix: None,
        suppress_history: false,
        detected_agent: None,
        is_terminal: false,
        is_code_editor: false,
    }
}

fn spreadsheet_profile() -> ContextProfile {
    ContextProfile {
        name: "Spreadsheet",
        whisper_hint: None::<Cow<'static, str>>,
        llm_suffix: None,
        suppress_history: false,
        detected_agent: None,
        is_terminal: false,
        is_code_editor: false,
    }
}

fn document_profile() -> ContextProfile {
    ContextProfile {
        name: "Document",
        whisper_hint: None::<Cow<'static, str>>,
        llm_suffix: None,
        suppress_history: false,
        detected_agent: None,
        is_terminal: false,
        is_code_editor: false,
    }
}

fn private_profile() -> ContextProfile {
    ContextProfile {
        name: "Private",
        whisper_hint: None::<Cow<'static, str>>,
        llm_suffix: None,
        suppress_history: true,
        detected_agent: None,
        is_terminal: false,
        is_code_editor: false,
    }
}

/// Static built-in rule table.  First matching rule wins.
pub static BUILTIN_RULES: &[BuiltinRule] = &[
    // Terminal emulators
    BuiltinRule {
        classes: &[
            "Alacritty",
            "kitty",
            "gnome-terminal",
            "konsole",
            "xterm",
            "urxvt",
            "foot",
            "wezterm",
            "tilix",
            "st-256color",
            "terminator",
            "xfce4-terminal",
            "lxterminal",
            "sakura",
            "rxvt-unicode",
        ],
        title_fragments: &[],
        profile: terminal_profile,
    },
    // Code editors — ordered roughly by current popularity.
    // "Cursor" (the AI IDE) window class is "Cursor" on Linux/X11.
    BuiltinRule {
        classes: &[
            "Cursor", "cursor", "zed", "code", "code-oss", "vscodium", "codium", "kate", "lapce",
            "helix", "neovide", "windsurf",
        ],
        title_fragments: &[],
        profile: code_editor_profile,
    },
    // Text editors (plain prose)
    BuiltinRule {
        classes: &["gedit", "mousepad", "xed", "pluma", "geany"],
        title_fragments: &[],
        profile: text_editor_profile,
    },
    // Web browsers
    BuiltinRule {
        classes: &[
            "firefox",
            "chromium",
            "google-chrome",
            "brave-browser",
            "librewolf",
            "falkon",
            "epiphany",
        ],
        title_fragments: &[],
        profile: browser_profile,
    },
    // Email clients
    BuiltinRule {
        classes: &["thunderbird", "evolution", "kmail", "geary"],
        title_fragments: &[],
        profile: email_profile,
    },
    // Chat / messaging
    BuiltinRule {
        classes: &["slack", "discord", "telegram-desktop", "signal-desktop", "element", "fractal"],
        title_fragments: &[],
        profile: chat_profile,
    },
    // Spreadsheets
    BuiltinRule {
        classes: &["libreoffice-calc", "gnumeric"],
        title_fragments: &[],
        profile: spreadsheet_profile,
    },
    // Documents / word processors
    BuiltinRule {
        classes: &["libreoffice-writer", "abiword"],
        title_fragments: &[],
        profile: document_profile,
    },
    // Private / sensitive (suppress_history = true, no hints)
    BuiltinRule {
        classes: &["keepassxc", "bitwarden", "1password", "gnome-keyring", "seahorse"],
        title_fragments: &[],
        profile: private_profile,
    },
];

// ── Classifier ───────────────────────────────────────────────────────────────

/// Stateless classifier; all state is in the `BUILTIN_RULES` static table.
pub struct ContextClassifier;

impl ContextClassifier {
    /// Apply terminal deep-enrichment to `profile` using the result of a
    /// `/proc` walk.
    ///
    /// - Appends project-type-specific vocabulary tokens to `profile.whisper_hint`
    ///   (e.g. `cargo build, cargo clippy` for a Rust project).
    /// - Stores the detected coding agent in `profile.detected_agent`.
    ///
    /// Called after [`ContextClassifier::classify`] when the focused window is
    /// a terminal emulator and `window_pid` is available (Phase C).
    pub fn enrich_terminal(profile: &mut ContextProfile, ctx: &TerminalContext) {
        let addition: Option<&'static str> = match ctx.project {
            ProjectKind::Rust => Some("cargo build, cargo test, cargo clippy, rustc, --release"),
            ProjectKind::Python => Some("python3, pip install, pytest, virtualenv, uv run"),
            ProjectKind::Node => Some("npm install, npx, yarn, node_modules, package.json"),
            ProjectKind::K8s => Some("kubectl apply, kubectl get pods, helm install, namespace"),
            ProjectKind::Docker => Some("docker build, docker run, docker compose up, --rm"),
            ProjectKind::Git => Some("git commit, git push, git rebase, git stash, --amend"),
            ProjectKind::Go | ProjectKind::Shell => None,
        };

        if let Some(extra) = addition {
            let enriched = profile
                .whisper_hint
                .as_ref()
                .map_or_else(|| extra.to_owned(), |existing| format!("{existing} {extra}"));
            profile.whisper_hint = Some(Cow::Owned(enriched));
        }

        profile.detected_agent = ctx.agent;
    }

    /// Classify a focused window into a [`ContextProfile`].
    ///
    /// - `window_class`: the WM_CLASS / app_id string (case-insensitive match).
    /// - `window_title`: the window title (case-insensitive substring match against
    ///   `title_fragments`; ignored when the rule's `title_fragments` is empty).
    ///
    /// Returns `None` when no built-in rule matches (unknown app) — the caller
    /// should treat this as "no enrichment, use base prompts".
    pub fn classify(
        window_class: Option<&str>,
        window_title: Option<&str>,
    ) -> Option<ContextProfile> {
        let class = window_class?;
        for rule in BUILTIN_RULES {
            let class_matches = rule.classes.iter().any(|&c| c.eq_ignore_ascii_case(class));
            if !class_matches {
                continue;
            }
            // If the rule has title fragments, at least one must match.
            if !rule.title_fragments.is_empty() {
                let title = window_title.unwrap_or("");
                let title_lower = title.to_ascii_lowercase();
                let title_matches = rule
                    .title_fragments
                    .iter()
                    .any(|&frag| title_lower.contains(&frag.to_ascii_lowercase()));
                if !title_matches {
                    continue;
                }
            }
            let mut profile = (rule.profile)();
            // Phase F: refine CodeEditor profiles based on file extension in title.
            if profile.is_code_editor {
                if let Some(title) = window_title {
                    refine_editor_profile(&mut profile, title);
                }
            }
            return Some(profile);
        }
        None
    }
}

/// Phase F: Refine a `CodeEditor` profile based on file extension detected in
/// the window title.
///
/// Code editors reliably expose the open file name in their window title
/// (e.g. `main.rs — Visual Studio Code`, `kate — ~/project/src/lib.py`).
/// This function scans the title for known extensions and sharpens the
/// `whisper_hint` and `llm_suffix` accordingly.
///
/// For prose formats (`.md`, `.rst`) the hint is cleared and a full-
/// punctuation suffix is applied. For all others the base `CodeEditor`
/// profile's `llm_suffix` is kept and only `whisper_hint` is replaced.
fn refine_editor_profile(profile: &mut ContextProfile, title: &str) {
    let title_lower = title.to_ascii_lowercase();

    // Check extensions from most to least specific so `.tsx` wins over `.ts`.
    let ext: Option<&str> = if title_lower.contains(".tsx") {
        Some("tsx")
    } else if title_lower.contains(".jsx") {
        Some("jsx")
    } else if title_lower.contains(".ts") && !title_lower.contains(".rst") {
        Some("ts")
    } else if title_lower.contains(".rs") {
        Some("rs")
    } else if title_lower.contains(".py") {
        Some("py")
    } else if title_lower.contains(".js") {
        Some("js")
    } else if title_lower.contains(".go") {
        Some("go")
    } else if title_lower.contains(".kt") {
        Some("kt")
    } else if title_lower.contains(".java") {
        Some("java")
    } else if title_lower.contains(".sql") {
        Some("sql")
    } else if title_lower.contains(".rst") {
        Some("rst")
    } else if title_lower.contains(".md") {
        Some("md")
    } else {
        None
    };

    match ext {
        Some("rs") => {
            profile.whisper_hint = Some(Cow::Borrowed(
                "fn, let mut, impl, pub struct, cargo build, cargo clippy, rustc",
            ));
        }
        Some("py") => {
            profile.whisper_hint =
                Some(Cow::Borrowed("def, class, import, async def, pip install, pytest, __init__"));
        }
        Some("ts" | "tsx" | "js" | "jsx") => {
            profile.whisper_hint = Some(Cow::Borrowed(
                "const, function, async, await, npm install, interface, export default",
            ));
        }
        Some("go") => {
            profile.whisper_hint =
                Some(Cow::Borrowed("func, package main, go build, go test, goroutine, defer"));
        }
        Some("java" | "kt") => {
            profile.whisper_hint =
                Some(Cow::Borrowed("public class, void, implements, extends, gradle, maven"));
        }
        Some("sql") => {
            profile.whisper_hint = Some(Cow::Borrowed(
                "SELECT, FROM, WHERE, JOIN, GROUP BY, INSERT INTO, CREATE TABLE",
            ));
        }
        Some("md" | "rst") => {
            // Prose: no special vocabulary; use full punctuation.
            profile.whisper_hint = None;
            profile.llm_suffix = Some(
                "The user is dictating prose or documentation. \
                 Use full punctuation and standard English capitalisation.",
            );
        }
        _ => {} // No matching extension — keep base CodeEditor profile.
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_classified_by_class() {
        let p = ContextClassifier::classify(Some("Alacritty"), None).unwrap();
        assert!(p.whisper_hint.is_some());
        assert!(p.llm_suffix.is_some());
        assert!(!p.suppress_history);
    }

    #[test]
    fn terminal_case_insensitive() {
        let p = ContextClassifier::classify(Some("KITTY"), None).unwrap();
        assert!(p.whisper_hint.is_some());
    }

    #[test]
    fn code_editor_classified() {
        let p = ContextClassifier::classify(Some("code"), None).unwrap();
        assert!(p.whisper_hint.is_some());
        // LLM suffix intentionally absent for editors until we have concrete biasing copy
        assert!(p.llm_suffix.is_none());
    }

    #[test]
    fn browser_no_whisper_hint() {
        let p = ContextClassifier::classify(Some("firefox"), None).unwrap();
        assert!(p.whisper_hint.is_none());
        // Browser LLM suffix removed — no concrete biasing text justified yet
        assert!(p.llm_suffix.is_none());
    }

    #[test]
    fn private_suppresses_history() {
        let p = ContextClassifier::classify(Some("keepassxc"), None).unwrap();
        assert!(p.suppress_history);
        assert!(p.whisper_hint.is_none());
        assert!(p.llm_suffix.is_none());
    }

    #[test]
    fn unknown_class_returns_none() {
        assert!(ContextClassifier::classify(Some("unknown-app"), None).is_none());
    }

    #[test]
    fn none_class_returns_none() {
        assert!(ContextClassifier::classify(None, Some("some title")).is_none());
    }

    #[test]
    fn text_editor_no_hints() {
        let p = ContextClassifier::classify(Some("gedit"), None).unwrap();
        assert!(p.whisper_hint.is_none());
        assert!(p.llm_suffix.is_none());
        assert!(!p.suppress_history);
    }

    #[test]
    fn detected_agent_is_none_in_phase_a() {
        let p = ContextClassifier::classify(Some("Alacritty"), None).unwrap();
        assert!(p.detected_agent.is_none());
    }
}
