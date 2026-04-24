// SPDX-License-Identifier: GPL-3.0-only
//! Minimal daemon event loop: serves IPC, tracks FSM state.
//!
//! Phase 8 deliverable: the daemon compiles, binds its socket, and responds
//! to CLI round-trips. End-to-end audio → STT → LLM → inject wiring is
//! sequenced into follow-up phases per the design plan.

use anyhow::{Context, Result};
use fono_core::Paths;
use fono_hotkey::{HotkeyAction, RecordingFsm};
use fono_ipc::{read_frame, write_frame, Request, Response};
use tokio::net::UnixStream;
use tracing::{info, warn};

pub async fn run(paths: &Paths, no_tray: bool) -> Result<()> {
    info!(
        "starting fono daemon (no_tray={no_tray}, socket={})",
        paths.ipc_socket().display()
    );
    write_pid(paths)?;

    // Spin up the FSM. The hotkey thread + audio thread will push actions
    // into this receiver in follow-up wiring; today the daemon just serves
    // IPC so `fono toggle` from a compositor keybind works.
    let (fsm, mut events) = RecordingFsm::new();
    let fsm = std::sync::Arc::new(std::sync::Mutex::new(fsm));

    // Drain events (no orchestrator consumer yet).
    tokio::spawn(async move {
        while let Some(e) = events.recv().await {
            info!("fsm event: {e:?}");
        }
    });

    let listener = fono_ipc::bind_listener(&paths.ipc_socket()).context("bind IPC socket")?;

    info!("daemon ready; press Ctrl+C to stop");
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("ctrl-c received; shutting down");
                break;
            }
            accept = listener.accept() => {
                let (stream, _) = accept?;
                let fsm = std::sync::Arc::clone(&fsm);
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, fsm).await {
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

async fn handle_client(
    mut stream: UnixStream,
    fsm: std::sync::Arc<std::sync::Mutex<RecordingFsm>>,
) -> Result<()> {
    let req: Request = read_frame(&mut stream).await?;
    let resp = match req {
        Request::Toggle => {
            if let Ok(mut f) = fsm.lock() {
                f.dispatch(HotkeyAction::TogglePressed);
            }
            Response::Ok
        }
        Request::HoldPress => {
            if let Ok(mut f) = fsm.lock() {
                f.dispatch(HotkeyAction::HoldPressed);
            }
            Response::Ok
        }
        Request::HoldRelease => {
            if let Ok(mut f) = fsm.lock() {
                f.dispatch(HotkeyAction::HoldReleased);
            }
            Response::Ok
        }
        Request::PasteLast => {
            if let Ok(mut f) = fsm.lock() {
                f.dispatch(HotkeyAction::PasteLastPressed);
            }
            Response::Ok
        }
        Request::Status => {
            let state = fsm.lock().map(|f| f.state()).ok();
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
