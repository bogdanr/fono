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
    ("cortex", "Glass Cortex"),
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

    #[cfg(all(
        feature = "tray-backend",
        any(target_os = "linux", target_os = "macos", target_os = "windows")
    ))]
    {
        #[cfg(target_os = "linux")]
        use backend_linux as platform_backend;
        #[cfg(target_os = "macos")]
        use backend_macos as platform_backend;
        #[cfg(target_os = "windows")]
        use backend_windows as platform_backend;

        let (state_tx, state_rx) = mpsc::unbounded_channel::<TrayState>();
        let started = platform_backend::spawn(
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

    // Feature on, but no backend exists for this OS yet (a platform
    // with no tray renderer). Linux, macOS and Windows all have one.
    #[cfg(all(
        feature = "tray-backend",
        not(any(target_os = "linux", target_os = "macos", target_os = "windows"))
    ))]
    {
        (Tray { shared_state: shared, state_tx: None }, action_rx)
    }

    #[cfg(not(feature = "tray-backend"))]
    {
        (Tray { shared_state: shared }, action_rx)
    }
}

// -------------------------------------------------------------------------
// macOS backend (NSStatusItem over the shared menu model).
// -------------------------------------------------------------------------

#[cfg(all(feature = "tray-backend", target_os = "macos"))]
mod backend_macos;

// The main-thread pump is part of the binary's startup contract on
// macOS: `fono::main` installs + runs it around the daemon thread.
// `dispatch_main` lets other AppKit consumers (the overlay's NSPanel
// backend) ship work to the same pump without depending on this
// crate's internals.
#[cfg(all(feature = "tray-backend", target_os = "macos"))]
pub use backend_macos::{
    dispatch_main, install_main_pump, run_main_pump, stop_main_pump, MainPumpJobs,
};

// -------------------------------------------------------------------------
// Linux backend (pure-Rust SNI via `ksni`, interpreting the shared
// menu model). A future Windows backend slots in identically as
// `backend_windows.rs` behind `target_os = "windows"` — the `spawn`
// dispatch above already falls through cleanly for OSes without a
// backend (Windows port plan Task 1.1).
// -------------------------------------------------------------------------

#[cfg(all(feature = "tray-backend", target_os = "linux"))]
mod backend_linux;

// -------------------------------------------------------------------------
// Windows backend (`tray-icon` Shell_NotifyIcon over the shared menu
// model, driven by a dedicated Win32 message-pump thread). Slots in
// behind `target_os = "windows"` exactly as the Linux/macOS backends
// do; the `spawn` dispatch above already routes to it (Windows port
// plan Task 6.2).
// -------------------------------------------------------------------------

#[cfg(all(feature = "tray-backend", target_os = "windows"))]
mod backend_windows;
