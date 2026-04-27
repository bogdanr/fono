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
use std::sync::Arc;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

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
                        HotkeyEvent::StartRecording(_) => {
                            let _ = ctrl.send(HotkeyControl::EnableCancel);
                        }
                        HotkeyEvent::StopRecording | HotkeyEvent::Cancel => {
                            let _ = ctrl.send(HotkeyControl::DisableCancel);
                        }
                    }
                }
                if let Some(t) = tray.as_ref().as_ref() {
                    match e {
                        HotkeyEvent::StartRecording(_) => t.set_state(TrayState::Recording),
                        HotkeyEvent::StopRecording => t.set_state(TrayState::Processing),
                        HotkeyEvent::Cancel => t.set_state(TrayState::Idle),
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
        let orch_for_tray = orchestrator.clone();
        // Snapshot the filtered backend lists for the tray dispatcher
        // so `UseStt(idx)` / `UseLlm(idx)` resolve to the same item the
        // user clicked (the indices come from the filtered submenu).
        let stt_backends_for_dispatch: Vec<_> =
            fono_core::providers::configured_stt_backends(&secrets, &config.stt.backend);
        let llm_backends_for_dispatch: Vec<_> =
            fono_core::providers::configured_llm_backends(&secrets, &config.llm.backend);
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
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, fsm, action_tx, orch).await {
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
        "inject       : also_copy_to_clipboard={}  notify_on_dictation={}",
        config.general.also_copy_to_clipboard, config.general.notify_on_dictation
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
            let _ = notify_rust::Notification::new()
                .summary("Fono — copied to clipboard")
                .body(&format!("Press Ctrl+V to paste (via {t})"))
                .icon("edit-paste")
                .timeout(notify_rust::Timeout::Milliseconds(4_000))
                .show();
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
                    let _ = notify_rust::Notification::new()
                        .summary("Fono — STT reload failed")
                        .body(&format!("{e}"))
                        .icon("dialog-error")
                        .timeout(notify_rust::Timeout::Milliseconds(5_000))
                        .show();
                    return;
                }
            }
            let _ = notify_rust::Notification::new()
                .summary("Fono — STT switched")
                .body(&format!("Active speech-to-text backend: {label}"))
                .icon("audio-input-microphone")
                .timeout(notify_rust::Timeout::Milliseconds(3_000))
                .show();
        }
        Ok(Err(e)) => {
            warn!("tray: STT switch failed: {e:#}");
            let _ = notify_rust::Notification::new()
                .summary("Fono — STT switch failed")
                .body(&format!("{e}"))
                .icon("dialog-error")
                .timeout(notify_rust::Timeout::Milliseconds(5_000))
                .show();
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
                    let _ = notify_rust::Notification::new()
                        .summary("Fono — LLM reload failed")
                        .body(&format!("{e}"))
                        .icon("dialog-error")
                        .timeout(notify_rust::Timeout::Milliseconds(5_000))
                        .show();
                    return;
                }
            }
            let _ = notify_rust::Notification::new()
                .summary("Fono — LLM switched")
                .body(&format!("Active text-cleanup backend: {label}"))
                .icon("accessories-text-editor")
                .timeout(notify_rust::Timeout::Milliseconds(3_000))
                .show();
        }
        Ok(Err(e)) => {
            warn!("tray: LLM switch failed: {e:#}");
            let _ = notify_rust::Notification::new()
                .summary("Fono — LLM switch failed")
                .body(&format!("{e}"))
                .icon("dialog-error")
                .timeout(notify_rust::Timeout::Milliseconds(5_000))
                .show();
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
        let _ = notify_rust::Notification::new()
            .summary("Fono — downloading speech model")
            .body(&body)
            .icon("emblem-downloads")
            .timeout(notify_rust::Timeout::Milliseconds(4_000))
            .show();
    }
    match crate::models::ensure_local_stt(paths, &model).await {
        Ok(crate::models::EnsureOutcome::Downloaded) => {
            let _ = notify_rust::Notification::new()
                .summary("Fono — speech model ready")
                .body(&format!("{model} downloaded and cached"))
                .icon("emblem-default")
                .timeout(notify_rust::Timeout::Milliseconds(4_000))
                .show();
            true
        }
        Ok(_) => true,
        Err(e) => {
            warn!("ensure_local_stt: download failed: {e:#}");
            let _ = notify_rust::Notification::new()
                .summary("Fono — speech model download failed")
                .body(&format!("{e}"))
                .icon("dialog-error")
                .timeout(notify_rust::Timeout::Milliseconds(6_000))
                .show();
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
    let dest_exists = paths.llm_models_dir().join(format!("{model}.gguf")).exists();
    if !dest_exists {
        let body = size_hint.map_or_else(
            || format!("LLM model: {model}"),
            |mb| format!("LLM model: {model} ({mb} MB)"),
        );
        let _ = notify_rust::Notification::new()
            .summary("Fono — downloading cleanup model")
            .body(&body)
            .icon("emblem-downloads")
            .timeout(notify_rust::Timeout::Milliseconds(4_000))
            .show();
    }
    match crate::models::ensure_local_llm(paths, &model).await {
        Ok(crate::models::EnsureOutcome::Downloaded) => {
            let _ = notify_rust::Notification::new()
                .summary("Fono — cleanup model ready")
                .body(&format!("{model} downloaded and cached"))
                .icon("emblem-default")
                .timeout(notify_rust::Timeout::Milliseconds(4_000))
                .show();
            true
        }
        Ok(_) => true,
        Err(e) => {
            warn!("ensure_local_llm: download failed: {e:#}");
            let _ = notify_rust::Notification::new()
                .summary("Fono — cleanup model download failed")
                .body(&format!("{e}"))
                .icon("dialog-error")
                .timeout(notify_rust::Timeout::Milliseconds(6_000))
                .show();
            false
        }
    }
}
