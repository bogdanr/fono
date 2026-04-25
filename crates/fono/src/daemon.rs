// SPDX-License-Identifier: GPL-3.0-only
//! Daemon event loop: startup banner, global-hotkey listener, tray icon,
//! IPC server, FSM dispatcher, and the [`crate::session::SessionOrchestrator`]
//! that drives audio → STT → LLM → inject end to end.

use anyhow::{Context, Result};
use fono_core::{Config, Paths, Secrets};
use fono_hotkey::{HotkeyAction, HotkeyBindings, HotkeyEvent, RecordingFsm};
use fono_ipc::{read_frame, write_frame, Request, Response};
use fono_tray::{TrayAction, TrayState};
use std::sync::Arc;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};

use crate::cli::Verbosity;
use crate::session::SessionOrchestrator;

#[allow(clippy::too_many_lines)]
pub async fn run(paths: &Paths, no_tray: bool, verbosity: Verbosity) -> Result<()> {
    let config = Arc::new(Config::load(&paths.config_file()).context("load config")?);
    let secrets = Secrets::load(&paths.secrets_file()).context("load secrets")?;
    print_banner(paths, &config, no_tray, verbosity);
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
        paste_last: config.hotkeys.paste_last.clone(),
        cancel: config.hotkeys.cancel.clone(),
    };
    match fono_hotkey::spawn_listener(bindings, action_tx.clone()) {
        Ok(_handle) => info!("global hotkeys registered"),
        Err(e) => warn!(
            "global hotkeys unavailable: {e:#}\n  \
             (the daemon will still accept `fono toggle` via IPC)"
        ),
    }

    // ---------------------------------------------------------------
    // Tray icon (feature-gated; no-op if the backend is compiled out)
    // ---------------------------------------------------------------
    let (tray, mut tray_rx) = if no_tray {
        info!("tray disabled (--no-tray)");
        let (_tx, rx) = mpsc::unbounded_channel::<TrayAction>();
        (None, rx)
    } else {
        let (t, rx) = fono_tray::spawn("Fono — voice dictation");
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
        tokio::spawn(async move {
            while let Some(e) = fsm_events.recv().await {
                info!("fsm event: {e:?}");
                if let Some(t) = tray.as_ref().as_ref() {
                    match e {
                        HotkeyEvent::StartRecording(_) => t.set_state(TrayState::Recording),
                        HotkeyEvent::StopRecording => t.set_state(TrayState::Processing),
                        HotkeyEvent::Cancel => t.set_state(TrayState::Idle),
                        HotkeyEvent::PasteLast => { /* no state change */ }
                    }
                }
                let Some(o) = orch.as_ref() else {
                    // Degraded mode: just emit ProcessingDone so the FSM
                    // returns to Idle without us hanging.
                    if matches!(e, HotkeyEvent::StopRecording | HotkeyEvent::Cancel) {
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
                            // Pipeline emits ProcessingDone when it
                            // finishes; we just clear the tray here once
                            // FSM returns to Idle. (The action dispatcher
                            // below sees ProcessingDone arrive and
                            // transitions; tray state is updated by the
                            // FSM event loop via Cancel/Idle paths or
                            // we explicitly tint to Idle once Done.)
                            let _ = tray_for_done; // borrowed; future overlay tie-in
                        });
                    }
                    HotkeyEvent::Cancel => {
                        let o = Arc::clone(o);
                        tokio::spawn(async move {
                            o.on_cancel().await;
                        });
                    }
                    HotkeyEvent::PasteLast => {
                        let o = Arc::clone(o);
                        tokio::spawn(async move {
                            o.on_paste_last().await;
                        });
                    }
                }
            }
        });
    }

    // ---------------------------------------------------------------
    // Action dispatcher — drains HotkeyAction into the FSM. Also flips
    // the tray to Idle when the pipeline reports ProcessingDone.
    // ---------------------------------------------------------------
    {
        let fsm = Arc::clone(&fsm);
        let tray = Arc::clone(&tray);
        tokio::spawn(async move {
            while let Some(action) = action_rx.recv().await {
                let new_state = fsm.lock().await.dispatch(action);
                tracing::debug!("dispatch {action:?} -> {new_state:?}");
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
        tokio::spawn(async move {
            while let Some(ta) = tray_rx.recv().await {
                info!("tray action: {ta:?}");
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
                    TrayAction::OpenHistory => open_history(&paths),
                    TrayAction::OpenConfig => open_path(&paths.config_file()),
                    TrayAction::Pause => {
                        info!("tray: Pause hotkeys (not yet implemented)");
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
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, fsm, action_tx).await {
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

fn print_banner(paths: &Paths, config: &Config, no_tray: bool, verbosity: Verbosity) {
    let config_path = paths.config_file();
    let config_present = config_path.exists();
    info!("Fono v{} starting", env!("CARGO_PKG_VERSION"));
    info!(
        "config       : {} ({})",
        config_path.display(),
        if config_present {
            "loaded"
        } else {
            "absent — using defaults"
        }
    );
    info!("secrets      : {}", paths.secrets_file().display());
    info!("history db   : {}", paths.history_db().display());
    info!("models/whisper: {}", paths.whisper_models_dir().display());
    info!("models/llm   : {}", paths.llm_models_dir().display());
    info!("cache        : {}", paths.cache_dir.display());
    info!("state        : {}", paths.state_dir.display());
    info!("ipc socket   : {}", paths.ipc_socket().display());
    info!("log level    : {verbosity:?}  (override with FONO_LOG=...)");
    info!(
        "tray icon    : {}",
        if no_tray {
            "disabled (--no-tray)"
        } else if cfg!(feature = "tray") {
            "enabled"
        } else {
            "not compiled in (rebuild with `--features tray`)"
        }
    );
    info!(
        "hotkeys      : hold={}  toggle={}  paste_last={}  cancel={}",
        config.hotkeys.hold,
        config.hotkeys.toggle,
        config.hotkeys.paste_last,
        config.hotkeys.cancel
    );
    info!(
        "stt backend  : {:?}  (local model: {})",
        config.stt.backend, config.stt.local.model
    );
    info!(
        "llm backend  : {:?}  (enabled={})",
        config.llm.backend, config.llm.enabled
    );
    tracing::debug!("to see per-crate debug output: re-run with `-v` (debug) or `-vv` (trace)");
}

async fn handle_client(
    mut stream: UnixStream,
    fsm: Arc<Mutex<RecordingFsm>>,
    action_tx: mpsc::UnboundedSender<HotkeyAction>,
) -> Result<()> {
    let req: Request = read_frame(&mut stream).await?;
    let resp = match req {
        Request::Toggle => {
            let _ = action_tx.send(HotkeyAction::TogglePressed);
            Response::Ok
        }
        Request::HoldPress => {
            let _ = action_tx.send(HotkeyAction::HoldPressed);
            Response::Ok
        }
        Request::HoldRelease => {
            let _ = action_tx.send(HotkeyAction::HoldReleased);
            Response::Ok
        }
        Request::PasteLast => {
            let _ = action_tx.send(HotkeyAction::PasteLastPressed);
            Response::Ok
        }
        Request::Status => {
            let state = fsm.lock().await.state();
            Response::Text(format!("fono daemon running; fsm={state:?}"))
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
    if let Err(e) = notify_rust::Notification::new()
        .summary("Fono — last transcription")
        .body(&body)
        .icon("audio-input-microphone")
        .timeout(notify_rust::Timeout::Milliseconds(8_000))
        .show()
    {
        // Fall back to logging when no notification daemon is running.
        warn!("notify failed ({e:#}); last transcription:\n{body}");
    }
}

fn open_history(paths: &Paths) {
    // SQLite isn't directly browsable; open the parent dir so the user
    // can see the DB + any rolling exports we add later. A future
    // refinement may render an HTML view on demand.
    let dir = paths
        .history_db()
        .parent()
        .unwrap_or(&paths.state_dir)
        .to_path_buf();
    open_path(&dir);
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
