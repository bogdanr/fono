// SPDX-License-Identifier: GPL-3.0-only
//! Tray-icon integration. Phase 7 Task 7.1.
//!
//! When the `tray-backend` feature is enabled (default for the `fono`
//! binary on Linux), this crate spawns a real system-tray icon as a
//! tokio task driving a pure-Rust StatusNotifierItem (SNI) D-Bus
//! service via [`ksni`]. Without the feature, a no-op [`Tray`] keeps
//! the code paths compiling for headless builds and cross-platform CI.
//!
//! # No GTK, no shared libraries
//!
//! Earlier versions used `tray-icon`'s libappindicator backend, which
//! dragged GTK3 + glib + cairo + pango + gio + gdk-pixbuf into the
//! binary's `NEEDED` list. SNI is the protocol every modern desktop
//! tray host already speaks (KDE Plasma, GNOME with the SNI shell
//! extension, sway+waybar, hyprland+waybar, i3+i3status, xfce4-panel,
//! lxqt-panel) — there's no reason to drag a toolkit through it.
//! `ksni` talks SNI + `com.canonical.dbusmenu` over `zbus` directly,
//! pure Rust, no C dependencies. Phase 2 Task 2.1 of
//! `plans/2026-04-30-fono-single-binary-size-v1.md`.
//!
//! # UX features
//!
//! - **Recent transcriptions submenu** — the last `RECENT_SLOTS`
//!   dictations are shown as clickable menu items. Clicking one fires
//!   [`TrayAction::PasteHistory`] with the slot index (0 = newest); the
//!   daemon then re-injects/copies that text. This is the clipit-style
//!   workflow users asked for.
//! - **STT / polish backend submenus** — switch the active provider on
//!   the fly; the active backend wears a leading "● " marker. The STT
//!   menu also includes live mDNS-discovered Wyoming servers, which the
//!   daemon writes to config and hot-reloads when selected.
//! - **Microphone submenu** — list Pulse/PipeWire input devices and
//!   set default via the daemon.
//! - **Update submenu entry** — appears only when the background
//!   checker has detected a new release.

use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use tokio::sync::mpsc;

/// How many recent transcriptions to surface in the tray menu.
pub const RECENT_SLOTS: usize = 10;

/// Sentinel prefix that callers can prepend to a submenu label to mark
/// it as disabled (greyed-out, non-clickable). The shared submenu
/// builder strips the prefix before display.
///
/// Used by the daemon's TTS submenu to grey out cloud backends whose
/// API key isn't present in `secrets.toml`, so the user sees the full
/// list at a glance but only the actionable entries are clickable.
/// Zero-width and outside the BMP-printable range so it never appears
/// in legitimate labels.
pub const DISABLED_SENTINEL: &str = "\u{0001}";

/// Provider that returns the most recent transcription labels (newest
/// first) for display in the tray's "Recent" submenu. Called from the
/// tray task on a poll interval.
pub type RecentProvider = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// Provider that returns `(stt_idx, llm_idx)` — indices into
/// `stt_labels` / `polish_labels` (the slices passed to [`spawn`]) for
/// the currently-active STT and polish backends. Polled every ~2 s; the
/// tray repaints the active marker (`●`) when the indices change.
///
/// `u8::MAX` for any index means "unknown / not in the list" and
/// renders no checkmark — useful when the active backend isn't one
/// fono knows about (custom OpenAI-compatible endpoint etc.).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ActiveBackends {
    pub stt: u8,
    pub polish: u8,
    pub assistant: u8,
    pub tts: u8,
}

impl ActiveBackends {
    /// Sentinel for "no backend known". Mirrors the per-field
    /// `u8::MAX` convention.
    #[must_use]
    pub fn unknown() -> Self {
        Self { stt: u8::MAX, polish: u8::MAX, assistant: u8::MAX, tts: u8::MAX }
    }
}

pub type ActiveProvider = Arc<dyn Fn() -> ActiveBackends + Send + Sync>;

/// Provider returning the current update label for the tray's
/// "Update" menu item, or `None` to keep the item hidden/disabled.
/// Called on the same ~2 s poll as the recent/active providers.
///
/// Convention:
/// - `None` → entry is hidden.
/// - `Some(label)` → e.g. "Update to v0.3.0" — clicking fires
///   [`TrayAction::ApplyUpdate`].
pub type UpdateProvider = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Provider for the "Update for GPU acceleration" menu entry —
/// distinct from the version-bump update because it triggers a
/// cross-variant switch (CPU build → GPU build) using the same
/// self-update infrastructure. Slice 3 of
/// `plans/2026-05-02-fono-cpu-gpu-variants-v1.md`.
///
/// Returns `Some(label)` only when:
/// - the running binary is the CPU variant, AND
/// - Vulkan is usable on this host (libvulkan.so.1 loadable + ≥1
///   physical device), AND
/// - a GPU-variant release asset for the latest tag exists.
///
/// Clicking fires [`TrayAction::UpdateForGpuAcceleration`]; the
/// daemon then runs `fono_update::apply_update` against the
/// `fono-gpu` asset prefix.
pub type GpuUpgradeProvider = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Provider returning labels for Wyoming servers discovered on the LAN.
/// Called on the same ~2 s poll as the backend providers. The daemon
/// filters out its own local advertisement before returning labels, so
/// this list contains only actionable remote servers.
pub type DiscoveredSttProvider = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// Provider returning `(devices, active_idx)` for the "Microphone"
/// submenu — `devices` is the live input-device list (display
/// names) and `active_idx` is the index of the device the OS reports
/// as the current default, or `u8::MAX` when none of the listed
/// devices is the default ("Auto / system default" row stays marked).
/// Polled every ~2 s; the tray refreshes the submenu in place when
/// either changes.
pub type MicrophonesProvider = Arc<dyn Fn() -> (Vec<String>, u8) + Send + Sync>;

/// Provider returning the current `[general]`/`[audio]`/`[overlay]`
/// values that back the "Preferences" submenu's checkmarks and radio
/// groups. Polled every ~2 s alongside the other providers; the tray
/// re-renders the submenu in place when any field changes (typical
/// trigger: an external `fono settings` write or `fono use ...` flips
/// a related field).
pub type PreferencesProvider = Arc<dyn Fn() -> PreferencesSnapshot + Send + Sync>;

/// Provider returning whether `[mcp] enabled = true` in the user
/// config. Polled on the same ~2 s cadence as the other providers so
/// the unified "Servers" submenu reflects on-disk changes (e.g. an
/// external `fono use mcp-server on/off` or a hand-edit of
/// `config.toml`) without a daemon restart. The submenu is always
/// visible — the checkmark next to "MCP (stdio)" just flips depending
/// on the current value.
pub type McpEnabledProvider = Arc<dyn Fn() -> bool + Send + Sync>;

/// Provider returning whether `[server.wyoming].enabled = true` in the
/// user config. Polled on the same ~2 s cadence so the "Servers ▸
/// Wyoming server" checkmark reflects external `config.toml` edits
/// without a tray restart. The single switch governs STT serving (always
/// on when enabled), TTS serving (added automatically when a `[tts]` backend
/// is configured), and wake-word serving (added automatically whenever the
/// binary can do wake detection) — all over the one listener. The daemon's
/// tray-action handler hot-reloads the LAN listener, so no restart is needed.
pub type WyomingEnabledProvider = Arc<dyn Fn() -> bool + Send + Sync>;

/// Provider returning whether `[server.llm].enabled = true` in the user
/// config. Polled on the same ~2 s cadence so the "Servers ▸ Local LLM
/// server" checkmark reflects external `config.toml` edits without a tray
/// restart. The switch governs the OpenAI + Ollama HTTP API (both wire
/// formats ride one listener; ADR 0036). The daemon's tray-action handler
/// hot-reloads the listener in place, so no restart is needed.
pub type LlmEnabledProvider = Arc<dyn Fn() -> bool + Send + Sync>;

/// Snapshot view of the user-facing config fields surfaced in the
/// "Preferences" submenu. Tracked as a flat struct so the 2 s poll
/// can diff with `PartialEq` and only repaint when something moved.
///
/// `language` is an index into [`LANGUAGE_SHORTLIST`] (`u8::MAX` for
/// "auto / not in shortlist"). `waveform_style` is an index into
/// [`WAVEFORM_STYLES`].
//
// `clippy::struct_excessive_bools` triggers because we keep the boolean
// preferences as plain bools rather than collapsing them into a state
// machine or bitfield. The flat shape is intentional: the tray polls
// this every 2 s and diffs with `PartialEq`, and a state machine
// would make `prefs_toggle` calls in `build_preferences_submenu`
// indirect for no readability win. Allow.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PreferencesSnapshot {
    pub auto_mute_system: bool,
    pub also_copy_to_clipboard: bool,
    pub startup_autostart: bool,
    pub vad_enabled: bool,
    /// Whether always-on wake-word activation is enabled (`[wakeword].enabled`).
    pub wakeword_enabled: bool,
    /// Read-only, pre-formatted display lines for the active wake phrases,
    /// each like `"Hey Jarvis" → Assistant`. Built by the daemon from
    /// `[wakeword].phrases`; the tray only renders them as disabled info
    /// rows under the toggle so the user always sees which keyword does what.
    pub wake_phrases: Vec<String>,
    pub auto_stop_silence_ms: u32,
    pub waveform_style: u8,
    /// Currently-allowed language codes (BCP-47), in canonical order.
    /// Empty list = "Auto-detect" (whisper free-detect; cloud
    /// providers' built-in detection). Multiple entries = constrained
    /// auto-detect / allow-list — Whisper picks from these and bans
    /// every other language. The tray surfaces this as a CheckmarkItem
    /// list so users can toggle individual languages on/off.
    pub languages: Vec<String>,
}

/// Re-export of the canonical curated language shortlist. The wizard,
/// the tray, and any future settings UI all draw from
/// [`fono_core::languages::CURATED_LANGUAGES`] so picking a language in
/// one surface looks identical in the others. Indices into this slice
/// are stable — `TrayAction::ToggleLanguage(u8)` references them.
pub use fono_core::languages::CURATED_LANGUAGES as LANGUAGE_SHORTLIST;

/// Waveform-style names paired with their `WaveformStyle` discriminant
/// label as serialised into TOML. Index used by
/// `TrayAction::SetWaveformStyle(u8)`. The last entry `Transcript`
/// flips the daemon into the live streaming-preview pipeline; the
/// four passive styles keep it on the batch path.
pub const WAVEFORM_STYLES: &[(&str, &str)] = &[
    ("bars", "Bars"),
    ("oscilloscope", "Oscilloscope"),
    ("fft", "FFT"),
    ("heatmap", "Heatmap"),
    ("transcript", "Transcript"),
    ("terrain3d", "Terrain 3D"),
    ("system360", "System/360"),
];

/// Auto-stop silence presets surfaced in the tray's radio group.
/// `0` means "Off" — the daemon's auto-stop silence timer is bypassed
/// when the value is zero. Indices map to `TrayAction::SetAutoStopSilenceMs(u32)`.
pub const AUTO_STOP_PRESETS_MS: &[(&str, u32)] = &[("Off", 0), ("3 s", 3_000), ("5 s", 5_000)];

/// FSM-aligned tray state used to tint the icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TrayState {
    Idle = 0,
    Recording = 1,
    Processing = 2,
    Paused = 3,
    /// Voice assistant is active (recording / thinking / speaking).
    /// Painted green to mirror the overlay's accent stripe so the
    /// user can tell the assistant flow apart from dictation at a
    /// glance. Sub-phases collapse into one tray state because the
    /// icon doesn't need that level of detail.
    Assistant = 4,
}

/// User actions fired from the tray menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    ShowStatus,
    /// SNI left-click activation. Distinct from [`TrayAction::ShowStatus`]
    /// (which is the explicit menu entry showing the last
    /// transcription) so the daemon can give left-click a contextual
    /// payload: a "run `fono setup`" hint when TTS is not configured,
    /// or the current hotkey cheat sheet once setup is done.
    ActivateLeftClick,
    ToggleRecording,
    Pause,
    /// Re-paste the i-th most recent transcription (0 = newest).
    PasteHistory(usize),
    /// Switch the active STT backend. Index into the `stt_labels`
    /// slice passed to [`spawn`]. Provider-switching plan task R2.1.
    UseStt(u8),
    /// Switch to the i-th mDNS-discovered Wyoming server shown in the
    /// tray's "STT backend" submenu.
    UseDiscoveredStt(u8),
    /// Switch the active polish backend. Index into the `polish_labels`
    /// slice passed to [`spawn`].
    UsePolish(u8),
    /// User clicked the "Update to vX" entry. The daemon handles
    /// this by running a check and applying the update via
    /// `fono-update::apply_update`.
    ApplyUpdate,
    /// User clicked the "Update for GPU acceleration" entry. The
    /// daemon dispatches this to `fono_update::apply_update` against
    /// the `fono-gpu` asset prefix to swap the running CPU binary for
    /// the Vulkan-enabled one. Only present in the menu on a CPU
    /// build with a usable Vulkan host.
    UpdateForGpuAcceleration,
    /// Switch the active input device. The `u8` is an index into the
    /// device list returned by [`MicrophonesProvider`] at the time of
    /// the click. On Pulse / PipeWire hosts the daemon dispatches
    /// this to `pactl set-default-source`; the cpal branch hides
    /// the submenu so this is never fired there.
    SetInputDevice(u8),
    /// Toggle `general.auto_mute_system` (mute other audio while recording).
    SetAutoMuteSystem(bool),
    /// Toggle `general.also_copy_to_clipboard`.
    SetAlsoCopyToClipboard(bool),
    /// Toggle `general.startup_autostart`.
    SetStartupAutostart(bool),
    /// Toggle VAD by flipping `audio.vad_backend` between `"energy"` (on)
    /// and `"off"`. The tray uses a boolean for menu legibility; the
    /// daemon translates back to the string field.
    SetVadEnabled(bool),
    /// Toggle always-on wake-word activation by flipping
    /// `wakeword.enabled`. Mirrors [`Self::SetVadEnabled`]: the tray exposes
    /// a boolean for menu legibility and the daemon persists the flag. The
    /// listener lifecycle (opening/closing the idle capture stream) is wired
    /// separately — see the daemon's Phase-D handling.
    SetWakeWordEnabled(bool),
    /// Set `audio.auto_stop_silence_ms` to one of the
    /// [`AUTO_STOP_PRESETS_MS`] presets. `0` disables auto-stop.
    SetAutoStopSilenceMs(u32),
    /// Set `overlay.style` by index into [`WAVEFORM_STYLES`].
    SetWaveformStyle(u8),
    /// Toggle a curated language by index into [`LANGUAGE_SHORTLIST`]:
    /// add it to `general.languages` if absent, remove it if present.
    /// Multi-select multi-language allow-list — picking two languages
    /// produces `general.languages = ["en", "ro"]` and so on.
    ToggleLanguage(u8),
    /// Clear `general.languages` so STT runs in unconstrained
    /// auto-detect mode. The tray's "Auto-detect" entry fires this.
    ClearLanguages,
    /// Spawn a topical settings TUI (`fono settings`) in the user's
    /// preferred terminal. Daemon shells out via `$TERMINAL` /
    /// xdg-terminal-exec.
    OpenSettingsTui,
    OpenConfig,
    /// Wipe the rolling assistant conversation history. Independent
    /// of the playback state; intended as a "fresh start" entry.
    AssistantForget,
    /// Switch the active assistant chat backend. Index into the
    /// `assistant_labels` slice passed to [`spawn`]. Mirrors
    /// [`Self::UsePolish`] but persists into `[assistant].backend`.
    UseAssistant(u8),
    /// Switch the active TTS backend. Index into `tts_labels`.
    UseTts(u8),
    /// Toggle `[mcp.server].enabled` from the tray. The daemon writes
    /// the change to `config.toml` and the setting takes effect on the
    /// next `fono mcp serve` invocation (a full daemon restart is not
    /// required).
    ToggleMcpServer,
    /// Toggle `[server.wyoming].enabled` from the tray. The daemon
    /// writes the change to `config.toml` and hot-reloads the LAN
    /// listener in place — no restart required. The one switch governs
    /// STT serving (always) plus TTS serving (whenever a `[tts]`
    /// backend is configured), since Wyoming multiplexes both over one
    /// connection.
    ToggleWyomingServer,
    /// Toggle `[server.llm].enabled` from the tray. The daemon writes
    /// the change to `config.toml` and hot-reloads the local LLM HTTP
    /// listener in place — no restart required. The one switch governs
    /// both the OpenAI and Ollama wire formats, which share the single
    /// listener (ADR 0036).
    ToggleLlmServer,
    Quit,
}

/// Handle returned by [`spawn`]. Dropping it tears down the tray task
/// on a best-effort basis (the underlying `mpsc` channel closes; the
/// ksni service then exits at the next tick).
pub struct Tray {
    shared_state: Arc<AtomicU8>,
    #[cfg(feature = "tray-backend")]
    state_tx: Option<mpsc::UnboundedSender<TrayState>>,
}

impl Tray {
    /// Update the tray icon to reflect the given FSM state.
    pub fn set_state(&self, state: TrayState) {
        self.shared_state.store(state as u8, Ordering::Relaxed);
        #[cfg(feature = "tray-backend")]
        if let Some(tx) = self.state_tx.as_ref() {
            let _ = tx.send(state);
        }
    }

    /// Last state stored via [`Tray::set_state`]. Useful for tests.
    pub fn state(&self) -> TrayState {
        match self.shared_state.load(Ordering::Relaxed) {
            1 => TrayState::Recording,
            2 => TrayState::Processing,
            3 => TrayState::Paused,
            4 => TrayState::Assistant,
            _ => TrayState::Idle,
        }
    }

    /// Build a tray handle that has no backend wired up — only the
    /// in-memory state atom. Intended for unit tests in downstream
    /// crates (notably `fono::daemon`) that need to feed `Tray` into
    /// the IPC dispatch helpers and inspect [`Tray::state`] without
    /// spawning the real ksni service.
    #[must_use]
    pub fn for_tests() -> Self {
        Self {
            shared_state: Arc::new(AtomicU8::new(TrayState::Idle as u8)),
            #[cfg(feature = "tray-backend")]
            state_tx: None,
        }
    }
}

/// Spawn the tray on the ambient tokio runtime.
///
/// Returns `(handle, actions_rx)`. `actions_rx` yields [`TrayAction`]s the
/// user clicked in the menu. If the feature is off (or init fails) the
/// returned receiver simply never fires.
///
/// Submenu inputs:
/// * `recent_provider` — invoked every ~2 s for the "Recent
///   transcriptions" submenu. Pass a noop (`|| Vec::new()`) to disable.
/// * `stt_labels` / `polish_labels` — display strings for each STT / LLM
///   backend, in canonical order (the order the daemon also iterates
///   when decoding indices back to `SttBackend` / `PolishBackend`).
/// * `active_provider` — invoked on the same poll; returns the indices
///   of the currently-active STT and LLM in the slices above. Used to
///   paint the active-marker (`●`) and migrate it on `Reload`.
/// * `discovered_stt_provider` — live labels for remote Wyoming servers
///   discovered via mDNS; empty list renders a disabled placeholder.
/// * `update_provider` — `Some(label)` shows / refreshes the "Update
///   to vX" entry; `None` hides it.
/// * `gpu_upgrade_provider` — `Some(label)` shows the "Update for GPU
///   acceleration" entry on a CPU-variant build with a usable Vulkan
///   host; `None` hides it. Distinct from `update_provider` because it
///   triggers a cross-variant switch, not a version bump.
/// * `microphones_provider` — `(devices, active_idx)` for the
///   Microphone submenu; empty list hides the submenu.
/// * `preferences_provider` — current values backing the
///   "Preferences" submenu's checkmarks and radios. Polled on the same
///   cadence as the others.
#[allow(unused_variables, clippy::too_many_arguments)]
pub fn spawn(
    tooltip: &str,
    recent_provider: RecentProvider,
    stt_labels: Vec<String>,
    polish_labels: Vec<String>,
    assistant_labels: Vec<String>,
    tts_labels: Vec<String>,
    active_provider: ActiveProvider,
    discovered_stt_provider: DiscoveredSttProvider,
    update_provider: UpdateProvider,
    gpu_upgrade_provider: GpuUpgradeProvider,
    microphones_provider: MicrophonesProvider,
    preferences_provider: PreferencesProvider,
    mcp_enabled_provider: McpEnabledProvider,
    wyoming_enabled_provider: WyomingEnabledProvider,
    llm_enabled_provider: LlmEnabledProvider,
) -> (Tray, mpsc::UnboundedReceiver<TrayAction>) {
    let shared = Arc::new(AtomicU8::new(TrayState::Idle as u8));
    let (action_tx, action_rx) = mpsc::unbounded_channel();

    #[cfg(feature = "tray-backend")]
    {
        let (state_tx, state_rx) = mpsc::unbounded_channel::<TrayState>();
        let started = backend::spawn(
            tooltip.to_string(),
            action_tx,
            state_rx,
            recent_provider,
            stt_labels,
            polish_labels,
            assistant_labels,
            tts_labels,
            active_provider,
            discovered_stt_provider,
            update_provider,
            gpu_upgrade_provider,
            microphones_provider,
            preferences_provider,
            mcp_enabled_provider,
            wyoming_enabled_provider,
            llm_enabled_provider,
        );
        let state_tx = if started { Some(state_tx) } else { None };
        (Tray { shared_state: shared, state_tx }, action_rx)
    }

    #[cfg(not(feature = "tray-backend"))]
    {
        (Tray { shared_state: shared }, action_rx)
    }
}

// -------------------------------------------------------------------------
// Real backend (pure-Rust SNI via `ksni`).
// -------------------------------------------------------------------------

#[cfg(feature = "tray-backend")]
mod backend {
    use super::{
        ActiveBackends, ActiveProvider, DiscoveredSttProvider, GpuUpgradeProvider,
        LlmEnabledProvider, McpEnabledProvider, MicrophonesProvider, PreferencesProvider,
        PreferencesSnapshot, RecentProvider, TrayAction, TrayState, UpdateProvider,
        WyomingEnabledProvider, AUTO_STOP_PRESETS_MS, LANGUAGE_SHORTLIST, RECENT_SLOTS,
        WAVEFORM_STYLES,
    };
    use fono_core::notify::{self, Urgency};
    use ksni::{
        menu::{CheckmarkItem, StandardItem, SubMenu},
        Handle, MenuItem, ToolTip, TrayMethods,
    };
    use tokio::sync::mpsc;

    const MISSING_WATCHER_NOTIFICATION_TIMEOUT_MS: u32 = 20_000;
    const MISSING_WATCHER_NOTIFICATION_TITLE: &str = "Fono tray unavailable";
    const MISSING_WATCHER_NOTIFICATION_BODY: &str = "No StatusNotifierWatcher found. Start a tray host, e.g. Waybar tray, KDE tray, xfce4-panel, or snixembed, then restart Fono.";

    /// Notification body shown when zbus can't connect to the D-Bus
    /// session bus at all (typical symptom: "I/O error: No such file or
    /// directory (os error 2)" out of `ksni::Tray::spawn`). This happens
    /// when Fono is launched from a context that doesn't inherit the
    /// graphical session env — e.g. a TTY, a system-level systemd unit,
    /// `sudo fono`, or an autostart script that runs before the user
    /// session bus is exported. Hotkeys / dictation continue to work,
    /// only the tray icon goes missing.
    const MISSING_BUS_NOTIFICATION_TITLE: &str = "Fono tray unavailable";
    const MISSING_BUS_NOTIFICATION_BODY: &str = "Couldn't reach the D-Bus session bus, so the tray icon won't appear. Launch Fono from your graphical desktop session (not a TTY, root shell, or system service); if you use a systemd --user unit make sure DBUS_SESSION_BUS_ADDRESS is exported. Hotkeys and dictation still work.";

    /// Generic fallback notification for any other tray failure. Keeps
    /// the user informed even when we can't pinpoint the cause.
    const GENERIC_TRAY_NOTIFICATION_TITLE: &str = "Fono tray unavailable";
    const GENERIC_TRAY_NOTIFICATION_BODY_PREFIX: &str =
        "The tray icon failed to start. Hotkeys and dictation still work. Details: ";

    /// Microphone slots in the "Microphone" submenu. Pre-allocated for
    /// the same reason as the STT/LLM lists: rebuilding causes flicker
    /// on KDE/GNOME indicator hosts. Eight covers the common case
    /// (laptop builtin + USB headset + dock + a second USB device).
    const MIC_SLOTS: usize = 8;

    /// Backing model for the SNI tray. ksni periodically queries this
    /// (via the `Tray` trait methods) to repaint the icon and menu
    /// when the desktop's tray host requests a refresh, so we keep
    /// every UI input as a plain field and let the trait methods
    /// transform them into menu items / icon pixmaps lazily.
    struct KsniTray {
        tooltip: String,
        state: TrayState,
        recent: Vec<String>,
        stt_labels: Vec<String>,
        polish_labels: Vec<String>,
        assistant_labels: Vec<String>,
        tts_labels: Vec<String>,
        active: ActiveBackends,
        discovered_stt: Vec<String>,
        update_label: Option<String>,
        gpu_upgrade_label: Option<String>,
        microphones: (Vec<String>, u8),
        prefs: PreferencesSnapshot,
        /// Whether `[mcp.server].enabled = true` in the user config.
        /// Reflected as a checkmark on the "MCP (stdio)" row of the
        /// unified "Servers" submenu.
        mcp_server_enabled: bool,
        /// Whether `[server.wyoming].enabled = true` in the user
        /// config. Reflected as a checkmark on the "Wyoming server"
        /// row of the unified "Servers" submenu.
        wyoming_server_enabled: bool,
        /// Whether `[server.llm].enabled = true` in the user config.
        /// Reflected as a checkmark on the "Local LLM server" row of
        /// the unified "Servers" submenu.
        llm_server_enabled: bool,
        actions: mpsc::UnboundedSender<TrayAction>,
    }

    impl ksni::Tray for KsniTray {
        fn id(&self) -> String {
            // Unique-per-application id; keeping it stable across
            // sessions so panel hosts can persist position / order.
            "fono".into()
        }

        fn title(&self) -> String {
            status_label(self.state).to_string()
        }

        fn tool_tip(&self) -> ToolTip {
            ToolTip {
                title: self.tooltip.clone(),
                description: status_label(self.state).into(),
                ..Default::default()
            }
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            vec![icon_for(self.state)]
        }

        // Left-click/status activation. Sends the dedicated
        // [`TrayAction::ActivateLeftClick`] so the daemon can show a
        // contextual notification (setup hint or hotkey cheat sheet);
        // the explicit "Show last transcription" menu entry still
        // sends [`TrayAction::ShowStatus`] and behaves as before.
        fn activate(&mut self, _x: i32, _y: i32) {
            let _ = self.actions.send(TrayAction::ActivateLeftClick);
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            build_menu(self)
        }
    }

    /// Spawn the SNI tray task. Returns `true` on success; on failure
    /// the caller falls back to a "no tray, hotkeys still work" path.
    /// Caller-side, success means the [`Tray`] handle gets a real
    /// [`mpsc::UnboundedSender<TrayState>`]; failure means it gets
    /// `None` and `set_state` becomes a no-op.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        tooltip: String,
        actions: mpsc::UnboundedSender<TrayAction>,
        state_rx: mpsc::UnboundedReceiver<TrayState>,
        recent_provider: RecentProvider,
        stt_labels: Vec<String>,
        polish_labels: Vec<String>,
        assistant_labels: Vec<String>,
        tts_labels: Vec<String>,
        active_provider: ActiveProvider,
        discovered_stt_provider: DiscoveredSttProvider,
        update_provider: UpdateProvider,
        gpu_upgrade_provider: GpuUpgradeProvider,
        microphones_provider: MicrophonesProvider,
        preferences_provider: PreferencesProvider,
        mcp_enabled_provider: McpEnabledProvider,
        wyoming_enabled_provider: WyomingEnabledProvider,
        llm_enabled_provider: LlmEnabledProvider,
    ) -> bool {
        // We need to be inside a tokio runtime to spawn the ksni
        // service; the daemon always is. Probe `Handle::try_current`
        // and bail cleanly if not (tests / odd embedders).
        if tokio::runtime::Handle::try_current().is_err() {
            tracing::warn!("tray backend skipped: no current tokio runtime");
            return false;
        }
        tokio::spawn(async move {
            match run(
                tooltip,
                actions,
                state_rx,
                recent_provider,
                stt_labels,
                polish_labels,
                assistant_labels,
                tts_labels,
                active_provider,
                discovered_stt_provider,
                update_provider,
                gpu_upgrade_provider,
                microphones_provider,
                preferences_provider,
                mcp_enabled_provider,
                wyoming_enabled_provider,
                llm_enabled_provider,
            )
            .await
            {
                Err(e) if is_missing_status_notifier_watcher(&e) => {
                    notify_missing_status_notifier_watcher();
                    tracing::warn!(
                        "tray unavailable: no StatusNotifierWatcher is registered on the session bus; \
                         start a tray host/watcher (for example KDE Plasma's tray, waybar with tray, \
                         xfce4-panel, or snixembed). Dictation and the overlay continue without the \
                         tray icon."
                    );
                }
                Err(e) if is_missing_session_bus(&e) => {
                    notify_missing_session_bus();
                    tracing::warn!(
                        "tray unavailable: D-Bus session bus is not reachable from this process \
                         (DBUS_SESSION_BUS_ADDRESS unset and no fallback socket found). Launch \
                         Fono from your graphical desktop session, or export the address into the \
                         service/unit that starts it. Underlying error: {e:#}"
                    );
                }
                Err(e) => {
                    notify_generic_tray_error(&e);
                    tracing::warn!("tray task exited with error: {e:#}");
                }
                Ok(()) => {
                    // The poll loop only returns `Ok(())` if every
                    // provider's mpsc closed (i.e. the daemon is
                    // shutting down). Logging at warn so a user who
                    // notices the icon disappear has a breadcrumb.
                    tracing::warn!(
                        "tray task exited cleanly — icon will disappear. \
                         Usually means the daemon dropped the providers; \
                         restart fono to bring the tray back."
                    );
                }
            }
        });
        true
    }

    fn notify_missing_status_notifier_watcher() {
        notify::send(
            MISSING_WATCHER_NOTIFICATION_TITLE,
            MISSING_WATCHER_NOTIFICATION_BODY,
            "dialog-error",
            MISSING_WATCHER_NOTIFICATION_TIMEOUT_MS,
            Urgency::Critical,
        );
    }

    fn notify_missing_session_bus() {
        notify::send(
            MISSING_BUS_NOTIFICATION_TITLE,
            MISSING_BUS_NOTIFICATION_BODY,
            "dialog-error",
            MISSING_WATCHER_NOTIFICATION_TIMEOUT_MS,
            Urgency::Critical,
        );
    }

    fn notify_generic_tray_error(err: &anyhow::Error) {
        // Trim to a single line so the popup stays readable.
        let short = err.to_string().lines().next().unwrap_or("unknown error").to_string();
        let body = format!("{GENERIC_TRAY_NOTIFICATION_BODY_PREFIX}{short}");
        notify::send(
            GENERIC_TRAY_NOTIFICATION_TITLE,
            &body,
            "dialog-error",
            MISSING_WATCHER_NOTIFICATION_TIMEOUT_MS,
            Urgency::Critical,
        );
    }

    fn is_missing_status_notifier_watcher(err: &anyhow::Error) -> bool {
        let msg = err.to_string();
        msg.contains("org.kde.StatusNotifierWatcher") || msg.contains("StatusNotifierWatcher")
    }

    /// Detect the "can't reach the D-Bus session bus at all" failure
    /// mode. zbus surfaces this as `D-Bus connection error: I/O error:
    /// No such file or directory (os error 2)` when no socket path is
    /// configured (DBUS_SESSION_BUS_ADDRESS unset and no fallback
    /// `$XDG_RUNTIME_DIR/bus`). Match on the substring rather than
    /// downcasting through anyhow's source chain because zbus's error
    /// types aren't part of our public API and the wording is stable
    /// across zbus 3.x / 4.x.
    fn is_missing_session_bus(err: &anyhow::Error) -> bool {
        let msg = err.to_string();
        // "D-Bus connection error" + ENOENT is the canonical signature.
        // Also accept the bare ENOENT phrasing in case zbus shortens it.
        (msg.contains("D-Bus connection error") || msg.contains("connection error"))
            && (msg.contains("No such file or directory") || msg.contains("os error 2"))
    }

    /// Best-effort discovery of the user's D-Bus session bus address.
    ///
    /// Sets `DBUS_SESSION_BUS_ADDRESS` in the current process env when
    /// it's missing, so the subsequent `ksni::Tray::spawn` call (which
    /// goes through zbus's pure-Rust connection logic) can find the
    /// bus. This mirrors what libdbus / `dbus-launch` do in C land but
    /// skips the autolaunch fork — we only adopt an existing bus, never
    /// spawn a new one.
    ///
    /// Tried, in order:
    /// 1. `DBUS_SESSION_BUS_ADDRESS` already set → leave it alone.
    /// 2. `$XDG_RUNTIME_DIR/bus` socket present → use it.
    /// 3. `/run/user/<uid>/bus` socket present → use it (covers cases
    ///    where `XDG_RUNTIME_DIR` is unset, common with `sudo`/su
    ///    sessions or minimal launchers).
    /// 4. Scan `/proc/*/environ` for any same-uid process that
    ///    inherited `DBUS_SESSION_BUS_ADDRESS` from the user's
    ///    graphical session and copy its value. This is the trick
    ///    that lets a daemon launched from a TTY find the desktop
    ///    session's bus.
    ///
    /// Returns `true` when the env var ends up set (either it was
    /// already, or one of the fallbacks succeeded), `false` when every
    /// strategy failed. The caller logs/notifies on `false`.
    fn ensure_dbus_session_bus() -> bool {
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() {
            return true;
        }

        // Strategy 2: XDG_RUNTIME_DIR/bus.
        if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
            let path = std::path::Path::new(&dir).join("bus");
            if path.exists() {
                let addr = format!("unix:path={}", path.display());
                tracing::debug!("tray: adopting session bus at {addr} (XDG_RUNTIME_DIR/bus)");
                std::env::set_var("DBUS_SESSION_BUS_ADDRESS", addr);
                return true;
            }
        }

        // Strategy 3: /run/user/<uid>/bus.
        #[cfg(target_os = "linux")]
        {
            let Some(uid) = current_uid() else {
                return false;
            };
            let path = std::path::PathBuf::from(format!("/run/user/{uid}/bus"));
            if path.exists() {
                let addr = format!("unix:path={}", path.display());
                tracing::debug!("tray: adopting session bus at {addr} (/run/user/<uid>/bus)");
                std::env::set_var("DBUS_SESSION_BUS_ADDRESS", addr);
                return true;
            }

            // Strategy 4: scan /proc for a same-uid process whose
            // environ contains DBUS_SESSION_BUS_ADDRESS.
            if let Some(addr) = scan_proc_for_session_bus(uid) {
                tracing::debug!("tray: adopting session bus at {addr} (inherited from /proc)");
                std::env::set_var("DBUS_SESSION_BUS_ADDRESS", addr);
                return true;
            }
        }

        false
    }

    /// Scan `/proc/<pid>/environ` for a same-uid process exporting
    /// `DBUS_SESSION_BUS_ADDRESS`. Returns the first value found.
    /// Linux-only (procfs); other Unixes hit the early-return in
    /// `ensure_dbus_session_bus`.
    #[cfg(target_os = "linux")]
    fn current_uid() -> Option<u32> {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata("/proc/self").ok().map(|m| m.uid())
    }

    #[cfg(target_os = "linux")]
    fn scan_proc_for_session_bus(uid: u32) -> Option<String> {
        use std::os::unix::fs::MetadataExt;

        let proc_dir = std::fs::read_dir("/proc").ok()?;
        for entry in proc_dir.flatten() {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            if !name_str.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let pid_path = entry.path();
            // Match owner uid so we don't read another user's env (also
            // fails the `/proc/<pid>/environ` open due to perms anyway,
            // but checking up front avoids the syscall).
            let Ok(meta) = std::fs::metadata(&pid_path) else {
                continue;
            };
            if meta.uid() != uid {
                continue;
            }
            let environ_path = pid_path.join("environ");
            let Ok(bytes) = std::fs::read(&environ_path) else {
                continue;
            };
            for entry in bytes.split(|&b| b == 0) {
                if let Some(rest) = entry.strip_prefix(b"DBUS_SESSION_BUS_ADDRESS=") {
                    if let Ok(s) = std::str::from_utf8(rest) {
                        let trimmed = s.trim();
                        if !trimmed.is_empty() {
                            return Some(trimmed.to_string());
                        }
                    }
                }
            }
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    async fn run(
        tooltip: String,
        actions: mpsc::UnboundedSender<TrayAction>,
        mut state_rx: mpsc::UnboundedReceiver<TrayState>,
        recent_provider: RecentProvider,
        stt_labels: Vec<String>,
        polish_labels: Vec<String>,
        assistant_labels: Vec<String>,
        tts_labels: Vec<String>,
        active_provider: ActiveProvider,
        discovered_stt_provider: DiscoveredSttProvider,
        update_provider: UpdateProvider,
        gpu_upgrade_provider: GpuUpgradeProvider,
        microphones_provider: MicrophonesProvider,
        preferences_provider: PreferencesProvider,
        mcp_enabled_provider: McpEnabledProvider,
        wyoming_enabled_provider: WyomingEnabledProvider,
        llm_enabled_provider: LlmEnabledProvider,
    ) -> anyhow::Result<()> {
        // Make sure DBUS_SESSION_BUS_ADDRESS is set before zbus tries
        // to connect; zbus's pure-Rust discovery is stricter than
        // libdbus's and won't probe `$XDG_RUNTIME_DIR/bus` /
        // `/run/user/<uid>/bus` on its own.
        if !ensure_dbus_session_bus() {
            notify_missing_session_bus();
            anyhow::bail!(
                "D-Bus session bus address unknown: DBUS_SESSION_BUS_ADDRESS is unset and no \
                 fallback socket (XDG_RUNTIME_DIR/bus, /run/user/<uid>/bus, /proc/*/environ) \
                 was found. Launch Fono from your graphical desktop session."
            );
        }

        let initial_mcp_enabled = mcp_enabled_provider();
        let initial_wyoming_enabled = wyoming_enabled_provider();
        let initial_llm_enabled = llm_enabled_provider();
        let model = KsniTray {
            tooltip,
            state: TrayState::Idle,
            recent: Vec::new(),
            stt_labels,
            polish_labels,
            assistant_labels,
            tts_labels,
            active: ActiveBackends::unknown(),
            discovered_stt: Vec::new(),
            update_label: None,
            gpu_upgrade_label: None,
            microphones: (Vec::new(), u8::MAX),
            prefs: PreferencesSnapshot::default(),
            mcp_server_enabled: initial_mcp_enabled,
            wyoming_server_enabled: initial_wyoming_enabled,
            llm_server_enabled: initial_llm_enabled,
            actions,
        };

        // `TrayMethods::spawn` connects to the session bus, registers
        // with `org.kde.StatusNotifierWatcher`, and returns a handle.
        // On hosts without a watcher (no DISPLAY, no D-Bus session
        // bus, etc.) this errors immediately — we surface it as a
        // warn! and let the rest of the daemon run unaffected.
        let handle: Handle<KsniTray> =
            model.spawn().await.map_err(|e| anyhow::anyhow!("ksni::Tray::spawn failed: {e}"))?;

        tracing::debug!("tray icon ready (SNI)");

        // Poll providers every 2 seconds and push the diff into the
        // ksni model. Cheap (history read + a config snapshot read)
        // but skip when nothing changed so we don't churn KDE/GNOME
        // indicator state.
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Cached last-seen provider results so we can skip the
        // `handle.update` round-trip (which always rebuilds + flattens
        // the menu and emits dbusmenu signals) when nothing actually
        // changed between ticks. Reduces D-Bus chatter to ~zero on a
        // steady-state daemon and gives KDE Plasma fewer
        // `LayoutUpdated` events to mis-render against.
        let mut last_recent: Vec<String> = Vec::new();
        let mut last_active: ActiveBackends = ActiveBackends::unknown();
        let mut last_discovered_stt: Vec<String> = Vec::new();
        let mut last_upd: Option<String> = None;
        let mut last_gpu_upd: Option<String> = None;
        let mut last_mics: (Vec<String>, u8) = (Vec::new(), u8::MAX);
        let mut last_prefs: PreferencesSnapshot = PreferencesSnapshot::default();
        let mut last_mcp_enabled: bool = initial_mcp_enabled;
        let mut last_wyoming_enabled: bool = initial_wyoming_enabled;
        let mut last_llm_enabled: bool = initial_llm_enabled;

        loop {
            tokio::select! {
                Some(state) = state_rx.recv() => {
                    handle.update(|t: &mut KsniTray| t.state = state).await;
                }
                _ = interval.tick() => {
                    let recent = recent_provider();
                    let active = active_provider();
                    let discovered_stt = discovered_stt_provider();
                    let upd = update_provider();
                    let gpu_upd = gpu_upgrade_provider();
                    let mics = microphones_provider();
                    let prefs = preferences_provider();
                    let mcp_enabled = mcp_enabled_provider();
                    let wyoming_enabled = wyoming_enabled_provider();
                    let llm_enabled = llm_enabled_provider();

                    let changed = recent != last_recent
                        || active != last_active
                        || discovered_stt != last_discovered_stt
                        || upd != last_upd
                        || gpu_upd != last_gpu_upd
                        || mics != last_mics
                        || prefs != last_prefs
                        || mcp_enabled != last_mcp_enabled
                        || wyoming_enabled != last_wyoming_enabled
                        || llm_enabled != last_llm_enabled;
                    if !changed {
                        continue;
                    }
                    last_recent.clone_from(&recent);
                    last_active = active;
                    last_discovered_stt.clone_from(&discovered_stt);
                    last_upd.clone_from(&upd);
                    last_gpu_upd.clone_from(&gpu_upd);
                    last_mics.clone_from(&mics);
                    last_prefs.clone_from(&prefs);
                    last_mcp_enabled = mcp_enabled;
                    last_wyoming_enabled = wyoming_enabled;
                    last_llm_enabled = llm_enabled;

                    handle.update(move |t: &mut KsniTray| {
                        t.recent = recent;
                        t.active = active;
                        t.discovered_stt = discovered_stt;
                        t.update_label = upd;
                        t.gpu_upgrade_label = gpu_upd;
                        t.microphones = mics;
                        t.prefs = prefs;
                        t.mcp_server_enabled = mcp_enabled;
                        t.wyoming_server_enabled = wyoming_enabled;
                        t.llm_server_enabled = llm_enabled;
                    }).await;
                }
                else => break,
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines, clippy::vec_init_then_push)]
    fn build_menu(t: &KsniTray) -> Vec<MenuItem<KsniTray>> {
        let mut items: Vec<MenuItem<KsniTray>> = Vec::new();

        // Status row (disabled, informational).
        items.push(
            StandardItem {
                label: status_label(t.state).into(),
                enabled: false,
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);

        items.push(
            StandardItem {
                label: "Toggle recording  (F7)".into(),
                activate: send_action(TrayAction::ToggleRecording),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Pause hotkeys".into(),
                activate: send_action(TrayAction::Pause),
                ..Default::default()
            }
            .into(),
        );
        // Assistant controls. The dedicated "Stop assistant" entry
        // was removed when `fono cancel` (which covers both dictation
        // and assistant playback) became the unified cancel surface —
        // duplicating it as a tray entry would invite UX confusion.
        // "Forget conversation" stays: it's a distinct operation
        // (wipes rolling history, no playback to stop).
        items.push(
            StandardItem {
                label: "Forget conversation".into(),
                activate: send_action(TrayAction::AssistantForget),
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);

        // Recent transcriptions submenu. Conditional inclusion of
        // children — snixembed's libdbusmenu-gtk emits
        // "Children but no menu" warnings when an item has the
        // `children-display=submenu` property but every child has
        // `visible: false`. So we accept `LayoutUpdated` churn over
        // visibility-toggled stability.
        let mut recent_items: Vec<MenuItem<KsniTray>> = Vec::new();
        if t.recent.is_empty() {
            recent_items.push(
                StandardItem {
                    label: "(no transcriptions yet)".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        } else {
            for (i, label) in t.recent.iter().take(RECENT_SLOTS).enumerate() {
                let action = TrayAction::PasteHistory(i);
                recent_items.push(
                    StandardItem {
                        label: format!("{}. {}", i + 1, truncate_label(label, 60)),
                        activate: send_action(action),
                        ..Default::default()
                    }
                    .into(),
                );
            }
        }
        items.push(
            SubMenu {
                label: "Recent transcriptions".into(),
                submenu: recent_items,
                ..Default::default()
            }
            .into(),
        );

        // STT backend submenu. Static provider-family rows come first;
        // remote Wyoming servers discovered over mDNS are appended below
        // a separator so users can choose either the generic backend or a
        // concrete LAN host from the same menu.
        if t.stt_labels.is_empty() {
            tracing::warn!(
                "tray: stt_labels is empty during build_menu — \
                 daemon should have populated at least the active backend"
            );
        }
        let mut stt_items: Vec<MenuItem<KsniTray>> = t
            .stt_labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let active = u8::try_from(i).is_ok_and(|i_u8| i_u8 == t.active.stt);
                let prefix = if active { "● " } else { "  " };
                let idx_u8 = u8::try_from(i).unwrap_or(u8::MAX);
                StandardItem {
                    label: format!("{prefix}{label}"),
                    activate: send_action(TrayAction::UseStt(idx_u8)),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        if stt_items.is_empty() {
            // Defensive empty-state row so the submenu never renders as
            // a blank popup (some tray hosts handle truly-empty submenus
            // poorly on layout-update churn). Disabled so accidental
            // clicks no-op.
            stt_items.push(
                StandardItem {
                    label: "(no backends configured — `fono keys add …`)".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        // Discovered Wyoming peers — conditional inclusion. See the
        // Recent submenu comment above for why we don't pre-allocate
        // hidden slots.
        if !t.discovered_stt.is_empty() {
            stt_items.push(MenuItem::Separator);
            stt_items.push(
                StandardItem {
                    label: "Discovered Wyoming servers".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
            for (i, label) in t.discovered_stt.iter().enumerate() {
                let idx_u8 = u8::try_from(i).unwrap_or(u8::MAX);
                stt_items.push(
                    StandardItem {
                        label: format!("  {}", truncate_label(label, 72)),
                        activate: send_action(TrayAction::UseDiscoveredStt(idx_u8)),
                        ..Default::default()
                    }
                    .into(),
                );
            }
        }
        items.push(
            SubMenu { label: "STT backend".into(), submenu: stt_items, ..Default::default() }
                .into(),
        );

        // polish backend submenu.
        if t.polish_labels.is_empty() {
            tracing::warn!(
                "tray: polish_labels is empty during build_menu — \
                 daemon should have populated at least the active backend"
            );
        }
        let mut polish_items: Vec<MenuItem<KsniTray>> = t
            .polish_labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let active = u8::try_from(i).is_ok_and(|i_u8| i_u8 == t.active.polish);
                let prefix = if active { "● " } else { "  " };
                let idx_u8 = u8::try_from(i).unwrap_or(u8::MAX);
                StandardItem {
                    label: format!("{prefix}{label}"),
                    activate: send_action(TrayAction::UsePolish(idx_u8)),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        if polish_items.is_empty() {
            polish_items.push(
                StandardItem {
                    label: "(no backends configured — `fono keys add …`)".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(
            SubMenu { label: "Polish backend".into(), submenu: polish_items, ..Default::default() }
                .into(),
        );

        // Assistant backend submenu. Independent of the polish
        // pipeline above — this drives `[assistant].backend`. Empty
        // when the user hasn't enabled the assistant or hasn't
        // configured any keys.
        let assistant_items: Vec<MenuItem<KsniTray>> = build_indexed_submenu_items(
            &t.assistant_labels,
            t.active.assistant,
            "(assistant disabled — `fono use assistant …` to enable)",
            TrayAction::UseAssistant,
        );
        items.push(
            SubMenu {
                label: "Assistant backend".into(),
                submenu: assistant_items,
                ..Default::default()
            }
            .into(),
        );

        // TTS backend submenu — `[tts].backend`. None / Wyoming /
        // Piper / OpenAI; clicking switches the backend via
        // `fono use tts <name>` semantics (the daemon dispatches
        // the right `set_active_tts` + Reload).
        let tts_items: Vec<MenuItem<KsniTray>> = build_indexed_submenu_items(
            &t.tts_labels,
            t.active.tts,
            "(tts disabled — `fono use tts …` to enable)",
            TrayAction::UseTts,
        );
        items.push(
            SubMenu { label: "TTS backend".into(), submenu: tts_items, ..Default::default() }
                .into(),
        );

        // Microphone submenu — only when the daemon supplied at least
        // one Pulse/PipeWire device. snixembed's libdbusmenu-gtk
        // dislikes pre-allocated hidden slots, so we accept the
        // structural change of toggling the submenu in/out.
        if !t.microphones.0.is_empty() {
            let auto_active = t.microphones.1 == u8::MAX;
            let mut mic_items: Vec<MenuItem<KsniTray>> = Vec::new();
            mic_items.push(
                StandardItem {
                    label: if auto_active {
                        "● Auto (system default)".into()
                    } else {
                        "  Auto (system default)".into()
                    },
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
            mic_items.push(MenuItem::Separator);
            for (i, name) in t.microphones.0.iter().take(MIC_SLOTS).enumerate() {
                let active = u8::try_from(i).is_ok_and(|i_u8| i_u8 == t.microphones.1);
                let prefix = if active { "● " } else { "  " };
                let idx_u8 = u8::try_from(i).unwrap_or(u8::MAX);
                mic_items.push(
                    StandardItem {
                        label: format!("{prefix}{}", truncate_label(name, 60)),
                        activate: send_action(TrayAction::SetInputDevice(idx_u8)),
                        ..Default::default()
                    }
                    .into(),
                );
            }
            items.push(
                SubMenu { label: "Microphone".into(), submenu: mic_items, ..Default::default() }
                    .into(),
            );
        }

        // Preferences submenu — quick toggles + radio groups for the
        // settings users touch frequently. The empty-submenu render
        // bug on snixembed appears to be in libdbusmenu-gtk itself
        // and isn't fixed by flattening or by visible-toggling, so
        // we keep the nested layout the user prefers and document the
        // workaround (`pkill snixembed && snixembed &`) for now.
        items.push(build_preferences_submenu(t));

        // Unified "Servers" submenu — groups everything Fono can
        // *expose* (MCP for coding agents, Wyoming STT host for the
        // LAN, and any future network-facing server) so the tray UX
        // mirrors the role-based STT / TTS / Polish submenus above
        // (which group what Fono *consumes*). CheckmarkItem renders a
        // real OS-native checkbox glyph; clicking flips the
        // corresponding `enabled` flag in `config.toml`.
        //
        // Network MCP is reserved as a disabled placeholder so the
        // entry can light up the day the transport ships without a
        // tray-layout churn.
        let mut server_items: Vec<MenuItem<KsniTray>> = Vec::new();
        server_items.push(
            CheckmarkItem {
                label: "MCP (local) — lets apps use Fono".into(),
                checked: t.mcp_server_enabled,
                activate: send_action(TrayAction::ToggleMcpServer),
                ..Default::default()
            }
            .into(),
        );
        server_items.push(
            StandardItem {
                label: "  MCP (network) — coming soon".into(),
                enabled: false,
                ..Default::default()
            }
            .into(),
        );
        server_items.push(
            CheckmarkItem {
                label: "Wyoming server (STT + TTS + wake) — shares Fono on the LAN".into(),
                checked: t.wyoming_server_enabled,
                activate: send_action(TrayAction::ToggleWyomingServer),
                ..Default::default()
            }
            .into(),
        );
        server_items.push(
            CheckmarkItem {
                label: "Local LLM server (OpenAI + Ollama API) — shares Fono on the LAN".into(),
                checked: t.llm_server_enabled,
                activate: send_action(TrayAction::ToggleLlmServer),
                ..Default::default()
            }
            .into(),
        );
        server_items.push(MenuItem::Separator);
        server_items.push(
            StandardItem {
                label: "See docs/coding-agents.md and docs/providers.md".into(),
                enabled: false,
                ..Default::default()
            }
            .into(),
        );
        items.push(
            SubMenu { label: "Servers".into(), submenu: server_items, ..Default::default() }.into(),
        );

        items.push(MenuItem::Separator);

        // Update entry — surfaced only when the background checker
        // has detected a newer release. Conditional inclusion (not
        // visibility-toggled) for snixembed compat.
        if let Some(label) = t.update_label.as_ref() {
            items.push(
                StandardItem {
                    label: label.clone(),
                    activate: send_action(TrayAction::ApplyUpdate),
                    ..Default::default()
                }
                .into(),
            );
        }

        // GPU-upgrade entry — same conditional pattern. Surfaced only
        // on a CPU-variant build with a usable Vulkan loader + GPU.
        if let Some(label) = t.gpu_upgrade_label.as_ref() {
            items.push(
                StandardItem {
                    label: label.clone(),
                    activate: send_action(TrayAction::UpdateForGpuAcceleration),
                    ..Default::default()
                }
                .into(),
            );
        }

        items.push(
            StandardItem {
                label: "Edit config".into(),
                activate: send_action(TrayAction::OpenConfig),
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit".into(),
                activate: send_action(TrayAction::Quit),
                ..Default::default()
            }
            .into(),
        );

        items
    }

    /// Compose the `Preferences ▸` submenu — boolean toggles up top,
    /// radio-style submenus (Auto-stop / Overlay / Language) below the
    /// separator, and a tail entry that opens `fono settings` in a
    /// for everything tray vocabulary can't express (free text,
    /// prompts, per-app overrides, secrets).
    //
    // Length lint allowed: the function is essentially declarative menu
    // composition — six boolean toggles plus three radio submenus —
    // and inlining the per-submenu loops here keeps the visual order
    // of the menu obvious at a glance. Splitting per-submenu would
    // hide that ordering across helpers.
    #[allow(clippy::too_many_lines, clippy::vec_init_then_push)]
    fn build_preferences_submenu(t: &KsniTray) -> MenuItem<KsniTray> {
        let p = &t.prefs;
        let mut items: Vec<MenuItem<KsniTray>> = Vec::new();

        // Booleans use ksni's native CheckmarkItem so the tray host
        // renders a real checkbox glyph. Some hosts (snixembed +
        // libdbusmenu-gtk) emit chatter warnings around dbusmenu but
        // render checkmarks correctly; the user prefers the proper
        // checkbox look over a `●`-prefix faux-checkmark.
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
        // shown while enabled so the user always sees the live mapping
        // (the editor itself is a later, non-tray surface).
        if p.wakeword_enabled {
            if p.wake_phrases.is_empty() {
                items.push(
                    StandardItem {
                        label: "    (no wake phrase configured)".into(),
                        enabled: false,
                        ..Default::default()
                    }
                    .into(),
                );
            } else {
                for line in &p.wake_phrases {
                    items.push(
                        StandardItem {
                            label: format!("    {line}"),
                            enabled: false,
                            ..Default::default()
                        }
                        .into(),
                    );
                }
            }
        }

        items.push(MenuItem::Separator);

        // Radio submenus — the parent label always carries the
        // current selection in the form "Title: <value>" so even if
        // a tray host renders nested submenus oddly, the user can
        // see the live state without expanding. The children carry
        // the `● ` active marker for the picked row.

        let auto_stop_label = AUTO_STOP_PRESETS_MS
            .iter()
            .find(|(_, ms)| *ms == p.auto_stop_silence_ms)
            .map_or_else(|| format!("{} ms", p.auto_stop_silence_ms), |(s, _)| (*s).to_string());
        let auto_stop_items: Vec<MenuItem<KsniTray>> = AUTO_STOP_PRESETS_MS
            .iter()
            .map(|(label, ms)| {
                let active = *ms == p.auto_stop_silence_ms;
                let prefix = if active { "● " } else { "    " };
                let descriptive = if *ms == 0 {
                    format!("{prefix}{label} (manual stop only)")
                } else {
                    format!("{prefix}{label} of silence")
                };
                let ms_val = *ms;
                StandardItem {
                    label: descriptive,
                    activate: send_action(TrayAction::SetAutoStopSilenceMs(ms_val)),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        items.push(
            SubMenu {
                label: format!("Auto-stop after silence: {auto_stop_label}"),
                submenu: auto_stop_items,
                ..Default::default()
            }
            .into(),
        );

        let waveform_label =
            WAVEFORM_STYLES.get(p.waveform_style as usize).map_or("Bars", |(_, l)| *l);
        let overlay_items: Vec<MenuItem<KsniTray>> = WAVEFORM_STYLES
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
                    other => other,
                };
                StandardItem {
                    label: format!("{prefix}{descriptive}"),
                    activate: send_action(TrayAction::SetWaveformStyle(idx_u8)),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        items.push(
            SubMenu {
                label: format!("Visualisation overlay: {waveform_label}"),
                submenu: overlay_items,
                ..Default::default()
            }
            .into(),
        );

        // Language — multi-select via CheckmarkItem so each entry
        // shows a real checkbox glyph. Each click toggles the code
        // in/out of `general.languages`; multiple entries can be
        // checked simultaneously. "Auto-detect" is the empty-list
        // state — checking it clears every other pick.
        let language_label = language_summary(&p.languages);
        let mut language_items: Vec<MenuItem<KsniTray>> = Vec::new();
        language_items.push(
            CheckmarkItem {
                label: "Auto-detect (clear language list)".into(),
                checked: p.languages.is_empty(),
                activate: send_action(TrayAction::ClearLanguages),
                ..Default::default()
            }
            .into(),
        );
        language_items.push(MenuItem::Separator);
        for (i, (code, label)) in LANGUAGE_SHORTLIST.iter().enumerate() {
            let idx_u8 = u8::try_from(i).unwrap_or(u8::MAX);
            let already_in = p.languages.iter().any(|c| c == code);
            language_items.push(
                CheckmarkItem {
                    label: format!("{label}  ({code})"),
                    checked: already_in,
                    activate: send_action(TrayAction::ToggleLanguage(idx_u8)),
                    ..Default::default()
                }
                .into(),
            );
        }
        // Tail hint for users who want a language outside the shortlist.
        language_items.push(MenuItem::Separator);
        language_items.push(
            StandardItem {
                label: "(more languages — see Edit config)".into(),
                enabled: false,
                ..Default::default()
            }
            .into(),
        );
        items.push(
            SubMenu {
                label: format!("Language: {language_label}"),
                submenu: language_items,
                ..Default::default()
            }
            .into(),
        );

        // Stage 2 will surface a "Open settings (TUI)…" entry here that
        // launches `fono settings` in $TERMINAL. Variant + daemon handler
        // exist already; the menu item is held back until the CLI
        // subcommand lands so we don't ship a click that opens a
        // terminal then errors out on an unknown subcommand. Until
        // then, "Edit config" at the top level is the escape hatch.

        SubMenu { label: "Preferences".into(), submenu: items, ..Default::default() }.into()
    }

    /// Summary string for the `Language ▸` parent row. Tells the user
    /// the live state at a glance, even before opening the submenu:
    ///
    /// - `[]`              → `Auto-detect`
    /// - `[en]`            → `English`
    /// - `[en, ro]`        → `English, Romanian`
    /// - `[en, ro, fr]`    → `English, Romanian, French`
    /// - `[en, ro, fr, …]` → `4 languages`
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

    /// Boolean preference checkbox using ksni's native [`CheckmarkItem`].
    /// Renders as a real checkbox glyph on every modern tray host —
    /// the user explicitly prefers this over a `● `-prefix faux marker.
    fn prefs_check<F>(label: &str, value: bool, action_for: F) -> MenuItem<KsniTray>
    where
        F: FnOnce(bool) -> TrayAction,
    {
        let next = action_for(!value);
        CheckmarkItem {
            label: label.into(),
            checked: value,
            activate: send_action(next),
            ..Default::default()
        }
        .into()
    }

    /// Build a menu-item activate callback that fires `action` on the
    /// tray's action channel. The closure ignores the `&mut KsniTray`
    /// argument because every action is a pure outbound message — the
    /// daemon owns the state machine, not the tray.
    fn send_action(action: TrayAction) -> Box<dyn Fn(&mut KsniTray) + Send + Sync + 'static> {
        Box::new(move |t: &mut KsniTray| {
            let _ = t.actions.send(action);
        })
    }

    /// Build a list of indexed submenu items with an active marker
    /// and a fallback "empty" disabled row. Shared by the Assistant
    /// and TTS backend submenus to keep their structure aligned with
    /// the STT / LLM submenus.
    fn build_indexed_submenu_items(
        labels: &[String],
        active_idx: u8,
        empty_msg: &str,
        action_for: impl Fn(u8) -> TrayAction,
    ) -> Vec<MenuItem<KsniTray>> {
        if labels.is_empty() {
            return vec![StandardItem {
                label: empty_msg.to_string(),
                enabled: false,
                ..Default::default()
            }
            .into()];
        }
        labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                // Honour the DISABLED_SENTINEL prefix exported by the
                // crate root: when present, strip it and render the
                // item as a non-clickable greyed-out row. Used by the
                // daemon's TTS submenu to surface cloud backends
                // whose API key is missing.
                let (enabled, label) = label.strip_prefix(super::DISABLED_SENTINEL).map_or_else(
                    || (true, label.clone()),
                    |stripped| (false, stripped.to_string()),
                );
                let active = u8::try_from(i).is_ok_and(|i_u8| i_u8 == active_idx);
                let prefix = if active { "● " } else { "  " };
                let idx_u8 = u8::try_from(i).unwrap_or(u8::MAX);
                StandardItem {
                    label: format!("{prefix}{label}"),
                    enabled,
                    activate: if enabled {
                        send_action(action_for(idx_u8))
                    } else {
                        // Disabled rows shouldn't have an activate
                        // callback wired — keep them inert.
                        Box::new(|_: &mut KsniTray| {})
                    },
                    ..Default::default()
                }
                .into()
            })
            .collect()
    }

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

    fn status_label(state: TrayState) -> &'static str {
        match state {
            TrayState::Idle => "Fono — idle",
            TrayState::Recording => "Fono — recording",
            TrayState::Processing => "Fono — processing",
            TrayState::Paused => "Fono — paused",
            TrayState::Assistant => "Fono — assistant",
        }
    }

    /// Solid-colour 32x32 ARGB icon tinted by FSM state. Generated
    /// in-code so we don't need a PNG at packaging time. SNI's
    /// pixmap format is ARGB32 in network byte order (A, R, G, B);
    /// not RGBA — the byte order is the one bit easy to get wrong.
    fn icon_for(state: TrayState) -> ksni::Icon {
        const SIZE: i32 = 32;
        let (r, g, b) = match state {
            TrayState::Idle => (0x3b, 0x82, 0xf6),       // blue
            TrayState::Recording => (0xef, 0x44, 0x44),  // red (dictation)
            TrayState::Processing => (0xf5, 0x9e, 0x0b), // amber
            TrayState::Paused => (0x6b, 0x72, 0x80),     // grey
            // Saturated green — matches the overlay's accent stripe
            // for assistant turns (`AssistantRecording`).
            TrayState::Assistant => (0x22, 0xc5, 0x5e),
        };
        let mut data = Vec::with_capacity((SIZE * SIZE * 4) as usize);
        let cx = SIZE / 2;
        let cy = SIZE / 2;
        let radius = (SIZE / 2) - 2;
        for y in 0..SIZE {
            for x in 0..SIZE {
                let dx = x - cx;
                let dy = y - cy;
                let inside = dx * dx + dy * dy <= radius * radius;
                if inside {
                    data.extend_from_slice(&[0xff, r, g, b]);
                } else {
                    data.extend_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        ksni::Icon { width: SIZE, height: SIZE, data }
    }
}
