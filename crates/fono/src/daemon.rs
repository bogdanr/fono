// SPDX-License-Identifier: GPL-3.0-only
//! Daemon event loop: startup banner, global-hotkey listener, tray icon,
//! IPC server, FSM dispatcher.
//!
//! End-to-end audio → STT → LLM → inject wiring is sequenced into
//! follow-up phases; this module owns the long-lived background workers
//! (hotkeys, tray, IPC) and forwards their events into the FSM.

use anyhow::{Context, Result};
use fono_core::{Config, Paths};
use fono_hotkey::{HotkeyAction, HotkeyBindings, HotkeyEvent, RecordingFsm};
use fono_ipc::{read_frame, write_frame, Request, Response};
use fono_tray::{TrayAction, TrayState};
use std::sync::Arc;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};

use crate::cli::Verbosity;

#[allow(clippy::too_many_lines)]
pub async fn run(paths: &Paths, no_tray: bool, verbosity: Verbosity) -> Result<()> {
    let config = Config::load(&paths.config_file()).context("load config")?;
    print_banner(paths, &config, no_tray, verbosity);
    write_pid(paths)?;

    // Ensure referenced models are on disk before we register hotkeys —
    // this is a no-op if they already exist, and only warns (doesn't
    // abort) if the network is unavailable so the daemon still comes up.
    if let Err(e) = crate::models::ensure_models(paths, &config).await {
        warn!("model preflight failed: {e:#}");
    }

    // ---------------------------------------------------------------
    // FSM + channels
    // ---------------------------------------------------------------
    let (fsm, mut fsm_events) = RecordingFsm::new();
    let fsm = Arc::new(Mutex::new(fsm));

    // Actions from hotkeys / tray / IPC converge on a single channel.
    let (action_tx, mut action_rx) = mpsc::unbounded_channel::<HotkeyAction>();

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

    // ---------------------------------------------------------------
    // FSM event consumer — logs events, updates tray tint, and (for now)
    // synthesises a `ProcessingDone` shortly after every `StopRecording`
    // so the FSM returns to `Idle`. Once the real STT→LLM→inject
    // pipeline is wired in, that pipeline will emit `ProcessingDone`
    // instead and this shim can go away.
    // ---------------------------------------------------------------
    let tray_for_events = Arc::new(tray);
    {
        let tray = Arc::clone(&tray_for_events);
        let action_tx_for_fsm = action_tx.clone();
        tokio::spawn(async move {
            while let Some(e) = fsm_events.recv().await {
                info!("fsm event: {e:?}");
                if let Some(t) = tray.as_ref() {
                    match e {
                        HotkeyEvent::StartRecording(_) => t.set_state(TrayState::Recording),
                        HotkeyEvent::StopRecording => t.set_state(TrayState::Processing),
                        HotkeyEvent::Cancel => t.set_state(TrayState::Idle),
                        HotkeyEvent::PasteLast => { /* no state change */ }
                    }
                }
                if matches!(e, HotkeyEvent::StopRecording | HotkeyEvent::Cancel) {
                    // Placeholder pipeline: pretend we finished processing
                    // after a beat so the FSM (and the tray tint) return
                    // to Idle and the next toggle press works.
                    let tx = action_tx_for_fsm.clone();
                    let tray = Arc::clone(&tray);
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                        let _ = tx.send(HotkeyAction::ProcessingDone);
                        if let Some(t) = tray.as_ref() {
                            t.set_state(TrayState::Idle);
                        }
                    });
                }
            }
        });
    }

    // ---------------------------------------------------------------
    // Action dispatcher — drains HotkeyAction into the FSM.
    // ---------------------------------------------------------------
    {
        let fsm = Arc::clone(&fsm);
        tokio::spawn(async move {
            while let Some(action) = action_rx.recv().await {
                let new_state = fsm.lock().await.dispatch(action);
                tracing::debug!("dispatch {action:?} -> {new_state:?}");
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
                    _ => { /* history/config/status wiring deferred */ }
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
    // Emit the banner through tracing so it respects FONO_LOG filters but
    // also lands in the log file. `info!` is used so users see it by
    // default; `--debug` surfaces even more detail below.
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

fn write_pid(paths: &Paths) -> Result<()> {
    if let Some(dir) = paths.pid_file().parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(paths.pid_file(), std::process::id().to_string())?;
    Ok(())
}
