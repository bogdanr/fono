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

#[allow(clippy::too_many_lines)]
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
    // Global hotkey listener
    // ---------------------------------------------------------------
    let bindings = HotkeyBindings {
        hold: config.hotkeys.hold.clone(),
        toggle: config.hotkeys.toggle.clone(),
        cancel: config.hotkeys.cancel.clone(),
    };
    let cancel_ctrl: Option<HotkeyControlSender> =
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
        if let Some(cached) = fono_update::load_cache(&cache_path) {
            *update_status.write().expect("update_status lock") = Some(cached.status);
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
                let st = fono_update::check(current, channel).await;
                match &st {
                    fono_update::UpdateStatus::Available { info: rel, .. } => {
                        info!(
                            "update check: new version available {current} -> {} ({})",
                            rel.version, rel.html_url
                        );
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
    // Tray icon (feature-gated; no-op if the backend is compiled out)
    // ---------------------------------------------------------------
    let (tray, mut tray_rx) = if no_tray {
        debug!("tray disabled (--no-tray)");
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

        let (t, rx) = fono_tray::spawn(
            "Fono — voice dictation",
            recent_provider,
            stt_labels,
            llm_labels,
            active_provider,
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
                // Languages provider for the "Languages" submenu (plan
                // v3 task 8). Polled every ~2 s; reflects whatever is
                // currently in `general.languages` after `Reload`.
                let config_path = paths.config_file();
                Arc::new(move || {
                    fono_core::Config::load(&config_path)
                        .map(|c| c.general.languages)
                        .unwrap_or_default()
                }) as fono_tray::LanguagesProvider
            },
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
                    TrayAction::UseLlm(idx) => {
                        switch_llm_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            &llm_backends_for_dispatch,
                            idx,
                        )
                        .await;
                    }
                    TrayAction::ApplyUpdate => {
                        apply_update_via_tray(Arc::clone(&update_status_tray)).await;
                    }
                    TrayAction::ClearLanguageMemory => {
                        fono_stt::LanguageCache::global().clear();
                        info!("language memory cleared via tray");
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
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, fsm, action_tx, orch, config).await {
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
        } else {
            info!("interactive  : not compiled in (rebuild with `--features interactive`)");
        }
    }
    info!("hw accel     : {}", hardware_acceleration_summary());
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
        let st = fono_update::check(env!("CARGO_PKG_VERSION"), fono_update::Channel::Stable).await;
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
            // failed to replace the process image.
            let Err(e) = fono_update::restart_in_place();
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
}
