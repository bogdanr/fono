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

pub mod menu;

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
    /// Open the browser settings page (`[server.web]`). The daemon
    /// lazily starts the web settings listener when it isn't running
    /// (persisting `server.web.enabled = true`), then opens the page
    /// via `xdg-open` / `open` / `explorer`.
    OpenSettingsWeb,
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
    use super::menu::{self, MenuInputs, MenuNode};
    use super::{
        ActiveBackends, ActiveProvider, DiscoveredSttProvider, GpuUpgradeProvider,
        LlmEnabledProvider, McpEnabledProvider, MicrophonesProvider, PreferencesProvider,
        PreferencesSnapshot, RecentProvider, TrayAction, TrayState, UpdateProvider,
        WyomingEnabledProvider,
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
            menu::status_label(self.state).to_string()
        }

        fn tool_tip(&self) -> ToolTip {
            ToolTip {
                title: self.tooltip.clone(),
                description: menu::status_label(self.state).into(),
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
            let inputs = MenuInputs {
                state: self.state,
                recent: &self.recent,
                stt_labels: &self.stt_labels,
                polish_labels: &self.polish_labels,
                assistant_labels: &self.assistant_labels,
                tts_labels: &self.tts_labels,
                active: self.active,
                discovered_stt: &self.discovered_stt,
                update_label: self.update_label.as_deref(),
                gpu_upgrade_label: self.gpu_upgrade_label.as_deref(),
                microphones: (&self.microphones.0, self.microphones.1),
                prefs: &self.prefs,
                mcp_server_enabled: self.mcp_server_enabled,
                wyoming_server_enabled: self.wyoming_server_enabled,
                llm_server_enabled: self.llm_server_enabled,
            };
            render_nodes(&menu::build(&inputs))
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

    /// Interpret the platform-neutral [`MenuNode`] tree into ksni menu
    /// items. This is the entire Linux renderer: it never changes when
    /// the menu content evolves — edit [`crate::menu::build`] instead.
    fn render_nodes(nodes: &[MenuNode]) -> Vec<MenuItem<KsniTray>> {
        nodes
            .iter()
            .map(|node| match node {
                MenuNode::Separator => MenuItem::Separator,
                MenuNode::Item { label, action: Some(action) } => StandardItem {
                    label: label.clone(),
                    activate: send_action(*action),
                    ..Default::default()
                }
                .into(),
                MenuNode::Item { label, action: None } => {
                    StandardItem { label: label.clone(), enabled: false, ..Default::default() }
                        .into()
                }
                MenuNode::Check { label, checked, action } => CheckmarkItem {
                    label: label.clone(),
                    checked: *checked,
                    activate: send_action(*action),
                    ..Default::default()
                }
                .into(),
                MenuNode::Menu { label, children } => SubMenu {
                    label: label.clone(),
                    submenu: render_nodes(children),
                    ..Default::default()
                }
                .into(),
            })
            .collect()
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
