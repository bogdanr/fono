// SPDX-License-Identifier: GPL-3.0-only
//! Always-on wake-word listener lifecycle (Phase D) and trigger synthesis
//! (Phase E) of `plans/2026-06-23-wake-word-openwakeword-v2.md`.
//!
//! The daemon owns exactly one [`WakeHandle`]. While `[wakeword].enabled` is
//! true *and* the recording FSM is in [`FsmState::Idle`], the controller holds
//! a single capture stream (via [`AudioCapture::start_with_forwarder`]) and
//! feeds every PCM slice to a [`WakeWord`] detector on a dedicated thread. The
//! forwarder is cheap: it `try_send`s the slice into a bounded
//! `crossbeam_channel` and drops on overflow, per the `start_with_forwarder`
//! contract — it never blocks the capture thread.
//!
//! **Single mic source is the core invariant.** The listener must hold the mic
//! *only* in `Idle`; the moment the FSM enters any active/processing state the
//! capture handle is dropped (RAII stop), so there is never contention with
//! push-to-talk or assistant capture. When the FSM returns to `Idle` (and the
//! feature is still enabled) the stream re-opens.
//!
//! On a confirmed detection the detector thread synthesizes the configured
//! [`HotkeyAction`] into the daemon's *existing* `action_tx`, so dictation /
//! assistant start through the identical path as the physical hotkey — no
//! parallel orchestrator branch. A post-fire refractory window prevents a
//! single utterance from double-triggering.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fono_audio::{AudioCapture, CaptureConfig, CaptureStreamHandle, EnergyWakeStub, WakeWord};
use fono_core::config::{WakeTarget, WakeWord as WakeWordCfg, WakeWyoming};
use fono_core::{Config, Paths};
use fono_hotkey::{HotkeyAction, State as FsmState};
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

/// Bounded depth of the PCM hand-off queue between the capture thread and the
/// detector thread. Slices are dropped on overflow rather than blocking the
/// real-time capture callback.
const PCM_QUEUE_DEPTH: usize = 64;

/// Low-confidence score floor for a debug-level candidate line. The continuous
/// two-second heartbeat stays at `trace`; debug only surfaces windows where the
/// detector saw something plausibly phrase-like but still below threshold.
const WAKE_CANDIDATE_SCORE: f32 = 0.10;

/// Whether the always-on listener may hold the mic in `state`.
///
/// True **only** in [`FsmState::Idle`]. Every active / processing / MCP-driven
/// state must release the mic so there is exactly one capture source at a time
/// (the core Phase-D invariant; ADR 0012).
#[must_use]
pub fn should_listen(state: FsmState) -> bool {
    matches!(state, FsmState::Idle)
}

/// Which capture source the wake detector should open (Phase I; ADR 0012:45-68).
///
/// Returns `None` for the **idle** always-on path — the headline feature, where
/// Fono is silent and nothing is playing. Idle wake-word MUST read the
/// **default** source with **no AEC** on every platform: AEC is Linux/PipeWire
/// only and, because its sink carries only Fono's own TTS, it cannot help
/// reject ambient TV/music anyway (ADR 0012:45-52). `None` flows to
/// [`CaptureConfig::source`], whose default behaviour opens the system default
/// source — so the idle invariant holds by construction and cannot be coupled
/// to AEC by a future edit here.
///
/// Returns `Some(source)` **only** for the *wake/interrupt-while-Fono-is-speaking*
/// sub-case: when Fono is actively speaking *and* the per-utterance PipeWire
/// echo-cancel source (`fono_aec_source_<pid>`) exists, the detector may switch
/// its input to it to hear a wake phrase over Fono's own TTS, then switch back
/// to the default when it disappears (ADR 0012:53-68).
///
/// **This is an inert seam today.** No code in the tree creates an AEC source
/// yet (it lands with the double-talk barge-in slice,
/// `plans/2026-05-25-double-talk-barge-in-pipewire-aec-v1.md:434-444`), and the
/// listener only runs while the FSM is [`FsmState::Idle`] (see
/// [`should_listen`]) — i.e. never while speaking — so both inputs are
/// currently `false`/`None` and this always returns `None`. The decision lives
/// here, pure and tested, so the wiring is ready when barge-in's AEC source and
/// a speaking-state listener arrive.
#[must_use]
pub fn wake_capture_source(speaking: bool, aec_source: Option<&str>) -> Option<String> {
    match (speaking, aec_source) {
        (true, Some(src)) => Some(src.to_string()),
        _ => None,
    }
}

/// Map a configured wake target to the [`HotkeyAction`] it synthesizes — the
/// same actions the physical hotkey produces.
#[must_use]
pub fn action_for_target(target: WakeTarget) -> HotkeyAction {
    match target {
        WakeTarget::Dictation => HotkeyAction::TogglePressed,
        WakeTarget::Assistant => HotkeyAction::AssistantPressed,
    }
}

/// Post-fire refractory gate: swallows fires that land within `window` of the
/// previous *allowed* fire. A pure, testable instant gate (no clock of its
/// own) so the detector-consumer thread can drive it with `Instant::now()`.
pub struct RefractoryGate {
    window: Duration,
    last_fire: Option<Instant>,
}

impl RefractoryGate {
    #[must_use]
    pub fn new(window: Duration) -> Self {
        Self { window, last_fire: None }
    }

    /// Returns `true` if a fire is permitted at `now` — i.e. no allowed fire
    /// happened within `window` before it. Records the fire when permitted.
    pub fn allow(&mut self, now: Instant) -> bool {
        match self.last_fire {
            Some(prev) if now.duration_since(prev) < self.window => false,
            _ => {
                self.last_fire = Some(now);
                true
            }
        }
    }
}

/// Cheap, clonable controller handle. The daemon clones one of these into the
/// FSM action dispatcher (suspend/resume), the tray dispatcher and the IPC
/// reload path (enable/disable + phrase changes).
#[derive(Clone)]
pub struct WakeHandle {
    tx: mpsc::UnboundedSender<WakeCmd>,
    /// Synchronous fire-gate shared with the detector thread. Mirrors the
    /// FSM idle-state: `true` only while the FSM is [`FsmState::Idle`]. The
    /// detector loads it immediately before synthesizing an action, so a
    /// wake phrase that lands in the brief window between a session starting
    /// and the async [`WakeCmd::SetIdle`] suspend (drop of the capture
    /// stream) actually taking effect is dropped rather than queuing a
    /// second, overlapping session. Without it, repeating the wake phrase
    /// while the assistant is already starting fires it again — the
    /// "saying it three times needs three Escapes" bug.
    armed: Arc<AtomicBool>,
}

impl WakeHandle {
    /// Report the FSM's idle state. `idle` is [`should_listen`] of the new
    /// state: `true` lets the listener (re)open the mic, `false` suspends it.
    pub fn set_idle(&self, idle: bool) {
        // Flip the synchronous gate first so the detector stops firing the
        // instant the FSM leaves Idle — before the command is even dequeued
        // and the capture stream torn down. This closes the race that let a
        // repeated wake phrase start multiple overlapping sessions.
        self.armed.store(idle, Ordering::SeqCst);
        let _ = self.tx.send(WakeCmd::SetIdle(idle));
    }

    /// Re-read `[wakeword]` from disk and reconcile (tray toggle / config
    /// reload). Starts the listener if newly enabled while idle; stops it if
    /// disabled.
    pub fn reload(&self) {
        let _ = self.tx.send(WakeCmd::Reload);
    }
}

enum WakeCmd {
    /// FSM idle-state changed: `true` => the listener may hold the mic.
    SetIdle(bool),
    /// `[wakeword]` config changed on disk: re-read and reconcile.
    Reload,
    /// Internal: an async model fetch completed; rebuild the detector so it
    /// loads the freshly-downloaded `.ort` files (no config re-read).
    Rebuild,
}

/// Spawn the listener controller task and return its handle. Reconciles once
/// against the startup config (the daemon begins in [`FsmState::Idle`]).
#[must_use]
pub fn spawn(
    config: &Config,
    paths: &Paths,
    action_tx: mpsc::UnboundedSender<HotkeyAction>,
) -> WakeHandle {
    let (tx, mut rx) = mpsc::unbounded_channel::<WakeCmd>();
    let config_path = paths.config_file();
    // The daemon starts in `FsmState::Idle`, so the detector is armed.
    let armed = Arc::new(AtomicBool::new(true));
    let mut state = WakeState {
        cfg: config.wakeword.clone(),
        paths: paths.clone(),
        action_tx,
        idle: true,
        runtime: None,
        self_tx: tx.clone(),
        fetch_attempted: false,
        armed: armed.clone(),
    };
    tokio::spawn(async move {
        warn_wyoming_client_privacy(&state.cfg);
        state.reconcile();
        while let Some(cmd) = rx.recv().await {
            match cmd {
                WakeCmd::SetIdle(idle) => {
                    if state.idle != idle {
                        state.idle = idle;
                        state.reconcile();
                    }
                }
                WakeCmd::Reload => match Config::load(&config_path) {
                    Ok(c) => {
                        state.cfg = c.wakeword;
                        warn_wyoming_client_privacy(&state.cfg);
                        // A live config change always rebuilds the runtime so
                        // phrase / sensitivity edits take effect: drop first,
                        // then reconcile re-opens if still warranted. A config
                        // change also re-arms the one-shot model fetch (the
                        // phrase set may have changed).
                        state.runtime = None;
                        state.fetch_attempted = false;
                        state.reconcile();
                    }
                    Err(e) => warn!("wake: reload config failed: {e:#}"),
                },
                // A background model fetch finished: rebuild the detector so it
                // picks up the now-present `.ort` files (config unchanged).
                WakeCmd::Rebuild => {
                    state.runtime = None;
                    state.reconcile();
                }
            }
        }
        // Channel closed (daemon shutting down): drop the state, whose
        // runtime field stops capture and joins the detector thread.
        drop(state);
    });
    WakeHandle { tx, armed }
}

/// Surface the loud privacy warning when `[wakeword].wyoming` is configured
/// in the opt-in **client** direction (enabled + an external `uri`). That
/// mode streams idle mic audio over the LAN, breaking the
/// audio-never-leaves-the-machine-while-idle guarantee, so we shout about it
/// in the daemon log. (`fono doctor` repeats the warning via the same
/// [`WakeWyoming::CLIENT_PRIVACY_WARNING`] string — Phase J.)
///
/// NOTE (follow-up seam): the actual full-duplex streaming client that would
/// delegate Fono's own activation to the external `wyoming-openwakeword`
/// service is not yet wired — local on-device detection stays active. The
/// config path, default-off behaviour, and this warning are real now; the
/// streaming transport attaches at [`WakeRuntime::start`] when implemented.
fn warn_wyoming_client_privacy(cfg: &WakeWordCfg) {
    if cfg.enabled && cfg.wyoming.as_ref().is_some_and(WakeWyoming::is_client) {
        warn!("wake: {}", WakeWyoming::CLIENT_PRIVACY_WARNING);
    }
}

struct WakeState {
    cfg: WakeWordCfg,
    paths: Paths,
    action_tx: mpsc::UnboundedSender<HotkeyAction>,
    /// `true` when the FSM is Idle (mic available to the listener).
    idle: bool,
    runtime: Option<WakeRuntime>,
    /// Self-channel, so a background model-fetch task can ask the controller
    /// to rebuild the detector once the `.ort` files have landed.
    self_tx: mpsc::UnboundedSender<WakeCmd>,
    /// One-shot guard: a model fetch has been spawned this enable/reload
    /// cycle. Prevents the `Rebuild → reconcile → fetch` path from hammering
    /// the network when a download fails (re-armed only on `Reload`).
    fetch_attempted: bool,
    /// Synchronous fire-gate (see [`WakeHandle::armed`]). Cloned into every
    /// [`WakeRuntime`] the controller spawns so the detector thread can read
    /// the live FSM idle-state.
    armed: Arc<AtomicBool>,
}

impl WakeState {
    fn should_run(&self) -> bool {
        self.cfg.enabled && self.idle
    }

    /// Open or drop the capture stream to match the desired state. Idempotent.
    fn reconcile(&mut self) {
        // Ensure the model artifacts are present whenever the feature is
        // enabled — independent of idle, so the `.ort` files are ready by the
        // time the listener may open the mic.
        #[cfg(feature = "wakeword-onnx")]
        if self.cfg.enabled {
            self.maybe_fetch_models();
        }
        if self.should_run() {
            if self.runtime.is_none() {
                match WakeRuntime::start(
                    &self.cfg,
                    &self.paths,
                    self.action_tx.clone(),
                    self.armed.clone(),
                ) {
                    Ok(rt) => {
                        debug!("wake: listener started (mic open)");
                        self.runtime = Some(rt);
                    }
                    Err(e) => warn!("wake: failed to start listener: {e:#}"),
                }
            }
        } else if self.runtime.take().is_some() {
            debug!("wake: listener suspended (enabled={}, idle={})", self.cfg.enabled, self.idle);
        }
    }

    /// Download any configured phrase models whose `.ort` files are not yet
    /// cached, on a background task. Fires at most once per enable/reload
    /// cycle (see [`WakeState::fetch_attempted`]); on completion it sends
    /// [`WakeCmd::Rebuild`] so the detector is rebuilt against the new files.
    /// Unknown ids and already-present models are skipped silently.
    #[cfg(feature = "wakeword-onnx")]
    fn maybe_fetch_models(&mut self) {
        use fono_audio::wake_registry;

        if self.fetch_attempted || self.cfg.phrases.is_empty() {
            return;
        }
        let cache_dir = self.paths.cache_dir.clone();
        let missing: Vec<String> = self
            .cfg
            .phrases
            .iter()
            .map(|p| p.model.clone())
            .filter(|id| match wake_registry::resolved_paths(id, &cache_dir) {
                Some(r) => !(r.melspec.exists() && r.embedding.exists() && r.classifier.exists()),
                None => false, // unknown id: nothing to fetch
            })
            .collect();
        if missing.is_empty() {
            return;
        }
        self.fetch_attempted = true;
        let tx = self.self_tx.clone();
        tokio::spawn(async move {
            for id in missing {
                match wake_registry::fetch_model(&id, &cache_dir, None).await {
                    Ok(_) => debug!("wake: fetched model '{id}'"),
                    Err(e) => warn!("wake: fetch model '{id}' failed: {e:#}"),
                }
            }
            let _ = tx.send(WakeCmd::Rebuild);
        });
    }
}

/// Live capture + detector pair. Dropping it stops everything: the capture
/// handle is released first (RAII stop + drops the forwarder's channel
/// sender), which lets the detector thread's `recv()` return `Err` and exit;
/// then the thread is joined.
struct WakeRuntime {
    capture: Option<CaptureStreamHandle>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl WakeRuntime {
    fn start(
        cfg: &WakeWordCfg,
        paths: &Paths,
        action_tx: mpsc::UnboundedSender<HotkeyAction>,
        armed: Arc<AtomicBool>,
    ) -> anyhow::Result<Self> {
        let detector = build_detector(cfg, paths);
        let targets: Vec<(String, WakeTarget)> =
            cfg.phrases.iter().map(|p| (p.model.clone(), p.target)).collect();
        let default_target = cfg.phrases.first().map_or(WakeTarget::Dictation, |p| p.target);
        let refractory = Duration::from_millis(cfg.refractory_ms.max(1));

        let (pcm_tx, pcm_rx) = crossbeam_channel::bounded::<Vec<f32>>(PCM_QUEUE_DEPTH);
        let worker =
            std::thread::Builder::new().name("fono-wake-detector".into()).spawn(move || {
                run_detector(
                    detector,
                    &pcm_rx,
                    &action_tx,
                    &targets,
                    default_target,
                    refractory,
                    &armed,
                );
            })?;

        // Cheap forwarder: copy the slice and `try_send`, dropping on overflow.
        //
        // Phase I / ADR 0012:45-68 — the always-on listener only ever runs
        // while the FSM is Idle (see `should_listen`), i.e. Fono is NOT
        // speaking, so it reads the DEFAULT source with NO AEC on every
        // platform. `wake_capture_source(false, None)` therefore resolves to
        // `None`, and `CaptureConfig::source = None` opens the default source.
        // The wake-while-speaking sub-case (switch to `fono_aec_source_<pid>`
        // while TTS plays) is the deferred seam: pass the real
        // speaking/AEC-source state here once barge-in's per-utterance AEC
        // source exists and the listener is allowed to run while speaking.
        let source = wake_capture_source(false, None);
        let mut first_pcm = true;
        let capture = AudioCapture::new(CaptureConfig { source, ..CaptureConfig::default() })
            .start_with_forwarder(move |pcm: &[f32]| {
                if first_pcm {
                    first_pcm = false;
                    let peak = pcm.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()));
                    trace!("wake: first PCM received samples={} peak={:.3}", pcm.len(), peak);
                }
                if pcm_tx.try_send(pcm.to_vec()).is_err() {
                    warn!("wake: PCM queue full; dropping {} samples", pcm.len());
                }
            })?;

        Ok(Self { capture: Some(capture), worker: Some(worker) })
    }
}

impl Drop for WakeRuntime {
    fn drop(&mut self) {
        // Order matters: drop capture (stops the backend + drops the channel
        // sender held by the forwarder) so the detector thread can exit, then
        // join it.
        self.capture = None;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Detector-consumer loop (runs on the dedicated thread). Feeds PCM into the
/// [`WakeWord`] backend, applies the refractory gate, and synthesizes the
/// mapped [`HotkeyAction`] into the daemon's existing channel on a fire.
fn run_detector(
    mut detector: Box<dyn WakeWord>,
    pcm_rx: &crossbeam_channel::Receiver<Vec<f32>>,
    action_tx: &mpsc::UnboundedSender<HotkeyAction>,
    targets: &[(String, WakeTarget)],
    default_target: WakeTarget,
    refractory: Duration,
    armed: &AtomicBool,
) {
    let mut gate = RefractoryGate::new(refractory);
    let mut chunks = 0_u64;
    let mut last_report = Instant::now();
    let mut window_chunks = 0_u64;
    let mut window_samples = 0_u64;
    let mut window_peak = 0.0_f32;
    let mut window_score = 0.0_f32;
    while let Ok(pcm) = pcm_rx.recv() {
        chunks += 1;
        window_chunks += 1;
        window_samples += pcm.len() as u64;
        window_peak = window_peak.max(pcm.iter().fold(0.0_f32, |acc, s| acc.max(s.abs())));
        let decision = match detector.feed(&pcm) {
            Ok(decision) => decision,
            Err(e) => {
                debug!("wake: detector error: {e:#}");
                continue;
            }
        };
        window_score = window_score.max(decision.score);
        // Drop the fire when the FSM is no longer Idle (a session is already
        // starting or running). Checked before the refractory gate so a
        // suppressed fire doesn't consume the gate's window. This is the
        // synchronous backstop to the async suspend: even while this thread
        // is still alive in the window before the capture stream is dropped,
        // a repeated wake phrase cannot start a second overlapping session.
        if decision.fired && !armed.load(Ordering::SeqCst) {
            debug!(
                "wake: fired phrase={:?} score={:.3} suppressed (session already active)",
                decision.phrase, decision.score
            );
            continue;
        }
        if decision.fired && gate.allow(Instant::now()) {
            let target = decision
                .phrase
                .as_deref()
                .and_then(|ph| targets.iter().find(|(m, _)| m == ph).map(|(_, t)| *t))
                .unwrap_or(default_target);
            let action = action_for_target(target);
            debug!(
                "wake: fired phrase={:?} score={:.3} -> {action:?}",
                decision.phrase, decision.score
            );
            if action_tx.send(action).is_err() {
                break; // daemon gone
            }
        }
        if last_report.elapsed() >= Duration::from_secs(2) {
            if window_score >= WAKE_CANDIDATE_SCORE {
                debug!("wake: candidate max_score={:.3} max_peak={:.3}", window_score, window_peak);
            }
            trace!(
                "wake: detector alive chunks={} window_chunks={} window_samples={} max_peak={:.3} max_score={:.3}",
                chunks, window_chunks, window_samples, window_peak, window_score
            );
            last_report = Instant::now();
            window_chunks = 0;
            window_samples = 0;
            window_peak = 0.0;
            window_score = 0.0;
        }
    }
    debug!("wake: detector thread exiting");
}

/// Build the active detector. Uses [`EnergyWakeStub`] by default; only when the
/// `wakeword-onnx` feature is compiled in **and** the model files resolve does
/// it build the real [`fono_audio::wakeword::OnnxWakeWord`], otherwise falls
/// back to the stub and logs at debug. Keeping the stub as the default keeps
/// the daemon functional before any model is fetched (Phase G).
///
/// Shared with the Wyoming wake **server** path (Phase H): the daemon binds a
/// [`fono_net::wyoming::server::WakeProvider`] that calls this to construct a
/// fresh per-connection detector, so the LAN wake service runs the *same*
/// detector as the local listener (audio stays on the machine).
pub(crate) fn build_detector(cfg: &WakeWordCfg, paths: &Paths) -> Box<dyn WakeWord> {
    #[cfg(feature = "wakeword-onnx")]
    {
        match try_load_onnx(cfg, paths) {
            Ok(d) => {
                debug!("wake: using ONNX detector");
                return Box::new(d);
            }
            Err(reason) => {
                if cfg.enabled {
                    warn!(
                        "wake: using fallback energy stub ({reason}); wake word will not fire until ONNX model files are cached and loadable"
                    );
                } else {
                    debug!("wake: using fallback energy stub ({reason})");
                }
            }
        }
    }
    #[cfg(not(feature = "wakeword-onnx"))]
    {
        if cfg.enabled {
            warn!(
                "wake: using fallback energy stub (wakeword-onnx feature disabled); wake word will not fire in this build"
            );
        } else {
            debug!("wake: using fallback energy stub (wakeword-onnx feature disabled)");
        }
    }
    let _ = (cfg, paths);
    Box::new(EnergyWakeStub::default())
}

/// Resolve the three-stage openWakeWord graphs from the on-disk model cache and
/// load every configured phrase classifier. Returns an error reason (caller
/// falls back to the stub) if any required `.ort` graph is missing or load
/// fails. Model fetch/registration is Phase G; this only consumes what is
/// already present.
#[cfg(feature = "wakeword-onnx")]
fn try_load_onnx(
    cfg: &WakeWordCfg,
    paths: &Paths,
) -> Result<fono_audio::wakeword::OnnxWakeWord, String> {
    use fono_audio::wakeword::{OnnxWakeWord, PhraseModelSpec, WakeModelPaths};

    if cfg.phrases.is_empty() {
        return Err("no wake phrases configured".into());
    }
    let dir = paths.cache_dir.join("models").join("wakeword");
    let melspec = dir.join("melspectrogram.ort");
    let embedding = dir.join("embedding.ort");
    if !melspec.exists() {
        return Err("missing melspectrogram.ort".into());
    }
    if !embedding.exists() {
        return Err("missing embedding.ort".into());
    }
    let mut phrases = Vec::with_capacity(cfg.phrases.len());
    for p in &cfg.phrases {
        let model = dir.join(format!("{}.ort", p.model));
        if !model.exists() {
            return Err(format!("missing classifier {}.ort", p.model));
        }
        phrases.push(PhraseModelSpec { phrase: p.model.clone(), model, threshold: p.sensitivity });
    }
    let model_paths = WakeModelPaths { melspec, embedding, phrases };
    OnnxWakeWord::load(&model_paths).map_err(|e| format!("ONNX load failed: {e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `should_listen` is true for `Idle` and false for every other state.
    /// Enumerated exhaustively so a new active state can't silently start
    /// contending for the mic.
    #[test]
    fn should_listen_only_in_idle() {
        use fono_hotkey::fsm::ToolKind;
        use fono_hotkey::RecordingMode;
        assert!(should_listen(FsmState::Idle), "Idle must listen");

        let active = [
            FsmState::McpDriven { tool: ToolKind::Speak },
            FsmState::McpDriven { tool: ToolKind::Listen },
            FsmState::McpDriven { tool: ToolKind::Confirm },
            FsmState::Recording(RecordingMode::Hold),
            FsmState::Recording(RecordingMode::Toggle),
            FsmState::LiveDictating(RecordingMode::Hold),
            FsmState::LiveDictating(RecordingMode::Toggle),
            FsmState::Processing,
            FsmState::AssistantRecording,
            FsmState::AssistantThinking,
            FsmState::AssistantSpeaking,
            FsmState::AssistantLive,
        ];
        for st in active {
            assert!(!should_listen(st), "{st:?} must NOT hold the mic");
        }
    }

    #[test]
    fn target_maps_to_hotkey_action() {
        assert_eq!(action_for_target(WakeTarget::Dictation), HotkeyAction::TogglePressed);
        assert_eq!(action_for_target(WakeTarget::Assistant), HotkeyAction::AssistantPressed);
    }

    /// Phase I / ADR 0012:45-68. The idle always-on path (the only path that
    /// runs today) must read the default source with NO AEC: `None` here.
    /// Only the wake-while-speaking sub-case — speaking AND an AEC source
    /// present — selects the `fono_aec_source_<pid>` echo-cancel source.
    #[test]
    fn wake_capture_source_picks_aec_only_while_speaking() {
        // Idle: not speaking, no AEC source -> default source.
        assert_eq!(wake_capture_source(false, None), None, "idle must use the default source");
        // Not speaking but an AEC source happens to exist -> still default:
        // idle wake-word never depends on AEC.
        assert_eq!(
            wake_capture_source(false, Some("fono_aec_source_4242")),
            None,
            "non-speaking path must ignore any AEC source"
        );
        // Speaking but no AEC source (e.g. non-Linux, or AEC not loaded)
        // -> default source; we never invent a source that isn't there.
        assert_eq!(
            wake_capture_source(true, None),
            None,
            "speaking without an AEC source falls back to the default"
        );
        // Speaking AND an AEC source exists -> switch to it.
        assert_eq!(
            wake_capture_source(true, Some("fono_aec_source_4242")),
            Some("fono_aec_source_4242".to_string()),
            "wake-while-speaking must reuse the AEC source"
        );
    }

    /// A detector stub that fires on every frame — lets us drive
    /// [`run_detector`]'s gating logic deterministically.
    struct AlwaysFire;
    impl WakeWord for AlwaysFire {
        fn feed(&mut self, _frame: &[f32]) -> anyhow::Result<fono_audio::wakeword::WakeDecision> {
            Ok(fono_audio::wakeword::WakeDecision { fired: true, score: 1.0, phrase: None })
        }
    }

    /// While the FSM is not Idle (`armed == false`) a wake fire must be
    /// dropped — even though the detector thread is still alive in the
    /// window before the async suspend tears down the capture stream. This
    /// is the synchronous backstop that stops a repeated wake phrase from
    /// starting multiple overlapping sessions.
    #[test]
    fn fire_suppressed_when_disarmed() {
        let (pcm_tx, pcm_rx) = crossbeam_channel::bounded::<Vec<f32>>(8);
        let (action_tx, mut action_rx) = mpsc::unbounded_channel::<HotkeyAction>();
        let armed = AtomicBool::new(false);
        let targets: Vec<(String, WakeTarget)> = Vec::new();
        std::thread::scope(|s| {
            s.spawn(|| {
                run_detector(
                    Box::new(AlwaysFire),
                    &pcm_rx,
                    &action_tx,
                    &targets,
                    WakeTarget::Dictation,
                    Duration::from_millis(1),
                    &armed,
                );
            });
            pcm_tx.send(vec![0.0_f32; 16]).unwrap();
            pcm_tx.send(vec![0.0_f32; 16]).unwrap();
            drop(pcm_tx);
        });
        assert!(action_rx.try_recv().is_err(), "disarmed detector must not synthesize an action");
    }

    /// While armed (`FsmState::Idle`) a fire synthesizes the mapped action.
    #[test]
    fn fire_synthesizes_action_when_armed() {
        let (pcm_tx, pcm_rx) = crossbeam_channel::bounded::<Vec<f32>>(8);
        let (action_tx, mut action_rx) = mpsc::unbounded_channel::<HotkeyAction>();
        let armed = AtomicBool::new(true);
        let targets: Vec<(String, WakeTarget)> = Vec::new();
        std::thread::scope(|s| {
            s.spawn(|| {
                run_detector(
                    Box::new(AlwaysFire),
                    &pcm_rx,
                    &action_tx,
                    &targets,
                    WakeTarget::Dictation,
                    Duration::from_millis(1),
                    &armed,
                );
            });
            pcm_tx.send(vec![0.0_f32; 16]).unwrap();
            drop(pcm_tx);
        });
        assert_eq!(
            action_rx.try_recv().ok(),
            Some(HotkeyAction::TogglePressed),
            "armed detector must synthesize the mapped action"
        );
    }

    #[test]
    fn refractory_gate_swallows_double_trigger() {
        let window = Duration::from_millis(500);
        let mut gate = RefractoryGate::new(window);
        let t0 = Instant::now();
        // First fire is allowed.
        assert!(gate.allow(t0), "first fire must pass");
        // A second fire well within the window is swallowed.
        assert!(!gate.allow(t0 + Duration::from_millis(100)), "double-trigger must be gated");
        // Still gated just before the window closes.
        assert!(!gate.allow(t0 + Duration::from_millis(499)));
        // One after the window elapses fires again.
        assert!(gate.allow(t0 + Duration::from_millis(600)), "post-window fire must pass");
        // And re-arms: immediately after the latest fire is gated again.
        assert!(!gate.allow(t0 + Duration::from_millis(650)));
    }

    #[test]
    fn refractory_window_is_relative_to_last_allowed_fire() {
        // A long quiet period then two close fires: first passes, second gated.
        let mut gate = RefractoryGate::new(Duration::from_millis(300));
        let t0 = Instant::now();
        assert!(gate.allow(t0));
        assert!(gate.allow(t0 + Duration::from_secs(5)), "isolated later fire passes");
        assert!(!gate.allow(t0 + Duration::from_secs(5) + Duration::from_millis(50)));
    }
}
