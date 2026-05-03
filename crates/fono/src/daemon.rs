// SPDX-License-Identifier: GPL-3.0-only
//! Daemon event loop: startup banner, global-hotkey listener, tray icon,
//! IPC server, FSM dispatcher, and the [`crate::session::SessionOrchestrator`]
//! that drives audio → STT → LLM → inject end to end.

use anyhow::{Context, Result};
use fono_core::{Config, Paths, Secrets};
use fono_hotkey::{
    HotkeyAction, HotkeyBindings, HotkeyControl, HotkeyControlSender, HotkeyEvent, RecordingFsm,
};
use fono_ipc::{read_frame, write_frame, Request, Response};
use fono_tray::{TrayAction, TrayState};
#[cfg(feature = "interactive")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

use crate::cli::Verbosity;
use crate::session::SessionOrchestrator;

/// Translate the standard `Hold*` / `Toggle*` actions to their `Live*`
/// counterparts when `[interactive].enabled = true`, the binary was
/// built with `--features interactive`, and an orchestrator is
/// available to drive the live state. Off otherwise — the action is
/// passed through unchanged.
///
/// `CancelPressed`, `ProcessingDone`, and `ProcessingStarted` are
/// always passed through; the FSM already routes Cancel from any
/// state to Idle.
fn translate_for_interactive(
    action: HotkeyAction,
    config: &Config,
    orchestrator_present: bool,
) -> HotkeyAction {
    #[cfg(feature = "interactive")]
    {
        if orchestrator_present && config.interactive.enabled {
            return match action {
                HotkeyAction::HoldPressed => HotkeyAction::LiveHoldPressed,
                HotkeyAction::HoldReleased => HotkeyAction::LiveHoldReleased,
                HotkeyAction::TogglePressed => HotkeyAction::LiveTogglePressed,
                other => other,
            };
        }
    }
    #[cfg(not(feature = "interactive"))]
    {
        let _ = (config, orchestrator_present);
    }
    action
}

/// One-shot guard so the "Live dictation active" desktop notification
/// fires only on the first successful `StartLiveDictation` per daemon
/// run. Helps users discover the overlay without nagging on every
/// hotkey press.
#[cfg(feature = "interactive")]
static LIVE_FIRST_RUN_NOTIFIED: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "interactive")]
fn notify_live_first_run() {
    // Mark first-run for any future use; the toast was removed because
    // the on-screen overlay is the user-visible indicator.
    let _ =
        LIVE_FIRST_RUN_NOTIFIED.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst);
}

#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
pub async fn run(paths: &Paths, no_tray: bool, verbosity: Verbosity) -> Result<()> {
    let config = Arc::new(Config::load(&paths.config_file()).context("load config")?);
    let secrets = Secrets::load(&paths.secrets_file()).context("load secrets")?;
    print_banner(paths, &config, no_tray, verbosity);

    // Single-instance guard via the IPC socket. If another daemon is
    // already running it answers `connect()`; bail before we duplicate
    // hotkey grabs, audio captures, and model loads. A stale socket
    // file from a crashed previous run yields ConnectionRefused (or
    // ENOENT) and the bind below replaces it cleanly.
    let socket_path = paths.ipc_socket();
    if socket_path.exists() {
        match tokio::net::UnixStream::connect(&socket_path).await {
            Ok(_) => anyhow::bail!(
                "another fono daemon is already running (IPC socket {} is live). \
                 Stop it before starting a new instance.",
                socket_path.display()
            ),
            Err(_) => {
                tracing::debug!(
                    "stale IPC socket at {} — cleaning up and continuing",
                    socket_path.display()
                );
            }
        }
    }

    write_pid(paths)?;

    // Ensure referenced models are on disk before we wire the orchestrator.
    if let Err(e) = crate::models::ensure_models(paths, &config).await {
        warn!("model preflight failed: {e:#}");
    }

    // ---------------------------------------------------------------
    // FSM + channels
    // ---------------------------------------------------------------
    let (fsm, mut fsm_events) = RecordingFsm::new();
    let fsm = Arc::new(Mutex::new(fsm));
    let (action_tx, mut action_rx) = mpsc::unbounded_channel::<HotkeyAction>();

    // ---------------------------------------------------------------
    // Build the orchestrator. STT failure → degraded mode (hotkeys
    // still register but recording emits a warning instead of audio).
    // ---------------------------------------------------------------
    let orchestrator: Option<Arc<SessionOrchestrator>> =
        match SessionOrchestrator::new(Arc::clone(&config), &secrets, paths, action_tx.clone()) {
            Ok(o) => Some(Arc::new(o)),
            Err(e) => {
                warn!(
                    "STT backend unavailable; daemon running in DEGRADED mode \
                 (hotkeys are live but recording will be skipped): {e:#}"
                );
                None
            }
        };

    // ---------------------------------------------------------------
    // LAN Wyoming server (Slice 3 of the network plan). Off by default;
    // spawned only when `[server.wyoming].enabled = true` *and* the
    // orchestrator came up (degraded mode skips serving — there's no
    // STT backend to host). The handle is dropped on daemon exit which
    // closes the listener; in-flight connections finish naturally.
    // ---------------------------------------------------------------
    let _wyoming_server: Option<fono_net::wyoming::server::WyomingServerHandle> =
        spawn_wyoming_server_if_enabled(&config, orchestrator.as_ref()).await;

    // ---------------------------------------------------------------
    // mDNS discovery (Slice 4 of the network plan). The browser is
    // always on when the daemon can create an mDNS service daemon — it
    // populates the LAN registry exposed via IPC `ListDiscovered`. The
    // advertiser only runs when a `[server.*]` block is enabled.
    // ---------------------------------------------------------------
    let discovery = spawn_discovery_if_enabled(&config).await;
    let discovery_registry: Option<fono_net::discovery::Registry> =
        discovery.as_ref().map(|d| d.registry.clone());

    // ---------------------------------------------------------------
    // Global hotkey listener
    //
    // Skipped entirely on headless hosts (no `DISPLAY`, no
    // `WAYLAND_DISPLAY`). The `global-hotkey` 0.6.4 crate spawns an X11
    // events_processor thread that calls `XOpenDisplay(NULL)` and then
    // dereferences the returned pointer via `XDefaultRootWindow` *without
    // checking for NULL* — when no X server is reachable the thread
    // segfaults, taking the whole daemon with it. The same `fono`
    // binary is used for headless inference servers (see `fono serve`),
    // so we runtime-gate exactly the way the tray already does
    // (see "Tray icon — runtime-gated" below).
    //
    // The daemon still serves IPC on the headless path, so
    // `fono toggle`, `fono record`, `fono transcribe`, etc. continue to
    // work; only the global hotkey grab is unavailable, which is
    // meaningless on a host with no kernel input focus anyway.
    // ---------------------------------------------------------------
    let bindings = HotkeyBindings {
        hold: config.hotkeys.hold.clone(),
        toggle: config.hotkeys.toggle.clone(),
        cancel: config.hotkeys.cancel.clone(),
    };
    let cancel_ctrl: Option<HotkeyControlSender> = if crate::is_graphical_session() {
        match fono_hotkey::spawn_listener(bindings, action_tx.clone()) {
            Ok(handle) => {
                debug!("global hotkeys registered");
                Some(handle.control)
            }
            Err(e) => {
                warn!(
                    "global hotkeys unavailable: {e:#}\n  \
                 (the daemon will still accept `fono toggle` via IPC)"
                );
                None
            }
        }
    } else {
        debug!("hotkey listener skipped (headless: no DISPLAY / WAYLAND_DISPLAY)");
        None
    };

    // ---------------------------------------------------------------
    // Background update checker — hits GitHub releases once on startup
    // and surfaces the result through `update_status` so the tray menu
    // (and IPC consumers) can render an "Update to vX.Y.Z" entry.
    // Honours `[update].auto_check`, `FONO_NO_UPDATE_CHECK=1`, and the
    // configured channel. Disabled entirely on package-managed installs
    // to avoid fighting the distro. One-shot rather than periodic — fono
    // is started often enough that a recurring timer would just add log
    // noise without catching releases any sooner.
    // ---------------------------------------------------------------
    let update_status: Arc<RwLock<Option<fono_update::UpdateStatus>>> = Arc::new(RwLock::new(None));
    {
        let cache_path = paths.state_dir.join("update.json");
        let current_version = env!("CARGO_PKG_VERSION");
        if let Some(cached) = fono_update::load_cache(&cache_path) {
            // Reject a stale cache whose `current` field doesn't match
            // the running binary (typical case: user just upgraded
            // 0.5.0 → 0.6.0 and the old cache still claims 0.5.0,
            // which would surface a bogus "Update available" entry
            // until the 10-second background re-check overwrites it).
            if cached.status.current() == current_version {
                *update_status.write().expect("update_status lock") = Some(cached.status);
            } else {
                tracing::debug!(
                    "update cache: discarding entry for v{} (running v{})",
                    cached.status.current(),
                    current_version
                );
            }
        }
        let pkg_managed = std::env::current_exe()
            .map(|p| fono_update::is_package_managed(&p))
            .unwrap_or(false);
        let auto_check = config.update.auto_check
            && !pkg_managed
            && std::env::var_os("FONO_NO_UPDATE_CHECK").is_none_or(|v| v != "1");
        if auto_check {
            let channel = fono_update::Channel::parse(&config.update.channel)
                .unwrap_or(fono_update::Channel::Stable);
            let status_for_task = Arc::clone(&update_status);
            let cache_path_task = cache_path;
            tokio::spawn(async move {
                // Brief delay so the check doesn't compete with daemon
                // startup work (audio init, model load, tray spawn).
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                let current = env!("CARGO_PKG_VERSION");
                let current_prefix = crate::variant::VARIANT.release_asset_prefix();
                let st = fono_update::check(current, current_prefix, channel).await;
                match &st {
                    fono_update::UpdateStatus::Available { info: rel, .. } => {
                        if rel.is_variant_switch_only(current) {
                            info!(
                                "update check: GPU build available for v{} ({} — same version, \
                                 different variant; click \"Update for GPU acceleration\" in the tray to switch)",
                                rel.version, rel.asset_name
                            );
                        } else {
                            info!(
                                "update check: new version available {current} -> {} ({})",
                                rel.version, rel.html_url
                            );
                        }
                    }
                    fono_update::UpdateStatus::UpToDate { current: c } => {
                        info!("update check: running latest version v{c}; no update available");
                    }
                    fono_update::UpdateStatus::CheckFailed { error, .. } => {
                        info!("update check: failed ({error}); will retry on next start");
                    }
                }
                fono_update::save_cache(&cache_path_task, &st);
                if let Ok(mut guard) = status_for_task.write() {
                    *guard = Some(st);
                }
            });
        } else if pkg_managed {
            debug!("update checker disabled: binary is package-managed");
        } else {
            debug!("update checker disabled by config / env");
        }
    }

    // ---------------------------------------------------------------
    // Tray icon — runtime-gated.
    //
    // The single Fono binary serves both graphical desktops and headless
    // servers. The tray crate is always compiled in; we just refuse to
    // spawn it when (a) the operator passed `--no-tray`, or (b) the host
    // is headless (no `DISPLAY` and no `WAYLAND_DISPLAY` in the daemon's
    // environment). On a headless host attempting to bring up an SNI
    // tray either fails noisily (no D-Bus session bus) or blocks the
    // libappindicator thread forever — neither is acceptable for the
    // `fono serve` use case.
    //
    // See `plans/2026-04-30-fono-single-binary-size-v1.md` Phase 3
    // Task 3.1 for the runtime-detection contract.
    // ---------------------------------------------------------------
    let graphical = crate::is_graphical_session();
    let (tray, mut tray_rx) = if no_tray || !graphical {
        if no_tray {
            debug!("tray disabled (--no-tray)");
        } else {
            debug!("tray skipped (headless: no DISPLAY / WAYLAND_DISPLAY)");
        }
        let (_tx, rx) = mpsc::unbounded_channel::<TrayAction>();
        (None, rx)
    } else {
        // Tray menu's "Recent transcriptions" submenu reads from the
        // history DB on a 2-second poll. Provide a closure that returns
        // the cleaned text (or raw if no LLM cleanup) of the last 10 rows.
        let history_db_path = paths.history_db();
        let recent_provider: fono_tray::RecentProvider = Arc::new(move || {
            let Ok(db) = fono_core::history::HistoryDb::open(&history_db_path) else {
                return Vec::new();
            };
            db.recent(fono_tray::RECENT_SLOTS)
                .map(|rows| {
                    rows.into_iter()
                        .map(|r| r.cleaned.unwrap_or(r.raw))
                        .collect()
                })
                .unwrap_or_default()
        });

        // STT / LLM submenu labels — restricted to backends the user
        // has actually configured (Local, plus any cloud backend whose
        // API key is present in secrets.toml or the environment). The
        // active backend is always included even if its key is missing
        // so the tray reflects reality. Snapshot at startup; users who
        // add a new key via `fono keys add` need to restart the daemon
        // to see it appear in the menu (v0.1 trade-off).
        let stt_backends: Vec<_> =
            fono_core::providers::configured_stt_backends(&secrets, &config.stt.backend);
        let llm_backends: Vec<_> =
            fono_core::providers::configured_llm_backends(&secrets, &config.llm.backend);
        let stt_labels: Vec<String> = stt_backends
            .iter()
            .map(|b| fono_core::providers::stt_backend_str(b).to_string())
            .collect();
        let llm_labels: Vec<String> = llm_backends
            .iter()
            .map(|b| fono_core::providers::llm_backend_str(b).to_string())
            .collect();

        // Startup diagnostic for the "tray STT/LLM submenu sometimes
        // empty" intermittent — these labels are static after spawn,
        // so logging them once gives us a definitive answer about
        // whether the issue is data (empty here means empty menu) or
        // host rendering (non-empty here + empty menu means it's
        // KDE/GNOME mishandling LayoutUpdated). Always at info so
        // users running with default verbosity see the line in their
        // terminal when they reproduce.
        info!(
            "tray: configured STT backends ({}) = {:?}",
            stt_labels.len(),
            stt_labels
        );
        info!(
            "tray: configured LLM backends ({}) = {:?}",
            llm_labels.len(),
            llm_labels
        );

        // Active-provider closure — tray polls this every ~2 s. Reads
        // the orchestrator's current backend pair (which already reflects
        // any `Reload`-driven hot-swap) and falls back to the on-disk
        // config string match when the orchestrator isn't available
        // (degraded mode).
        let orch_for_tray = orchestrator.clone();
        let config_path = paths.config_file();
        let active_provider: fono_tray::ActiveProvider = Arc::new(move || {
            let (stt_str, llm_str) = orch_for_tray.as_ref().map_or_else(
                || {
                    fono_core::Config::load(&config_path)
                        .map(|c| {
                            (
                                fono_core::providers::stt_backend_str(&c.stt.backend).to_string(),
                                fono_core::providers::llm_backend_str(&c.llm.backend).to_string(),
                            )
                        })
                        .unwrap_or_else(|_| ("local".into(), "none".into()))
                },
                |o| o.active_backends(),
            );
            let stt_idx = stt_backends
                .iter()
                .position(|b| fono_core::providers::stt_backend_str(b) == stt_str)
                .and_then(|i| u8::try_from(i).ok())
                .unwrap_or(u8::MAX);
            let llm_idx = llm_backends
                .iter()
                .position(|b| fono_core::providers::llm_backend_str(b) == llm_str)
                .and_then(|i| u8::try_from(i).ok())
                .unwrap_or(u8::MAX);
            (stt_idx, llm_idx)
        });

        let discovered_registry_for_tray = discovery_registry.clone();
        let local_wyoming_fullname_for_tray = local_wyoming_fullname(&config);
        let discovered_stt_provider: fono_tray::DiscoveredSttProvider = Arc::new(move || {
            discovered_wyoming_peers_for_tray(
                discovered_registry_for_tray.as_ref(),
                local_wyoming_fullname_for_tray.as_deref(),
            )
            .into_iter()
            .map(|p| p.tray_label())
            .collect()
        });

        // Preferences provider for the "Preferences" submenu — reads
        // the on-disk config every ~2 s so the tray reflects external
        // edits (`fono config edit`, `fono settings`) without a daemon
        // restart. Cheap: a single TOML parse, dropped after the
        // closure returns. We deliberately read disk rather than the
        // in-process Config Arc because tray-driven mutations write
        // to disk and reload the orchestrator — disk is the source of
        // truth that survives every code path.
        let config_path_for_prefs = paths.config_file();
        let preferences_provider: fono_tray::PreferencesProvider =
            Arc::new(move || preferences_snapshot_from_disk(&config_path_for_prefs));

        let (t, rx) = fono_tray::spawn(
            "Fono — voice dictation",
            recent_provider,
            stt_labels,
            llm_labels,
            active_provider,
            discovered_stt_provider,
            {
                // Render the "Update to vX.Y.Z" tray label whenever the
                // background checker has surfaced a newer release.
                // Returning `None` keeps the menu item on its default
                // "Check for updates…" copy, which still works as an
                // on-demand trigger when the user clicks it.
                let status = Arc::clone(&update_status);
                Arc::new(move || update_label(&status)) as fono_tray::UpdateProvider
            },
            {
                // Render the "Update for GPU acceleration" entry only on
                // a CPU-variant build with a usable Vulkan host. Probed
                // once at daemon startup and cached for the session —
                // hardware doesn't grow a GPU between ticks.
                let probe_is_usable =
                    matches!(crate::variant::VARIANT, crate::variant::Variant::Cpu,)
                        && fono_core::vulkan_probe::probe().is_usable();
                Arc::new(move || {
                    if probe_is_usable {
                        Some("Update for GPU acceleration".to_string())
                    } else {
                        None
                    }
                }) as fono_tray::GpuUpgradeProvider
            },
            {
                // Microphones provider for the "Microphone" submenu.
                // On Pulse/PipeWire hosts the audio server is the
                // authority on what's a microphone; on `Unknown` hosts
                // (macOS / Windows / pure-ALSA) the submenu is hidden
                // because the tray can't actually switch capture there.
                Arc::new(move || {
                    use fono_audio::devices::{list_input_devices, InputBackend};
                    let devices = list_input_devices();
                    // Hide the submenu on cpal-only hosts: leaving it
                    // populated would offer a switch we can't honour.
                    let pulse_only: Vec<_> = devices
                        .iter()
                        .filter(|d| matches!(d.backend, InputBackend::Pulse { .. }))
                        .collect();
                    if pulse_only.is_empty() {
                        return (Vec::new(), u8::MAX);
                    }
                    let names: Vec<String> =
                        pulse_only.iter().map(|d| d.display_name.clone()).collect();
                    let active_idx = pulse_only
                        .iter()
                        .position(|d| d.is_default)
                        .and_then(|i| u8::try_from(i).ok())
                        .unwrap_or(u8::MAX);
                    (names, active_idx)
                }) as fono_tray::MicrophonesProvider
            },
            preferences_provider,
        );
        (Some(t), rx)
    };
    let tray = Arc::new(tray);

    // ---------------------------------------------------------------
    // FSM event consumer — drives the orchestrator pipeline.
    // ---------------------------------------------------------------
    {
        let tray = Arc::clone(&tray);
        let action_tx_ev = action_tx.clone();
        let orch = orchestrator.clone();
        let cancel_ctrl_ev = cancel_ctrl.clone();
        tokio::spawn(async move {
            while let Some(e) = fsm_events.recv().await {
                debug!("fsm event: {e:?}");
                if let Some(ctrl) = cancel_ctrl_ev.as_ref() {
                    match e {
                        HotkeyEvent::StartRecording(_) | HotkeyEvent::StartLiveDictation(_) => {
                            let _ = ctrl.send(HotkeyControl::EnableCancel);
                        }
                        HotkeyEvent::StopRecording
                        | HotkeyEvent::StopLiveDictation
                        | HotkeyEvent::Cancel => {
                            let _ = ctrl.send(HotkeyControl::DisableCancel);
                        }
                    }
                }
                if let Some(t) = tray.as_ref().as_ref() {
                    match e {
                        HotkeyEvent::StartRecording(_) | HotkeyEvent::StartLiveDictation(_) => {
                            t.set_state(TrayState::Recording);
                        }
                        HotkeyEvent::StopRecording | HotkeyEvent::StopLiveDictation => {
                            t.set_state(TrayState::Processing);
                        }
                        HotkeyEvent::Cancel => t.set_state(TrayState::Idle),
                    }
                }
                let Some(o) = orch.as_ref() else {
                    // Degraded mode: just emit ProcessingDone so the FSM
                    // returns to Idle without us hanging.
                    if matches!(
                        e,
                        HotkeyEvent::StopRecording
                            | HotkeyEvent::StopLiveDictation
                            | HotkeyEvent::Cancel
                    ) {
                        let _ = action_tx_ev.send(HotkeyAction::ProcessingDone);
                        if let Some(t) = tray.as_ref().as_ref() {
                            t.set_state(TrayState::Idle);
                        }
                    }
                    continue;
                };
                match e {
                    HotkeyEvent::StartRecording(mode) => {
                        if let Err(err) = o.on_start_recording(mode).await {
                            warn!("start_recording failed: {err:#}");
                            if let Some(t) = tray.as_ref().as_ref() {
                                t.set_state(TrayState::Idle);
                            }
                            let _ = action_tx_ev.send(HotkeyAction::ProcessingDone);
                        }
                    }
                    HotkeyEvent::StopRecording => {
                        let tray_for_done = Arc::clone(&tray);
                        let o = Arc::clone(o);
                        tokio::spawn(async move {
                            o.on_stop_recording().await;
                            let _ = tray_for_done;
                        });
                    }
                    HotkeyEvent::Cancel => {
                        let o = Arc::clone(o);
                        tokio::spawn(async move {
                            o.on_cancel().await;
                        });
                    }
                    // Plan R7.4: live-dictation start/stop. Wires the
                    // streaming pipeline through the orchestrator's
                    // dedicated `on_start_live_dictation` /
                    // `on_stop_live_dictation` methods when the
                    // `interactive` feature is compiled in. Slim
                    // builds keep the original batch-fallback path so
                    // a config with `[interactive].enabled = true`
                    // still produces working dictation when the
                    // feature was opted out at build time.
                    #[cfg(feature = "interactive")]
                    HotkeyEvent::StartLiveDictation(mode) => {
                        tracing::debug!("live: started ({mode:?})");
                        if let Err(err) = o.on_start_live_dictation(mode).await {
                            warn!(
                                "start_live_dictation failed: {err:#} — \
                                 falling back to batch path"
                            );
                            if let Err(err2) = o.on_start_recording(mode).await {
                                warn!("start_recording fallback also failed: {err2:#}");
                                if let Some(t) = tray.as_ref().as_ref() {
                                    t.set_state(TrayState::Idle);
                                }
                                let _ = action_tx_ev.send(HotkeyAction::ProcessingDone);
                            }
                        } else {
                            notify_live_first_run();
                        }
                    }
                    #[cfg(feature = "interactive")]
                    HotkeyEvent::StopLiveDictation => {
                        tracing::info!("live: stopped");
                        let o = Arc::clone(o);
                        tokio::spawn(async move {
                            o.on_stop_live_dictation().await;
                        });
                    }
                    #[cfg(not(feature = "interactive"))]
                    HotkeyEvent::StartLiveDictation(mode) => {
                        tracing::info!(
                            "live-dictation start ({mode:?}) — falling back to batch path \
                             (binary built without `--features interactive`)"
                        );
                        if let Err(err) = o.on_start_recording(mode).await {
                            warn!("start_recording failed: {err:#}");
                            if let Some(t) = tray.as_ref().as_ref() {
                                t.set_state(TrayState::Idle);
                            }
                            let _ = action_tx_ev.send(HotkeyAction::ProcessingDone);
                        }
                    }
                    #[cfg(not(feature = "interactive"))]
                    HotkeyEvent::StopLiveDictation => {
                        let o = Arc::clone(o);
                        tokio::spawn(async move {
                            o.on_stop_recording().await;
                        });
                    }
                }
            }
        });
    }

    // ---------------------------------------------------------------
    // Action dispatcher — drains HotkeyAction into the FSM. Also flips
    // the tray to Idle when the pipeline reports ProcessingDone.
    //
    // When `[interactive].enabled = true` and this build was compiled
    // with the `interactive` feature, incoming Hold/Toggle actions are
    // translated to their `Live*` variants so the FSM enters
    // `LiveDictating` and emits `StartLiveDictation` to the
    // orchestrator. The translation only fires when an orchestrator is
    // available — degraded mode (no orchestrator) skips it because
    // there is nothing to drive the live state through.
    // ---------------------------------------------------------------
    {
        let fsm = Arc::clone(&fsm);
        let tray = Arc::clone(&tray);
        let config_for_dispatch = Arc::clone(&config);
        let orch_for_dispatch = orchestrator.clone();
        tokio::spawn(async move {
            while let Some(action) = action_rx.recv().await {
                let action = translate_for_interactive(
                    action,
                    &config_for_dispatch,
                    orch_for_dispatch.is_some(),
                );
                let new_state = fsm.lock().await.dispatch(action);
                tracing::debug!("hotkey: {action:?} -> {new_state:?}");
                if matches!(action, HotkeyAction::ProcessingDone) {
                    if let Some(t) = tray.as_ref().as_ref() {
                        t.set_state(TrayState::Idle);
                    }
                }
            }
        });
    }

    // ---------------------------------------------------------------
    // Tray menu actions -> hotkey actions / process exit.
    // ---------------------------------------------------------------
    {
        let action_tx = action_tx.clone();
        let paths = paths.clone();
        let orch_for_tray = orchestrator.clone();
        // Snapshot the filtered backend lists for the tray dispatcher
        // so `UseStt(idx)` / `UseLlm(idx)` resolve to the same item the
        // user clicked (the indices come from the filtered submenu).
        let stt_backends_for_dispatch: Vec<_> =
            fono_core::providers::configured_stt_backends(&secrets, &config.stt.backend);
        let llm_backends_for_dispatch: Vec<_> =
            fono_core::providers::configured_llm_backends(&secrets, &config.llm.backend);
        let update_status_tray = Arc::clone(&update_status);
        let discovered_registry_for_dispatch = discovery_registry.clone();
        let local_wyoming_fullname_for_dispatch = local_wyoming_fullname(&config);
        tokio::spawn(async move {
            while let Some(ta) = tray_rx.recv().await {
                debug!("tray action: {ta:?}");
                match ta {
                    TrayAction::ToggleRecording => {
                        let _ = action_tx.send(HotkeyAction::TogglePressed);
                    }
                    TrayAction::Quit => {
                        let _ = std::fs::remove_file(paths.ipc_socket());
                        let _ = std::fs::remove_file(paths.pid_file());
                        std::process::exit(0);
                    }
                    TrayAction::ShowStatus => notify_last_transcription(&paths),
                    TrayAction::PasteHistory(idx) => paste_history_at(&paths, idx).await,
                    TrayAction::OpenConfig => open_path(&paths.config_file()),
                    TrayAction::Pause => {
                        debug!("tray: Pause hotkeys (not yet implemented)");
                    }
                    TrayAction::UseStt(idx) => {
                        switch_stt_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            &stt_backends_for_dispatch,
                            idx,
                        )
                        .await;
                    }
                    TrayAction::UseDiscoveredStt(idx) => {
                        switch_discovered_stt_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            discovered_registry_for_dispatch.as_ref(),
                            local_wyoming_fullname_for_dispatch.as_deref(),
                            idx,
                        )
                        .await;
                    }
                    TrayAction::UseLlm(idx) => {
                        switch_llm_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            &llm_backends_for_dispatch,
                            idx,
                        )
                        .await;
                    }
                    TrayAction::ApplyUpdate | TrayAction::UpdateForGpuAcceleration => {
                        // Both actions land here. `apply_update_via_tray`
                        // re-runs the version+variant check against the
                        // host's current Vulkan probe; on a CPU binary
                        // with a usable GPU it naturally picks up the
                        // `fono-gpu` asset for the same tag. So a click
                        // on either menu item resolves to the right
                        // download for this hardware.
                        apply_update_via_tray(Arc::clone(&update_status_tray)).await;
                    }
                    TrayAction::SetInputDevice(idx) => {
                        switch_input_device_via_tray(orch_for_tray.as_ref(), idx).await;
                    }
                    TrayAction::SetAutoMuteSystem(v) => {
                        apply_pref_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            "auto_mute_system",
                            move |cfg| cfg.general.auto_mute_system = v,
                        )
                        .await;
                    }
                    TrayAction::SetAlwaysWarmMic(v) => {
                        apply_pref_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            "always_warm_mic",
                            move |cfg| cfg.general.always_warm_mic = v,
                        )
                        .await;
                    }
                    TrayAction::SetAlsoCopyToClipboard(v) => {
                        apply_pref_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            "also_copy_to_clipboard",
                            move |cfg| cfg.general.also_copy_to_clipboard = v,
                        )
                        .await;
                    }
                    TrayAction::SetStartupAutostart(v) => {
                        apply_pref_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            "startup_autostart",
                            move |cfg| cfg.general.startup_autostart = v,
                        )
                        .await;
                    }
                    TrayAction::SetVadEnabled(v) => {
                        // The schema stores VAD as a string (`"silero"`,
                        // `"off"`, possibly more in the future); the tray
                        // exposes it as a boolean for menu legibility.
                        // Translate here. `"silero"` is the only enabled
                        // backend today; future backends will need their
                        // own tray entry rather than bundling under VAD.
                        apply_pref_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            "vad_backend",
                            move |cfg| {
                                cfg.audio.vad_backend =
                                    if v { "silero".into() } else { "off".into() };
                            },
                        )
                        .await;
                    }
                    TrayAction::SetAutoStopSilenceMs(ms) => {
                        apply_pref_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            "auto_stop_silence_ms",
                            move |cfg| cfg.audio.auto_stop_silence_ms = ms,
                        )
                        .await;
                    }
                    TrayAction::SetWaveformStyle(idx) => {
                        let Some(style) = waveform_style_from_idx(idx) else {
                            warn!("tray SetWaveformStyle({idx}): out of range");
                            continue;
                        };
                        apply_pref_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            "overlay.style",
                            move |cfg| cfg.overlay.style = style,
                        )
                        .await;
                    }
                    TrayAction::ToggleLanguage(idx) => {
                        let Some(code) = language_code_from_idx(idx) else {
                            warn!("tray ToggleLanguage({idx}): out of range");
                            continue;
                        };
                        apply_pref_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            "languages",
                            move |cfg| {
                                if let Some(pos) =
                                    cfg.general.languages.iter().position(|c| c == code)
                                {
                                    cfg.general.languages.remove(pos);
                                } else {
                                    cfg.general.languages.push(code.to_string());
                                }
                            },
                        )
                        .await;
                    }
                    TrayAction::ClearLanguages => {
                        apply_pref_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            "languages",
                            move |cfg| cfg.general.languages.clear(),
                        )
                        .await;
                    }
                    TrayAction::OpenSettingsTui => {
                        open_settings_tui();
                    }
                }
            }
        });
    }

    // ---------------------------------------------------------------
    // IPC server
    // ---------------------------------------------------------------
    let listener = fono_ipc::bind_listener(&paths.ipc_socket()).context("bind IPC socket")?;
    info!("daemon ready — press Ctrl+C to stop");
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("ctrl-c received; shutting down");
                break;
            }
            accept = listener.accept() => {
                let (stream, _) = accept?;
                let fsm = Arc::clone(&fsm);
                let action_tx = action_tx.clone();
                let orch = orchestrator.clone();
                let config = Arc::clone(&config);
                let registry = discovery_registry.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, fsm, action_tx, orch, config, registry).await {
                        warn!("client error: {e}");
                    }
                });
            }
        }
    }

    let _ = std::fs::remove_file(paths.ipc_socket());
    let _ = std::fs::remove_file(paths.pid_file());
    Ok(())
}

#[allow(clippy::cognitive_complexity)]
fn print_banner(paths: &Paths, config: &Config, no_tray: bool, verbosity: Verbosity) {
    let config_path = paths.config_file();
    let config_present = config_path.exists();
    info!(
        "Fono v{} starting — stt={:?} llm={:?} tray={}",
        env!("CARGO_PKG_VERSION"),
        config.stt.backend,
        config.llm.backend,
        if no_tray {
            "disabled"
        } else if cfg!(feature = "tray") {
            "enabled"
        } else {
            "not compiled"
        }
    );
    // [interactive] visibility — always emitted, even on slim builds
    // where the streaming pipeline is compiled out, so the user can
    // diagnose "I set `enabled = true` and nothing happened" without
    // turning on debug logging.
    #[cfg(feature = "interactive")]
    {
        info!(
            "interactive  : {} (mode={})",
            if config.interactive.enabled {
                "enabled"
            } else {
                "disabled"
            },
            config.interactive.mode,
        );
    }
    #[cfg(not(feature = "interactive"))]
    {
        if config.interactive.enabled {
            warn!(
                "interactive  : not compiled in (rebuild with `--features interactive`); \
                 `[interactive].enabled = true` in config will be ignored"
            );
        }
        // Else: silent. A slim build with `[interactive].enabled = false`
        // is a fully-supported configuration; no log line needed.
    }
    info!("variant      : {}", crate::variant::VARIANT.label());
    info!("hw accel     : {}", hardware_acceleration_summary());
    info!(
        "vulkan probe : {}",
        fono_core::vulkan_probe::probe().summary_line()
    );
    debug!(
        "config       : {} ({})",
        config_path.display(),
        if config_present {
            "loaded"
        } else {
            "absent — using defaults"
        }
    );
    debug!("secrets      : {}", paths.secrets_file().display());
    debug!("history db   : {}", paths.history_db().display());
    debug!("models/whisper: {}", paths.whisper_models_dir().display());
    debug!("models/llm   : {}", paths.llm_models_dir().display());
    debug!("cache        : {}", paths.cache_dir.display());
    debug!("state        : {}", paths.state_dir.display());
    debug!("ipc socket   : {}", paths.ipc_socket().display());
    debug!("log level    : {verbosity:?}  (override with FONO_LOG=...)");
    debug!(
        "tray icon    : {}",
        if no_tray {
            "disabled (--no-tray)"
        } else if cfg!(feature = "tray") {
            "enabled"
        } else {
            "not compiled in (rebuild with `--features tray`)"
        }
    );
    debug!(
        "hotkeys      : hold={}  toggle={}  cancel={}",
        config.hotkeys.hold, config.hotkeys.toggle, config.hotkeys.cancel
    );
    debug!(
        "stt backend  : {:?}  (local model: {})",
        config.stt.backend, config.stt.local.model
    );
    debug!(
        "llm backend  : {:?}  (enabled={})",
        config.llm.backend, config.llm.enabled
    );
    debug!(
        "inject       : also_copy_to_clipboard={}",
        config.general.also_copy_to_clipboard
    );
    // Probe and print which inject + clipboard tools are detected, so
    // users immediately see whether they have a working delivery path.
    let injector = fono_inject::Injector::detect();
    let clipboard_tool = ["wl-copy", "xclip", "xsel"]
        .iter()
        .find(|t| which_in_path(t).is_some())
        .copied()
        .unwrap_or("none");
    debug!("delivery     : key-injector={injector:?}  clipboard-tool={clipboard_tool}");
    if matches!(injector, fono_inject::Injector::None) && clipboard_tool == "none" {
        warn!(
            "NO injection backend AND no clipboard tool detected — on X11 the built-in \
             xtest-paste backend should work when DISPLAY is set; otherwise install one of: \
             wtype/ydotool (Wayland), xdotool (X11/XWayland), or rebuild with \
             --features enigo-backend; plus wl-clipboard/xclip/xsel for clipboard fallback"
        );
    }
}

fn which_in_path(tool: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for p in std::env::split_paths(&path) {
        let c = p.join(tool);
        if c.is_file() {
            return Some(c);
        }
    }
    None
}

/// One-line summary of the hardware acceleration this binary will use,
/// emitted at `info` level on every daemon start so users immediately
/// see whether their machine is running with the expected accelerator.
///
/// `whisper-rs` and `llama-cpp-2` share a single statically-linked
/// `ggml` (the duplicate-symbol collision is resolved by
/// `-Wl,--allow-multiple-definition` in `.cargo/config.toml`); whatever
/// accelerator backends are compiled into either crate are therefore
/// exercised by both engines. Today the default ship build is CPU-only
/// — the line is `CPU AVX2+FMA+F16C` on a typical x86_64 laptop. When
/// GPU accelerator features land in `fono-stt`/`fono-llm` the matching
/// `cfg(feature = …)` blocks below light up, e.g. `CUDA + CPU AVX2`.
fn hardware_acceleration_summary() -> String {
    // `mut` is required when any of the cfg(feature = "accel-*") arms
    // below are active. On the default CPU-only build none fire, hence
    // the allow.
    #[allow(unused_mut)]
    let mut accels: Vec<&'static str> = Vec::new();

    // GPU / accelerator backends — pulled in via opt-in cargo features
    // on `fono-stt` / `fono-llm`. Both crates consume the same ggml,
    // so a single compile-time feature flag enables the backend for
    // STT + LLM in lockstep. The cfg blocks here mirror the feature
    // graph and are no-ops on the default CPU-only build.
    #[cfg(feature = "accel-cuda")]
    accels.push("CUDA");
    #[cfg(feature = "accel-metal")]
    accels.push("Metal");
    #[cfg(feature = "accel-vulkan")]
    accels.push("Vulkan");
    #[cfg(feature = "accel-rocm")]
    accels.push("ROCm/HIP");
    #[cfg(feature = "accel-coreml")]
    accels.push("CoreML");
    #[cfg(feature = "accel-openblas")]
    accels.push("OpenBLAS");

    // CPU SIMD probe is runtime — even an AVX2-compiled binary has to
    // report honestly when run on an older CPU. ggml internally falls
    // back along the same path; this string just tells the user which
    // kernels will actually be picked.
    let cpu = cpu_simd_summary();
    let mut parts: Vec<String> = accels.into_iter().map(String::from).collect();
    parts.push(format!("CPU {cpu}"));
    parts.join(" + ")
}

/// Best-effort summary of the host CPU's SIMD feature set. Used by
/// [`hardware_acceleration_summary`] to tell users which kernels ggml
/// will pick on their machine.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn cpu_simd_summary() -> String {
    let mut feats: Vec<&'static str> = Vec::new();
    if std::is_x86_feature_detected!("avx512f") {
        feats.push("AVX512");
    } else if std::is_x86_feature_detected!("avx2") {
        feats.push("AVX2");
    } else if std::is_x86_feature_detected!("avx") {
        feats.push("AVX");
    } else if std::is_x86_feature_detected!("sse4.2") {
        feats.push("SSE4.2");
    } else {
        feats.push("baseline");
    }
    if std::is_x86_feature_detected!("fma") {
        feats.push("FMA");
    }
    if std::is_x86_feature_detected!("f16c") {
        feats.push("F16C");
    }
    feats.join("+")
}

#[cfg(target_arch = "aarch64")]
fn cpu_simd_summary() -> String {
    // aarch64 always has NEON; report the optional dot-product +
    // fp16 extensions when present, since ggml's arm64 kernels
    // pick them up.
    let mut feats: Vec<&'static str> = vec!["NEON"];
    #[cfg(target_feature = "dotprod")]
    feats.push("DotProd");
    #[cfg(target_feature = "fp16")]
    feats.push("FP16");
    feats.join("+")
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
fn cpu_simd_summary() -> String {
    std::env::consts::ARCH.to_string()
}

async fn handle_client(
    mut stream: UnixStream,
    fsm: Arc<Mutex<RecordingFsm>>,
    action_tx: mpsc::UnboundedSender<HotkeyAction>,
    orchestrator: Option<Arc<SessionOrchestrator>>,
    config: Arc<Config>,
    discovery_registry: Option<fono_net::discovery::Registry>,
) -> Result<()> {
    let req: Request = read_frame(&mut stream).await?;
    let orch_present = orchestrator.is_some();
    let send_translated = |a: HotkeyAction| {
        let _ = action_tx.send(translate_for_interactive(a, &config, orch_present));
    };
    let resp = match req {
        Request::Toggle => {
            send_translated(HotkeyAction::TogglePressed);
            Response::Ok
        }
        Request::HoldPress => {
            send_translated(HotkeyAction::HoldPressed);
            Response::Ok
        }
        Request::HoldRelease => {
            send_translated(HotkeyAction::HoldReleased);
            Response::Ok
        }
        Request::PasteLast => {
            if let Some(o) = orchestrator.as_ref() {
                o.on_paste_last().await;
            }
            Response::Ok
        }
        Request::Status => {
            let state = fsm.lock().await.state();
            let active = orchestrator.as_ref().map_or_else(
                || "(degraded — no orchestrator)".to_string(),
                |o| {
                    let (s, l) = o.active_backends();
                    format!("stt={s} llm={l}")
                },
            );
            Response::Text(format!("fono daemon running; fsm={state:?}; {active}"))
        }
        Request::Reload => {
            // Provider-switching plan task S11. Re-reads config + secrets
            // and atomically swaps the orchestrator's STT/LLM.
            match orchestrator.as_ref() {
                Some(o) => match o.reload().await {
                    Ok(summary) => Response::Text(summary),
                    Err(e) => Response::Error(format!("reload failed: {e:#}")),
                },
                None => Response::Error(
                    "daemon is in degraded mode (no orchestrator); cannot reload".into(),
                ),
            }
        }
        Request::Doctor => {
            Response::Text("doctor via IPC not yet available; run `fono doctor` directly".into())
        }
        Request::ListDiscovered => {
            let peers = discovery_registry
                .as_ref()
                .map(snapshot_discovered)
                .unwrap_or_default();
            Response::Discovered(peers)
        }
        Request::Shutdown => {
            std::process::exit(0);
        }
    };
    write_frame(&mut stream, &resp).await?;
    Ok(())
}

/// Pop a desktop notification with the most recent transcription's
/// raw STT output and cleaned LLM output. Used by the tray
/// "Show last transcription" entry.
fn notify_last_transcription(paths: &Paths) {
    let db = match fono_core::history::HistoryDb::open(&paths.history_db()) {
        Ok(d) => d,
        Err(e) => {
            warn!("tray: cannot open history db: {e:#}");
            return;
        }
    };
    let rows = match db.recent(1) {
        Ok(r) => r,
        Err(e) => {
            warn!("tray: history query failed: {e:#}");
            return;
        }
    };
    let body = rows.first().map_or_else(
        || "(no transcriptions yet)".to_string(),
        |t| {
            let cleaned = t.cleaned.as_deref().unwrap_or("(no LLM cleanup)");
            format!(
                "raw    : {}\ncleaned: {}\nstt={}  llm={}",
                truncate(&t.raw, 240),
                truncate(cleaned, 240),
                t.stt_backend.as_deref().unwrap_or("?"),
                t.llm_backend.as_deref().unwrap_or("none"),
            )
        },
    );
    fono_core::notify::send(
        "Fono — last transcription",
        &body,
        "audio-input-microphone",
        8_000,
        fono_core::notify::Urgency::Normal,
    );
    // Always log the transcription too so it's recoverable from logs
    // when notifications are unavailable.
    debug!("last transcription notify body:\n{body}");
}

fn open_path(path: &std::path::Path) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    match std::process::Command::new(cmd).arg(path).spawn() {
        Ok(_) => info!("opened {} via {cmd}", path.display()),
        Err(e) => warn!("failed to spawn {cmd} for {}: {e:#}", path.display()),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

fn write_pid(paths: &Paths) -> Result<()> {
    if let Some(dir) = paths.pid_file().parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(paths.pid_file(), std::process::id().to_string())?;
    Ok(())
}

/// Re-paste the i-th most recent transcription (0 = newest) via the
/// best available injection backend, falling through to the clipboard
/// when key injection isn't possible. Triggered from the tray's
/// "Recent transcriptions" submenu.
async fn paste_history_at(paths: &Paths, idx: usize) {
    let db = match fono_core::history::HistoryDb::open(&paths.history_db()) {
        Ok(d) => d,
        Err(e) => {
            warn!("paste_history: cannot open db: {e:#}");
            return;
        }
    };
    // recent() returns newest-first up to limit; ask for idx+1 so the
    // last element of the result is what we want.
    let rows = match db.recent(idx + 1) {
        Ok(r) => r,
        Err(e) => {
            warn!("paste_history: query failed: {e:#}");
            return;
        }
    };
    let Some(row) = rows.get(idx) else {
        warn!("paste_history: slot {idx} is empty");
        return;
    };
    let text = row.cleaned.clone().unwrap_or_else(|| row.raw.clone());
    if text.is_empty() {
        warn!("paste_history: slot {idx} is empty string");
        return;
    }
    // Run inject on the blocking pool so we don't trip cpal/tokio rules.
    let outcome = tokio::task::spawn_blocking(move || fono_inject::type_text_with_outcome(&text))
        .await
        .ok()
        .and_then(std::result::Result::ok);
    match outcome {
        Some(fono_inject::InjectOutcome::Typed(b)) => {
            info!("paste_history[{idx}]: typed via {b}");
        }
        Some(fono_inject::InjectOutcome::Clipboard(t)) => {
            info!("paste_history[{idx}]: copied to clipboard via {t} (paste with Ctrl+V)");
            fono_core::notify::send(
                "Fono — copied to clipboard",
                &format!("Press Ctrl+V to paste (via {t})"),
                "edit-paste",
                4_000,
                fono_core::notify::Urgency::Low,
            );
        }
        None => {
            warn!("paste_history[{idx}]: no inject backend and no clipboard tool available");
        }
    }
}

/// Switch the active STT backend from the tray submenu and trigger a
/// hot-reload of the orchestrator. Same code path as `fono use stt …`.
///
/// Special case: switching **to Local** when the configured whisper
/// model is missing kicks off an auto-download in this same task —
/// surfacing "downloading…" / "ready" / "failed" notifications — and
/// only reloads the orchestrator once the file is on disk. The reload
/// would otherwise fail with "model file not found" and leave the user
/// stuck on a broken backend.
async fn switch_stt_via_tray(
    paths: &fono_core::Paths,
    orch: Option<&Arc<crate::session::SessionOrchestrator>>,
    backends: &[fono_core::config::SttBackend],
    idx: u8,
) {
    let Some(backend) = backends.get(idx as usize) else {
        warn!("tray UseStt({idx}): out of range (max={})", backends.len());
        return;
    };
    let label = fono_core::providers::stt_backend_str(backend);
    let config_path = paths.config_file();
    let backend_clone = backend.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut cfg = fono_core::Config::load(&config_path)?;
        crate::cli::set_active_stt(&mut cfg, backend_clone);
        cfg.save(&config_path)?;
        Ok(())
    })
    .await;
    match result {
        Ok(Ok(())) => {
            info!("tray: switched STT to {label}");
            if matches!(backend, fono_core::config::SttBackend::Local)
                && !ensure_local_stt_with_notify(paths).await
            {
                return;
            }
            if let Some(o) = orch {
                if let Err(e) = o.reload().await {
                    warn!("tray: STT reload failed: {e:#}");
                    fono_core::notify::send(
                        "Fono — STT reload failed",
                        &format!("{e}"),
                        "dialog-error",
                        5_000,
                        fono_core::notify::Urgency::Critical,
                    );
                }
            }
            // Success toast removed: user just clicked the tray menu;
            // the tray label updates to reflect the change.
        }
        Ok(Err(e)) => {
            warn!("tray: STT switch failed: {e:#}");
            fono_core::notify::send(
                "Fono — STT switch failed",
                &format!("{e}"),
                "dialog-error",
                5_000,
                fono_core::notify::Urgency::Critical,
            );
        }
        Err(e) => warn!("tray: STT switch task join error: {e}"),
    }
}

fn remote_wyoming_uri(peer: &fono_net::discovery::DiscoveredPeer) -> String {
    peer.address.map_or_else(
        || {
            format!(
                "tcp://{}:{}",
                peer.hostname.trim_end_matches('.'),
                peer.port
            )
        },
        |addr| match addr {
            std::net::IpAddr::V4(v4) => format!("tcp://{v4}:{}", peer.port),
            std::net::IpAddr::V6(v6) => format!("tcp://[{v6}]:{}", peer.port),
        },
    )
}

fn local_wyoming_fullname(config: &Config) -> Option<String> {
    if !config.server.wyoming.enabled {
        return None;
    }
    let instance = if config.network.instance_name.is_empty() {
        format!("fono-{}", hostname()?)
    } else {
        config.network.instance_name.clone()
    };
    Some(format!(
        "{instance}.{}",
        fono_net::discovery::WYOMING_SERVICE_TYPE
    ))
}

fn discovered_wyoming_peers_for_tray(
    registry: Option<&fono_net::discovery::Registry>,
    local_fullname: Option<&str>,
) -> Vec<fono_net::discovery::DiscoveredPeer> {
    registry
        .map(|registry| {
            registry
                .snapshot()
                .into_iter()
                .filter(|peer| peer.kind == fono_net::discovery::PeerKind::Wyoming)
                .filter(|peer| local_fullname != Some(peer.fullname.as_str()))
                .collect()
        })
        .unwrap_or_default()
}

async fn switch_discovered_stt_via_tray(
    paths: &fono_core::Paths,
    orch: Option<&Arc<crate::session::SessionOrchestrator>>,
    registry: Option<&fono_net::discovery::Registry>,
    local_fullname: Option<&str>,
    idx: u8,
) {
    let peers = discovered_wyoming_peers_for_tray(registry, local_fullname);
    let Some(peer) = peers.get(idx as usize).cloned() else {
        warn!(
            "tray UseDiscoveredStt({idx}): out of range (max={})",
            peers.len()
        );
        return;
    };
    let label = peer.tray_label();
    let uri = remote_wyoming_uri(&peer);
    let config_path = paths.config_file();
    let model = peer.model.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut cfg = fono_core::Config::load(&config_path)?;
        let mut wyoming = cfg.stt.wyoming.unwrap_or_default();
        wyoming.uri = uri;
        if let Some(model) = model {
            wyoming.model = model;
        }
        cfg.stt.wyoming = Some(wyoming);
        cfg.stt.backend = fono_core::config::SttBackend::Wyoming;
        cfg.save(&config_path)?;
        Ok(())
    })
    .await;

    match result {
        Ok(Ok(())) => {
            info!("tray: switched STT to discovered {label}");
            if let Some(o) = orch {
                if let Err(e) = o.reload().await {
                    warn!("tray: discovered STT reload failed: {e:#}");
                    fono_core::notify::send(
                        "Fono — STT reload failed",
                        &format!("{e}"),
                        "dialog-error",
                        5_000,
                        fono_core::notify::Urgency::Critical,
                    );
                }
            }
        }
        Ok(Err(e)) => {
            warn!("tray: discovered STT switch failed: {e:#}");
            fono_core::notify::send(
                "Fono — STT switch failed",
                &format!("{e}"),
                "dialog-error",
                5_000,
                fono_core::notify::Urgency::Critical,
            );
        }
        Err(e) => warn!("tray: discovered STT switch task join error: {e}"),
    }
}

/// Switch the active LLM backend from the tray submenu and trigger a
/// hot-reload of the orchestrator. Same code path as `fono use llm …`.
async fn switch_llm_via_tray(
    paths: &fono_core::Paths,
    orch: Option<&Arc<crate::session::SessionOrchestrator>>,
    backends: &[fono_core::config::LlmBackend],
    idx: u8,
) {
    let Some(backend) = backends.get(idx as usize) else {
        warn!("tray UseLlm({idx}): out of range (max={})", backends.len());
        return;
    };
    let label = fono_core::providers::llm_backend_str(backend);
    let config_path = paths.config_file();
    let backend_clone = backend.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut cfg = fono_core::Config::load(&config_path)?;
        crate::cli::set_active_llm(&mut cfg, backend_clone);
        cfg.save(&config_path)?;
        Ok(())
    })
    .await;
    match result {
        Ok(Ok(())) => {
            info!("tray: switched LLM to {label}");
            if matches!(backend, fono_core::config::LlmBackend::Local)
                && !ensure_local_llm_with_notify(paths).await
            {
                return;
            }
            if let Some(o) = orch {
                if let Err(e) = o.reload().await {
                    warn!("tray: LLM reload failed: {e:#}");
                    fono_core::notify::send(
                        "Fono — LLM reload failed",
                        &format!("{e}"),
                        "dialog-error",
                        5_000,
                        fono_core::notify::Urgency::Critical,
                    );
                }
            }
            // Success toast removed: user just clicked the tray menu;
            // the tray label updates to reflect the change.
        }
        Ok(Err(e)) => {
            warn!("tray: LLM switch failed: {e:#}");
            fono_core::notify::send(
                "Fono — LLM switch failed",
                &format!("{e}"),
                "dialog-error",
                5_000,
                fono_core::notify::Urgency::Critical,
            );
        }
        Err(e) => warn!("tray: LLM switch task join error: {e}"),
    }
}

/// Switch the active input device from the tray "Microphone" submenu.
/// On Pulse/PipeWire hosts this calls `pactl set-default-source` so
/// the change applies system-wide and is reflected in pavucontrol /
/// GNOME / KDE settings; on cpal hosts the submenu is hidden so this
/// path is never taken. No config write — Fono no longer keeps an
/// `[audio].input_device` override.
async fn switch_input_device_via_tray(
    orch: Option<&Arc<crate::session::SessionOrchestrator>>,
    idx: u8,
) {
    use fono_audio::devices::{list_input_devices, InputBackend};
    let devices: Vec<_> = list_input_devices()
        .into_iter()
        .filter(|d| matches!(d.backend, InputBackend::Pulse { .. }))
        .collect();
    let Some(dev) = devices.get(idx as usize) else {
        warn!(
            "tray SetInputDevice({idx}): out of range (max={})",
            devices.len()
        );
        return;
    };
    let InputBackend::Pulse { pa_name } = &dev.backend else {
        // Filter above guarantees Pulse, but be defensive.
        warn!("tray SetInputDevice({idx}): not a Pulse source");
        return;
    };
    let display_name = dev.display_name.clone();
    match fono_audio::pulse::set_default_pulse_source(pa_name) {
        Ok(()) => {
            info!("tray: switched default Pulse source to {display_name} ({pa_name})");
            if let Some(o) = orch {
                if let Err(e) = o.reload().await {
                    warn!("tray: input device reload failed: {e:#}");
                    fono_core::notify::send(
                        "Fono — microphone reload failed",
                        &format!("{e}"),
                        "dialog-error",
                        5_000,
                        fono_core::notify::Urgency::Critical,
                    );
                }
            }
        }
        Err(e) => {
            warn!("tray: pactl set-default-source failed: {e:#}");
            fono_core::notify::send(
                "Fono — microphone switch failed",
                &format!("{e}"),
                "dialog-error",
                5_000,
                fono_core::notify::Urgency::Critical,
            );
        }
    }
}

/// Take a snapshot of the on-disk config fields backing the tray's
/// "Preferences" submenu. Returns a default snapshot on read failure
/// rather than propagating: the tray prefers stale-but-renderable to
/// no-menu, and a missing config file just means first-run defaults.
fn preferences_snapshot_from_disk(config_path: &std::path::Path) -> fono_tray::PreferencesSnapshot {
    let cfg = fono_core::Config::load(config_path).unwrap_or_default();
    let waveform_style = waveform_style_to_idx(cfg.overlay.style);
    fono_tray::PreferencesSnapshot {
        auto_mute_system: cfg.general.auto_mute_system,
        always_warm_mic: cfg.general.always_warm_mic,
        also_copy_to_clipboard: cfg.general.also_copy_to_clipboard,
        startup_autostart: cfg.general.startup_autostart,
        // Tray exposes VAD as a boolean. `"silero"` is the only enabled
        // backend today; treat any other non-`"off"` value as "on" so
        // future backends still light the menu correctly.
        vad_enabled: !cfg.audio.vad_backend.eq_ignore_ascii_case("off"),
        auto_stop_silence_ms: cfg.audio.auto_stop_silence_ms,
        waveform_style,
        languages: cfg.general.languages.clone(),
    }
}

/// Map a `WaveformStyle` to its index in `fono_tray::WAVEFORM_STYLES`.
fn waveform_style_to_idx(style: fono_core::config::WaveformStyle) -> u8 {
    match style {
        fono_core::config::WaveformStyle::Bars => 0,
        fono_core::config::WaveformStyle::Oscilloscope => 1,
        fono_core::config::WaveformStyle::Fft => 2,
        fono_core::config::WaveformStyle::Heatmap => 3,
    }
}

/// Inverse of `waveform_style_to_idx`. Returns `None` on out-of-range
/// indices so the caller can surface a `warn!` instead of silently
/// reverting to Bars.
fn waveform_style_from_idx(idx: u8) -> Option<fono_core::config::WaveformStyle> {
    match idx {
        0 => Some(fono_core::config::WaveformStyle::Bars),
        1 => Some(fono_core::config::WaveformStyle::Oscilloscope),
        2 => Some(fono_core::config::WaveformStyle::Fft),
        3 => Some(fono_core::config::WaveformStyle::Heatmap),
        _ => None,
    }
}

/// Map a tray `ToggleLanguage(idx)` index to the BCP-47 code in
/// `LANGUAGE_SHORTLIST`. Returns `None` on out-of-range so the
/// caller can `warn!` instead of silently no-oping.
fn language_code_from_idx(idx: u8) -> Option<&'static str> {
    fono_tray::LANGUAGE_SHORTLIST
        .get(idx as usize)
        .map(|(code, _)| *code)
}

/// Shared load → mutate → save → reload path for tray Preferences
/// toggles. Mirrors the structure of `switch_stt_via_tray` /
/// `switch_llm_via_tray` but parametrised on a closure so each new
/// preference doesn't need its own ~30-line helper. The closure
/// receives `&mut Config` and mutates one field; we re-load fresh
/// from disk inside the spawn_blocking task to avoid clobbering any
/// concurrent `fono use ...` write.
async fn apply_pref_via_tray<F>(
    paths: &fono_core::Paths,
    orch: Option<&Arc<crate::session::SessionOrchestrator>>,
    field: &'static str,
    mutate: F,
) where
    F: FnOnce(&mut fono_core::Config) + Send + 'static,
{
    let config_path = paths.config_file();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut cfg = fono_core::Config::load(&config_path)?;
        mutate(&mut cfg);
        cfg.save(&config_path)?;
        Ok(())
    })
    .await;
    match result {
        Ok(Ok(())) => {
            info!("tray: updated {field}");
            if let Some(o) = orch {
                if let Err(e) = o.reload().await {
                    warn!("tray: {field} reload failed: {e:#}");
                    fono_core::notify::send(
                        "Fono — settings reload failed",
                        &format!("{e}"),
                        "dialog-error",
                        5_000,
                        fono_core::notify::Urgency::Critical,
                    );
                }
            }
        }
        Ok(Err(e)) => {
            warn!("tray: {field} update failed: {e:#}");
            fono_core::notify::send(
                "Fono — settings update failed",
                &format!("{e}"),
                "dialog-error",
                5_000,
                fono_core::notify::Urgency::Critical,
            );
        }
        Err(e) => warn!("tray: {field} update task join error: {e}"),
    }
}

/// Spawn `fono settings` in the user's preferred terminal. Picked in
/// order: `$TERMINAL` (honoured by the tray "Open settings (TUI)…"
/// entry), then a small list of common Linux terminals. Falls back to
/// `xdg-open` on the daemon's own argv0 with a `settings` arg, which
/// most desktops will resolve via the `.desktop` file. The daemon
/// does NOT block on the spawned process — the terminal lives until
/// the user closes it.
fn open_settings_tui() {
    use std::process::Command;
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("fono"));
    let exe_str = exe.to_string_lossy().into_owned();
    let env_term = std::env::var("TERMINAL").ok();
    let candidates: Vec<&str> = env_term
        .as_deref()
        .into_iter()
        .chain([
            "xdg-terminal-exec",
            "kgx",
            "konsole",
            "alacritty",
            "kitty",
            "wezterm",
            "foot",
            "xfce4-terminal",
            "gnome-terminal",
            "xterm",
        ])
        .collect();
    for term in candidates {
        // Most terminals accept `-e <cmd> [args...]`; konsole prefers `-e`
        // with a single string. We pass argv-style and let the shell
        // re-quote — `fono` itself doesn't take spaces in argv0.
        let spawn = match term {
            "konsole" => Command::new(term)
                .args(["-e", &exe_str, "settings"])
                .spawn(),
            // gnome-terminal needs `--` to terminate its option parser
            // before forwarding.
            "gnome-terminal" => Command::new(term)
                .args(["--", &exe_str, "settings"])
                .spawn(),
            _ => Command::new(term)
                .args(["-e", &exe_str, "settings"])
                .spawn(),
        };
        if spawn.is_ok() {
            info!("tray: launched settings TUI in {term}");
            return;
        }
    }
    warn!(
        "tray: could not launch a terminal for `fono settings` — \
         set $TERMINAL or run `fono settings` manually"
    );
}

/// Ensure the local STT (whisper) model referenced by the on-disk
/// config is present, surfacing user-visible notifications around any
/// download. Returns `true` when the model is ready to load (either
/// already present or successfully downloaded), `false` on failure —
/// callers must NOT proceed to reload the orchestrator on `false`.
async fn ensure_local_stt_with_notify(paths: &fono_core::Paths) -> bool {
    let cfg = match fono_core::Config::load(&paths.config_file()) {
        Ok(c) => c,
        Err(e) => {
            warn!("ensure_local_stt: config load failed: {e:#}");
            return false;
        }
    };
    let model = cfg.stt.local.model.clone();
    let size_hint = crate::models::local_stt_size_mb(&model);
    let dest_exists = paths
        .whisper_models_dir()
        .join(format!("ggml-{model}.bin"))
        .exists();
    if !dest_exists {
        let body = size_hint.map_or_else(
            || format!("Whisper model: {model}"),
            |mb| format!("Whisper model: {model} ({mb} MB)"),
        );
        fono_core::notify::send(
            "Fono — downloading speech model",
            &body,
            "emblem-downloads",
            4_000,
            fono_core::notify::Urgency::Normal,
        );
    }
    match crate::models::ensure_local_stt(paths, &model).await {
        Ok(crate::models::EnsureOutcome::Downloaded) => {
            fono_core::notify::send(
                "Fono — speech model ready",
                &format!("{model} downloaded and cached"),
                "emblem-default",
                4_000,
                fono_core::notify::Urgency::Normal,
            );
            true
        }
        Ok(_) => true,
        Err(e) => {
            warn!("ensure_local_stt: download failed: {e:#}");
            fono_core::notify::send(
                "Fono — speech model download failed",
                &format!("{e}"),
                "dialog-error",
                6_000,
                fono_core::notify::Urgency::Critical,
            );
            false
        }
    }
}

/// LLM counterpart to [`ensure_local_stt_with_notify`]. Same contract:
/// returns `true` when the GGUF is ready to load, `false` on failure.
async fn ensure_local_llm_with_notify(paths: &fono_core::Paths) -> bool {
    let cfg = match fono_core::Config::load(&paths.config_file()) {
        Ok(c) => c,
        Err(e) => {
            warn!("ensure_local_llm: config load failed: {e:#}");
            return false;
        }
    };
    let model = cfg.llm.local.model.clone();
    let size_hint = crate::models::local_llm_size_mb(&model);
    let dest_exists = paths
        .llm_models_dir()
        .join(format!("{model}.gguf"))
        .exists();
    if !dest_exists {
        let body = size_hint.map_or_else(
            || format!("LLM model: {model}"),
            |mb| format!("LLM model: {model} ({mb} MB)"),
        );
        fono_core::notify::send(
            "Fono — downloading cleanup model",
            &body,
            "emblem-downloads",
            4_000,
            fono_core::notify::Urgency::Normal,
        );
    }
    match crate::models::ensure_local_llm(paths, &model).await {
        Ok(crate::models::EnsureOutcome::Downloaded) => {
            fono_core::notify::send(
                "Fono — cleanup model ready",
                &format!("{model} downloaded and cached"),
                "emblem-default",
                4_000,
                fono_core::notify::Urgency::Normal,
            );
            true
        }
        Ok(_) => true,
        Err(e) => {
            warn!("ensure_local_llm: download failed: {e:#}");
            fono_core::notify::send(
                "Fono — cleanup model download failed",
                &format!("{e}"),
                "dialog-error",
                6_000,
                fono_core::notify::Urgency::Critical,
            );
            false
        }
    }
}

/// Render the tray "Update to vX.Y.Z" label from the shared update
/// status; returns `None` when no upgrade is available so the menu
/// falls back to its on-demand "Check for updates…" copy.
#[allow(clippy::significant_drop_tightening)]
fn update_label(status: &Arc<RwLock<Option<fono_update::UpdateStatus>>>) -> Option<String> {
    let g = status.read().ok()?;
    let info = g.as_ref()?.available()?;
    // Suppress the generic "Update to vX" entry when the only thing
    // changing is the build variant (CPU↔GPU at the same version).
    // The dedicated `gpu_upgrade_label` provider already surfaces a
    // user-friendly "Update for GPU acceleration" entry in that case;
    // showing both would just duplicate the action.
    if info.is_variant_switch_only(env!("CARGO_PKG_VERSION")) {
        return None;
    }
    Some(format!("Update to {}", info.tag))
}

/// Tray-driven update flow. Two modes depending on `update_status`:
///
/// * If a check has already classified an update as `Available`, jump
///   straight to download + apply, then re-exec the daemon into the new
///   binary so the user immediately runs the new version.
/// * If the cache is empty or up-to-date, run an on-demand check and
///   surface the result via a desktop notification (clicking the menu
///   entry without a pending update behaves like `fono update --check`).
async fn apply_update_via_tray(update_status: Arc<RwLock<Option<fono_update::UpdateStatus>>>) {
    let cached = update_status.read().ok().and_then(|g| g.clone());
    let info_opt = cached
        .as_ref()
        .and_then(fono_update::UpdateStatus::available)
        .cloned();
    let info = if let Some(i) = info_opt {
        i
    } else {
        // No pending update — run an on-demand check and notify.
        let st = fono_update::check(
            env!("CARGO_PKG_VERSION"),
            crate::variant::VARIANT.release_asset_prefix(),
            fono_update::Channel::Stable,
        )
        .await;
        if let Ok(mut g) = update_status.write() {
            *g = Some(st.clone());
        }
        match st {
            fono_update::UpdateStatus::UpToDate { current } => {
                fono_core::notify::send(
                    "Fono — up to date",
                    &format!("Running v{current}; no newer release available."),
                    "emblem-default",
                    4_000,
                    fono_core::notify::Urgency::Low,
                );
                return;
            }
            fono_update::UpdateStatus::CheckFailed { error, .. } => {
                warn!("tray update check failed: {error}");
                fono_core::notify::send(
                    "Fono — update check failed",
                    &error,
                    "dialog-error",
                    5_000,
                    fono_core::notify::Urgency::Critical,
                );
                return;
            }
            fono_update::UpdateStatus::Available { info, .. } => info,
        }
    };

    fono_core::notify::send(
        "Fono — downloading update",
        &format!(
            "Fetching {} ({} MB)…",
            info.asset_name,
            info.asset_size / 1_048_576
        ),
        "emblem-downloads",
        4_000,
        fono_core::notify::Urgency::Normal,
    );

    match fono_update::apply_update(&info, fono_update::ApplyOpts::default()).await {
        Ok(out) => {
            info!(
                "tray: installed update at {} ({} bytes, sha={})",
                out.installed_at.display(),
                out.bytes,
                out.sha256
            );
            fono_core::notify::send(
                "Fono — update installed",
                &format!("Restarting into {} …", info.tag),
                "emblem-default",
                3_000,
                fono_core::notify::Urgency::Normal,
            );
            // Give the notification daemon a beat to render before we
            // replace the process image.
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            // `restart_in_place`'s Ok variant is `Infallible`, so on
            // success it never returns; we only land here when execv
            // failed to replace the process image. Pass `out.installed_at`
            // explicitly: `current_exe()` resolves to the pre-rename
            // inode (now at `<target>.bak`) and exec'ing that re-runs
            // the OLD binary, so the update silently fails to take
            // effect — the user has to manually restart for the new
            // binary to load.
            let Err(e) = fono_update::restart_in_place(&out.installed_at);
            warn!("tray: re-exec failed: {e:#}");
            fono_core::notify::send(
                "Fono — restart failed",
                &format!("Update installed; please restart Fono manually.\n{e}"),
                "dialog-warning",
                8_000,
                fono_core::notify::Urgency::Critical,
            );
        }
        Err(e) => {
            warn!("tray: apply_update failed: {e:#}");
            fono_core::notify::send(
                "Fono — update failed",
                &format!("{e}"),
                "dialog-error",
                8_000,
                fono_core::notify::Urgency::Critical,
            );
        }
    }
}

/// Spawn the LAN Wyoming server if `[server.wyoming].enabled = true`
/// and the orchestrator is alive. Returns `None` when the server is
/// disabled, the orchestrator is in degraded mode, or the listener
/// fails to bind (failures are logged at `warn!` and never abort the
/// daemon — dictation must keep working even if the LAN server can't
/// come up). Slice 3 of
/// `plans/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`.
async fn spawn_wyoming_server_if_enabled(
    config: &Config,
    orchestrator: Option<&Arc<SessionOrchestrator>>,
) -> Option<fono_net::wyoming::server::WyomingServerHandle> {
    let cfg = &config.server.wyoming;
    if !cfg.enabled {
        return None;
    }
    let Some(orch) = orchestrator else {
        warn!(
            "[server.wyoming].enabled = true but the daemon is in degraded mode \
             (no STT backend); skipping Wyoming server"
        );
        return None;
    };

    let loopback_only = cfg.bind == "127.0.0.1" || cfg.bind == "::1";
    let auth_token = if cfg.auth_token_ref.is_empty() {
        None
    } else {
        std::env::var(&cfg.auth_token_ref).ok()
    };
    let model = config.stt.local.model.clone();
    let server_cfg = fono_net::wyoming::server::WyomingServerConfig {
        bind: cfg.bind.clone(),
        port: cfg.port,
        auth_token,
        server_name: "Fono".to_string(),
        server_version: env!("CARGO_PKG_VERSION").to_string(),
        models: vec![fono_net::wyoming::server::AdvertisedModel {
            name: model,
            languages: config.general.languages.clone(),
            description: Some(format!(
                "fono daemon backend: {}",
                fono_core::providers::stt_backend_str(&config.stt.backend)
            )),
            version: None,
        }],
        loopback_only,
    };

    let orch_for_provider = Arc::clone(orch);
    let provider: fono_net::wyoming::server::SttProvider =
        Arc::new(move || orch_for_provider.stt_snapshot());
    let server = fono_net::wyoming::server::WyomingServer::new(server_cfg, provider);
    match server.start().await {
        Ok(handle) => {
            info!(
                "Wyoming server listening on {} (loopback_only={})",
                handle.local_addr(),
                loopback_only
            );
            Some(handle)
        }
        Err(e) => {
            warn!("Wyoming server failed to start: {e:#}");
            None
        }
    }
}

/// Live discovery runtime: shared registry, browser, and (optional)
/// advertisers. Held by the daemon for its lifetime so the goodbye
/// packets fire when it exits.
struct DiscoveryRuntime {
    registry: fono_net::discovery::Registry,
    _browser: Option<fono_net::discovery::browser::BrowserHandle>,
    _wyoming_advert: Option<fono_net::discovery::advertiser::AdvertiserHandle>,
}

/// Spawn the always-on mDNS browser and the matching advertisers for
/// any enabled `[server.*]` blocks. All failure paths log and continue —
/// discovery is a convenience layer, not a hard dependency. Slice 4 of
/// the network plan.
async fn spawn_discovery_if_enabled(config: &Config) -> Option<DiscoveryRuntime> {
    let server = &config.server;

    let daemon = match mdns_sd::ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            warn!("mdns: ServiceDaemon::new() failed; LAN discovery disabled: {e:#}");
            return None;
        }
    };

    let registry = fono_net::discovery::Registry::new();
    let browser = {
        let b = fono_net::discovery::Browser::new(daemon.clone(), registry.clone());
        match b.start(&[
            fono_net::discovery::PeerKind::Wyoming,
            fono_net::discovery::PeerKind::Fono,
        ]) {
            Ok(handle) => {
                info!("mDNS browser started");
                Some(handle)
            }
            Err(e) => {
                warn!("mdns browser failed to start: {e:#}");
                None
            }
        }
    };

    let wyoming_advert = if server.wyoming.enabled {
        spawn_wyoming_advert(&daemon, config)
    } else {
        None
    };

    // Seed the local Wyoming service directly into the registry so that
    // `fono discover` shows it immediately without waiting for the
    // mDNS probing phase + multicast-loopback round-trip. The browser
    // will keep the entry fresh via loopback announcements once the
    // probing phase completes (~750 ms); the initial upsert ensures the
    // peer is visible even if the first `fono discover` call races the
    // probe.
    if let Some((ref handle, ref short_host)) = wyoming_advert {
        let peer = local_wyoming_peer(config, short_host, handle.fullname());
        registry.upsert(peer);
        debug!(
            target: "fono::discovery",
            fullname = %handle.fullname(),
            "seeded local wyoming peer into registry"
        );
    }

    Some(DiscoveryRuntime {
        registry,
        _browser: browser,
        _wyoming_advert: wyoming_advert.map(|(h, _)| h),
    })
}

fn spawn_wyoming_advert(
    daemon: &mdns_sd::ServiceDaemon,
    config: &Config,
) -> Option<(fono_net::discovery::advertiser::AdvertiserHandle, String)> {
    let cfg = &config.server.wyoming;
    let Some(host) = hostname() else {
        warn!("mdns: cannot determine hostname; skipping advertise");
        return None;
    };
    let instance = if config.network.instance_name.is_empty() {
        format!("fono-{host}")
    } else {
        config.network.instance_name.clone()
    };
    // mDNS hostnames must be in the `<name>.local.` form (RFC 6762 §19).
    // `hostname()` returns the bare short name (e.g. "nimblex"); append
    // ".local" so `ensure_trailing_dot` in the advertiser produces the
    // correct "nimblex.local." FQDN. Without this the SRV record's host
    // field is "nimblex." which mdns-sd browsers on other hosts cannot
    // resolve via mDNS.
    let mdns_host = if host.contains('.') {
        host.clone() // already qualified (e.g. "kitchen.local")
    } else {
        format!("{host}.local")
    };
    let spec = fono_net::discovery::advertiser::AdvertiseSpec {
        kind: fono_net::discovery::PeerKind::Wyoming,
        instance_name: instance,
        hostname: mdns_host,
        port: cfg.port,
        addresses: vec![],
        proto: "wyoming/1".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        caps: vec!["stt".into()],
        model: Some(config.stt.local.model.clone()),
        auth_required: !cfg.auth_token_ref.is_empty(),
        path: None,
    };
    let advertiser = fono_net::discovery::Advertiser::new(daemon.clone());
    match advertiser.register(spec) {
        Ok(h) => {
            info!(
                "mDNS advertising _wyoming._tcp on port {} as {}",
                cfg.port,
                h.fullname()
            );
            Some((h, host))
        }
        Err(e) => {
            warn!("mdns wyoming advertise failed: {e:#}");
            None
        }
    }
}

/// Build a [`fono_net::discovery::DiscoveredPeer`] representing the
/// locally-running Wyoming service. Used to seed the registry at startup
/// so that `fono discover` shows the local peer without waiting for the
/// mDNS probing phase + multicast-loopback round-trip.
fn local_wyoming_peer(
    config: &Config,
    short_host: &str,
    fullname: &str,
) -> fono_net::discovery::DiscoveredPeer {
    use fono_net::discovery::{DiscoveredPeer, PeerKind, WYOMING_SERVICE_TYPE};
    use std::time::Instant;
    let hostname = format!("{short_host}.local.");
    // Strip the service-type suffix to get the friendly instance name.
    let name = fullname
        .strip_suffix(WYOMING_SERVICE_TYPE)
        .and_then(|s| s.strip_suffix('.'))
        .unwrap_or(fullname)
        .to_string();
    DiscoveredPeer {
        kind: PeerKind::Wyoming,
        fullname: fullname.to_string(),
        name,
        hostname,
        address: None,
        port: config.server.wyoming.port,
        proto: "wyoming/1".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        caps: vec!["stt".into()],
        model: Some(config.stt.local.model.clone()),
        auth_required: !config.server.wyoming.auth_token_ref.is_empty(),
        path: None,
        last_seen: Instant::now(),
    }
}

fn hostname() -> Option<String> {
    // Best-effort. `gethostname` would pull a new dep; the env / file
    // fallbacks cover every Linux + macOS we care about.
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.is_empty() {
            return Some(h);
        }
    }
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Convert a `fono_net::discovery::Registry` snapshot into the
/// IPC-friendly representation. Slice 4.
fn snapshot_discovered(registry: &fono_net::discovery::Registry) -> Vec<fono_ipc::DiscoveredPeer> {
    let now = std::time::Instant::now();
    registry
        .snapshot()
        .into_iter()
        .map(|p| fono_ipc::DiscoveredPeer {
            kind: match p.kind {
                fono_net::discovery::PeerKind::Wyoming => "wyoming".into(),
                fono_net::discovery::PeerKind::Fono => "fono".into(),
            },
            fullname: p.fullname,
            name: p.name,
            hostname: p.hostname.trim_end_matches('.').to_string(),
            address: p.address.map(|a| a.to_string()),
            port: p.port,
            proto: p.proto,
            version: p.version,
            caps: p.caps,
            model: p.model,
            auth_required: p.auth_required,
            path: p.path,
            age_secs: now.saturating_duration_since(p.last_seen).as_secs(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Slim build: translation never fires regardless of config.
    #[cfg(not(feature = "interactive"))]
    #[test]
    fn translate_passthrough_when_feature_off() {
        let mut cfg = Config::default();
        cfg.interactive.enabled = true;
        assert_eq!(
            translate_for_interactive(HotkeyAction::HoldPressed, &cfg, true),
            HotkeyAction::HoldPressed
        );
        assert_eq!(
            translate_for_interactive(HotkeyAction::TogglePressed, &cfg, true),
            HotkeyAction::TogglePressed
        );
    }

    /// `interactive` feature on, `[interactive].enabled = true`, and an
    /// orchestrator is present → Hold/Toggle become their `Live*`
    /// variants. Cancel and processing actions pass through.
    #[cfg(feature = "interactive")]
    #[test]
    fn translate_hold_toggle_to_live_when_enabled() {
        let mut cfg = Config::default();
        cfg.interactive.enabled = true;
        assert_eq!(
            translate_for_interactive(HotkeyAction::HoldPressed, &cfg, true),
            HotkeyAction::LiveHoldPressed
        );
        assert_eq!(
            translate_for_interactive(HotkeyAction::HoldReleased, &cfg, true),
            HotkeyAction::LiveHoldReleased
        );
        assert_eq!(
            translate_for_interactive(HotkeyAction::TogglePressed, &cfg, true),
            HotkeyAction::LiveTogglePressed
        );
        // Cancel / Processing variants always pass through.
        assert_eq!(
            translate_for_interactive(HotkeyAction::CancelPressed, &cfg, true),
            HotkeyAction::CancelPressed
        );
        assert_eq!(
            translate_for_interactive(HotkeyAction::ProcessingDone, &cfg, true),
            HotkeyAction::ProcessingDone
        );
        assert_eq!(
            translate_for_interactive(HotkeyAction::ProcessingStarted, &cfg, true),
            HotkeyAction::ProcessingStarted
        );
    }

    /// `enabled = false` → no translation even with the feature on.
    #[cfg(feature = "interactive")]
    #[test]
    fn translate_passthrough_when_disabled() {
        let cfg = Config::default(); // interactive.enabled defaults to false
        assert_eq!(
            translate_for_interactive(HotkeyAction::HoldPressed, &cfg, true),
            HotkeyAction::HoldPressed
        );
    }

    /// Degraded mode (no orchestrator) → translation is suppressed
    /// because the FSM's `LiveDictating` state has no driver.
    #[cfg(feature = "interactive")]
    #[test]
    fn translate_passthrough_in_degraded_mode() {
        let mut cfg = Config::default();
        cfg.interactive.enabled = true;
        assert_eq!(
            translate_for_interactive(HotkeyAction::HoldPressed, &cfg, false),
            HotkeyAction::HoldPressed
        );
    }

    /// `local_wyoming_peer` produces a properly-formed `DiscoveredPeer`:
    /// hostname gets a `.local.` suffix, fullname is preserved, friendly
    /// name strips the service-type tail, and port/model come from config.
    #[test]
    fn local_wyoming_peer_fields() {
        let mut cfg = Config::default();
        cfg.server.wyoming.port = 10300;
        cfg.stt.local.model = "small".into();
        let fullname = "fono-nimblex._wyoming._tcp.local.";
        let peer = local_wyoming_peer(&cfg, "nimblex", fullname);
        assert_eq!(peer.hostname, "nimblex.local.");
        assert_eq!(peer.name, "fono-nimblex");
        assert_eq!(peer.fullname, fullname);
        assert_eq!(peer.port, 10300);
        assert_eq!(peer.model.as_deref(), Some("small"));
        assert_eq!(peer.proto, "wyoming/1");
        assert!(!peer.auth_required);
    }

    #[test]
    fn tray_wyoming_peers_filter_local_fullname() {
        use fono_net::discovery::{DiscoveredPeer, PeerKind, Registry};
        use std::time::Instant;

        fn peer(fullname: &str, host: &str) -> DiscoveredPeer {
            DiscoveredPeer {
                kind: PeerKind::Wyoming,
                fullname: fullname.into(),
                name: fullname
                    .strip_suffix(fono_net::discovery::WYOMING_SERVICE_TYPE)
                    .and_then(|s| s.strip_suffix('.'))
                    .unwrap_or(fullname)
                    .into(),
                hostname: format!("{host}.local."),
                address: None,
                port: 10300,
                proto: "wyoming/1".into(),
                version: "test".into(),
                caps: vec!["stt".into()],
                model: Some("small".into()),
                auth_required: false,
                path: None,
                last_seen: Instant::now(),
            }
        }

        let registry = Registry::new();
        registry.upsert(peer("fono-nimblex._wyoming._tcp.local.", "nimblex"));
        registry.upsert(peer("fono-ai._wyoming._tcp.local.", "ai"));

        let peers = discovered_wyoming_peers_for_tray(
            Some(&registry),
            Some("fono-nimblex._wyoming._tcp.local."),
        );

        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].fullname, "fono-ai._wyoming._tcp.local.");
    }

    /// A bare short hostname (e.g. "kitchen") gets ".local" appended so
    /// that the mDNS SRV record carries a valid `<name>.local.` FQDN.
    /// An already-qualified name (e.g. "kitchen.local") is left alone.
    #[test]
    fn mdns_host_qualification() {
        // bare → qualified
        let bare = "kitchen";
        let qualified = if bare.contains('.') {
            bare.to_string()
        } else {
            format!("{bare}.local")
        };
        assert_eq!(qualified, "kitchen.local");

        // already qualified → unchanged
        let already = "kitchen.local";
        let still_qualified = if already.contains('.') {
            already.to_string()
        } else {
            format!("{already}.local")
        };
        assert_eq!(still_qualified, "kitchen.local");
    }
}
