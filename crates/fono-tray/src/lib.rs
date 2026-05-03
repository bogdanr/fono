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
//! - **STT / LLM backend submenus** — switch the active provider on
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

/// Provider that returns the most recent transcription labels (newest
/// first) for display in the tray's "Recent" submenu. Called from the
/// tray task on a poll interval.
pub type RecentProvider = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// Provider that returns `(stt_idx, llm_idx)` — indices into
/// `stt_labels` / `llm_labels` (the slices passed to [`spawn`]) for
/// the currently-active STT and LLM backends. Polled every ~2 s; the
/// tray repaints the active marker (`●`) when the indices change.
///
/// `u8::MAX` for either index means "unknown / not in the list" and
/// renders no checkmark — useful when the active backend isn't one
/// fono knows about (custom OpenAI-compatible endpoint etc.).
pub type ActiveProvider = Arc<dyn Fn() -> (u8, u8) + Send + Sync>;

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
    pub sound_feedback: bool,
    pub auto_mute_system: bool,
    pub always_warm_mic: bool,
    pub also_copy_to_clipboard: bool,
    pub startup_autostart: bool,
    pub vad_enabled: bool,
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
/// `TrayAction::SetWaveformStyle(u8)`.
pub const WAVEFORM_STYLES: &[(&str, &str)] = &[
    ("bars", "Bars"),
    ("oscilloscope", "Oscilloscope"),
    ("fft", "FFT"),
    ("heatmap", "Heatmap"),
];

/// Auto-stop silence presets surfaced in the tray's radio group.
/// `0` means "Off" — the daemon's auto-stop silence timer is bypassed
/// when the value is zero. Indices map to `TrayAction::SetAutoStopSilenceMs(u32)`.
pub const AUTO_STOP_PRESETS_MS: &[(&str, u32)] =
    &[("Off", 0), ("0.8 s", 800), ("1.5 s", 1500), ("3 s", 3000)];

/// FSM-aligned tray state used to tint the icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TrayState {
    Idle = 0,
    Recording = 1,
    Processing = 2,
    Paused = 3,
}

/// User actions fired from the tray menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    ShowStatus,
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
    /// Switch the active LLM backend. Index into the `llm_labels`
    /// slice passed to [`spawn`].
    UseLlm(u8),
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
    /// Toggle `general.sound_feedback` (start/stop chimes).
    SetSoundFeedback(bool),
    /// Toggle `general.auto_mute_system` (mute other audio while recording).
    SetAutoMuteSystem(bool),
    /// Toggle `general.always_warm_mic` (keep cpal stream open between dictations).
    SetAlwaysWarmMic(bool),
    /// Toggle `general.also_copy_to_clipboard`.
    SetAlsoCopyToClipboard(bool),
    /// Toggle `general.startup_autostart`.
    SetStartupAutostart(bool),
    /// Toggle VAD by flipping `audio.vad_backend` between `"silero"` (on)
    /// and `"off"`. The tray uses a boolean for menu legibility; the
    /// daemon translates back to the string field.
    SetVadEnabled(bool),
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
            _ => TrayState::Idle,
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
/// * `stt_labels` / `llm_labels` — display strings for each STT / LLM
///   backend, in canonical order (the order the daemon also iterates
///   when decoding indices back to `SttBackend` / `LlmBackend`).
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
    llm_labels: Vec<String>,
    active_provider: ActiveProvider,
    discovered_stt_provider: DiscoveredSttProvider,
    update_provider: UpdateProvider,
    gpu_upgrade_provider: GpuUpgradeProvider,
    microphones_provider: MicrophonesProvider,
    preferences_provider: PreferencesProvider,
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
            llm_labels,
            active_provider,
            discovered_stt_provider,
            update_provider,
            gpu_upgrade_provider,
            microphones_provider,
            preferences_provider,
        );
        let state_tx = if started { Some(state_tx) } else { None };
        (
            Tray {
                shared_state: shared,
                state_tx,
            },
            action_rx,
        )
    }

    #[cfg(not(feature = "tray-backend"))]
    {
        (
            Tray {
                shared_state: shared,
            },
            action_rx,
        )
    }
}

// -------------------------------------------------------------------------
// Real backend (pure-Rust SNI via `ksni`).
// -------------------------------------------------------------------------

#[cfg(feature = "tray-backend")]
mod backend {
    use super::{
        ActiveProvider, DiscoveredSttProvider, GpuUpgradeProvider, MicrophonesProvider,
        PreferencesProvider, PreferencesSnapshot, RecentProvider, TrayAction, TrayState,
        UpdateProvider, AUTO_STOP_PRESETS_MS, LANGUAGE_SHORTLIST, RECENT_SLOTS, WAVEFORM_STYLES,
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
        llm_labels: Vec<String>,
        active: (u8, u8),
        discovered_stt: Vec<String>,
        update_label: Option<String>,
        gpu_upgrade_label: Option<String>,
        microphones: (Vec<String>, u8),
        prefs: PreferencesSnapshot,
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

        // Left-click/status activation. Tray hosts that call the SNI
        // `Activate` method (including snixembed) show the same status
        // notification as the explicit menu action; right-click still
        // opens the D-Bus menu through the host's normal ContextMenu
        // path.
        fn activate(&mut self, _x: i32, _y: i32) {
            let _ = self.actions.send(TrayAction::ShowStatus);
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
        llm_labels: Vec<String>,
        active_provider: ActiveProvider,
        discovered_stt_provider: DiscoveredSttProvider,
        update_provider: UpdateProvider,
        gpu_upgrade_provider: GpuUpgradeProvider,
        microphones_provider: MicrophonesProvider,
        preferences_provider: PreferencesProvider,
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
                llm_labels,
                active_provider,
                discovered_stt_provider,
                update_provider,
                gpu_upgrade_provider,
                microphones_provider,
                preferences_provider,
            )
            .await
            {
                Err(e) if is_missing_status_notifier_watcher(&e) => {
                    notify_missing_status_notifier_watcher();
                    tracing::warn!(
                        "tray unavailable: no StatusNotifierWatcher is registered on the session bus; \
                         start a tray host/watcher (for example KDE Plasma's tray, waybar with tray, \
                         xfce4-panel, or snixembed) or run with --no-tray. Dictation and the overlay \
                         continue without the tray icon."
                    );
                }
                Err(e) => {
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

    fn is_missing_status_notifier_watcher(err: &anyhow::Error) -> bool {
        let msg = err.to_string();
        msg.contains("org.kde.StatusNotifierWatcher") || msg.contains("StatusNotifierWatcher")
    }

    #[allow(clippy::too_many_arguments)]
    async fn run(
        tooltip: String,
        actions: mpsc::UnboundedSender<TrayAction>,
        mut state_rx: mpsc::UnboundedReceiver<TrayState>,
        recent_provider: RecentProvider,
        stt_labels: Vec<String>,
        llm_labels: Vec<String>,
        active_provider: ActiveProvider,
        discovered_stt_provider: DiscoveredSttProvider,
        update_provider: UpdateProvider,
        gpu_upgrade_provider: GpuUpgradeProvider,
        microphones_provider: MicrophonesProvider,
        preferences_provider: PreferencesProvider,
    ) -> anyhow::Result<()> {
        let model = KsniTray {
            tooltip,
            state: TrayState::Idle,
            recent: Vec::new(),
            stt_labels,
            llm_labels,
            active: (u8::MAX, u8::MAX),
            discovered_stt: Vec::new(),
            update_label: None,
            gpu_upgrade_label: None,
            microphones: (Vec::new(), u8::MAX),
            prefs: PreferencesSnapshot::default(),
            actions,
        };

        // `TrayMethods::spawn` connects to the session bus, registers
        // with `org.kde.StatusNotifierWatcher`, and returns a handle.
        // On hosts without a watcher (no DISPLAY, no D-Bus session
        // bus, etc.) this errors immediately — we surface it as a
        // warn! and let the rest of the daemon run unaffected.
        let handle: Handle<KsniTray> = model
            .spawn()
            .await
            .map_err(|e| anyhow::anyhow!("ksni::Tray::spawn failed: {e}"))?;

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
        let mut last_active: (u8, u8) = (u8::MAX, u8::MAX);
        let mut last_discovered_stt: Vec<String> = Vec::new();
        let mut last_upd: Option<String> = None;
        let mut last_gpu_upd: Option<String> = None;
        let mut last_mics: (Vec<String>, u8) = (Vec::new(), u8::MAX);
        let mut last_prefs: PreferencesSnapshot = PreferencesSnapshot::default();

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

                    let changed = recent != last_recent
                        || active != last_active
                        || discovered_stt != last_discovered_stt
                        || upd != last_upd
                        || gpu_upd != last_gpu_upd
                        || mics != last_mics
                        || prefs != last_prefs;
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

                    handle.update(move |t: &mut KsniTray| {
                        t.recent = recent;
                        t.active = active;
                        t.discovered_stt = discovered_stt;
                        t.update_label = upd;
                        t.gpu_upgrade_label = gpu_upd;
                        t.microphones = mics;
                        t.prefs = prefs;
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
                label: "Toggle recording  (F9)".into(),
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
                let active = u8::try_from(i).is_ok_and(|i_u8| i_u8 == t.active.0);
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
            SubMenu {
                label: "STT backend".into(),
                submenu: stt_items,
                ..Default::default()
            }
            .into(),
        );

        // LLM backend submenu.
        if t.llm_labels.is_empty() {
            tracing::warn!(
                "tray: llm_labels is empty during build_menu — \
                 daemon should have populated at least the active backend"
            );
        }
        let mut llm_items: Vec<MenuItem<KsniTray>> = t
            .llm_labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let active = u8::try_from(i).is_ok_and(|i_u8| i_u8 == t.active.1);
                let prefix = if active { "● " } else { "  " };
                let idx_u8 = u8::try_from(i).unwrap_or(u8::MAX);
                StandardItem {
                    label: format!("{prefix}{label}"),
                    activate: send_action(TrayAction::UseLlm(idx_u8)),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        if llm_items.is_empty() {
            llm_items.push(
                StandardItem {
                    label: "(no backends configured — `fono keys add …`)".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(
            SubMenu {
                label: "LLM backend".into(),
                submenu: llm_items,
                ..Default::default()
            }
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
                SubMenu {
                    label: "Microphone".into(),
                    submenu: mic_items,
                    ..Default::default()
                }
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
            "Play start/stop chimes",
            p.sound_feedback,
            TrayAction::SetSoundFeedback,
        ));
        items.push(prefs_check(
            "Mute system audio while recording",
            p.auto_mute_system,
            TrayAction::SetAutoMuteSystem,
        ));
        items.push(prefs_check(
            "Keep microphone always-on (faster start, see privacy docs)",
            p.always_warm_mic,
            TrayAction::SetAlwaysWarmMic,
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

        items.push(MenuItem::Separator);

        // Radio submenus — the parent label always carries the
        // current selection in the form "Title: <value>" so even if
        // a tray host renders nested submenus oddly, the user can
        // see the live state without expanding. The children carry
        // the `● ` active marker for the picked row.

        let auto_stop_label = AUTO_STOP_PRESETS_MS
            .iter()
            .find(|(_, ms)| *ms == p.auto_stop_silence_ms)
            .map_or_else(
                || format!("{} ms", p.auto_stop_silence_ms),
                |(s, _)| (*s).to_string(),
            );
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

        let waveform_label = WAVEFORM_STYLES
            .get(p.waveform_style as usize)
            .map_or("Bars", |(_, l)| *l);
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

        SubMenu {
            label: "Preferences".into(),
            submenu: items,
            ..Default::default()
        }
        .into()
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
            TrayState::Recording => (0xef, 0x44, 0x44),  // red
            TrayState::Processing => (0xf5, 0x9e, 0x0b), // amber
            TrayState::Paused => (0x6b, 0x72, 0x80),     // grey
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
        ksni::Icon {
            width: SIZE,
            height: SIZE,
            data,
        }
    }
}
