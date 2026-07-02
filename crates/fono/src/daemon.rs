// SPDX-License-Identifier: GPL-3.0-only
//! Daemon event loop: startup banner, global-hotkey listener, tray icon,
//! IPC server, FSM dispatcher, and the [`crate::session::SessionOrchestrator`]
//! that drives audio → STT → LLM → inject end to end.

use anyhow::{Context, Result};
use fono_core::{Config, Paths, Secrets};
use fono_hotkey::{
    HotkeyAction, HotkeyBindings, HotkeyControl, HotkeyControlSender, HotkeyEvent, RecordingFsm,
    State as FsmState,
};
use fono_ipc::{read_frame, write_frame, McpPhase, Request, Response};
use fono_tray::{Tray, TrayAction, TrayState};
#[cfg(feature = "interactive")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

use crate::cli::Verbosity;
use crate::session::SessionOrchestrator;

/// Translate the standard `Hold*` / `Toggle*` actions to their `Live*`
/// counterparts when `Config::live_preview()` is true (currently:
/// `[overlay].style == "transcript"`), the binary was built with
/// `--features interactive`, and an orchestrator is available to
/// drive the live state. Off otherwise — the action is passed
/// through unchanged.
///
/// `CancelPressed`, `ProcessingDone`, and `ProcessingStarted` are
/// always passed through; the FSM already routes Cancel from any
/// state to Idle.
fn translate_for_live_preview(action: HotkeyAction, live_preview_enabled: bool) -> HotkeyAction {
    #[cfg(feature = "interactive")]
    {
        if live_preview_enabled {
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
        let _ = live_preview_enabled;
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

#[allow(clippy::too_many_lines, clippy::cognitive_complexity, clippy::if_not_else)]
pub async fn run(paths: &Paths, verbosity: Verbosity) -> Result<()> {
    let config_path = paths.config_file();
    let first_run = !config_path.exists();
    let mut config = Config::load(&config_path).context("load config")?;
    let mut config_dirty = false;
    if first_run {
        // No config on disk: pick a hardware-appropriate whisper model
        // so the daemon comes up working even when the user skipped the
        // wizard.
        let mut snap = fono_core::hwcheck::probe(&paths.cache_dir);
        // Upgrade `host_gpu` from the Vulkan probe before classifying;
        // see ADR 0028. Daemon shares the cached probe with the wizard.
        if snap.host_gpu == fono_core::hwcheck::HostGpu::None {
            snap.host_gpu = fono_core::vulkan_probe::probe().host_gpu_class();
        }
        // The CPU release variant cannot route inference to the host's
        // Vulkan GPU even when one is present; collapse host_gpu before
        // affordability scoring so we don't pre-pick a model that
        // relies on a GPU speedup we can't deliver.
        let inference_snap =
            snap.for_inference(matches!(crate::variant::VARIANT, crate::variant::Variant::Gpu));
        let picked = fono_stt::registry::ModelRegistry::pick_default_local(&inference_snap);
        if picked != config.stt.local.model {
            info!(
                "first run: defaulting whisper model to {:?} (was {:?})",
                picked, config.stt.local.model
            );
            config.stt.local.model = picked.into();
            config_dirty = true;
        }
    }
    if config.general.languages.is_empty() {
        // Whenever the allow-list is empty (fresh config, or pre-existing
        // config from a build that didn't populate it), seed from OS
        // locale signals. The wizard fills this explicitly when run, so
        // the only path here is "user skipped the wizard".
        let detected: Vec<String> =
            fono_core::locale::detect_user_languages_ranked().into_iter().map(|d| d.code).collect();
        if !detected.is_empty() {
            info!("auto-populating languages from OS locale: {detected:?}");
            config.general.languages = detected;
            config_dirty = true;
        }
    }
    if config_dirty {
        if let Err(e) = config.save(&config_path) {
            warn!("could not persist auto-populated config: {e:#}");
        }
    }

    // Propagate `[inject].backend` to fono-inject via env so every
    // call site (daemon, CLI subcommands, doctor) sees the same
    // effective backend without each one having to thread the config
    // through `Injector::detect()`. Leave the env untouched on the
    // `"auto"` default so debug overrides via the shell still win.
    apply_inject_backend_env(&config.inject);
    let config = Arc::new(config);
    let secrets = Secrets::load(&paths.secrets_file()).context("load secrets")?;
    print_banner(paths, &config, verbosity);

    // Best-effort, non-blocking voice-discovery refresh at daemon start.
    // Keeps the active cloud backend's voice palette fresh without delaying
    // startup; any failure (no key, network, provider error) is logged at
    // debug and leaves the existing cache untouched. The lazy `fono voices
    // list` refresh (>24h) complements this for long-lived daemons.
    if config.tts.voice_discovery {
        let backend = fono_core::providers::tts_backend_str(&config.tts.backend).to_string();
        if fono_core::provider_catalog::tts_discovery(&backend).is_some() {
            let paths_bg = paths.clone();
            let cfg_bg = Arc::clone(&config);
            tokio::spawn(async move {
                match crate::cli::refresh_discovered_palette(
                    &paths_bg,
                    &cfg_bg,
                    &backend,
                    fono_tts::discovery::DEFAULT_DISCOVERY_TIMEOUT,
                )
                .await
                {
                    Ok(crate::cli::RefreshOutcome::Refreshed(r)) => {
                        info!(
                            "voice discovery: refreshed {} voice(s) for {backend}",
                            r.voices.len()
                        );
                    }
                    Ok(_) => debug!("voice discovery: nothing to refresh for {backend}"),
                    Err(e) => debug!("voice discovery refresh failed for {backend}: {e:#}"),
                }
            });
        }
    }

    // Single-instance guard: probe the IPC socket. If a previous
    // daemon is alive it answers `connect()`; bail before we
    // duplicate it. Stale sockets (ConnectionRefused / ENOENT) and
    // the bind below replaces them cleanly.
    let socket_path = paths.ipc_socket();
    if socket_path.exists() {
        match tokio::net::UnixStream::connect(&socket_path).await {
            Ok(_) => anyhow::bail!(
                "another fono daemon is already running (IPC socket {} is live). \
                 Stop it before starting a new instance (e.g. `pkill fono`).",
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

    // Shared "is the key physically held down?" flags. Constructed
    // here so the same `Arc<AtomicBool>`s are seen by both the
    // hotkey listener (writer) and the orchestrator's silence-watch
    // task (reader). See `fono_hotkey::KeyHeldFlags` for the
    // motivation.
    let held_flags = fono_hotkey::KeyHeldFlags::new();

    // ---------------------------------------------------------------
    // Build the orchestrator. STT failure → degraded mode (hotkeys
    // still register but recording emits a warning instead of audio).
    // ---------------------------------------------------------------
    let orchestrator: Option<Arc<SessionOrchestrator>> = match SessionOrchestrator::new(
        Arc::clone(&config),
        &secrets,
        paths,
        action_tx.clone(),
        held_flags.clone(),
    ) {
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
    // Always-on wake-word listener (Phases D/E). Off unless
    // `[wakeword].enabled`. Owns a single capture stream that is held
    // only while the FSM is Idle and dropped during any active session
    // (suspend/resume below), and fires the configured `HotkeyAction`
    // into the same `action_tx` as the physical hotkey on detection.
    // ---------------------------------------------------------------
    let wake = crate::wake::spawn(config.as_ref(), paths, action_tx.clone());

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
        dictation: config.hotkeys.dictation.clone(),
        cancel: config.hotkeys.cancel.clone(),
        assistant: config.hotkeys.assistant.clone(),
    };
    let forced_backend = std::env::var("FONO_HOTKEY_BACKEND").ok().and_then(|v| {
        let parsed = fono_hotkey::HotkeyBackend::parse(&v);
        if parsed.is_none() && !v.trim().is_empty() {
            warn!("unknown FONO_HOTKEY_BACKEND={v:?}; falling back to auto-detection");
        }
        parsed
    });
    let cancel_ctrl: Option<HotkeyControlSender> = if crate::is_graphical_session() {
        match fono_hotkey::spawn_with_backend(
            forced_backend,
            bindings,
            action_tx.clone(),
            held_flags.clone(),
        ) {
            Ok(Some(handle)) => {
                debug!("global hotkeys registered");
                Some(handle.control)
            }
            Ok(None) => {
                debug!("hotkey listener disabled by backend choice");
                None
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
    // mDNS discovery (Slice 4 of the network plan). Starts after the
    // global hotkey grab so LAN browsing cannot delay local desktop
    // readiness. The Wyoming advertiser is managed alongside the LAN
    // listener below so both can be toggled live from the tray.
    // ---------------------------------------------------------------
    let discovery = spawn_discovery_if_enabled().await;
    let discovery_registry: Option<fono_net::discovery::Registry> =
        discovery.as_ref().map(|d| d.registry.clone());

    // ---------------------------------------------------------------
    // LAN Wyoming server (Slice 3 of the network plan). Off by default;
    // reconciled after the hotkey grab so optional network serving does
    // not hold up local dictation readiness. Held for the daemon's
    // lifetime; dropped on exit, which closes the listener and fires
    // the mDNS goodbye.
    // ---------------------------------------------------------------
    let wyoming_ctl = WyomingControl {
        rt: Arc::new(tokio::sync::Mutex::new(WyomingRuntime::default())),
        mdns: discovery.as_ref().and_then(|d| d.daemon.clone()),
        registry: discovery_registry.clone(),
    };
    wyoming_ctl.reconcile(&config, paths, orchestrator.as_ref()).await;

    // ---------------------------------------------------------------
    // Local LLM inference server (OpenAI + Ollama HTTP API; ADR 0036).
    // Off by default; spawned after the hotkey grab so optional network
    // serving does not hold up local dictation readiness. Held for the
    // daemon's lifetime; dropping the handles closes the listener and
    // fires the mDNS goodbye. Backend swaps (`fono use assistant …`) are
    // tracked live via the per-request provider closure. Toggling
    // `[server.llm].enabled` hot-reloads the listener in place via the
    // tray (`ToggleLlmServer`) — no restart required, mirroring Wyoming.
    // ---------------------------------------------------------------
    let llm_ctl = LlmControl {
        rt: Arc::new(tokio::sync::Mutex::new(LlmRuntime::default())),
        mdns: discovery.as_ref().and_then(|d| d.daemon.clone()),
        registry: discovery_registry.clone(),
    };
    llm_ctl.reconcile(&config, orchestrator.as_ref()).await;

    // ---------------------------------------------------------------
    // Web settings UI server (plans/2026-07-02-web-config-ui-v2.md).
    // Off by default; loopback-only unless `[server.web].bind` is
    // widened. Serves the embedded browser settings page + JSON API.
    // Config writes go through the same save → orchestrator reload →
    // wake reload path as `fono use …`. The handle lives in a shared
    // slot so the tray's "Settings…" entry can lazy-start the
    // listener on first click; disabling `[server.web].enabled`
    // afterwards needs a restart.
    // ---------------------------------------------------------------
    let web_settings: WebSettingsSlot = Arc::new(tokio::sync::Mutex::new(
        spawn_web_settings_if_enabled(&config, paths, &secrets, orchestrator.as_ref(), &wake).await,
    ));

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
        let pkg_managed =
            std::env::current_exe().map(|p| fono_update::is_package_managed(&p)).unwrap_or(false);
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
    // spawn it when the host is headless (no `DISPLAY` and no
    // `WAYLAND_DISPLAY` in the daemon's environment). On a headless host
    // attempting to bring up an SNI tray either fails noisily (no D-Bus
    // session bus) or blocks the libappindicator thread forever — neither
    // is acceptable for the `fono serve` use case. Graphical sessions
    // without an SNI host (sway-without-waybar, bare X11) are handled
    // inside `fono-tray` itself: the tray task logs one warning and
    // exits cleanly while dictation + overlay continue.
    //
    // See `plans/2026-04-30-fono-single-binary-size-v1.md` Phase 3
    // Task 3.1 for the runtime-detection contract.
    // ---------------------------------------------------------------
    let graphical = crate::is_graphical_session();
    let (tray, mut tray_rx) = if !graphical {
        debug!("tray skipped (headless: no DISPLAY / WAYLAND_DISPLAY)");
        let (_tx, rx) = mpsc::unbounded_channel::<TrayAction>();
        (None, rx)
    } else {
        // Tray menu's "Recent transcriptions" submenu reads from the
        // history DB on a 2-second poll. Provide a closure that returns
        // the cleaned text (or raw if no polish) of the last 10 rows.
        let history_db_path = paths.history_db();
        let recent_provider: fono_tray::RecentProvider = Arc::new(move || {
            let Ok(db) = fono_core::history::HistoryDb::open(&history_db_path) else {
                return Vec::new();
            };
            db.recent(fono_tray::RECENT_SLOTS)
                .map(|rows| rows.into_iter().map(|r| r.cleaned.unwrap_or(r.raw)).collect())
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
        let polish_backends: Vec<_> =
            fono_core::providers::configured_polish_backends(&secrets, &config.polish.backend);
        // Assistant + TTS submenus mirror the STT/LLM ones. We show
        // every "configured" backend (key present in secrets, plus
        // None / Local / Wyoming which never need a key) and the
        // active one even if its key is missing — same convention as
        // STT/LLM. Snapshot at startup; users who add a key via
        // `fono keys add` need a daemon restart to see it appear.
        let assistant_backends: Vec<_> = fono_core::providers::all_assistant_backends()
            .into_iter()
            .filter(|b| {
                let active = *b == config.assistant.backend;
                let needs_key = fono_core::providers::assistant_requires_key(b);
                active
                    || !needs_key
                    || secrets.resolve(fono_core::providers::assistant_key_env(b)).is_some()
            })
            .collect();
        let tts_backends: Vec<_> = fono_core::providers::configured_tts_backends(
            &secrets,
            &config.tts.backend,
            config.tts.wyoming.as_ref().is_some_and(|w| !w.uri.trim().is_empty()),
        );
        let stt_labels: Vec<String> = stt_backends
            .iter()
            .map(|b| fono_core::providers::stt_backend_str(b).to_string())
            .collect();
        let polish_labels: Vec<String> = polish_backends
            .iter()
            .map(|b| fono_core::providers::polish_backend_str(b).to_string())
            .collect();
        let assistant_labels: Vec<String> = assistant_backends
            .iter()
            .map(|b| fono_core::providers::assistant_backend_str(b).to_string())
            .collect();
        let tts_labels: Vec<String> =
            tts_backends.iter().map(|b| tts_menu_label(b, &secrets)).collect();

        // Startup diagnostic for the "tray STT/LLM submenu sometimes
        // empty" intermittent — these labels are static after spawn,
        // so logging them once gives us a definitive answer about
        // whether the issue is data (empty here means empty menu) or
        // host rendering (non-empty here + empty menu means it's
        // KDE/GNOME mishandling LayoutUpdated). At debug to avoid
        // cluttering the default startup output.
        debug!("tray: configured STT backends ({}) = {:?}", stt_labels.len(), stt_labels);
        debug!("tray: configured polish backends ({}) = {:?}", polish_labels.len(), polish_labels);

        // Active-provider closure — tray polls this every ~2 s. Reads
        // the orchestrator's current backend pair (which already reflects
        // any `Reload`-driven hot-swap) and falls back to the on-disk
        // config string match when the orchestrator isn't available
        // (degraded mode).
        let orch_for_tray = orchestrator.clone();
        let config_path = paths.config_file();
        let active_provider: fono_tray::ActiveProvider = Arc::new(move || {
            let (stt_str, polish_str, assistant_str, tts_str) = orch_for_tray.as_ref().map_or_else(
                || {
                    fono_core::Config::load(&config_path)
                        .map(|c| {
                            (
                                fono_core::providers::stt_backend_str(&c.stt.backend).to_string(),
                                fono_core::providers::polish_backend_str(&c.polish.backend)
                                    .to_string(),
                                fono_core::providers::assistant_backend_str(&c.assistant.backend)
                                    .to_string(),
                                fono_core::providers::tts_backend_str(&c.tts.backend).to_string(),
                            )
                        })
                        .unwrap_or_else(|_| {
                            ("local".into(), "none".into(), "none".into(), "none".into())
                        })
                },
                |o| o.active_backends_full(),
            );
            let stt_idx = stt_backends
                .iter()
                .position(|b| fono_core::providers::stt_backend_str(b) == stt_str)
                .and_then(|i| u8::try_from(i).ok())
                .unwrap_or(u8::MAX);
            let llm_idx = polish_backends
                .iter()
                .position(|b| fono_core::providers::polish_backend_str(b) == polish_str)
                .and_then(|i| u8::try_from(i).ok())
                .unwrap_or(u8::MAX);
            let assistant_idx = assistant_backends
                .iter()
                .position(|b| fono_core::providers::assistant_backend_str(b) == assistant_str)
                .and_then(|i| u8::try_from(i).ok())
                .unwrap_or(u8::MAX);
            let tts_idx = tts_backends
                .iter()
                .position(|b| fono_core::providers::tts_backend_str(b) == tts_str)
                .and_then(|i| u8::try_from(i).ok())
                .unwrap_or(u8::MAX);
            fono_tray::ActiveBackends {
                stt: stt_idx,
                polish: llm_idx,
                assistant: assistant_idx,
                tts: tts_idx,
            }
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
            polish_labels,
            assistant_labels,
            tts_labels,
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
            {
                // MCP-enabled provider: re-read the on-disk config on
                // every ~2 s tray tick so the unified "Servers"
                // submenu's MCP checkmark reflects external toggles
                // (`fono use mcp-server on/off`) and the tray-driven
                // toggle below picks up the new state without a
                // daemon restart.
                let config_path_for_mcp = paths.config_file();
                Arc::new(move || {
                    fono_core::Config::load(&config_path_for_mcp)
                        .map(|c| c.mcp.enabled)
                        .unwrap_or(true)
                }) as fono_tray::McpEnabledProvider
            },
            {
                // Wyoming-enabled provider: same shape as the MCP
                // provider but reads `[server.wyoming].enabled`.
                // Default to `false` on a read error since the LAN
                // listener is off by default — a missing config
                // shouldn't render a misleading checkmark.
                let config_path_for_wyoming = paths.config_file();
                Arc::new(move || {
                    fono_core::Config::load(&config_path_for_wyoming)
                        .map(|c| c.server.wyoming.enabled)
                        .unwrap_or(false)
                }) as fono_tray::WyomingEnabledProvider
            },
            {
                // LLM-enabled provider: same shape as the Wyoming
                // provider but reads `[server.llm].enabled`. Default to
                // `false` on a read error since the listener is off by
                // default — a missing config shouldn't render a
                // misleading checkmark.
                let config_path_for_llm = paths.config_file();
                Arc::new(move || {
                    fono_core::Config::load(&config_path_for_llm)
                        .map(|c| c.server.llm.enabled)
                        .unwrap_or(false)
                }) as fono_tray::LlmEnabledProvider
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
                        HotkeyEvent::StartRecording(_)
                        | HotkeyEvent::StartLiveDictation(_)
                        | HotkeyEvent::StartAssistant
                        | HotkeyEvent::RestartAssistant
                        // Keep Escape grabbed for the lifetime of a live
                        // session so the user can exit it with Escape.
                        | HotkeyEvent::EnterAssistantLive => {
                            let _ = ctrl.send(HotkeyControl::EnableCancel);
                        }
                        HotkeyEvent::StopRecording
                        | HotkeyEvent::StopLiveDictation
                        | HotkeyEvent::Cancel
                        | HotkeyEvent::StopAssistantPlayback
                        | HotkeyEvent::ExitAssistantLive => {
                            let _ = ctrl.send(HotkeyControl::DisableCancel);
                        }
                        HotkeyEvent::StopAssistant => {
                            // Keep Escape grabbed while we're in
                            // Thinking / Speaking — the user might
                            // bail out of a long reply.
                        }
                        HotkeyEvent::McpToolStarted(_) => {
                            let _ = ctrl.send(HotkeyControl::EnableCancel);
                        }
                        HotkeyEvent::McpToolCancelled | HotkeyEvent::McpToolDone => {
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
                        HotkeyEvent::Cancel | HotkeyEvent::StopAssistantPlayback => {
                            t.set_state(TrayState::Idle);
                        }
                        // Assistant flow: green during the recording
                        // phase (mirrors the overlay's green
                        // accent), then amber on release through
                        // the post-release pump (thinking +
                        // speaking). The amber matches both the
                        // overlay's "THINKING" panel and the
                        // colour the user already associates with
                        // "the LLM is working".
                        HotkeyEvent::StartAssistant => t.set_state(TrayState::Assistant),
                        // Barge-in restart re-enters the recording phase
                        // → green, same as a fresh assistant press.
                        HotkeyEvent::RestartAssistant => t.set_state(TrayState::Assistant),
                        HotkeyEvent::StopAssistant => t.set_state(TrayState::Processing),
                        // Live conversation mode: amber while the
                        // session is open; back to idle on exit.
                        HotkeyEvent::EnterAssistantLive => t.set_state(TrayState::Assistant),
                        HotkeyEvent::ExitAssistantLive => t.set_state(TrayState::Idle),
                        HotkeyEvent::McpToolStarted(_) => t.set_state(TrayState::Assistant),
                        HotkeyEvent::McpToolCancelled | HotkeyEvent::McpToolDone => {
                            t.set_state(TrayState::Idle);
                        }
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
                            | HotkeyEvent::StopAssistant
                            | HotkeyEvent::StopAssistantPlayback
                            | HotkeyEvent::RestartAssistant
                            | HotkeyEvent::EnterAssistantLive
                            | HotkeyEvent::ExitAssistantLive
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
                            notify_recording_failure(&err);
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
                                notify_recording_failure(&err2);
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
                            notify_recording_failure(&err);
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
                    HotkeyEvent::StartAssistant => {
                        if let Err(err) = o.on_assistant_hold_press().await {
                            warn!("assistant start failed: {err:#}");
                            if let Some(t) = tray.as_ref().as_ref() {
                                t.set_state(TrayState::Idle);
                            }
                            let _ = action_tx_ev.send(HotkeyAction::ProcessingDone);
                        }
                    }
                    HotkeyEvent::StopAssistant => {
                        let o = Arc::clone(o);
                        tokio::spawn(async move {
                            o.on_assistant_hold_release().await;
                        });
                    }
                    HotkeyEvent::StopAssistantPlayback => {
                        let o = Arc::clone(o);
                        let action_tx = action_tx_ev.clone();
                        tokio::spawn(async move {
                            o.on_assistant_stop().await;
                            // Cancellation isn't reported back through
                            // the pump's `ProcessingDone` path (it
                            // bails out before that fires), so emit
                            // it here so the FSM returns to Idle.
                            let _ = action_tx.send(HotkeyAction::ProcessingDone);
                        });
                    }
                    // Barge-in: stop the in-flight reply, then start a
                    // fresh assistant recording — sequentially, awaited
                    // inline. Crucially we do NOT emit `ProcessingDone`
                    // between the two (as the `StopAssistantPlayback`
                    // arm does): the FSM is already in
                    // `AssistantRecording` and a stray `ProcessingDone`
                    // would race it back to `Idle`, stranding the
                    // capture with no overlay and making the next press
                    // a no-op. Awaiting inline (rather than spawning)
                    // mirrors `StartAssistant` so the capture slot is
                    // populated before any release event is processed.
                    HotkeyEvent::RestartAssistant => {
                        o.on_assistant_stop().await;
                        if let Err(err) = o.on_assistant_hold_press().await {
                            warn!("assistant barge-in restart failed: {err:#}");
                            if let Some(t) = tray.as_ref().as_ref() {
                                t.set_state(TrayState::Idle);
                            }
                            let _ = action_tx_ev.send(HotkeyAction::ProcessingDone);
                        }
                    }
                    // Full-duplex live conversation mode (F8 tap on a
                    // realtime backend). Awaited inline — like
                    // `StartAssistant` — so the persistent session handle
                    // is recorded before the next event (e.g. the
                    // exit-tap) is dequeued, avoiding an enter/exit race.
                    HotkeyEvent::EnterAssistantLive => {
                        #[cfg(feature = "realtime")]
                        o.on_assistant_live_enter().await;
                        #[cfg(not(feature = "realtime"))]
                        {
                            let _ = o;
                            let _ = action_tx_ev.send(HotkeyAction::ProcessingDone);
                        }
                    }
                    HotkeyEvent::ExitAssistantLive => {
                        #[cfg(feature = "realtime")]
                        o.on_assistant_live_exit().await;
                        #[cfg(not(feature = "realtime"))]
                        {
                            let _ = o;
                            let _ = action_tx_ev.send(HotkeyAction::ProcessingDone);
                        }
                    }
                    // MCP tool events are generated by `fono mcp serve` (a
                    // separate process/command). The daemon acknowledges them
                    // for FSM state tracking but has no orchestrator action.
                    HotkeyEvent::McpToolStarted(_)
                    | HotkeyEvent::McpToolCancelled
                    | HotkeyEvent::McpToolDone => {}
                }
            }
        });
    }

    // Tracks nested MCP voice interactions over the lifetime of the
    // daemon. The `u32` is the recursion depth (0 = no MCP activity
    // active; >0 = inside `fono.listen` / `fono.speak` / `fono.confirm`).
    // The `TrayState` is the snapshot taken on the 0→1 transition so
    // we can restore exactly what was on screen before MCP took over.
    // Slice 7 of plan v7.
    let mcp_activity: Arc<std::sync::Mutex<(u32, TrayState)>> =
        Arc::new(std::sync::Mutex::new((0, TrayState::Idle)));
    // Broadcast sender used to forward "Escape pressed while MCP is
    // listening" from the action dispatcher to the active
    // `McpActivityHold` connection handlers. Each hold handler
    // subscribes on entry and writes `Response::McpListenCancelled` to
    // the MCP server when a token arrives. Capacity 16 prevents slow
    // handlers from lagging behind on rapid presses; any lag just means
    // the oldest cancel gets dropped and the next one lands.
    let (mcp_cancel_tx, _mcp_cancel_rx) = tokio::sync::broadcast::channel::<()>(16);
    let mcp_cancel_tx: Arc<tokio::sync::broadcast::Sender<()>> = Arc::new(mcp_cancel_tx);

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
        let orch_for_dispatch = orchestrator.clone();
        let cancel_ctrl_disp = cancel_ctrl.clone();
        let mcp_activity_disp = Arc::clone(&mcp_activity);
        let mcp_cancel_tx_disp = Arc::clone(&mcp_cancel_tx);
        let wake_disp = wake.clone();
        tokio::spawn(async move {
            while let Some(action) = action_rx.recv().await {
                // Read `live_preview` straight from the orchestrator's
                // post-reload config so a tray-triggered switch into
                // Transcript style routes the very next hotkey press
                // through the live pipeline (and shows the overlay).
                // Capturing a startup `Arc<Config>` here would freeze
                // the routing decision and suppress the Transcript
                // overlay until a daemon restart.
                let live_preview_enabled =
                    orch_for_dispatch.as_ref().is_some_and(|o| o.live_preview());
                let action = translate_for_live_preview(action, live_preview_enabled);
                let new_state = fsm.lock().await.dispatch(action);
                tracing::debug!("hotkey: {action:?} -> {new_state:?}");
                // Suspend/resume the wake-word listener so the mic is held
                // only in Idle: any active/processing state drops the capture
                // stream, returning to Idle re-opens it (Phase D).
                wake_disp.set_idle(crate::wake::should_listen(new_state));
                if matches!(action, HotkeyAction::ProcessingDone) {
                    if let Some(t) = tray.as_ref().as_ref() {
                        t.set_state(TrayState::Idle);
                    }
                }
                // Belt-and-braces: any transition back to Idle releases
                // the cancel-hotkey grab. The FSM-event consumer above
                // already handles the common Stop*/Cancel paths, but
                // the assistant's natural-completion path leaves
                // Thinking/Speaking via `ProcessingDone` alone and
                // emits no `HotkeyEvent`, so without this the Escape
                // grab would leak until the next cancel/barge-in. Done
                // state-based so future Idle-bound paths get the same
                // treatment for free.
                if new_state == FsmState::Idle {
                    if let Some(ctrl) = cancel_ctrl_disp.as_ref() {
                        let _ = ctrl.send(HotkeyControl::DisableCancel);
                    }
                }
                // Forward Escape to any active MCP listen sessions.
                // When the user presses Escape while an `McpActivityHold`
                // connection is open, broadcast a cancel token so the
                // hold handler writes `Response::McpListenCancelled` to
                // the MCP server's read half, aborting the current
                // `listen_once` call.
                if matches!(action, HotkeyAction::CancelPressed) {
                    let depth = mcp_activity_disp.lock().map(|g| g.0).unwrap_or(0);
                    if depth > 0 {
                        let _ = mcp_cancel_tx_disp.send(());
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
        // so `UseStt(idx)` / `UsePolish(idx)` resolve to the same item the
        // user clicked (the indices come from the filtered submenu).
        let stt_backends_for_dispatch: Vec<_> =
            fono_core::providers::configured_stt_backends(&secrets, &config.stt.backend);
        let llm_backends_for_dispatch: Vec<_> =
            fono_core::providers::configured_polish_backends(&secrets, &config.polish.backend);
        // Assistant + TTS dispatch lists mirror the tray's filter
        // (active + key-present + no-key-needed) so a click resolves
        // back to the same backend the user saw. Rebuilt here
        // because the active_provider closure moved the original
        // vectors when it was constructed.
        let assistant_backends_for_dispatch: Vec<_> =
            fono_core::providers::all_assistant_backends()
                .into_iter()
                .filter(|b| {
                    let active = *b == config.assistant.backend;
                    let needs_key = fono_core::providers::assistant_requires_key(b);
                    active
                        || !needs_key
                        || secrets.resolve(fono_core::providers::assistant_key_env(b)).is_some()
                })
                .collect();
        let tts_backends_for_dispatch: Vec<_> = fono_core::providers::configured_tts_backends(
            &secrets,
            &config.tts.backend,
            config.tts.wyoming.as_ref().is_some_and(|w| !w.uri.trim().is_empty()),
        );
        let update_status_tray = Arc::clone(&update_status);
        let discovered_registry_for_dispatch = discovery_registry.clone();
        let local_wyoming_fullname_for_dispatch = local_wyoming_fullname(&config);
        let orchestrator_for_tray = orchestrator.clone();
        let wyoming_ctl_for_tray = wyoming_ctl.clone();
        let llm_ctl_for_tray = llm_ctl.clone();
        let web_ctl_for_tray = Arc::clone(&web_settings);
        let wake_tray = wake.clone();
        tokio::spawn(async move {
            while let Some(ta) = tray_rx.recv().await {
                debug!("tray action: {ta:?}");
                match ta {
                    TrayAction::ToggleRecording => {
                        let _ = action_tx.send(HotkeyAction::TogglePressed);
                    }
                    TrayAction::Quit => {
                        let _ = std::fs::remove_file(paths.ipc_socket());
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
                    TrayAction::UsePolish(idx) => {
                        switch_llm_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            &llm_backends_for_dispatch,
                            idx,
                        )
                        .await;
                    }
                    TrayAction::UseAssistant(idx) => {
                        switch_assistant_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            &assistant_backends_for_dispatch,
                            idx,
                        )
                        .await;
                    }
                    TrayAction::UseTts(idx) => {
                        switch_tts_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            &tts_backends_for_dispatch,
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
                        // The schema stores VAD as a string (`"energy"`,
                        // `"off"`, possibly more in the future); the tray
                        // exposes it as a boolean for menu legibility.
                        // Translate here. `"energy"` is the only enabled
                        // backend today (the RMS gate); a neural VAD is not
                        // wired yet and would get its own tray entry rather
                        // than bundling under this toggle.
                        apply_pref_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            "vad_backend",
                            move |cfg| {
                                cfg.audio.vad_backend =
                                    if v { "energy".into() } else { "off".into() };
                            },
                        )
                        .await;
                    }
                    TrayAction::SetWakeWordEnabled(v) => {
                        // Persist the toggle, then reconcile the always-on
                        // listener live (Phase D): enabling while Idle opens
                        // the capture stream, disabling drops it — no daemon
                        // restart. `apply_pref_via_tray` writes to disk and
                        // reloads the orchestrator; `wake_tray.reload()` then
                        // re-reads `[wakeword]` from disk and reconciles.
                        apply_pref_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            "wakeword_enabled",
                            move |cfg| {
                                cfg.wakeword.enabled = v;
                                // First enable with no phrases configured:
                                // seed a sensible default so the toggle has
                                // an obvious, named effect rather than
                                // silently doing nothing. Until the clean
                                // `hey_fono` artifact ships we default to
                                // `hey_jarvis` → Assistant.
                                if v && cfg.wakeword.phrases.is_empty() {
                                    cfg.wakeword.phrases.push(fono_core::config::WakePhrase {
                                        model: "hey_jarvis".into(),
                                        sensitivity: 0.5,
                                        target: fono_core::config::WakeTarget::Assistant,
                                    });
                                }
                            },
                        )
                        .await;
                        wake_tray.reload();
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
                        // The VU bar only renders in the Transcript style
                        // (`renderer.rs::redraw` gates it on
                        // `is_text_style(style)`). Pair the two settings so
                        // the tray switch leaves a coherent overlay:
                        // non-Transcript styles silently turn the bar off;
                        // Transcript revives it at `Simple` (the default).
                        // Users who want `Advanced` set it manually in
                        // `config.toml` after switching to Transcript.
                        let paired_bar =
                            if matches!(style, fono_core::config::WaveformStyle::Transcript) {
                                fono_core::config::VolumeBarMode::Simple
                            } else {
                                fono_core::config::VolumeBarMode::Off
                            };
                        apply_pref_via_tray(
                            &paths,
                            orch_for_tray.as_ref(),
                            "overlay.style",
                            move |cfg| {
                                cfg.overlay.style = style;
                                cfg.overlay.volume_bar = paired_bar;
                            },
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
                    TrayAction::OpenSettingsWeb => {
                        // Box the future for the same large_stack_frames
                        // reason as the server toggles below.
                        Box::pin(open_settings_web_via_tray(
                            &paths,
                            &web_ctl_for_tray,
                            orchestrator_for_tray.as_ref(),
                            &wake_tray,
                        ))
                        .await;
                    }
                    TrayAction::AssistantForget => {
                        if let Some(o) = orchestrator_for_tray.as_ref() {
                            o.on_assistant_forget().await;
                        }
                    }
                    TrayAction::ActivateLeftClick => {
                        // Left-click on the tray icon: surface the same
                        // contextual hint the wizard / status notification
                        // would. No-op for now; full handling will land
                        // alongside the onboarding plan.
                        debug!("tray: ActivateLeftClick (no-op)");
                    }
                    TrayAction::ToggleMcpServer => {
                        // Toggle `[mcp.server].enabled` and persist.
                        match fono_core::Config::load(&paths.config_file()) {
                            Ok(mut cfg) => {
                                cfg.mcp.enabled = !cfg.mcp.enabled;
                                if let Err(e) = cfg.save(&paths.config_file()) {
                                    warn!("tray ToggleMcpServer: save failed: {e:#}");
                                } else {
                                    info!(
                                        enabled = cfg.mcp.enabled,
                                        "MCP server toggled via tray; restart `fono mcp serve` \
                                         to apply"
                                    );
                                }
                            }
                            Err(e) => warn!("tray ToggleMcpServer: load config failed: {e:#}"),
                        }
                    }
                    TrayAction::ToggleWyomingServer => {
                        // Box the future so its (Config-holding) stack
                        // frame lives on the heap, keeping the enclosing
                        // tray-dispatch async block under clippy's
                        // `large_stack_frames` threshold.
                        Box::pin(toggle_wyoming_server_via_tray(
                            &paths,
                            &wyoming_ctl_for_tray,
                            orchestrator_for_tray.as_ref(),
                        ))
                        .await;
                    }
                    TrayAction::ToggleLlmServer => {
                        // Box the future so its (Config-holding) stack
                        // frame lives on the heap, keeping the enclosing
                        // tray-dispatch async block under clippy's
                        // `large_stack_frames` threshold.
                        Box::pin(toggle_llm_server_via_tray(
                            &paths,
                            &llm_ctl_for_tray,
                            orchestrator_for_tray.as_ref(),
                        ))
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
    // Cross-process audio-output mutex for `fono.speak`. Each
    // coding-agent integration spawns its own `fono mcp serve`
    // process; without this lock two agents calling `fono.speak`
    // simultaneously would mix overlapping TTS audio on the same
    // device. Acquire is connection-scoped (see the
    // `Request::McpSpeakAcquire` doc-comment in `fono-ipc`), so a
    // crashed MCP server releases the slot via kernel-level socket
    // cleanup. Using `tokio::sync::Mutex` (rather than `std`) so the
    // handler can `.await` on lock acquisition without parking the
    // executor thread.
    let mcp_speak_slot: Arc<tokio::sync::Mutex<()>> = Arc::new(tokio::sync::Mutex::new(()));
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
                let registry = discovery_registry.clone();
                let tray_for_ipc = Arc::clone(&tray);
                let mcp_activity = Arc::clone(&mcp_activity);
                let mcp_speak_slot = Arc::clone(&mcp_speak_slot);
                let mcp_cancel_tx = Arc::clone(&mcp_cancel_tx);
                let cancel_ctrl_for_ipc = cancel_ctrl.clone();
                let wake_for_ipc = wake.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(
                        stream, fsm, action_tx, orch, registry, tray_for_ipc, mcp_activity,
                        mcp_speak_slot, mcp_cancel_tx, cancel_ctrl_for_ipc, wake_for_ipc,
                    )
                    .await
                    {
                        warn!("client error: {e}");
                    }
                });
            }
        }
    }

    let _ = std::fs::remove_file(paths.ipc_socket());
    Ok(())
}

#[allow(clippy::cognitive_complexity, clippy::too_many_lines)]
fn print_banner(paths: &Paths, config: &Config, verbosity: Verbosity) {
    let config_path = paths.config_file();
    let config_present = config_path.exists();
    let graphical = crate::is_graphical_session();
    info!(
        "Fono v{} starting — stt={:?} polish={:?} tray={}",
        env!("CARGO_PKG_VERSION"),
        config.stt.backend,
        config.polish.backend,
        if !graphical {
            "headless"
        } else if cfg!(feature = "tray") {
            "enabled"
        } else {
            "not compiled"
        }
    );
    // [overlay] visibility — always emitted, even on slim builds
    // where the streaming pipeline is compiled out, so the user can
    // diagnose "I picked Transcript style and nothing happened"
    // without turning on debug logging.
    #[cfg(feature = "interactive")]
    {
        info!(
            "live preview : {} (style={:?})",
            if config.live_preview() { "enabled" } else { "disabled" },
            config.overlay.style,
        );
    }
    #[cfg(not(feature = "interactive"))]
    {
        if config.live_preview() {
            warn!(
                "live preview : not compiled in (rebuild with `--features interactive`); \
                 `[overlay].style = \"transcript\"` in config will be ignored"
            );
        }
        // Else: silent. A slim build with a passive waveform style
        // is a fully-supported configuration; no log line needed.
    }
    debug!("variant      : {}", crate::variant::VARIANT.label());
    info!("hw accel     : {}", hardware_acceleration_summary());
    info!("vulkan probe : {}", fono_core::vulkan_probe::probe().summary_line());
    // Surface what the OS thinks the user's languages are so users
    // can sanity-check the wizard's auto-selection and catch
    // misconfigurations (e.g. `LANG=en_US` on a Romanian box). Best
    // effort — the line is suppressed when nothing was detected so
    // headless CI / containerised runs stay quiet. Configured
    // `general.languages` is the source of truth at runtime; this is
    // informational only.
    if let Some(summary) = fono_core::locale::format_detection_summary(
        &fono_core::locale::detect_user_languages_ranked(),
    ) {
        info!("languages    : {summary}");
    }
    debug!(
        "config       : {} ({})",
        config_path.display(),
        if config_present { "loaded" } else { "absent — using defaults" }
    );
    debug!("secrets      : {}", paths.secrets_file().display());
    debug!("history db   : {}", paths.history_db().display());
    debug!("models/whisper: {}", paths.whisper_models_dir().display());
    debug!("models/polish   : {}", paths.polish_models_dir().display());
    debug!("cache        : {}", paths.cache_dir.display());
    debug!("state        : {}", paths.state_dir.display());
    debug!("ipc socket   : {}", paths.ipc_socket().display());
    debug!("log level    : {verbosity:?}  (override with FONO_LOG=...)");
    debug!(
        "tray icon    : {}",
        if !graphical {
            "skipped (headless: no DISPLAY / WAYLAND_DISPLAY)"
        } else if cfg!(feature = "tray") {
            "enabled"
        } else {
            "not compiled in (rebuild with `--features tray`)"
        }
    );
    debug!(
        "hotkeys      : dictation={}  assistant={}  cancel={}  (short=toggle, long=hold)",
        config.hotkeys.dictation, config.hotkeys.assistant, config.hotkeys.cancel,
    );
    debug!("stt backend  : {:?}  (local model: {})", config.stt.backend, config.stt.local.model);
    debug!("polish backend  : {:?}  (enabled={})", config.polish.backend, config.polish.enabled);
    debug!("inject       : also_copy_to_clipboard={}", config.general.also_copy_to_clipboard);
    // Probe and print which inject + clipboard tools are detected, so
    // users immediately see whether they have a working delivery path.
    // Note: fono-inject's native `arboard` clipboard path is always
    // available regardless of which external CLI tools are present,
    // so this probe is purely informational — we never fail closed.
    let injector = fono_inject::Injector::detect();
    let clipboard_tool = ["wl-copy", "xclip", "xsel"]
        .iter()
        .find(|t| which_in_path(t).is_some())
        .copied()
        .unwrap_or("none (using native arboard)");
    debug!("delivery     : key-injector={injector:?}  clipboard-tool={clipboard_tool}");
    if matches!(injector, fono_inject::Injector::None) {
        let session = crate::install::Session::detect();
        let recommendation = session.recommend_injector();
        // On GNOME-Wayland, clipboard-only is the *intended* default
        // (see crates/fono-inject/src/inject.rs `detect_auto`). It's
        // not a degraded mode; log it calmly. On every other session
        // a missing injector is genuinely something to flag.
        if matches!(session, crate::install::Session::GnomeWayland) {
            tracing::info!(
                "inject       : clipboard delivery (GNOME-Wayland default). {recommendation}"
            );
        } else {
            warn!(
                "no auto-typing backend available on this session — dictation will land \
                 on the clipboard; press Ctrl+V or Shift+Insert to paste. {recommendation}"
            );
        }
    }
}

/// Apply the user's `[inject].backend` config setting as a process-
/// wide `FONO_INJECT_BACKEND` env var, so `fono_inject::Injector::detect()`
/// observes the override. Leaves the env untouched when the value is
/// `"auto"` (or empty), preserving any shell-side debug override.
/// This is called once at daemon startup and again after every config
/// reload so `fono use inject …` takes effect without a restart.
pub(crate) fn apply_inject_backend_env(inject: &fono_core::config::Inject) {
    let v = inject.backend.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("auto") {
        // Leave whatever the user set in their shell (if anything)
        // alone — that's the documented debug-override channel.
        return;
    }
    // SAFETY: setting process env from a single-threaded startup path.
    // The daemon binary calls this before spawning any worker threads
    // that read inject env, and again on reload from a single tokio
    // task. fono-inject reads the env at each `Injector::detect()`,
    // so no thread-vs-env races exist.
    std::env::set_var("FONO_INJECT_BACKEND", v);
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
/// GPU accelerator features land in `fono-stt`/`fono-polish` the matching
/// `cfg(feature = …)` blocks below light up, e.g. `CUDA + CPU AVX2`.
fn hardware_acceleration_summary() -> String {
    // `mut` is required when any of the cfg(feature = "accel-*") arms
    // below are active. On the default CPU-only build none fire, hence
    // the allow.
    #[allow(unused_mut)]
    let mut accels: Vec<&'static str> = Vec::new();

    // GPU / accelerator backends — pulled in via opt-in cargo features
    // on `fono-stt` / `fono-polish`. Both crates consume the same ggml,
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
    // pick them up. `mut` is only needed when one of the cfg-gated
    // pushes below is active; the portable baseline (no DotProd /
    // no FP16 in `target_feature`) leaves `feats` untouched, so
    // suppress the conditional `unused_mut` warning.
    #[allow(unused_mut)]
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

#[allow(clippy::too_many_lines, clippy::too_many_arguments, clippy::cognitive_complexity)]
async fn handle_client(
    mut stream: UnixStream,
    fsm: Arc<Mutex<RecordingFsm>>,
    action_tx: mpsc::UnboundedSender<HotkeyAction>,
    orchestrator: Option<Arc<SessionOrchestrator>>,
    discovery_registry: Option<fono_net::discovery::Registry>,
    tray: Arc<Option<Tray>>,
    mcp_activity: Arc<std::sync::Mutex<(u32, TrayState)>>,
    mcp_speak_slot: Arc<tokio::sync::Mutex<()>>,
    mcp_cancel_tx: Arc<tokio::sync::broadcast::Sender<()>>,
    cancel_ctrl: Option<fono_hotkey::HotkeyControlSender>,
    wake: crate::wake::WakeHandle,
) -> Result<()> {
    let req: Request = read_frame(&mut stream).await?;
    // Read live-preview from the orchestrator's post-reload config
    // snapshot (rather than a startup-time `Arc<Config>` capture) so
    // a tray-triggered switch into Transcript style takes effect on
    // the very next IPC press — see the action dispatcher above for
    // the same rationale.
    let live_preview_enabled = orchestrator.as_ref().is_some_and(|o| o.live_preview());
    let send_translated = |a: HotkeyAction| {
        let _ = action_tx.send(translate_for_live_preview(a, live_preview_enabled));
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
                    format!("stt={s} polish={l}")
                },
            );
            Response::Text(format!("fono daemon running; fsm={state:?}; {active}"))
        }
        Request::Reload => {
            // Provider-switching plan task S11. Re-reads config + secrets
            // and atomically swaps the orchestrator's STT/LLM.
            match orchestrator.as_ref() {
                Some(o) => match o.reload().await {
                    Ok(summary) => {
                        // Reconcile the wake-word listener against the freshly
                        // reloaded `[wakeword]` config (Phase D live reload).
                        wake.reload();
                        Response::Text(summary)
                    }
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
            let peers = discovery_registry.as_ref().map(snapshot_discovered).unwrap_or_default();
            Response::Discovered(peers)
        }
        Request::AssistantHoldPress => match orchestrator.as_ref() {
            Some(o) => match o.on_assistant_hold_press().await {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error(format!("assistant start failed: {e:#}")),
            },
            None => Response::Error("daemon is in degraded mode (no orchestrator)".into()),
        },
        Request::AssistantHoldRelease => match orchestrator.as_ref() {
            Some(o) => {
                o.on_assistant_hold_release().await;
                Response::Ok
            }
            None => Response::Error("daemon is in degraded mode (no orchestrator)".into()),
        },
        Request::AssistantStop => match orchestrator.as_ref() {
            Some(o) => {
                o.on_assistant_stop().await;
                Response::Ok
            }
            None => Response::Error("daemon is in degraded mode (no orchestrator)".into()),
        },
        Request::AssistantForget => match orchestrator.as_ref() {
            Some(o) => {
                o.on_assistant_forget().await;
                Response::Ok
            }
            None => Response::Error("daemon is in degraded mode (no orchestrator)".into()),
        },
        Request::Cancel => {
            // Route through the FSM so its state stays in sync. The
            // FSM's `CancelPressed` arm covers every active state
            // (Recording, LiveDictating, AssistantRecording,
            // AssistantThinking, AssistantSpeaking) and emits the
            // matching `HotkeyEvent::Cancel` /
            // `StopAssistantPlayback`, which the event consumer
            // already dispatches to `orch.on_cancel()` /
            // `orch.on_assistant_stop()` plus `DisableCancel`. Going
            // straight to the orchestrator instead would leave the
            // FSM stuck in Recording, so the next F7 press would
            // transition Recording → Processing (a no-op stop) and
            // the *second* F7 would be the one that actually starts
            // a new recording — the user-visible "F7 twice" bug.
            send_translated(HotkeyAction::CancelPressed);
            Response::Ok
        }
        Request::McpActivityStart { phase } => {
            handle_mcp_activity_start(
                &mcp_activity,
                tray.as_ref().as_ref(),
                phase,
                cancel_ctrl.as_ref(),
            );
            Response::Ok
        }
        Request::McpActivityEnd => {
            handle_mcp_activity_end(&mcp_activity, tray.as_ref().as_ref(), cancel_ctrl.as_ref());
            Response::Ok
        }
        Request::McpActivityHold { phase } => {
            // Persistent-connection hold: increment depth (flip tray on
            // 0→1), ack the client, then wait for either a cancel broadcast
            // or EOF from the client side. On EOF (clean exit or Ctrl-C
            // kill of the MCP server) the depth is decremented and the
            // tray is restored. If the user presses Escape while we're
            // in here, the broadcast fires and we forward
            // `Response::McpListenCancelled` so the MCP server's polling
            // loop can abort `listen_once`.
            use tokio::io::AsyncReadExt as _;
            handle_mcp_activity_start(
                &mcp_activity,
                tray.as_ref().as_ref(),
                phase,
                cancel_ctrl.as_ref(),
            );
            write_frame(&mut stream, &Response::Ok).await?;
            // Subscribe before the loop so we don't miss a cancel fired
            // between the Ok ack and the first iteration.
            let mut cancel_rx = mcp_cancel_tx.subscribe();
            // Split so we can read for EOF while also writing cancel signals.
            let (mut read_half, mut write_half) = stream.into_split();
            let mut eof_buf = [0u8; 1];
            loop {
                tokio::select! {
                    // Cancel broadcast from the action dispatcher (Escape key).
                    result = cancel_rx.recv() => {
                        match result {
                            Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                debug!(
                                    target: "fono::daemon",
                                    "MCP hold: forwarding cancel to MCP server"
                                );
                                if let Err(e) =
                                    write_frame(&mut write_half, &Response::McpListenCancelled)
                                        .await
                                {
                                    debug!(
                                        target: "fono::daemon",
                                        error = %e,
                                        "MCP hold: cancel write failed (client gone?)",
                                    );
                                    break;
                                }
                                // Keep looping — wait for the client to
                                // finish its listen and close the connection.
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                    // EOF / error from the MCP server side means it's done.
                    result = read_half.read(&mut eof_buf) => {
                        let _ = result;
                        break;
                    }
                }
            }
            handle_mcp_activity_end(&mcp_activity, tray.as_ref().as_ref(), cancel_ctrl.as_ref());
            return Ok(());
        }
        Request::McpSpeakAcquire => {
            // Acquire the global speak-slot mutex (await — concurrent
            // MCP servers queue up here in FIFO-ish order). Once held,
            // ack the client with `Response::Ok` and *do not close
            // the connection*: the lock lifetime is bound to the
            // socket. The client signals release by dropping its end
            // of the stream, which produces an EOF on our read.
            use tokio::io::AsyncReadExt as _;
            let _slot_guard = mcp_speak_slot.lock().await;
            write_frame(&mut stream, &Response::Ok).await?;
            let mut buf = [0u8; 1];
            // Loop because a well-behaved client only closes the
            // socket, but a buggy one might write stray bytes; we
            // keep holding the mutex until EOF / error either way.
            loop {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            return Ok(());
        }
        Request::Shutdown => {
            std::process::exit(0);
        }
    };
    write_frame(&mut stream, &resp).await?;
    Ok(())
}

/// Handle a `Request::McpActivityStart`. Increments the depth
/// counter; on the 0→1 transition, snapshots the tray's current
/// state into the shared mutex and flips it to amber
/// ([`TrayState::Processing`]). Nested starts only bump the counter
/// — they do not re-snapshot, so the eventual restore picks up
/// whatever the tray showed before the *outermost* MCP span began.
/// Slice 7 of plan v7.
#[allow(clippy::significant_drop_tightening)]
fn handle_mcp_activity_start(
    state: &std::sync::Mutex<(u32, TrayState)>,
    tray: Option<&Tray>,
    phase: McpPhase,
    cancel_ctrl: Option<&fono_hotkey::HotkeyControlSender>,
) {
    let mut g = state.lock().expect("mcp_activity lock poisoned");
    let (depth, baseline) = &mut *g;
    if *depth == 0 {
        let snapshot = tray.map_or(TrayState::Idle, Tray::state);
        *baseline = snapshot;
        if let Some(t) = tray {
            t.set_state(TrayState::Processing);
        }
        // Grab the Escape key so the user can cancel the MCP listen
        // even though the FSM is Idle (no F7 dictation active).
        if let Some(ctrl) = cancel_ctrl {
            let _ = ctrl.send(fono_hotkey::HotkeyControl::EnableCancel);
        }
        info!(
            ?phase,
            prev_state = ?snapshot,
            "MCP activity started; tray flipped to amber",
        );
    } else {
        info!(?phase, depth = *depth, "MCP activity start (nested)");
    }
    *depth = depth.saturating_add(1);
}

/// Handle a `Request::McpActivityEnd`. Decrements the depth counter;
/// on →0 restores the tray baseline iff the tray is still showing
/// the amber state we last set. If another writer (FSM event
/// consumer, tray dispatcher) has moved the tray in the interim, we
/// leave it alone — last-writer-wins per the v7 design. Calls with
/// depth already 0 are ignored with a `debug` log; the IPC layer
/// has no way to know whether the matching Start was lost in
/// transit, so being lenient avoids spurious tray flips on noisy
/// links. Slice 7 of plan v7.
#[allow(clippy::significant_drop_tightening)]
fn handle_mcp_activity_end(
    state: &std::sync::Mutex<(u32, TrayState)>,
    tray: Option<&Tray>,
    cancel_ctrl: Option<&fono_hotkey::HotkeyControlSender>,
) {
    let mut g = state.lock().expect("mcp_activity lock poisoned");
    let (depth, baseline) = &mut *g;
    if *depth == 0 {
        debug!("MCP activity end received with depth=0; ignoring (unmatched Start?)");
        return;
    }
    *depth -= 1;
    if *depth == 0 {
        // Release the Escape grab now that no MCP listen is active.
        if let Some(ctrl) = cancel_ctrl {
            let _ = ctrl.send(fono_hotkey::HotkeyControl::DisableCancel);
        }
        if let Some(t) = tray {
            let current = t.state();
            if current == TrayState::Processing {
                t.set_state(*baseline);
                info!(restored_state = ?*baseline, "MCP activity ended; tray restored");
            } else {
                debug!(
                    current_state = ?current,
                    baseline = ?*baseline,
                    "MCP activity ended; another writer owns the tray — skipping restore",
                );
            }
        }
    }
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
            let cleaned = t.cleaned.as_deref().unwrap_or("(no polish)");
            format!(
                "raw    : {}\ncleaned: {}\nstt={}  polish={}",
                truncate(&t.raw, 240),
                truncate(cleaned, 240),
                t.stt_backend.as_deref().unwrap_or("?"),
                t.polish_backend.as_deref().unwrap_or("none"),
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

/// Surface a desktop notification when starting the recording
/// pipeline fails. The most common cause on a modern Linux desktop is
/// the PipeWire client tools not being installed; detect that and tell
/// the user to install them via their distro's package manager. For
/// any other failure, fall back to surfacing the underlying error text.
fn notify_recording_failure(err: &anyhow::Error) {
    let raw = format!("{err:#}");
    let body = if raw.contains("no usable capture tool")
        || raw.contains("pw-cat")
        || raw.contains("parec")
        || raw.contains("PulseAudio")
    {
        "No audio capture tool found. Install your distro's PipeWire \
         client tools (commonly `pipewire-bin` or `pipewire`)."
            .to_string()
    } else if raw.contains("audio capture") {
        format!(
            "Audio capture failed to start. Check your microphone input device.\n\nDetails: {raw}"
        )
    } else {
        format!("Recording could not start.\n\n{raw}")
    };
    fono_core::notify::send(
        "Fono — recording failed",
        &body,
        "microphone-sensitivity-muted",
        15_000,
        fono_core::notify::Urgency::Critical,
    );
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

/// Open a URL in the default browser. Same launcher fallbacks as
/// [`open_path`]; `explorer` and `open` both accept URLs. Also used by
/// `fono config web` (`crate::cli`).
pub(crate) fn open_url(url: &str) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    match std::process::Command::new(cmd).arg(url).spawn() {
        Ok(_) => info!("opened {url} via {cmd}"),
        Err(e) => warn!("failed to spawn {cmd} for {url}: {e:#}"),
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
        || format!("tcp://{}:{}", peer.hostname.trim_end_matches('.'), peer.port),
        |addr| match addr {
            std::net::IpAddr::V4(v4) => format!("tcp://{v4}:{}", peer.port),
            std::net::IpAddr::V6(v6) => format!("tcp://[{v6}]:{}", peer.port),
        },
    )
}

/// Render the tray TTS submenu label for a backend.
///
/// At v0.8 every backend in this submenu other than the local options
/// is a cloud provider, so the redundant `(cloud, ...)` prefix is
/// dropped from the suffix. Backends whose API key is missing are
/// rendered with the [`crate::tray_label::DISABLED_SENTINEL`] prefix
/// so the tray builder shows them greyed-out; the user clicks the
/// wizard / `fono keys add` flow to enable them. Falls back to the
/// canonical backend id when a backend has no catalogue entry
/// (None / Piper / Wyoming).
fn tts_menu_label(b: &fono_core::config::TtsBackend, secrets: &fono_core::Secrets) -> String {
    use fono_core::config::TtsBackend;
    let canonical = fono_core::providers::tts_backend_str(b);
    match b {
        TtsBackend::None => "Off (disabled)".to_string(),
        TtsBackend::Wyoming => "Wyoming (local)".to_string(),
        TtsBackend::Local => "Local voice".to_string(),
        TtsBackend::OpenAI
        | TtsBackend::Groq
        | TtsBackend::OpenRouter
        | TtsBackend::Cartesia
        | TtsBackend::Deepgram
        | TtsBackend::ElevenLabs
        | TtsBackend::Speechmatics
        | TtsBackend::Gemini => {
            let display = fono_core::provider_catalog::find(canonical)
                .map_or_else(|| canonical.to_string(), |p| p.display_name.to_string());
            let has_key = secrets.has_in_file(fono_core::providers::tts_key_env(b));
            if has_key {
                display
            } else {
                // Grey-out marker: the tray's submenu builder strips
                // this prefix and sets `enabled: false` on the item.
                format!("{}{display} (no API key)", fono_tray::DISABLED_SENTINEL)
            }
        }
    }
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
    Some(format!("{instance}.{}", fono_net::discovery::WYOMING_SERVICE_TYPE))
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
        warn!("tray UseDiscoveredStt({idx}): out of range (max={})", peers.len());
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

/// Switch the active polish backend from the tray submenu and trigger a
/// hot-reload of the orchestrator. Same code path as `fono use polish …`.
async fn switch_llm_via_tray(
    paths: &fono_core::Paths,
    orch: Option<&Arc<crate::session::SessionOrchestrator>>,
    backends: &[fono_core::config::PolishBackend],
    idx: u8,
) {
    let Some(backend) = backends.get(idx as usize) else {
        warn!("tray UsePolish({idx}): out of range (max={})", backends.len());
        return;
    };
    let label = fono_core::providers::polish_backend_str(backend);
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
            if matches!(backend, fono_core::config::PolishBackend::Local)
                && !ensure_local_polish_with_notify(paths).await
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

/// Switch the assistant chat backend from the tray's "Assistant
/// backend" submenu. Persists into `[assistant]` and triggers a
/// hot-reload so the change is live without a daemon restart.
async fn switch_assistant_via_tray(
    paths: &fono_core::Paths,
    orch: Option<&Arc<crate::session::SessionOrchestrator>>,
    backends: &[fono_core::config::AssistantBackend],
    idx: u8,
) {
    let Some(backend) = backends.get(idx as usize) else {
        warn!("tray UseAssistant({idx}): out of range (max={})", backends.len());
        return;
    };
    let label = fono_core::providers::assistant_backend_str(backend);
    let config_path = paths.config_file();
    let backend_clone = backend.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut cfg = fono_core::Config::load(&config_path)?;
        crate::cli::set_active_assistant(&mut cfg, backend_clone);
        cfg.save(&config_path)?;
        Ok(())
    })
    .await;
    match result {
        Ok(Ok(())) => {
            info!("tray: switched assistant to {label}");
            if let Some(o) = orch {
                if let Err(e) = o.reload().await {
                    warn!("tray: assistant reload failed: {e:#}");
                    fono_core::notify::send(
                        "Fono — assistant reload failed",
                        &format!("{e}"),
                        "dialog-error",
                        5_000,
                        fono_core::notify::Urgency::Critical,
                    );
                }
            }
        }
        Ok(Err(e)) => {
            warn!("tray: assistant switch failed: {e:#}");
            fono_core::notify::send(
                "Fono — assistant switch failed",
                &format!("{e}"),
                "dialog-error",
                5_000,
                fono_core::notify::Urgency::Critical,
            );
        }
        Err(e) => warn!("tray: assistant switch task join error: {e}"),
    }
}

/// Switch the TTS backend from the tray's "TTS backend" submenu.
/// For Wyoming, the existing `[tts.wyoming].uri` is preserved if set
/// (or the default wyoming-piper URI is filled in); for OpenAI /
/// Piper / None the sub-block is cleared so the factory picks up the
/// canonical env-var or a stub error.
async fn switch_tts_via_tray(
    paths: &fono_core::Paths,
    orch: Option<&Arc<crate::session::SessionOrchestrator>>,
    backends: &[fono_core::config::TtsBackend],
    idx: u8,
) {
    let Some(backend) = backends.get(idx as usize) else {
        warn!("tray UseTts({idx}): out of range (max={})", backends.len());
        return;
    };
    let label = fono_core::providers::tts_backend_str(backend);
    let config_path = paths.config_file();
    let backend_clone = backend.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut cfg = fono_core::Config::load(&config_path)?;
        crate::cli::set_active_tts(&mut cfg, backend_clone, None);
        cfg.save(&config_path)?;
        Ok(())
    })
    .await;
    match result {
        Ok(Ok(())) => {
            info!("tray: switched TTS to {label}");
            // Switching to the on-device backend with voices not yet
            // downloaded (e.g. the user started on a cloud backend, so
            // startup never fetched them) would reload onto a router
            // that can't load its voice and silently drop every reply.
            // Fetch first, notifying, and only reload once it's ready.
            #[cfg(feature = "tts-local")]
            if matches!(backend, fono_core::config::TtsBackend::Local)
                && !ensure_local_tts_with_notify(paths).await
            {
                return;
            }
            if let Some(o) = orch {
                if let Err(e) = o.reload().await {
                    warn!("tray: TTS reload failed: {e:#}");
                    fono_core::notify::send(
                        "Fono — TTS reload failed",
                        &format!("{e}"),
                        "dialog-error",
                        5_000,
                        fono_core::notify::Urgency::Critical,
                    );
                }
            }
        }
        Ok(Err(e)) => {
            warn!("tray: TTS switch failed: {e:#}");
            fono_core::notify::send(
                "Fono — TTS switch failed",
                &format!("{e}"),
                "dialog-error",
                5_000,
                fono_core::notify::Urgency::Critical,
            );
        }
        Err(e) => warn!("tray: TTS switch task join error: {e}"),
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
        warn!("tray SetInputDevice({idx}): out of range (max={})", devices.len());
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
        also_copy_to_clipboard: cfg.general.also_copy_to_clipboard,
        startup_autostart: cfg.general.startup_autostart,
        // Tray exposes VAD as a boolean. `"energy"` is the only enabled
        // backend today; any non-`"off"` value counts as "on".
        vad_enabled: !cfg.audio.vad_backend.eq_ignore_ascii_case("off"),
        wakeword_enabled: cfg.wakeword.enabled,
        wake_phrases: cfg
            .wakeword
            .phrases
            .iter()
            .map(|p| {
                let action = match p.target {
                    fono_core::config::WakeTarget::Dictation => "Dictation",
                    fono_core::config::WakeTarget::Assistant => "Assistant",
                };
                format!("\u{201c}{}\u{201d} \u{2192} {action}", prettify_wake_phrase(&p.model))
            })
            .collect(),
        auto_stop_silence_ms: cfg.audio.auto_stop_silence_ms,
        waveform_style,
        languages: cfg.general.languages.clone(),
    }
}

/// Prettify a wake-phrase model id for display, e.g. `"hey_jarvis"` →
/// `"Hey Jarvis"`. Splits on underscores and title-cases each word.
fn prettify_wake_phrase(model: &str) -> String {
    model
        .split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Map a `WaveformStyle` to its index in `fono_tray::WAVEFORM_STYLES`.
fn waveform_style_to_idx(style: fono_core::config::WaveformStyle) -> u8 {
    match style {
        fono_core::config::WaveformStyle::Bars => 0,
        fono_core::config::WaveformStyle::Oscilloscope => 1,
        fono_core::config::WaveformStyle::Fft => 2,
        fono_core::config::WaveformStyle::Heatmap => 3,
        fono_core::config::WaveformStyle::Transcript => 4,
        fono_core::config::WaveformStyle::Terrain3d => 5,
        fono_core::config::WaveformStyle::System360 => 6,
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
        4 => Some(fono_core::config::WaveformStyle::Transcript),
        5 => Some(fono_core::config::WaveformStyle::Terrain3d),
        6 => Some(fono_core::config::WaveformStyle::System360),
        _ => None,
    }
}

/// Map a tray `ToggleLanguage(idx)` index to the BCP-47 code in
/// `LANGUAGE_SHORTLIST`. Returns `None` on out-of-range so the
/// caller can `warn!` instead of silently no-oping.
fn language_code_from_idx(idx: u8) -> Option<&'static str> {
    fono_tray::LANGUAGE_SHORTLIST.get(idx as usize).map(|(code, _)| *code)
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
            "konsole" => Command::new(term).args(["-e", &exe_str, "settings"]).spawn(),
            // gnome-terminal needs `--` to terminate its option parser
            // before forwarding.
            "gnome-terminal" => Command::new(term).args(["--", &exe_str, "settings"]).spawn(),
            _ => Command::new(term).args(["-e", &exe_str, "settings"]).spawn(),
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
    let quantization = cfg.stt.local.quantization.clone();
    let size_hint = crate::models::local_stt_size_mb(&model, &quantization);
    let resolved_path = crate::models::resolve_local_stt(&model, &quantization)
        .ok()
        .flatten()
        .map(|(info, q)| crate::models::whisper_dest(paths, info.name, q));
    let dest_exists = resolved_path.as_ref().is_some_and(|p| p.exists());
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
    match crate::models::ensure_local_stt(paths, &model, &quantization).await {
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
async fn ensure_local_polish_with_notify(paths: &fono_core::Paths) -> bool {
    let cfg = match fono_core::Config::load(&paths.config_file()) {
        Ok(c) => c,
        Err(e) => {
            warn!("ensure_local_polish: config load failed: {e:#}");
            return false;
        }
    };
    let model = cfg.polish.local.model.clone();
    let size_hint = crate::models::local_llm_size_mb(&model);
    let dest_exists = paths.polish_models_dir().join(format!("{model}.gguf")).exists();
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
    match crate::models::ensure_local_polish(paths, &model).await {
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
            warn!("ensure_local_polish: download failed: {e:#}");
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

/// Local TTS counterpart to [`ensure_local_stt_with_notify`]. Ensures
/// the on-device Piper voice(s) the on-disk config requires are present,
/// downloading and notifying around any fetch. Returns `true` when the
/// voices are ready to load, `false` on failure — callers must NOT
/// reload the orchestrator on `false` (the local router would fail to
/// load and replies would be silent). Only compiled with `tts-local`;
/// without it, switching to Local surfaces a "not compiled in" error
/// from the factory on reload, so no ensure step is needed.
#[cfg(feature = "tts-local")]
async fn ensure_local_tts_with_notify(paths: &fono_core::Paths) -> bool {
    let cfg = match fono_core::Config::load(&paths.config_file()) {
        Ok(c) => c,
        Err(e) => {
            warn!("ensure_local_tts: config load failed: {e:#}");
            return false;
        }
    };
    if let Some(mb) = crate::models::local_tts_pending_mb(paths, &cfg) {
        fono_core::notify::send(
            "Fono — downloading voice",
            &format!("On-device TTS voice ({mb} MB)"),
            "emblem-downloads",
            4_000,
            fono_core::notify::Urgency::Normal,
        );
    }
    match crate::models::ensure_local_tts(paths, &cfg).await {
        Ok(crate::models::EnsureOutcome::Downloaded) => {
            fono_core::notify::send(
                "Fono — voice ready",
                "On-device TTS voice downloaded and cached",
                "emblem-default",
                4_000,
                fono_core::notify::Urgency::Normal,
            );
            true
        }
        Ok(_) => true,
        Err(e) => {
            warn!("ensure_local_tts: download failed: {e:#}");
            fono_core::notify::send(
                "Fono — voice download failed",
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
    let info_opt = cached.as_ref().and_then(fono_update::UpdateStatus::available).cloned();
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
        &format!("Fetching {} ({} MB)…", info.asset_name, info.asset_size / 1_048_576),
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

/// Shared, hot-reloadable handles for the LAN Wyoming server and its
/// mDNS advertiser. Cloned into the tray-action task so the "Wyoming
/// server" toggle can start/stop the listener in place — no daemon
/// restart. Held by the daemon for its lifetime; dropping the last
/// clone closes the listener and fires the mDNS goodbye.
#[derive(Clone)]
struct WyomingControl {
    rt: Arc<tokio::sync::Mutex<WyomingRuntime>>,
    /// The shared mDNS service daemon, when discovery came up. Needed
    /// to register/unregister the Wyoming advert on toggle.
    mdns: Option<mdns_sd::ServiceDaemon>,
    /// The discovery registry, so the local peer can be (un)seeded as
    /// the listener starts/stops.
    registry: Option<fono_net::discovery::Registry>,
}

/// The live handles owned by a [`WyomingControl`]. Both are `None` when
/// the listener is stopped.
#[derive(Default)]
struct WyomingRuntime {
    server: Option<fono_net::wyoming::server::WyomingServerHandle>,
    advert: Option<fono_net::discovery::advertiser::AdvertiserHandle>,
}

impl WyomingControl {
    /// Whether the LAN listener is currently running.
    async fn is_running(&self) -> bool {
        self.rt.lock().await.server.is_some()
    }

    /// Reconcile the running listener (and its mDNS advert) with
    /// `config.server.wyoming.enabled`, starting or stopping both in
    /// place. Idempotent: a no-op when already in the desired state.
    async fn reconcile(
        &self,
        config: &Config,
        paths: &Paths,
        orch: Option<&Arc<SessionOrchestrator>>,
    ) {
        let mut rt = self.rt.lock().await;
        let want = config.server.wyoming.enabled;
        let running = rt.server.is_some();
        if want && !running {
            let handle = spawn_wyoming_server_if_enabled(config, paths, orch).await;
            let started = handle.is_some();
            rt.server = handle;
            if started {
                if let Some(daemon) = &self.mdns {
                    if let Some((h, host)) = spawn_wyoming_advert(daemon, config) {
                        if let Some(reg) = &self.registry {
                            reg.upsert(local_wyoming_peer(config, &host, h.fullname()));
                        }
                        rt.advert = Some(h);
                    }
                }
            }
        } else if !want && running {
            // Dropping both handles shuts the listener and fires the
            // mDNS goodbye; in-flight connections finish naturally.
            rt.server = None;
            rt.advert = None;
        }
    }
}

/// Hot-reloadable control for the local LLM inference server (OpenAI +
/// Ollama HTTP API; ADR 0036). Mirrors [`WyomingControl`]: owns the live
/// listener + mDNS advert handles behind a mutex so the tray toggle can
/// start/stop the server in place without a daemon restart.
#[derive(Clone)]
struct LlmControl {
    rt: Arc<tokio::sync::Mutex<LlmRuntime>>,
    /// The shared mDNS service daemon, when discovery came up. Needed to
    /// register/unregister the `_ollama._tcp` advert on toggle.
    mdns: Option<mdns_sd::ServiceDaemon>,
    /// The discovery registry, so the local peer can be (un)seeded as the
    /// listener starts/stops.
    registry: Option<fono_net::discovery::Registry>,
}

/// The live handles owned by an [`LlmControl`]. Both are `None` when the
/// listener is stopped.
#[derive(Default)]
struct LlmRuntime {
    server: Option<fono_net::LlmServerHandle>,
    advert: Option<fono_net::discovery::advertiser::AdvertiserHandle>,
}

impl LlmControl {
    /// Whether the LLM listener is currently running.
    async fn is_running(&self) -> bool {
        self.rt.lock().await.server.is_some()
    }

    /// Reconcile the running listener (and its mDNS advert) with
    /// `config.server.llm.enabled`, starting or stopping both in place.
    /// Idempotent: a no-op when already in the desired state.
    async fn reconcile(&self, config: &Config, orch: Option<&Arc<SessionOrchestrator>>) {
        let mut rt = self.rt.lock().await;
        let want = config.server.llm.enabled;
        let running = rt.server.is_some();
        if want && !running {
            let handle = spawn_llm_server_if_enabled(config, orch).await;
            let started = handle.is_some();
            rt.server = handle;
            if started {
                if let Some(daemon) = &self.mdns {
                    if let Some((h, host)) = spawn_llm_advert(daemon, config) {
                        if let Some(reg) = &self.registry {
                            reg.upsert(local_llm_peer(config, &host, h.fullname()));
                        }
                        rt.advert = Some(h);
                    }
                }
            }
        } else if !want && running {
            // Dropping both handles shuts the listener and fires the
            // mDNS goodbye; in-flight connections finish naturally.
            rt.server = None;
            rt.advert = None;
        }
    }
}

/// Handle a tray `ToggleWyomingServer` action: flip
/// `[server.wyoming].enabled`, persist, hot-reload the listener in place,
/// and notify. Factored out (and `Box::pin`-ned at the call site) to keep
/// the tray-dispatch async block's stack frame under clippy's
/// `large_stack_frames` threshold. The one switch serves STT always and
/// TTS automatically whenever a `[tts]` backend is configured.
async fn toggle_wyoming_server_via_tray(
    paths: &Paths,
    wyoming_ctl: &WyomingControl,
    orch: Option<&Arc<SessionOrchestrator>>,
) {
    match fono_core::Config::load(&paths.config_file()) {
        Ok(mut cfg) => {
            cfg.server.wyoming.enabled = !cfg.server.wyoming.enabled;
            let new_state = cfg.server.wyoming.enabled;
            if let Err(e) = cfg.save(&paths.config_file()) {
                warn!("tray ToggleWyomingServer: save failed: {e:#}");
                return;
            }
            wyoming_ctl.reconcile(&cfg, paths, orch).await;
            let running = wyoming_ctl.is_running().await;
            info!(enabled = new_state, running, "Wyoming server toggled via tray (hot-reloaded)");
            let body = if new_state && running {
                "Wyoming server is live on the LAN — sharing speech-to-text, plus \
                 text-to-speech when a voice backend is configured."
            } else if new_state {
                "Wyoming server enabled, but the listener could not start — check the logs \
                 (no STT backend, or the port is busy)."
            } else {
                "Wyoming server stopped — Fono is no longer shared on the LAN."
            };
            fono_core::notify::send(
                "Fono — Wyoming server",
                body,
                "network-server",
                6_000,
                fono_core::notify::Urgency::Normal,
            );
        }
        Err(e) => warn!("tray ToggleWyomingServer: load config failed: {e:#}"),
    }
}

/// Handle a tray `ToggleLlmServer` action: flip `[server.llm].enabled`,
/// persist, hot-reload the listener in place, and notify. Factored out of
/// the tray-dispatch closure to keep that async block's stack frame under
/// clippy's `large_stack_frames` threshold (the closure already handles
/// every other tray action inline).
async fn toggle_llm_server_via_tray(
    paths: &Paths,
    llm_ctl: &LlmControl,
    orch: Option<&Arc<SessionOrchestrator>>,
) {
    match fono_core::Config::load(&paths.config_file()) {
        Ok(mut cfg) => {
            cfg.server.llm.enabled = !cfg.server.llm.enabled;
            let new_state = cfg.server.llm.enabled;
            if let Err(e) = cfg.save(&paths.config_file()) {
                warn!("tray ToggleLlmServer: save failed: {e:#}");
                return;
            }
            llm_ctl.reconcile(&cfg, orch).await;
            let running = llm_ctl.is_running().await;
            info!(enabled = new_state, running, "LLM server toggled via tray (hot-reloaded)");
            let body = if new_state && running {
                "Local LLM server is live on the LAN — sharing a text assistant model \
                 over the OpenAI and Ollama APIs."
            } else if new_state && orch.is_some_and(|o| o.assistant_is_realtime_only()) {
                "LLM server enabled, but the assistant is a realtime (speech-to-speech) \
                 model and the same-provider text fallback couldn't start — add the \
                 provider API key with `fono keys add`, or set [server.llm].model."
            } else if new_state {
                "LLM server enabled, but the listener could not start — check the logs (no \
                 [assistant] backend configured, or the port is busy)."
            } else {
                "LLM server stopped — Fono is no longer shared on the LAN."
            };
            fono_core::notify::send(
                "Fono — LLM server",
                body,
                "network-server",
                6_000,
                fono_core::notify::Urgency::Normal,
            );
        }
        Err(e) => warn!("tray ToggleLlmServer: load config failed: {e:#}"),
    }
}

/// Voices advertised in `info.tts[].voices`, derived from the active
/// `[tts]` backend. Local (on-device) backends advertise the whole
/// embedded catalog so Home Assistant can pick any installed voice at
/// no cost. Cloud backends advertise *none*: every `synthesize` request
/// then synthesises with the daemon's single configured `[tts].voice`,
/// so a LAN peer can't run up cloud bills by voice-shopping (option B
/// of the design discussion).
fn wyoming_tts_voices(config: &Config) -> Vec<fono_net::wyoming::server::AdvertisedVoice> {
    #[cfg(feature = "tts-local")]
    use fono_core::config::TtsBackend;
    match config.tts.backend {
        #[cfg(feature = "tts-local")]
        TtsBackend::Local => fono_tts::voices::catalog()
            .unwrap_or_default()
            .into_iter()
            .map(|v| fono_net::wyoming::server::AdvertisedVoice {
                name: v.name,
                languages: vec![v.language],
                description: None,
                version: None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Wake-word models advertised in `info.wake[].models`, derived from the
/// active `[wakeword].phrases`. Falls back to the default "hey fono"
/// model when the feature is enabled but no explicit phrases are listed,
/// so the wake service always advertises at least one model. The spoken
/// `phrase` is a best-effort readable form of the model id.
fn wyoming_wake_models(config: &Config) -> Vec<fono_net::wyoming::server::AdvertisedWakeModel> {
    // Mirror exactly what the detector will run (crate::wake::effective_wake
    // _phrases): the configured phrases, or the runtime default model when
    // none are configured. Keeps the advertised `info.wake` list in lockstep
    // with the bound detector.
    crate::wake::effective_wake_phrases(&config.wakeword)
        .into_iter()
        .map(|p| {
            let model = p.model;
            let phrase = model.replace('_', " ");
            fono_net::wyoming::server::AdvertisedWakeModel {
                name: model,
                languages: config.general.languages.clone(),
                phrase: Some(phrase),
                description: Some("fono local wake-word detector".to_string()),
                version: None,
            }
        })
        .collect()
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
    paths: &Paths,
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
    let auth_token =
        if cfg.auth_token_ref.is_empty() { None } else { std::env::var(&cfg.auth_token_ref).ok() };
    let model = config.stt.local.model.clone();
    // TTS rides this same listener and is served automatically whenever
    // a `[tts]` backend is configured (the orchestrator hands out a
    // snapshot only then). Advertised voices are derived from that
    // backend; see `wyoming_tts_voices`.
    let serve_tts = orch.tts_snapshot().is_some();
    let tts_voices = if serve_tts { wyoming_tts_voices(config) } else { Vec::new() };
    let advertised_voice_count = tts_voices.len();
    // Wake-word SERVER direction: expose Fono's local detector over the
    // Wyoming `Detection` protocol automatically, exactly like STT (always)
    // and TTS (whenever a backend is configured). Capability is a build-time
    // fact — a fetchable default model always exists — so any binary that can
    // do wake serves it the moment the Wyoming server is up, independent of
    // the local always-on listener (`[wakeword].enabled`). Audio stays on the
    // machine: the server *is* the detector. The privacy-breaking CLIENT
    // direction (`[wakeword].wyoming` with a uri) is unrelated and never
    // advertises a wake service here.
    let serve_wake = crate::wake::detection_available();
    let wake_models = if serve_wake { wyoming_wake_models(config) } else { Vec::new() };
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
        tts_voices,
        wake_models,
    };

    let orch_for_provider = Arc::clone(orch);
    let provider: fono_net::wyoming::server::SttProvider =
        Arc::new(move || orch_for_provider.stt_snapshot());
    let mut server = fono_net::wyoming::server::WyomingServer::new(server_cfg, provider);
    if serve_tts {
        if let Some(initial) = orch.tts_snapshot() {
            let orch_tts = Arc::clone(orch);
            let tts_provider: fono_net::wyoming::server::TtsProvider =
                Arc::new(move || orch_tts.tts_snapshot().unwrap_or_else(|| Arc::clone(&initial)));
            server = server.with_tts(tts_provider);
            info!(
                "Wyoming server: TTS serving enabled ({advertised_voice_count} advertised \
                 voice(s))"
            );
        }
    } else {
        info!("Wyoming server: no [tts] backend configured; serving STT only");
    }
    if serve_wake {
        // Bind a wake provider that builds the *same* detector as the local
        // listener (Phase C/D) per connection, so the LAN wake service runs
        // on-device. fono-net's `WakeProvider` is invoked once per accepted
        // connection (wake sessions are stateful).
        // Build from the *effective* config so a fresh install with no
        // `[wakeword].phrases` still serves the runtime default model.
        let wake_cfg = crate::wake::effective_wake_config(&config.wakeword);
        let wake_paths = paths.clone();
        // Ensure the served models' `.ort` files are cached even when the
        // local always-on listener is disabled (it owns the other fetch
        // path). Per-connection detectors built before the fetch completes
        // fall back to the stub and recover on the next connection.
        for phrase in &wake_cfg.phrases {
            let id = phrase.model.clone();
            let cache_dir = wake_paths.cache_dir.clone();
            if fono_audio::wake_registry::resolved_paths(&id, &cache_dir).is_some_and(|r| {
                !(r.melspec.exists() && r.embedding.exists() && r.classifier.exists())
            }) {
                tokio::spawn(async move {
                    match fono_audio::wake_registry::fetch_model(&id, &cache_dir, None).await {
                        Ok(_) => info!("Wyoming server: fetched wake model '{id}'"),
                        Err(e) => warn!("Wyoming server: wake model '{id}' fetch failed: {e:#}"),
                    }
                });
            }
        }
        let wake_provider: fono_net::wyoming::server::WakeProvider =
            Arc::new(move || crate::wake::build_detector(&wake_cfg, &wake_paths));
        server = server.with_wake(wake_provider);
        info!("Wyoming server: wake-word detection service enabled (audio stays local)");
    }
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

/// The model id surfaced by the LLM server's `/v1/models` + `/api/tags`.
/// Cosmetic: the server drives whichever assistant the orchestrator
/// hands out regardless of the `model` field a client sends. Accounts
/// for the `[server.llm].model` override and the realtime→text-sibling
/// fallback (ADR 0036) — so a Gemini Live primary advertises the
/// `gemini-flash-lite-latest` text model it actually serves — falling
/// back to `"fono"` when nothing resolves.
fn llm_model_name(config: &Config) -> String {
    let m = config.server.llm.model.trim();
    let override_model = (!m.is_empty()).then_some(m);
    let name = fono_assistant::server_assistant_model_name(&config.assistant, override_model);
    if name.is_empty() {
        "fono".to_string()
    } else {
        name
    }
}

/// Spawn the local LLM inference server if `[server.llm].enabled = true`
/// and an assistant backend is configured. Returns `None` when the
/// server is disabled, no assistant is available, or the listener fails
/// to bind (failures are logged at `warn!` and never abort the daemon —
/// dictation must keep working even if the LLM server can't come up).
/// Mirrors [`spawn_wyoming_server_if_enabled`]. See ADR 0036.
async fn spawn_llm_server_if_enabled(
    config: &Config,
    orchestrator: Option<&Arc<SessionOrchestrator>>,
) -> Option<fono_net::LlmServerHandle> {
    let cfg = &config.server.llm;
    if !cfg.enabled {
        return None;
    }
    let Some(orch) = orchestrator else {
        warn!(
            "[server.llm].enabled = true but the daemon is in degraded mode \
             (no STT backend); skipping LLM server"
        );
        return None;
    };
    if orch.server_assistant_snapshot().is_none() {
        if orch.assistant_is_realtime_only() {
            warn!(
                "[server.llm].enabled = true and the active [assistant] is a realtime \
                 speech-to-speech model (e.g. Gemini Live), but the same-provider text \
                 fallback could not be built — most likely a missing API key. Add the \
                 provider key with `fono keys add`, or set [server.llm].model to a staged \
                 text model; skipping LLM server"
            );
        } else {
            warn!(
                "[server.llm].enabled = true but no [assistant] backend is configured; \
                 skipping LLM server (set [assistant].backend)"
            );
        }
        return None;
    }

    let loopback_only = cfg.bind == "127.0.0.1" || cfg.bind == "::1";
    let auth_token =
        if cfg.auth_token_ref.is_empty() { None } else { std::env::var(&cfg.auth_token_ref).ok() };
    let server_cfg = fono_net::LlmServerConfig {
        bind: cfg.bind.clone(),
        port: cfg.port,
        auth_token,
        model_name: llm_model_name(config),
        server_version: env!("CARGO_PKG_VERSION").to_string(),
        loopback_only,
    };

    // Provider closure invoked per request so `Reload`-driven assistant
    // backend swaps are tracked without restarting the listener.
    let orch_for_provider = Arc::clone(orch);
    let provider: fono_net::AssistantProvider =
        Arc::new(move || orch_for_provider.server_assistant_snapshot());
    // Cloud pass-through upstream (ADR 0036): when the served backend is
    // an OpenAI-compatible cloud provider the OpenAI surface forwards
    // requests verbatim for full tool/vision/parameter fidelity. `None`
    // for non-proxyable backends, where the assistant adapter drives.
    let orch_for_upstream = Arc::clone(orch);
    let upstream: fono_net::UpstreamProvider =
        Arc::new(move || orch_for_upstream.server_upstream_snapshot());
    let model = server_cfg.model_name.clone();
    match fono_net::LlmServer::new(server_cfg, provider).with_upstream(upstream).start().await {
        Ok(handle) => {
            info!(
                "LLM server listening on {} (model={model}, loopback_only={loopback_only}); \
                 OpenAI + Ollama API",
                handle.local_addr()
            );
            Some(handle)
        }
        Err(e) => {
            warn!("LLM server failed to start: {e:#}");
            None
        }
    }
}

/// Shared slot holding the (lazily started) web settings server handle.
/// The tray's "Settings…" entry starts the listener on demand.
type WebSettingsSlot = Arc<tokio::sync::Mutex<Option<fono_net::WebSettingsHandle>>>;

/// Spawn the web settings UI server if `[server.web].enabled = true`.
/// Returns `None` when disabled or when the listener fails to bind
/// (failures are logged at `warn!` and never abort the daemon).
/// Mirrors [`spawn_llm_server_if_enabled`]; see
/// `plans/2026-07-02-web-config-ui-v2.md`.
///
/// The hook closures re-read config/secrets from disk on every request
/// (disk is the source of truth, same as the tray preference paths) and
/// route saves through `Config::save` → `SessionOrchestrator::reload` →
/// `WakeHandle::reload`, so a browser save behaves exactly like
/// `fono use …` + `fono reload`.
async fn spawn_web_settings_if_enabled(
    config: &Config,
    paths: &Paths,
    secrets: &Secrets,
    orchestrator: Option<&Arc<SessionOrchestrator>>,
    wake: &crate::wake::WakeHandle,
) -> Option<fono_net::WebSettingsHandle> {
    if !config.server.web.enabled {
        return None;
    }
    spawn_web_settings(config, paths, secrets, orchestrator, wake).await
}

/// Tray "Settings…" handler: lazy-start the web settings listener when
/// it isn't running (persisting `server.web.enabled = true` so it
/// survives restarts), then open the page in the default browser.
async fn open_settings_web_via_tray(
    paths: &Paths,
    slot: &WebSettingsSlot,
    orchestrator: Option<&Arc<SessionOrchestrator>>,
    wake: &crate::wake::WakeHandle,
) {
    let mut guard = slot.lock().await;
    if guard.is_none() {
        let cfg = match Config::load(&paths.config_file()) {
            Ok(mut cfg) => {
                if !cfg.server.web.enabled {
                    cfg.server.web.enabled = true;
                    if let Err(e) = cfg.save(&paths.config_file()) {
                        warn!("tray OpenSettingsWeb: save failed: {e:#}");
                    }
                }
                cfg
            }
            Err(e) => {
                warn!("tray OpenSettingsWeb: load config failed: {e:#}");
                return;
            }
        };
        let secrets = Secrets::load(&paths.secrets_file()).unwrap_or_default();
        *guard = spawn_web_settings(&cfg, paths, &secrets, orchestrator, wake).await;
    }
    if let Some(handle) = guard.as_ref() {
        open_url(&web_settings_url(&handle.local_addr()));
    } else {
        fono_core::notify::send(
            "Fono — settings page failed to start",
            "The web settings listener could not start. Check the log \
             (`fono doctor -f`) and `[server.web]` in config.toml.",
            "dialog-error",
            8_000,
            fono_core::notify::Urgency::Critical,
        );
    }
}

/// Browser URL for a web-settings listener address. Unspecified binds
/// (`0.0.0.0` / `::`) aren't browsable — substitute loopback.
fn web_settings_url(addr: &std::net::SocketAddr) -> String {
    if addr.ip().is_unspecified() {
        format!("http://127.0.0.1:{}/", addr.port())
    } else {
        format!("http://{addr}/")
    }
}

/// Start the web settings server unconditionally (caller has already
/// decided it should run).
async fn spawn_web_settings(
    config: &Config,
    paths: &Paths,
    secrets: &Secrets,
    orchestrator: Option<&Arc<SessionOrchestrator>>,
    wake: &crate::wake::WakeHandle,
) -> Option<fono_net::WebSettingsHandle> {
    let cfg = &config.server.web;
    let loopback_only = cfg.bind == "127.0.0.1" || cfg.bind == "::1";
    let auth_token =
        if cfg.auth_token_ref.is_empty() { None } else { secrets.resolve(&cfg.auth_token_ref) };
    if !loopback_only && auth_token.is_none() {
        warn!(
            "[server.web] is bound beyond loopback ({}) without a resolvable auth_token_ref — \
             anyone on the network can rewrite the config; set [server.web].auth_token_ref",
            cfg.bind
        );
    }
    let server_cfg = fono_net::WebSettingsConfig {
        bind: cfg.bind.clone(),
        port: cfg.port,
        auth_token,
        loopback_only,
    };
    let hooks = web_settings_hooks(paths, orchestrator, wake);
    match fono_net::WebSettingsServer::new(server_cfg, hooks).start().await {
        Ok(handle) => {
            info!(
                "web settings UI listening on http://{} (loopback_only={loopback_only})",
                handle.local_addr()
            );
            Some(handle)
        }
        Err(e) => {
            warn!("web settings server failed to start: {e:#}");
            None
        }
    }
}

/// Build the daemon-side hook closures for the web settings server:
/// config read/write, write-only secret updates, and page metadata.
/// Split out of [`spawn_web_settings`] to keep both under clippy's
/// `too_many_lines`.
fn web_settings_hooks(
    paths: &Paths,
    orchestrator: Option<&Arc<SessionOrchestrator>>,
    wake: &crate::wake::WakeHandle,
) -> fono_net::WebSettingsHooks {
    let config_path = paths.config_file();
    let secrets_path = paths.secrets_file();

    let cp = config_path.clone();
    let get_config: fono_net::web_settings::GetConfigFn = Arc::new(move || {
        let cfg = Config::load(&cp).map_err(|e| format!("load config: {e}"))?;
        serde_json::to_value(&cfg).map_err(|e| format!("serialize config: {e}"))
    });

    let cp = config_path.clone();
    let orch = orchestrator.map(Arc::clone);
    let wake_put = wake.clone();
    let put_config: fono_net::web_settings::PutConfigFn = Arc::new(move |value| {
        let cp = cp.clone();
        let orch = orch.clone();
        let wake = wake_put.clone();
        Box::pin(async move {
            let mut new_cfg: Config =
                serde_json::from_value(value).map_err(|e| format!("invalid config: {e}"))?;
            new_cfg.migrate().map_err(|e| format!("config migration: {e}"))?;
            new_cfg.save(&cp).map_err(|e| format!("save config: {e}"))?;
            info!("web settings: config saved via browser UI");
            match orch.as_ref() {
                Some(o) => match o.reload().await {
                    Ok(summary) => {
                        wake.reload();
                        Ok(summary)
                    }
                    Err(e) => {
                        Ok(format!("saved, but hot-reload failed: {e:#} (restart the daemon)"))
                    }
                },
                None => Ok("saved; daemon is in degraded mode (restart to apply)".to_string()),
            }
        })
    });

    let sp = secrets_path.clone();
    let orch = orchestrator.map(Arc::clone);
    let wake_secret = wake.clone();
    let set_secret: fono_net::web_settings::SetSecretFn = Arc::new(move |name, value| {
        let mut s = Secrets::load(&sp).map_err(|e| format!("load secrets: {e}"))?;
        if value.is_empty() {
            s.keys.remove(name);
        } else {
            s.insert(name, value);
        }
        s.save(&sp).map_err(|e| format!("save secrets: {e}"))?;
        info!(
            "web settings: secret {name} {} via browser UI",
            if value.is_empty() { "cleared" } else { "updated" }
        );
        // Background reload so a freshly added key takes effect without a
        // separate save. Best-effort — the secret itself is already on disk.
        if let Some(o) = orch.clone() {
            let wake = wake_secret.clone();
            tokio::spawn(async move {
                match o.reload().await {
                    Ok(_) => wake.reload(),
                    Err(e) => warn!("web settings: reload after secret change failed: {e:#}"),
                }
            });
        }
        Ok(())
    });

    let cp = config_path;
    let meta: fono_net::web_settings::MetaFn = Arc::new(move || {
        use fono_core::providers as p;
        let mut names = std::collections::BTreeSet::new();
        for b in p::all_stt_backends() {
            names.insert(p::stt_key_env(&b));
        }
        for b in p::all_polish_backends() {
            names.insert(p::polish_key_env(&b));
        }
        for b in p::all_assistant_backends() {
            names.insert(p::assistant_key_env(&b));
        }
        for b in p::all_tts_backends() {
            names.insert(p::tts_key_env(&b));
        }
        names.remove("");
        let secrets = Secrets::load(&secrets_path).unwrap_or_default();
        let statuses: serde_json::Map<String, serde_json::Value> = names
            .into_iter()
            .map(|n| (n.to_string(), serde_json::Value::Bool(secrets.has_in_file(n))))
            .collect();
        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "config_path": cp.display().to_string(),
            "secrets": statuses,
            "defaults": {
                "polish_prompt_main": fono_core::config::default_prompt_main(),
                "polish_prompt_advanced": fono_core::config::default_prompt_advanced(),
                "assistant_prompt": fono_core::config::default_assistant_prompt(),
            },
        })
    });

    fono_net::WebSettingsHooks { get_config, put_config, set_secret, meta }
}

/// Capability tags advertised over mDNS for the LLM service. Both wire
/// formats ride the one listener, so both are advertised.
fn llm_caps() -> Vec<String> {
    vec!["openai".to_string(), "ollama".to_string()]
}

/// Register the mDNS `_ollama._tcp` advert for the LLM server. Mirrors
/// [`spawn_wyoming_advert`]; returns the handle plus the short hostname
/// used to seed the local registry entry.
fn spawn_llm_advert(
    daemon: &mdns_sd::ServiceDaemon,
    config: &Config,
) -> Option<(fono_net::discovery::advertiser::AdvertiserHandle, String)> {
    let cfg = &config.server.llm;
    let Some(host) = hostname() else {
        warn!("mdns: cannot determine hostname; skipping LLM advertise");
        return None;
    };
    let instance = if config.network.instance_name.is_empty() {
        format!("fono-{host}")
    } else {
        config.network.instance_name.clone()
    };
    let mdns_host = if host.contains('.') { host.clone() } else { format!("{host}.local") };
    let spec = fono_net::discovery::advertiser::AdvertiseSpec {
        kind: fono_net::discovery::PeerKind::Ollama,
        instance_name: instance,
        hostname: mdns_host,
        port: cfg.port,
        addresses: vec![],
        proto: "ollama/1".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        caps: llm_caps(),
        model: Some(llm_model_name(config)),
        auth_required: !cfg.auth_token_ref.is_empty(),
        path: None,
    };
    let advertiser = fono_net::discovery::Advertiser::new(daemon.clone());
    match advertiser.register(spec) {
        Ok(h) => {
            info!("mDNS advertising _ollama._tcp on port {} as {}", cfg.port, h.fullname());
            Some((h, host))
        }
        Err(e) => {
            warn!("mdns llm advertise failed: {e:#}");
            None
        }
    }
}

/// Build the [`fono_net::discovery::DiscoveredPeer`] for the
/// locally-running LLM service so `fono discover` shows it immediately.
/// Mirrors [`local_wyoming_peer`].
fn local_llm_peer(
    config: &Config,
    short_host: &str,
    fullname: &str,
) -> fono_net::discovery::DiscoveredPeer {
    use fono_net::discovery::{DiscoveredPeer, PeerKind, OLLAMA_SERVICE_TYPE};
    use std::time::Instant;
    let hostname = format!("{short_host}.local.");
    let name = fullname
        .strip_suffix(OLLAMA_SERVICE_TYPE)
        .and_then(|s| s.strip_suffix('.'))
        .unwrap_or(fullname)
        .to_string();
    DiscoveredPeer {
        kind: PeerKind::Ollama,
        fullname: fullname.to_string(),
        name,
        hostname,
        address: None,
        port: config.server.llm.port,
        proto: "ollama/1".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        caps: llm_caps(),
        model: Some(llm_model_name(config)),
        auth_required: !config.server.llm.auth_token_ref.is_empty(),
        path: None,
        last_seen: Instant::now(),
    }
}

/// Live discovery runtime: shared registry, browser, and (optional)
/// advertisers. Held by the daemon for its lifetime so the goodbye
/// packets fire when it exits.
struct DiscoveryRuntime {
    registry: fono_net::discovery::Registry,
    /// The shared mDNS service daemon, cloned into [`WyomingControl`] so
    /// the Wyoming advert can be (un)registered live on a tray toggle.
    daemon: Option<mdns_sd::ServiceDaemon>,
    _browser: Option<fono_net::discovery::browser::BrowserHandle>,
}

/// Spawn the always-on mDNS browser. The Wyoming advertiser is *not*
/// started here — it is owned by [`WyomingControl`] alongside the LAN
/// listener so both can be toggled live from the tray. All failure
/// paths log and continue — discovery is a convenience layer, not a
/// hard dependency. Slice 4 of the network plan.
async fn spawn_discovery_if_enabled() -> Option<DiscoveryRuntime> {
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
        // Fono-native discovery is reserved for the future WebSocket protocol;
        // until that server is implemented, only browse Wyoming to avoid
        // duplicate mDNS query churn and duplicate verbose logs.
        match b.start(&[fono_net::discovery::PeerKind::Wyoming]) {
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

    Some(DiscoveryRuntime { registry, daemon: Some(daemon), _browser: browser })
}

/// Capability tags advertised over mDNS for the Wyoming service.
/// `stt` is always served; `tts` is added whenever a `[tts]` backend is
/// configured, and `wake` is added whenever this build can do wake
/// detection (a fetchable default model always exists), exactly mirroring
/// how STT/TTS are advertised. Home Assistant discovers this daemon as an
/// STT / TTS / wake provider as appropriate. The served wake detector is
/// local (audio stays on the machine); the opt-in client direction
/// (`[wakeword].wyoming` with a uri) is unrelated and never advertised.
fn wyoming_caps(config: &Config) -> Vec<String> {
    let mut caps = vec!["stt".to_string()];
    if config.tts.backend != fono_core::config::TtsBackend::None {
        caps.push("tts".to_string());
    }
    if crate::wake::detection_available() {
        caps.push("wake".to_string());
    }
    caps
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
        caps: wyoming_caps(config),
        model: Some(config.stt.local.model.clone()),
        auth_required: !cfg.auth_token_ref.is_empty(),
        path: None,
    };
    let advertiser = fono_net::discovery::Advertiser::new(daemon.clone());
    match advertiser.register(spec) {
        Ok(h) => {
            info!("mDNS advertising _wyoming._tcp on port {} as {}", cfg.port, h.fullname());
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
        caps: wyoming_caps(config),
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
                fono_net::discovery::PeerKind::Ollama => "ollama".into(),
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

    #[test]
    fn prettify_wake_phrase_title_cases_underscored_ids() {
        assert_eq!(prettify_wake_phrase("hey_jarvis"), "Hey Jarvis");
        assert_eq!(prettify_wake_phrase("hey_fono"), "Hey Fono");
        assert_eq!(prettify_wake_phrase("alexa"), "Alexa");
        assert_eq!(prettify_wake_phrase("hey_mycroft"), "Hey Mycroft");
        assert_eq!(prettify_wake_phrase(""), "");
    }

    #[test]
    fn wyoming_caps_stt_only_by_default() {
        // STT is always advertised; TTS only with a backend; wake only when
        // this build can do detection. Filter the capability-gated wake cap so
        // the assertion holds for both slim and wake-enabled builds.
        let cfg = Config::default();
        let caps: Vec<String> = wyoming_caps(&cfg).into_iter().filter(|c| c != "wake").collect();
        assert_eq!(caps, vec!["stt".to_string()]);
    }

    #[test]
    fn wyoming_caps_adds_tts_when_backend_configured() {
        let mut cfg = Config::default();
        cfg.tts.backend = fono_core::config::TtsBackend::OpenAI;
        let caps: Vec<String> = wyoming_caps(&cfg).into_iter().filter(|c| c != "wake").collect();
        assert_eq!(caps, vec!["stt".to_string(), "tts".to_string()]);
    }

    #[test]
    fn wyoming_caps_advertises_wake_by_capability_not_config() {
        // Wake is now served automatically (like STT/TTS), gated purely on the
        // build capability — not on any `[wakeword]` config switch. The cap is
        // present exactly when this binary can do wake detection.
        let cfg = Config::default();
        let caps = wyoming_caps(&cfg);
        assert!(caps.contains(&"stt".to_string()), "stt is always advertised");
        assert_eq!(
            caps.contains(&"wake".to_string()),
            crate::wake::detection_available(),
            "wake cap iff the binary can do wake detection"
        );
    }

    /// Slim build: translation never fires regardless of config.
    #[cfg(not(feature = "interactive"))]
    #[test]
    fn translate_passthrough_when_feature_off() {
        let mut cfg = Config::default();
        cfg.overlay.style = fono_core::config::WaveformStyle::Transcript;
        let live = cfg.live_preview();
        assert_eq!(
            translate_for_live_preview(HotkeyAction::HoldPressed, live),
            HotkeyAction::HoldPressed
        );
        assert_eq!(
            translate_for_live_preview(HotkeyAction::TogglePressed, live),
            HotkeyAction::TogglePressed
        );
    }

    /// `interactive` feature on, Transcript style picked, and an
    /// orchestrator is present → Hold/Toggle become their `Live*`
    /// variants. Cancel and processing actions pass through.
    #[cfg(feature = "interactive")]
    #[test]
    fn translate_hold_toggle_to_live_when_enabled() {
        let mut cfg = Config::default();
        cfg.overlay.style = fono_core::config::WaveformStyle::Transcript;
        let live = cfg.live_preview();
        assert_eq!(
            translate_for_live_preview(HotkeyAction::HoldPressed, live),
            HotkeyAction::LiveHoldPressed
        );
        assert_eq!(
            translate_for_live_preview(HotkeyAction::HoldReleased, live),
            HotkeyAction::LiveHoldReleased
        );
        assert_eq!(
            translate_for_live_preview(HotkeyAction::TogglePressed, live),
            HotkeyAction::LiveTogglePressed
        );
        // Cancel / Processing variants always pass through.
        assert_eq!(
            translate_for_live_preview(HotkeyAction::CancelPressed, live),
            HotkeyAction::CancelPressed
        );
        assert_eq!(
            translate_for_live_preview(HotkeyAction::ProcessingDone, live),
            HotkeyAction::ProcessingDone
        );
        assert_eq!(
            translate_for_live_preview(HotkeyAction::ProcessingStarted, live),
            HotkeyAction::ProcessingStarted
        );
    }

    /// Non-Transcript style → no translation even with the feature on.
    #[cfg(feature = "interactive")]
    #[test]
    fn translate_passthrough_when_disabled() {
        let cfg = Config::default(); // overlay.style defaults to Fft (not Transcript)
        assert_eq!(
            translate_for_live_preview(HotkeyAction::HoldPressed, cfg.live_preview()),
            HotkeyAction::HoldPressed
        );
    }

    /// Degraded mode (no orchestrator) → translation is suppressed
    /// because the FSM's `LiveDictating` state has no driver.
    #[cfg(feature = "interactive")]
    #[test]
    fn translate_passthrough_in_degraded_mode() {
        let mut cfg = Config::default();
        cfg.overlay.style = fono_core::config::WaveformStyle::Transcript;
        // Degraded mode = no orchestrator, so the runtime caller
        // passes `false` regardless of the config value.
        let _ = cfg.live_preview();
        assert_eq!(
            translate_for_live_preview(HotkeyAction::HoldPressed, false),
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

    #[test]
    fn mcp_activity_nested_start_end_holds_processing() {
        let tray = Tray::for_tests();
        // Pretend the tray was painting Recording (e.g. an F7 dictation
        // already running) when the MCP activity starts.
        tray.set_state(TrayState::Recording);
        let state = std::sync::Mutex::new((0u32, TrayState::Idle));

        // Outer start: snapshot Recording, flip to Processing.
        handle_mcp_activity_start(&state, Some(&tray), McpPhase::Listening, None);
        assert_eq!(tray.state(), TrayState::Processing);
        assert_eq!(state.lock().unwrap().0, 1);
        assert_eq!(state.lock().unwrap().1, TrayState::Recording);

        // Nested start: depth bumps, tray stays amber, baseline unchanged.
        handle_mcp_activity_start(&state, Some(&tray), McpPhase::Speaking, None);
        assert_eq!(tray.state(), TrayState::Processing);
        assert_eq!(state.lock().unwrap().0, 2);
        assert_eq!(state.lock().unwrap().1, TrayState::Recording);

        // Inner end: depth drops to 1, tray stays amber.
        handle_mcp_activity_end(&state, Some(&tray), None);
        assert_eq!(tray.state(), TrayState::Processing);
        assert_eq!(state.lock().unwrap().0, 1);

        // Outer end: depth hits 0, tray restores to the original baseline.
        handle_mcp_activity_end(&state, Some(&tray), None);
        assert_eq!(tray.state(), TrayState::Recording);
        assert_eq!(state.lock().unwrap().0, 0);
    }

    #[test]
    fn mcp_activity_restore_skipped_when_other_writer_owns_tray() {
        let tray = Tray::for_tests();
        let state = std::sync::Mutex::new((0u32, TrayState::Idle));

        handle_mcp_activity_start(&state, Some(&tray), McpPhase::Listening, None);
        assert_eq!(tray.state(), TrayState::Processing);

        // Simulate another writer (FSM event consumer) taking over the
        // tray while the MCP span is still active.
        tray.set_state(TrayState::Recording);

        handle_mcp_activity_end(&state, Some(&tray), None);
        // Tray must stay where the other writer left it — last-writer-wins.
        assert_eq!(tray.state(), TrayState::Recording);
        assert_eq!(state.lock().unwrap().0, 0);
    }

    #[test]
    fn mcp_activity_idle_to_amber_to_idle() {
        let tray = Tray::for_tests();
        let state = std::sync::Mutex::new((0u32, TrayState::Idle));

        assert_eq!(tray.state(), TrayState::Idle);
        handle_mcp_activity_start(&state, Some(&tray), McpPhase::Confirming, None);
        assert_eq!(tray.state(), TrayState::Processing);
        handle_mcp_activity_end(&state, Some(&tray), None);
        assert_eq!(tray.state(), TrayState::Idle);
    }

    #[test]
    fn mcp_activity_unmatched_end_is_ignored() {
        let tray = Tray::for_tests();
        tray.set_state(TrayState::Recording);
        let state = std::sync::Mutex::new((0u32, TrayState::Idle));
        // No prior Start — End must be a no-op (don't touch the tray,
        // don't underflow the counter).
        handle_mcp_activity_end(&state, Some(&tray), None);
        assert_eq!(tray.state(), TrayState::Recording);
        assert_eq!(state.lock().unwrap().0, 0);
    }

    /// A bare short hostname (e.g. "kitchen") gets ".local" appended so
    /// that the mDNS SRV record carries a valid `<name>.local.` FQDN.
    /// An already-qualified name (e.g. "kitchen.local") is left alone.
    #[test]
    fn mdns_host_qualification() {
        // bare → qualified
        let bare = "kitchen";
        let qualified = if bare.contains('.') { bare.to_string() } else { format!("{bare}.local") };
        assert_eq!(qualified, "kitchen.local");

        // already qualified → unchanged
        let already = "kitchen.local";
        let still_qualified =
            if already.contains('.') { already.to_string() } else { format!("{already}.local") };
        assert_eq!(still_qualified, "kitchen.local");
    }
}
