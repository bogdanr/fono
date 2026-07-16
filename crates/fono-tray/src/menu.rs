// SPDX-License-Identifier: GPL-3.0-only
//! Platform-neutral tray menu model. Phase 7 Task 7.2 of
//! `plans/2026-07-03-macos-port-v1.md`.
//!
//! The menu *content* — every label, submenu, checkmark, active-marker
//! and conditional row — is built exactly once, here, as a declarative
//! [`MenuNode`] tree. Platform backends (Linux ksni today, the macOS
//! `NSStatusItem` renderer next, the Windows `tray-icon` renderer when
//! that port lands) are dumb one-time interpreters over this tree:
//! they never change when the menu evolves, so a menu edit is made in
//! one place and every OS picks it up.
//!
//! Keep this module free of any backend types (`ksni`, AppKit, …) and
//! free of `cfg(target_os)` — it must compile and unit-test on every
//! platform, which is also what pins cross-OS menu parity in CI.

use crate::{
    ActiveBackends, PreferencesSnapshot, TrayAction, TrayState, AUTO_STOP_PRESETS_MS,
    DISABLED_SENTINEL, LANGUAGE_SHORTLIST, RECENT_SLOTS, WAVEFORM_STYLES,
};

/// Microphone slots in the "Microphone" submenu. Eight covers the
/// common case (laptop builtin + USB headset + dock + a second USB
/// device); the shared builder truncates longer device lists.
pub const MIC_SLOTS: usize = 8;

/// One node of the platform-neutral menu tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuNode {
    /// A plain row. `action == None` renders as a disabled
    /// (greyed-out, non-clickable) informational row.
    Item {
        label: String,
        action: Option<TrayAction>,
    },
    /// A row with a native checkbox glyph. Clicking fires `action`;
    /// the daemon flips the underlying config flag and the next poll
    /// repaints `checked`.
    Check {
        label: String,
        checked: bool,
        action: TrayAction,
    },
    /// A nested submenu.
    Menu {
        label: String,
        children: Vec<Self>,
    },
    Separator,
}

impl MenuNode {
    fn item(label: impl Into<String>, action: TrayAction) -> Self {
        Self::Item { label: label.into(), action: Some(action) }
    }

    fn info(label: impl Into<String>) -> Self {
        Self::Item { label: label.into(), action: None }
    }

    fn menu(label: impl Into<String>, children: Vec<Self>) -> Self {
        Self::Menu { label: label.into(), children }
    }

    fn check(label: impl Into<String>, checked: bool, action: TrayAction) -> Self {
        Self::Check { label: label.into(), checked, action }
    }
}

/// Borrowed snapshot of everything the menu renders from. The ksni
/// backend fills this from its polled fields on every repaint; other
/// backends do the same from their own state.
#[derive(Debug, Clone, Copy)]
pub struct MenuInputs<'a> {
    pub state: TrayState,
    pub recent: &'a [String],
    pub stt_labels: &'a [String],
    pub polish_labels: &'a [String],
    pub assistant_labels: &'a [String],
    pub tts_labels: &'a [String],
    pub active: ActiveBackends,
    pub discovered_stt: &'a [String],
    pub update_label: Option<&'a str>,
    pub gpu_upgrade_label: Option<&'a str>,
    /// `(devices, active_idx)`; `u8::MAX` active_idx = "Auto".
    pub microphones: (&'a [String], u8),
    pub prefs: &'a PreferencesSnapshot,
    pub mcp_server_enabled: bool,
    pub wyoming_server_enabled: bool,
    pub llm_server_enabled: bool,
}

/// Human label for the current FSM state — used for the disabled
/// status row and by backends for the tooltip/title.
#[must_use]
pub fn status_label(state: TrayState) -> &'static str {
    match state {
        TrayState::Idle => "Fono — idle",
        TrayState::Recording => "Fono — recording",
        TrayState::Processing => "Fono — processing",
        TrayState::Paused => "Fono — paused",
        TrayState::Assistant => "Fono — assistant",
    }
}

/// Icon tint for the current FSM state, shared by every backend so
/// the tray colour language is identical across platforms. Returned
/// as `(r, g, b)`; backends rasterize in their own pixel format
/// (ksni wants ARGB, AppKit wants RGBA).
#[must_use]
pub fn state_color(state: TrayState) -> (u8, u8, u8) {
    match state {
        TrayState::Idle => (0x3b, 0x82, 0xf6),       // blue
        TrayState::Recording => (0xef, 0x44, 0x44),  // red (dictation)
        TrayState::Processing => (0xf5, 0x9e, 0x0b), // amber
        TrayState::Paused => (0x6b, 0x72, 0x80),     // grey
        // Saturated green — matches the overlay's accent stripe for
        // assistant turns (`AssistantRecording`).
        TrayState::Assistant => (0x22, 0xc5, 0x5e),
    }
}

/// Build the full menu tree. This is the single shared definition of
/// the tray menu for every platform.
//
// Length lint allowed for the same reason the old ksni builder allowed
// it: the function is declarative menu composition, and keeping the
// sections inline preserves the visual order of the menu at a glance.
#[allow(clippy::too_many_lines, clippy::vec_init_then_push)]
#[must_use]
pub fn build(t: &MenuInputs<'_>) -> Vec<MenuNode> {
    let mut items: Vec<MenuNode> = Vec::new();

    // Status row (disabled, informational).
    items.push(MenuNode::info(status_label(t.state)));
    items.push(MenuNode::Separator);

    items.push(MenuNode::item("Toggle recording  (F7)", TrayAction::ToggleRecording));
    items.push(MenuNode::item("Pause hotkeys", TrayAction::Pause));
    // Assistant controls. The dedicated "Stop assistant" entry was
    // removed when `fono cancel` became the unified cancel surface;
    // "Forget conversation" stays (distinct operation, no playback to
    // stop).
    items.push(MenuNode::item("Forget conversation", TrayAction::AssistantForget));
    items.push(MenuNode::Separator);

    // Recent transcriptions submenu. Conditional inclusion of children
    // — snixembed's libdbusmenu-gtk emits "Children but no menu"
    // warnings when an item has `children-display=submenu` but every
    // child has `visible: false`, so we accept `LayoutUpdated` churn
    // over visibility-toggled stability.
    let mut recent_items: Vec<MenuNode> = Vec::new();
    if t.recent.is_empty() {
        recent_items.push(MenuNode::info("(no transcriptions yet)"));
    } else {
        for (i, label) in t.recent.iter().take(RECENT_SLOTS).enumerate() {
            recent_items.push(MenuNode::item(
                format!("{}. {}", i + 1, truncate_label(label, 60)),
                TrayAction::PasteHistory(i),
            ));
        }
    }
    items.push(MenuNode::menu("Recent transcriptions", recent_items));

    // STT backend submenu. Static provider-family rows come first;
    // remote Wyoming servers discovered over mDNS are appended below a
    // separator so users can choose either the generic backend or a
    // concrete LAN host from the same menu.
    if t.stt_labels.is_empty() {
        tracing::warn!(
            "tray: stt_labels is empty during menu build — \
             daemon should have populated at least the active backend"
        );
    }
    let mut stt_items: Vec<MenuNode> = t
        .stt_labels
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let active = u8::try_from(i).is_ok_and(|i_u8| i_u8 == t.active.stt);
            let prefix = if active { "● " } else { "  " };
            let idx_u8 = u8::try_from(i).unwrap_or(u8::MAX);
            MenuNode::item(format!("{prefix}{label}"), TrayAction::UseStt(idx_u8))
        })
        .collect();
    if stt_items.is_empty() {
        // Defensive empty-state row so the submenu never renders as a
        // blank popup (some tray hosts handle truly-empty submenus
        // poorly on layout-update churn). Disabled so accidental
        // clicks no-op.
        stt_items.push(MenuNode::info("(no backends configured — `fono keys add …`)"));
    }
    // Discovered Wyoming peers — conditional inclusion. See the Recent
    // submenu comment above for why we don't pre-allocate hidden slots.
    if !t.discovered_stt.is_empty() {
        stt_items.push(MenuNode::Separator);
        stt_items.push(MenuNode::info("Discovered Wyoming servers"));
        for (i, label) in t.discovered_stt.iter().enumerate() {
            let idx_u8 = u8::try_from(i).unwrap_or(u8::MAX);
            stt_items.push(MenuNode::item(
                format!("  {}", truncate_label(label, 72)),
                TrayAction::UseDiscoveredStt(idx_u8),
            ));
        }
    }
    items.push(MenuNode::menu("STT backend", stt_items));

    // Polish backend submenu.
    if t.polish_labels.is_empty() {
        tracing::warn!(
            "tray: polish_labels is empty during menu build — \
             daemon should have populated at least the active backend"
        );
    }
    let mut polish_items: Vec<MenuNode> = t
        .polish_labels
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let active = u8::try_from(i).is_ok_and(|i_u8| i_u8 == t.active.polish);
            let prefix = if active { "● " } else { "  " };
            let idx_u8 = u8::try_from(i).unwrap_or(u8::MAX);
            MenuNode::item(format!("{prefix}{label}"), TrayAction::UsePolish(idx_u8))
        })
        .collect();
    if polish_items.is_empty() {
        polish_items.push(MenuNode::info("(no backends configured — `fono keys add …`)"));
    }
    items.push(MenuNode::menu("Polish backend", polish_items));

    // Assistant backend submenu. Independent of the polish pipeline —
    // this drives `[assistant].backend`.
    items.push(MenuNode::menu(
        "Assistant backend",
        indexed_items(
            t.assistant_labels,
            t.active.assistant,
            "(assistant disabled — `fono use assistant …` to enable)",
            TrayAction::UseAssistant,
        ),
    ));

    // TTS backend submenu — `[tts].backend`.
    items.push(MenuNode::menu(
        "TTS backend",
        indexed_items(
            t.tts_labels,
            t.active.tts,
            "(tts disabled — `fono use tts …` to enable)",
            TrayAction::UseTts,
        ),
    ));

    // Microphone submenu — only when the daemon supplied at least one
    // input device.
    if !t.microphones.0.is_empty() {
        let auto_active = t.microphones.1 == u8::MAX;
        let mut mic_items: Vec<MenuNode> = Vec::new();
        mic_items.push(MenuNode::info(if auto_active {
            "● Auto (system default)"
        } else {
            "  Auto (system default)"
        }));
        mic_items.push(MenuNode::Separator);
        for (i, name) in t.microphones.0.iter().take(MIC_SLOTS).enumerate() {
            let active = u8::try_from(i).is_ok_and(|i_u8| i_u8 == t.microphones.1);
            let prefix = if active { "● " } else { "  " };
            let idx_u8 = u8::try_from(i).unwrap_or(u8::MAX);
            mic_items.push(MenuNode::item(
                format!("{prefix}{}", truncate_label(name, 60)),
                TrayAction::SetInputDevice(idx_u8),
            ));
        }
        items.push(MenuNode::menu("Microphone", mic_items));
    }

    // Preferences submenu — quick toggles + radio groups.
    items.push(preferences_submenu(t.prefs));

    // Unified "Servers" submenu — groups everything Fono can *expose*
    // (MCP for coding agents, Wyoming STT host for the LAN, the local
    // LLM API) so the tray UX mirrors the role-based STT / TTS /
    // Polish submenus above (which group what Fono *consumes*).
    //
    // Network MCP is reserved as a disabled placeholder so the entry
    // can light up the day the transport ships without a tray-layout
    // churn.
    let server_items = vec![
        MenuNode::check(
            "MCP (local) — lets apps use Fono",
            t.mcp_server_enabled,
            TrayAction::ToggleMcpServer,
        ),
        MenuNode::info("  MCP (network) — coming soon"),
        MenuNode::check(
            "Wyoming server (STT + TTS + wake) — shares Fono on the LAN",
            t.wyoming_server_enabled,
            TrayAction::ToggleWyomingServer,
        ),
        MenuNode::check(
            "Local LLM server (OpenAI + Ollama API) — shares Fono on the LAN",
            t.llm_server_enabled,
            TrayAction::ToggleLlmServer,
        ),
        MenuNode::Separator,
        MenuNode::info("See docs/coding-agents.md and docs/providers.md"),
    ];
    items.push(MenuNode::menu("Servers", server_items));

    items.push(MenuNode::Separator);

    // Update entry — surfaced only when the background checker has
    // detected a newer release. Conditional inclusion (not
    // visibility-toggled) for snixembed compat.
    if let Some(label) = t.update_label {
        items.push(MenuNode::item(label, TrayAction::ApplyUpdate));
    }

    // GPU-upgrade entry — same conditional pattern. Surfaced only on a
    // CPU-variant build with a usable Vulkan loader + GPU.
    if let Some(label) = t.gpu_upgrade_label {
        items.push(MenuNode::item(label, TrayAction::UpdateForGpuAcceleration));
    }

    items.push(MenuNode::item("Settings…", TrayAction::OpenSettingsWeb));
    items.push(MenuNode::item("Edit config", TrayAction::OpenConfig));
    items.push(MenuNode::Separator);
    items.push(MenuNode::item("Quit", TrayAction::Quit));

    items
}

/// Compose the `Preferences ▸` submenu — boolean toggles up top,
/// radio-style submenus (Auto-stop / Overlay / Language) below the
/// separator.
//
// Length lint allowed: declarative menu composition; inlining the
// per-submenu loops keeps the visual order of the menu obvious.
#[allow(clippy::too_many_lines, clippy::vec_init_then_push)]
fn preferences_submenu(p: &PreferencesSnapshot) -> MenuNode {
    let mut items: Vec<MenuNode> = Vec::new();

    // Booleans use native checkbox glyphs on every backend — the user
    // explicitly prefers the proper checkbox look over a `●`-prefix
    // faux-checkmark.
    items.push(prefs_check(
        "Mute system audio while recording",
        p.auto_mute_system,
        TrayAction::SetAutoMuteSystem,
    ));
    items.push(prefs_check(
        "Also copy transcript to clipboard",
        p.also_copy_to_clipboard,
        TrayAction::SetAlsoCopyToClipboard,
    ));
    items.push(prefs_check(
        "Start Fono on login",
        p.startup_autostart,
        TrayAction::SetStartupAutostart,
    ));
    items.push(prefs_check(
        "Voice-activity detection (auto-trim silence)",
        p.vad_enabled,
        TrayAction::SetVadEnabled,
    ));
    items.push(prefs_check(
        "Wake-word activation (always listening)",
        p.wakeword_enabled,
        TrayAction::SetWakeWordEnabled,
    ));
    // Read-only info rows naming which phrase triggers what. Only
    // shown while enabled so the user always sees the live mapping.
    if p.wakeword_enabled {
        if p.wake_phrases.is_empty() {
            items.push(MenuNode::info("    (no wake phrase configured)"));
        } else {
            for line in &p.wake_phrases {
                items.push(MenuNode::info(format!("    {line}")));
            }
        }
    }

    items.push(MenuNode::Separator);

    // Radio submenus — the parent label always carries the current
    // selection in the form "Title: <value>" so even if a tray host
    // renders nested submenus oddly, the user can see the live state
    // without expanding. The children carry the `● ` active marker.

    let auto_stop_label = AUTO_STOP_PRESETS_MS
        .iter()
        .find(|(_, ms)| *ms == p.auto_stop_silence_ms)
        .map_or_else(|| format!("{} ms", p.auto_stop_silence_ms), |(s, _)| (*s).to_string());
    let auto_stop_items: Vec<MenuNode> = AUTO_STOP_PRESETS_MS
        .iter()
        .map(|(label, ms)| {
            let active = *ms == p.auto_stop_silence_ms;
            let prefix = if active { "● " } else { "    " };
            let descriptive = if *ms == 0 {
                format!("{prefix}{label} (manual stop only)")
            } else {
                format!("{prefix}{label} of silence")
            };
            MenuNode::item(descriptive, TrayAction::SetAutoStopSilenceMs(*ms))
        })
        .collect();
    items.push(MenuNode::menu(
        format!("Auto-stop after silence: {auto_stop_label}"),
        auto_stop_items,
    ));

    let waveform_label = WAVEFORM_STYLES.get(p.waveform_style as usize).map_or("Bars", |(_, l)| *l);
    let overlay_items: Vec<MenuNode> = WAVEFORM_STYLES
        .iter()
        .enumerate()
        .map(|(i, (_serde, label))| {
            let idx_u8 = u8::try_from(i).unwrap_or(u8::MAX);
            let active = idx_u8 == p.waveform_style;
            let prefix = if active { "● " } else { "    " };
            let descriptive = match *label {
                "Bars" => "Bars (volume bars)",
                "Oscilloscope" => "Oscilloscope (raw waveform)",
                "FFT" => "FFT (frequency spectrum)",
                "Heatmap" => "Heatmap (rolling spectrogram)",
                "Transcript" => "Transcript (live preview — more CPU / tokens)",
                "Terrain 3D" => "Terrain 3D (spectrogram landscape)",
                "Aurora Beziers" => "Aurora Beziers (glowing fluid ribbons)",
                "System/360" => "System/360 (mainframe console lamps)",
                "Glass Cortex" => "Glass Cortex (live AI thinking)",
                other => other,
            };
            MenuNode::item(format!("{prefix}{descriptive}"), TrayAction::SetWaveformStyle(idx_u8))
        })
        .collect();
    items.push(MenuNode::menu(format!("Visualisation overlay: {waveform_label}"), overlay_items));

    // Language — multi-select via checkmarks; each click toggles the
    // code in/out of `general.languages`. "Auto-detect" is the
    // empty-list state — checking it clears every other pick.
    let language_label = language_summary(&p.languages);
    let mut language_items: Vec<MenuNode> = Vec::new();
    language_items.push(MenuNode::check(
        "Auto-detect (clear language list)",
        p.languages.is_empty(),
        TrayAction::ClearLanguages,
    ));
    language_items.push(MenuNode::Separator);
    for (i, (code, label)) in LANGUAGE_SHORTLIST.iter().enumerate() {
        let idx_u8 = u8::try_from(i).unwrap_or(u8::MAX);
        let already_in = p.languages.iter().any(|c| c == code);
        language_items.push(MenuNode::check(
            format!("{label}  ({code})"),
            already_in,
            TrayAction::ToggleLanguage(idx_u8),
        ));
    }
    // Tail hint for users who want a language outside the shortlist.
    language_items.push(MenuNode::Separator);
    language_items.push(MenuNode::info("(more languages — see Edit config)"));
    items.push(MenuNode::menu(format!("Language: {language_label}"), language_items));

    MenuNode::menu("Preferences", items)
}

/// Summary string for the `Language ▸` parent row (live state at a
/// glance): `[]` → `Auto-detect`, up to three names joined by commas,
/// more → `"N languages"`.
fn language_summary(languages: &[String]) -> String {
    if languages.is_empty() {
        return "Auto-detect".into();
    }
    if languages.len() > 3 {
        return format!("{} languages", languages.len());
    }
    let names: Vec<String> = languages
        .iter()
        .map(|code| {
            LANGUAGE_SHORTLIST
                .iter()
                .find(|(c, _)| c == code)
                .map_or_else(|| code.clone(), |(_, name)| (*name).to_string())
        })
        .collect();
    names.join(", ")
}

/// Boolean preference checkbox row.
fn prefs_check<F>(label: &str, value: bool, action_for: F) -> MenuNode
where
    F: FnOnce(bool) -> TrayAction,
{
    MenuNode::check(label, value, action_for(!value))
}

/// Build a list of indexed submenu rows with an active marker and a
/// fallback "empty" disabled row. Shared by the Assistant and TTS
/// backend submenus to keep their structure aligned with the STT /
/// Polish submenus. Honours the [`DISABLED_SENTINEL`] label prefix:
/// when present the row renders greyed-out and non-clickable (used by
/// the daemon's TTS submenu for cloud backends missing an API key).
fn indexed_items(
    labels: &[String],
    active_idx: u8,
    empty_msg: &str,
    action_for: impl Fn(u8) -> TrayAction,
) -> Vec<MenuNode> {
    if labels.is_empty() {
        return vec![MenuNode::info(empty_msg)];
    }
    labels
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let (enabled, label) = label
                .strip_prefix(DISABLED_SENTINEL)
                .map_or_else(|| (true, label.clone()), |stripped| (false, stripped.to_string()));
            let active = u8::try_from(i).is_ok_and(|i_u8| i_u8 == active_idx);
            let prefix = if active { "● " } else { "  " };
            let idx_u8 = u8::try_from(i).unwrap_or(u8::MAX);
            MenuNode::Item {
                label: format!("{prefix}{label}"),
                action: if enabled { Some(action_for(idx_u8)) } else { None },
            }
        })
        .collect()
}

/// Collapse a (possibly multi-line) transcript into a single trimmed
/// menu label of at most `max_chars` characters, `…`-terminated.
fn truncate_label(s: &str, max_chars: usize) -> String {
    let trimmed = s.replace('\n', " ");
    let trimmed = trimmed.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        let mut out: String = trimmed.chars().take(max_chars).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_inputs<'a>(
        recent: &'a [String],
        labels: &'a [String],
        mics: &'a [String],
        prefs: &'a PreferencesSnapshot,
    ) -> MenuInputs<'a> {
        MenuInputs {
            state: TrayState::Idle,
            recent,
            stt_labels: labels,
            polish_labels: labels,
            assistant_labels: labels,
            tts_labels: labels,
            active: ActiveBackends { stt: 0, polish: 1, assistant: u8::MAX, tts: 0 },
            discovered_stt: recent,
            update_label: Some("Update to v9.9.9"),
            gpu_upgrade_label: None,
            microphones: (mics, u8::MAX),
            prefs,
            mcp_server_enabled: true,
            wyoming_server_enabled: false,
            llm_server_enabled: false,
        }
    }

    /// Compact one-line-per-node rendering used by the snapshot test.
    fn render(nodes: &[MenuNode], depth: usize, out: &mut String) {
        use std::fmt::Write as _;
        for n in nodes {
            let pad = "  ".repeat(depth);
            match n {
                MenuNode::Separator => {
                    let _ = writeln!(out, "{pad}---");
                }
                MenuNode::Item { label, action } => {
                    let marker = if action.is_some() { "*" } else { "-" };
                    let _ = writeln!(out, "{pad}{marker} {label}");
                }
                MenuNode::Check { label, checked, .. } => {
                    let glyph = if *checked { "[x]" } else { "[ ]" };
                    let _ = writeln!(out, "{pad}{glyph} {label}");
                }
                MenuNode::Menu { label, children } => {
                    let _ = writeln!(out, "{pad}> {label}");
                    render(children, depth + 1, out);
                }
            }
        }
    }

    /// Pins the top-level structure of the menu so a backend-motivated
    /// edit can't silently reshape the tree. Content details are
    /// covered by the focused tests below.
    #[test]
    fn top_level_structure_is_stable() {
        let recent = vec!["hello world".to_string()];
        let labels = vec!["Local (whisper)".to_string(), "Groq".to_string()];
        let mics = vec!["USB Mic".to_string()];
        let prefs = PreferencesSnapshot::default();
        let tree = build(&full_inputs(&recent, &labels, &mics, &prefs));

        let top: Vec<String> = tree
            .iter()
            .map(|n| match n {
                MenuNode::Separator => "---".into(),
                MenuNode::Item { label, .. } => label.clone(),
                MenuNode::Check { label, .. } => format!("[{label}]"),
                MenuNode::Menu { label, .. } => format!("{label} >"),
            })
            .collect();
        assert_eq!(
            top,
            vec![
                "Fono — idle",
                "---",
                "Toggle recording  (F7)",
                "Pause hotkeys",
                "Forget conversation",
                "---",
                "Recent transcriptions >",
                "STT backend >",
                "Polish backend >",
                "Assistant backend >",
                "TTS backend >",
                "Microphone >",
                "Preferences >",
                "Servers >",
                "---",
                "Update to v9.9.9",
                "Settings…",
                "Edit config",
                "---",
                "Quit",
            ]
        );
    }

    #[test]
    fn snapshot_full_tree_renders() {
        let recent = vec!["hello world".to_string()];
        let labels = vec!["Local (whisper)".to_string(), "Groq".to_string()];
        let mics = vec!["USB Mic".to_string()];
        let prefs = PreferencesSnapshot {
            wakeword_enabled: true,
            wake_phrases: vec!["\"Hey Fono\" → Assistant".to_string()],
            ..Default::default()
        };
        let tree = build(&full_inputs(&recent, &labels, &mics, &prefs));
        let mut out = String::new();
        render(&tree, 0, &mut out);
        // Spot-pin the load-bearing details across the tree.
        for needle in [
            "* Toggle recording  (F7)\n",
            "> Recent transcriptions\n  * 1. hello world\n",
            "* ● Local (whisper)\n", // active STT marker
            "*   Local (whisper)\n", // polish active is index 1
            "* ● Groq\n",            // …so Groq carries the polish marker
            "- Discovered Wyoming servers\n",
            "-     \"Hey Fono\" → Assistant\n",
            "- ● Auto (system default)\n",
            "[x] MCP (local) — lets apps use Fono\n",
            "[ ] Wyoming server (STT + TTS + wake) — shares Fono on the LAN\n",
            "> Auto-stop after silence: Off\n",
            "> Visualisation overlay: Bars\n",
            "> Language: Auto-detect\n",
            "[x] Auto-detect (clear language list)\n",
            "* Update to v9.9.9\n",
            "* Quit\n",
        ] {
            assert!(out.contains(needle), "missing {needle:?} in rendered tree:\n{out}");
        }
    }

    #[test]
    fn empty_states_render_disabled_rows() {
        let empty: Vec<String> = Vec::new();
        let prefs = PreferencesSnapshot::default();
        let inputs = MenuInputs {
            state: TrayState::Recording,
            recent: &empty,
            stt_labels: &empty,
            polish_labels: &empty,
            assistant_labels: &empty,
            tts_labels: &empty,
            active: ActiveBackends::unknown(),
            discovered_stt: &empty,
            update_label: None,
            gpu_upgrade_label: None,
            microphones: (&empty, u8::MAX),
            prefs: &prefs,
            mcp_server_enabled: false,
            wyoming_server_enabled: false,
            llm_server_enabled: false,
        };
        let tree = build(&inputs);
        let mut out = String::new();
        render(&tree, 0, &mut out);
        assert!(out.contains("- Fono — recording\n"));
        assert!(out.contains("- (no transcriptions yet)\n"));
        assert!(out.contains("- (no backends configured — `fono keys add …`)\n"));
        assert!(out.contains("- (assistant disabled — `fono use assistant …` to enable)\n"));
        assert!(out.contains("- (tts disabled — `fono use tts …` to enable)\n"));
        // No mic submenu when the device list is empty; no update rows.
        assert!(!out.contains("> Microphone\n"));
        assert!(!out.contains("Update"));
    }

    #[test]
    fn disabled_sentinel_greys_out_indexed_rows() {
        let labels =
            vec!["Piper (local)".to_string(), format!("{DISABLED_SENTINEL}OpenAI (no key)")];
        let rows = indexed_items(&labels, 0, "(empty)", TrayAction::UseTts);
        assert_eq!(
            rows[0],
            MenuNode::Item {
                label: "● Piper (local)".into(), action: Some(TrayAction::UseTts(0))
            }
        );
        assert_eq!(rows[1], MenuNode::Item { label: "  OpenAI (no key)".into(), action: None });
    }

    #[test]
    fn truncate_label_flattens_and_caps() {
        assert_eq!(truncate_label("a\nb", 60), "a b");
        let long = "x".repeat(80);
        let out = truncate_label(&long, 60);
        assert_eq!(out.chars().count(), 61);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn language_summary_tiers() {
        assert_eq!(language_summary(&[]), "Auto-detect");
        let one = vec!["en".to_string()];
        assert_eq!(language_summary(&one), "English");
        let many: Vec<String> = ["en", "ro", "fr", "de"].iter().map(ToString::to_string).collect();
        assert_eq!(language_summary(&many), "4 languages");
    }
}
