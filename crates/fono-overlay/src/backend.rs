// SPDX-License-Identifier: GPL-3.0-only
//! Pluggable overlay backends.
//!
//! Each backend owns the windowing-surface plumbing for one
//! environment (X11, wlr-layer-shell, xdg_toplevel fallback, no-op).
//! The renderer in [`crate::renderer`] is unchanged across backends —
//! every backend hands the renderer the same `&mut [u32]`
//! framebuffer.
//!
//! ## Design deviation from the plan §2 sketch
//!
//! The plan's `OverlayBackend` trait sketch (`pump` + `with_framebuffer`
//! + `set_visible`) assumed a unified polling loop owned by
//!   `spawn_overlay`. The winit / softbuffer X11 path owns its event
//!   loop via `EventLoop::run_app`, and the Wayland backends run their
//!   own `event_queue.dispatch` loops with backend-specific shm /
//!   configure / scale lifecycles. Forcing all three into a common
//!   `pump` shape costs more than it gains (the pure renderer split is
//!   already where 95 % of the leverage is).
//!
//! What we keep from the plan:
//!
//! - One module per backend under `backends/`.
//! - A trait-shaped descriptor ([`BackendDescriptor`]) used by
//!   [`spawn_overlay`] to walk the candidate list and report into
//!   `fono doctor`.
//! - The `OverlayCmd` channel + waker contract that lets backends
//!   schedule wake-ups without touching their event-loop internals.
//! - Deterministic env-driven backend selection with a `noop`
//!   terminal fallback that always succeeds.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use fono_core::config::WaveformStyle;

use crate::OverlayState;

// ---------------------------------------------------------------------------
//  Backend identification
// ---------------------------------------------------------------------------

/// Stable identifier for each overlay backend. Used by env-override
/// parsing, `fono doctor`, and structured log lines so operators can
/// see at a glance which backend was selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendId {
    /// `wlr-layer-shell` — primary Wayland path (sway, hyprland,
    /// river, KDE Plasma 5.27+, COSMIC, Wayfire).
    WlrLayerShell,
    /// `x11-override-redirect` — the original winit + softbuffer
    /// path. Used on native X11 sessions and on Wayland sessions
    /// via Xwayland (the GNOME / KDE-Wayland default), where
    /// Mutter honours override-redirect placement and excludes the
    /// surface from Alt+Tab.
    X11OverrideRedirect,
    /// `noop` — silent in-process stub. Terminal fallback that
    /// always succeeds so the daemon never aborts on a missing
    /// graphics environment.
    Noop,
}

impl BackendId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WlrLayerShell => "wlr-layer-shell",
            Self::X11OverrideRedirect => "x11-override-redirect",
            Self::Noop => "noop",
        }
    }

    /// Parse the value of `FONO_OVERLAY_BACKEND` into a forced
    /// backend selection.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "wlr" | "wlr-layer-shell" | "layer-shell" => Some(Self::WlrLayerShell),
            "x11" | "x11-override-redirect" => Some(Self::X11OverrideRedirect),
            "noop" | "none" | "off" => Some(Self::Noop),
            _ => None,
        }
    }
}

/// Capability summary surfaced by `fono doctor` so users can see at
/// a glance what the selected backend can and can't do.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy)]
pub struct BackendCapabilities {
    /// Surface honours per-pixel alpha (rounded corners visible).
    pub transparency: bool,
    /// Client controls window position.
    pub client_positioning: bool,
    /// Surface never receives keyboard focus.
    pub focus_passthrough: bool,
    /// Pointer input passes through to the underlying window.
    pub click_passthrough: bool,
}

impl BackendCapabilities {
    pub fn summary(&self) -> String {
        format!(
            "transparency={} positioning={} focus-passthrough={} click-passthrough={}",
            yn(self.transparency),
            if self.client_positioning { "client" } else { "compositor" },
            yn(self.focus_passthrough),
            yn(self.click_passthrough),
        )
    }
}

fn yn(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

// ---------------------------------------------------------------------------
//  Commands + handle plumbing
// ---------------------------------------------------------------------------

/// Commands sent from the orchestrator to the overlay backend's
/// thread. Stable across backends so the public
/// [`crate::OverlayHandle`] API surface is identical regardless of
/// which surface technology is rendering.
#[derive(Debug)]
pub enum OverlayCmd {
    SetState(OverlayState),
    UpdateText(String),
    AudioLevel(f32),
    AudioSamples(Vec<f32>),
    FftBins(Vec<f32>),
    SetVolumeBar(bool),
    SetWaveformStyle(WaveformStyle),
    Shutdown,
}

/// Wake-up hook a backend provides so the orchestrator can rouse the
/// backend's event loop after pushing a command into the channel.
/// X11 uses `winit::EventLoopProxy::send_event`; Wayland backends use
/// a self-pipe / eventfd; the no-op backend uses a no-op closure.
pub type Waker = Box<dyn Fn() + Send + Sync>;

/// Per-spawn metadata bundled with the resulting handle.
pub struct SpawnedBackend {
    pub id: BackendId,
    pub capabilities: BackendCapabilities,
    pub tx: Sender<OverlayCmd>,
    pub waker: Waker,
    pub join: JoinHandle<()>,
}

/// Errors a backend can report from its `try_spawn` constructor.
#[derive(Debug)]
pub enum BackendError {
    /// Backend prerequisites missing in the current environment
    /// (`DISPLAY` unset, layer-shell protocol absent, library not
    /// loadable, …). Selection falls through to the next candidate.
    NotAvailable(String),
    /// Backend was selected but failed during spawn (thread spawn
    /// error, registry walk failed, …). Selection falls through.
    SpawnFailed(String),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAvailable(s) => write!(f, "not viable: {s}"),
            Self::SpawnFailed(s) => write!(f, "spawn failed: {s}"),
        }
    }
}

// ---------------------------------------------------------------------------
//  Public OverlayHandle (the orchestrator-facing surface)
// ---------------------------------------------------------------------------

/// Handle to a running overlay. Cheap to clone — wraps an
/// `Arc<HandleInner>`. The public `set_*` / `push_*` / `shutdown`
/// API is unchanged across backends so call sites in
/// `crates/fono/src/{session,cli,live}.rs` compile against any
/// selected backend.
#[derive(Clone)]
pub struct OverlayHandle {
    inner: Arc<HandleInner>,
}

struct HandleInner {
    tx: Sender<OverlayCmd>,
    waker: Waker,
    join: Mutex<Option<JoinHandle<()>>>,
    id: BackendId,
    capabilities: BackendCapabilities,
}

impl OverlayHandle {
    pub(crate) fn from_spawned(s: SpawnedBackend) -> Self {
        Self {
            inner: Arc::new(HandleInner {
                tx: s.tx,
                waker: s.waker,
                join: Mutex::new(Some(s.join)),
                id: s.id,
                capabilities: s.capabilities,
            }),
        }
    }

    fn send(&self, cmd: OverlayCmd) {
        let _ = self.inner.tx.send(cmd);
        (self.inner.waker)();
    }

    pub fn set_state(&self, state: OverlayState) {
        self.send(OverlayCmd::SetState(state));
    }

    pub fn update_text(&self, text: impl Into<String>) {
        self.send(OverlayCmd::UpdateText(text.into()));
    }

    pub fn push_level(&self, amplitude: f32) {
        self.send(OverlayCmd::AudioLevel(amplitude));
    }

    pub fn push_samples(&self, samples: Vec<f32>) {
        self.send(OverlayCmd::AudioSamples(samples));
    }

    pub fn push_fft_bins(&self, bins: Vec<f32>) {
        self.send(OverlayCmd::FftBins(bins));
    }

    pub fn set_volume_bar(&self, enabled: bool) {
        self.send(OverlayCmd::SetVolumeBar(enabled));
    }

    pub fn set_waveform_style(&self, style: WaveformStyle) {
        self.send(OverlayCmd::SetWaveformStyle(style));
    }

    pub fn shutdown(&self) {
        self.send(OverlayCmd::Shutdown);
        if let Ok(mut g) = self.inner.join.lock() {
            if let Some(j) = g.take() {
                let _ = j.join();
            }
        }
    }

    /// Backend selected for this handle. Surfaced by `fono doctor`
    /// and by the daemon's startup log line.
    pub fn backend_id(&self) -> BackendId {
        self.inner.id
    }

    pub fn backend_capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities
    }
}

// ---------------------------------------------------------------------------
//  Backend selection
// ---------------------------------------------------------------------------

/// Compute the ordered candidate list for the current session, per
/// the plan §3 selection table.
fn candidate_list() -> Vec<BackendId> {
    candidate_list_with(|k| std::env::var_os(k).is_some())
}

/// Test seam: same logic as [`candidate_list`] but with an injectable
/// env-lookup predicate. `env_present("WAYLAND_DISPLAY") == true`
/// emulates a Wayland session, `env_present("DISPLAY")` an X11 one.
fn candidate_list_with(env_present: impl Fn(&str) -> bool) -> Vec<BackendId> {
    let wayland = env_present("WAYLAND_DISPLAY");
    let x11 = env_present("DISPLAY");
    match (wayland, x11) {
        // Wayland session with Xwayland present (the default on
        // Ubuntu / Fedora GNOME, Ubuntu KDE, etc.). Try the layer-
        // shell first — it's the correct protocol when the compositor
        // supports it (sway, hyprland, KDE Wayland, …). On Mutter /
        // GNOME `try_spawn` for `wlr` returns NotAvailable at runtime
        // and we fall through to the X11 override-redirect backend
        // running under Xwayland. Mutter respects override-redirect:
        // the overlay is client-positioned, stays above normal
        // windows, and is excluded from Alt+Tab and the taskbar —
        // same UX as on a native X11 session.
        (true, true) => {
            vec![BackendId::WlrLayerShell, BackendId::X11OverrideRedirect, BackendId::Noop]
        }
        // Wayland-only session (no Xwayland; rare — embedded /
        // sandboxed / stripped distro builds). Layer-shell is the
        // only graphical option; without it we degrade to noop
        // (dictation still works, just no visual indicator). Users
        // wanting an overlay here should install xwayland or run a
        // compositor that supports zwlr_layer_shell_v1.
        (true, false) => vec![BackendId::WlrLayerShell, BackendId::Noop],
        (false, true) => vec![BackendId::X11OverrideRedirect, BackendId::Noop],
        (false, false) => vec![BackendId::Noop],
    }
}

/// Test seam: simulate the env-driven first-pick that [`spawn_overlay`]
/// would use, with both `FONO_OVERLAY_BACKEND` and the
/// `WAYLAND_DISPLAY` / `DISPLAY` lookup injected. Returns the
/// candidate sequence (not the actually-spawned one — we don't try
/// any backend here, so this is safe to call from tests).
#[doc(hidden)]
pub fn pick_backend_with(
    forced: Option<&str>,
    env_present: impl Fn(&str) -> bool,
) -> Vec<BackendId> {
    let forced = forced.and_then(BackendId::parse);
    forced.map_or_else(|| candidate_list_with(env_present), |b| vec![b, BackendId::Noop])
}

/// Spawn the overlay using the best available backend for the
/// current environment.
///
/// 1. If `FONO_OVERLAY_BACKEND` is set to a recognised value, only
///    that backend is attempted; on failure we still fall through to
///    `noop` so the daemon never aborts.
/// 2. Otherwise we walk [`candidate_list`] and the first backend
///    whose `try_spawn` returns `Ok` wins.
///
/// Always returns `Ok` — the `noop` backend is a terminal sink.
pub fn spawn_overlay(style: WaveformStyle) -> std::io::Result<OverlayHandle> {
    let forced = std::env::var("FONO_OVERLAY_BACKEND").ok().and_then(|raw| {
        let parsed = BackendId::parse(&raw);
        if parsed.is_none() {
            tracing::warn!(
                "overlay: FONO_OVERLAY_BACKEND={raw:?} not recognised; \
                 using automatic selection"
            );
        }
        parsed
    });

    let candidates: Vec<BackendId> = forced.map_or_else(candidate_list, |b| {
        tracing::info!("overlay: FONO_OVERLAY_BACKEND forces backend={}", b.as_str());
        vec![b, BackendId::Noop]
    });

    for id in candidates {
        tracing::info!("overlay: trying backend={}", id.as_str());
        match try_spawn(id, style) {
            Ok(spawned) => {
                tracing::info!(
                    "overlay: backend={} selected ({})",
                    spawned.id.as_str(),
                    spawned.capabilities.summary()
                );
                return Ok(OverlayHandle::from_spawned(spawned));
            }
            Err(e) => {
                tracing::info!("overlay: backend={} {e}", id.as_str());
            }
        }
    }
    // Unreachable in practice: noop always succeeds.
    Err(std::io::Error::other("no overlay backend available (noop should be a terminal sink)"))
}

fn try_spawn(id: BackendId, style: WaveformStyle) -> Result<SpawnedBackend, BackendError> {
    match id {
        BackendId::WlrLayerShell => {
            #[cfg(feature = "backend-wlr")]
            {
                crate::backends::wayland_layer_shell::try_spawn(style)
            }
            #[cfg(not(feature = "backend-wlr"))]
            {
                let _ = style;
                Err(BackendError::NotAvailable("backend-wlr feature disabled".into()))
            }
        }
        BackendId::X11OverrideRedirect => {
            #[cfg(feature = "backend-x11")]
            {
                crate::backends::winit_x11::try_spawn(style)
            }
            #[cfg(not(feature = "backend-x11"))]
            {
                let _ = style;
                Err(BackendError::NotAvailable("backend-x11 feature disabled".into()))
            }
        }
        BackendId::Noop => Ok(crate::backends::noop::spawn(style)),
    }
}

/// Probe-only helper for `fono doctor`. Returns the backend the next
/// `spawn_overlay` call would select, without actually creating a
/// surface. Currently this short-circuits on the env-var override
/// and otherwise returns the first candidate in the list — actual
/// viability of e.g. `wlr-layer-shell` is only confirmed at spawn
/// time.
pub fn probe_selection() -> (BackendId, &'static str) {
    if let Some(raw) = std::env::var_os("FONO_OVERLAY_BACKEND") {
        if let Some(id) = raw.to_str().and_then(BackendId::parse) {
            return (id, "FONO_OVERLAY_BACKEND override");
        }
    }
    let list = candidate_list();
    let first = list.first().copied().unwrap_or(BackendId::Noop);
    let reason = match first {
        BackendId::WlrLayerShell => {
            "Wayland + layer-shell preferred (falls through to Xwayland on GNOME / Mutter)"
        }
        BackendId::X11OverrideRedirect => "X11 (or Xwayland on Wayland sessions)",
        BackendId::Noop => "no graphics session detected",
    };
    (first, reason)
}
