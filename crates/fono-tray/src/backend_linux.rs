// SPDX-License-Identifier: GPL-3.0-only
//! Linux tray backend: pure-Rust StatusNotifierItem (SNI) over
//! D-Bus via `ksni`, interpreting the platform-neutral
//! [`crate::menu::MenuNode`] tree. Moved verbatim out of `lib.rs`
//! per Windows port plan Task 1.1 so each OS backend lives in its
//! own file (`backend_linux.rs` / `backend_macos.rs` / a future
//! `backend_windows.rs`).

use super::menu::{self, MenuInputs, MenuNode};
use super::{
    ActiveBackends, ActiveProvider, DiscoveredSttProvider, GpuUpgradeProvider, LlmEnabledProvider,
    McpEnabledProvider, MicrophonesProvider, PreferencesProvider, PreferencesSnapshot,
    RecentProvider, TrayAction, TrayState, UpdateProvider, WyomingEnabledProvider,
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
                StandardItem { label: label.clone(), enabled: false, ..Default::default() }.into()
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
    let (r, g, b) = menu::state_color(state);
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
